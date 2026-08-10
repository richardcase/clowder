//! Turning a user's selector ("studio", "10.0.0.5:7777", or nothing at all) into a dialable target.

use crate::forward::RemoteTarget;
use clowder_config::hosts::HostEntry;
use clowder_config::Config;

/// Resolve a selector against the merged host list.
///
/// Pure — no I/O — so every rule below is table-tested. Rules, in order:
///
/// 1. No selector → the entry matching `[remote] host`, if any; **falls back to ad-hoc TOFU** if
///    the configured host is not in the registry.
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

    // Structurally unreachable for targets built by `adhoc()` — its `||` rule guarantees
    // `tls == true` whenever a token is present. Still applied after every branch for clarity.
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
        // Entry has tls=true, token=Some("entok") to discriminate from cfg (tls=false, token=None).
        // If the registry lookup were skipped, adhoc() would return tls=false, revealing the bug.
        let hosts = vec![entry("config", "h:1", true, Some("entok"), None)];
        let t = resolve_target(None, &hosts, &cfg(Some("h:1"), false, None)).unwrap();
        assert_eq!(t.label, "config", "must consult the registry, not fall straight to adhoc");
        assert_eq!(t.address, "h:1");
        assert_eq!(t.token.as_deref(), Some("entok"), "entry's token must be used");
        assert!(t.tls, "entry's tls must be used");
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
    fn a_name_match_takes_precedence_over_an_address_match() {
        // Entry A: name="studio", address="s:7777", tls=false, fp="aa11"
        // Entry B: name="s:7777", address="b:2", tls=true, fp="bb22"
        // Selecting "s:7777" must resolve to B (name match), not A (address match).
        let hosts = vec![
            entry("studio", "s:7777", false, None, Some("aa11")),
            entry("s:7777", "b:2", true, None, Some("bb22")),
        ];
        let t = resolve_target(Some("s:7777"), &hosts, &cfg(None, false, None)).unwrap();
        assert_eq!(t.label, "s:7777", "must resolve to the name match (entry B), not the address match (entry A)");
        assert_eq!(t.address, "b:2", "entry B's address confirms the name match was chosen");
        assert_eq!(t.fingerprint.as_deref(), Some("bb22"), "entry B's fingerprint confirms the name match was chosen");
        assert!(t.tls, "entry B's tls=true confirms the name match was chosen");
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
