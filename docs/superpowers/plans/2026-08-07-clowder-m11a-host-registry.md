# M11a — Remote host registry, CLI, and probe/pairing primitives

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended)
> or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax
> for tracking.

**Goal:** Give clowder a persistent, nicknamed list of remote daemons — managed entirely from the CLI with
no daemon running — plus a probe/trust pairing flow that shows a daemon's certificate fingerprint before
anything is pinned.

**Architecture:** A `HostsStore` in `clowder-config` owns `hosts.json` in the state dir (0600, flock'd,
atomically written). `[remote] host` is merged in as a **virtual, read-only** entry by a pure
`merged_hosts` function, so `config.toml` stays authoritative and is never rewritten. `clowder-client`'s
TOFU verifier becomes a three-armed `Trust` policy (`Pinned` / `Tofu` / `Capture`), which lets a new
`probe` observe a daemon's fingerprint without persisting it. A `RemoteTarget` carries address, token,
TLS flag, and pin together, decoupling "use TLS" from "a token exists". A new `clowder remote`
subcommand tree wraps all of it with a JSON stdout contract the macOS app will consume in M11b.

**Tech Stack:** Rust 2021, tokio, serde/serde_json, rustls/tokio-rustls (already vendored), rustix (flock).
No new crates beyond `serde_json` + `rustix` in `clowder-config`.

**Spec:** `docs/superpowers/specs/2026-08-07-clowder-m11-remote-host-management-design.md`

## Global Constraints

- **Prefix every cargo command** with `source "$HOME/.cargo/env" && ` — rustup is not auto-sourced here.
- Rust edition 2021, workspace version 0.5.1. CI runs `cargo test --workspace --locked`, so commit
  `Cargo.lock` whenever a dependency changes.
- **No clap.** `crates/clowder-client/src/main.rs` uses hand-rolled `std::env::args()` dispatch and must
  keep doing so; flags are parsed by the ~30-line `parse_flags` written in Task 8.
- **All JSON is `#[serde(rename_all = "camelCase")]`** — the house convention (`clowder-proto/src/control.rs`)
  and what lets Swift `Codable` decode with default key coding.
- **The token is never printed and never passed in argv.** `list`/`show` emit `hasToken: bool`; the app
  passes secrets via `--token-stdin` because argv is world-readable through `ps`.
- **Files holding tokens are `0600`, their directory `0700`**, and the atomic temp file must be *created*
  private, not chmod'd after.
- **Conventional Commits** — `type(scope): subject`. Run `scripts/check-commit-messages.sh` before pushing.
  The type drives the released version, so use `feat` for new surface and `fix` only for real defects.
- Tests that set `XDG_STATE_HOME` (or any process-global env var) **must hold the test module's `ENV_LOCK`
  mutex for their whole span** — `crates/clowder-client/src/tofu.rs:118` has the existing one.
- Back-compat is non-negotiable in two places: `clowder connect <host:port>` must behave exactly as it does
  today, and every existing TLS user has `remote_tls == false` (they set only `host` + `token`, per
  `docs/remote-tls.md`), so the config-derived path must compute `tls = remote_tls || token.is_some()`.

## File structure

| File | Responsibility |
|---|---|
| `crates/clowder-config/src/hosts.rs` (new) | `HostRecord`, name/address validation, `HostsStore` (flock + 0600 atomic write), `remote_hosts_path()`, `merged_hosts` |
| `crates/clowder-config/src/lib.rs` | add `pub mod hosts;` only |
| `crates/clowder-client/src/tofu.rs` | `Trust` (3 arms), `RemoteVerifier`, `connector(Trust)`; `check()` unchanged |
| `crates/clowder-client/src/forward.rs` | `RemoteTarget`, `forward(target, dir)`, `forward_stream(local, target, channel)` |
| `crates/clowder-client/src/target.rs` (new) | pure `resolve_target` |
| `crates/clowder-client/src/probe.rs` (new) | `ProbeResult`, `probe(target, timeout)` |
| `crates/clowder-client/src/remote_cli.rs` (new) | `parse_flags`, JSON view types, the 8 `remote` subcommands |
| `crates/clowder-client/src/main.rs` | one new `remote` arm; `connect` gains registry resolution, `--socket-dir`, exit 4 |
| `docs/protocol/fixtures/{remote-host-list,remote-probe,host-names}.json` (new) | cross-language golden fixtures |
| `docs/protocol/README.md`, `docs/remote-tls.md`, `README.md`, `AGENTS.md` | documentation |

---

### Task 1: `HostRecord` + validation + the shared name fixture

**Files:**
- Create: `crates/clowder-config/src/hosts.rs`
- Modify: `crates/clowder-config/src/lib.rs` (add `pub mod hosts;`), `crates/clowder-config/Cargo.toml`
- Create: `docs/protocol/fixtures/host-names.json`

**Interfaces:**
- Produces: `HostRecord { name: String, address: String, tls: bool, token: Option<String>, fingerprint: Option<String> }`;
  `pub fn validate_name(&str) -> Result<(), String>`; `pub fn validate_address(&str) -> Result<(), String>`.

- [ ] **Step 1: Add the dependency**

In `crates/clowder-config/Cargo.toml`, under `[dependencies]`:

```toml
serde_json = "1"
rustix = { version = "0.38", features = ["fs"] }
```

Both versions match what `crates/clowder-daemon/Cargo.toml` already pins, so `Cargo.lock` gains no new
entries. `serde` is already there; change it to include derive if it doesn't (it uses the workspace dep,
which already has `features = ["derive"]`).

- [ ] **Step 2: Write the failing tests**

Create `crates/clowder-config/src/hosts.rs` with only the test module plus `use` lines:

```rust
//! The remote host registry: a nicknamed list of remote daemons, owned by the CLI (not the daemon)
//! so it stays readable and writable when nothing is reachable.

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names_are_accepted_and_invalid_ones_rejected() {
        for good in ["studio", "mac-studio", "box_1", "a.b", "A", &"x".repeat(64)] {
            assert!(validate_name(good).is_ok(), "{good:?} should be valid");
        }
        for bad in ["", "has space", "sl/ash", "quote\"", &"x".repeat(65), "tab\there"] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn addresses_require_a_host_and_a_port() {
        for good in ["h:7777", "10.0.0.5:1", "studio.tail1234.ts.net:7777", "[::1]:7777", "[fd7a::1]:22"] {
            assert!(validate_address(good).is_ok(), "{good:?} should be valid");
        }
        for bad in ["", "h", "h:", ":7777", "h:0", "h:70000", "h:abc", "::1:7777", "[::1]7777"] {
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
}
```

Add `pub mod hosts;` to the top of `crates/clowder-config/src/lib.rs`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config hosts`
Expected: FAIL — `cannot find function validate_name`, `cannot find type HostRecord`.

- [ ] **Step 4: Write the fixture**

Create `docs/protocol/fixtures/host-names.json`:

```json
[
  { "name": "studio", "valid": true },
  { "name": "mac-studio", "valid": true },
  { "name": "box_1", "valid": true },
  { "name": "a.b", "valid": true },
  { "name": "A", "valid": true },
  { "name": "", "valid": false },
  { "name": "has space", "valid": false },
  { "name": "sl/ash", "valid": false },
  { "name": "back\\slash", "valid": false },
  { "name": "quote\"", "valid": false },
  { "name": "colon:name", "valid": false }
]
```

- [ ] **Step 5: Implement the record and validators**

Add above the test module in `crates/clowder-config/src/hosts.rs`:

```rust
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
    Ok(())
}

