# muxy M0b — Workspace Provisioning + Hook Attention

## Context

M0a delivered the daemon/client/PTY spine: the daemon owns PTY-backed panes, a client
attaches/detaches over a Unix socket, and panes survive detach. But an M0a "pane" is just a
raw command in the current directory — it has no isolation and no notion of *needing
attention*. M0b makes a pane into an **agent**: an isolated coding-agent process that runs in
its own git worktree and tells muxy, via the tool's own hooks, when it needs the user.

This is the feature that makes "run many agents in parallel" actually safe (isolated working
copies) and actually usable (you're told when one is blocked/done). M0b stays **headless**
(the GUI is M0c) and is tested end-to-end with a **synthetic agent**, so nothing here depends
on a live `claude` binary, API auth, or the network.

Builds directly on M0a: `muxy-proto` (`PaneId`, `MsgStream`, `ClientToDaemon`/`DaemonToClient`),
`muxy-daemon` (`Daemon`, `Pane`, `PaneCommand{program,args,cwd,env}`, `spawn_pane`, `handle_conn`),
`muxy-client` (`pump()`).

## Confirmed decisions

- **Test boundary:** synthetic agent (a shell script that calls `muxy-hook`) drives all
  automated tests — deterministic, offline, free. Real `claude` is an adapter *config* on top;
  its live end-to-end run is manual.
- **Worktree mechanism:** shell out to `git worktree add/remove` behind a `WorkspaceDriver`
  trait. Simplest, no new heavy dependency (git is already required), fully testable. git2/gix
  and jj-lib (M3) can replace the impl behind the trait later.
- **Attention surface (no GUI yet):** daemon records per-pane attention state, emits a new
  `DaemonToClient::AttentionChanged` over `muxy-proto`, AND fires an OS desktop notification
  (via `notify-rust`) — so M0b is demoable end-to-end without a GUI.
- **Lifecycle scope:** provision + spawn + attention + **basic teardown** (remove the worktree
  on explicit request). The land/merge/PR UX is deferred (needs the palette/GUI → M1/M3).

## Architecture

### New crate: `muxy-workspace`

```rust
pub struct Workspace { pub path: PathBuf, pub branch: String }

pub trait WorkspaceDriver: Send + Sync {
    /// Create an isolated working copy on a fresh branch under the project's repo.
    fn provision(&self, project: &Path, name: &str) -> anyhow::Result<Workspace>;
    /// Remove the working copy (best-effort prune of stale registrations).
    fn teardown(&self, ws: &Workspace) -> anyhow::Result<()>;
}

pub struct GitWorktreeDriver;   // shells out to `git`
```

`GitWorktreeDriver::provision` runs `git -C <project> worktree add <worktrees_dir>/<name> -b muxy/<name>`
(worktrees created under a muxy-owned scratch dir, e.g. `<project>/.muxy/worktrees/<name>`);
`teardown` runs `git -C <project> worktree remove <path> --force` then `git -C <project> worktree prune`.
Tested against a temp git repo: provision yields an existing dir on the new branch isolated from
the project's working copy; teardown removes it.

### New crate: `muxy-hook`

The tiny relay binary injected into an agent's hook config. It:
1. reads the tool's hook JSON from **stdin**,
2. reads `MUXY_AGENT_ID` and `MUXY_HOOK_SOCK` from its environment,
3. connects to the daemon's hook socket, sends one framed `HookEvent`, exits.

~100 lines. Unit-tested by feeding stdin JSON + env and asserting the framed `HookEvent` it
writes to a socket.

### `muxy-proto` additions

```rust
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookKind { Notification, Stop }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HookEvent { pub agent_id: PaneId, pub kind: HookKind }

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttentionState { Idle, Working, NeedsInput, Completed }

// added variant on the existing enum:
// DaemonToClient::AttentionChanged { pane: PaneId, state: AttentionState }
```

`HookEvent` travels over the hook socket (one-shot); `AttentionChanged` travels over the
existing client protocol. Both use the existing `MsgStream` framing.

### `muxy-daemon` additions

- **`agent.rs`** — the agent concept on top of `Pane`:
  ```rust
  pub trait AgentAdapter: Send + Sync {
      fn id(&self) -> &'static str;                 // "claude", "synthetic"
      /// Write the tool's hook config into the fresh worktree so its hooks call muxy-hook.
      fn provision_hooks(&self, worktree: &Path, agent_id: PaneId, hook_sock: &Path) -> anyhow::Result<()>;
      /// The command to launch the agent in the worktree.
      fn launch_command(&self, worktree: &Path) -> PaneCommand;
  }
  ```
  - `ClaudeAdapter` — `provision_hooks` writes a git-ignored `<worktree>/.claude/settings.local.json`
    whose `Notification` and `Stop` hooks run `muxy-hook --event <kind>`; `launch_command` returns
    the `claude` invocation.
  - `SyntheticAdapter` — for tests: `provision_hooks` writes a small script into the worktree, and
    `launch_command` runs it; the script invokes `muxy-hook` to emit a chosen event. No real agent.
  - `Daemon::spawn_agent(project: &Path, adapter: &dyn AgentAdapter, task: &str) -> Result<PaneId>`:
    provision worktree → set env (`MUXY_AGENT_ID`=pane id, `MUXY_HOOK_SOCK`=hook socket path) →
    `adapter.provision_hooks(...)` → build `PaneCommand` from `adapter.launch_command(...)` with the
    worktree as `cwd` and the env → `spawn_pane`. Records `PaneId → (Workspace, AttentionState)`.

- **`attention.rs`** — the hook receiver + attention state:
  - A second `UnixListener` on the hook socket path; each connection is a one-shot: read one
    `HookEvent`, map `agent_id → PaneId`, update attention, broadcast `AttentionChanged`, fire a
    notification. `HookKind::Notification → NeedsInput`; `HookKind::Stop → Completed`.
  - Per-pane `AttentionState` stored on the daemon; a broadcast channel of `AttentionChanged`
    that `handle_conn` forwards to attached clients.
  - **Pane-exit wiring (resolves the M0a child-exit deferral):** watch each agent pane's
    `wait_exit()`; on exit, emit `PaneExited` and set attention `Completed` + notify. This closes
    the M0a gap where an attached client hung forever when the child exited.

- **`notify.rs`** — `trait Notifier { fn notify(&self, pane: PaneId, state: AttentionState); }`;
  `OsNotifier` (via `notify-rust`) and a `FakeNotifier` (records calls) so tests assert
  notification without popping real banners. The daemon holds a `Box<dyn Notifier>`.

- **Teardown:** `Daemon::teardown_agent(pane: PaneId) -> Result<()>` = kill the pane +
  `WorkspaceDriver::teardown(workspace)` + drop daemon state for that pane. Driven by tests in
  M0b; a client-facing trigger arrives with the GUI.

### Correlation

An agent is identified by `MUXY_AGENT_ID` (= its `PaneId`), pinned in the environment at spawn —
never by cwd or session-id (a subagent or a `cd` would break those). `muxy-hook` echoes it in the
`HookEvent`; the daemon maps it back to the `PaneId`.

## Testability

- **`muxy-workspace`:** temp git repo → provision → assert worktree dir exists on branch
  `muxy/<name>` and is a distinct working copy; teardown → assert removed.
- **`muxy-hook`:** feed hook JSON on stdin + `MUXY_AGENT_ID`/`MUXY_HOOK_SOCK` env pointing at a
  test `UnixListener`; assert the received framed `HookEvent` matches.
- **`ClaudeAdapter`:** assert `provision_hooks` writes a `.claude/settings.local.json` containing
  the `muxy-hook` command under `Notification` and `Stop` hooks.
- **End-to-end (the M0b proof):** temp repo → `spawn_agent` with `SyntheticAdapter` → assert the
  worktree was created on a fresh branch and the agent ran there → the synthetic agent's
  `muxy-hook` call drives the daemon to flip attention to `NeedsInput`, broadcast
  `AttentionChanged` (observed via an attached client / the attention broadcast), and call
  `FakeNotifier` → `teardown_agent` removes the worktree.
- **Pane-exit:** spawn a synthetic agent that exits; assert `PaneExited` is emitted and attention
  becomes `Completed` (regression test for the M0a child-exit hang).

## Explicitly deferred (later milestones)

- VT-signal fallback (BEL/OSC) + hook/VT fusion → M2.
- jj `WorkspaceDriver` impl → M3.
- Land/merge/discard UX → M1/M3.
- GUI sidebar badges + client-triggered teardown → M0c.

## Verification

- `cargo test` — whole workspace green, including the M0b end-to-end synthetic-agent test and
  the pane-exit regression test.
- Manual (optional, real agent): with `claude` installed + authed, `spawn_agent` a `ClaudeAdapter`
  on a scratch repo; confirm a `git worktree` appears on a `muxy/<task>` branch, and that when
  Claude asks a question or finishes, an OS notification fires and `AttentionChanged` reaches an
  attached client.
