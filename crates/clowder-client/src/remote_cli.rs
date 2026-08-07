//! The `clowder remote …` subcommand tree: manage the host registry, probe a daemon, and record
//! a pairing decision. Everything here works with NO daemon running — that is the point.

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
                Some(v) if VALUE_FLAGS.contains(&key.as_str()) => Some(v),
                Some(_) => return Err(format!("--{key} does not take a value")),
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
    /// Note: if a flag is repeated (e.g., `--token t1 --token t2`), the last value wins.
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

/// One `probe` result as it appears on stdout.
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

/// What `run()` prints on failure, and which stream it goes to. A pure value (no I/O, no
/// `process::exit`) so the mapping from an error to its rendered form is unit-testable directly,
/// without capturing real stdout/stderr or killing the test process.
struct Rendered {
    json: bool,
    body: String,
}

impl Rendered {
    fn new(msg: &str, json: bool) -> Self {
        let body = if json {
            serde_json::to_string(&ErrOut { error: msg.to_string() })
                // `ErrOut` is a plain `{error: String}` — serialization cannot fail — but a
                // hand-built fallback is cheaper than an `.unwrap()` that could theoretically
                // panic on the error-reporting path itself.
                .unwrap_or_else(|_| format!(r#"{{"error":{msg:?}}}"#))
        } else {
            format!("clowder remote: {msg}")
        };
        Self { json, body }
    }

    fn print_and_exit(&self) -> ! {
        if self.json {
            println!("{}", self.body);
        } else {
            eprintln!("{}", self.body);
        }
        std::process::exit(1);
    }
}

/// Parse + dispatch `clowder remote <sub> …`, converging EVERY failure — a malformed flag
/// included — onto one `Rendered` value.
///
/// `json` is recovered from the raw args (a literal `--json` token) BEFORE `parse_flags` runs,
/// because a `parse_flags` failure (e.g. `--tls=false`, which Task 8's fix rejects since `tls`
/// takes no inline value) must still honor `--json` — waiting on a `Flags` that may not exist
/// yet would defeat the purpose and silently fall back to a bare stderr line, breaking the
/// contract the app's later milestone depends on (it decodes stdout first, stderr never).
///
/// Split out from `run()` (rather than inlined together with `process::exit`) so a test can
/// drive this exact path without terminating the test binary.
async fn try_run(args: &[String]) -> std::result::Result<(), Rendered> {
    let json = args.iter().any(|a| a == "--json");
    let flags = parse_flags(args).map_err(|e| Rendered::new(&e, json))?;
    dispatch(&flags)
        .await
        .map_err(|e| Rendered::new(&e.to_string(), flags.bool("json")))
}

/// Dispatch `clowder remote <sub> …`. Thin by design: `try_run` is where the error-rendering
/// contract lives, so this has nothing left to get wrong.
pub async fn run(args: &[String]) -> Result<()> {
    if let Err(rendered) = try_run(args).await {
        rendered.print_and_exit();
    }
    Ok(())
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

/// Thin by design: each subcommand's logic lives in its own `cmd_*` function so this can stay a
/// one-line-per-arm match even as Task 10 appends `probe`/`trust`/`untrust`.
async fn dispatch(flags: &Flags) -> Result<()> {
    match flags.positional(0) {
        Some("list") => cmd_list(flags),
        Some("show") => cmd_show(flags),
        Some("add") => cmd_add(flags),
        Some("set") => cmd_set(flags),
        Some("rm") => cmd_rm(flags),
        Some("probe") => cmd_probe(flags).await,
        Some("trust") => cmd_trust(flags).await,
        Some("untrust") => cmd_untrust(flags),
        Some(other) => anyhow::bail!("unknown subcommand {other:?}; usage: clowder remote <list|show|add|set|rm|probe|trust|untrust> …"),
        None => anyhow::bail!("usage: clowder remote <list|show|add|set|rm|probe|trust|untrust> …"),
    }
}

fn cmd_list(flags: &Flags) -> Result<()> {
    flags.reject_unknown(&["json"]).map_err(anyhow::Error::msg)?;
    let all = merged();
    if flags.bool("json") {
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

fn cmd_show(flags: &Flags) -> Result<()> {
    flags.reject_unknown(&["json"]).map_err(anyhow::Error::msg)?;
    let name = flags.positional(1).ok_or_else(|| anyhow::anyhow!("usage: clowder remote show <name>"))?;
    let all = merged();
    let e = all
        .iter()
        .find(|e| e.record.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown host {name:?}; try `clowder remote list`"))?;
    let v = HostView::from(e);
    if flags.bool("json") {
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

fn cmd_add(flags: &Flags) -> Result<()> {
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
    report_one(&record, flags.bool("json"))
}

fn cmd_set(flags: &Flags) -> Result<()> {
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
    report_one(&updated, flags.bool("json"))
}

fn cmd_rm(flags: &Flags) -> Result<()> {
    flags.reject_unknown(&["json"]).map_err(anyhow::Error::msg)?;
    let name = flags.positional(1).ok_or_else(|| anyhow::anyhow!("usage: clowder remote rm <name>"))?.to_string();
    find_writable(&merged(), &name)?;
    let removed = HostsStore::default_store().try_mutate(|all| {
        let idx = all.iter().position(|r| r.name == name)?;
        Some(all.remove(idx))
    })?;
    let removed = removed.ok_or_else(|| anyhow::anyhow!("unknown host {name:?}"))?;
    // Prune the legacy TOFU line only when no OTHER entry still dials that address —
    // otherwise removing one nickname would silently un-trust another. This must check the
    // MERGED view (registry + the `[remote] host` virtual entry), not just the raw registry:
    // `merged_hosts` masks the virtual config entry whenever a registry record already claims
    // its address, so removing that registry record can make the (always-unpinned) config entry
    // visible again — and its trust rests entirely on this TOFU line. Checking the bare registry
    // alone would miss that and prune out from under it.
    let still_used = merged().iter().any(|e| e.record.address == removed.address);
    if !still_used {
        prune_known_host(&removed.address);
    }
    if flags.bool("json") {
        println!("{}", serde_json::to_string(&serde_json::json!({ "removed": removed.name }))?);
    } else {
        println!("removed {}", removed.name);
    }
    Ok(())
}

/// Reach a daemon and report what it presented. Either a saved host by name, or an as-yet-unsaved
/// address (`--address`, what the Settings pane's "Test" button needs before the host exists).
/// Persists NOTHING — that is the whole point of the pairing flow: observing and trusting are
/// deliberately separate acts, with a human in between.
async fn cmd_probe(flags: &Flags) -> Result<()> {
    flags
        .reject_unknown(&["json", "address", "tls", "no-tls", "token", "token-stdin", "timeout"])
        .map_err(anyhow::Error::msg)?;
    let all = merged();
    let target = match (flags.positional(1), flags.str("address")) {
        (Some(name), _) => crate::target::resolve_target(Some(name), &all, &Config::load())
            .map_err(anyhow::Error::msg)?,
        (None, Some(addr)) => {
            hosts::validate_address(addr).map_err(anyhow::Error::msg)?;
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
    // `probe` bounds the TCP connect, the TLS handshake, and the read-line each by this SAME
    // timeout value, so one call can take up to ~3x what's passed here — worst case ~9s at the
    // 3s default. Not a bug in `probe`; just worth knowing before raising this.
    let secs: u64 = flags
        .str("timeout")
        .unwrap_or("3")
        .parse()
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
    if flags.bool("json") {
        println!("{}", serde_json::to_string_pretty(&ProbeOut { probe: view })?);
    } else {
        println!("reachable\t{}", view.reachable);
        println!("tls\t{}", view.tls);
        println!("fingerprint\t{}", view.fingerprint.as_deref().unwrap_or("-"));
        println!("match\t{}", view.fingerprint_match.unwrap_or("-"));
        // A plaintext daemon passes expected_token: None and so accepts ANY token. Saying
        // "authenticated" there would be a lie.
        println!(
            "auth\t{}",
            if !view.tls {
                "none (plaintext daemon)"
            } else if view.authenticated {
                "token accepted"
            } else {
                "token rejected"
            }
        );
        if let Some(e) = &view.error {
            println!("error\t{e}");
        }
    }
    Ok(())
}

/// Record the human's pairing decision. Does NOT re-probe unless `--verify` is given — the
/// probe→trust TOCTOU window is accepted by design: the UI passes back verbatim the fingerprint
/// it displayed, so a cert swapped in between produces a pin that fails loudly on the very next
/// connect. `--verify` closes that window at the cost of one more round trip.
async fn cmd_trust(flags: &Flags) -> Result<()> {
    flags.reject_unknown(&["json", "fingerprint", "verify"]).map_err(anyhow::Error::msg)?;
    let name = flags
        .positional(1)
        .ok_or_else(|| anyhow::anyhow!("usage: clowder remote trust <name> --fingerprint <hex>"))?
        .to_string();
    let fp = flags
        .str("fingerprint")
        .ok_or_else(|| anyhow::anyhow!("--fingerprint is required — run `clowder remote probe {name}` first"))?
        .to_lowercase();
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
    // Also record it the SSH way, so a plain shell `clowder connect <address>` — which has no
    // registry entry to consult — agrees with the app.
    record_known_host(&record.address, &fp);
    report_one(&record, flags.bool("json"))
}

fn cmd_untrust(flags: &Flags) -> Result<()> {
    flags.reject_unknown(&["json"]).map_err(anyhow::Error::msg)?;
    let name = flags.positional(1).ok_or_else(|| anyhow::anyhow!("usage: clowder remote untrust <name>"))?.to_string();
    find_writable(&merged(), &name)?;
    let record = HostsStore::default_store().try_mutate(|all| {
        let r = all.iter_mut().find(|r| r.name == name)?;
        r.fingerprint = None;
        Some(r.clone())
    })?;
    let record = record.ok_or_else(|| anyhow::anyhow!("unknown host {name:?}"))?;
    // Same hazard `cmd_rm` guards against: `remote_known_hosts` is keyed on ADDRESS, not name, so
    // pruning it unconditionally would also un-trust any OTHER entry (registry or the masked
    // `[remote] host` virtual entry) that dials the same address and still relies on TOFU. Prune
    // only when no OTHER entry (this one still exists, just unpinned now) still claims the address.
    let still_used_elsewhere = merged().iter().any(|e| e.record.address == record.address && e.record.name != record.name);
    if !still_used_elsewhere {
        prune_known_host(&record.address);
    }
    report_one(&record, flags.bool("json"))
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

    #[test]
    fn a_boolean_flag_with_an_inline_value_is_an_error() {
        // `--tls=false` must be an error because `tls` is not in VALUE_FLAGS.
        // Users must use `--tls` (enable) or `--no-tls` (disable), never an `=` value.
        let err = parse_flags(&args(&["--tls=false"])).unwrap_err();
        assert!(err.contains("does not take a value"), "must reject --tls=false: {err}");
        // Verify that `--tls` alone still works and yields true
        let f = parse_flags(&args(&["--tls"])).unwrap();
        assert!(f.bool("tls"));
    }

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

    // Guards the process-global env vars (`XDG_STATE_HOME`, `CLOWDER_HOSTS_FILE`,
    // `CLOWDER_REMOTE_HOST`) that `rm_does_not_prune_a_known_hosts_line_still_used_by_the_masked_config_entry`
    // sets against races with any other env-mutating test in this crate's test binary — same
    // rationale, same crate-wide lock, as `tofu`'s and `probe`'s use of it.
    use crate::ENV_LOCK;

    #[tokio::test]
    async fn a_malformed_flag_still_honors_the_json_error_contract() {
        // Regression for a real bug: `parse_flags` failures (e.g. `--tls=false`, rejected
        // because `tls` isn't in VALUE_FLAGS and so takes no inline value) used to bypass the
        // `--json` contract entirely: `run()`'s old `parse_flags(args).map_err(...)?` returned
        // before `json` was even read, so the error propagated out of `run()` to `main()` and
        // Rust's default `Termination` impl printed a bare `Error: ...` to STDERR — meaning the
        // app, which decodes stdout first per the documented contract, would see nothing.
        //
        // `try_run` is exercised directly (not `run()`) because `run()`'s failure path ends in
        // `process::exit(1)`, which would kill the test process.
        let err = try_run(&args(&["list", "--tls=false", "--json"])).await.unwrap_err();
        assert!(err.json, "must honor --json even though flag parsing itself failed: body={}", err.body);
        let parsed: serde_json::Value = serde_json::from_str(&err.body).expect("body must be valid JSON");
        assert!(
            parsed["error"].as_str().unwrap().contains("does not take a value"),
            "unexpected error body: {}",
            err.body
        );
    }

    #[tokio::test]
    async fn rm_does_not_prune_a_known_hosts_line_still_used_by_the_masked_config_entry() {
        // Regression for a real bug: `cmd_rm`'s "is this address still used by another entry?"
        // check read the bare registry file. But `merged_hosts` MASKS the `[remote] host`
        // virtual entry whenever a registry record already claims its address — so removing
        // that one registry record un-masks the (always-unpinned, `fingerprint: None` by
        // construction) config entry, which depends entirely on this known_hosts line for
        // trust. Checking the bare registry missed that the config entry was about to become
        // the sole remaining user of the address, and pruned the line out from under it.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", dir.path());
        std::env::set_var("CLOWDER_HOSTS_FILE", dir.path().join("hosts.json"));
        std::env::set_var("CLOWDER_REMOTE_HOST", "10.0.0.5:7777");

        // Seed the registry with one entry at the SAME address [remote] host resolves to — this
        // is exactly what masks the virtual config entry while the registry entry exists.
        HostsStore::default_store()
            .try_mutate(|all| {
                all.push(HostRecord {
                    name: "studio".into(),
                    address: "10.0.0.5:7777".into(),
                    tls: false,
                    token: None,
                    fingerprint: None,
                })
            })
            .unwrap();

        // Seed a known_hosts line for that address, as an earlier TOFU connection would have.
        let kh = crate::tofu::known_hosts_path();
        std::fs::create_dir_all(kh.parent().unwrap()).unwrap();
        std::fs::write(&kh, "10.0.0.5:7777 aa11bb22\n").unwrap();

        // Sanity: before removal, the config entry is masked — only the registry entry shows.
        let before = merged();
        assert_eq!(before.len(), 1, "config entry should start masked: {before:?}");

        let flags = parse_flags(&args(&["rm", "studio"])).unwrap();
        cmd_rm(&flags).unwrap();

        // After removal, the virtual config entry becomes visible again at the same address...
        let after = merged();
        assert_eq!(after.len(), 1, "the config entry should now be visible: {after:?}");
        assert_eq!(after[0].source, HostSource::Config);

        // ...and it must still be able to connect without a fresh TOFU prompt: the line survives.
        let kept = std::fs::read_to_string(&kh).unwrap();
        assert!(
            kept.contains("10.0.0.5:7777"),
            "removing one nickname must not un-trust a host the config entry still dials: {kept:?}"
        );

        std::env::remove_var("XDG_STATE_HOME");
        std::env::remove_var("CLOWDER_HOSTS_FILE");
        std::env::remove_var("CLOWDER_REMOTE_HOST");
    }

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

    /// Sets up a real TLS `clowder-daemon` on an ephemeral port and returns everything a
    /// probe/trust test needs to dial it. Mirrors `probe.rs`'s own e2e test setup.
    async fn spawn_tls_daemon() -> (std::net::SocketAddr, String, String) {
        use clowder_daemon::{server::Daemon, FakeNotifier};
        use std::sync::Arc;
        use tokio::net::TcpListener;

        let creds = clowder_daemon::remote_tls::load_or_generate().unwrap();
        let token = creds.token.clone();
        let fp = clowder_daemon::remote_tls::fingerprint(&creds);
        let tls = clowder_daemon::remote::build_remote_tls(&creds).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-m11a-remotecli.sock"),
        ));
        tokio::spawn(daemon.serve_remote(listener, Some(tls)));
        (addr, token, fp)
    }

    #[tokio::test]
    async fn cmd_probe_persists_nothing() {
        // The whole point of the pairing flow: probing must never write the registry or
        // known_hosts, even against a real TLS daemon that authenticates successfully.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", dir.path());
        std::env::set_var("CLOWDER_HOSTS_FILE", dir.path().join("hosts.json"));

        let (addr, token, _fp) = spawn_tls_daemon().await;
        HostsStore::default_store()
            .try_mutate(|all| {
                all.push(HostRecord {
                    name: "studio".into(),
                    address: addr.to_string(),
                    tls: true,
                    token: Some(token),
                    fingerprint: None,
                })
            })
            .unwrap();

        let kh = crate::tofu::known_hosts_path();
        std::fs::create_dir_all(kh.parent().unwrap()).unwrap();
        let sentinel = "sentinel-host aa11\n";
        std::fs::write(&kh, sentinel).unwrap();
        let before = HostsStore::default_store().load();

        let flags = parse_flags(&args(&["probe", "studio", "--json"])).unwrap();
        cmd_probe(&flags).await.unwrap();

        assert_eq!(HostsStore::default_store().load(), before, "probe must not touch the registry");
        assert_eq!(
            std::fs::read_to_string(&kh).unwrap(),
            sentinel,
            "probe must not touch known_hosts"
        );

        std::env::remove_var("XDG_STATE_HOME");
        std::env::remove_var("CLOWDER_HOSTS_FILE");
    }

    #[tokio::test]
    async fn cmd_trust_records_the_pin_and_the_known_hosts_line() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", dir.path());
        std::env::set_var("CLOWDER_HOSTS_FILE", dir.path().join("hosts.json"));

        HostsStore::default_store()
            .try_mutate(|all| {
                all.push(HostRecord {
                    name: "studio".into(),
                    address: "10.0.0.5:7777".into(),
                    tls: true,
                    token: None,
                    fingerprint: None,
                })
            })
            .unwrap();

        let flags = parse_flags(&args(&["trust", "studio", "--fingerprint", "AA11BB22"])).unwrap();
        cmd_trust(&flags).await.unwrap();

        let loaded = HostsStore::default_store().load();
        assert_eq!(loaded[0].fingerprint.as_deref(), Some("aa11bb22"), "must lowercase the fingerprint");

        // `remote_known_hosts` must agree with the registry pin, so a bare `clowder connect
        // 10.0.0.5:7777` (which has no registry entry to consult) trusts the same fingerprint.
        let kh = std::fs::read_to_string(crate::tofu::known_hosts_path()).unwrap();
        assert!(kh.contains("10.0.0.5:7777 aa11bb22"), "known_hosts line missing: {kh:?}");

        std::env::remove_var("XDG_STATE_HOME");
        std::env::remove_var("CLOWDER_HOSTS_FILE");
    }

    #[tokio::test]
    async fn cmd_trust_verify_on_a_mismatch_refuses_and_writes_nothing() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", dir.path());
        std::env::set_var("CLOWDER_HOSTS_FILE", dir.path().join("hosts.json"));

        let (addr, token, _real_fp) = spawn_tls_daemon().await;
        HostsStore::default_store()
            .try_mutate(|all| {
                all.push(HostRecord {
                    name: "studio".into(),
                    address: addr.to_string(),
                    tls: true,
                    token: Some(token),
                    fingerprint: None,
                })
            })
            .unwrap();
        let before = HostsStore::default_store().load();
        let kh = crate::tofu::known_hosts_path();
        std::fs::create_dir_all(kh.parent().unwrap()).unwrap();
        let sentinel = "sentinel-host aa11\n";
        std::fs::write(&kh, sentinel).unwrap();

        // A fingerprint that does not match what the daemon actually presents.
        let flags = parse_flags(&args(&["trust", "studio", "--fingerprint", "deadbeef", "--verify"])).unwrap();
        let err = cmd_trust(&flags).await.unwrap_err();
        assert!(err.to_string().contains("--verify failed"), "unexpected error: {err}");

        assert_eq!(HostsStore::default_store().load(), before, "a failed --verify must write nothing to the registry");
        assert_eq!(
            std::fs::read_to_string(&kh).unwrap(),
            sentinel,
            "a failed --verify must write nothing to known_hosts"
        );

        std::env::remove_var("XDG_STATE_HOME");
        std::env::remove_var("CLOWDER_HOSTS_FILE");
    }

    #[test]
    fn cmd_untrust_clears_the_pin_and_prunes_the_known_hosts_line() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", dir.path());
        std::env::set_var("CLOWDER_HOSTS_FILE", dir.path().join("hosts.json"));

        HostsStore::default_store()
            .try_mutate(|all| {
                all.push(HostRecord {
                    name: "studio".into(),
                    address: "10.0.0.5:7777".into(),
                    tls: true,
                    token: None,
                    fingerprint: Some("aa11bb22".into()),
                })
            })
            .unwrap();
        let kh = crate::tofu::known_hosts_path();
        std::fs::create_dir_all(kh.parent().unwrap()).unwrap();
        std::fs::write(&kh, "10.0.0.5:7777 aa11bb22\nother:1 cc33\n").unwrap();

        let flags = parse_flags(&args(&["untrust", "studio"])).unwrap();
        cmd_untrust(&flags).unwrap();

        let loaded = HostsStore::default_store().load();
        assert_eq!(loaded[0].fingerprint, None, "the registry pin must be cleared");
        let kept = std::fs::read_to_string(&kh).unwrap();
        assert!(!kept.contains("10.0.0.5:7777"), "the known_hosts line for this address must be pruned: {kept:?}");
        assert!(kept.contains("other:1 cc33"), "unrelated lines must survive: {kept:?}");

        std::env::remove_var("XDG_STATE_HOME");
        std::env::remove_var("CLOWDER_HOSTS_FILE");
    }

    #[test]
    fn cmd_untrust_does_not_prune_a_known_hosts_line_still_used_by_another_entry_at_the_same_address() {
        // Same hazard `cmd_rm` guards against: `remote_known_hosts` is keyed on ADDRESS, not
        // name. Two registry nicknames can share one address; untrusting one must not silently
        // un-trust the other, which may still be relying on that TOFU line.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", dir.path());
        std::env::set_var("CLOWDER_HOSTS_FILE", dir.path().join("hosts.json"));

        HostsStore::default_store()
            .try_mutate(|all| {
                all.push(HostRecord {
                    name: "studio".into(),
                    address: "10.0.0.5:7777".into(),
                    tls: true,
                    token: None,
                    fingerprint: Some("aa11bb22".into()),
                });
                all.push(HostRecord {
                    name: "studio-alt".into(),
                    address: "10.0.0.5:7777".into(),
                    tls: true,
                    token: None,
                    fingerprint: None, // relies on the shared TOFU line
                });
            })
            .unwrap();
        let kh = crate::tofu::known_hosts_path();
        std::fs::create_dir_all(kh.parent().unwrap()).unwrap();
        std::fs::write(&kh, "10.0.0.5:7777 aa11bb22\n").unwrap();

        let flags = parse_flags(&args(&["untrust", "studio"])).unwrap();
        cmd_untrust(&flags).unwrap();

        let kept = std::fs::read_to_string(&kh).unwrap();
        assert!(
            kept.contains("10.0.0.5:7777"),
            "untrusting 'studio' must not un-trust 'studio-alt', which still dials the same address: {kept:?}"
        );

        std::env::remove_var("XDG_STATE_HOME");
        std::env::remove_var("CLOWDER_HOSTS_FILE");
    }
}
