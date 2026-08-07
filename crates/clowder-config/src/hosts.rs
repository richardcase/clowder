//! The remote host registry: a nicknamed list of remote daemons, owned by the CLI (not the daemon)
//! so it stays readable and writable when nothing is reachable.

use serde::{Deserialize, Serialize};

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
