# muxy — Agent Reaper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the daemon from reporting *ghost* agents. When an agent's process exits on its own, the daemon must reflect it accurately (a terminal `Exited` state, not a stale `Working`) while **keeping the agent in the list** so the user can still land/discard its worktree. Only explicit `teardown_agent` removes an agent — and it must now tell control clients via a new `AgentRemoved` event.

**Architecture:** Pure Rust, TDD, on top of M0a/M0b/M0c-1. Two independent effects: (1) a per-agent **exit-watcher** task (spawned at `spawn_agent`) that, on `pane.wait_exit()`, sets the agent's attention to `AttentionState::Exited` — broadcast over the *existing* attention feed, so the control connection already forwards it. (2) a **removal event**: `teardown_agent` broadcasts the removed pane over a new channel that the control connection forwards as `DaemonToClient::AgentRemoved`.

**Tech Stack:** Rust stable, tokio, muxy-proto, muxy-daemon (unchanged deps).

## Global Constraints

- **Cargo is not on PATH.** Prefix every cargo command: `source "$HOME/.cargo/env" && cargo ...`
- **Rust stable only**; crates prefixed `muxy-` under `crates/`.
- **Do not break M0a/M0b/M0c-1.** All existing tests (currently **27**) stay green; run `source "$HOME/.cargo/env" && cargo test` (whole workspace) after every task.
- **Adding enum variants breaks exhaustive matches** (this has bitten twice). Task 1 adds `AttentionState::Exited` and `DaemonToClient::AgentRemoved` and MUST fix every exhaustive match they break: `muxy-client` `pump()` (on `DaemonToClient`) and `OsNotifier::notify` (on `AttentionState`, `crates/muxy-daemon/src/notify.rs`). Build the WHOLE workspace to catch them.
- **Explicit `git add`** of changed files (+ `Cargo.lock` if changed); never `git add .`.

---

### Task 1: `muxy-proto` — `AttentionState::Exited` + `DaemonToClient::AgentRemoved` (+ fix exhaustive matches)

**Files:**
- Modify: `crates/muxy-proto/src/message.rs` (add a variant to each enum)
- Modify: `crates/muxy-client/src/lib.rs` (`pump()` match arm for `AgentRemoved`)
- Modify: `crates/muxy-daemon/src/notify.rs` (`OsNotifier::notify` arm for `Exited`)
- Test: inline `#[cfg(test)]` in `message.rs`

**Interfaces:**
- Produces: `AttentionState::Exited` (terminal — process died); `DaemonToClient::AgentRemoved { pane: PaneId }`.

- [ ] **Step 1: Add the variants in `crates/muxy-proto/src/message.rs`**

Add `Exited` to `AttentionState` (keep the 4 existing):
```rust
pub enum AttentionState {
    Idle,
    Working,
    NeedsInput,
    Completed,
    Exited,
}
```
Add `AgentRemoved` to `DaemonToClient` (keep the 5 existing):
```rust
    AgentRemoved { pane: PaneId },
```

- [ ] **Step 2: Fix `pump()`'s match in `crates/muxy-client/src/lib.rs`**

Add an arm beside the other ignored control variants (the headless pump ignores removals):
```rust
Some(DaemonToClient::AgentRemoved { .. }) => {}
```

- [ ] **Step 3: Fix `OsNotifier::notify` in `crates/muxy-daemon/src/notify.rs`**

Add an `Exited` arm to the `match state` (a process exit is worth a notification, like `Completed`):
```rust
            AttentionState::Exited => "exited",
```
(Place it beside the `Completed => "finished"` arm; keep `Idle | Working => return`.)

