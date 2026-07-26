# muxy M0c-3a — JSON Control Channel + Spawn

## Context

M0c-3 is the native macOS client. Its Rust-side prerequisite (M0c-3a) is a **Swift-friendly
way to drive the daemon**: a JSON control channel the SwiftUI app reads with `Codable` (no FFI,
no postcard-in-Swift), plus a way to **spawn agents** (currently agents are only created in
tests). This is pure Rust, TDD-able; the Swift app is M0c-3b.

Builds on M0c-1's control feed (`list_agents()`, `subscribe_attention()`) and the reaper
(`subscribe_removed()`, `AttentionState::Exited`). The render path is unchanged — a libghostty
surface runs `muxy attach <pane>` (postcard client socket); this JSON channel is *only* the
sidebar's data feed + spawn control.

## Decisions (confirmed)

- **A dedicated JSON-lines control socket** (a third Unix socket beside the postcard client
  socket and the hook socket). JSON-only — no protocol detection. The Swift app connects here.
- **Spawn from both** a `muxy spawn` CLI and (later, M0c-3b) a GUI button — both send the same
  `SpawnAgent` control request.
- The control socket path is **not** stored on `Daemon` (it isn't injected into agents like
  `hook_sock`): `main.rs` binds it and calls `serve_control_json`; the CLI/GUI connect via
  `MUXY_CONTROL_SOCK` (default `/tmp/muxy-control.sock`). No `Daemon` constructor change.

## Protocol (`muxy-proto::control`, JSON)

Newline-delimited JSON, internally tagged (`#[serde(tag = "type", rename_all = "camelCase")]`)
so Swift `Codable` maps cleanly. Reuses `AgentInfo`/`AttentionState`/`PaneId` (already `Serialize`;
`PaneId` is a newtype → a bare number in JSON).

```rust
// GUI/CLI → daemon
pub enum ControlRequest {
    ListAgents,                                              // {"type":"listAgents"}
    SpawnAgent { project: String, task: String, adapter: String },
}

// daemon → GUI/CLI
pub enum ControlEvent {
    AgentList { agents: Vec<AgentInfo> },
    AttentionChanged { pane: PaneId, state: AttentionState },
    AgentRemoved { pane: PaneId },
    AgentSpawned { pane: PaneId },
    Error { message: String },
}
```

## Daemon behavior (`serve_control_json` / `handle_control_json`)

A control-JSON connection: on connect, send an `AgentList` snapshot; then a `select!` loop:
- **incoming line** → parse `ControlRequest`: `ListAgents` re-sends `AgentList`; `SpawnAgent`
  maps the adapter string → an `AgentAdapter` and calls `spawn_agent`, replying `AgentSpawned{pane}`
  or `Error{message}` (a failed spawn tears down its worktree, per the reaper work).
- **attention broadcast** (all panes) → `AttentionChanged` (incl. `Exited`).
- **removed broadcast** → `AgentRemoved`.
- client disconnect → end.

**Adapter mapping** (`spawn_from_control`): `"claude"` → `ClaudeAdapter` (the real agent);
`"shell"` → a plain shell in the worktree (`SyntheticAdapter` running `$SHELL`/`/bin/sh`) — a
genuinely useful "just a terminal in an isolated worktree" agent that is also deterministically
testable (unlike `claude`, which needs the binary + auth). Unknown adapter → `Error`.

`main.rs` binds a third socket (`MUXY_CONTROL_SOCK`, default `/tmp/muxy-control.sock`) and spawns
`serve_control_json` alongside the existing client + hook serving.

## CLI (`muxy spawn`)

The `muxy` binary gains subcommand dispatch: `muxy spawn <project> <task> [adapter=claude]`
connects to the control socket, sends a `SpawnAgent` line, reads one `ControlEvent`, and prints
the new pane id (or the error). `muxy attach <pane>` / the legacy `muxy <pane>` (pump) is
unchanged. A small testable `spawn_via_control(sock, req) -> Result<PaneId>` holds the logic.

## Testability (all Rust/TDD)

- `muxy-proto::control`: `serde_json` round-trips of each `ControlRequest`/`ControlEvent`, asserting
  the tagged JSON shape (e.g. `{"type":"spawnAgent",...}`).
- `serve_control_json` over an in-memory `duplex`: `listAgents` → `agentList`; `spawnAgent` with
  `"shell"` in a temp git repo → `agentSpawned{pane}` and the agent then appears in `agentList`;
  a following `set_attention` streams an `attentionChanged`; `teardown_agent` streams `agentRemoved`.
- `muxy spawn` CLI logic: `spawn_via_control` against an in-process daemon + `serve_control_json`
  on a temp socket → returns the new pane id.

## Deferred (M0c-3b)

The SwiftUI app itself: connecting to the JSON socket, the sidebar (agents by project + badges),
the libghostty surface view running `muxy attach`, and the GUI spawn button (which sends the same
`SpawnAgent`). Verified visually.

## Verification

`cargo test` — whole workspace green including the control-JSON handler + CLI tests. Manual:
run the daemon, `muxy spawn <repo> demo shell`, then `muxy attach <pane>` to confirm the spawned
shell agent renders and survives; a second process reading the control socket sees the `agentList`
+ live events.
