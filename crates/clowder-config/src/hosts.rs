//! The remote host registry: a nicknamed list of remote daemons, owned by the CLI (not the daemon)
//! so it stays readable and writable when nothing is reachable.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// One remote daemon. `name` is the identity (unique, user-chosen); `address` is editable
/// underneath it, so "same box, new DNS name" keeps its pin.
///
/// Evolved by ADDITIVE `#[serde(default)]` fields only — the mechanism proven by
/// `AgentRecord::tree`. Deliberately no version key: this repo's precedent is additive fields,
/// and a version key invites migration code nobody writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostRecord {
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub tls: bool,
    /// Bearer token for this daemon. Why this file is 0600.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The pinned server-cert SHA-256 (lowercase hex). `None` = not yet paired.
    ///
    /// AUTHORITATIVE when present — `remote_known_hosts` is only consulted for unpinned entries.
    /// Keying trust here rather than on the address is what stops an address edit from silently
    /// reverting the entry to trust-on-first-use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

const MAX_NAME: usize = 64;

/// `[A-Za-z0-9._-]{1,64}`. Kept deliberately narrow: the name becomes a socket *directory* name
/// (`<runtime>/clowder/remote/<name>/`) in M11b, so path separators and whitespace are out.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name must not be empty".into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!("name must be at most {MAX_NAME} characters"));
    }
    if let Some(bad) = name.chars().find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))) {
        return Err(format!("name may only contain letters, digits, '.', '_' and '-' (found {bad:?})"));
    }
    // '.' is in the allowed set (so "a.b" works), which lets the two path-traversal names through
    // the character check. `..` is exactly the non-separator escape from the per-host socket
    // directory this name becomes (`<runtime>/clowder/remote/<name>/`), and `.` names the
    // directory itself.
    if name == "." || name == ".." {
        return Err("name must not be '.' or '..'".into());
    }
    Ok(())
}

/// A certificate pin as it is stored and as `remote_known_hosts` records it: lowercase hex, even
/// length. Whitespace matters more than it looks — `remote_known_hosts` lines are
/// `"{address} {fingerprint}"` and are read back with `split_whitespace()`, so a fingerprint
/// containing a space would be silently truncated on read and the pin would never match again.
pub fn validate_fingerprint(fp: &str) -> Result<(), String> {
    if fp.is_empty() {
        return Err("fingerprint must not be empty (expected lowercase hex)".into());
    }
    if fp.len() % 2 != 0 || !fp.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)) {
        return Err(format!(
            "fingerprint must be lowercase hex with an even number of digits, e.g. the value \
             printed by `clowder remote probe` (got {fp:?})"
        ));
    }
    Ok(())
}

/// Requires an explicit port: `host:port`, or `[v6]:port` for a bracketed IPv6 literal.
/// There is no default port to fall back on — the daemon's `[remote] listen` is operator-chosen.
pub fn validate_address(address: &str) -> Result<(), String> {
    // Whitespace anywhere is fatal, not cosmetic: `remote_known_hosts` lines are
    // `"{address} {fingerprint}"`, read back with `split_whitespace()`, so `"a b:7777"` would
    // write a line whose first token is `a`, never match on read, and grow the file without bound.
    if address.chars().any(char::is_whitespace) {
        return Err(format!("address must not contain whitespace (got {address:?})"));
    }
    let (host, port) = split_host_port(address)
        .ok_or_else(|| format!("address must be host:port or [ipv6]:port (got {address:?})"))?;
    if host.is_empty() {
        return Err(format!("address is missing a host (got {address:?})"));
    }
    match port.parse::<u16>() {
        Ok(0) | Err(_) => Err(format!("address has an invalid port (got {port:?})")),
        Ok(_) => Ok(()),
    }
}

/// Split `host:port` / `[v6]:port`. Returns None when there is no port, or when a bare
/// (unbracketed) IPv6 literal makes the split ambiguous.
fn split_host_port(s: &str) -> Option<(&str, &str)> {
    if let Some(rest) = s.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        Some((host, tail.strip_prefix(':')?))
    } else {
        let (host, port) = s.rsplit_once(':')?;
        // "::1:7777" is a bare v6 literal, not host:port — require brackets.
        if host.contains(':') {
            return None;
        }
        Some((host, port))
    }
}

