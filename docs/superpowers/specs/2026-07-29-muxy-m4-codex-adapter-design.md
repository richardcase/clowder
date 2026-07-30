# muxy M4 — Adapter Breadth: Codex + Discoverable Adapter Picker

## Context

muxy launches CLI coding agents through the `AgentAdapter` seam
(`crates/muxy-daemon/src/agent.rs`): `ClaudeAdapter` injects Claude Code hooks that
call `muxy-hook`, and hook-less agents fall back to the M2 VT-signal scanner
(BEL/OSC). Only Claude (and the test `SyntheticAdapter`/`shell`) exist today; the
client selects an adapter by a free-text name string that `control_json.rs` maps
(`"claude"` → real, else → shell).

**M4 adds the second real tool — OpenAI Codex — as an end-to-end vertical slice**, and
builds the **discovery machinery** (an adapter registry + a `ListAdapters` protocol
message + a client dropdown) so that aider and goose become thin follow-ups. Scope is
deliberately one tool: prove the native-hook-injection pattern for a new tool and make
adapters self-describing, rather than fanning out breadth-first.

Design inputs are captured in the research doc
`docs/superpowers/research/2026-07-29-m4-adapter-notifications.md` (Codex/aider/goose
notification mechanisms, primary-source-cited). aider and goose are documented there
for later; **M4 implements Codex only**.

### What exists (ground truth)

- `AgentAdapter` trait (`agent.rs:6`): `id()`, `provision_hooks(worktree, agent_id,
  hook_sock)`, `launch_command(worktree)`, `provides_hooks()`. `ClaudeAdapter` writes
  `.claude/settings.local.json` (Notification/Stop/UserPromptSubmit/PreToolUse hooks →
  `muxy-hook --event <kind>`); `SyntheticAdapter` is the test adapter. `muxy_hook_bin()`
  resolves the `muxy-hook` path ($MUXY_HOOK_BIN → daemon-exe sibling → bare).
- The daemon (`server.rs`) pushes `MUXY_AGENT_ID` + `MUXY_HOOK_SOCK` onto every agent's
  command env (`server.rs:128-129`), and calls `provision_hooks` then `launch_command`
  at spawn (`server.rs:124-126`).
- `muxy-hook` (`crates/muxy-hook/src/main.rs`) parses only `--event
  <notification|stop|active>`, **ignores any extra argv**, drains and discards stdin,
  reads `MUXY_AGENT_ID`/`MUXY_HOOK_SOCK` from env, posts one `HookEvent`. The daemon
  maps `HookKind::Stop` → `AttentionState::Completed`, `Notification` → `NeedsInput`,
  `Active` → Working.
- M2 attention: hook-less agents get a VT scanner AND an "input clears NeedsInput →
  Working" behavior, both gated on a `hookless` set in the daemon.
- `control_json.rs`: `SpawnAgent{project,task,adapter}` matches the adapter string
  (`"claude"` → `ClaudeAdapter`, else → `SyntheticAdapter` shell). `ListAgents` →
  `AgentList` is the existing request/response pattern (internally tagged, camelCase,
  PaneId a bare number).
- Client: `SpawnSheet.swift` has a free-text `adapter` `TextField` (default `"claude"`);
  `AppModel.spawn(project,task,adapter)` sends `spawnAgent`. `ControlRequest`/
  `ControlEvent` in `Models.swift` use hand-written internally-tagged coding.

## Goals / Non-goals

**Goals:** (1) a real **`CodexAdapter`** — launching `codex` shows a live Codex agent
whose attention flips to **Completed** when a turn finishes and **clears to Working**
when the user types; (2) an **adapter registry** as the single source of truth for
spawn + discovery; (3) a **`ListAdapters`/`AdapterList`** control message; (4) a client
**adapter dropdown** replacing the free-text field. Codex 0.145.0 is installed, so the
one Uncertain mechanism (`-c notify=`) is validated during implementation.

**Non-goals (deferred):** aider and goose adapters (research captured; own follow-ups);
Codex's newer `[hooks]` system (legacy `notify` suffices for turn-complete); a
mid-turn NeedsInput signal for Codex (`notify` only fires turn-complete → Completed);
per-adapter "installed?" probing / availability UI; adapter config beyond the name.

## Component design

### `CodexAdapter` (`muxy-daemon/src/agent.rs`)

Codex's legacy `notify` fires **only** on `agent-turn-complete`, invoking an arbitrary
program with a JSON string as the trailing argv arg. Project-local `.codex/config.toml`
**cannot** set `notify` (a documented machine-local key restriction), so muxy wires it
at launch via the `-c` inline override rather than a provisioned file:

```rust
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn id(&self) -> &'static str { "codex" }

    // Nothing to write: the notify hook is a launch arg, not a file. (Valid second
    // shape for the trait — provision writes config for Claude, launch args for Codex.)
    fn provision_hooks(&self, _worktree: &Path, _agent_id: PaneId, _hook_sock: &Path)
        -> Result<()> { Ok(()) }

    fn launch_command(&self, _worktree: &Path) -> PaneCommand {
        let bin = muxy_hook_bin();
        // Codex fires `notify` only on agent-turn-complete → treat as Stop → Completed.
        // muxy-hook self-IDs from the MUXY_AGENT_ID/MUXY_HOOK_SOCK env the daemon injects;
        // it ignores Codex's trailing JSON argv. TOML array-of-argv value for `-c`.
        let notify = format!("notify=[\"{bin}\",\"--event\",\"stop\"]");
        PaneCommand { program: "codex".into(), args: vec!["-c".into(), notify],
                      cwd: None, env: vec![] }
    }

    fn provides_hooks(&self) -> bool { true }   // native completion signal; no VT scanner
}
```

- cwd/env are filled by the daemon exactly as for Claude (`cwd:None` here; the daemon
  roots the pane in the worktree and pushes the hook env vars).
- The `-c` value is a TOML array literal; the resolved `muxy-hook` path is quoted so a
  path with spaces still parses. (Codex parses `-c value` as TOML when possible.)
- **No `muxy-hook` change:** `codex … notify → muxy-hook --event stop '<json>'` already
  works — the extra JSON argv is ignored, stdin is drained, `Stop` → `Completed`.

**Validation-first (plan Task 1, Step 0):** confirm on the installed **codex 0.145.0**
that `codex -c 'notify=["<bin>","--event","stop"]'` is honored (the notify child runs
and inherits env). If `-c notify` is rejected (the one Uncertain item), fall back to
either mutating global `~/.codex/config.toml` at provision or the newer project-scoped
`[hooks]` system — decided at implementation against the live CLI. Document the working
form.

### Attention clear — generalize M2 input-clear (`muxy-daemon/src/server.rs`)

Codex signals only turn-complete (→ Completed); it has no Claude-style `Active`
("resumed work") signal, so its badge must clear when the user engages. **Generalize
the M2 input-clear**: on `ClientToDaemon::Input` to an agent pane, if that agent's
current attention is `NeedsInput` **or** `Completed`, `set_attention(pane, Working)` —
for **all** agents, not only hook-less ones. This decouples input-clear from the
VT-scanner gate (the `hookless` set still gates only the scanner). Harmless for Claude
(its `Active` hook already drives Working); necessary for Codex.

### Adapter registry + protocol (`muxy-proto`, `muxy-daemon`)

One registry is the single source of truth for spawning and for discovery, replacing
the ad-hoc string match:

```rust
// muxy-daemon: a descriptor + lookup used by both control_json spawn and list_adapters.
pub struct AdapterDescriptor { pub id: &'static str, pub display_name: &'static str }

// Known spawnable adapters (claude, codex, shell). `build(id) -> Option<Box<dyn AgentAdapter>>`
// constructs one; `all() -> &[AdapterDescriptor]` lists them.
```

`control_json.rs::SpawnAgent` uses `build(adapter)` (unknown id → `Error`), so `"codex"`
routes to `CodexAdapter` and the "claude"/else special-case is gone (shell stays an
explicit registry entry).

New proto (`muxy-proto`, mirroring `ListAgents`/`AgentList`, `#[serde(tag="type",
rename_all="camelCase")]`):

```rust
ControlRequest::ListAdapters
ControlEvent::AdapterList { adapters: Vec<AdapterInfo> }
pub struct AdapterInfo { pub id: String, pub display_name: String }   // camelCase on the wire
```

Daemon `list_adapters()` returns `AdapterList` from the registry; `control_json` handles
`ListAdapters`. (`build` returning the trait object and `all()` returning descriptors
keep one list; adding aider/goose later = one registry entry + one adapter struct.)

### Client (`MuxyCore`, `MuxyApp`)

- `MuxyCore/Models.swift`: `ControlRequest.listAdapters` (encode `type:"listAdapters"`),
  and decode `ControlEvent.adapterList` → `[AdapterInfo]` (`AdapterInfo{id,displayName}`,
  camelCase, hand-written coding like the other messages).
- `MuxyCore/AppModel.swift`: `@Published var adapters: [AdapterInfo]`, populated on
  connect by sending `listAdapters` (alongside the existing `listAgents`); default to
  `[claude]` until the reply arrives so the sheet is never empty.
- `MuxyApp/SpawnSheet.swift`: replace the free-text `TextField("Adapter")` with a
  `Picker` over `model.adapters` (bind the selected `id`, show `display_name`), default
  `"claude"`. `AppModel.spawn(...)` is unchanged (still sends the chosen id string).

