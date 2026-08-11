//! Agent profiles: named, enable-able wrappers around the daemon's built-in adapters, each
//! carrying an argument template appended to the adapter's own launch arguments.

use std::path::Path;

/// Every token an argument template may use. One exact spelling each — `{{ x }}` and `{{X}}` are
/// errors, not silently-accepted variants, so a typo can never reach an agent's argv as a literal.
pub const TOKENS: &[&str] = &[
    "project_name",
    "project_path",
    "workspace_name",
    "workspace_path",
    "branch",
];

/// The values a token can take, as known by the daemon **after** the worktree is provisioned —
/// which is the first moment `workspace_path` and `branch` exist.
pub struct TokenContext<'a> {
    pub project_path: &'a Path,
    pub workspace_path: &'a Path,
    pub workspace_name: &'a str,
    pub branch: &'a str,
}

impl TokenContext<'_> {
    fn value(&self, token: &str) -> Option<String> {
        let name_of = |p: &Path| {
            p.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned())
        };
        match token {
            "project_name" => Some(name_of(self.project_path)),
            "project_path" => Some(self.project_path.to_string_lossy().into_owned()),
            "workspace_name" => Some(self.workspace_name.to_string()),
            "workspace_path" => Some(self.workspace_path.to_string_lossy().into_owned()),
            "branch" => Some(self.branch.to_string()),
            _ => None,
        }
    }
}

/// Check that `args` splits cleanly and every `{{…}}` in it names a known token.
///
/// Called at save time (Settings, CLI) *and* by the daemon before spawning, because
/// `agent-profiles.json` is hand-editable and is not validated on load.
pub fn validate_template(args: &str) -> Result<(), String> {
    for arg in split_args(args)? {
        let mut rest = arg.as_str();
        while let Some(start) = rest.find("{{") {
            let after = &rest[start + 2..];
            let end = after
                .find("}}")
                .ok_or_else(|| format!("unclosed '{{{{' in {arg:?}"))?;
            let token = &after[..end];
            if !TOKENS.contains(&token) {
                return Err(format!(
                    "unknown token {{{{{token}}}}} — valid tokens are {}",
                    TOKENS.iter().map(|t| format!("{{{{{t}}}}}")).collect::<Vec<_>>().join(", ")
                ));
            }
            rest = &after[end + 2..];
        }
    }
    Ok(())
}

/// Replace every known token in each already-split argument.
///
/// Per-argument, after splitting — so a value containing whitespace stays exactly one argv element
/// and no token value can inject additional arguments. Unknown tokens are left untouched; they
/// cannot get here, because `validate_template` gates every path that reaches a spawn.
pub fn substitute(args: &[String], ctx: &TokenContext) -> Vec<String> {
    args.iter()
        .map(|arg| {
            let mut out = String::with_capacity(arg.len());
            let mut rest = arg.as_str();
            while let Some(start) = rest.find("{{") {
                let after = &rest[start + 2..];
                let Some(end) = after.find("}}") else { break };
                match ctx.value(&after[..end]) {
                    Some(v) => {
                        out.push_str(&rest[..start]);
                        out.push_str(&v);
                    }
                    None => out.push_str(&rest[..start + 2 + end + 2]),
                }
                rest = &after[end + 2..];
            }
            out.push_str(rest);
            out
        })
        .collect()
}

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

    #[test]
    fn validate_template_agrees_with_the_shared_fixture() {
        for c in cases() {
            match (&c.argv, c.error.as_deref()) {
                (Some(_), _) => assert!(
                    validate_template(&c.input).is_ok(),
                    "expected {:?} to validate: {:?}",
                    c.input,
                    validate_template(&c.input)
                ),
                (None, Some("token")) => assert!(
                    validate_template(&c.input).is_err(),
                    "expected a token error for {:?}",
                    c.input
                ),
                // Quoting errors surface from validate_template too — it splits first.
                (None, Some("quote")) => assert!(validate_template(&c.input).is_err()),
                (None, _) => {}
            }
        }
    }

    #[test]
    fn unknown_token_error_names_the_token_and_the_valid_set() {
        let e = validate_template("--x {{nope}}").unwrap_err();
        assert!(e.contains("nope"), "must name the offender: {e}");
        assert!(e.contains("workspace_name"), "must list the valid tokens: {e}");
    }

    fn ctx() -> TokenContext<'static> {
        TokenContext {
            project_path: std::path::Path::new("/Users/rc/code/my project"),
            workspace_path: std::path::Path::new("/data/clowder/worktrees/my project-abc/task-a"),
            workspace_name: "task-a",
            branch: "clowder/task-a",
        }
    }

    #[test]
    fn substitute_replaces_every_token() {
        let argv = split_args(
            "--p {{project_name}} --pp {{project_path}} --w {{workspace_name}} --wp {{workspace_path}} --b {{branch}}",
        )
        .unwrap();
        assert_eq!(
            substitute(&argv, &ctx()),
            vec![
                "--p", "my project",
                "--pp", "/Users/rc/code/my project",
                "--w", "task-a",
                "--wp", "/data/clowder/worktrees/my project-abc/task-a",
                "--b", "clowder/task-a",
            ]
        );
    }

    #[test]
    fn a_value_containing_spaces_stays_one_argument() {
        // The whole point of substituting AFTER splitting: no token value can inject an argument.
        let argv = split_args("--prompt \"work on {{project_name}}\"").unwrap();
        let out = substitute(&argv, &ctx());
        assert_eq!(out, vec!["--prompt", "work on my project"]);
        assert_eq!(out.len(), 2, "a space in the value must not add an argument");
    }

    #[test]
    fn repeated_and_adjacent_tokens_all_substitute() {
        let argv = split_args("{{workspace_name}}-{{workspace_name}}").unwrap();
        assert_eq!(substitute(&argv, &ctx()), vec!["task-a-task-a"]);
    }
}