- [ ] **Step 4: Write the failing tests (append to `message.rs`'s `#[cfg(test)] mod tests`)**

```rust
#[test]
fn attention_exited_roundtrips() {
    let m = DaemonToClient::AttentionChanged { pane: PaneId(1), state: AttentionState::Exited };
    let bytes = postcard::to_stdvec(&m).unwrap();
    assert_eq!(m, postcard::from_bytes::<DaemonToClient>(&bytes).unwrap());
}

#[test]
fn agent_removed_roundtrips() {
    let m = DaemonToClient::AgentRemoved { pane: PaneId(5) };
    let bytes = postcard::to_stdvec(&m).unwrap();
    assert_eq!(m, postcard::from_bytes::<DaemonToClient>(&bytes).unwrap());
}
```

- [ ] **Step 5: Run tests (whole workspace — catches the two match breaks)**

Run: `source "$HOME/.cargo/env" && cargo test`
Expected: PASS — 2 new tests; `muxy-client` + `muxy-daemon` compile; all 27 prior green (29 total).

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-proto/src/message.rs crates/muxy-client/src/lib.rs crates/muxy-daemon/src/notify.rs
git commit -m "feat(proto): add AttentionState::Exited + DaemonToClient::AgentRemoved"
```

---

### Task 2: `muxy-daemon` — exit-watcher sets `Exited` on process exit

**Files:**
- Modify: `crates/muxy-daemon/src/server.rs` (`spawn_agent` → `self: &Arc<Self>`; spawn an exit-watcher task)
- Test: inline `#[cfg(test)]` in `server.rs`

**Interfaces:**
- Changes: `spawn_agent(self: &Arc<Self>, project, adapter, task) -> Result<PaneId>` (was `&self`; all callers already hold `Arc<Daemon>`).
- Behavior: after a successful spawn, a background task awaits the pane's exit and sets attention `Exited` (daemon-side, independent of any attached client).

- [ ] **Step 1: Change `spawn_agent` to `self: &Arc<Self>` and spawn the watcher**

Change the signature:
```rust
    pub fn spawn_agent(self: &Arc<Self>, project: &Path, adapter: &dyn AgentAdapter, task: &str) -> Result<PaneId> {
```
At the end (after `self.set_attention(id, AttentionState::Working);`, before `Ok(id)`), spawn the exit-watcher. It re-fetches the pane `Arc` and, on exit, marks the agent `Exited` (the agent stays in the registry — only `teardown_agent` removes it):
```rust
        let me = Arc::clone(self);
        if let Some(pane_arc) = self.panes.lock().unwrap().get(&id).cloned() {
            tokio::spawn(async move {
                pane_arc.wait_exit().await;
                me.set_attention(id, AttentionState::Exited);
            });
        }
```
(`Pane::wait_exit()` is idempotent — the M0b `handle_conn` exit arm may also await it when a client is attached; both firing is fine.)

- [ ] **Step 2: Write the failing test (append to `server.rs`'s `tests` module)**

Spawns a synthetic agent that exits immediately and asserts the daemon marks it `Exited` **with no client attached**, and that it remains in `list_agents()`.

```rust
#[tokio::test]
async fn agent_marked_exited_on_process_exit() {
    use crate::{FakeNotifier, SyntheticAdapter};
    use muxy_proto::AttentionState;
    use muxy_workspace::GitWorktreeDriver;
    use std::process::Command as PCommand;
    use std::time::Duration;

    let repo = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t.test"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "init"]);

    let daemon = Arc::new(Daemon::new_with(
        Arc::new(GitWorktreeDriver),
        Arc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-reaper.sock"),
    ));
    let adapter = SyntheticAdapter {
        command: crate::PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 0".into()],
            cwd: None,
            env: vec![],
        },
    };
    let pane = daemon.spawn_agent(repo.path(), &adapter, "task-x").unwrap();

    // No client attached: the daemon-side watcher must still flip attention to Exited.
    let mut exited = false;
    for _ in 0..100 {
        if daemon.attention_of(pane) == Some(AttentionState::Exited) { exited = true; break; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(exited, "agent was not marked Exited after its process exited");
    // It stays in the list (mark-exited-and-keep), still reported with Exited state.
    let list = daemon.list_agents();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].state, AttentionState::Exited);

    daemon.teardown_agent(pane).unwrap();
}
```

- [ ] **Step 3: Run tests**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon agent_marked_exited` then `source "$HOME/.cargo/env" && cargo test` (whole workspace).
Expected: PASS. The `agent_e2e` test (which spawns a long-lived `sleep 30` agent) stays green — its watcher just pends.

- [ ] **Step 4: Commit**

```bash
git add crates/muxy-daemon/src/server.rs
git commit -m "feat(daemon): exit-watcher marks agents Exited on process exit"
```

---

### Task 3: `muxy-daemon` — `AgentRemoved` on teardown, forwarded by the control feed

**Files:**
- Modify: `crates/muxy-daemon/src/server.rs` (add `removed_tx` broadcast + `subscribe_removed()`; `teardown_agent` broadcasts; `handle_control` forwards `AgentRemoved`)
- Test: inline `#[cfg(test)]` in `server.rs`

**Interfaces:**
- Produces on `Daemon`: `removed_tx: broadcast::Sender<PaneId>` field; `pub fn subscribe_removed(&self) -> broadcast::Receiver<PaneId>`.
- Behavior: `teardown_agent` broadcasts the removed pane; a control connection forwards it as `DaemonToClient::AgentRemoved { pane }`.

- [ ] **Step 1: Add the `removed_tx` field + `subscribe_removed()`**

Add to `struct Daemon`:
```rust
    removed_tx: broadcast::Sender<PaneId>,
```
Initialize in `new_with` (alongside `attention_tx`):
```rust
        let (removed_tx, _) = broadcast::channel(256);
```
and add `removed_tx,` to the `Daemon { ... }` constructor. Add the accessor:
```rust
    pub fn subscribe_removed(&self) -> broadcast::Receiver<PaneId> {
        self.removed_tx.subscribe()
    }
```

- [ ] **Step 2: Broadcast from `teardown_agent`**

In `teardown_agent`, after removing per-pane state (panes/attention/workspaces/agents), broadcast the removal (ignore send error when there are no control subscribers):
```rust
        let _ = self.removed_tx.send(pane);
```

- [ ] **Step 3: Forward `AgentRemoved` in `handle_control`**

In `handle_control`, subscribe before the loop:
```rust
        let mut removed_rx = self.subscribe_removed();
```
Add a `select!` arm (beside the attention + incoming arms):
```rust
                removed = removed_rx.recv() => {
                    match removed {
                        Ok(pane) => { msgs.send(&DaemonToClient::AgentRemoved { pane }).await?; }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
```

- [ ] **Step 4: Write the failing test (append to `server.rs`'s `tests` module)**

A control client sees an `AgentRemoved` when an agent is torn down.

```rust
#[tokio::test]
async fn control_conn_gets_agent_removed_on_teardown() {
    use crate::{FakeNotifier, SyntheticAdapter};
    use muxy_workspace::GitWorktreeDriver;
    use std::process::Command as PCommand;
    use std::time::Duration;

    let repo = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t.test"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "init"]);

    let daemon = Arc::new(Daemon::new_with(
        Arc::new(GitWorktreeDriver),
        Arc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-removed.sock"),
    ));
    let adapter = SyntheticAdapter {
        command: crate::PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 30".into()],
            cwd: None,
            env: vec![],
        },
    };
    let pane = daemon.spawn_agent(repo.path(), &adapter, "task-a").unwrap();

    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let d = daemon.clone();
    tokio::spawn(async move { let _ = d.handle_conn(server_io).await; });

    let mut client = MsgStream::<_>::new(client_io);
    client.send(&ClientToDaemon::ListAgents).await.unwrap();
    // Drain the initial AgentList.
    let _ = client.recv::<DaemonToClient>().await.unwrap().unwrap();

    daemon.teardown_agent(pane).unwrap();

    let mut removed = None;
    for _ in 0..40 {
        if let Ok(Ok(Some(DaemonToClient::AgentRemoved { pane: p }))) =
            tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await
        {
            removed = Some(p);
            break;
        }
    }
    assert_eq!(removed, Some(pane));
}
```

- [ ] **Step 5: Run tests**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon control_conn_gets_agent_removed` then `source "$HOME/.cargo/env" && cargo test` (whole workspace green).
Expected: PASS (30 total: 27 prior + Task 1's 2 + Task 2's 1 = 30; this task adds 1 → 31). Confirm the count grows and nothing regresses.

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-daemon/src/server.rs
git commit -m "feat(daemon): AgentRemoved on teardown, forwarded by control feed"
```

---

## Self-Review

- **Spec coverage:** process exit → terminal `Exited` state, agent kept in list (Task 2, daemon-side watcher, verified with no client attached) ✓; explicit teardown → `AgentRemoved` to control clients (Task 3) ✓; the stale-`Working`-ghost bug is fixed (exit flips to `Exited`, which the control feed already streams via the attention channel) ✓.
- **Placeholder scan:** every step has complete code; no TBD.
- **Type consistency:** `AttentionState::Exited` and `DaemonToClient::AgentRemoved{pane}` defined in Task 1 and consumed identically in Tasks 2–3; `spawn_agent`'s new `self: &Arc<Self>` receiver is consistent with all existing call sites (tests + e2e already hold `Arc<Daemon>`); `subscribe_removed()`/`removed_tx` defined in Task 3 Step 1 before use in Steps 2–3.
- **Regression guards:** Task 1 fixes BOTH exhaustive matches the new variants break (pump on `DaemonToClient`, `OsNotifier` on `AttentionState`) and builds the whole workspace; the exit-watcher only fires on real exit so long-lived-agent tests stay green.
- **Workflow correctness:** an exited agent is NOT auto-removed (its worktree survives for land/discard); removal + `AgentRemoved` happen only via `teardown_agent`.