/// `$CLOWDER_HOSTS_FILE` › `<remote_state_dir()>/hosts.json` — the directory that already holds
/// `remote_known_hosts`, the remote TLS creds, and the daemon's `agents.json`/`projects.json`.
pub fn remote_hosts_path() -> PathBuf {
    match std::env::var("CLOWDER_HOSTS_FILE") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => crate::remote_state_dir().join("hosts.json"),
    }
}

/// The durable host list. Shaped like `clowder-daemon`'s `JsonStore` — `load` never panics, and
/// `try_mutate` surfaces write errors because these operations answer a user request — with two
/// differences that matter here:
///
/// 1. **0600, created private.** The file holds bearer tokens, and the temp file is opened with
///    `mode(0o600)` BEFORE the rename rather than chmod'd after (which would leave a window in
///    which the token is world-readable).
/// 2. **A cross-process advisory lock.** The daemon gets mutual exclusion from its single-instance
///    flock; the CLI has none, and both a shell (`clowder remote add`) and the app's Settings pane
///    write this file interactively. Without the lock, one writer's load-modify-write silently
///    discards the other's.
pub struct HostsStore {
    path: PathBuf,
}

impl HostsStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The store at `remote_hosts_path()`.
    pub fn default_store() -> Self {
        Self::new(remote_hosts_path())
    }

    /// Current contents. Missing = empty; corrupt = empty + a warning. Never panics: a corrupt
    /// registry must not stop the app from reaching its local daemon.
    pub fn load(&self) -> Vec<HostRecord> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                eprintln!(
                    "clowder-config: host registry {} is unreadable ({e}); starting empty",
                    self.path.display()
                );
                Vec::new()
            }),
            Err(_) => Vec::new(),
        }
    }

    /// Like `load`, but a file that exists and does not parse is an ERROR rather than an empty
    /// list. Only `try_mutate` uses this: `load`'s tolerance is what keeps a corrupt registry from
    /// stopping the app reaching its local daemon, but tolerating it on the WRITE path would
    /// serialize the empty fallback straight back over the file — one hand-edit typo followed by
    /// any `clowder remote add|set|rm|trust|untrust` would delete every bearer token and every
    /// trust pin. `agents.json` can afford that; a file of secrets and trust decisions cannot.
    fn load_for_mutation(&self) -> Result<Vec<HostRecord>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            // Missing is the ordinary "first write" case; anything else (a permissions problem,
            // an I/O error) means we cannot know what we would be overwriting.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(anyhow::Error::from(e))
                    .with_context(|| format!("read host registry {}", self.path.display()))
            }
        };
        // A zero-length file has nothing to preserve, so treat it as "empty" rather than wedging
        // the CLI on a truncated write from an older crash.
        if bytes.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(Vec::new());
        }
        serde_json::from_slice(&bytes).map_err(|e| {
            anyhow::anyhow!(
                "host registry {} is not valid JSON ({e}); refusing to overwrite it — it holds \
                 your bearer tokens and certificate pins. Fix or move the file, then retry.",
                self.path.display()
            )
        })
    }

    /// Load, apply `f`, write back — the whole cycle under an exclusive advisory lock held on a
    /// SEPARATE `.lock` file. It has to be separate: the data file is replaced by `rename`, so a
    /// lock held on its inode would not be seen by the next writer.
    ///
    /// A corrupt existing file aborts the mutation (see `load_for_mutation`) — nothing is written.
    pub fn try_mutate<R>(&self, f: impl FnOnce(&mut Vec<HostRecord>) -> R) -> Result<R> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create {}", dir.display()))?;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let _guard = FileLock::acquire(&lock_path(&self.path))?;
        let mut all = self.load_for_mutation()?;
        let out = f(&mut all);
        let bytes = serde_json::to_vec_pretty(&all)?;
        write_atomic_0600(&self.path, &bytes)?;
        Ok(out)
    }
}

fn lock_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// An exclusive advisory `flock`, released when dropped (or when the process dies) — the same
/// primitive and crate the daemon's `InstanceLock` uses, but BLOCKING: two interactive writers
/// should serialize, not fail.
struct FileLock {
    // Held only for its lifetime — dropping it closes the fd, which releases the flock. Named
    // `_file` (matching `InstanceLock`'s convention) so the compiler doesn't flag it as dead code.
    _file: std::fs::File,
}