/// Requires an explicit port: `host:port`, or `[v6]:port` for a bracketed IPv6 literal.
/// There is no default port to fall back on — the daemon's `[remote] listen` is operator-chosen.
pub fn validate_address(address: &str) -> Result<(), String> {
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
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config hosts`
Expected: PASS — 5 tests.

- [ ] **Step 7: Commit**

```bash
git add crates/clowder-config/src/hosts.rs crates/clowder-config/src/lib.rs \
        crates/clowder-config/Cargo.toml Cargo.lock docs/protocol/fixtures/host-names.json
git commit -m "feat(config): add HostRecord and host name/address validation"
```

---

### Task 2: `HostsStore` — 0600, atomic, flock'd

**Files:**
- Modify: `crates/clowder-config/src/hosts.rs`

**Interfaces:**
- Consumes: `HostRecord` (Task 1).
- Produces: `pub fn remote_hosts_path() -> PathBuf`; `HostsStore::new(PathBuf)`, `HostsStore::default_store()`,
  `HostsStore::load() -> Vec<HostRecord>`, `HostsStore::try_mutate<R>(impl FnOnce(&mut Vec<HostRecord>) -> R) -> anyhow::Result<R>`.

**Why not reuse `clowder-daemon`'s `JsonStore`:** it lives in the wrong crate (`clowder-client` only
*dev*-depends on `clowder-daemon`), it writes its temp file with `std::fs::write` (0644 by umask — this
file holds bearer tokens), and it relies on the daemon's single-instance flock for cross-process safety,
which the CLI does not have. We mirror its *shape* and its test names, not its code.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `crates/clowder-config/src/hosts.rs`:

```rust
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
```

Also add these helpers at the top of the `tests` module (the `ENV_LOCK` guards the process-global env
vars this file's tests set — the same discipline as `tofu.rs:118`):

```rust
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
```

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config hosts`
Expected: FAIL — `cannot find type HostsStore`, `cannot find function create_private`, `remote_hosts_path`.

- [ ] **Step 3: Implement the store**

Append to `crates/clowder-config/src/hosts.rs` (above the tests):

```rust
use anyhow::{Context, Result};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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

    /// Load, apply `f`, write back — the whole cycle under an exclusive advisory lock held on a
    /// SEPARATE `.lock` file. It has to be separate: the data file is replaced by `rename`, so a
    /// lock held on its inode would not be seen by the next writer.
    pub fn try_mutate<R>(&self, f: impl FnOnce(&mut Vec<HostRecord>) -> R) -> Result<R> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create {}", dir.display()))?;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let _guard = FileLock::acquire(&lock_path(&self.path))?;
        let mut all = self.load();
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
struct FileLock(std::fs::File);

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
        Ok(Self(file))
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
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config hosts`
Expected: PASS — 14 tests.

If `concurrent_try_mutate_does_not_lose_records` is flaky, the lock is not covering the read: confirm
`self.load()` is called *after* `FileLock::acquire`, not before.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-config/src/hosts.rs
git commit -m "feat(config): add a 0600, flock'd host registry store"
```

---

### Task 3: `merged_hosts` — `[remote] host` as a virtual entry

**Files:**
- Modify: `crates/clowder-config/src/hosts.rs`

**Interfaces:**
- Consumes: `HostRecord` (Task 1), `Config` (existing, `crates/clowder-config/src/lib.rs:10`).
- Produces: `pub enum HostSource { Registry, Config }`; `pub struct HostEntry { pub record: HostRecord, pub source: HostSource }`;
  `pub fn merged_hosts(file: Vec<HostRecord>, cfg: &Config) -> Vec<HostEntry>`.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module:

```rust
    fn cfg_with_host(host: Option<&str>, tls: bool, token: Option<&str>) -> crate::Config {
        crate::Config {
            remote_host: host.map(String::from),
            remote_tls: tls,
            remote_token: token.map(String::from),
            ..crate::Config::default()
        }
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
```

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config hosts`
Expected: FAIL — `cannot find function merged_hosts`.

Note: `Config` must derive `Default` for `..Config::default()` to work — it already implements `Default`
manually at `crates/clowder-config/src/lib.rs:127`, and struct-update syntax works with that. All `Config`
fields are already `pub`.

- [ ] **Step 3: Implement**

```rust
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
pub fn merged_hosts(file: Vec<HostRecord>, cfg: &Config) -> Vec<HostEntry> {
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
    // "config" is the friendly default name; fall back to the address if a registry entry
    // already took it, so the list never contains two entries with the same name.
    let name = if out.iter().any(|e| e.record.name == "config") {
        address.clone()
    } else {
        "config".to_string()
    };
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
```

Add `use crate::Config;` to the module's imports.

- [ ] **Step 4: Run to verify the tests pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config`
Expected: PASS — the whole crate, 20 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-config/src/hosts.rs
git commit -m "feat(config): merge [remote] host into the registry as a virtual entry"
```

---

### Task 4: Three-armed `Trust` policy

**Files:**
- Modify: `crates/clowder-client/src/tofu.rs`, `crates/clowder-client/src/lib.rs`

**Interfaces:**
- Produces: `pub enum Trust { Pinned(String), Tofu { host: String, known_hosts: PathBuf }, Capture(Arc<Mutex<Option<String>>>) }`;
  `pub struct RemoteVerifier`; `pub fn connector(trust: Trust) -> Arc<ClientConfig>`.
- `check()` and `known_hosts_path()` are unchanged — `Trust::Tofu` is exactly today's behavior.

**`tofu` must become a public module.** It is `mod tofu;` at `crates/clowder-client/src/lib.rs:26`
today, which was fine while nothing public mentioned its types. Task 5 gives the public
`forward::RemoteTarget` a `trust() -> Trust` method, and a private type in a public signature is a hard
compile error (E0446). Change line 26 to `pub mod tofu;` as part of this task.

**Breaking change:** `connector` changes signature from `connector(host: &str)`. Its two callers are
`forward.rs:47` (updated in Task 5) and the e2e test at `tofu.rs:147`/`:165` (updated here).

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `crates/clowder-client/src/tofu.rs`:

```rust
    use std::sync::{Arc, Mutex};

    /// Build a real self-signed cert so the verifier sees a genuine DER, not a fabricated one.
    fn a_cert() -> (Vec<u8>, String) {
        let c = rcgen::generate_simple_self_signed(vec!["clowder".to_string()]).unwrap();
        let der = c.cert.der().to_vec();
        let fp = clowder_proto::cert_fingerprint_hex(&der);
        (der, fp)
    }

    fn verify(trust: Trust, der: &[u8]) -> Result<(), String> {
        use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
        let v = RemoteVerifier {
            trust,
            provider: Arc::new(tokio_rustls::rustls::crypto::ring::default_provider()),
        };
        v.verify_server_cert(
            &CertificateDer::from(der.to_vec()),
            &[],
            &ServerName::try_from("clowder").unwrap(),
            &[],
            UnixTime::now(),
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    #[test]
    fn pinned_accepts_the_matching_fingerprint_and_refuses_any_other() {
        let (der, fp) = a_cert();
        assert!(verify(Trust::Pinned(fp.clone()), &der).is_ok());
        let err = verify(Trust::Pinned("deadbeef".into()), &der).unwrap_err();
        assert!(err.to_lowercase().contains("changed"), "loud refuse: {err}");
    }

    #[test]
    fn pinned_never_touches_known_hosts() {
        // This is the whole point of pairing: a pinned entry must not consult, and must not
        // silently record into, the TOFU file.
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        let (der, fp) = a_cert();
        assert!(verify(Trust::Pinned(fp), &der).is_ok());
        assert!(!kh.exists(), "Pinned must not write known_hosts");
    }

    #[test]
    fn capture_accepts_anything_and_publishes_the_fingerprint_without_persisting() {
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        let (der, fp) = a_cert();
        let sink: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        assert!(verify(Trust::Capture(sink.clone()), &der).is_ok());
        assert_eq!(sink.lock().unwrap().as_deref(), Some(fp.as_str()));
        assert!(!kh.exists(), "a probe must persist nothing");
    }

    #[test]
    fn tofu_arm_still_records_then_refuses_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        let (der, _) = a_cert();
        let t = || Trust::Tofu { host: "h:7777".into(), known_hosts: kh.clone() };
        assert!(verify(t(), &der).is_ok(), "first sight records");
        assert!(verify(t(), &der).is_ok(), "same cert accepts");
        let (other_der, _) = a_cert();
        assert!(verify(t(), &other_der).is_err(), "changed cert refuses");
    }
```

`rcgen` is not currently a dependency of `clowder-client`. Add it under `[dev-dependencies]` in
`crates/clowder-client/Cargo.toml` (same pin as the daemon):

```toml
rcgen = { version = "0.13", default-features = false, features = ["ring", "pem"] }
```

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client tofu`
Expected: FAIL — `cannot find type Trust`, `cannot find type RemoteVerifier`.

- [ ] **Step 3: Make the module public**

In `crates/clowder-client/src/lib.rs`, change line 26 from `mod tofu;` to `pub mod tofu;`.

- [ ] **Step 4: Replace `TofuVerifier` with `RemoteVerifier`**

In `crates/clowder-client/src/tofu.rs`, replace the `TofuVerifier` struct and its `impl` (lines 46–90)
with:

```rust
/// How to decide whether a presented server certificate is the daemon we meant to reach.
#[derive(Debug, Clone)]
pub enum Trust {
    /// The host entry carries a pinned fingerprint: strict compare, no recording, and
    /// `remote_known_hosts` is never consulted. This is what pairing produces.
    Pinned(String),
    /// No pin yet (or a legacy config-only host): today's record-on-first-sight behavior.
    /// `host` is the key written into `known_hosts` — it must be the DIAL ADDRESS, so entries
    /// recorded by earlier versions keep matching.
    Tofu { host: String, known_hosts: PathBuf },
    /// Probe only: accept whatever is presented, publish its fingerprint, persist nothing.
    Capture(Arc<std::sync::Mutex<Option<String>>>),
}

#[derive(Debug)]
pub struct RemoteVerifier {
    pub trust: Trust,
    pub provider: Arc<tokio_rustls::rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for RemoteVerifier {
    fn verify_server_cert(&self, end_entity: &CertificateDer, _i: &[CertificateDer], _n: &ServerName, _o: &[u8], _t: UnixTime) -> Result<ServerCertVerified, Error> {
        let fp = clowder_proto::cert_fingerprint_hex(end_entity);
        let result = match &self.trust {
            Trust::Pinned(expected) => {
                if fp == *expected {
                    Ok(())
                } else {
                    Err(format!(
                        "REMOTE DAEMON IDENTIFICATION HAS CHANGED: pinned {expected}, got {fp}. \
                         If you rotated the daemon cert, re-pair this host; otherwise this may be a MITM."
                    ))
                }
            }
            Trust::Tofu { host, known_hosts } => check(known_hosts, host, &fp),
            Trust::Capture(sink) => {
                *sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(fp);
                Ok(())
            }
        };
        result.map(|_| ServerCertVerified::assertion()).map_err(Error::General)
    }
    // Fingerprint pinning above proves identity (the peer presented the expected cert), but
    // that alone isn't enough — an active MITM can also hold a copy of that (public) cert. These
    // two checks prove key POSSESSION: the peer signed the handshake transcript with the
    // private key matching the pinned cert, which a MITM without that key cannot forge.
    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        tokio_rustls::rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        tokio_rustls::rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Build a TLS connector that verifies the daemon under `trust`.
pub fn connector(trust: Trust) -> Arc<tokio_rustls::rustls::ClientConfig> {
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let verifier = RemoteVerifier { trust, provider: provider.clone() };
    Arc::new(
        tokio_rustls::rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions().unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth(),
    )
}
```

- [ ] **Step 5: Update the two existing `connector(...)` call sites in the e2e test**

In `crates/clowder-client/src/tofu.rs`, the e2e test builds connectors at what are currently lines 147
and 165. Replace both `connector(TOFU_HOST_LABEL)` calls with:

```rust
        let tls_connector = tokio_rustls::TlsConnector::from(connector(Trust::Tofu {
            host: TOFU_HOST_LABEL.to_string(),
            known_hosts: known_hosts_path(),
        }));
```

and correspondingly for `tls_connector2`. The test's behavior is unchanged — it still records on first
sight and refuses after the cert rotation.

- [ ] **Step 6: Run to verify the tests pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client tofu`
Expected: PASS — 6 tests (4 new + the 2 existing). `forward.rs` will not compile yet; that is Task 5.
To check just this module while `forward.rs` is broken, temporarily comment out `forward`'s `connector`
call — or simply do Tasks 4 and 5 back to back and run the suite once at the end of Task 5.

- [ ] **Step 7: Commit**

```bash
git add crates/clowder-client/src/tofu.rs crates/clowder-client/src/lib.rs \
        crates/clowder-client/Cargo.toml Cargo.lock
git commit -m "feat(client): add pinned and capture trust policies alongside TOFU"
```

---

### Task 5: `RemoteTarget` and decoupling TLS from the token

**Files:**
- Modify: `crates/clowder-client/src/forward.rs`, `crates/clowder-client/src/lib.rs` (module list if needed)

**Interfaces:**
- Consumes: `Trust` (Task 4).
- Produces: `pub struct RemoteTarget { pub label: String, pub address: String, pub token: Option<String>, pub tls: bool, pub fingerprint: Option<String> }`
  with `pub fn trust(&self) -> Trust`; `pub async fn forward(target: RemoteTarget, dir: PathBuf) -> Result<()>`;
  `pub async fn forward_stream<L>(local: L, target: &RemoteTarget, channel: Channel) -> Result<()>`.

**Why `RemoteTarget` holds `fingerprint` rather than a `Trust`:** the target is cloned per accepted
connection, and `Trust::Capture` holds an `Arc<Mutex<…>>` whose sharing across connections would be
meaningless. `trust()` derives a fresh policy per dial.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `crates/clowder-client/src/forward.rs`:

```rust
    fn plain_target(addr: &str) -> RemoteTarget {
        RemoteTarget {
            label: "test".into(),
            address: addr.into(),
            token: None,
            tls: false,
            fingerprint: None,
        }
    }

    #[test]
    fn trust_is_pinned_when_the_entry_has_a_fingerprint() {
        let mut t = plain_target("h:1");
        t.fingerprint = Some("aa11".into());
        assert!(matches!(t.trust(), crate::tofu::Trust::Pinned(fp) if fp == "aa11"));
    }

    #[test]
    fn trust_falls_back_to_tofu_keyed_on_the_dial_address() {
        // Keyed on the ADDRESS, not the nickname: entries recorded by earlier versions of
        // clowder were written with the dial address, and must keep matching.
        let t = plain_target("studio.tail:7777");
        match t.trust() {
            crate::tofu::Trust::Tofu { host, .. } => assert_eq!(host, "studio.tail:7777"),
            other => panic!("expected Tofu, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_token_is_never_sent_over_plaintext() {
        // Defense in depth: resolve_target refuses this combination (Task 6), but if one ever
        // reaches the wire, the token must not leak in cleartext.
        let (addr, hello_rx) = echo_remote_recording_hello_returning_token().await;
        let (mut client, server) = tokio::io::duplex(4096);
        let mut target = plain_target(&addr.to_string());
        target.token = Some("s3cr3t".into()); // tls stays false
        let fwd = tokio::spawn(async move { forward_stream(server, &target, Channel::Control).await });

        client.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        client.read_exact(&mut got).await.unwrap();
        let (_channel, token) = hello_rx.await.unwrap();
        assert_eq!(token, None, "the token must not be sent without TLS");

        drop(client);
        let _ = fwd.await;
    }
```

Replace the existing `echo_remote_recording_hello` helper with one that reports the token too, and update
its two existing callers (`forwards_hello_then_pipes_bytes`, `control_socket_forwards_with_control_hello`)
to destructure the pair:

```rust
    // A fake remote: reads the full channel hello (channel byte + length-prefixed optional
    // token), records both, then echoes the rest back.
    async fn echo_remote_recording_hello_returning_token(
    ) -> (std::net::SocketAddr, tokio::sync::oneshot::Receiver<(u8, Option<String>)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (channel, token) = clowder_proto::read_hello(&mut sock).await.unwrap();
            let byte = match channel {
                Channel::Control => 1u8,
                Channel::Render => 2u8,
            };
            let _ = tx.send((byte, token));
            let mut buf = [0u8; 64];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sock.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        (addr, rx)
    }
```

In the two existing tests, change `assert_eq!(hello_rx.await.unwrap(), 1);` to
`assert_eq!(hello_rx.await.unwrap().0, 1);`, and change their `forward_stream` / `forward` calls to the
new signatures — `forward_stream(server, &plain_target(&addr.to_string()), Channel::Control)` and
`forward(plain_target(&host), dirpath)`.

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client forward`
Expected: FAIL — `cannot find type RemoteTarget`.

- [ ] **Step 3: Implement**

In `crates/clowder-client/src/forward.rs`, add above `forward_stream`:

```rust
/// Everything needed to reach one remote daemon: where it is, how to authenticate, and how to
/// decide the certificate is really its.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTarget {
    /// The host nickname. Used for logging and (in M11b) the per-host forwarder socket directory.
    pub label: String,
    /// `host:port` — what we actually dial, and the `known_hosts` key on the un-pinned path.
    pub address: String,
    pub token: Option<String>,
    /// Whether to wrap the connection in TLS. Deliberately INDEPENDENT of `token`: the old
    /// `token.is_some() ⇒ TLS` coupling made it impossible to have an authenticated plaintext
    /// tunnel or a TLS host without a token.
    pub tls: bool,
    /// The pinned server-cert fingerprint, when this host has been paired.
    pub fingerprint: Option<String>,
}

impl RemoteTarget {
    /// The trust policy for one dial. A pin wins; otherwise fall back to TOFU against the
    /// shared `remote_known_hosts`, keyed on the dial address so pre-M11 entries keep matching.
    pub fn trust(&self) -> crate::tofu::Trust {
        match &self.fingerprint {
            Some(fp) => crate::tofu::Trust::Pinned(fp.clone()),
            None => crate::tofu::Trust::Tofu {
                host: self.address.clone(),
                known_hosts: crate::tofu::known_hosts_path(),
            },
        }
    }
}
```

Replace `forward_stream` (lines 31–57) with:

```rust
/// Forward one local connection to the remote daemon: dial, send the channel hello, then pipe
/// bytes both ways until either side closes.
pub async fn forward_stream<L>(mut local: L, target: &RemoteTarget, channel: Channel) -> Result<()>
where
    L: AsyncRead + AsyncWrite + Unpin + Send,
{
    let tcp = dial_with_backoff(&target.address).await?;
    let mut remote: Box<dyn RemoteStream> = if target.tls {
        let connector = tokio_rustls::TlsConnector::from(crate::tofu::connector(target.trust()));
        let name = tokio_rustls::rustls::pki_types::ServerName::try_from("clowder")
            .map_err(|e| anyhow::anyhow!("server name: {e}"))?;
        Box::new(connector.connect(name, tcp).await?)
    } else {
        Box::new(tcp)
    };
    // A bearer token in cleartext is worse than no token at all, and the daemon ignores it on a
    // plaintext listener anyway (`serve_remote` passes `expected_token: None`). `resolve_target`
    // refuses this combination up front; this is the belt to that pair of braces.
    let token = if target.tls { target.token.as_deref() } else { None };
    write_hello(&mut remote, channel, token).await?;
    tokio::io::copy_bidirectional(&mut local, &mut remote).await?;
    Ok(())
}
```

Replace `forward` (lines 66–103) with:

```rust
/// Bind the local render + control Unix sockets under `dir` and forward every connection to the
/// remote daemon (render → `Channel::Render`, control → `Channel::Control`). Prints the two paths
/// so callers can point CLOWDER_SOCK / CLOWDER_CONTROL_SOCK at them.
pub async fn forward(target: RemoteTarget, dir: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&dir)?;
    let render_path = dir.join("clowder.sock");
    let control_path = dir.join("clowder-control.sock");
    let _ = std::fs::remove_file(&render_path);
    let _ = std::fs::remove_file(&control_path);

    let render = UnixListener::bind(&render_path)?;
    let control = UnixListener::bind(&control_path)?;
    println!("clowder connect: forwarding to {} ({})", target.label, target.address);
    println!("  export CLOWDER_SOCK={}", render_path.display());
    println!("  export CLOWDER_CONTROL_SOCK={}", control_path.display());

    let accept = |listener: UnixListener, target: RemoteTarget, channel: Channel| async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("clowder connect: accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };
            let target = target.clone();
            tokio::spawn(async move {
                if let Err(e) = forward_stream(stream, &target, channel).await {
                    eprintln!("clowder connect: {channel:?} connection ended: {e}");
                }
            });
        }
    };

    tokio::select! {
        _ = accept(render, target.clone(), Channel::Render) => Ok(()),
        _ = accept(control, target, Channel::Control) => Ok(()),
    }
}
```

- [ ] **Step 4: Update `main.rs`'s `connect` arm so the crate compiles**

This is a placeholder wiring, replaced properly in Task 11. In `crates/clowder-client/src/main.rs`
lines 48–57, change the `forward` call to:

```rust
            clowder_client::forward::forward(
                clowder_client::forward::RemoteTarget {
                    label: host.clone(),
                    address: host,
                    tls: cfg.remote_tls || cfg.remote_token.is_some(),
                    token: cfg.remote_token,
                    fingerprint: None,
                },
                dir,
            )
            .await
```

- [ ] **Step 5: Run the whole crate's tests**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client`
Expected: PASS — including the Task 4 trust tests and the reworked forward tests.

- [ ] **Step 6: Commit**

```bash
git add crates/clowder-client/src/forward.rs crates/clowder-client/src/main.rs
git commit -m "feat(client): carry remote dial parameters in a RemoteTarget

Decouples TLS from the presence of a token, so a host can be plaintext
with no token, TLS with a token, or (refused) neither. The token is
suppressed on the plaintext path rather than sent in cleartext."
```

---

### Task 6: `resolve_target` — the pure selection rules

**Files:**
- Create: `crates/clowder-client/src/target.rs`
- Modify: `crates/clowder-client/src/lib.rs` (add `pub mod target;`)

**Interfaces:**
- Consumes: `HostEntry`/`HostSource` (Task 3), `RemoteTarget` (Task 5), `Config`.
- Produces: `pub fn resolve_target(selector: Option<&str>, hosts: &[HostEntry], cfg: &Config) -> Result<RemoteTarget, String>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/clowder-client/src/target.rs` with the test module:

```rust
//! Turning a user's selector ("studio", "10.0.0.5:7777", or nothing at all) into a dialable target.

#[cfg(test)]
mod tests {
    use super::*;
    use clowder_config::hosts::{HostEntry, HostRecord, HostSource};

    fn entry(name: &str, address: &str, tls: bool, token: Option<&str>, fp: Option<&str>) -> HostEntry {
        HostEntry {
            record: HostRecord {
                name: name.into(),
                address: address.into(),
                tls,
                token: token.map(String::from),
                fingerprint: fp.map(String::from),
            },
            source: HostSource::Registry,
        }
    }

    fn cfg(host: Option<&str>, tls: bool, token: Option<&str>) -> clowder_config::Config {
        clowder_config::Config {
            remote_host: host.map(String::from),
            remote_tls: tls,
            remote_token: token.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn no_selector_uses_the_only_configured_host() {
        let hosts = vec![entry("config", "h:1", false, None, None)];
        let t = resolve_target(None, &hosts, &cfg(Some("h:1"), false, None)).unwrap();
        assert_eq!(t.address, "h:1");
    }

    #[test]
    fn no_selector_with_no_config_host_is_an_error_naming_the_fix() {
        let err = resolve_target(None, &[], &cfg(None, false, None)).unwrap_err();
        assert!(err.contains("clowder remote"), "must point at the fix: {err}");
    }

    #[test]
    fn a_name_selects_that_entry_with_its_pin() {
        let hosts = vec![
            entry("other", "h:1", false, None, None),
            entry("studio", "s:7777", true, Some("tok"), Some("aa11")),
        ];
        let t = resolve_target(Some("studio"), &hosts, &cfg(None, false, None)).unwrap();
        assert_eq!(t.label, "studio");
        assert_eq!(t.address, "s:7777");
        assert_eq!(t.token.as_deref(), Some("tok"));
        assert!(t.tls);
        assert_eq!(t.fingerprint.as_deref(), Some("aa11"));
    }

    #[test]
    fn an_address_matching_an_entry_selects_that_entry() {
        let hosts = vec![entry("studio", "s:7777", true, Some("tok"), Some("aa11"))];
        let t = resolve_target(Some("s:7777"), &hosts, &cfg(None, false, None)).unwrap();
        assert_eq!(t.label, "studio", "an address match must still use the entry's identity");
        assert_eq!(t.fingerprint.as_deref(), Some("aa11"));
    }

    #[test]
    fn an_unknown_address_becomes_an_adhoc_tofu_target_from_config() {
        // Verbatim back-compat with today's documented `clowder connect host:port`.
        let t = resolve_target(Some("10.0.0.9:7777"), &[], &cfg(None, false, Some("ctok"))).unwrap();
        assert_eq!(t.label, "10.0.0.9:7777");
        assert_eq!(t.address, "10.0.0.9:7777");
        assert_eq!(t.token.as_deref(), Some("ctok"));
        assert!(t.tls, "a configured token implies TLS on the ad-hoc path too");
        assert_eq!(t.fingerprint, None, "ad-hoc dials stay TOFU");
    }

    #[test]
    fn an_unknown_name_is_an_error_naming_the_fix() {
        let hosts = vec![entry("studio", "s:7777", false, None, None)];
        let err = resolve_target(Some("studi"), &hosts, &cfg(None, false, None)).unwrap_err();
        assert!(err.contains("studi"), "must echo what was typed: {err}");
        assert!(err.contains("clowder remote list"), "must point at the fix: {err}");
    }

    #[test]
    fn a_token_without_tls_is_refused() {
        let hosts = vec![entry("studio", "s:7777", false, Some("tok"), None)];
        let err = resolve_target(Some("studio"), &hosts, &cfg(None, false, None)).unwrap_err();
        assert!(err.to_lowercase().contains("tls"), "must explain the fix: {err}");
    }
}
```

Add `pub mod target;` to `crates/clowder-client/src/lib.rs`.

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client target`
Expected: FAIL — `cannot find function resolve_target`.

- [ ] **Step 3: Implement**

Add above the test module in `crates/clowder-client/src/target.rs`:

```rust
use crate::forward::RemoteTarget;
use clowder_config::hosts::HostEntry;
use clowder_config::Config;

/// Resolve a selector against the merged host list.
///
/// Pure — no I/O — so every rule below is table-tested. Rules, in order:
///
/// 1. No selector → the entry matching `[remote] host`, if any.
/// 2. An exact **name** match.
/// 3. An exact **address** match (keeps the entry's identity, pin, and token).
/// 4. A selector that looks like an address but matches nothing → an **ad-hoc TOFU target** using
///    the config token. This is the verbatim back-compat path for `clowder connect host:port`.
/// 5. Anything else → an error naming `clowder remote list`.
pub fn resolve_target(
    selector: Option<&str>,
    hosts: &[HostEntry],
    cfg: &Config,
) -> Result<RemoteTarget, String> {
    let target = match selector {
        None => {
            let address = cfg.remote_host.as_deref().ok_or_else(|| {
                "no remote host given and none configured — pass one (`clowder connect <name|host:port>`), \
                 add one (`clowder remote add <name> <host:port>`), or set [remote] host"
                    .to_string()
            })?;
            hosts
                .iter()
                .find(|e| e.record.address == address)
                .map(from_entry)
                .unwrap_or_else(|| adhoc(address, cfg))
        }
        Some(sel) => {
            if let Some(e) = hosts.iter().find(|e| e.record.name == sel) {
                from_entry(e)
            } else if let Some(e) = hosts.iter().find(|e| e.record.address == sel) {
                from_entry(e)
            } else if clowder_config::hosts::validate_address(sel).is_ok() {
                adhoc(sel, cfg)
            } else {
                return Err(format!(
                    "unknown host {sel:?}; try `clowder remote list` (or pass a full host:port)"
                ));
            }
        }
    };

    if target.token.is_some() && !target.tls {
        return Err(format!(
            "host {:?} has a token but TLS is off — a bearer token must never cross the network in \
             cleartext. Run `clowder remote set {} --tls`, or clear the token with --no-token.",
            target.label, target.label
        ));
    }
    Ok(target)
}

fn from_entry(e: &HostEntry) -> RemoteTarget {
    RemoteTarget {
        label: e.record.name.clone(),
        address: e.record.address.clone(),
        token: e.record.token.clone(),
        tls: e.record.tls,
        fingerprint: e.record.fingerprint.clone(),
    }
}

/// A target for an address that is not in the registry: config credentials, TOFU trust.
fn adhoc(address: &str, cfg: &Config) -> RemoteTarget {
    RemoteTarget {
        label: address.to_string(),
        address: address.to_string(),
        // Same compatibility rule as `merged_hosts`: a configured token implies TLS, because
        // docs/remote-tls.md documents `tls` as a daemon-side key.
        tls: cfg.remote_tls || cfg.remote_token.is_some(),
        token: cfg.remote_token.clone(),
        fingerprint: None,
    }
}
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client target`
Expected: PASS — 7 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-client/src/target.rs crates/clowder-client/src/lib.rs
git commit -m "feat(client): resolve a host selector to a dialable target"
```

---

### Task 7: `probe` — observe a daemon without trusting it

**Files:**
- Create: `crates/clowder-client/src/probe.rs`
- Modify: `crates/clowder-client/src/lib.rs` (add `pub mod probe;`)

**Interfaces:**
- Consumes: `RemoteTarget` (Task 5), `Trust::Capture` (Task 4).
- Produces: `pub struct ProbeResult { pub reachable: bool, pub fingerprint: Option<String>, pub authenticated: bool, pub error: Option<String> }`;
  `pub async fn probe(target: &RemoteTarget, timeout: Duration) -> ProbeResult`.

**Why one line is the auth signal:** `handle_control_json` emits a `worktreeList` event unprompted the
moment it dispatches, and `handle_remote_conn` bails *before* dispatch on a bad token. Both behaviors are
already asserted by existing daemon tests (`control_hello_routes_to_control_handler`,
`tls_wrong_token_is_rejected`), so "a line arrived" ⇒ the token was accepted.

- [ ] **Step 1: Write the failing tests**

Create `crates/clowder-client/src/probe.rs` with the test module:

```rust
//! Reach a daemon, report what it presented, and persist nothing.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forward::RemoteTarget;

    /// Guards the process-global `XDG_STATE_HOME` these tests set, against other env-mutating
    /// tests in this crate's binary (see the same pattern in `tofu.rs`).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn target(addr: &str, tls: bool, token: Option<&str>) -> RemoteTarget {
        RemoteTarget {
            label: "probe-test".into(),
            address: addr.into(),
            token: token.map(String::from),
            tls,
            fingerprint: None,
        }
    }

    #[tokio::test]
    async fn a_dead_port_is_unreachable_and_fails_fast() {
        let t = target("127.0.0.1:1", false, None);
        let started = std::time::Instant::now();
        let r = probe(&t, Duration::from_secs(3)).await;
        assert!(!r.reachable);
        assert!(!r.authenticated);
        assert!(r.error.is_some());
        // dial_with_backoff would take ~15s; a probe must not.
        assert!(started.elapsed() < Duration::from_secs(5), "probe must fail fast");
    }

    #[tokio::test]
    async fn a_tls_daemon_with_the_right_token_authenticates_and_reports_its_fingerprint() {
        use clowder_daemon::{server::Daemon, FakeNotifier};
        use std::sync::Arc;
        use tokio::net::TcpListener;

        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", state.path());

        let creds = clowder_daemon::remote_tls::load_or_generate().unwrap();
        let token = creds.token.clone();
        let expected_fp = clowder_daemon::remote_tls::fingerprint(&creds);
        let tls = clowder_daemon::remote::build_remote_tls(&creds).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-m11a-probe.sock"),
        ));
        tokio::spawn(daemon.serve_remote(listener, Some(tls)));

        let good = probe(&target(&addr.to_string(), true, Some(&token)), Duration::from_secs(5)).await;
        assert!(good.reachable);
        assert!(good.authenticated, "a valid token must authenticate: {:?}", good.error);
        assert_eq!(good.fingerprint.as_deref(), Some(expected_fp.as_str()));

        // A wrong token is refused — but the fingerprint was still observed, which is what lets
        // the pairing UI show the user what it saw even when auth fails.
        let bad = probe(&target(&addr.to_string(), true, Some("wrong")), Duration::from_secs(5)).await;
        assert!(bad.reachable);
        assert!(!bad.authenticated);
        assert_eq!(bad.fingerprint.as_deref(), Some(expected_fp.as_str()));

        // Nothing was persisted: a probe must never pin.
        assert!(!crate::tofu::known_hosts_path().exists(), "probe must not write known_hosts");

        std::env::remove_var("XDG_STATE_HOME");
    }

    #[tokio::test]
    async fn a_plaintext_daemon_reports_no_fingerprint() {
        use clowder_daemon::{server::Daemon, FakeNotifier};
        use std::sync::Arc;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-m11a-probe2.sock"),
        ));
        tokio::spawn(daemon.serve_remote(listener, None));

        let r = probe(&target(&addr.to_string(), false, None), Duration::from_secs(5)).await;
        assert!(r.reachable);
        assert_eq!(r.fingerprint, None, "no TLS means no certificate to show");
        // A plaintext daemon passes `expected_token: None`, so it accepts anything. The CLI
        // reports this honestly as "no authentication" rather than as a success.
        assert!(r.authenticated);
    }
}
```

Add `pub mod probe;` to `crates/clowder-client/src/lib.rs`.

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client probe`
Expected: FAIL — `cannot find function probe`.

- [ ] **Step 3: Implement**

Add above the test module:

```rust
use crate::forward::RemoteTarget;
use crate::tofu::Trust;
use clowder_proto::{write_hello, Channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::net::TcpStream;

/// What one probe observed. Deliberately not a `Result`: "unreachable" and "reachable but the
/// token was refused" are both useful answers the pairing UI needs to show differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub reachable: bool,
    /// The server certificate's SHA-256, lowercase hex. `None` on a plaintext daemon, or when
    /// the TLS handshake failed.
    pub fingerprint: Option<String>,
    /// Whether the daemon accepted our token. NOTE: a plaintext daemon accepts anything, so
    /// `authenticated` alone does not mean "authenticated" — callers must pair it with `tls`.
    pub authenticated: bool,
    pub error: Option<String>,
}

impl ProbeResult {
    fn unreachable(e: impl std::fmt::Display) -> Self {
        Self { reachable: false, fingerprint: None, authenticated: false, error: Some(e.to_string()) }
    }
}

/// Reach `target`, report what it presented, and **persist nothing** — not `remote_known_hosts`,
/// not the registry. Pairing is a two-step flow precisely so that observing and trusting are
/// separate acts, with a human in between.
pub async fn probe(target: &RemoteTarget, timeout: Duration) -> ProbeResult {
    // A plain connect under one timeout — NOT `dial_with_backoff`, which takes ~15s to give up.
    // A probe runs while a user waits, and "is this address right?" must answer in seconds.
    let tcp = match tokio::time::timeout(timeout, TcpStream::connect(&target.address)).await {
        Err(_) => return ProbeResult::unreachable(format!("timed out after {timeout:?}")),
        Ok(Err(e)) => return ProbeResult::unreachable(e),
        Ok(Ok(s)) => s,
    };

    let sink: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let stream: Box<dyn ProbeStream> = if target.tls {
        let connector = tokio_rustls::TlsConnector::from(crate::tofu::connector(Trust::Capture(sink.clone())));
        let name = match tokio_rustls::rustls::pki_types::ServerName::try_from("clowder") {
            Ok(n) => n,
            Err(e) => return ProbeResult::unreachable(format!("server name: {e}")),
        };
        match tokio::time::timeout(timeout, connector.connect(name, tcp)).await {
            Err(_) => {
                return ProbeResult {
                    reachable: true,
                    fingerprint: fp_of(&sink),
                    authenticated: false,
                    error: Some(format!("TLS handshake timed out after {timeout:?}")),
                }
            }
            Ok(Err(e)) => {
                return ProbeResult {
                    reachable: true,
                    fingerprint: fp_of(&sink),
                    authenticated: false,
                    error: Some(format!("TLS handshake failed: {e}")),
                }
            }
            Ok(Ok(s)) => Box::new(s),
        }
    } else {
        Box::new(tcp)
    };

    let fingerprint = fp_of(&sink);
    match authenticate(stream, target, timeout).await {
        Ok(()) => ProbeResult { reachable: true, fingerprint, authenticated: true, error: None },
        Err(e) => ProbeResult {
            reachable: true,
            fingerprint,
            authenticated: false,
            error: Some(e),
        },
    }
}

/// Send a Control hello and wait for the daemon's first line.
///
/// The daemon's control handler emits a `worktreeList` event unprompted as soon as it dispatches,
/// and `handle_remote_conn` drops the connection BEFORE dispatch when the token is wrong. So a
/// line means the token was accepted, and EOF means it was not — no new protocol needed.
async fn authenticate(
    mut stream: Box<dyn ProbeStream>,
    target: &RemoteTarget,
    timeout: Duration,
) -> Result<(), String> {
    let token = if target.tls { target.token.as_deref() } else { None };
    write_hello(&mut stream, Channel::Control, token)
        .await
        .map_err(|e| format!("sending hello: {e}"))?;

    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    match tokio::time::timeout(timeout, reader.read_line(&mut line)).await {
        Err(_) => Err("the daemon accepted the connection but sent nothing (bad or missing token?)".into()),
        Ok(Err(e)) => Err(format!("reading the daemon's greeting: {e}")),
        Ok(Ok(0)) => Err("the daemon closed the connection (bad or missing token)".into()),
        Ok(Ok(_)) => Ok(()),
    }
}

fn fp_of(sink: &Arc<Mutex<Option<String>>>) -> Option<String> {
    sink.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Object-safe alias so the TLS and plaintext streams share one path (mirrors `forward`'s
/// `RemoteStream`).
trait ProbeStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ProbeStream for T {}
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client probe -- --test-threads=1`
Expected: PASS — 3 tests. Single-threaded because two of them set `XDG_STATE_HOME`; the `ENV_LOCK`
handles it, but serializing removes a whole class of confusing failure.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-client/src/probe.rs crates/clowder-client/src/lib.rs
git commit -m "feat(client): probe a remote daemon without pinning its certificate"
```

---

### Task 8: The flag parser

**Files:**
- Create: `crates/clowder-client/src/remote_cli.rs`
- Modify: `crates/clowder-client/src/lib.rs` (add `pub mod remote_cli;`)

**Interfaces:**
- Produces: `struct Flags` with `parse_flags(&[String]) -> Result<Flags, String>`, and methods
  `reject_unknown(&[&str]) -> Result<(), String>`, `str(&str) -> Option<&str>`, `bool(&str) -> bool`,
  `tristate(&str, &str) -> Result<Option<bool>, String>`, `positional(usize) -> Option<&str>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/clowder-client/src/remote_cli.rs`:

```rust
//! The `clowder remote …` subcommand tree: manage the host registry, probe a daemon, and record
//! a pairing decision. Everything here works with NO daemon running — that is the point.

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_positionals_and_both_flag_spellings() {
        let f = parse_flags(&args(&["add", "studio", "--address=h:1", "--tls", "--token", "t"])).unwrap();
        assert_eq!(f.positional(0), Some("add"));
        assert_eq!(f.positional(1), Some("studio"));
        assert_eq!(f.positional(2), None);
        assert_eq!(f.str("address"), Some("h:1"));
        assert_eq!(f.str("token"), Some("t"));
        assert!(f.bool("tls"));
        assert!(!f.bool("json"));
    }

    #[test]
    fn a_flag_with_no_value_is_a_bool_even_before_a_positional() {
        // `--tls studio` must not swallow "studio" as --tls's value, because --tls is declared
        // valueless. The parser learns that from the allowlist, so it needs the allowlist.
        let f = parse_flags(&args(&["--tls", "studio"])).unwrap();
        assert!(f.bool("tls"));
        assert_eq!(f.positional(0), Some("studio"));
    }

    #[test]
    fn unknown_flags_are_rejected_loudly() {
        let f = parse_flags(&args(&["--tsl"])).unwrap();
        let err = f.reject_unknown(&["tls", "json"]).unwrap_err();
        assert!(err.contains("tsl"), "must echo the typo: {err}");
    }

    #[test]
    fn tristate_reads_a_pair_of_opposing_flags() {
        let on = parse_flags(&args(&["--tls"])).unwrap();
        assert_eq!(on.tristate("tls", "no-tls").unwrap(), Some(true));
        let off = parse_flags(&args(&["--no-tls"])).unwrap();
        assert_eq!(off.tristate("tls", "no-tls").unwrap(), Some(false));
        let neither = parse_flags(&args(&[])).unwrap();
        assert_eq!(neither.tristate("tls", "no-tls").unwrap(), None);
        let both = parse_flags(&args(&["--tls", "--no-tls"])).unwrap();
        assert!(both.tristate("tls", "no-tls").is_err(), "contradictory flags must not pick one");
    }

    #[test]
    fn a_bare_double_dash_flag_with_an_empty_name_is_an_error() {
        assert!(parse_flags(&args(&["--"])).is_err());
        assert!(parse_flags(&args(&["--=x"])).is_err());
    }
}
```

Add `pub mod remote_cli;` to `crates/clowder-client/src/lib.rs`.

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client remote_cli`
Expected: FAIL — `cannot find function parse_flags`.

- [ ] **Step 3: Implement**

Note the design point exposed by the second test: `--tls studio` is ambiguous unless the parser knows
`--tls` takes no value. Rather than thread an allowlist through parsing, **a flag consumes the next
argument as its value only when that argument does not itself start with `--` and the flag was written
without `=`** — and valueless flags are then read via `bool()`, which treats "present with any value" and
"present with none" alike. `--tls studio` therefore parses `studio` as `--tls`'s value *and* still leaves
`bool("tls") == true`, so the second test asserts what actually matters. To keep positionals correct, we
declare the small set of value-taking flags up front.

```rust
use std::collections::HashMap;

/// The complete set of `--flags` that take a value. Everything else is a boolean, so
/// `--tls studio` leaves `studio` as a positional instead of swallowing it.
const VALUE_FLAGS: &[&str] = &[
    "address", "token", "rename", "fingerprint", "timeout", "socket-dir",
];

/// Parsed `--flag`/positional arguments. Deliberately tiny: this repo's CLI is hand-rolled
/// `std::env::args()` dispatch and adding clap for eight subcommands is not a trade worth making.
#[derive(Debug, Default)]
pub struct Flags {
    flags: HashMap<String, Option<String>>,
    positional: Vec<String>,
}

/// Accepts `--key value`, `--key=value`, and valueless `--key`.
pub fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut out = Flags::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(rest) = a.strip_prefix("--") {
            let (key, inline) = match rest.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (rest.to_string(), None),
            };
            if key.is_empty() {
                return Err(format!("malformed flag {a:?}"));
            }
            let value = match inline {
                Some(v) => Some(v),
                None if VALUE_FLAGS.contains(&key.as_str()) => {
                    i += 1;
                    Some(
                        args.get(i)
                            .cloned()
                            .ok_or_else(|| format!("--{key} needs a value"))?,
                    )
                }
                None => None,
            };
            out.flags.insert(key, value);
        } else {
            out.positional.push(a.clone());
        }
        i += 1;
    }
    Ok(out)
}

