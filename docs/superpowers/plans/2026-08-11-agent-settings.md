# Agent Settings (M12) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user enable/disable agents and give each one launch arguments (with
`{{project_name}}`-style tokens substituted at spawn) from a new Settings tab and a `clowder agent`
CLI, and drive the New Worktree agent picker from that configuration.

**Architecture:** A *profile* is a named, enable-able wrapper around one of the daemon's built-in
adapters, carrying an argument template. Pure types, validation, the arg splitter and token
substitution live in `clowder-config::agents`; the daemon owns a delta-based JSON store
(`agent-profiles.json`) exposed over the control socket; profile args are **appended** to the
adapter's own args, substituted **after** splitting, and the resolved tail is recorded on the agent
so a daemon restart resumes with byte-identical arguments.

**Tech Stack:** Rust (edition 2021, tokio, serde, anyhow), Swift 5.9 / SwiftUI (SwiftPM package under
`macos/`), JSON-lines control protocol.

Spec: `docs/superpowers/specs/2026-08-11-clowder-agent-settings-design.md`.

## Global Constraints

- **Prefix every cargo command** with `source "$HOME/.cargo/env" && `.
- Swift commands run inside `macos/`. `swift test` compiles `ClowderApp` but does not link it — a
  compile error anywhere in `ClowderApp` aborts the test run before any test executes.
- Conventional Commits (`type(scope): subject`); run `scripts/check-commit-messages.sh` before
  pushing. `feat` → minor, `fix`/`perf` → patch.
- Work on feature branches, never `main`. Four stacked PRs, each targeting the previous:
  `feat/m12a-agent-profile-core` → `feat/m12b-agent-profile-daemon` →
  `feat/m12c-agent-cli` → `feat/m12d-agent-settings-ui`.
- Built-in agent ids are exactly `claude`, `codex`, `shell` — the ids in
  `adapter_descriptors()` (`crates/clowder-daemon/src/agent.rs:154`).
- The five valid tokens are exactly: `project_name`, `project_path`, `workspace_name`,
  `workspace_path`, `branch`.
- The profiles file is `agent-profiles.json`. **Never** `agents.json` — that is the live-agent
  registry (`crates/clowder-daemon/src/registry.rs:40`).
- Profile args are always **appended** to an adapter's own args; adapter args are never replaced.

## File Structure

**Created**

| Path | Responsibility |
|---|---|
| `crates/clowder-config/src/agents.rs` | Pure core: `AgentProfile`, `validate_id`/`validate_profile`, `split_args`, `validate_template`, `substitute`, `merged_profiles`. No I/O. |
| `docs/protocol/fixtures/agent-args.json` | Shared arg-splitting/token cases, read by the Rust **and** Swift tests. |
| `crates/clowder-daemon/src/agent_profiles.rs` | `AgentProfileStore`: the `JsonStore`-backed delta file, effective-list merge, `resolve`. |
| `macos/Sources/ClowderCore/AgentArgs.swift` | Swift port of the splitter + token validation + preview. |
| `macos/Sources/ClowderCore/AgentsViewModel.swift` | Every decision behind the Settings Agents pane. |
| `macos/Sources/ClowderApp/AgentsSettingsView.swift` | List + editor split (render only). |
| `macos/Sources/ClowderApp/AgentEditorView.swift` | The per-profile form (render only). |
| `macos/Tests/ClowderCoreTests/AgentArgsTests.swift` | Fixture-driven splitter/token tests. |
| `macos/Tests/ClowderCoreTests/AgentsViewModelTests.swift` | View-model behaviour against a fake sender. |

**Modified**

| Path | Change |
|---|---|
| `crates/clowder-config/src/lib.rs` | `pub mod agents;` |
| `crates/clowder-proto/src/control.rs` | `AgentProfileInfo` + 4 requests + 1 event. |
| `crates/clowder-daemon/src/lib.rs` | `pub mod agent_profiles;` + re-exports. |
| `crates/clowder-daemon/src/agent.rs` | `SpawnSpec`. |
| `crates/clowder-daemon/src/server.rs` | Store field, `new_with_paths` param, profile methods, `list_adapters` from enabled profiles, `spawn_agent` argv, resume replay. |
| `crates/clowder-daemon/src/registry.rs` | `AgentRecord.profile_id` + `.extra_args`. |
| `crates/clowder-daemon/src/control_json.rs` | 4 request arms + the profiles broadcast arm. |
| `crates/clowder-client/src/lib.rs` | 4 `*_via_control` helpers. |
| `crates/clowder-client/src/main.rs` | `clowder agent` subcommand. |
| `macos/Sources/ClowderCore/Models.swift` | `AgentProfileInfo`, request cases, event case. |
| `macos/Sources/ClowderCore/AgentStore.swift` | `agentProfiles` + `reset`. |
| `macos/Sources/ClowderCore/SheetForms.swift` | `AgentProfileDraft`. |
| `macos/Sources/ClowderApp/SettingsView.swift` | Agents tab. |
| `macos/Sources/ClowderApp/App.swift` | Build + pass `AgentsViewModel`. |
| `AGENTS.md` | Repo layout + runtime model. |

---

# PR 1 — M12a: the pure core

Branch: `feat/m12a-agent-profile-core` off `main`.

### Task 1: Shared fixture + `split_args`

**Files:**
- Create: `docs/protocol/fixtures/agent-args.json`
- Create: `crates/clowder-config/src/agents.rs`
- Modify: `crates/clowder-config/src/lib.rs:4` (add `pub mod agents;` beside `pub mod hosts;`)
- Test: inline `#[cfg(test)] mod tests` in `crates/clowder-config/src/agents.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `clowder_config::agents::split_args(&str) -> Result<Vec<String>, String>`.

The fixture is read by Rust here and by Swift in Task 12. Each case is one of:
`{"input": …, "argv": [...]}` (splits cleanly **and** validates), `{"input": …, "error": "quote"}`
(splitting fails), `{"input": …, "error": "token"}` (splits fine, template validation fails).

- [ ] **Step 1: Write the fixture**

```json
[
  { "input": "", "argv": [] },
  { "input": "   ", "argv": [] },
  { "input": "--model opus", "argv": ["--model", "opus"] },
  { "input": "  --a   --b  ", "argv": ["--a", "--b"] },
  { "input": "--append-system-prompt \"work on {{workspace_name}}\"", "argv": ["--append-system-prompt", "work on {{workspace_name}}"] },
  { "input": "'single quoted with spaces'", "argv": ["single quoted with spaces"] },
  { "input": "\"\"", "argv": [""] },
  { "input": "a\"b\"c", "argv": ["abc"] },
  { "input": "--path /a\\ b", "argv": ["--path", "/a b"] },
  { "input": "--q \"say \\\"hi\\\"\"", "argv": ["--q", "say \"hi\""] },
  { "input": "'it''s'", "argv": ["its"] },
  { "input": "--pipe '|' --gt '>'", "argv": ["--pipe", "|", "--gt", ">"] },
  { "input": "{{project_name}}/{{workspace_name}}", "argv": ["{{project_name}}/{{workspace_name}}"] },
  { "input": "--all {{project_path}} {{workspace_path}} {{branch}}", "argv": ["--all", "{{project_path}}", "{{workspace_path}}", "{{branch}}"] },
  { "input": "\"unterminated", "error": "quote" },
  { "input": "'unterminated", "error": "quote" },
  { "input": "--ok \"a b", "error": "quote" },
  { "input": "--x {{nope}}", "error": "token" },
  { "input": "--x {{ workspace_name }}", "error": "token" },
  { "input": "--x {{PROJECT_NAME}}", "error": "token" },
  { "input": "--x {{project_name", "error": "token" },
  { "input": "--x \"{{bogus}}\"", "error": "token" }
]
```

Note the deliberate rules encoded here: `{{ workspace_name }}` (inner spaces) and `{{PROJECT_NAME}}`
(wrong case) are **errors**, not silently-trimmed matches — one exact spelling per token.

- [ ] **Step 2: Write the failing test**

Create `crates/clowder-config/src/agents.rs` containing only the test module for now:

```rust
//! Agent profiles: named, enable-able wrappers around the daemon's built-in adapters, each
//! carrying an argument template appended to the adapter's own launch arguments.

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
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config agents::`
Expected: FAIL — `cannot find function split_args in this scope`.

- [ ] **Step 4: Implement `split_args`**

Add above the test module in `crates/clowder-config/src/agents.rs`:

```rust
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
```

- [ ] **Step 5: Register the module**

In `crates/clowder-config/src/lib.rs`, beside `pub mod hosts;`:

```rust
pub mod agents;
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config agents::`
Expected: PASS (2 tests).

- [ ] **Step 7: Commit**

```bash
git add docs/protocol/fixtures/agent-args.json crates/clowder-config/src/agents.rs crates/clowder-config/src/lib.rs
git commit -m "feat(config): add the agent argument splitter and its shared fixture"
```

---

### Task 2: Tokens — validation and substitution

**Files:**
- Modify: `crates/clowder-config/src/agents.rs`
- Test: inline test module in the same file

**Interfaces:**
- Consumes: `split_args`.
- Produces:
  - `pub const TOKENS: &[&str]`
  - `pub struct TokenContext<'a> { project_path: &'a Path, workspace_path: &'a Path, workspace_name: &'a str, branch: &'a str }`
  - `pub fn validate_template(args: &str) -> Result<(), String>`
  - `pub fn substitute(args: &[String], ctx: &TokenContext) -> Vec<String>`

- [ ] **Step 1: Write the failing tests**

Add to the test module in `crates/clowder-config/src/agents.rs`:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config agents::`
Expected: FAIL — `cannot find function validate_template` / `cannot find type TokenContext`.

- [ ] **Step 3: Implement tokens**

Add to `crates/clowder-config/src/agents.rs` (top of file, after the doc comment):

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config agents::`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-config/src/agents.rs
git commit -m "feat(config): validate and substitute agent argument tokens"
```

---

### Task 3: `AgentProfile`, validation, and the effective-list merge

**Files:**
- Modify: `crates/clowder-config/src/agents.rs`
- Test: inline test module in the same file

**Interfaces:**
- Consumes: `split_args`, `validate_template`, `crate::hosts::validate_name`.
- Produces:
  - `pub struct AgentProfile { id: String, base: String, display_name: String, enabled: bool, args: String }` (all `pub`, `camelCase` serde)
  - `pub struct EffectiveProfile { profile: AgentProfile, builtin: bool }`
  - `pub fn validate_id(&str) -> Result<(), String>`
  - `pub fn validate_profile(&AgentProfile, builtins: &[(&str, &str)]) -> Result<(), String>`
  - `pub fn merged_profiles(rows: Vec<AgentProfile>, builtins: &[(&str, &str)]) -> Vec<EffectiveProfile>`

`builtins` is `&[(id, display_name)]` — the daemon passes its `adapter_descriptors()`, so this crate
never learns the adapter list and the merge stays testable with fake builtins.

- [ ] **Step 1: Write the failing tests**

Add to the test module:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config agents::`
Expected: FAIL — `cannot find type AgentProfile`.

- [ ] **Step 3: Implement the types, validation and merge**

Add to `crates/clowder-config/src/agents.rs`:

```rust
use serde::{Deserialize, Serialize};

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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config`
Expected: PASS — all `agents::` tests plus the pre-existing config/hosts tests.

- [ ] **Step 5: Run the whole workspace to be sure nothing else moved**

Run: `source "$HOME/.cargo/env" && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit and open the PR**

```bash
git add crates/clowder-config/src/agents.rs
git commit -m "feat(config): add agent profile types, validation and the effective-list merge"
gh pr create --base main --title "feat(config): agent profile core (M12a)" \
  --body "First of four stacked PRs for #80. Pure types, validation, arg splitting and token substitution in clowder-config::agents, plus the shared docs/protocol/fixtures/agent-args.json. No behaviour change yet.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_01QZBTSP2UQiEkGK5j8zcUKN"