impl FileLock {
    fn acquire(path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("open lock {}", path.display()))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .with_context(|| format!("lock {}", path.display()))?;
        Ok(Self { _file: file })
    }
}

/// Create `path` for writing, failing if it already exists, with 0600 from the moment it exists.
fn create_private(path: &Path) -> Result<std::fs::File> {
    Ok(std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?)
}

/// Write `bytes` to `path` atomically, never widening permissions and never leaving a temp file.
fn write_atomic_0600(path: &Path, bytes: &[u8]) -> Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = PathBuf::from(tmp);

    let mut f = create_private(&tmp)?;
    if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).context("write host registry");
    }
    drop(f);
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("replace {}", path.display()));
    }
    // A pre-existing file keeps ITS mode through a rename on some filesystems; make sure.
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

/// Where an entry came from. `Config` entries are read-only: they live in `config.toml`, which
/// this code never rewrites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSource {
    Registry,
    Config,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    pub record: HostRecord,
    pub source: HostSource,
}

/// The user-visible host list: the registry file, plus `[remote] host` as a **virtual** entry.
///
/// Pure — no I/O — so it is table-testable, exactly like `Config::resolve`.
///
/// The config entry is never written back to the registry. A one-time migration was rejected
/// because it would make a later hand-edit of `config.toml` silently stop taking effect, and the
/// migration itself could clobber. This way `config.toml` stays authoritative forever and the
/// merge is idempotent.
pub fn merged_hosts(file: Vec<HostRecord>, cfg: &crate::Config) -> Vec<HostEntry> {
    let mut out: Vec<HostEntry> = file
        .into_iter()
        .map(|record| HostEntry { record, source: HostSource::Registry })
        .collect();

    let Some(address) = cfg.remote_host.clone() else {
        return out;
    };
    if out.iter().any(|e| e.record.address == address) {
        return out; // the file record wins entirely
    }

    // The returned list never contains two entries with the same name. The validators'
    // disjoint character sets (validate_name forbids ':', validate_address requires ':')
    // make a name-address collision unreachable for validated data. However, HostsStore::load
    // does not re-validate on load, and the registry is meant to be hand-editable, so a
    // hand-edited file can reach this case. We guarantee uniqueness by trying preferred
    // names in order, then suffixing with -2, -3, etc. until one is available.
    let name = find_unique_name(&out, "config", &address);
    out.push(HostEntry {
        record: HostRecord {
            name,
            address,
            // A configured token is only ever useful over TLS, and `docs/remote-tls.md` documents
            // `tls` as a DAEMON key — so every existing client with a token has `remote_tls == false`
            // and would be silently downgraded to plaintext without this `||`.
            tls: cfg.remote_tls || cfg.remote_token.is_some(),
            token: cfg.remote_token.clone(),
            fingerprint: None,
        },
        source: HostSource::Config,
    });
    out
}