impl Flags {
    pub fn positional(&self, n: usize) -> Option<&str> {
        self.positional.get(n).map(|s| s.as_str())
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.flags.get(key).and_then(|v| v.as_deref())
    }

    /// True when the flag is present at all, regardless of whether it carried a value.
    pub fn bool(&self, key: &str) -> bool {
        self.flags.contains_key(key)
    }

    /// A typo in a flag name must fail loudly rather than being silently ignored — silently
    /// ignoring `--tsl` would leave a host unencrypted while reporting success.
    pub fn reject_unknown(&self, allowed: &[&str]) -> Result<(), String> {
        for k in self.flags.keys() {
            if !allowed.contains(&k.as_str()) {
                return Err(format!(
                    "unknown flag --{k} (expected one of: {})",
                    allowed.iter().map(|a| format!("--{a}")).collect::<Vec<_>>().join(", ")
                ));
            }
        }
        Ok(())
    }

    /// A pair of opposing switches (`--tls` / `--no-tls`) as `Some(true)` / `Some(false)` /
    /// `None` for "leave unchanged". Both at once is a contradiction, not a precedence puzzle.
    pub fn tristate(&self, on: &str, off: &str) -> Result<Option<bool>, String> {
        match (self.bool(on), self.bool(off)) {
            (true, true) => Err(format!("--{on} and --{off} contradict each other")),
            (true, false) => Ok(Some(true)),
            (false, true) => Ok(Some(false)),
            (false, false) => Ok(None),
        }
    }
}
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client remote_cli`
Expected: PASS — 5 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-client/src/remote_cli.rs crates/clowder-client/src/lib.rs
git commit -m "feat(client): add a minimal flag parser for the remote subcommands"
```