## Data flow

```
spawn: client SpawnAgent{adapter:"codex"} ─► registry.build("codex") ─► CodexAdapter
   ─► launch `codex -c 'notify=["muxy-hook","--event","stop"]'` (env: MUXY_AGENT_ID/SOCK)
codex turn ends ─► codex runs notify child ─► muxy-hook --event stop ─► HookKind::Stop
   ─► set_attention(Completed) ─► badge/tray (unchanged path)
user types to the pane ─► Input ─► attention was Completed/NeedsInput ─► set_attention(Working)
discovery: client ListAdapters ─► AdapterList{[claude,codex,shell]} ─► SpawnSheet Picker
```

## Decomposition (each its own plan → SDD → PR)

- **M4a — Codex adapter + input-clear (Rust, muxy-daemon):** `CodexAdapter` (validated
  `-c notify` form), the adapter registry (`AdapterDescriptor`/`build`/`all`), rewire
  `control_json` spawn onto the registry, and generalize the M2 input-clear to
  Completed/NeedsInput for all agents. Unit-tested; Codex live-launch is a manual/gated
  check.
- **M4b — ListAdapters protocol (Rust, muxy-proto + muxy-daemon):** `ListAdapters`/
  `AdapterList`/`AdapterInfo` proto, `list_adapters()`, control_json handling. Unit-tested
  (round-trip + control-channel reply).
- **M4c — client adapter picker (Swift, MuxyCore + MuxyApp):** `listAdapters` request +
  `AdapterList` decode + `AppModel.adapters`; SpawnSheet free-text → `Picker`. MuxyCore
  unit-tested; UI build + manual.

## Testing

- **M4a (`cargo test`):** `CodexAdapter.launch_command` yields program `codex` with `-c`
  + a `notify=[...]` containing the resolved `muxy-hook` path and `--event stop`;
  `provides_hooks()==true`; `provision_hooks` writes nothing; `registry.build("codex")`
  is a `CodexAdapter` and `build("nope")` is `None`; the generalized input-clear turns a
  **hook'd** agent in `Completed` to `Working` on `Input` (extend the existing input-clear
  test). Codex-invocation validation (Step 0) is manual against the installed CLI.
- **M4b (`cargo test`):** `ListAdapters` → `AdapterList` round-trips; `list_adapters()`
  returns claude+codex+shell with display names; over the control channel a `ListAdapters`
  yields the `AdapterList`. PaneId/casing conventions unchanged.
- **M4c (`swift test` + build):** `ControlRequest.listAdapters` encodes to
  `{"type":"listAdapters"}`; an `adapterList` event decodes into `AppModel.adapters`;
  the picker renders the list; default remains `claude`.
- **Manual (user):** spawn a real `codex` agent from the picker; finish a turn → the
  sidebar/tray shows Completed; type into it → clears to Working; the picker lists
  claude/codex/shell.

## Risks

1. **`codex -c notify=` scoping** (the one Uncertain mechanism) — validated first on the
   installed 0.145.0; documented fallback (global config or `[hooks]`) if rejected.
   Contained to `CodexAdapter`.
2. **Codex has no mid-turn NeedsInput signal** — `notify` is turn-complete only, so
   Codex attention is Completed (not NeedsInput) and relies on input-clear to reset.
   Acceptable for M4; finer states (`PermissionRequest`/`[hooks]`) are a later option.
3. **Input-clear generalization touching Claude** — Claude already reaches Working via
   its `Active` hook, so also clearing on keystroke is redundant, not a regression; the
   VT-scanner gate is untouched.
4. **muxy-hook payload divergence (future)** — aider passes no payload, goose uses
   stdin; Codex uses argv which muxy-hook already tolerates. No M4 change; noted for the
   aider/goose follow-ups.

## Verification gate

Per slice: `cargo test` / `swift test` green for that slice + all existing. End state: a
Codex agent is spawnable from a discoverable picker, its turn-complete drives a
Completed badge that clears on input, and the adapter registry + `ListAdapters` make
aider/goose one-entry additions. Manual confirmation by the user running a real `codex`
agent.

**✅ MANUALLY VERIFIED — 2026-07-30.** A real authenticated `codex` agent was spawned
from the adapter picker (which listed claude/codex/shell): on turn-complete the sidebar
badge went **Completed** and typing into the pane cleared it back to **Working**. This
confirms `codex -c 'notify=[...]'` fires end-to-end on **codex 0.145.0** — the load-bearing
M4a uncertainty is resolved, and no global-config / `[hooks]` fallback is needed.
Slices merged: M4a (PR #26), M4b (PR #27), M4c (PR #28).