```

---

# PR 2 — M12b: the daemon

Branch: `feat/m12b-agent-profile-daemon` off `feat/m12a-agent-profile-core`.

### Task 4: `AgentProfileStore`

**Files:**
- Create: `crates/clowder-daemon/src/agent_profiles.rs`
- Modify: `crates/clowder-daemon/src/lib.rs:13` (add `pub mod agent_profiles;` after `pub mod projects;`)

**Interfaces:**
- Consumes: `clowder_config::agents::{AgentProfile, EffectiveProfile, merged_profiles, validate_profile, split_args}`, `crate::store::JsonStore`, `crate::agent::adapter_descriptors`.
- Produces:
  - `AgentProfileStore::new(PathBuf)`, `::default_path()`
  - `.effective() -> Vec<EffectiveProfile>`, `.add(AgentProfile) -> Result<()>`,
    `.update(AgentProfile) -> Result<()>`, `.remove(&str) -> Result<()>`,
    `.resolve(&str) -> Result<ResolvedProfile>`
  - `pub struct ResolvedProfile { pub profile_id: String, pub base: String, pub arg_template: Vec<String> }`
  - `pub fn builtin_pairs() -> Vec<(&'static str, &'static str)>`

- [ ] **Step 1: Write the failing tests**

Create `crates/clowder-daemon/src/agent_profiles.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, AgentProfileStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = AgentProfileStore::new(dir.path().join("agent-profiles.json"));
        (dir, s)
    }

    fn opus() -> AgentProfile {
        AgentProfile {
            id: "opus".into(),
            base: "claude".into(),
            display_name: "Claude (Opus)".into(),
            enabled: true,
            args: "--model opus".into(),
        }
    }

    #[test]
    fn a_fresh_store_lists_exactly_the_builtins() {
        let (_d, s) = store();
        let ids: Vec<String> = s.effective().into_iter().map(|e| e.profile.id).collect();
        assert_eq!(ids, vec!["claude", "codex", "shell"]);
    }

    #[test]
    fn add_then_effective_includes_the_new_profile() {
        let (_d, s) = store();
        s.add(opus()).unwrap();
        let e = s.effective();
        assert_eq!(e.len(), 4);
        assert_eq!(e[3].profile.id, "opus");
        assert!(!e[3].builtin);
    }

    #[test]
    fn add_rejects_a_duplicate_or_builtin_id() {
        let (_d, s) = store();
        s.add(opus()).unwrap();
        assert!(s.add(opus()).unwrap_err().to_string().contains("already"));
        let mut clash = opus();
        clash.id = "claude".into();
        assert!(s.add(clash).unwrap_err().to_string().contains("built-in"));
    }

    #[test]
    fn add_rejects_an_invalid_profile() {
        let (_d, s) = store();
        let mut bad = opus();
        bad.args = "--x {{nope}}".into();
        assert!(s.add(bad).unwrap_err().to_string().contains("nope"));
    }

    #[test]
    fn update_writes_an_override_row_for_a_builtin() {
        let (_d, s) = store();
        let mut codex = s.effective().into_iter().find(|e| e.profile.id == "codex").unwrap().profile;
        codex.enabled = false;
        s.update(codex).unwrap();
        let e = s.effective();
        assert_eq!(e.len(), 3, "overriding a builtin must not add a row");
        assert!(!e.iter().find(|e| e.profile.id == "codex").unwrap().profile.enabled);
    }

    #[test]
    fn update_rejects_an_unknown_id() {
        let (_d, s) = store();
        let mut ghost = opus();
        ghost.id = "ghost".into();
        assert!(s.update(ghost).unwrap_err().to_string().contains("ghost"));
    }

    #[test]
    fn remove_drops_a_user_profile_and_refuses_a_builtin() {
        let (_d, s) = store();
        s.add(opus()).unwrap();
        s.remove("opus").unwrap();
        assert_eq!(s.effective().len(), 3);

        let e = s.remove("claude").unwrap_err().to_string();
        assert!(e.contains("built-in") && e.contains("disable"), "must point at disable: {e}");
    }

    #[test]
    fn remove_of_an_overridden_builtin_is_still_refused() {
        let (_d, s) = store();
        let mut claude = s.effective().into_iter().next().unwrap().profile;
        claude.args = "--model opus".into();
        s.update(claude).unwrap();
        assert!(s.remove("claude").is_err(), "an override row does not make a builtin removable");
        assert_eq!(s.effective().len(), 3);
    }

    #[test]
    fn resolve_splits_the_template_and_reports_the_base() {
        let (_d, s) = store();
        s.add(opus()).unwrap();
        let r = s.resolve("opus").unwrap();
        assert_eq!(r.base, "claude");
        assert_eq!(r.profile_id, "opus");
        assert_eq!(r.arg_template, vec!["--model", "opus"]);
    }

    #[test]
    fn resolve_rejects_unknown_and_disabled_ids_differently() {
        let (_d, s) = store();
        assert!(s.resolve("ghost").unwrap_err().to_string().contains("unknown"));

        let mut codex = s.effective().into_iter().find(|e| e.profile.id == "codex").unwrap().profile;
        codex.enabled = false;
        s.update(codex).unwrap();
        let e = s.resolve("codex").unwrap_err().to_string();
        assert!(e.contains("disabled"), "{e}");
    }

    #[test]
    fn resolve_rejects_a_hand_edited_bad_template() {
        // agent-profiles.json is hand-editable and is not validated on load, so the spawn path
        // must validate too — a bad token must fail loudly rather than reach an agent's argv.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("agent-profiles.json");
        std::fs::write(
            &p,
            r#"[{"id":"bad","base":"claude","displayName":"Bad","enabled":true,"args":"--x {{nope}}"}]"#,
        )
        .unwrap();
        let s = AgentProfileStore::new(p);
        assert!(s.resolve("bad").unwrap_err().to_string().contains("nope"));
    }

    #[test]
    fn a_corrupt_file_falls_back_to_the_builtins() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("agent-profiles.json");
        std::fs::write(&p, b"not json").unwrap();
        assert_eq!(AgentProfileStore::new(p).effective().len(), 3);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon agent_profiles::`
Expected: FAIL — `cannot find type AgentProfileStore` (module not declared yet).

- [ ] **Step 3: Implement the store**

Add above the test module in `crates/clowder-daemon/src/agent_profiles.rs`:

```rust
use crate::store::JsonStore;
use anyhow::{bail, Result};
use clowder_config::agents::{merged_profiles, validate_profile, AgentProfile, EffectiveProfile};
use std::path::PathBuf;

/// A profile resolved for spawning: which adapter to build, and the argument template to append
/// once the worktree exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub profile_id: String,
    pub base: String,
    /// Split, NOT yet substituted — the token values only exist after the worktree is provisioned.
    pub arg_template: Vec<String>,
}

/// The built-in `(id, display_name)` pairs, from the one adapter registry.
pub fn builtin_pairs() -> Vec<(&'static str, &'static str)> {
    crate::agent::adapter_descriptors().iter().map(|d| (d.id, d.display_name)).collect()
}

/// The user's agent profiles. Policy-free like `ProjectStore`: it validates and persists, and knows
/// nothing about spawning.
///
/// The file holds only DELTAS — an override row per built-in the user has touched, plus one row per
/// user-created profile. See `merged_profiles`.
pub struct AgentProfileStore {
    store: JsonStore<AgentProfile>,
}

impl AgentProfileStore {
    pub fn new(path: PathBuf) -> Self {
        Self { store: JsonStore::new(path) }
    }

    /// `$CLOWDER_AGENT_PROFILES_FILE` › `$XDG_STATE_HOME/clowder/agent-profiles.json` ›
    /// `$HOME/.local/state/clowder/agent-profiles.json` — the same derivation as the agent
    /// registry and the project store. NOT `agents.json`, which is the live-agent registry.
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("CLOWDER_AGENT_PROFILES_FILE") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
        let base = std::env::var("XDG_STATE_HOME").ok().filter(|s| !s.is_empty())
            .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.local/state")))
            .unwrap_or_else(|| "/tmp".to_string());
        PathBuf::from(base).join("clowder").join("agent-profiles.json")
    }

    pub fn effective(&self) -> Vec<EffectiveProfile> {
        merged_profiles(self.store.load(), &builtin_pairs())
    }

    fn is_builtin(id: &str) -> bool {
        builtin_pairs().iter().any(|(b, _)| *b == id)
    }

    /// Create a new user profile. Messages are user-facing — they surface in the Settings alert
    /// and on the CLI.
    pub fn add(&self, profile: AgentProfile) -> Result<()> {
        validate_profile(&profile, &builtin_pairs()).map_err(anyhow::Error::msg)?;
        if Self::is_builtin(&profile.id) {
            bail!("{} is a built-in agent — pick another id", profile.id);
        }
        if self.effective().iter().any(|e| e.profile.id == profile.id) {
            bail!("an agent named {} already exists", profile.id);
        }
        self.store.try_mutate(move |all| all.push(profile))?;
        Ok(())
    }

    /// Replace an existing profile — a user row, or a built-in (which writes an override row).
    pub fn update(&self, profile: AgentProfile) -> Result<()> {
        validate_profile(&profile, &builtin_pairs()).map_err(anyhow::Error::msg)?;
        if !self.effective().iter().any(|e| e.profile.id == profile.id) {
            bail!("no such agent: {}", profile.id);
        }
        self.store.try_mutate(move |all| {
            all.retain(|r| r.id != profile.id);
            all.push(profile);
        })?;
        Ok(())
    }

    /// Delete a user profile. Built-ins are never removable — their row is only an override.
    pub fn remove(&self, id: &str) -> Result<()> {
        if Self::is_builtin(id) {
            bail!("{id} is a built-in agent and cannot be removed — disable it instead");
        }
        if !self.effective().iter().any(|e| e.profile.id == id) {
            bail!("no such agent: {id}");
        }
        let id = id.to_string();
        self.store.try_mutate(move |all| all.retain(|r| r.id != id))?;
        Ok(())
    }

    /// Resolve a spawnable id. Validates the template again here: the file is hand-editable and is
    /// not validated on load, so this is the last gate before an agent's argv.
    pub fn resolve(&self, id: &str) -> Result<ResolvedProfile> {
        let e = self
            .effective()
            .into_iter()
            .find(|e| e.profile.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown agent: {id}"))?;
        if !e.profile.enabled {
            bail!("agent {id} is disabled — enable it in Settings, or run `clowder agent enable {id}`");
        }
        clowder_config::agents::validate_template(&e.profile.args).map_err(anyhow::Error::msg)?;
        Ok(ResolvedProfile {
            profile_id: e.profile.id,
            base: e.profile.base,
            arg_template: clowder_config::agents::split_args(&e.profile.args)
                .map_err(anyhow::Error::msg)?,
        })
    }
}
```

- [ ] **Step 4: Declare the module**

In `crates/clowder-daemon/src/lib.rs`, after `pub mod projects;`:

```rust
pub mod agent_profiles;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon agent_profiles::`
Expected: PASS (12 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/clowder-daemon/src/agent_profiles.rs crates/clowder-daemon/src/lib.rs
git commit -m "feat(daemon): add the agent profile store"
```

---

### Task 5: Control-protocol types

**Files:**
- Modify: `crates/clowder-proto/src/control.rs` (`ProjectInfo` block ~line 29, `ControlRequest` ~line 45, `ControlEvent` ~line 65)
- Test: inline tests in the same file

**Interfaces:**
- Produces: `clowder_proto::AgentProfileInfo`, `ControlRequest::{ListAgentProfiles, AddAgentProfile, UpdateAgentProfile, RemoveAgentProfile}`, `ControlEvent::AgentProfileList`.

`AgentProfileInfo` is the **wire** form and lives here; `clowder_config::agents::AgentProfile` is the
**storage** form. Same split as `ProjectInfo` vs `ProjectRecord` — and required, since
`clowder-proto` and `clowder-config` do not depend on each other.

- [ ] **Step 1: Write the failing tests**

Add to the test module at the bottom of `crates/clowder-proto/src/control.rs`:

```rust
    #[test]
    fn agent_profile_list_event_round_trips_with_camelcase() {
        let ev = ControlEvent::AgentProfileList {
            profiles: vec![AgentProfileInfo {
                id: "opus".into(),
                base: "claude".into(),
                display_name: "Claude (Opus)".into(),
                enabled: true,
                args: "--model opus".into(),
                builtin: false,
            }],
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains(r#""type":"agentProfileList""#), "{s}");
        assert!(s.contains(r#""displayName":"Claude (Opus)""#), "{s}");
        assert!(s.contains(r#""builtin":false"#), "{s}");
        assert_eq!(ev, serde_json::from_str::<ControlEvent>(&s).unwrap());
    }

    #[test]
    fn agent_profile_requests_round_trip() {
        let list = ControlRequest::ListAgentProfiles;
        assert_eq!(serde_json::to_string(&list).unwrap(), r#"{"type":"listAgentProfiles"}"#);

        let p = AgentProfileInfo {
            id: "opus".into(),
            base: "claude".into(),
            display_name: "Claude (Opus)".into(),
            enabled: false,
            args: "--model opus".into(),
            builtin: false,
        };
        for r in [
            ControlRequest::AddAgentProfile { profile: p.clone() },
            ControlRequest::UpdateAgentProfile { profile: p },
            ControlRequest::RemoveAgentProfile { id: "opus".into() },
        ] {
            let s = serde_json::to_string(&r).unwrap();
            assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap(), "{s}");
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-proto control::`
Expected: FAIL — `cannot find type AgentProfileInfo`.

- [ ] **Step 3: Add the wire types**

In `crates/clowder-proto/src/control.rs`, after the `ProjectInfo` struct:

```rust
/// One agent profile on the wire. The storage form is `clowder_config::agents::AgentProfile`;
/// this adds `builtin` so a client can disable Remove without knowing the adapter registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfileInfo {
    pub id: String,
    pub base: String,
    pub display_name: String,
    pub enabled: bool,
    pub args: String,
    pub builtin: bool,
}
```

Add to `ControlRequest`:

```rust
    ListAgentProfiles,
    AddAgentProfile { profile: AgentProfileInfo },
    UpdateAgentProfile { profile: AgentProfileInfo },
    RemoveAgentProfile { id: String },
```

Add to `ControlEvent`:

```rust
    AgentProfileList { profiles: Vec<AgentProfileInfo> },
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-proto`
Expected: PASS. (`clowder-daemon` will now fail to compile — its `match` on `ControlRequest` is not
exhaustive. Task 6 fixes that.)

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-proto/src/control.rs
git commit -m "feat(proto): add agent profile control requests and events"
```

---

### Task 6: Daemon wiring — store field, profile methods, adapter list

**Files:**
- Modify: `crates/clowder-daemon/src/server.rs` (struct ~line 53, `new_with_paths` ~line 123, `new_with` ~line 107, `new_from_config` ~line 172, `list_adapters` ~line 1069)
- Modify: all `Daemon::new_with_paths(` call sites (33 across `crates/clowder-daemon/src/server.rs`, `control_json.rs`, `crates/clowder-daemon/tests/agent_e2e.rs`, `crates/clowder-client/src/lib.rs`)
- Test: inline tests in `crates/clowder-daemon/src/server.rs`

**Interfaces:**
- Consumes: `AgentProfileStore`, `ResolvedProfile`, `clowder_proto::AgentProfileInfo`.
- Produces on `Daemon`:
  - `pub fn list_agent_profiles(&self) -> Vec<AgentProfileInfo>`
  - `pub fn add_agent_profile(&self, AgentProfileInfo) -> Result<()>`
  - `pub fn update_agent_profile(&self, AgentProfileInfo) -> Result<()>`
  - `pub fn remove_agent_profile(&self, &str) -> Result<()>`
  - `pub fn subscribe_agent_profiles(&self) -> broadcast::Receiver<()>`
  - `pub fn resolve_profile(&self, &str) -> Result<ResolvedProfile>`
  - `new_with_paths(notifier, hook_sock, registry_path, projects_path, profiles_path, worktree_base)` — **6th parameter added before `worktree_base`**

The broadcast payload is `()` — a tick. Each control connection recomputes and writes both
`AgentProfileList` and `AdapterList`, so there is exactly one code path that turns store state into
wire events.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `crates/clowder-daemon/src/server.rs`:

```rust
    #[test]
    fn list_adapters_returns_only_enabled_profiles() {
        let state = tempfile::tempdir().unwrap();
        let daemon = std::sync::Arc::new(Daemon::new_with_paths(
            std::sync::Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-profiles.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));

        let ids: Vec<String> = daemon.list_adapters().into_iter().map(|a| a.id).collect();
        assert_eq!(ids, vec!["claude", "codex", "shell"], "defaults are all enabled");

        let mut codex = daemon
            .list_agent_profiles()
            .into_iter()
            .find(|p| p.id == "codex")
            .unwrap();
        codex.enabled = false;
        daemon.update_agent_profile(codex).unwrap();

        let ids: Vec<String> = daemon.list_adapters().into_iter().map(|a| a.id).collect();
        assert_eq!(ids, vec!["claude", "shell"], "a disabled profile leaves the picker");

        // ...but the Settings list still shows it, marked disabled.
        let codex = daemon.list_agent_profiles().into_iter().find(|p| p.id == "codex").unwrap();
        assert!(!codex.enabled && codex.builtin);
    }

    #[test]
    fn adapter_list_shows_a_user_profiles_display_name() {
        let state = tempfile::tempdir().unwrap();
        let daemon = std::sync::Arc::new(Daemon::new_with_paths(
            std::sync::Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-profiles2.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));
        daemon
            .add_agent_profile(clowder_proto::AgentProfileInfo {
                id: "opus".into(),
                base: "claude".into(),
                display_name: "Claude (Opus)".into(),
                enabled: true,
                args: "--model opus".into(),
                builtin: false,
            })
            .unwrap();
        let a = daemon.list_adapters();
        assert!(a.iter().any(|a| a.id == "opus" && a.display_name == "Claude (Opus)"), "{a:?}");
    }

    #[test]
    fn profile_mutations_tick_the_broadcast() {
        let state = tempfile::tempdir().unwrap();
        let daemon = std::sync::Arc::new(Daemon::new_with_paths(
            std::sync::Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-profiles3.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));
        let mut rx = daemon.subscribe_agent_profiles();
        daemon
            .add_agent_profile(clowder_proto::AgentProfileInfo {
                id: "opus".into(),
                base: "claude".into(),
                display_name: "Opus".into(),
                enabled: true,
                args: String::new(),
                builtin: false,
            })
            .unwrap();
        assert!(rx.try_recv().is_ok(), "add must notify connected clients");

        daemon.remove_agent_profile("opus").unwrap();
        assert!(rx.try_recv().is_ok(), "remove must notify too");
    }

    #[test]
    fn a_failed_mutation_does_not_tick() {
        let state = tempfile::tempdir().unwrap();
        let daemon = std::sync::Arc::new(Daemon::new_with_paths(
            std::sync::Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-profiles4.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));
        let mut rx = daemon.subscribe_agent_profiles();
        assert!(daemon.remove_agent_profile("claude").is_err());
        assert!(rx.try_recv().is_err(), "a refused mutation must not broadcast");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon`
Expected: FAIL to compile — `new_with_paths` takes 5 arguments, `list_agent_profiles` not found.

- [ ] **Step 3: Add the field, the constructor parameter and the methods**

In `crates/clowder-daemon/src/server.rs`, add to the `Daemon` struct beside `projects`:

```rust
    profiles: Arc<crate::agent_profiles::AgentProfileStore>,
    /// Ticked after any successful profile mutation. Carries no payload: every control connection
    /// recomputes `AgentProfileList` + `AdapterList` from the store, so there is one code path
    /// from store state to wire events.
    profiles_tx: broadcast::Sender<()>,
```

In `new_with_paths`, add the parameter `profiles_path: PathBuf` **between `projects_path` and
`worktree_base`**, create `let (profiles_tx, _) = broadcast::channel(256);` beside the other
channels, and initialise:

```rust
            profiles: Arc::new(crate::agent_profiles::AgentProfileStore::new(profiles_path)),
            profiles_tx,
```

In `new_with` and `new_from_config`, pass `crate::agent_profiles::AgentProfileStore::default_path()`
in the new position.

Add the methods next to the project ones (after `remove_project`):

```rust
    pub fn subscribe_agent_profiles(&self) -> broadcast::Receiver<()> {
        self.profiles_tx.subscribe()
    }

    /// Every profile, enabled or not — what the Settings pane renders.
    pub fn list_agent_profiles(&self) -> Vec<clowder_proto::AgentProfileInfo> {
        self.profiles
            .effective()
            .into_iter()
            .map(|e| clowder_proto::AgentProfileInfo {
                id: e.profile.id,
                base: e.profile.base,
                display_name: e.profile.display_name,
                enabled: e.profile.enabled,
                args: e.profile.args,
                builtin: e.builtin,
            })
            .collect()
    }

    pub fn add_agent_profile(&self, p: clowder_proto::AgentProfileInfo) -> Result<()> {
        self.profiles.add(storage_profile(p))?;
        let _ = self.profiles_tx.send(());
        Ok(())
    }

    pub fn update_agent_profile(&self, p: clowder_proto::AgentProfileInfo) -> Result<()> {
        self.profiles.update(storage_profile(p))?;
        let _ = self.profiles_tx.send(());
        Ok(())
    }

    pub fn remove_agent_profile(&self, id: &str) -> Result<()> {
        self.profiles.remove(id)?;
        let _ = self.profiles_tx.send(());
        Ok(())
    }

    /// Resolve a spawnable profile id to its adapter + argument template.
    pub fn resolve_profile(&self, id: &str) -> Result<crate::agent_profiles::ResolvedProfile> {
        self.profiles.resolve(id)
    }
```

Add the free function beside `project_info`:

```rust
/// Wire form → storage form. `builtin` is derived, never stored, so it is dropped here.
pub(crate) fn storage_profile(p: clowder_proto::AgentProfileInfo) -> clowder_config::agents::AgentProfile {
    clowder_config::agents::AgentProfile {
        id: p.id,
        base: p.base,
        display_name: p.display_name,
        enabled: p.enabled,
        args: p.args,
    }
}
```

Replace the body of `list_adapters` (~line 1069):

```rust
    /// The agents a client may spawn: the ENABLED profiles, in effective order.
    pub fn list_adapters(&self) -> Vec<clowder_proto::AdapterInfo> {
        self.profiles
            .effective()
            .into_iter()
            .filter(|e| e.profile.enabled)
            .map(|e| clowder_proto::AdapterInfo {
                id: e.profile.id,
                display_name: e.profile.display_name,
            })
            .collect()
    }
```

- [ ] **Step 4: Update every `new_with_paths` call site**

The compiler is the checklist. Each existing call passes
`state.path().join("agents.json"), state.path().join("projects.json"), <worktrees>` — insert
`state.path().join("agent-profiles.json"),` before the worktrees argument. Like `worktree_base`, this
parameter is deliberately mandatory: a test that silently defaulted it would read the developer's
real `~/.local/state/clowder/agent-profiles.json` and become order-dependent.

Run: `source "$HOME/.cargo/env" && cargo build --workspace --tests 2>&1 | grep -c "^error"`
Repeat the edit until this prints `0`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon`
Expected: PASS, except `control_json.rs` — its `match` is not yet exhaustive over the new requests,
which Task 7 fixes. If the crate does not compile for that reason, do Task 7 before re-running.

- [ ] **Step 6: Commit**

```bash
git add crates/clowder-daemon crates/clowder-client/src/lib.rs
git commit -m "feat(daemon): drive the adapter list from stored agent profiles"
```

---

### Task 7: Control-socket handlers and the profiles broadcast

**Files:**
- Modify: `crates/clowder-daemon/src/control_json.rs` (request match ~line 54-125, `select!` arms ~line 132-169)
- Test: inline tests in the same file

**Interfaces:**
- Consumes: `Daemon::{list_agent_profiles, add_agent_profile, update_agent_profile, remove_agent_profile, subscribe_agent_profiles, list_adapters}`.
- Produces: the four request arms and the tick arm; no new Rust API.

- [ ] **Step 1: Write the failing test**

Add to the test module in `crates/clowder-daemon/src/control_json.rs`, following the shape of
`control_json_list_adapters_yields_adapter_list_with_codex`:

```rust
    #[tokio::test]
    async fn control_json_adds_a_profile_and_broadcasts_the_new_lists() {
        let state = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cjson-profiles.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        tokio::spawn(daemon.clone().handle_control_json(server));

        let (crd, mut cwr) = tokio::io::split(&mut client);
        let mut lines = BufReader::new(crd).lines();
        let _snapshot = lines.next_line().await.unwrap(); // the initial WorktreeList

        cwr.write_all(
            b"{\"type\":\"addAgentProfile\",\"profile\":{\"id\":\"opus\",\"base\":\"claude\",\
              \"displayName\":\"Claude (Opus)\",\"enabled\":true,\"args\":\"--model opus\",\"builtin\":false}}\n",
        )
        .await
        .unwrap();

        // The reply, then the broadcast pair — collect a few lines and assert on the set, since
        // the direct reply and the tick-driven events are not ordered against each other.
        let mut saw_profiles_with_opus = false;
        let mut saw_adapters_with_opus = false;
        for _ in 0..4 {
            let Ok(Some(l)) = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
                .await
                .unwrap()
            else {
                break;
            };
            match serde_json::from_str::<ControlEvent>(&l) {
                Ok(ControlEvent::AgentProfileList { profiles }) => {
                    saw_profiles_with_opus |= profiles.iter().any(|p| p.id == "opus");
                    assert!(profiles.iter().any(|p| p.id == "claude" && p.builtin));
                }
                Ok(ControlEvent::AdapterList { adapters }) => {
                    saw_adapters_with_opus |= adapters.iter().any(|a| a.id == "opus");
                }
                Ok(ControlEvent::Error { message }) => panic!("unexpected error: {message}"),
                _ => {}
            }
            if saw_profiles_with_opus && saw_adapters_with_opus {
                break;
            }
        }
        assert!(saw_profiles_with_opus, "an added profile must reach the profile list");
        assert!(saw_adapters_with_opus, "and the spawnable adapter list");
    }

    #[tokio::test]
    async fn control_json_refuses_removing_a_builtin() {
        let state = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cjson-builtin.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        tokio::spawn(daemon.clone().handle_control_json(server));

        let (crd, mut cwr) = tokio::io::split(&mut client);
        let mut lines = BufReader::new(crd).lines();
        let _snapshot = lines.next_line().await.unwrap();

        cwr.write_all(b"{\"type\":\"removeAgentProfile\",\"id\":\"claude\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        match serde_json::from_str::<ControlEvent>(&l).unwrap() {
            ControlEvent::Error { message } => assert!(message.contains("built-in"), "{message}"),
            other => panic!("expected an Error, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon control_json::`
Expected: FAIL to compile — non-exhaustive `match` over `ControlRequest`.

- [ ] **Step 3: Add the request arms**

In `handle_control_json`'s request match, beside the project arms:

```rust
                                Ok(ControlRequest::ListAgentProfiles) =>
                                    ControlEvent::AgentProfileList { profiles: self.list_agent_profiles() },
                                Ok(ControlRequest::AddAgentProfile { profile }) =>
                                    match self.add_agent_profile(profile) {
                                        Ok(()) => ControlEvent::AgentProfileList { profiles: self.list_agent_profiles() },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::UpdateAgentProfile { profile }) =>
                                    match self.update_agent_profile(profile) {
                                        Ok(()) => ControlEvent::AgentProfileList { profiles: self.list_agent_profiles() },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::RemoveAgentProfile { id }) =>
                                    match self.remove_agent_profile(&id) {
                                        Ok(()) => ControlEvent::AgentProfileList { profiles: self.list_agent_profiles() },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
```

- [ ] **Step 4: Add the broadcast arm**

Beside `let mut proj_rx = self.subscribe_projects();`:

```rust
        let mut prof_rx = self.subscribe_agent_profiles();
```

and, as a new `select!` arm after the `pc = proj_rx.recv()` arm:

```rust
                pf = prof_rx.recv() => {
                    match pf {
                        // A tick, not a payload: recompute both lists so the Settings pane and the
                        // New Worktree picker update together, from one source.
                        Ok(()) => {
                            write_event(&mut wr, &ControlEvent::AgentProfileList {
                                profiles: self.list_agent_profiles() }).await?;
                            write_event(&mut wr, &ControlEvent::AdapterList {
                                adapters: self.list_adapters() }).await?;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/clowder-daemon/src/control_json.rs
git commit -m "feat(daemon): serve agent profile requests over the control socket"
```

---

### Task 8: Spawn with profile args, and resume replay

**Files:**
- Modify: `crates/clowder-daemon/src/agent.rs` (add `SpawnSpec` after the `AgentAdapter` trait)
- Modify: `crates/clowder-daemon/src/lib.rs:17-20` (re-export `SpawnSpec`)
- Modify: `crates/clowder-daemon/src/registry.rs:6-20` (two fields)
- Modify: `crates/clowder-daemon/src/server.rs` (`spawn_agent` ~line 516, `resume_one` ~line 411)
- Modify: `crates/clowder-daemon/src/control_json.rs:175-179` (`spawn_from_control`)
- Modify: every `.spawn_agent(` call site (31, in `server.rs`, `control_json.rs`, `tests/agent_e2e.rs`)
- Modify: `AGENTS.md` (Runtime model paragraph)
- Test: inline tests in `crates/clowder-daemon/src/server.rs`

**Interfaces:**
- Consumes: `ResolvedProfile`, `clowder_config::agents::{substitute, TokenContext}`.
- Produces:
  - `pub struct SpawnSpec<'a> { pub adapter: &'a dyn AgentAdapter, pub profile_id: Option<String>, pub arg_template: Vec<String> }` with `SpawnSpec::adapter_only(&dyn AgentAdapter) -> SpawnSpec`
  - `Daemon::spawn_agent(&Arc<Self>, project: &Path, spec: SpawnSpec<'_>, name: &str) -> Result<PaneId>`
  - `AgentRecord.profile_id: Option<String>`, `AgentRecord.extra_args: Vec<String>`

- [ ] **Step 1: Write the failing tests**

Add to the test module in `crates/clowder-daemon/src/server.rs` (the existing spawn tests there show
the `init_repo` + `add_project` preamble to copy):

```rust
    #[test]
    fn spawn_appends_substituted_profile_args_to_the_adapter_args() {
        let state = tempfile::tempdir().unwrap();
        let repo = crate::test_support::init_repo();
        let daemon = std::sync::Arc::new(Daemon::new_with_paths(
            std::sync::Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-spawnargs.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));
        daemon.add_project(repo.path()).unwrap();

        // /bin/echo takes any arguments and exits — a real process, no agent binary needed.
        let adapter = SyntheticAdapter {
            command: PaneCommand { program: "/bin/echo".into(), args: vec!["base".into()], cwd: None, env: vec![] },
        };
        let spec = SpawnSpec {
            adapter: &adapter,
            profile_id: Some("echoer".into()),
            arg_template: clowder_config::agents::split_args("--w {{workspace_name}} --b {{branch}}").unwrap(),
        };
        let pane = daemon.spawn_agent(repo.path(), spec, "task-a").unwrap();

        let rec = daemon.registry_for_test().load().into_iter().find(|r| r.agent_id == pane.0).unwrap();
        assert_eq!(rec.profile_id.as_deref(), Some("echoer"));
        assert_eq!(rec.extra_args, vec!["--w", "task-a", "--b", "clowder/task-a"],
                   "tokens are substituted once, at spawn");
        assert_eq!(rec.adapter_id, "synthetic", "adapter_id still names the BASE adapter");
    }

    #[test]
    fn spawn_spec_adapter_only_records_no_profile_and_no_args() {
        let state = tempfile::tempdir().unwrap();
        let repo = crate::test_support::init_repo();
        let daemon = std::sync::Arc::new(Daemon::new_with_paths(
            std::sync::Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-spawnplain.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));
        daemon.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: PaneCommand { program: "/bin/echo".into(), args: vec![], cwd: None, env: vec![] },
        };
        let pane = daemon.spawn_agent(repo.path(), SpawnSpec::adapter_only(&adapter), "task-b").unwrap();
        let rec = daemon.registry_for_test().load().into_iter().find(|r| r.agent_id == pane.0).unwrap();
        assert_eq!(rec.profile_id, None);
        assert!(rec.extra_args.is_empty());
    }

    #[test]
    fn a_pre_m12_record_loads_with_no_profile_and_no_args() {
        // Additive-field evolution: records written before M12 must keep resuming.
        let rec: crate::registry::AgentRecord = serde_json::from_str(
            r#"{"agent_id":1,"project":"/p","task":"t","adapter_id":"claude","worktree_path":"/w",
                "branch":"clowder/t","workspace_kind":"git","cols":80,"rows":24}"#,
        )
        .unwrap();
        assert_eq!(rec.profile_id, None);
        assert!(rec.extra_args.is_empty());
    }

    #[test]
    fn resume_argv_is_the_resume_command_plus_the_recorded_args() {
        // The unit the reconcile path relies on: recorded args are replayed verbatim, never
        // re-substituted, so a deleted or edited profile cannot change a running agent's argv.
        let mut cmd = ClaudeAdapter.resume_command(std::path::Path::new("/w"));
        cmd.args.extend(vec!["--model".to_string(), "opus".to_string()]);
        assert_eq!(cmd.args, vec!["--continue", "--model", "opus"]);
    }
```

If `Daemon` has no `registry_for_test()` accessor, add one next to the other `#[cfg(test)]` helpers
in `server.rs`:

```rust
    #[cfg(test)]
    pub(crate) fn registry_for_test(&self) -> std::sync::Arc<crate::registry::Registry> {
        std::sync::Arc::clone(&self.registry)
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon spawn_`
Expected: FAIL to compile — `cannot find type SpawnSpec`.

- [ ] **Step 3: Add `SpawnSpec`**

In `crates/clowder-daemon/src/agent.rs`, after the `AgentAdapter` trait:

```rust
/// What to spawn: the base adapter, plus the profile that selected it and the argument template to
/// append. Substitution happens inside `Daemon::spawn_agent`, once the worktree exists.
pub struct SpawnSpec<'a> {
    pub adapter: &'a dyn AgentAdapter,
    /// The profile this came from, recorded on the agent. `None` for a direct adapter spawn (tests).
    pub profile_id: Option<String>,
    /// Split, not yet substituted.
    pub arg_template: Vec<String>,
}

impl<'a> SpawnSpec<'a> {
    /// A bare adapter spawn: no profile, no extra arguments.
    pub fn adapter_only(adapter: &'a dyn AgentAdapter) -> Self {
        Self { adapter, profile_id: None, arg_template: Vec::new() }
    }
}
```

Re-export from `crates/clowder-daemon/src/lib.rs`:

```rust
pub use agent::{
    adapter_descriptors, build_adapter, AdapterDescriptor, AgentAdapter, ClaudeAdapter, CodexAdapter,
    SpawnSpec, SyntheticAdapter,
};
```

- [ ] **Step 4: Add the record fields**

In `crates/clowder-daemon/src/registry.rs`, inside `AgentRecord`:

```rust
    /// The profile this agent was spawned from, if any. Informational — resume uses `extra_args`.
    #[serde(default)]
    pub profile_id: Option<String>,
    /// The profile's arguments AS SUBSTITUTED at spawn. Replayed verbatim on resume, so editing,
    /// renaming or deleting the profile can never change (or break) a running agent's argv.
    #[serde(default)]
    pub extra_args: Vec<String>,
```

- [ ] **Step 5: Use the spec in `spawn_agent`**

In `crates/clowder-daemon/src/server.rs`, change the signature and body:

```rust
    pub fn spawn_agent(self: &Arc<Self>, project: &Path, spec: SpawnSpec<'_>, name: &str) -> Result<PaneId> {
```

Immediately after `let ws = driver.provision(&self.worktrees, &project, task)?;` — the first point
where the worktree path and branch exist — add:

```rust
        let adapter = spec.adapter;
        // Substitute per already-split argument, so a value containing whitespace stays one argv
        // element and cannot inject arguments of its own.
        let extra_args = clowder_config::agents::substitute(
            &spec.arg_template,
            &clowder_config::agents::TokenContext {
                project_path: &project,
                workspace_path: &ws.path,
                workspace_name: task,
                branch: &ws.branch,
            },
        );
```

Inside the closure, append after the adapter's own args:

```rust
            let mut cmd = adapter.launch_command(&ws.path);
            cmd.args.extend(extra_args.iter().cloned());
```

and in the `registry.upsert(...)` call add:

```rust
            profile_id: spec.profile_id.clone(),
            extra_args: extra_args.clone(),
```

- [ ] **Step 6: Replay the args on resume**

In `resume_one` (~line 430), after `let mut cmd = adapter.resume_command(&ws.path);`:

```rust
        cmd.args.extend(rec.extra_args.iter().cloned());
```

- [ ] **Step 7: Resolve the profile in `spawn_from_control`**

In `crates/clowder-daemon/src/control_json.rs`:

```rust
    fn spawn_from_control(self: &Arc<Self>, project: &str, name: &str, profile: &str) -> Result<PaneId> {
        let project_path = Path::new(project);
        // `adapter` on the wire is a PROFILE id now. Built-in ids (claude/codex/shell) still
        // resolve, so `clowder spawn <project> <name> claude` is unchanged.
        let resolved = self.resolve_profile(profile)?;
        let a = build_adapter(&resolved.base)
            .ok_or_else(|| anyhow!("unknown adapter: {}", resolved.base))?;
        self.spawn_agent(
            project_path,
            crate::SpawnSpec {
                adapter: a.as_ref(),
                profile_id: Some(resolved.profile_id),
                arg_template: resolved.arg_template,
            },
            name,
        )
    }
```

- [ ] **Step 8: Update every `.spawn_agent(` call site**

Each existing `daemon.spawn_agent(repo.path(), &adapter, "task-a")` becomes
`daemon.spawn_agent(repo.path(), SpawnSpec::adapter_only(&adapter), "task-a")`. The compiler lists
them all.

Run: `source "$HOME/.cargo/env" && cargo build --workspace --tests 2>&1 | grep -c "^error"`
Repeat until it prints `0`.

- [ ] **Step 9: Run the full workspace suite**

Run: `source "$HOME/.cargo/env" && cargo test --workspace`
Expected: PASS. Three `clowder-daemon` tests are known to flake under parallel load — re-run any
failure once before investigating.

- [ ] **Step 10: Update `AGENTS.md`**

In the **Runtime model** section, after the sentence listing the adapters, add:

```markdown
The spawnable list is **not** the adapter list: it is the set of enabled **agent profiles** — named
wrappers around those adapters, each with an argument template appended to the adapter's own args —
stored per-daemon in `$XDG_STATE_HOME/clowder/agent-profiles.json` (`CLOWDER_AGENT_PROFILES_FILE`
overrides) and managed with `clowder agent add|list|set|enable|disable|rm` or the Settings window's
Agents tab. The file holds only deltas: built-ins always exist (disable-able, not deletable) and
appear even if the file is empty. Template tokens (`{{project_name}}`, `{{project_path}}`,
`{{workspace_name}}`, `{{workspace_path}}`, `{{branch}}`) are substituted **per already-split
argument** at spawn, and the resolved arguments are recorded on the agent, so editing or deleting a
profile never changes what a running agent resumes with.
```

Also add `crates/clowder-daemon/src/agent_profiles.rs` to the daemon's row in the **Repo layout**
table description.

- [ ] **Step 11: Commit and open the PR**

```bash
git add crates/clowder-daemon crates/clowder-client AGENTS.md
git commit -m "feat(daemon): spawn agents with profile arguments and replay them on resume"
gh pr create --base feat/m12a-agent-profile-core --title "feat(daemon): agent profile store and spawning (M12b)" \
  --body "Second of four stacked PRs for #80. The daemon owns agent-profiles.json, serves it over the control socket, drives ListAdapters from the enabled profiles, appends substituted profile args at spawn and replays the recorded args on resume.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_01QZBTSP2UQiEkGK5j8zcUKN"
```

---

# PR 3 — M12c: the CLI

Branch: `feat/m12c-agent-cli` off `feat/m12b-agent-profile-daemon`.

### Task 9: `*_via_control` helpers

**Files:**
- Modify: `crates/clowder-client/src/lib.rs` (beside `list_projects_via_control`, ~line 164)
- Test: inline tests in the same file (follow the existing `*_via_control` tests, which spin a
  `Daemon` and its control socket)

**Interfaces:**
- Produces:
  - `list_agent_profiles_via_control(&Path) -> Result<Vec<AgentProfileInfo>>`
  - `add_agent_profile_via_control(&Path, AgentProfileInfo) -> Result<Vec<AgentProfileInfo>>`
  - `update_agent_profile_via_control(&Path, AgentProfileInfo) -> Result<Vec<AgentProfileInfo>>`
  - `remove_agent_profile_via_control(&Path, &str) -> Result<Vec<AgentProfileInfo>>`

All four return the resulting profile list, so the CLI can print the new state without a second
round trip.

- [ ] **Step 1: Write the failing test**

Add to the test module in `crates/clowder-client/src/lib.rs`:

```rust
    #[tokio::test]
    async fn agent_profile_helpers_round_trip_against_a_daemon() {
        // Same setup as the spawn test above (lib.rs:390-404) minus the git repo — profiles need
        // no project. Each test gets its own state dir so none can read the developer's real file.
        let sockdir = tempfile::tempdir().unwrap();
        let sock = sockdir.path().join("control.sock");
        let state = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cli-profiles.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
            state.path().join("agent-profiles.json"),
            state.path().join("worktrees"),
        ));
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move { let _ = daemon.serve_control_json(listener).await; });

        let before = list_agent_profiles_via_control(&sock).await.unwrap();
        assert_eq!(before.len(), 3, "the three builtins");

        let after = add_agent_profile_via_control(
            &sock,
            clowder_proto::AgentProfileInfo {
                id: "opus".into(),
                base: "claude".into(),
                display_name: "Claude (Opus)".into(),
                enabled: true,
                args: "--model opus".into(),
                builtin: false,
            },
        )
        .await
        .unwrap();
        assert!(after.iter().any(|p| p.id == "opus"));

        let mut opus = after.into_iter().find(|p| p.id == "opus").unwrap();
        opus.enabled = false;
        let after = update_agent_profile_via_control(&sock, opus).await.unwrap();
        assert!(!after.iter().find(|p| p.id == "opus").unwrap().enabled);

        let after = remove_agent_profile_via_control(&sock, "opus").await.unwrap();
        assert!(!after.iter().any(|p| p.id == "opus"));

        let e = remove_agent_profile_via_control(&sock, "claude").await.unwrap_err().to_string();
        assert!(e.contains("built-in"), "{e}");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client agent_profile`
Expected: FAIL — `cannot find function list_agent_profiles_via_control`.

- [ ] **Step 3: Implement the helpers**

Add to `crates/clowder-client/src/lib.rs`:

```rust
/// Send one profile request and wait for the resulting `AgentProfileList`.
///
/// Shared by all four helpers: every profile mutation answers with the full new list, so the CLI
/// can print the resulting state without a second round trip.
async fn agent_profiles_request(
    control_sock: &std::path::Path,
    req: ControlRequest,
) -> anyhow::Result<Vec<clowder_proto::AgentProfileInfo>> {
    let stream = UnixStream::connect(control_sock).await?;
    let (rd, mut wr) = tokio::io::split(stream);
    let mut lines = BufReader::new(rd).lines();

    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    wr.write_all(line.as_bytes()).await?;

    loop {
        match next_control_line(&mut lines).await? {
            Some(l) => match serde_json::from_str::<ControlEvent>(&l) {
                Ok(ControlEvent::AgentProfileList { profiles }) => return Ok(profiles),
                Ok(ControlEvent::Error { message }) => return Err(anyhow::anyhow!(message)),
                Ok(_) => continue,  // the initial WorktreeList, streamed attention, ...
                Err(_) => continue, // ignore unparseable lines defensively
            },
            None => return Err(anyhow::anyhow!("control socket closed before the result")),
        }
    }
}

pub async fn list_agent_profiles_via_control(
    control_sock: &std::path::Path,
) -> anyhow::Result<Vec<clowder_proto::AgentProfileInfo>> {
    agent_profiles_request(control_sock, ControlRequest::ListAgentProfiles).await
}

pub async fn add_agent_profile_via_control(
    control_sock: &std::path::Path,
    profile: clowder_proto::AgentProfileInfo,
) -> anyhow::Result<Vec<clowder_proto::AgentProfileInfo>> {
    agent_profiles_request(control_sock, ControlRequest::AddAgentProfile { profile }).await
}

pub async fn update_agent_profile_via_control(
    control_sock: &std::path::Path,
    profile: clowder_proto::AgentProfileInfo,
) -> anyhow::Result<Vec<clowder_proto::AgentProfileInfo>> {
    agent_profiles_request(control_sock, ControlRequest::UpdateAgentProfile { profile }).await
}

pub async fn remove_agent_profile_via_control(
    control_sock: &std::path::Path,
    id: &str,
) -> anyhow::Result<Vec<clowder_proto::AgentProfileInfo>> {
    agent_profiles_request(control_sock, ControlRequest::RemoveAgentProfile { id: id.to_string() }).await
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client agent_profile`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-client/src/lib.rs
git commit -m "feat(client): add agent profile control helpers"
```

---

### Task 10: `clowder agent` subcommand

**Files:**
- Modify: `crates/clowder-client/src/main.rs` (new `Some("agent")` arm; usage line at the bottom)
- Create: `crates/clowder-client/src/agent_cli.rs`
- Modify: `crates/clowder-client/src/lib.rs` (add `pub mod agent_cli;`)
- Modify: `crates/clowder-client/src/remote_cli.rs:8-10` (`VALUE_FLAGS`)
- Modify: `AGENTS.md` (Runtime model — the CLI list)
- Test: inline tests in `crates/clowder-client/src/agent_cli.rs`

**Interfaces:**
- Consumes: the four `*_via_control` helpers, `remote_cli::parse_flags` (`Flags::positional(n)`,
  `::str(key)`, `::bool(key)`, `::reject_unknown(allowed)` — note it is `bool`, **not** `has`),
  `clowder_config::agents::{validate_id, validate_template}`.
- Produces: `pub async fn run(args: &[String]) -> anyhow::Result<()>` and the pure
  `pub fn plan(args: &[String], existing: &[AgentProfileInfo]) -> Result<Action, String>`.

Splitting a pure `plan` out of `run` is what makes the CLI testable without a daemon — the same
argument-shape decisions the daemon would otherwise have to be spun up to exercise.

- [ ] **Step 1: Write the failing tests**

Create `crates/clowder-client/src/agent_cli.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn existing() -> Vec<clowder_proto::AgentProfileInfo> {
        vec![
            clowder_proto::AgentProfileInfo {
                id: "claude".into(), base: "claude".into(), display_name: "Claude Code".into(),
                enabled: true, args: String::new(), builtin: true,
            },
            clowder_proto::AgentProfileInfo {
                id: "opus".into(), base: "claude".into(), display_name: "Claude (Opus)".into(),
                enabled: true, args: "--model opus".into(), builtin: false,
            },
        ]
    }

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn add_builds_a_profile_from_flags() {
        let a = args("add plan --base claude --name Planner --args --permission-mode=plan");
        match plan(&a, &existing()).unwrap() {
            Action::Add(p) => {
                assert_eq!((p.id.as_str(), p.base.as_str()), ("plan", "claude"));
                assert_eq!(p.display_name, "Planner");
                assert_eq!(p.args, "--permission-mode=plan");
                assert!(p.enabled);
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    #[test]
    fn add_defaults_the_display_name_to_the_id_and_honours_disabled() {
        match plan(&args("add plan --base shell --disabled"), &existing()).unwrap() {
            Action::Add(p) => {
                assert_eq!(p.display_name, "plan");
                assert!(!p.enabled);
                assert_eq!(p.args, "");
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    #[test]
    fn add_requires_a_base_and_a_valid_id_and_valid_args() {
        assert!(plan(&args("add plan"), &existing()).is_err(), "--base is required");
        assert!(plan(&args("add has\\ space --base claude"), &existing()).is_err());
        let e = plan(&args("add p --base claude --args {{nope}}"), &existing()).unwrap_err();
        assert!(e.contains("nope"), "args are validated before the daemon is dialled: {e}");
    }

    #[test]
    fn set_patches_only_the_flags_given() {
        match plan(&args("set opus --name Opus"), &existing()).unwrap() {
            Action::Update(p) => {
                assert_eq!(p.display_name, "Opus");
                assert_eq!(p.args, "--model opus", "an untouched field keeps its stored value");
                assert!(p.enabled);
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn enable_and_disable_flip_only_that_field() {
        match plan(&args("disable opus"), &existing()).unwrap() {
            Action::Update(p) => {
                assert!(!p.enabled);
                assert_eq!(p.args, "--model opus");
                assert_eq!(p.display_name, "Claude (Opus)");
            }
            other => panic!("expected Update, got {other:?}"),
        }
        match plan(&args("enable opus"), &existing()).unwrap() {
            Action::Update(p) => assert!(p.enabled),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn set_and_rm_reject_an_unknown_id() {
        assert!(plan(&args("set ghost --name x"), &existing()).unwrap_err().contains("ghost"));
        assert!(plan(&args("rm ghost"), &existing()).unwrap_err().contains("ghost"));
    }

    #[test]
    fn rm_refuses_a_builtin_locally_with_the_same_advice_as_the_daemon() {
        let e = plan(&args("rm claude"), &existing()).unwrap_err();
        assert!(e.contains("built-in") && e.contains("disable"), "{e}");
    }

    #[test]
    fn list_needs_no_id() {
        assert!(matches!(plan(&args("list"), &existing()).unwrap(), Action::List));
    }

    #[test]
    fn an_unknown_subcommand_reports_usage() {
        assert!(plan(&args("frobnicate"), &existing()).unwrap_err().contains("usage"));
        assert!(plan(&[], &existing()).unwrap_err().contains("usage"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client agent_cli`
Expected: FAIL — module not declared / `plan` not found.

- [ ] **Step 3: Implement the CLI**

Add above the test module in `crates/clowder-client/src/agent_cli.rs`:

```rust
//! `clowder agent …` — manage the daemon's agent profiles.
//!
//! `plan` is pure so the argument shapes are tested without a daemon; `run` dials the control
//! socket and prints. The daemon revalidates everything — a remote client is untrusted — but
//! validating here too means a typo fails instantly and locally.

use crate::{
    add_agent_profile_via_control, list_agent_profiles_via_control, remove_agent_profile_via_control,
    update_agent_profile_via_control,
};
use clowder_proto::AgentProfileInfo;

const USAGE: &str = "usage: clowder agent <list|add|set|enable|disable|rm> ...\n\
    \x20 clowder agent list\n\
    \x20 clowder agent add <id> --base <claude|codex|shell> [--name <s>] [--args \"<template>\"] [--disabled]\n\
    \x20 clowder agent set <id> [--name <s>] [--args \"<template>\"]\n\
    \x20 clowder agent enable <id>\n\
    \x20 clowder agent disable <id>\n\
    \x20 clowder agent rm <id>";

#[derive(Debug)]
pub enum Action {
    List,
    Add(AgentProfileInfo),
    Update(AgentProfileInfo),
    Remove(String),
}

/// Decide what `args` means against the daemon's current profiles. Pure.
pub fn plan(args: &[String], existing: &[AgentProfileInfo]) -> Result<Action, String> {
    let sub = args.first().map(|s| s.as_str()).ok_or_else(|| USAGE.to_string())?;
    if sub == "list" {
        return Ok(Action::List);
    }
    // Parse the whole tail: `parse_flags` consumes each value flag's value, so the id is simply
    // the first positional — and `agent add --base claude plan` works as well as the usual order.
    let flags = crate::remote_cli::parse_flags(&args[1..])?;
    let id = flags
        .positional(0)
        .map(str::to_string)
        .ok_or_else(|| format!("{USAGE}\n\nmissing <id>"))?;

    let find = |id: &str| {
        existing
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| format!("no such agent: {id}"))
    };

    match sub {
        "add" => {
            flags.reject_unknown(&["base", "name", "args", "disabled"])?;
            clowder_config::agents::validate_id(&id)?;
            if existing.iter().any(|p| p.id == id) {
                return Err(format!("an agent named {id} already exists"));
            }
            let base = flags
                .str("base")
                .ok_or_else(|| "add requires --base <claude|codex|shell>".to_string())?
                .to_string();
            let args_template = flags.str("args").unwrap_or_default().to_string();
            clowder_config::agents::validate_template(&args_template)?;
            Ok(Action::Add(AgentProfileInfo {
                display_name: flags.str("name").unwrap_or(&id).to_string(),
                id,
                base,
                enabled: !flags.bool("disabled"),
                args: args_template,
                builtin: false,
            }))
        }
        "set" => {
            flags.reject_unknown(&["name", "args"])?;
            let mut p = find(&id)?;
            if let Some(name) = flags.str("name") {
                p.display_name = name.to_string();
            }
            if let Some(a) = flags.str("args") {
                clowder_config::agents::validate_template(a)?;
                p.args = a.to_string();
            }
            Ok(Action::Update(p))
        }
        "enable" | "disable" => {
            flags.reject_unknown(&[])?;
            let mut p = find(&id)?;
            p.enabled = sub == "enable";
            Ok(Action::Update(p))
        }
        "rm" => {
            flags.reject_unknown(&[])?;
            let p = find(&id)?;
            if p.builtin {
                return Err(format!(
                    "{id} is a built-in agent and cannot be removed — run `clowder agent disable {id}` instead"
                ));
            }
            Ok(Action::Remove(id))
        }
        other => Err(format!("{USAGE}\n\nunknown subcommand: {other}")),
    }
}

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let sock = clowder_config::Config::load().control_sock;
    let existing = list_agent_profiles_via_control(&sock).await?;
    let action = plan(args, &existing).map_err(anyhow::Error::msg)?;
    let profiles = match action {
        Action::List => existing,
        Action::Add(p) => add_agent_profile_via_control(&sock, p).await?,
        Action::Update(p) => update_agent_profile_via_control(&sock, p).await?,
        Action::Remove(id) => remove_agent_profile_via_control(&sock, &id).await?,
    };
    for p in profiles {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            p.id,
            p.base,
            if p.enabled { "enabled" } else { "disabled" },
            p.display_name,
            p.args
        );
    }
    Ok(())
}
```

- [ ] **Step 4: Register the value-taking flags**

`parse_flags` only consumes a value for flags listed in `VALUE_FLAGS`; without this,
`--base claude` parses as a valueless `--base` plus a stray positional and `flags.str("base")`
returns `None`. In `crates/clowder-client/src/remote_cli.rs:8`:

```rust
const VALUE_FLAGS: &[&str] = &[
    "address", "token", "rename", "fingerprint", "timeout", "socket-dir", "base", "name", "args",
];
```

- [ ] **Step 5: Wire it into `main.rs`**

In `crates/clowder-client/src/main.rs`, beside `Some("remote") => …`:

```rust
        Some("agent") => clowder_client::agent_cli::run(&args[2..]).await,
```

and extend the final usage string to
`usage: clowder <spawn|project|agent|attach|connect|remote|remote-host|remote-token> ...`.

Declare the module in `crates/clowder-client/src/lib.rs` beside `pub mod remote_cli;`:

```rust
pub mod agent_cli;
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client`
Expected: PASS.

- [ ] **Step 7: Try it end to end**

```bash
source "$HOME/.cargo/env" && cargo build
CLOWDER_AGENT_PROFILES_FILE=/tmp/m12-cli-demo.json ./target/debug/clowder-daemon &
sleep 1
./target/debug/clowder agent list
./target/debug/clowder agent add opus --base claude --name "Claude (Opus)" --args "--model opus"
./target/debug/clowder agent disable codex
./target/debug/clowder agent list
./target/debug/clowder agent rm claude   # must fail, pointing at disable
kill %1
```

Expected: `list` shows three built-ins, then four rows with `opus` added and `codex` disabled; the
final `rm` prints the "built-in … disable" error and exits non-zero.

- [ ] **Step 8: Update `AGENTS.md`**

In the Runtime model paragraph added in Task 8, the `clowder agent …` command list is already
mentioned; confirm it matches the real subcommands and fix any drift.

- [ ] **Step 9: Commit and open the PR**

```bash
git add crates/clowder-client AGENTS.md
git commit -m "feat(client): add the clowder agent CLI"
gh pr create --base feat/m12b-agent-profile-daemon --title "feat(client): clowder agent CLI (M12c)" \
  --body "Third of four stacked PRs for #80. \`clowder agent list|add|set|enable|disable|rm\` over the control socket, with a pure \`plan\` covering the argument shapes.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_01QZBTSP2UQiEkGK5j8zcUKN"
```

---

# PR 4 — M12d: the Settings tab

Branch: `feat/m12d-agent-settings-ui` off `feat/m12c-agent-cli`.

### Task 11: Swift wire types and the store

**Files:**
- Modify: `macos/Sources/ClowderCore/Models.swift` (`AdapterInfo` ~line 34, `ControlRequest` encode ~line 90-128, `ControlEvent` ~line 133-189)
- Modify: `macos/Sources/ClowderCore/AgentStore.swift` (`apply` ~line 37, `reset` ~line 103)
- Test: `macos/Tests/ClowderCoreTests/ModelsTests.swift`, `macos/Tests/ClowderCoreTests/AgentStoreTests.swift`

**Interfaces:**
- Produces:
  - `public struct AgentProfileInfo: Codable, Identifiable, Equatable, Sendable { id, base, displayName, enabled, args, builtin }`
  - `ControlRequest.listAgentProfiles / .addAgentProfile(AgentProfileInfo) / .updateAgentProfile(AgentProfileInfo) / .removeAgentProfile(id: String)`
  - `ControlEvent.agentProfileList([AgentProfileInfo])`
  - `AgentStore.agentProfiles: [AgentProfileInfo]`

- [ ] **Step 1: Write the failing tests**

Add to `macos/Tests/ClowderCoreTests/ModelsTests.swift`:

```swift
    func testAgentProfileRequestsEncodeLikeTheRustEnum() throws {
        let p = AgentProfileInfo(id: "opus", base: "claude", displayName: "Claude (Opus)",
                                 enabled: true, args: "--model opus", builtin: false)
        let add = try JSONEncoder().encode(ControlRequest.addAgentProfile(p))
        let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: add) as? [String: Any])
        XCTAssertEqual(obj["type"] as? String, "addAgentProfile")
        let profile = try XCTUnwrap(obj["profile"] as? [String: Any])
        XCTAssertEqual(profile["displayName"] as? String, "Claude (Opus)")
        XCTAssertEqual(profile["builtin"] as? Bool, false)

        let list = try JSONEncoder().encode(ControlRequest.listAgentProfiles)
        XCTAssertEqual(String(decoding: list, as: UTF8.self), #"{"type":"listAgentProfiles"}"#)

        let rm = try JSONEncoder().encode(ControlRequest.removeAgentProfile(id: "opus"))
        let rmObj = try XCTUnwrap(JSONSerialization.jsonObject(with: rm) as? [String: Any])
        XCTAssertEqual(rmObj["type"] as? String, "removeAgentProfile")
        XCTAssertEqual(rmObj["id"] as? String, "opus")
    }

    func testAgentProfileListEventDecodes() throws {
        let json = #"""
        {"type":"agentProfileList","profiles":[
          {"id":"claude","base":"claude","displayName":"Claude Code","enabled":true,"args":"","builtin":true},
          {"id":"opus","base":"claude","displayName":"Claude (Opus)","enabled":false,"args":"--model opus","builtin":false}
        ]}
        """#
        let ev = try JSONDecoder().decode(ControlEvent.self, from: Data(json.utf8))
        guard case let .agentProfileList(profiles) = ev else { return XCTFail("wrong case: \(ev)") }
        XCTAssertEqual(profiles.count, 2)
        XCTAssertTrue(profiles[0].builtin)
        XCTAssertFalse(profiles[1].enabled)
        XCTAssertEqual(profiles[1].args, "--model opus")
    }
```

Add to `macos/Tests/ClowderCoreTests/AgentStoreTests.swift`:

```swift
    func testAgentProfileListIsStoredAndClearedOnReset() {
        let store = AgentStore()
        XCTAssertTrue(store.agentProfiles.isEmpty)
        store.apply(.agentProfileList([
            AgentProfileInfo(id: "claude", base: "claude", displayName: "Claude Code",
                             enabled: true, args: "", builtin: true)
        ]))
        XCTAssertEqual(store.agentProfiles.map(\.id), ["claude"])
        // Backend switches must not carry one host's profiles to another.
        store.reset()
        XCTAssertTrue(store.agentProfiles.isEmpty)
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd macos && swift test --filter ModelsTests`
Expected: FAIL to compile — `cannot find AgentProfileInfo`.

- [ ] **Step 3: Add the model, request cases and event case**

In `macos/Sources/ClowderCore/Models.swift`, after `AdapterInfo`:

```swift
/// Mirrors the Rust `AgentProfileInfo`. `builtin` is derived daemon-side: built-in agents can be
/// edited and disabled but never removed.
public struct AgentProfileInfo: Codable, Identifiable, Equatable, Sendable {
    public let id: String
    public var base: String
    public var displayName: String
    public var enabled: Bool
    public var args: String
    public let builtin: Bool
    public init(id: String, base: String, displayName: String, enabled: Bool, args: String, builtin: Bool) {
        self.id = id
        self.base = base
        self.displayName = displayName
        self.enabled = enabled
        self.args = args
        self.builtin = builtin
    }
}
```

Add the cases to `ControlRequest`, a `profiles` / `profile` / `id` key to its `CodingKeys` as needed,
and to `encode(to:)`:

```swift
        case .listAgentProfiles:
            try c.encode("listAgentProfiles", forKey: .type)
        case let .addAgentProfile(profile):
            try c.encode("addAgentProfile", forKey: .type)
            try c.encode(profile, forKey: .profile)
        case let .updateAgentProfile(profile):
            try c.encode("updateAgentProfile", forKey: .type)
            try c.encode(profile, forKey: .profile)
        case let .removeAgentProfile(id):
            try c.encode("removeAgentProfile", forKey: .type)
            try c.encode(id, forKey: .id)
```

Add to `ControlEvent`: the case `agentProfileList([AgentProfileInfo])`, the `profiles` coding key,
and the decode arm:

```swift
        case "agentProfileList":
            self = .agentProfileList(try c.decode([AgentProfileInfo].self, forKey: .profiles))
```

- [ ] **Step 4: Store them**

In `macos/Sources/ClowderCore/AgentStore.swift`, add the published property beside `adapters`:

```swift
    /// Every profile, enabled or not — what the Settings Agents pane renders. `adapters` remains
    /// the ENABLED subset the daemon sends for the New Worktree picker.
    @Published public private(set) var agentProfiles: [AgentProfileInfo] = []
```

an `apply` arm:

```swift
        case let .agentProfileList(list):
            agentProfiles = list
```

and a line in `reset()`:

```swift
        agentProfiles = []
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd macos && swift test --filter 'ModelsTests|AgentStoreTests'`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/Models.swift macos/Sources/ClowderCore/AgentStore.swift macos/Tests/ClowderCoreTests
git commit -m "feat(app): decode agent profiles from the control socket"
```

---

### Task 12: `AgentArgs` (Swift port) and `AgentProfileDraft`

**Files:**
- Create: `macos/Sources/ClowderCore/AgentArgs.swift`
- Create: `macos/Tests/ClowderCoreTests/AgentArgsTests.swift`
- Modify: `macos/Sources/ClowderCore/SheetForms.swift` (append `AgentProfileDraft`)
- Modify: `macos/Tests/ClowderCoreTests/SheetFormsTests.swift`

**Interfaces:**
- Consumes: `docs/protocol/fixtures/agent-args.json`.
- Produces:
  - `public enum AgentArgs { static let tokens: [String]; static func split(_:) throws -> [String]; static func templateError(_:) -> String?; static func preview(_:) -> String }`
  - `public struct AgentProfileDraft: Equatable, Sendable { id, base, displayName, enabled, args, isNew; idError; displayNameError; argsError; isValid }`

The Swift splitter exists so the editor can reject a bad template *as typed* and show a resolved-argv
preview. The fixture is what keeps it identical to `clowder_config::agents`.

- [ ] **Step 1: Write the failing tests**

Create `macos/Tests/ClowderCoreTests/AgentArgsTests.swift`:

```swift
import XCTest
@testable import ClowderCore

final class AgentArgsTests: XCTestCase {
    private struct Case: Decodable {
        let input: String
        let argv: [String]?
        let error: String?
    }

    private func cases(file: StaticString = #filePath) throws -> [Case] {
        let here = URL(fileURLWithPath: "\(file)")
        let repo = here.deletingLastPathComponent()   // ClowderCoreTests
            .deletingLastPathComponent()              // Tests
            .deletingLastPathComponent()              // macos
            .deletingLastPathComponent()              // repo root
        let data = try Data(contentsOf: repo.appendingPathComponent("docs/protocol/fixtures/agent-args.json"))
        return try JSONDecoder().decode([Case].self, from: data)
    }

    func testSplitAgreesWithTheSharedFixture() throws {
        let all = try cases()
        XCTAssertFalse(all.isEmpty, "fixture must not be empty")
        for c in all {
            switch (c.argv, c.error) {
            case let (argv?, _):
                XCTAssertEqual(try? AgentArgs.split(c.input), argv,
                               "split disagreed on \(c.input.debugDescription) — if you changed a rule, "
                               + "update the shared cases AND clowder_config::agents::split_args")
            case (nil, "quote"):
                XCTAssertThrowsError(try AgentArgs.split(c.input), c.input)
            case (nil, "token"):
                XCTAssertNoThrow(try AgentArgs.split(c.input), c.input)
            default:
                XCTFail("case \(c.input.debugDescription) has neither argv nor a known error")
            }
        }
    }

    func testTemplateErrorAgreesWithTheSharedFixture() throws {
        for c in try cases() {
            if c.argv != nil {
                XCTAssertNil(AgentArgs.templateError(c.input), c.input)
            } else {
                XCTAssertNotNil(AgentArgs.templateError(c.input), c.input)
            }
        }
    }

    func testTemplateErrorNamesTheOffendingToken() {
        let e = AgentArgs.templateError("--x {{nope}}")
        XCTAssertTrue(e?.contains("nope") == true, "unhelpful: \(e ?? "nil")")
        XCTAssertTrue(e?.contains("workspace_name") == true, "must list the valid tokens: \(e ?? "nil")")
    }

    func testPreviewShowsResolvedArgumentsOneQuotedElementEach() {
        let out = AgentArgs.preview("--prompt \"work on {{workspace_name}}\" --p {{project_name}}")
        XCTAssertEqual(out, "--prompt 'work on my-task' --p my-project")
    }

    func testPreviewOfABadTemplateIsEmptyRatherThanMisleading() {
        XCTAssertEqual(AgentArgs.preview("\"unterminated"), "")
    }
}
```

Add to `macos/Tests/ClowderCoreTests/SheetFormsTests.swift`:

```swift
final class AgentProfileDraftTests: XCTestCase {
    func testValidDraftIsValid() {
        let d = AgentProfileDraft(id: "opus", base: "claude", displayName: "Claude (Opus)",
                                  enabled: true, args: "--model opus", isNew: true)
        XCTAssertNil(d.idError)
        XCTAssertNil(d.displayNameError)
        XCTAssertNil(d.argsError)
        XCTAssertTrue(d.isValid)
    }

    func testIdFollowsTheHostNameRule() throws {
        var d = AgentProfileDraft(id: "has space", base: "claude", displayName: "x", enabled: true,
                                  args: "", isNew: true)
        XCTAssertNotNil(d.idError)
        d.id = ""
        XCTAssertNotNil(d.idError)
        d.id = "a.b-c_1"
        XCTAssertNil(d.idError)
    }

    func testBlankDisplayNameAndBadArgsAreRejected() {
        var d = AgentProfileDraft(id: "opus", base: "claude", displayName: "  ", enabled: true,
                                  args: "", isNew: true)
        XCTAssertNotNil(d.displayNameError)
        XCTAssertFalse(d.isValid)

        d.displayName = "Opus"
        d.args = "--x {{nope}}"
        XCTAssertNotNil(d.argsError)
        XCTAssertFalse(d.isValid)
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd macos && swift test --filter AgentArgsTests`
Expected: FAIL to compile — `cannot find 'AgentArgs' in scope`.

- [ ] **Step 3: Implement `AgentArgs`**

Create `macos/Sources/ClowderCore/AgentArgs.swift`:

```swift
import Foundation

/// The Swift half of the agent-argument rules, mirroring `clowder_config::agents`.
///
/// It exists so the Settings editor can reject a bad template as typed and show a resolved preview.
/// The daemon remains the authority — anything that slips through here still gets a clean error
/// back. Both halves are pinned to `docs/protocol/fixtures/agent-args.json`, so they cannot drift.
public enum AgentArgs {
    /// Must match `clowder_config::agents::TOKENS` exactly, in the same order.
    public static let tokens = ["project_name", "project_path", "workspace_name", "workspace_path", "branch"]

    public struct SplitError: Error, Equatable { public let message: String }

    /// Split a template the way `split_args` does: whitespace separates; `'…'` is fully literal;
    /// `"…"` honours `\"` and `\\`; `\` escapes outside quotes. No shell, no globbing, no `$VAR`.
    public static func split(_ s: String) throws -> [String] {
        var out: [String] = []
        var cur = ""
        var hasCur = false          // distinguishes `""` (an empty arg) from a gap between args
        var it = Array(s)
        var i = 0
        while i < it.count {
            let c = it[i]
            i += 1
            if c.isWhitespace {
                if hasCur { out.append(cur); cur = ""; hasCur = false }
            } else if c == "'" {
                hasCur = true
                var closed = false
                while i < it.count {
                    let n = it[i]; i += 1
                    if n == "'" { closed = true; break }
                    cur.append(n)
                }
                if !closed { throw SplitError(message: "unterminated single quote (') in arguments") }
            } else if c == "\"" {
                hasCur = true
                var closed = false
                while i < it.count {
                    let n = it[i]; i += 1
                    if n == "\"" { closed = true; break }
                    if n == "\\" {
                        guard i < it.count else { break }
                        let e = it[i]; i += 1
                        if e == "\"" || e == "\\" { cur.append(e) } else { cur.append("\\"); cur.append(e) }
                    } else {
                        cur.append(n)
                    }
                }
                if !closed { throw SplitError(message: "unterminated double quote (\") in arguments") }
            } else if c == "\\" {
                hasCur = true
                guard i < it.count else { throw SplitError(message: "trailing backslash (\\) in arguments") }
                cur.append(it[i]); i += 1
            } else {
                hasCur = true
                cur.append(c)
            }
        }
        if hasCur { out.append(cur) }
        return out
    }

    /// Nil when the template is acceptable; otherwise a user-facing reason (quoting or a bad token).
    public static func templateError(_ s: String) -> String? {
        let argv: [String]
        do { argv = try split(s) } catch let e as SplitError { return e.message } catch { return "\(error)" }
        let valid = tokens.map { "{{\($0)}}" }.joined(separator: ", ")
        for arg in argv {
            var rest = Substring(arg)
            while let start = rest.range(of: "{{") {
                let after = rest[start.upperBound...]
                guard let end = after.range(of: "}}") else { return "unclosed '{{' in \(arg)" }
                let token = String(after[..<end.lowerBound])
                if !tokens.contains(token) {
                    return "unknown token {{\(token)}} — valid tokens are \(valid)"
                }
                rest = after[end.upperBound...]
            }
        }
        return nil
    }

    /// What the arguments look like once resolved, using illustrative values — the editor's live
    /// preview. Each argv element is single-quoted when it contains whitespace, so the user can see
    /// that a value with a space stays ONE argument. Empty when the template does not parse.
    public static func preview(_ s: String) -> String {
        guard templateError(s) == nil, let argv = try? split(s) else { return "" }
        let example = [
            "project_name": "my-project",
            "project_path": "/Users/you/code/my-project",
            "workspace_name": "my-task",
            "workspace_path": "/Users/you/.local/share/clowder/worktrees/my-project-ab12cd34ef56/my-task",
            "branch": "clowder/my-task",
        ]
        return argv
            .map { arg -> String in
                var out = arg
                for (t, v) in example { out = out.replacingOccurrences(of: "{{\(t)}}", with: v) }
                return out.contains(where: { $0.isWhitespace }) ? "'\(out)'" : out
            }
            .joined(separator: " ")
    }
}
```

- [ ] **Step 4: Implement `AgentProfileDraft`**

Append to `macos/Sources/ClowderCore/SheetForms.swift`:

```swift
/// The Agents pane's editor state.
///
/// `idError` mirrors `clowder_config::agents::validate_id`, which delegates to the host-name rule —
/// so it is checked against the same `docs/protocol/fixtures/host-names.json`. `argsError` mirrors
/// `split_args` + `validate_template` via `AgentArgs`, pinned to `agent-args.json`. The daemon
/// remains the authority.
public struct AgentProfileDraft: Equatable, Sendable {
    /// Immutable once created: the id is recorded on every agent spawned from this profile.
    public var id: String
    public var base: String
    public var displayName: String
    public var enabled: Bool
    public var args: String
    /// True when this draft creates a profile rather than editing one.
    public var isNew: Bool

    public init(id: String = "", base: String = "claude", displayName: String = "",
                enabled: Bool = true, args: String = "", isNew: Bool = true) {
        self.id = id
        self.base = base
        self.displayName = displayName
        self.enabled = enabled
        self.args = args
        self.isNew = isNew
    }

    private static let maxDisplayName = 64

    /// Nil when acceptable. Same rule as a host name — see `HostDraft.nameError`.
    public var idError: String? {
        var host = HostDraft()
        host.name = id
        return host.nameError
    }

    public var displayNameError: String? {
        if displayName.trimmingCharacters(in: .whitespaces).isEmpty { return "Name must not be empty" }
        if displayName.unicodeScalars.count > Self.maxDisplayName {
            return "Name must be \(Self.maxDisplayName) characters or fewer"
        }
        return nil
    }

    public var argsError: String? { AgentArgs.templateError(args) }

    public var isValid: Bool { idError == nil && displayNameError == nil && argsError == nil }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd macos && swift test --filter 'AgentArgsTests|AgentProfileDraftTests'`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/AgentArgs.swift macos/Sources/ClowderCore/SheetForms.swift macos/Tests/ClowderCoreTests
git commit -m "feat(app): mirror the agent argument rules in Swift"
```

---

### Task 13: `AgentsViewModel`

**Files:**
- Create: `macos/Sources/ClowderCore/AgentsViewModel.swift`
- Create: `macos/Tests/ClowderCoreTests/AgentsViewModelTests.swift`

**Interfaces:**
- Consumes: `AgentProfileInfo`, `AgentProfileDraft`, `ControlRequest`, `AgentArgs`.
- Produces:
  - `@MainActor public final class AgentsViewModel: ObservableObject`
  - published: `profiles: [AgentProfileInfo]`, `selected: String?`, `draft: AgentProfileDraft?`, `lastError: String?`
  - `init(send: @escaping (ControlRequest) throws -> Void)`
  - `apply(profiles:)`, `reload()`, `select(_:)`, `beginAdd()`, `duplicateSelected()`, `save()`,
    `revert()`, `remove(_:)`, `dismissError()`, `isDirty`, `canRemoveSelection`, `selectedProfile`,
    `preview`

`send` is injected so tests drive it with a recorder; the app passes `AppModel`'s control session.
The daemon is the source of truth — `save()` sends a request and the resulting broadcast lands via
`apply(profiles:)`, so nothing is optimistically written into `profiles`.

- [ ] **Step 1: Write the failing tests**

Create `macos/Tests/ClowderCoreTests/AgentsViewModelTests.swift`:

```swift
import XCTest
@testable import ClowderCore

@MainActor
final class AgentsViewModelTests: XCTestCase {
    private final class Recorder {
        var sent: [ControlRequest] = []
        var failNext = false
    }

    private func model() -> (AgentsViewModel, Recorder) {
        let rec = Recorder()
        let vm = AgentsViewModel(send: { req in
            if rec.failNext { rec.failNext = false; throw NSError(domain: "test", code: 1) }
            rec.sent.append(req)
        })
        vm.apply(profiles: [
            AgentProfileInfo(id: "claude", base: "claude", displayName: "Claude Code",
                             enabled: true, args: "", builtin: true),
            AgentProfileInfo(id: "codex", base: "codex", displayName: "OpenAI Codex",
                             enabled: true, args: "", builtin: true),
            AgentProfileInfo(id: "opus", base: "claude", displayName: "Claude (Opus)",
                             enabled: true, args: "--model opus", builtin: false),
        ])
        return (vm, rec)
    }

    func testReloadAsksTheDaemon() {
        let (vm, rec) = model()
        vm.reload()
        XCTAssertEqual(rec.sent, [.listAgentProfiles])
    }

    func testSelectFillsTheDraftFromTheProfile() {
        let (vm, _) = model()
        vm.select("opus")
        XCTAssertEqual(vm.draft?.id, "opus")
        XCTAssertEqual(vm.draft?.args, "--model opus")
        XCTAssertEqual(vm.draft?.isNew, false)
        XCTAssertFalse(vm.isDirty, "a freshly selected draft is not dirty")
    }

    func testSaveAnEditSendsAnUpdate() {
        let (vm, rec) = model()
        vm.select("opus")
        vm.draft?.args = "--model opus --verbose"
        XCTAssertTrue(vm.isDirty)
        vm.save()
        guard case let .updateAgentProfile(p)? = rec.sent.last else {
            return XCTFail("expected updateAgentProfile, got \(rec.sent)")
        }
        XCTAssertEqual(p.id, "opus")
        XCTAssertEqual(p.args, "--model opus --verbose")
    }

    func testSaveANewProfileSendsAnAdd() {
        let (vm, rec) = model()
        vm.beginAdd()
        vm.draft?.id = "plan"
        vm.draft?.displayName = "Planner"
        vm.draft?.base = "claude"
        vm.save()
        guard case let .addAgentProfile(p)? = rec.sent.last else {
            return XCTFail("expected addAgentProfile, got \(rec.sent)")
        }
        XCTAssertEqual(p.id, "plan")
        XCTAssertFalse(p.builtin)
    }

    func testSaveRefusesAnInvalidDraftAndExplainsWhy() {
        let (vm, rec) = model()
        vm.select("opus")
        vm.draft?.args = "--x {{nope}}"
        vm.save()
        XCTAssertTrue(rec.sent.isEmpty, "nothing may be sent for an invalid draft")
        XCTAssertTrue(vm.lastError?.contains("nope") == true, "unhelpful: \(vm.lastError ?? "nil")")
        XCTAssertEqual(vm.draft?.args, "--x {{nope}}", "a refused save must not disturb what was typed")
    }

    func testDuplicateProducesAnEditableCopyWithAFreshId() {
        let (vm, rec) = model()
        vm.select("opus")
        vm.duplicateSelected()
        XCTAssertEqual(vm.draft?.isNew, true)
        XCTAssertEqual(vm.draft?.base, "claude")
        XCTAssertEqual(vm.draft?.args, "--model opus")
        XCTAssertNotEqual(vm.draft?.id, "opus", "a duplicate needs its own id")
        XCTAssertNil(vm.draft?.idError, "the suggested id must be valid as-is: \(vm.draft?.id ?? "")")
        XCTAssertTrue(rec.sent.isEmpty, "duplicate is local until saved")
    }

    func testDuplicatingABuiltinSavesAsANewNonBuiltinProfile() {
        let (vm, rec) = model()
        vm.select("claude")
        vm.duplicateSelected()
        vm.save()
        guard case let .addAgentProfile(p)? = rec.sent.last else {
            return XCTFail("duplicating a builtin must ADD, never update it: \(rec.sent)")
        }
        XCTAssertEqual(p.id, "claude-copy")
        XCTAssertEqual(p.base, "claude")
        XCTAssertFalse(p.builtin)
    }

    func testRemoveSendsARemoveAndIsRefusedForBuiltins() {
        let (vm, rec) = model()
        vm.remove("opus")
        XCTAssertEqual(rec.sent.last, .removeAgentProfile(id: "opus"))

        rec.sent.removeAll()
        vm.remove("claude")
        XCTAssertTrue(rec.sent.isEmpty, "a builtin removal must not reach the daemon")
        XCTAssertTrue(vm.lastError?.contains("built-in") == true, "unhelpful: \(vm.lastError ?? "nil")")
    }

    func testCanRemoveSelectionIsFalseForBuiltinsAndNoSelection() {
        let (vm, _) = model()
        XCTAssertFalse(vm.canRemoveSelection)
        vm.select("claude")
        XCTAssertFalse(vm.canRemoveSelection)
        vm.select("opus")
        XCTAssertTrue(vm.canRemoveSelection)
    }

    func testRevertRestoresTheStoredValues() {
        let (vm, _) = model()
        vm.select("opus")
        vm.draft?.displayName = "changed"
        XCTAssertTrue(vm.isDirty)
        vm.revert()
        XCTAssertEqual(vm.draft?.displayName, "Claude (Opus)")
        XCTAssertFalse(vm.isDirty)
    }

    func testApplyProfilesKeepsTheSelectionAndClearsADirtyDraftOnlyIfGone() {
        let (vm, _) = model()
        vm.select("opus")
        vm.draft?.displayName = "changed"
        // A broadcast caused by someone else's edit must not silently discard what the user typed.
        vm.apply(profiles: vm.profiles)
        XCTAssertEqual(vm.draft?.displayName, "changed")
        XCTAssertEqual(vm.selected, "opus")

        // ...but a profile that has gone away cannot stay selected.
        vm.apply(profiles: vm.profiles.filter { $0.id != "opus" })
        XCTAssertNil(vm.selected)
        XCTAssertNil(vm.draft)
    }

    func testASendFailureSurfacesAsAnError() {
        let (vm, rec) = model()
        rec.failNext = true
        vm.reload()
        XCTAssertNotNil(vm.lastError)
        vm.dismissError()
        XCTAssertNil(vm.lastError)
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd macos && swift test --filter AgentsViewModelTests`
Expected: FAIL to compile — `cannot find 'AgentsViewModel' in scope`.

- [ ] **Step 3: Implement the view model**

Create `macos/Sources/ClowderCore/AgentsViewModel.swift`:

```swift
import Foundation
import Combine

/// All state and operations behind the Settings window's Agents pane.
///
/// Lives in `ClowderCore` because `ClowderApp` has no test target — every decision here is driven in
/// `swift test` by a recording `send`. The views render this and nothing else.
///
/// The daemon is the source of truth: `save`/`remove` send a request and the resulting
/// `agentProfileList` broadcast arrives via `apply(profiles:)`. Nothing is written optimistically,
/// so the list can never show a profile the daemon refused.
@MainActor
public final class AgentsViewModel: ObservableObject {
    @Published public private(set) var profiles: [AgentProfileInfo] = []
    @Published public private(set) var selected: String?
    /// The editor's live state. Nil when nothing is selected.
    @Published public var draft: AgentProfileDraft?
    @Published public private(set) var lastError: String?

    private let send: (ControlRequest) throws -> Void

    public init(send: @escaping (ControlRequest) throws -> Void) {
        self.send = send
    }

    public var selectedProfile: AgentProfileInfo? {
        selected.flatMap { id in profiles.first { $0.id == id } }
    }

    /// Built-ins can be edited and disabled but never removed — the daemon refuses it too.
    public var canRemoveSelection: Bool { selectedProfile.map { !$0.builtin } ?? false }

    /// Whether `draft` differs from what the daemon holds, so Save/Revert only light up when there
    /// is something to save or discard.
    public var isDirty: Bool {
        guard let draft else { return false }
        guard !draft.isNew else { return draft != AgentProfileDraft() }
        guard let p = selectedProfile else { return true }
        return draft.displayName != p.displayName || draft.enabled != p.enabled
            || draft.args != p.args || draft.base != p.base
    }

    /// The editor's live preview of the resolved arguments.
    public var preview: String { draft.map { AgentArgs.preview($0.args) } ?? "" }

    public func dismissError() { lastError = nil }

    /// Adopt a list from the daemon. Keeps the current selection and any in-progress edit, unless
    /// the selected profile has gone away.
    public func apply(profiles: [AgentProfileInfo]) {
        self.profiles = profiles
        guard let selected else { return }
        if !profiles.contains(where: { $0.id == selected }) {
            self.selected = nil
            draft = nil
        }
    }

    public func reload() { dispatch(.listAgentProfiles) }

    public func select(_ id: String?) {
        selected = id
        guard let p = id.flatMap({ i in profiles.first { $0.id == i } }) else {
            draft = nil
            return
        }
        draft = AgentProfileDraft(id: p.id, base: p.base, displayName: p.displayName,
                                  enabled: p.enabled, args: p.args, isNew: false)
    }

    public func beginAdd() {
        selected = nil
        draft = AgentProfileDraft()
    }

    /// A local, unsaved copy of the selection under a fresh id — how a user makes "Claude (Opus)"
    /// from "Claude Code" without inventing a program.
    public func duplicateSelected() {
        guard let p = selectedProfile else { return }
        selected = nil
        draft = AgentProfileDraft(id: freshID(basedOn: p.id), base: p.base,
                                  displayName: "\(p.displayName) copy", enabled: p.enabled,
                                  args: p.args, isNew: true)
    }

    /// Restore the editor to what the daemon holds.
    public func revert() { select(selected) }

    public func save() {
        guard let draft else { return }
        guard draft.isValid else {
            lastError = draft.idError ?? draft.displayNameError ?? draft.argsError
            return
        }
        let wire = AgentProfileInfo(id: draft.id, base: draft.base, displayName: draft.displayName,
                                    enabled: draft.enabled, args: draft.args, builtin: false)
        dispatch(draft.isNew ? .addAgentProfile(wire) : .updateAgentProfile(wire))
    }

    public func remove(_ id: String) {
        if profiles.first(where: { $0.id == id })?.builtin == true {
            lastError = "\(id) is a built-in agent and cannot be removed — disable it instead."
            return
        }
        dispatch(.removeAgentProfile(id: id))
    }

    private func dispatch(_ req: ControlRequest) {
        do { try send(req) } catch { lastError = "Could not reach the daemon: \(error.localizedDescription)" }
    }

    /// `<id>-copy`, then `-copy2`, `-copy3`… — always a valid id, never a collision.
    private func freshID(basedOn id: String) -> String {
        let taken = Set(profiles.map(\.id))
        var candidate = "\(id)-copy"
        var n = 2
        while taken.contains(candidate) {
            candidate = "\(id)-copy\(n)"
            n += 1
        }
        return candidate
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd macos && swift test --filter AgentsViewModelTests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/ClowderCore/AgentsViewModel.swift macos/Tests/ClowderCoreTests/AgentsViewModelTests.swift
git commit -m "feat(app): add the agents settings view model"
```

---

### Task 14: The Agents tab

**Files:**
- Create: `macos/Sources/ClowderApp/AgentsSettingsView.swift`
- Create: `macos/Sources/ClowderApp/AgentEditorView.swift`
- Modify: `macos/Sources/ClowderApp/SettingsView.swift`
- Modify: `macos/Sources/ClowderApp/App.swift` (`bootstrap()` ~line 36-55, `Settings` scene ~line 315)
- Modify: `AGENTS.md` (the `macos/` row of the Repo layout table)

**Interfaces:**
- Consumes: `AgentsViewModel`, `AgentProfileDraft`, `AgentStore.agentProfiles`.
- Produces: no testable API — these views render the view model and nothing else.

- [ ] **Step 1: Write the list view**

Create `macos/Sources/ClowderApp/AgentsSettingsView.swift`:

```swift
import SwiftUI
import ClowderCore

/// Master/detail over the agent profiles: the list on the left, the editor on the right.
struct AgentsSettingsView: View {
    @ObservedObject var model: AgentsViewModel

    var body: some View {
        HSplitView {
            VStack(spacing: 0) {
                List(selection: Binding(
                    get: { model.selected },
                    set: { model.select($0) }
                )) {
                    ForEach(model.profiles) { p in
                        HStack(spacing: 6) {
                            Image(systemName: p.enabled ? "checkmark.circle.fill" : "circle")
                                .foregroundStyle(p.enabled ? .green : .secondary)
                                .help(p.enabled ? "Shown in New Worktree" : "Hidden from New Worktree")
                            VStack(alignment: .leading, spacing: 1) {
                                Text(p.displayName)
                                Text(p.args.isEmpty ? p.base : "\(p.base) \(p.args)")
                                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }
                            Spacer()
                            if p.builtin {
                                Text("built-in").font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                        .tag(p.id)
                    }
                }

                Divider()
                HStack(spacing: 4) {
                    Button { model.beginAdd() } label: { Image(systemName: "plus") }
                        .help("Add an agent")
                    Button {
                        if let id = model.selected { model.remove(id) }
                    } label: { Image(systemName: "minus") }
                        .disabled(!model.canRemoveSelection)
                        .help("Remove the selected agent (built-ins can only be disabled)")
                    Button { model.duplicateSelected() } label: { Image(systemName: "plus.square.on.square") }
                        .disabled(model.selectedProfile == nil)
                        .help("Duplicate the selected agent")
                    Spacer()
                }
                .buttonStyle(.borderless)
                .padding(6)
            }
            .frame(minWidth: 220)

            Group {
                if model.draft != nil {
                    AgentEditorView(model: model)
                } else {
                    Text("Select an agent, or press + to add one.")
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .frame(minWidth: 420)
        }
        .onAppear { model.reload() }
        .alert("Agents", isPresented: Binding(
            get: { model.lastError != nil },
            set: { if !$0 { model.dismissError() } }
        )) {
            Button("OK") { model.dismissError() }
        } message: {
            Text(model.lastError ?? "")
        }
    }
}
```

- [ ] **Step 2: Write the editor view**

Create `macos/Sources/ClowderApp/AgentEditorView.swift`:

```swift
import SwiftUI
import ClowderCore

/// The per-agent form. Renders `model.draft`; every decision lives in `AgentsViewModel`.
struct AgentEditorView: View {
    @ObservedObject var model: AgentsViewModel

    private let bases = ["claude", "codex", "shell"]

    var body: some View {
        if let draft = model.draft {
            VStack(alignment: .leading, spacing: 12) {
                Form {
                    TextField("Name", text: Binding(
                        get: { model.draft?.displayName ?? "" },
                        set: { model.draft?.displayName = $0 }))

                    if draft.isNew {
                        TextField("Id", text: Binding(
                            get: { model.draft?.id ?? "" },
                            set: { model.draft?.id = $0 }))
                        .help("Used by `clowder spawn <project> <name> <id>`. Cannot be changed later.")
                        Picker("Agent", selection: Binding(
                            get: { model.draft?.base ?? "claude" },
                            set: { model.draft?.base = $0 })) {
                            ForEach(bases, id: \.self) { Text($0).tag($0) }
                        }
                    } else {
                        LabeledContent("Id", value: draft.id)
                        LabeledContent("Agent", value: draft.base)
                    }

                    Toggle("Show in New Worktree", isOn: Binding(
                        get: { model.draft?.enabled ?? false },
                        set: { model.draft?.enabled = $0 }))

                    TextField("Arguments", text: Binding(
                        get: { model.draft?.args ?? "" },
                        set: { model.draft?.args = $0 }))
                    .font(.system(.body, design: .monospaced))
                }

                if let err = draft.idError, draft.isNew, !draft.id.isEmpty {
                    Text(err).font(.caption).foregroundStyle(.red)
                }
                if let err = draft.displayNameError, !draft.displayName.isEmpty {
                    Text(err).font(.caption).foregroundStyle(.red)
                }
                if let err = draft.argsError {
                    Text(err).font(.caption).foregroundStyle(.red)
                } else if !model.preview.isEmpty {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Resolved").font(.caption).foregroundStyle(.secondary)
                        Text(model.preview)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                    }
                }

                Text("Arguments are appended to the agent's own. Tokens: "
                     + AgentArgs.tokens.map { "{{\($0)}}" }.joined(separator: ", "))
                    .font(.caption).foregroundStyle(.secondary)

                Spacer()
                HStack {
                    Button("Revert") { model.revert() }.disabled(!model.isDirty)
                    Spacer()
                    Button("Save") { model.save() }
                        .keyboardShortcut(.defaultAction)
                        .disabled(!model.isDirty || !draft.isValid)
                }
            }
            .padding(20)
        }
    }
}
```

- [ ] **Step 3: Add the tab and build the view model**

In `macos/Sources/ClowderApp/SettingsView.swift`:

```swift
struct SettingsView: View {
    let hosts: HostsViewModel?
    let agents: AgentsViewModel?

    var body: some View {
        TabView {
            Group {
                if let hosts {
                    HostsSettingsView(model: hosts)
                } else {
                    Text("Host management is unavailable in this build.")
                        .foregroundStyle(.secondary)
                }
            }
            .tabItem { Label("Hosts", systemImage: "network") }

            Group {
                if let agents {
                    AgentsSettingsView(model: agents)
                } else {
                    Text("Agent settings need a running daemon.")
                        .foregroundStyle(.secondary)
                }
            }
            .tabItem { Label("Agents", systemImage: "cpu") }
        }
    }
}
```

`AppModel.session` is private (`AppModel.swift:116`), so add a sender beside the other
`session?.send` callers (`AppModel.swift:434`) — this is also the one place a send failure becomes
visible to the pane:

```swift
    public enum ControlSendError: Error { case notConnected }

    /// Send one control request, failing loudly when there is no connection — unlike the
    /// fire-and-forget `try? session?.send(...)` callers, the Settings pane must be able to tell
    /// the user that nothing was saved.
    public func sendControl(_ req: ControlRequest) throws {
        guard let session else { throw ControlSendError.notConnected }
        try session.send(req)
    }
```

In the same file, add `.listAgentProfiles` beside the existing `.listAdapters` send at
`AppModel.swift:204`, so the pane is populated before Settings is ever opened:

```swift
        try session.send(.listAgentProfiles)
```

In `macos/Sources/ClowderApp/App.swift`, add the two stored properties beside `hostsModel`:

```swift
    /// Backs the Settings window's Agents pane. Built in `bootstrap()` once `appModel` exists,
    /// since it sends through that model's control session.
    private(set) var agentsModel: AgentsViewModel?
    /// Keeps the store → pane subscription alive for the app's lifetime.
    private var agentProfilesSubscription: AnyCancellable?
```

and, in `bootstrap()` **after** `appModel` is created and assigned:

```swift
        let agents = AgentsViewModel(send: { [weak self] req in
            guard let model = self?.appModel else { throw AppModel.ControlSendError.notConnected }
            try model.sendControl(req)
        })
        // The daemon broadcasts the full list after every mutation — including one made from
        // another client, or from `clowder agent` in a terminal — so the pane follows the daemon
        // rather than its own last write. Also covers a backend switch: `reset()` empties the
        // store, and the new connection's list arrives the same way.
        agentProfilesSubscription = appModel.store.$agentProfiles
            .receive(on: DispatchQueue.main)
            .sink { [weak agents] profiles in agents?.apply(profiles: profiles) }
        agentsModel = agents
```

Add `import Combine` to `App.swift` if it is not already there. Widen `bootstrap()`'s return type to
`(appModel: AppModel, surfaceHost: SurfaceHost, hostsModel: HostsViewModel?, agentsModel: AgentsViewModel?)`
— including its early-return line (`App.swift:37`) — and update the `Settings` scene
(`App.swift:315`):

```swift
        Settings {
            let b = delegate.bootstrap()
            SettingsView(hosts: b.hostsModel, agents: b.agentsModel)
        }
```

The compiler lists the other `bootstrap()` call sites; each destructures a tuple, so add the fourth
element where needed.

- [ ] **Step 4: Handle "every agent disabled" in the New Worktree sheet**

Disabling every profile is allowed, and it empties the picker. Today `NewWorktreeSheet` would fall
back to `"claude"` (`NewWorktreeSheet.swift:45`) and spawn would fail with a "disabled" error from
the daemon — a confusing way to learn what happened. In
`macos/Sources/ClowderApp/NewWorktreeSheet.swift`, replace the `Picker("Agent", …)` with:

```swift
                if adapters.isEmpty {
                    Text("No agents are enabled — turn one on in Settings → Agents.")
                        .font(.caption).foregroundStyle(.secondary)
                } else {
                    Picker("Agent", selection: $form.adapter) {
                        ForEach(adapters) { a in Text(a.displayName).tag(a.id) }
                    }
                }
```

and disable Create in that case by changing the button's modifier to:

```swift
                .disabled(!form.isValid || adapters.isEmpty)
```

- [ ] **Step 5: Verify it compiles and the suite is green**

Run: `cd macos && swift test`
Expected: PASS — every `ClowderCoreTests` test, with `ClowderApp` compiling cleanly. (A compile error
in the new views aborts the run before any test executes; that is the signal to fix them.)

- [ ] **Step 6: Update `AGENTS.md`**

In the Repo layout table's `macos/` row, extend the Settings sentence:

```markdown
The Settings window (⌘,) has two panes: `SettingsView` → `HostsSettingsView` (list + editor) →
`HostEditorView` → `PairingSheet`, and `SettingsView` → `AgentsSettingsView` → `AgentEditorView`.
All of them render only — every decision (validation, add/edit/remove/pair, argument parsing) lives
in `ClowderCore`'s `HostsViewModel` / `AgentsViewModel` / `AgentArgs`, since `clowder-app` has no
test target.
```

- [ ] **Step 7: Run the app end to end**

```bash
scripts/build-app.sh
open dist/Clowder.app
```

In the app: ⌘, → **Agents**. Add a profile — Name "Claude (Opus)", Id `opus`, Agent `claude`,
Arguments `--model opus --append-system-prompt "working on {{workspace_name}}"`. Confirm the
Resolved preview shows `'working on my-task'` as one quoted element, Save, then open New Worktree
and confirm "Claude (Opus)" is in the picker. Spawn it, and check the argv:

```bash
ps -Ao args | grep -- "--model opus"
```

Expected: one `claude --model opus --append-system-prompt working on my-task` process, with the
prompt as a single argument. Then disable Codex in Settings and confirm it leaves the New Worktree
picker without reopening the sheet.

- [ ] **Step 8: Commit and open the PR**

```bash
git add macos AGENTS.md
git commit -m "feat(app): add the Agents settings pane"
gh pr create --base feat/m12c-agent-cli --title "feat(app): agent settings pane (M12d)" \
  --body "Last of four stacked PRs for #80. A Settings > Agents tab over the daemon's profiles: enable/disable, arguments with a live resolved preview, duplicate, and add/remove of user profiles. All logic in ClowderCore's AgentsViewModel/AgentArgs; the views render only.

Closes #80.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_01QZBTSP2UQiEkGK5j8zcUKN"
```

---

## Final verification

Run all of these from the repo root on the tip of the stack before merging:

1. `source "$HOME/.cargo/env" && cargo test --workspace --locked` — must pass. (Three
   `clowder-daemon` timing tests are known to flake under parallel load; re-run once before
   investigating a failure.)
2. `cd macos && swift test` — must pass, ~188 existing tests plus the new ones.
3. `scripts/check-commit-messages.sh` — every commit on every branch of the stack.
4. **Live restart check** — with the app running and a profile-spawned agent alive:
   ```bash
   pkill -f clowder-daemon      # the app relaunches and reconciles
   ps -Ao args | grep -- "--continue"
   ```
   Expected: the agent comes back with `claude --continue --model opus …` — the recorded arguments,
   replayed. Then delete the `opus` profile in Settings, restart the daemon again, and confirm the
   agent still resumes with the same arguments.
5. **Disabled spawn** — `./target/debug/clowder agent disable codex` then
   `./target/debug/clowder spawn <project> t codex`: must fail with the "disabled" message, not spawn.
6. **Remote check** — connect the app to a remote host, open Settings → Agents, and confirm it shows
   *that host's* profiles; `CLOWDER_CONTROL_SOCK=<runtime>/clowder/remote/<host>/clowder-control.sock
   ./target/debug/clowder agent list` must agree with the GUI.