---

### Task 9: `remote list|show|add|set|rm` + the JSON contract

**Files:**
- Modify: `crates/clowder-client/src/remote_cli.rs`, `crates/clowder-client/src/main.rs`
- Create: `docs/protocol/fixtures/remote-host-list.json`

**Interfaces:**
- Consumes: `HostsStore`, `merged_hosts` (Tasks 2–3), `Flags` (Task 8).
- Produces: `pub async fn run(args: &[String]) -> anyhow::Result<()>`; `HostView`, `ListOut`, `ErrOut`
  (serde types, `camelCase`).

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `remote_cli.rs`:

```rust
    use clowder_config::hosts::{HostEntry, HostRecord, HostSource};

    fn entry(name: &str, address: &str, tls: bool, token: Option<&str>, fp: Option<&str>, src: HostSource) -> HostEntry {
        HostEntry {
            record: HostRecord {
                name: name.into(),
                address: address.into(),
                tls,
                token: token.map(String::from),
                fingerprint: fp.map(String::from),
            },
            source: src,
        }
    }

    #[test]
    fn host_view_never_leaks_the_token() {
        let v = HostView::from(&entry("studio", "s:7777", true, Some("s3cr3t"), None, HostSource::Registry));
        let json = serde_json::to_string(&v).unwrap();
        assert!(!json.contains("s3cr3t"), "the token must never reach stdout: {json}");
        assert!(json.contains(r#""hasToken":true"#));
    }

    #[test]
    fn host_view_reports_trust_and_source() {
        let paired = HostView::from(&entry("a", "h:1", true, None, Some("aa11"), HostSource::Registry));
        assert!(paired.trusted);
        assert_eq!(paired.source, "registry");
        let unpaired = HostView::from(&entry("b", "h:2", false, None, None, HostSource::Config));
        assert!(!unpaired.trusted);
        assert_eq!(unpaired.source, "config");
    }

    #[test]
    fn list_output_matches_the_golden_fixture() {
        // Rust encodes byte-exact; Swift decodes the same bytes in M11b. See docs/protocol/README.md.
        let out = ListOut {
            hosts: vec![
                HostView::from(&entry("studio", "studio.tailnet:7777", true, Some("t"), Some("a1b2"), HostSource::Registry)),
                HostView::from(&entry("config", "10.0.0.5:7777", false, None, None, HostSource::Config)),
            ],
        };
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/protocol/fixtures/remote-host-list.json");
        let want = std::fs::read_to_string(path).expect("fixture readable");
        assert_eq!(
            serde_json::to_string_pretty(&out).unwrap().trim(),
            want.trim(),
            "encoder and fixture disagree — update whichever is wrong"
        );
    }

    #[test]
    fn error_output_is_a_json_object() {
        let s = serde_json::to_string(&ErrOut { error: "no such host: studi".into() }).unwrap();
        assert_eq!(s, r#"{"error":"no such host: studi"}"#);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client remote_cli`
