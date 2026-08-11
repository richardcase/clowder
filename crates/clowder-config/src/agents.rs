//! Agent profiles: named, enable-able wrappers around the daemon's built-in adapters, each
//! carrying an argument template appended to the adapter's own launch arguments.

use std::path::Path;

use serde::{Deserialize, Serialize};

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

const MAX_DISPLAY_NAME: usize = 64;

/// One agent profile, as stored in `agent-profiles.json` and as edited in the UI.
///
/// Evolved by ADDITIVE `#[serde(default)]` fields only — the mechanism proven by
/// `AgentRecord::tree` and `HostRecord::fingerprint`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfile {
    /// Stable, spawnable (`clowder spawn <project> <name> <id>`), and recorded on every agent
    /// spawned from it.
    pub id: String,
    /// The built-in adapter this wraps: `claude`, `codex` or `shell`.
    pub base: String,
    pub display_name: String,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    /// The argument template exactly as typed. Split and substituted at spawn.
    #[serde(default)]
    pub args: String,
}

fn enabled_default() -> bool {
    true
}

/// A profile as the daemon presents it, after merging the stored rows over the built-in defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveProfile {
    pub profile: AgentProfile,
    /// True for the adapters that ship with clowder. They can be edited and disabled, never removed.
    pub builtin: bool,
}

/// Profile ids follow the host-name rule. Deliberately delegated rather than re-stated: one narrow,
/// already-mirrored charset is worth more than a third bespoke validator.
pub fn validate_id(id: &str) -> Result<(), String> {
    crate::hosts::validate_name(id)
}

/// Full validation of a profile the user is trying to store. `builtins` is `(id, display_name)`.
pub fn validate_profile(p: &AgentProfile, builtins: &[(&str, &str)]) -> Result<(), String> {
    validate_id(&p.id)?;
    if p.display_name.trim().is_empty() {
        return Err("display name must not be empty".into());
    }
    if p.display_name.chars().count() > MAX_DISPLAY_NAME {
        return Err(format!("display name must be at most {MAX_DISPLAY_NAME} characters"));
    }
    if !builtins.iter().any(|(id, _)| *id == p.base) {
        return Err(format!(
            "unknown agent {:?} — must be one of {}",
            p.base,
            builtins.iter().map(|(id, _)| *id).collect::<Vec<_>>().join(", ")
        ));
    }
    validate_template(&p.args)
}