/// Find a unique name by preferring the primary choice, then falling back to the secondary,
/// then suffixing the secondary with -2, -3, etc. until a name is found that does not
/// appear in the existing entries.
fn find_unique_name(entries: &[HostEntry], primary: &str, secondary: &str) -> String {
    if !entries.iter().any(|e| e.record.name == primary) {
        return primary.to_string();
    }
    if !entries.iter().any(|e| e.record.name == secondary) {
        return secondary.to_string();
    }
    for i in 2.. {
        let candidate = format!("{secondary}-{i}");
        if !entries.iter().any(|e| e.record.name == candidate) {
            return candidate;
        }
    }
    unreachable!("must find a unique name before running out of suffix space")
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn rec(name: &str, address: &str) -> HostRecord {
        HostRecord {
            name: name.into(),
            address: address.into(),
            tls: false,
            token: None,
            fingerprint: None,
        }
    }

    fn cfg_with_host(host: Option<&str>, tls: bool, token: Option<&str>) -> crate::Config {
        crate::Config {
            remote_host: host.map(String::from),
            remote_tls: tls,
            remote_token: token.map(String::from),
            ..crate::Config::default()
        }
    }

    #[test]
    fn valid_names_are_accepted_and_invalid_ones_rejected() {
        for good in ["studio", "mac-studio", "box_1", "a.b", "A", &"x".repeat(64)] {
            assert!(validate_name(good).is_ok(), "{good:?} should be valid");
        }
        // "." and ".." pass the character check ('.' is allowed so "a.b" works) but must be
        // rejected outright: the name becomes a socket directory name in M11b, and ".." is
        // precisely the traversal that needs no path separator.
        for bad in ["", "has space", "sl/ash", "quote\"", &"x".repeat(65), "tab\there", ".", ".."] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be invalid");
        }
        // A dot elsewhere is still fine — the rule is exact-match, not "contains a dot".
        for good in ["...", "a..b", ".a", "a."] {
            assert!(validate_name(good).is_ok(), "{good:?} should still be valid");
        }
    }

    #[test]
    fn fingerprints_must_be_even_length_lowercase_hex() {
        for good in ["aa11", "00", &"ab".repeat(32)] {
            assert!(validate_fingerprint(good).is_ok(), "{good:?} should be valid");
        }
        // Whitespace is the one that actually corrupts state: `remote_known_hosts` is
        // whitespace-delimited, so "aa 11" would be truncated to "aa" on read.
        for bad in ["", "aa 11", "aa\t11", "AA11", "aa1", "zz11", "aa:11", "aa11\n"] {
            assert!(validate_fingerprint(bad).is_err(), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn addresses_require_a_host_and_a_port() {
        for good in ["h:7777", "10.0.0.5:1", "studio.tail1234.ts.net:7777", "[::1]:7777", "[fd7a::1]:22"] {
            assert!(validate_address(good).is_ok(), "{good:?} should be valid");
        }
        // Whitespace anywhere is invalid: `remote_known_hosts` lines are whitespace-delimited, so
        // "a b:7777" would write a line keyed on "a" that never matches on read, appending a new
        // line on every single connect.
        for bad in [
            "", "h", "h:", ":7777", "h:0", "h:70000", "h:abc", "::1:7777", "[::1]7777",
            "a b:7777", " h:7777", "h:7777 ", "h :7777", "h:77\t77", "h:7777\n",
        ] {
            assert!(validate_address(bad).is_err(), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn record_json_is_camel_case_and_omits_empty_optionals() {
        let r = HostRecord {
            name: "studio".into(),
            address: "studio.tail:7777".into(),
            tls: true,
            token: None,
            fingerprint: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"name":"studio","address":"studio.tail:7777","tls":true}"#);
    }

    #[test]
    fn record_defaults_missing_fields() {
        // Forward-compat: a record written by an older/newer clowder that omits the optional
        // fields must still load, the way AgentRecord::tree does.
        let r: HostRecord = serde_json::from_str(r#"{"name":"a","address":"h:1"}"#).unwrap();
        assert!(!r.tls);
        assert_eq!(r.token, None);
        assert_eq!(r.fingerprint, None);
    }

    #[test]
    fn name_validation_matches_the_shared_fixture() {
        // The same fixture drives Swift's HostDraft.nameError in M11c, so the two validators
        // cannot drift. Mirrors clowder-workspace's worktree-names.json check.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/protocol/fixtures/host-names.json");
        let text = std::fs::read_to_string(path).expect("fixture readable");
        let cases: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        assert!(!cases.is_empty(), "fixture must not be empty");
        for c in cases {
            let name = c["name"].as_str().unwrap();
            let want = c["valid"].as_bool().unwrap();
            assert_eq!(validate_name(name).is_ok(), want, "fixture case {name:?}");
        }
    }

    use std::os::unix::fs::PermissionsExt;

    fn mode_of(p: &std::path::Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn missing_file_loads_empty_and_try_mutate_creates_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("hosts.json");
        let store = HostsStore::new(p.clone());
        assert!(store.load().is_empty());
        store.try_mutate(|all| all.push(rec("studio", "h:7777"))).unwrap();
        assert!(p.exists(), "try_mutate must create the file and its parent dir");
        assert_eq!(store.load(), vec![rec("studio", "h:7777")]);
    }

    #[test]
    fn corrupt_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hosts.json");
        std::fs::write(&p, b"not json").unwrap();
        assert!(HostsStore::new(p).load().is_empty(), "must never panic on a corrupt file");
    }

    #[test]
    fn written_file_is_0600_and_its_dir_0700() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("private").join("hosts.json");
        let store = HostsStore::new(p.clone());
        store.try_mutate(|all| all.push(rec("studio", "h:7777"))).unwrap();
        assert_eq!(mode_of(&p), 0o600, "the file holds bearer tokens");
        assert_eq!(mode_of(p.parent().unwrap()), 0o700);
    }

    #[test]
    fn a_corrupt_file_is_never_overwritten_by_a_mutation() {
        // The data-loss case: `load` falls back to an empty vec on a parse error, so a
        // `try_mutate` that reused it would serialize that empty vec straight over a file full of
        // bearer tokens and certificate pins. One hand-edit typo would silently delete them all.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hosts.json");
        let original = br#"[{"name":"studio","address":"h:7777","tls":true,"token":"s3cr3t","fingerprint":"aa11"},"#;
        std::fs::write(&p, original).unwrap(); // valid-looking, but truncated: not parseable
        let store = HostsStore::new(p.clone());

        let err = store.try_mutate(|all| all.push(rec("new", "h:1"))).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("hosts.json"), "the error must name the file: {msg}");

        // The whole point: the tokens are still on disk, byte for byte.
        assert_eq!(std::fs::read(&p).unwrap(), original, "the original bytes must survive");
    }

    #[test]
    fn an_empty_file_is_treated_as_an_empty_registry() {
        // Nothing to preserve in a zero-length file, so it must not wedge every later mutation.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hosts.json");
        std::fs::write(&p, b"  \n").unwrap();
        let store = HostsStore::new(p.clone());
        store.try_mutate(|all| all.push(rec("studio", "h:7777"))).unwrap();
        assert_eq!(store.load(), vec![rec("studio", "h:7777")]);
    }

    #[test]
    fn rewriting_a_too_wide_file_tightens_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hosts.json");
        std::fs::write(&p, b"[]").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let store = HostsStore::new(p.clone());
        store.try_mutate(|all| all.push(rec("studio", "h:7777"))).unwrap();
        assert_eq!(mode_of(&p), 0o600);
    }

    #[test]
    fn the_temp_file_is_created_private_not_chmodded_after() {
        // The window between create-then-chmod is exactly when a token would be world-readable,
        // so assert the primitive itself, not just the end state.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t");
        let f = create_private(&p).unwrap();
        drop(f);
        assert_eq!(mode_of(&p), 0o600);
        // create_private must refuse an existing path, so it can never adopt another writer's temp.
        assert!(create_private(&p).is_err());
    }

    #[test]
    fn no_temp_files_are_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hosts.json");
        let store = HostsStore::new(p);
        store.try_mutate(|all| all.push(rec("studio", "h:7777"))).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
    }

    #[test]
    fn concurrent_try_mutate_does_not_lose_records() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hosts.json");
        let handles: Vec<_> = (0..16u32)
            .map(|i| {
                // A store PER THREAD, sharing only the path: this exercises the cross-process
                // flock, not an in-process mutex. That is the case the CLI + app actually hit.
                let p = p.clone();
                std::thread::spawn(move || {
                    let s = Arc::new(HostsStore::new(p));
                    s.try_mutate(|all| all.push(rec(&format!("h{i}"), "h:7777"))).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let mut names: Vec<String> = HostsStore::new(p).load().into_iter().map(|r| r.name).collect();
        names.sort();
        let mut want: Vec<String> = (0..16).map(|i| format!("h{i}")).collect();
        want.sort();
        assert_eq!(names, want);
    }

    #[test]
    fn try_mutate_surfaces_write_failures() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"i am a file").unwrap();
        let store = HostsStore::new(blocker.join("hosts.json"));
        assert!(store.try_mutate(|all| all.push(rec("a", "h:1"))).is_err());
    }

    #[test]
    fn default_path_honors_the_env_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CLOWDER_HOSTS_FILE", "/custom/hosts.json");
        assert_eq!(remote_hosts_path(), std::path::PathBuf::from("/custom/hosts.json"));
        std::env::remove_var("CLOWDER_HOSTS_FILE");
        std::env::set_var("XDG_STATE_HOME", "/xdg/state");
        assert_eq!(remote_hosts_path(), std::path::PathBuf::from("/xdg/state/clowder/hosts.json"));
        std::env::remove_var("XDG_STATE_HOME");
    }

    #[test]
    fn file_records_come_first_in_file_order() {
        let file = vec![rec("b", "hb:1"), rec("a", "ha:1")];
        let out = merged_hosts(file, &cfg_with_host(None, false, None));
        assert_eq!(out.iter().map(|e| e.record.name.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert!(out.iter().all(|e| e.source == HostSource::Registry));
    }

    #[test]
    fn config_host_appears_as_a_virtual_entry() {
        let out = merged_hosts(vec![], &cfg_with_host(Some("10.0.0.5:7777"), false, Some("tok")));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, HostSource::Config);
        assert_eq!(out[0].record.name, "config");
        assert_eq!(out[0].record.address, "10.0.0.5:7777");
        assert_eq!(out[0].record.token.as_deref(), Some("tok"));
        // A configured token implies TLS even when [remote] tls is unset — docs/remote-tls.md
        // tells clients to set only host + token, so every existing TLS user lands here.
        assert!(out[0].record.tls);
    }

    #[test]
    fn config_host_without_a_token_is_plaintext_unless_tls_is_set() {
        let out = merged_hosts(vec![], &cfg_with_host(Some("h:1"), false, None));
        assert!(!out[0].record.tls);
        let out = merged_hosts(vec![], &cfg_with_host(Some("h:1"), true, None));
        assert!(out[0].record.tls);
    }

    #[test]
    fn a_file_record_with_the_same_address_wins_entirely() {
        // No per-field merging: nobody can debug "why is my config token overriding my registry token".
        let mut r = rec("studio", "10.0.0.5:7777");
        r.token = Some("registry-token".into());
        let out = merged_hosts(vec![r], &cfg_with_host(Some("10.0.0.5:7777"), false, Some("config-token")));
        assert_eq!(out.len(), 1, "the config host must not be added twice");
        assert_eq!(out[0].source, HostSource::Registry);
        assert_eq!(out[0].record.token.as_deref(), Some("registry-token"));
    }

    #[test]
    fn a_taken_config_name_falls_back_to_the_address() {
        // A registry entry already NAMED "config", at a different address.
        let out = merged_hosts(vec![rec("config", "other:1")], &cfg_with_host(Some("h:2"), false, None));
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].record.name, "h:2");
        assert_eq!(out[1].source, HostSource::Config);
    }

    #[test]
    fn no_config_host_means_no_virtual_entry() {
        let out = merged_hosts(vec![rec("a", "h:1")], &cfg_with_host(None, false, None));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, HostSource::Registry);
    }

    #[test]
    fn chained_collision_remains_unique() {
        // Regression test: a hand-edited registry with both an entry named "config" and
        // an entry whose name equals the config host's address. The uniqueness guarantee
        // must still hold even though the primary and secondary names are both taken.
        let file = vec![
            rec("config", "other:1"),          // primary name taken
            rec("10.0.0.5:7777", "other:2"),   // secondary name (address) taken
        ];
        let out = merged_hosts(file, &cfg_with_host(Some("10.0.0.5:7777"), false, None));
        assert_eq!(out.len(), 3, "both file records plus the config virtual entry");

        let names: Vec<&str> = out.iter().map(|e| e.record.name.as_str()).collect();
        // Check all names are distinct (no duplicates).
        let mut names_sorted = names.clone();
        names_sorted.sort();
        names_sorted.dedup();
        assert_eq!(names_sorted.len(), names.len(), "all names must be unique, got: {names:?}");
        // Verify the virtual entry got a suffixed fallback name.
        assert_eq!(out[2].source, HostSource::Config);
        assert_eq!(out[2].record.name, "10.0.0.5:7777-2", "should suffix when primary and secondary are taken");
    }

    #[test]
    fn merged_hosts_is_idempotent() {
        // Feeding merged_hosts output back in as the file argument must not duplicate
        // the config entry. This is the property claimed in the function's doc comment.
        let orig_file = vec![rec("studio", "s:7777")];
        let cfg = cfg_with_host(Some("h:2"), false, None);

        let first = merged_hosts(orig_file.clone(), &cfg);
        assert_eq!(first.len(), 2, "original file record + config virtual");

        // Convert back to HostRecords (simulating a second load).
        let second_input: Vec<HostRecord> = first.iter().map(|e| e.record.clone()).collect();
        let second = merged_hosts(second_input, &cfg);

        assert_eq!(second.len(), first.len(), "merging the output should produce the same length");
        assert_eq!(
            second.iter().map(|e| &e.record).collect::<Vec<_>>(),
            first.iter().map(|e| &e.record).collect::<Vec<_>>(),
            "all records must be identical on re-merge"
        );
    }
}