Expected: FAIL — `cannot find type HostView`.

- [ ] **Step 3: Write the fixture**

Create `docs/protocol/fixtures/remote-host-list.json`:

```json
{
  "hosts": [
    {
      "name": "studio",
      "address": "studio.tailnet:7777",
      "tls": true,
      "hasToken": true,
      "fingerprint": "a1b2",
      "trusted": true,
      "source": "registry"
    },
    {
      "name": "config",
      "address": "10.0.0.5:7777",
      "tls": false,
      "hasToken": false,
      "fingerprint": null,
      "trusted": false,
      "source": "config"
    }
  ]
}
```

- [ ] **Step 4: Implement the view types and the five registry subcommands**

Add to `remote_cli.rs`:

```rust
use anyhow::Result;
use clowder_config::hosts::{self, HostEntry, HostRecord, HostSource, HostsStore};
use clowder_config::Config;
use serde::Serialize;

/// One host as it appears on stdout. Note what is ABSENT: the token. The app only ever needs to
/// know whether one is set, so the secret never has to leave the Rust side.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostView {
    pub name: String,
    pub address: String,
    pub tls: bool,
    pub has_token: bool,
    pub fingerprint: Option<String>,
    pub trusted: bool,
    pub source: &'static str,
}

impl From<&HostEntry> for HostView {
    fn from(e: &HostEntry) -> Self {
        Self {
            name: e.record.name.clone(),
            address: e.record.address.clone(),
            tls: e.record.tls,
            has_token: e.record.token.is_some(),
            fingerprint: e.record.fingerprint.clone(),
            trusted: e.record.fingerprint.is_some(),
            source: match e.source {
                HostSource::Registry => "registry",
                HostSource::Config => "config",
            },
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ListOut {
    pub hosts: Vec<HostView>,
}

#[derive(Debug, Serialize)]
pub struct ErrOut {
    pub error: String,
}

/// Dispatch `clowder remote <sub> …`.
///
/// Every failure below is returned rather than printed, so `run` can render it as `{"error": …}`
/// under `--json` and as a plain stderr line otherwise — one place, one contract.
pub async fn run(args: &[String]) -> Result<()> {
    let flags = parse_flags(args).map_err(anyhow::Error::msg)?;
    let json = flags.bool("json");
    match dispatch(&flags).await {
        Ok(()) => Ok(()),
        Err(e) => {
            if json {
                println!("{}", serde_json::to_string(&ErrOut { error: e.to_string() })?);
            } else {
                eprintln!("clowder remote: {e}");
            }
            std::process::exit(1);
        }
    }
}

fn merged() -> Vec<HostEntry> {
    hosts::merged_hosts(HostsStore::default_store().load(), &Config::load())
}

/// Read a token from stdin (`--token-stdin`), so it never appears in argv — which is
/// world-readable through `ps`.
fn read_token_stdin() -> Result<String> {
    use std::io::Read;
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    let s = s.trim().to_string();
    if s.is_empty() {
        anyhow::bail!("--token-stdin was given but stdin was empty");
    }
    Ok(s)
}

/// The token for an add/set, from `--token-stdin` or `--token`.
fn token_from(flags: &Flags) -> Result<Option<String>> {
    if flags.bool("token-stdin") {
        return Ok(Some(read_token_stdin()?));
    }
    Ok(flags.str("token").map(String::from))
}

/// Find a registry (writable) record by name, refusing config-sourced entries with an
/// explanation rather than a generic "not found".
fn find_writable(all: &[HostEntry], name: &str) -> Result<()> {
    match all.iter().find(|e| e.record.name == name) {
        None => anyhow::bail!("unknown host {name:?}; try `clowder remote list`"),
        Some(e) if e.source == HostSource::Config => anyhow::bail!(
            "{name:?} is defined by [remote] host in config.toml and cannot be edited here — \
             edit config.toml, or add a separate entry with `clowder remote add`"
        ),
        Some(_) => Ok(()),
    }
}

async fn dispatch(flags: &Flags) -> Result<()> {
    let json = flags.bool("json");
    match flags.positional(0) {
        Some("list") => {
            flags.reject_unknown(&["json"]).map_err(anyhow::Error::msg)?;
            let all = merged();
            if json {
                let out = ListOut { hosts: all.iter().map(HostView::from).collect() };
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                for e in &all {
                    let v = HostView::from(e);
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        v.name,
                        v.address,
                        if v.tls { "tls" } else { "plain" },
                        if v.trusted { "paired" } else { "unpaired" },
                        v.source
                    );
                }
            }
            Ok(())
        }
        Some("show") => {
            flags.reject_unknown(&["json"]).map_err(anyhow::Error::msg)?;
            let name = flags.positional(1).ok_or_else(|| anyhow::anyhow!("usage: clowder remote show <name>"))?;
            let all = merged();
            let e = all
                .iter()
                .find(|e| e.record.name == name)
                .ok_or_else(|| anyhow::anyhow!("unknown host {name:?}; try `clowder remote list`"))?;
            let v = HostView::from(e);
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("name\t{}", v.name);
                println!("address\t{}", v.address);
                println!("tls\t{}", v.tls);
                println!("token\t{}", if v.has_token { "set" } else { "unset" });
                println!("fingerprint\t{}", v.fingerprint.as_deref().unwrap_or("-"));
                println!("source\t{}", v.source);
            }
            Ok(())
        }
        Some("add") => {
            flags.reject_unknown(&["json", "tls", "no-tls", "token", "token-stdin"]).map_err(anyhow::Error::msg)?;
            let name = flags.positional(1).ok_or_else(|| anyhow::anyhow!("usage: clowder remote add <name> <host:port>"))?.to_string();
            let address = flags.positional(2).ok_or_else(|| anyhow::anyhow!("usage: clowder remote add <name> <host:port>"))?.to_string();
            hosts::validate_name(&name).map_err(anyhow::Error::msg)?;
            hosts::validate_address(&address).map_err(anyhow::Error::msg)?;
            if merged().iter().any(|e| e.record.name == name) {
                anyhow::bail!("a host named {name:?} already exists");
            }
            let token = token_from(flags)?;
            // A token is only usable over TLS, so default TLS on when one is given rather than
            // silently creating a combination `resolve_target` will refuse.
            let tls = flags.tristate("tls", "no-tls").map_err(anyhow::Error::msg)?.unwrap_or(token.is_some());
            let record = HostRecord { name, address, tls, token, fingerprint: None };
            HostsStore::default_store().try_mutate(|all| all.push(record.clone()))?;
            report_one(&record, json)
        }
        Some("set") => {
            flags.reject_unknown(&["json", "tls", "no-tls", "token", "token-stdin", "no-token", "rename", "address"]).map_err(anyhow::Error::msg)?;
            let name = flags.positional(1).ok_or_else(|| anyhow::anyhow!("usage: clowder remote set <name> [--address …] [--rename …] …"))?.to_string();
            find_writable(&merged(), &name)?;
            if let Some(new) = flags.str("rename") {
                hosts::validate_name(new).map_err(anyhow::Error::msg)?;
                if new != name && merged().iter().any(|e| e.record.name == new) {
                    anyhow::bail!("a host named {new:?} already exists");
                }
            }
            if let Some(addr) = flags.str("address") {
                hosts::validate_address(addr).map_err(anyhow::Error::msg)?;
            }
            let tls = flags.tristate("tls", "no-tls").map_err(anyhow::Error::msg)?;
            let token = token_from(flags)?;
            if token.is_some() && flags.bool("no-token") {
                anyhow::bail!("--token/--token-stdin and --no-token contradict each other");
            }
            let clear_token = flags.bool("no-token");
            let rename = flags.str("rename").map(String::from);
            let address = flags.str("address").map(String::from);

            let updated = HostsStore::default_store().try_mutate(|all| {
                let Some(r) = all.iter_mut().find(|r| r.name == name) else {
                    return None;
                };
                if let Some(n) = rename { r.name = n; }
                if let Some(a) = address { r.address = a; }
                if let Some(t) = tls { r.tls = t; }
                if clear_token { r.token = None; }
                if let Some(t) = token { r.token = Some(t); }
                Some(r.clone())
            })?;
            let updated = updated.ok_or_else(|| anyhow::anyhow!("unknown host {name:?}"))?;
            report_one(&updated, json)
        }
        Some("rm") => {
            flags.reject_unknown(&["json"]).map_err(anyhow::Error::msg)?;
            let name = flags.positional(1).ok_or_else(|| anyhow::anyhow!("usage: clowder remote rm <name>"))?.to_string();
            find_writable(&merged(), &name)?;
            let removed = HostsStore::default_store().try_mutate(|all| {
                let idx = all.iter().position(|r| r.name == name)?;
                Some(all.remove(idx))
            })?;
            let removed = removed.ok_or_else(|| anyhow::anyhow!("unknown host {name:?}"))?;
            // Prune the legacy TOFU line only when no OTHER entry still dials that address —
            // otherwise removing one nickname would silently un-trust another.
            let still_used = HostsStore::default_store()
                .load()
                .iter()
                .any(|r| r.address == removed.address);
            if !still_used {
                prune_known_host(&removed.address);
            }
            if json {
                println!("{}", serde_json::to_string(&serde_json::json!({ "removed": removed.name }))?);
            } else {
                println!("removed {}", removed.name);
            }
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown subcommand {other:?}; usage: clowder remote <list|show|add|set|rm|probe|trust|untrust> …"),
        None => anyhow::bail!("usage: clowder remote <list|show|add|set|rm|probe|trust|untrust> …"),
    }
}

fn report_one(record: &HostRecord, json: bool) -> Result<()> {
    let view = HostView::from(&HostEntry { record: record.clone(), source: HostSource::Registry });
    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        println!("{}\t{}", view.name, view.address);
    }
    Ok(())
}

/// Drop `address`'s line from `remote_known_hosts`, best-effort. A failure here is not worth
/// failing the command over: the registry is the source of truth, and a stale line only ever
/// causes a loud refuse, never a silent trust.
fn prune_known_host(address: &str) {
    let path = crate::tofu::known_hosts_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let kept: String = text
        .lines()
        .filter(|l| l.split_whitespace().next() != Some(address))
        .map(|l| format!("{l}\n"))
        .collect();
    let _ = std::fs::write(&path, kept);
}
```

