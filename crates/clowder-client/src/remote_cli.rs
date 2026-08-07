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