/// The effective profile list: the built-ins in descriptor order (each replaced wholesale by its
/// stored row, if any), then the user-created rows in file order.
///
/// The file holds only DELTAS, so a built-in added in a future release appears automatically
/// instead of being masked by a stale saved list, and "reset to default" is deleting a row.
/// Rows naming an unknown `base` are dropped — a hand-edited file must never wedge the daemon.
pub fn merged_profiles(rows: Vec<AgentProfile>, builtins: &[(&str, &str)]) -> Vec<EffectiveProfile> {
    let mut out: Vec<EffectiveProfile> = builtins
        .iter()
        .map(|(id, label)| {
            let profile = match rows.iter().find(|r| r.id == *id) {
                // A built-in's base is fixed by its id: a hand-edited row cannot repoint `shell`
                // at `claude` and change what a running agent resumes as.
                Some(row) => AgentProfile { base: (*id).to_string(), ..row.clone() },
                None => AgentProfile {
                    id: (*id).to_string(),
                    base: (*id).to_string(),
                    display_name: (*label).to_string(),
                    enabled: true,
                    args: String::new(),
                },
            };
            EffectiveProfile { profile, builtin: true }
        })
        .collect();

    out.extend(
        rows.into_iter()
            .filter(|r| !builtins.iter().any(|(id, _)| *id == r.id))
            .filter(|r| builtins.iter().any(|(id, _)| *id == r.base))
            .map(|profile| EffectiveProfile { profile, builtin: false }),
    );
    out
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

    const BUILTINS: &[(&str, &str)] = &[("claude", "Claude Code"), ("codex", "OpenAI Codex"), ("shell", "Shell")];

    fn profile(id: &str, base: &str) -> AgentProfile {
        AgentProfile {
            id: id.into(),
            base: base.into(),
            display_name: format!("{id} label"),
            enabled: true,
            args: String::new(),
        }
    }

    #[test]
    fn merge_with_no_rows_is_exactly_the_builtins() {
        let out = merged_profiles(vec![], BUILTINS);
        assert_eq!(
            out.iter().map(|e| e.profile.id.as_str()).collect::<Vec<_>>(),
            vec!["claude", "codex", "shell"]
        );
        assert!(out.iter().all(|e| e.builtin && e.profile.enabled));
        assert_eq!(out[0].profile.display_name, "Claude Code");
        assert_eq!(out[0].profile.base, "claude");
        assert_eq!(out[0].profile.args, "");
    }

    #[test]
    fn a_row_overrides_its_builtin_in_place() {
        let mut row = profile("codex", "codex");
        row.enabled = false;
        row.display_name = "Codex (off)".into();
        let out = merged_profiles(vec![row], BUILTINS);
        assert_eq!(out.len(), 3, "an override must not add a row");
        let codex = out.iter().find(|e| e.profile.id == "codex").unwrap();
        assert!(!codex.profile.enabled);
        assert_eq!(codex.profile.display_name, "Codex (off)");
        assert!(codex.builtin, "an overridden builtin is still a builtin");
        assert_eq!(out[1].profile.id, "codex", "builtins keep descriptor order");
    }

    #[test]
    fn user_rows_follow_the_builtins_in_file_order() {
        let out = merged_profiles(vec![profile("opus", "claude"), profile("plan", "claude")], BUILTINS);
        assert_eq!(
            out.iter().map(|e| e.profile.id.as_str()).collect::<Vec<_>>(),
            vec!["claude", "codex", "shell", "opus", "plan"]
        );
        assert!(!out[3].builtin && !out[4].builtin);
    }

    #[test]
    fn rows_with_an_unknown_base_are_dropped_not_fatal() {
        // A hand-edited file must never wedge the daemon; it just loses the bad row.
        let out = merged_profiles(vec![profile("weird", "emacs")], BUILTINS);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn an_override_row_cannot_change_a_builtins_base() {
        let mut row = profile("shell", "claude"); // hand-edited nonsense
        row.display_name = "Shell".into();
        let out = merged_profiles(vec![row], BUILTINS);
        let shell = out.iter().find(|e| e.profile.id == "shell").unwrap();
        assert_eq!(shell.profile.base, "shell", "a builtin's base is fixed by its id");
    }

    #[test]
    fn validate_profile_rejects_bad_ids_names_and_bases() {
        assert!(validate_profile(&profile("opus", "claude"), BUILTINS).is_ok());

        let mut bad_id = profile("has space", "claude");
        bad_id.display_name = "x".into();
        assert!(validate_profile(&bad_id, BUILTINS).is_err());

        let mut bad_base = profile("x", "emacs");
        bad_base.display_name = "x".into();
        let e = validate_profile(&bad_base, BUILTINS).unwrap_err();
        assert!(e.contains("emacs") && e.contains("claude"), "must name the bad base and the valid ones: {e}");

        let mut blank = profile("x", "claude");
        blank.display_name = "  ".into();
        assert!(validate_profile(&blank, BUILTINS).is_err(), "a blank display name is unusable in a picker");

        let mut bad_args = profile("x", "claude");
        bad_args.args = "--x {{nope}}".into();
        assert!(validate_profile(&bad_args, BUILTINS).is_err());
    }

    #[test]
    fn profile_json_is_camel_case_and_survives_a_round_trip() {
        let p = profile("opus", "claude");
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains(r#""displayName":"opus label""#), "{s}");
        assert_eq!(serde_json::from_str::<AgentProfile>(&s).unwrap(), p);
    }

    #[test]
    fn a_row_missing_optional_fields_still_loads() {
        // Additive-field evolution, as with AgentRecord::tree: a minimal hand-written row works.
        let p: AgentProfile =
            serde_json::from_str(r#"{"id":"opus","base":"claude","displayName":"Opus"}"#).unwrap();
        assert!(p.enabled, "a row without `enabled` defaults to enabled");
        assert_eq!(p.args, "");
    }
}
