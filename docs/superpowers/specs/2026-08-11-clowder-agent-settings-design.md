# clowder M12 — agent settings (profiles, args, tokens)

Issue: [#80 Agent Settings](https://github.com/richardcase/clowder/issues/80)

## Context

The set of spawnable agents is a compile-time constant. `adapter_descriptors()` returns a fixed
`{claude, codex, shell}`, each adapter hardcodes its own launch arguments, and the app's New Worktree
picker renders whatever `ListAdapters` returns. A user who wants `claude --model opus`, or a second
Claude entry pinned to a different mode, or who never uses Codex and wants it out of the picker, has
no way to say so.

M12 adds that: **a daemon-owned set of agent profiles — a named, enable-able wrapper around one of
the built-in adapters, carrying an argument template with runtime-substituted tokens — editable from
a new Settings tab and from a `clowder agent` CLI, and used as the single source of the
spawnable-agent list.**

### What exists (ground truth, verified 2026-08-11 at `aba0bbc`)

- **Adapters** (`crates/clowder-daemon/src/agent.rs`): the `AgentAdapter` trait
  (`id`, `provision_hooks`, `launch_command`, `resume_command`, `provides_hooks`);
  `ClaudeAdapter` (no launch args, `--continue` on resume), `CodexAdapter`
  (`-c notify=["<clowder-hook>","--event","stop"]` — this arg **is** its attention wiring),
  `SyntheticAdapter` (used for `shell`, self-reports id `"synthetic"`).
  `adapter_descriptors()` is the id+label list; `build_adapter(id)` constructs one, accepting both
  `"shell"` and `"synthetic"` so a shell agent round-trips through the registry.
- **Spawn** (`server.rs:516` `spawn_agent`): canonicalize project → check it is registered →
  `validate_workspace_name` → collision checks → provision worktree → `provision_hooks` →
  `launch_command` (+ `cwd`, `CLOWDER_AGENT_ID`, `CLOWDER_HOOK_SOCK`) → `Pane::spawn` → registry
  `upsert` → `finalize_agent`. A failure after provisioning discards the worktree.
- **Registry** (`registry.rs`): `AgentRecord { agent_id, project, task, adapter_id, worktree_path,
  branch, workspace_kind, cols, rows, tree }` in `agents.json`; `reconcile` (`server.rs:411`)
  rebuilds each agent from its record with `build_adapter(&rec.adapter_id)` +
  `adapter.resume_command(&ws.path)`.
- **Control protocol** (`clowder-proto/src/control.rs`): `ListAdapters` → `AdapterList { adapters:
  Vec<AdapterInfo> }`, `SpawnAgent { project, name, adapter }`, and the project trio
  (`ListProjects` / `AddProject` / `RemoveProject` → `ProjectList` / `ProjectAdded` /
  `ProjectRemoved`).
- **Daemon-side stores**: `JsonStore<T>` (`store.rs`) — one JSON file, atomic temp+rename, corrupt
  file loads empty, `mutate` / `try_mutate` / `mutate_if` under a write lock. `ProjectStore`
  (`projects.rs`) is the policy-free example, path
  `$CLOWDER_PROJECTS_FILE › $XDG_STATE_HOME/clowder/projects.json › ~/.local/state/…`.
- **Validation precedent**: `clowder_config::hosts::validate_name` (`[A-Za-z0-9._-]{1,64}`, not
  `.`/`..`), mirrored in Swift by `HostDraft.nameError` and pinned against the shared fixture
  `docs/protocol/fixtures/host-names.json` so the two cannot drift.
- **CLI** (`clowder-client/src/main.rs`): `clowder project add|list|rm` calls
  `*_via_control(&sock, …)` helpers in `clowder-client/src/lib.rs` against
  `Config::load().control_sock`. `clowder remote …` (`remote_cli.rs`) is the larger CLI precedent.
- **App**: `ControlSession` decodes events into `AgentStore`; `AppModel` sends `listAdapters` at
  connect and exposes `adapters`; `NewWorktreeSheet` renders that list. `SettingsView` is a `TabView`
  with a single Hosts tab; `HostsViewModel` (in `ClowderCore`) owns every decision because
  `clowder-app` has no test target.

## Decisions

| Question | Decision |
|---|---|
| What is configurable | The built-in adapters **plus user-created profiles** — clones of a built-in differing in display name and args. Users cannot introduce a new program. |
| Where profiles live | **Daemon-side**, `agent-profiles.json` in the state dir, edited over the control socket, so the app, `clowder spawn` and remote hosts all agree. Each host has its own profiles; agent binaries and flags are machine-specific. |
| Arg semantics | Profile args are **appended** to the adapter's own args. Hook wiring (`codex -c notify=…`) and `claude --continue` can never be lost. |
| Arg entry | One text field, **shell-style quoting**, split by our own parser. No shell, no globbing, no `$VAR`, no operators. |
| Built-ins | Renameable, disable-able, arg-able — **not deletable**. User profiles are deletable. |
| Tokens | `{{project_name}}`, `{{project_path}}`, `{{workspace_name}}`, `{{workspace_path}}`, `{{branch}}`. An unknown token is a **save-time error**, never a literal passed to the agent. |
| CLI | Full `clowder agent list|add|set|enable|disable|rm`, mirroring the `clowder remote` precedent. |

**Non-goals:** per-project overrides, environment-variable expansion, replacing (rather than
extending) an adapter's own args, agent kinds beyond the three built-ins, per-profile environment
variables.

## Design

### 1. `clowder-config::agents` — the pure core

A new module beside `hosts.rs`, depended on by the daemon and the CLI. Everything here is pure and
table-tested; nothing touches the filesystem.

```rust
pub struct AgentProfile {
    pub id: String,            // stable, spawnable, recorded with each agent
    pub base: String,          // "claude" | "codex" | "shell"
    pub display_name: String,
    pub enabled: bool,
    pub args: String,          // the template, exactly as typed
}

pub struct TokenContext<'a> {  // what the daemon knows once the worktree exists
    pub project_path: &'a Path,
    pub workspace_path: &'a Path,
    pub workspace_name: &'a str,
    pub branch: &'a str,
}
```

- `validate_id(&str)` — delegates to `hosts::validate_name`'s rule rather than adding a third copy of
  the charset check. (Ids are not path segments here, but a narrow, already-mirrored rule is worth
  more than a bespoke one.)
- `split_args(&str) -> Result<Vec<String>, String>` — quote-aware split: `'…'` literal; `"…"` with
  `\` escapes; `\` escapes outside quotes; everything else literal, so `|`, `>`, `&&` are ordinary
  characters, not operators. An unterminated quote is an error carrying a user-facing message.
- `validate_template(&str) -> Result<(), String>` — splits, then rejects any `{{…}}` that does not
  name a known token, naming the offender and listing the valid set.
- `substitute(args: &[String], ctx: &TokenContext) -> Vec<String>` — replaces tokens **per
  already-split argument**.

**Substitution happens after splitting, never before.** A project path containing a space stays one
argv element, and no token value can inject extra arguments — the property that makes it safe to feed
paths into an argument list at all.

Shared fixture `docs/protocol/fixtures/agent-args.json`: cases of `{ input, argv }` and
`{ input, error }`, read by both the Rust tests and the Swift editor tests, following the
`host-names.json` precedent.

### 2. Daemon: the profile store

`crates/clowder-daemon/src/agent_profiles.rs` — a `JsonStore<AgentProfileRow>`, shaped like
`ProjectStore`: policy-free, atomic writes, corrupt file loads empty, `try_mutate` for anything
answering a user request.

- Path: `$CLOWDER_AGENT_PROFILES_FILE › $XDG_STATE_HOME/clowder/agent-profiles.json ›
  ~/.local/state/clowder/agent-profiles.json`. **Not** `agents.json` — that name is already the
  live-agent registry.
- The file holds **only deltas**: an override row for each built-in the user has touched, plus one
  row per user-created profile. The *effective* list is `adapter_descriptors()` (defaults), each
  merged with its override when present, in descriptor order, followed by the user rows in insertion
  order.
  - A built-in added in a future release appears automatically instead of being masked by a stale
    saved list.
  - "Reset to default" is just deleting the override row.
  - A hand-edited file naming an unknown built-in, or a user row whose `base` is unknown, is dropped
    from the effective list with a warning rather than wedging the daemon.
- `resolve(id) -> Result<ResolvedProfile>` yields the base adapter (via the existing `build_adapter`)
  plus the split-but-not-yet-substituted arg template. An unknown id and a disabled id produce
  distinct, user-facing errors.

### 3. Spawn and resume

`Daemon::spawn_agent` currently takes `&dyn AgentAdapter`. That parameter becomes a small value:

```rust
pub struct SpawnSpec<'a> {
    pub adapter: &'a dyn AgentAdapter,
    pub profile_id: Option<String>,
    pub arg_template: Vec<String>,   // split, not yet substituted
}
impl<'a> SpawnSpec<'a> { pub fn adapter_only(adapter: &'a dyn AgentAdapter) -> Self { … } }
```

Substitution happens **inside** `spawn_agent`, after the worktree is provisioned — that is the first
moment `workspace_path` and `branch` exist. Final argv = `adapter.launch_command().args ++
substitute(arg_template, ctx)`.

`AgentRecord` gains two `#[serde(default)]` fields: `profile_id: Option<String>` and
`extra_args: Vec<String>` holding the **already-substituted** tail. `reconcile` replays
`adapter.resume_command().args ++ rec.extra_args`. Consequences, all deliberate:

- Editing, renaming or deleting a profile never breaks a running agent's resume.
- Tokens are not re-substituted on resume, so a resumed agent gets byte-identical arguments.
- `adapter_id` keeps its current meaning — the base adapter's self-reported id — so pre-M12 records
  and the existing reconcile path are untouched.

### 4. Control protocol

In `clowder-proto/src/control.rs`:

- `AgentProfileInfo { id, base, displayName, enabled, args, builtin }` — `builtin` so a client can
  disable Remove without duplicating the built-in list.
- Requests: `ListAgentProfiles`, `AddAgentProfile { profile }`, `UpdateAgentProfile { profile }`,
  `RemoveAgentProfile { id }`. Add and update are explicit so a duplicate id and a missing id get
  distinct errors; remove refuses a built-in and points at disable.
- Event: `AgentProfileList { profiles }`, broadcast to every control client after any mutation.
- `ListAdapters` / `AdapterList` keep their wire shape but are now sourced from the **enabled**
  profiles, and `AdapterList` is re-broadcast alongside `AgentProfileList` on every mutation.
  `NewWorktreeSheet` needs no change — the picker becomes profile-driven for free, and updates live.
- `SpawnAgent { adapter }`'s string is now a profile id. `clowder spawn <project> <name> <profile>`
  is unchanged for the built-in ids, which remain `claude` / `codex` / `shell`.

The daemon validates every incoming profile itself. A remote client is untrusted, and a hand-edited
file is not validated on load.

### 5. `clowder agent` CLI

In `clowder-client`, with `*_via_control` helpers beside the existing project ones:

```
clowder agent list                        # id, base, enabled, display name, args
clowder agent add <id> --base <claude|codex|shell> [--name <s>] [--args "<template>"] [--disabled]
clowder agent set <id> [--name <s>] [--args "<template>"]
clowder agent enable <id> | disable <id>
clowder agent rm <id>                     # refuses built-ins, points at `disable`
```

Args are validated client-side for a fast, local error and again in the daemon, which is the
authority.

### 6. macOS Settings

`SettingsView` gains an **Agents** tab beside Hosts; the `TabView` was written to accept one.

Split as M11c established, because `clowder-app` has no test target:

- **`ClowderCore/AgentsViewModel.swift`** — list, selection, draft, dirty tracking,
  add/duplicate/save/revert/remove, error surfacing. Constructed with a
  `send: (ControlRequest) throws -> Void` and fed from `AgentStore`'s `agentProfileList` handling, so
  it follows the active backend (local or remote) with no extra plumbing and is driven in
  `swift test` by a fake sender. Save/Revert gate on *actual* dirtiness (the fix in `e94fc98` — do
  not regress it).
- **`ClowderCore/SheetForms.swift`** — `AgentProfileDraft` with `idError` / `argsError` mirroring the
  Rust validators against `agent-args.json`.
- **`ClowderApp/AgentsSettingsView.swift` + `AgentEditorView.swift`** — list + editor split mirroring
  `HostsSettingsView` / `HostEditorView`. Editor: Display name, Enabled, Base agent (fixed for
  built-ins, chosen at creation for a clone), Args, a token legend, and a live preview of the
  resolved argv against example values. Remove disabled for built-ins; Duplicate offered for all.

## Risks and rejected alternatives

1. **Replacing an adapter's args** — rejected. Dropping `codex -c notify=…` silently breaks attention
   routing, and resume would need a second field or lose `--continue`. Append-only keeps profiles
   valid across adapter changes.
2. **Environment expansion (`$VAR`) in args** — rejected. It blurs the "no shell" rule and makes a
   profile behave differently local vs remote.
3. **App-side profiles shared across hosts** — rejected. The daemon must substitute tokens (only it
   knows the worktree path), and a client-owned list would leave `clowder spawn` unable to name a
   profile. Cost accepted: a new remote host starts from defaults.
4. **Seeding defaults into the file** — rejected in favour of delta rows, so a stale file cannot mask
   a built-in added later and "reset" is a deletion.
5. **Re-resolving the profile at resume** — rejected. Snapshotting the substituted args means a
   deleted or edited profile cannot strand a running agent, and a resumed session keeps the exact
   arguments it started with.
6. **Profile id collisions with future built-ins** — a user profile named after a not-yet-existing
   built-in would become an override on upgrade. Accepted: the merge prefers the row's own `base`,
   so the entry keeps working; the only visible effect is that it is no longer deletable.
7. **Disabling every profile** — allowed; the New Worktree picker is then empty and Create is
   disabled, with a hint pointing at Settings.

## Decomposition

- **M12a — the pure core.** `clowder-config::agents` (types, `validate_id`, `split_args`,
  `validate_template`, `substitute`) + `docs/protocol/fixtures/agent-args.json`. No behavior change.
- **M12b — the daemon.** `agent_profiles.rs`, effective-list merge, control requests/events,
  `ListAdapters` from enabled profiles, `SpawnSpec`, the two `AgentRecord` fields, resume replay.
- **M12c — the CLI.** `clowder agent list|add|set|enable|disable|rm` + `*_via_control` helpers.
- **M12d — the app.** Agents settings tab, `AgentsViewModel`, `AgentProfileDraft`.

Each ships a usable increment: after M12b the profiles exist and drive the picker; after M12c they
are manageable headlessly; M12d makes it self-service.

## Verification gate

A user can add a profile "Claude (Opus)" with
`--model opus --append-system-prompt "working on {{workspace_name}}"` from the Settings window, see it
appear immediately in the New Worktree picker, spawn it, and find the substituted value in the
agent's argv as a **single** argument. Disabling Codex removes it from the picker live and makes
`clowder spawn … codex` fail with a "disabled" error. Restarting the daemon resumes a running
profile-spawned agent with `--continue` plus its recorded args, and still does so after the profile
has been deleted. A typo'd token is refused at save time in the editor, in the CLI, and in the
daemon. `clowder agent list` against a remote forwarder shows that host's profiles and agrees with
the GUI. Built-ins cannot be removed, only disabled.