- [ ] **Step 5: Wire the subcommand into `main.rs`**

In `crates/clowder-client/src/main.rs`, add one arm to the `match` (before the legacy numeric arm):

```rust
        Some("remote") => clowder_client::remote_cli::run(&args[2..]).await,
```

and extend the final usage string:

```rust
        _ => Err(anyhow!("usage: clowder <spawn|project|attach|connect|remote|remote-host|remote-token> ...")),
```

- [ ] **Step 6: Run to verify the tests pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client remote_cli`
Expected: PASS — 9 tests.

- [ ] **Step 7: Verify by hand against a scratch registry**

```bash
source "$HOME/.cargo/env" && cargo build -p clowder-client
export CLOWDER_HOSTS_FILE=/tmp/m11a-hosts.json
./target/debug/clowder remote add studio studio.example:7777 --tls
./target/debug/clowder remote list
./target/debug/clowder remote list --json
echo "s3cr3t" | ./target/debug/clowder remote set studio --token-stdin
./target/debug/clowder remote show studio --json     # hasToken true, no token value anywhere
./target/debug/clowder remote rm studio
ls -l /tmp/m11a-hosts.json                            # -rw------- while it existed
./target/debug/clowder remote add x --tsl             # must fail loudly on the typo
unset CLOWDER_HOSTS_FILE
```

- [ ] **Step 8: Commit**

```bash
git add crates/clowder-client/src/remote_cli.rs crates/clowder-client/src/main.rs \
        docs/protocol/fixtures/remote-host-list.json
