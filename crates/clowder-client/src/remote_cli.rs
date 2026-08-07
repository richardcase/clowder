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
}
