//! Agent profiles: named, enable-able wrappers around the daemon's built-in adapters, each
//! carrying an argument template appended to the adapter's own launch arguments.

/// Split an argument template the way a shell would split a command line — and **only** that.
///
/// Supported: whitespace separation, `'…'` (fully literal), `"…"` (with `\"` and `\\` escapes),
/// and `\` escaping the next character outside quotes. Everything else is literal: `|`, `>`, `&&`,
/// `$VAR` and `~` are ordinary characters, because this never reaches a shell — the daemon
/// `execve`s the resulting argv directly.
///
/// Errors carry a user-facing message; they surface in the Settings editor, the CLI and the daemon.
pub fn split_args(s: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut has_cur = false; // distinguishes `""` (an empty arg) from a gap between args
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if has_cur {
                    out.push(std::mem::take(&mut cur));
                    has_cur = false;
                }
            }
            '\'' => {
                has_cur = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err("unterminated single quote (') in arguments".into()),
                    }
                }
            }
            '"' => {
                has_cur = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            // Only these two are escapes inside double quotes; a backslash before
                            // anything else stays literal, so a Windows-ish path is not mangled.
                            Some(e @ ('"' | '\\')) => cur.push(e),
                            Some(other) => {
                                cur.push('\\');
                                cur.push(other);
                            }
                            None => return Err("unterminated double quote (\") in arguments".into()),
                        },
                        Some(c) => cur.push(c),
                        None => return Err("unterminated double quote (\") in arguments".into()),
                    }
                }
            }
            '\\' => {
                has_cur = true;
                match chars.next() {
                    Some(e) => cur.push(e),
                    None => return Err("trailing backslash (\\) in arguments".into()),
                }
            }
            c => {
                has_cur = true;
                cur.push(c);
            }
        }
    }
    if has_cur {
        out.push(cur);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Case {
        input: String,
        #[serde(default)]
        argv: Option<Vec<String>>,
        #[serde(default)]
        error: Option<String>,
    }

    fn cases() -> Vec<Case> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/protocol/fixtures/agent-args.json");
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable")).expect("fixture parses")
    }

    #[test]
    fn split_args_agrees_with_the_shared_fixture() {
        let all = cases();
        assert!(!all.is_empty(), "fixture must not be empty");
        for c in all {
            match (&c.argv, c.error.as_deref()) {
                (Some(argv), _) => assert_eq!(
                    split_args(&c.input).as_ref(),
                    Ok(argv),
                    "split disagreed on {:?} — if you changed a rule, update the shared cases AND the Swift port",
                    c.input
                ),
                (None, Some("quote")) => assert!(
                    split_args(&c.input).is_err(),
                    "expected a quoting error for {:?}",
                    c.input
                ),
                // "token" cases split cleanly; only validate_template rejects them (Task 2).
                (None, Some("token")) => assert!(
                    split_args(&c.input).is_ok(),
                    "a token error case must still split: {:?}",
                    c.input
                ),
                (None, other) => panic!("case {:?} has neither argv nor a known error: {other:?}", c.input),
            }
        }
    }

    #[test]
    fn unterminated_quote_error_says_so() {
        let e = split_args("--a \"b").unwrap_err();
        assert!(e.contains("quote"), "unhelpful: {e}");
    }
}