git commit -m "feat(client): add clowder remote list/show/add/set/rm"
```

---

### Task 10: `remote probe|trust|untrust`

**Files:**
- Modify: `crates/clowder-client/src/remote_cli.rs`
- Create: `docs/protocol/fixtures/remote-probe.json`

**Interfaces:**
- Consumes: `probe` (Task 7), `resolve_target` (Task 6).
- Produces: `ProbeView`, `ProbeOut` (serde, `camelCase`), and the three subcommand arms.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `remote_cli.rs`:

```rust
    #[test]
    fn fingerprint_match_classifies_against_the_pin() {
        assert_eq!(fingerprint_match(None, Some("aa11")), Some("new"));
        assert_eq!(fingerprint_match(Some("aa11"), Some("aa11")), Some("match"));
        assert_eq!(fingerprint_match(Some("aa11"), Some("bb22")), Some("changed"));
        // No certificate observed at all (plaintext, or a failed handshake) — not a classification.
        assert_eq!(fingerprint_match(Some("aa11"), None), None);
        assert_eq!(fingerprint_match(None, None), None);
    }

    #[test]
    fn probe_output_matches_the_golden_fixture() {
        let out = ProbeOut {
            probe: ProbeView {
                name: "studio".into(),
                address: "studio.tailnet:7777".into(),
                reachable: true,
                tls: true,
                fingerprint: Some("a1b2".into()),
                pinned_fingerprint: None,
                fingerprint_match: Some("new"),
                authenticated: true,
                error: None,
            },
        };
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/protocol/fixtures/remote-probe.json");
        let want = std::fs::read_to_string(path).expect("fixture readable");
        assert_eq!(serde_json::to_string_pretty(&out).unwrap().trim(), want.trim());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client remote_cli`
Expected: FAIL — `cannot find function fingerprint_match`.

- [ ] **Step 3: Write the fixture**

Create `docs/protocol/fixtures/remote-probe.json`:

```json
{
  "probe": {
    "name": "studio",
    "address": "studio.tailnet:7777",
    "reachable": true,
    "tls": true,
    "fingerprint": "a1b2",
    "pinnedFingerprint": null,
    "fingerprintMatch": "new",
    "authenticated": true,
    "error": null
  }
}
```

- [ ] **Step 4: Implement**

Add to `remote_cli.rs`:

```rust
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeView {
    pub name: String,
    pub address: String,
    pub reachable: bool,
    pub tls: bool,
    pub fingerprint: Option<String>,
    pub pinned_fingerprint: Option<String>,
    /// `new` | `match` | `changed`, or absent when no certificate was seen.
    pub fingerprint_match: Option<&'static str>,
    pub authenticated: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProbeOut {
    pub probe: ProbeView,
}

/// How the observed fingerprint relates to the stored pin. `None` when nothing was observed —
/// a plaintext daemon or a failed handshake is not a "changed" certificate.
fn fingerprint_match(pinned: Option<&str>, observed: Option<&str>) -> Option<&'static str> {
    match (pinned, observed) {
        (_, None) => None,
        (None, Some(_)) => Some("new"),
        (Some(p), Some(o)) if p == o => Some("match"),
        (Some(_), Some(_)) => Some("changed"),
    }
}
```

Add three arms to `dispatch`, before the catch-all:

```rust
        Some("probe") => {
            flags.reject_unknown(&["json", "address", "tls", "no-tls", "token", "token-stdin", "timeout"]).map_err(anyhow::Error::msg)?;
            let all = merged();
            // Either probe a saved host by name, or an as-yet-unsaved address (which is what the
            // Settings pane's "Test" button needs before the host exists).
            let target = match (flags.positional(1), flags.str("address")) {
                (Some(name), _) => crate::target::resolve_target(Some(name), &all, &Config::load())
                    .map_err(anyhow::Error::msg)?,
                (None, Some(addr)) => {
                    clowder_config::hosts::validate_address(addr).map_err(anyhow::Error::msg)?;
                    let token = token_from(flags)?;
                    crate::forward::RemoteTarget {
                        label: addr.to_string(),
                        address: addr.to_string(),
                        tls: flags.tristate("tls", "no-tls").map_err(anyhow::Error::msg)?.unwrap_or(token.is_some()),
                        token,
                        fingerprint: None,
                    }
                }
                (None, None) => anyhow::bail!("usage: clowder remote probe <name> | --address <host:port>"),
            };
            let secs: u64 = flags.str("timeout").unwrap_or("3").parse()
                .map_err(|_| anyhow::anyhow!("--timeout must be a whole number of seconds"))?;
            let result = crate::probe::probe(&target, std::time::Duration::from_secs(secs)).await;
            let pinned = target.fingerprint.clone();
            let view = ProbeView {
                name: target.label.clone(),
                address: target.address.clone(),
                reachable: result.reachable,
                tls: target.tls,
                fingerprint_match: fingerprint_match(pinned.as_deref(), result.fingerprint.as_deref()),
                fingerprint: result.fingerprint,
                pinned_fingerprint: pinned,
                authenticated: result.authenticated,
                error: result.error,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&ProbeOut { probe: view })?);
            } else {
                println!("reachable\t{}", view.reachable);
                println!("tls\t{}", view.tls);
                println!("fingerprint\t{}", view.fingerprint.as_deref().unwrap_or("-"));
                println!("match\t{}", view.fingerprint_match.unwrap_or("-"));
                // A plaintext daemon passes expected_token: None and so accepts ANY token.
                // Saying "authenticated" there would be a lie.
                println!(
                    "auth\t{}",
                    if !view.tls { "none (plaintext daemon)" }
                    else if view.authenticated { "token accepted" }
                    else { "token rejected" }
                );
                if let Some(e) = &view.error { println!("error\t{e}"); }
            }
            Ok(())
        }
        Some("trust") => {
            flags.reject_unknown(&["json", "fingerprint", "verify"]).map_err(anyhow::Error::msg)?;
            let name = flags.positional(1).ok_or_else(|| anyhow::anyhow!("usage: clowder remote trust <name> --fingerprint <hex>"))?.to_string();
            let fp = flags.str("fingerprint").ok_or_else(|| anyhow::anyhow!("--fingerprint is required — run `clowder remote probe {name}` first"))?.to_lowercase();
            let all = merged();
            find_writable(&all, &name)?;
            if flags.bool("verify") {
                let target = crate::target::resolve_target(Some(&name), &all, &Config::load()).map_err(anyhow::Error::msg)?;
                let r = crate::probe::probe(&target, std::time::Duration::from_secs(3)).await;
                match r.fingerprint.as_deref() {
                    Some(seen) if seen == fp => {}
                    Some(seen) => anyhow::bail!("--verify failed: the daemon presented {seen}, not {fp}"),
                    None => anyhow::bail!("--verify failed: no certificate was presented ({})", r.error.unwrap_or_default()),
                }
            }
            let record = HostsStore::default_store().try_mutate(|all| {
                let r = all.iter_mut().find(|r| r.name == name)?;
                r.fingerprint = Some(fp.clone());
                Some(r.clone())
            })?;
            let record = record.ok_or_else(|| anyhow::anyhow!("unknown host {name:?}"))?;
            // Also record it the SSH way, so a plain shell `clowder connect <address>` — which
            // has no registry entry to consult — agrees with the app.
            record_known_host(&record.address, &fp);
            report_one(&record, json)
        }
        Some("untrust") => {
            flags.reject_unknown(&["json"]).map_err(anyhow::Error::msg)?;
            let name = flags.positional(1).ok_or_else(|| anyhow::anyhow!("usage: clowder remote untrust <name>"))?.to_string();
            find_writable(&merged(), &name)?;
            let record = HostsStore::default_store().try_mutate(|all| {
                let r = all.iter_mut().find(|r| r.name == name)?;
                r.fingerprint = None;
                Some(r.clone())
            })?;
            let record = record.ok_or_else(|| anyhow::anyhow!("unknown host {name:?}"))?;
            prune_known_host(&record.address);
            report_one(&record, json)
        }
```

And the writer that `trust` needs:

```rust
/// Record `address → fp` in `remote_known_hosts`, replacing any existing line for that address.
/// Best-effort for the same reason as `prune_known_host`: the registry pin is authoritative.
fn record_known_host(address: &str, fp: &str) {
    let path = crate::tofu::known_hosts_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut out: String = existing
        .lines()
        .filter(|l| l.split_whitespace().next() != Some(address))
        .map(|l| format!("{l}\n"))
        .collect();
    out.push_str(&format!("{address} {fp}\n"));
    let _ = std::fs::write(&path, out);
}
```

- [ ] **Step 5: Run to verify the tests pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client`
Expected: PASS — the whole crate.

- [ ] **Step 6: Verify the pairing flow by hand**

In one terminal, run a TLS daemon:

```bash
source "$HOME/.cargo/env"
CLOWDER_LISTEN=127.0.0.1:7777 CLOWDER_REMOTE_TLS=1 cargo run -p clowder-daemon
```

In another:

```bash
source "$HOME/.cargo/env"
export CLOWDER_HOSTS_FILE=/tmp/m11a-hosts.json
./target/debug/clowder remote-token                       # note the fingerprint + token
./target/debug/clowder remote add local 127.0.0.1:7777 --tls
echo "<token from above>" | ./target/debug/clowder remote set local --token-stdin
./target/debug/clowder remote probe local --json          # fingerprintMatch "new", authenticated true
./target/debug/clowder remote trust local --fingerprint <fp> --verify
./target/debug/clowder remote probe local --json          # now "match"
./target/debug/clowder remote set local --no-token
./target/debug/clowder remote probe local --json          # authenticated false, fingerprint still shown
```

- [ ] **Step 7: Commit**

```bash
git add crates/clowder-client/src/remote_cli.rs docs/protocol/fixtures/remote-probe.json
git commit -m "feat(client): add clowder remote probe/trust/untrust for pairing"
```

---

### Task 11: `connect` through the registry, `--socket-dir`, exit code 4

**Files:**
- Modify: `crates/clowder-client/src/main.rs`
- Create: `crates/clowder-client/tests/connect_exit_codes.rs`

**Interfaces:**
- Consumes: `resolve_target` (Task 6), `merged_hosts` (Task 3), `forward` (Task 5).
- Produces: the final `connect` behavior M11b's `DaemonSupervisor` depends on — **exit code 4 means
  "the address is wrong or the daemon is down", and must not be retried blindly.**

- [ ] **Step 1: Write the failing test**

Create `crates/clowder-client/tests/connect_exit_codes.rs`:

```rust
//! `clowder connect` exit codes are a contract with the macOS app's DaemonSupervisor:
//! 4 = the first dial never landed (stop and show the user), anything else = relaunchable.

use std::process::Command;

#[test]
fn connect_to_a_dead_address_exits_4() {
    let dir = tempfile::tempdir().unwrap();
    // 127.0.0.1:1 refuses immediately, so this does not wait for the timeout.
    let out = Command::new(env!("CARGO_BIN_EXE_clowder"))
        .args(["connect", "127.0.0.1:1", "--socket-dir"])
        .arg(dir.path())
        .env("CLOWDER_HOSTS_FILE", dir.path().join("hosts.json"))
        .env("XDG_STATE_HOME", dir.path())
        .output()
        .expect("run clowder");
    assert_eq!(out.status.code(), Some(4), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("127.0.0.1:1"),
        "the error must name the address it could not reach"
    );
}

#[test]
fn connect_to_an_unknown_name_exits_1_not_4() {
    // A typo is a user error to be corrected, not an unreachable host to be retried.
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_clowder"))
        .args(["connect", "nosuchhost"])
        .env("CLOWDER_HOSTS_FILE", dir.path().join("hosts.json"))
        .env("XDG_STATE_HOME", dir.path())
        .output()
        .expect("run clowder");
    assert_eq!(out.status.code(), Some(1));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client --test connect_exit_codes`
Expected: FAIL — exit code 1, not 4 (and `--socket-dir` is an unrecognized positional today).

- [ ] **Step 3: Implement the `connect` arm**

Replace the whole `Some("connect")` arm in `crates/clowder-client/src/main.rs` with:

```rust
        Some("connect") => {
            let cfg = clowder_config::Config::load();
            let flags = clowder_client::remote_cli::parse_flags(&args[2..]).map_err(anyhow::Error::msg)?;
            flags.reject_unknown(&["socket-dir"]).map_err(anyhow::Error::msg)?;
            let hosts = clowder_config::hosts::merged_hosts(
                clowder_config::hosts::HostsStore::default_store().load(),
                &cfg,
            );
            let target = clowder_client::target::resolve_target(flags.positional(0), &hosts, &cfg)
                .map_err(anyhow::Error::msg)?;

            // Per-host socket dir. The APP passes --socket-dir explicitly (one authority for the
            // path, instead of Swift re-deriving this rule); the default keeps a bare
            // `clowder connect` working from a shell.
            let dir = match flags.str("socket-dir") {
                Some(d) => std::path::PathBuf::from(d),
                None => cfg
                    .control_sock
                    .parent()
                    .ok_or_else(|| anyhow!("cannot derive forwarder socket dir"))?
                    .join("remote")
                    .join(&target.label),
            };

            // Fail fast when the very first dial never lands. Without this the forwarder binds
            // its sockets and lives on, and the app's supervisor relaunches it forever behind a
            // permanent "Reconnecting…" with no way to tell a typo from a daemon that is down.
            // Exit 4 is the signal to stop and show the user (see DaemonSupervisor in M11b).
            const FIRST_DIAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
            if tokio::time::timeout(
                FIRST_DIAL_TIMEOUT,
                tokio::net::TcpStream::connect(&target.address),
            )
            .await
            .map_err(|_| ())
            .and_then(|r| r.map_err(|_| ()))
            .is_err()
            {
                eprintln!(
                    "clowder connect: cannot reach {} at {} — check the address, and that the daemon \
                     is running with [remote] listen set",
                    target.label, target.address
                );
                std::process::exit(4);
            }

            clowder_client::forward::forward(target, dir).await
        }
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client --test connect_exit_codes`
Expected: PASS — 2 tests.

- [ ] **Step 5: Verify back-compat by hand**

The one behavior that must not change: a bare `clowder connect <host:port>` against a live plaintext
daemon. With the daemon from Task 10 step 6 running (this time without TLS):

```bash
source "$HOME/.cargo/env"
CLOWDER_LISTEN=127.0.0.1:7777 cargo run -p clowder-daemon &
./target/debug/clowder connect 127.0.0.1:7777
# expect the two `export CLOWDER_SOCK=… / CLOWDER_CONTROL_SOCK=…` lines, as before
```

- [ ] **Step 6: Run the full workspace suite**

Run: `source "$HOME/.cargo/env" && cargo test --workspace --locked`
Expected: PASS. Three `clowder-daemon` tests are known to flake under parallel load and pass on re-run —
re-run before investigating.

- [ ] **Step 7: Commit**

```bash
git add crates/clowder-client/src/main.rs crates/clowder-client/tests/connect_exit_codes.rs
git commit -m "feat(client): resolve clowder connect through the host registry

Adds --socket-dir so the caller owns the forwarder's socket path, and
exit code 4 when the first dial never lands, so an unreachable host
cannot become an unbounded relaunch loop in the app."
```

---

### Task 12: Documentation

**Files:**
- Modify: `docs/protocol/README.md`, `docs/remote-tls.md`, `README.md`, `AGENTS.md`

- [ ] **Step 1: Document the new fixture direction**

`docs/protocol/README.md` currently describes two directions (`ControlEvent`: Rust encodes / Swift
decodes; `ControlRequest`: the reverse) plus `worktree-names.json` as a shared-validator fixture. Add a
third section describing **CLI stdout**: `remote-host-list.json` and `remote-probe.json` are encoded
byte-exact by `crates/clowder-client/src/remote_cli.rs` (tests
`list_output_matches_the_golden_fixture`, `probe_output_matches_the_golden_fixture`) and decoded by
Swift's `HostRegistry` in M11b. Add `host-names.json` alongside `worktree-names.json` in the
shared-validator section, naming `clowder_config::hosts::validate_name` and (forward reference)
Swift's `HostDraft.nameError`.

- [ ] **Step 2: Document the registry and pairing in `docs/remote-tls.md`**

Add a "Managing hosts" section covering `clowder remote add|list|show|set|rm`, and a "Pairing" section
covering `probe` → compare against `clowder remote-token` on the daemon host → `trust`. Three points the
doc must make explicitly, because they are the parts a reader will get wrong:

1. **The registry pin is authoritative**; `remote_known_hosts` is written on `trust` so a bare
   `clowder connect <address>` agrees, and is only *read* for hosts with no pin.
2. **A token requires TLS** — `clowder remote add … --token-stdin` turns TLS on by default for exactly
   this reason, and a token-without-TLS entry is refused at connect time.
3. **Pairing only closes the MITM window if the fingerprint is compared out-of-band.** Name the source:
   `clowder remote-token` run *on the daemon host*, or the daemon's startup log.

Also fix the stale caveat the doc already carries about the exposure warning being over-cautious under
TLS — `crates/clowder-daemon/src/main.rs` logs an `info!` rather than a warning when `tls = true`, so
the caveat no longer applies.

- [ ] **Step 3: Update `README.md` and `AGENTS.md`**

In `README.md`, add `clowder remote …` to the CLI list. In `AGENTS.md`'s Runtime model section, extend
the sentence about the optional remote TCP listener to mention that remote daemons are managed as a
nicknamed registry in `$XDG_STATE_HOME/clowder/hosts.json` (`CLOWDER_HOSTS_FILE` overrides), that the
file is `0600` because it holds bearer tokens, and that `[remote] host` still works and appears as a
read-only entry.

- [ ] **Step 4: Verify the docs match the code**

Run every command block you wrote in `docs/remote-tls.md` against the scratch registry
(`CLOWDER_HOSTS_FILE=/tmp/m11a-doccheck.json`) and confirm the output matches what the doc claims. Docs
that were never run are how the stale TLS caveat got there in the first place.

- [ ] **Step 5: Commit**

```bash
git add docs/protocol/README.md docs/remote-tls.md README.md AGENTS.md
git commit -m "docs(m11a): document the host registry, pairing, and the CLI stdout fixtures"
```

---

## Verification gate for M11a

- [ ] `source "$HOME/.cargo/env" && cargo test --workspace --locked` is green (re-run once if one of the
      three known daemon timing tests flakes).
- [ ] `scripts/check-commit-messages.sh` passes for every commit on the branch.
- [ ] `clowder remote add|list|show|set|rm` manages `hosts.json` with **no daemon running**, the file is
      `0600`, and no token value ever appears in `list`/`show` output or in argv.
- [ ] Two concurrent writers (`clowder remote add` in a shell loop against the same
      `CLOWDER_HOSTS_FILE`) lose no records.
- [ ] `clowder remote probe` reports the fingerprint of a TLS daemon, distinguishes an accepted token
      from a rejected one, says "none (plaintext daemon)" against a plaintext listener, and writes
      **nothing** — verify `remote_known_hosts` and `hosts.json` are untouched after a probe.
- [ ] `clowder remote trust --verify` refuses a wrong fingerprint and records a right one in both the
      registry and `remote_known_hosts`.
- [ ] A paired host whose daemon cert is rotated is refused loudly on the next `connect`.
- [ ] **Back-compat:** `clowder connect <host:port>` against a plaintext daemon behaves exactly as before;
      a `[remote] host` + `[remote] token` config with no registry file still connects over TLS.
- [ ] `clowder connect` to a dead address exits **4**; to an unknown name exits **1**.

## Self-review notes

Checked against the spec's §1–§5 (M11a's scope):

- §1 registry — Tasks 1–3. §2 trust — Task 4. §3 target/TLS decoupling — Tasks 5–6. §4 probe — Tasks 7,
  10. §5 CLI — Tasks 8–10. Fixtures — Tasks 1, 9, 10. `--socket-dir` + exit 4 — Task 11. Docs — Task 12.
- The spec's `HostsStore::try_mutate` flock is Task 2; `merged_hosts`'s five rules are all covered by
  Task 3's six tests.
- Names are consistent across tasks: `HostRecord`/`HostEntry`/`HostSource`/`HostsStore`/`merged_hosts`
  (config), `Trust`/`RemoteVerifier`/`connector` (tofu), `RemoteTarget`/`forward`/`forward_stream`
  (forward), `resolve_target` (target), `ProbeResult`/`probe` (probe),
  `Flags`/`parse_flags`/`HostView`/`ListOut`/`ErrOut`/`ProbeView`/`ProbeOut`/`run` (remote_cli).
- Deliberately deferred to M11b, per the spec: everything Swift, and the `DaemonSupervisor` handling of
  exit code 4 (M11a only *produces* the code; Task 11's test pins the contract).
