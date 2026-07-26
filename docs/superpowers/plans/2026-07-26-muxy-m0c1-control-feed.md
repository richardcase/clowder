# muxy M0c-1 — Control Feed (agent list + global attention) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the daemon a **control channel** so a GUI client can (a) list all agents grouped by project with their current attention state, and (b) subscribe to *all* agents' attention changes — not just the pane it's attached to. This is the daemon-side prerequisite for M0c-3's sidebar badges.

**Architecture:** Pure Rust, TDD, on top of M0a/M0b. The daemon already holds a **global** `attention_tx` broadcast; M0b's `handle_conn` just filtered it to the attached pane. This plan adds an agent-metadata registry (`project`/`task` per agent), a `list_agents()` snapshot, and a **control mode** for a connection: a client that opens with `ListAgents` receives an `AgentList` and then a stream of every `AttentionChanged`. No new socket — it's an alternate opening move on the existing client socket, beside `Attach`.

**Tech Stack:** Rust stable, tokio, muxy-proto, muxy-daemon (unchanged deps).

## Global Constraints

- **Cargo is not on PATH.** Prefix every cargo command: `source "$HOME/.cargo/env" && cargo ...`
- **Rust stable only**; crates prefixed `muxy-` under `crates/`; workspace root `members = ["crates/*"]`.
- **Do not break M0a/M0b.** All existing tests (currently **23**) must stay green; run `source "$HOME/.cargo/env" && cargo test` (whole workspace) after every task.
- **Adding a `DaemonToClient` variant breaks exhaustive matches.** `muxy-client`'s `pump()` matches `DaemonToClient` exhaustively — Task 1 MUST add an arm for the new variant (this exact regression happened in M0b and cost a fix round). Grep for every `match` on `DaemonToClient` before finishing Task 1.
- **Explicit `git add`** of changed files (+ `Cargo.lock` if it changed); never `git add .`; never commit `target/`.
- **Deferred to M0c-3** (do NOT build here): pump `SIGWINCH`→`Resize` adaptation and the `muxy-control-ffi` C-ABI shim for Swift — both are only exercised when the SwiftUI client + libghostty surface exist.

---

### Task 1: `muxy-proto` — `AgentInfo`, `ListAgents`, `AgentList`

**Files:**
- Modify: `crates/muxy-proto/src/message.rs` (add types + variants)
- Modify: `crates/muxy-proto/src/lib.rs` (re-export `AgentInfo`)
- Modify: `crates/muxy-client/src/lib.rs` (add the new `DaemonToClient` arm to `pump()`'s match — prevents a compile break)
- Test: inline `#[cfg(test)]` in `message.rs`

**Interfaces:**
- Consumes: `PaneId`, `AttentionState`, `ClientToDaemon`, `DaemonToClient`.
- Produces:
  - `pub struct AgentInfo { pub pane: PaneId, pub project: String, pub task: String, pub state: AttentionState }` — `Clone, Debug, PartialEq, Serialize, Deserialize`.
  - New variant `ClientToDaemon::ListAgents` (opens a control connection).
  - New variant `DaemonToClient::AgentList { agents: Vec<AgentInfo> }`.

- [ ] **Step 1: Add the type + variants in `crates/muxy-proto/src/message.rs`**

Add the struct (after `AttentionState`):
```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub pane: PaneId,
    pub project: String,
    pub task: String,
    pub state: AttentionState,
}
```
Add `ListAgents` to `ClientToDaemon` (keep the 4 existing variants):
```rust
pub enum ClientToDaemon {
    Attach { pane: PaneId },
    Input { pane: PaneId, bytes: Vec<u8> },
    Resize { pane: PaneId, cols: u16, rows: u16 },
    Detach,
    ListAgents,
}
```
Add `AgentList` to `DaemonToClient` (keep the 4 existing variants):
```rust
pub enum DaemonToClient {
    Attached { pane: PaneId, cols: u16, rows: u16 },
    Output { pane: PaneId, bytes: Vec<u8> },
    PaneExited { pane: PaneId, code: Option<i32> },
    AttentionChanged { pane: PaneId, state: AttentionState },
    AgentList { agents: Vec<AgentInfo> },
}
```

- [ ] **Step 2: Re-export in `crates/muxy-proto/src/lib.rs`**

Add `AgentInfo` to the `pub use message::{...}` list (keep the existing exports).

- [ ] **Step 3: Fix `pump()`'s exhaustive match in `crates/muxy-client/src/lib.rs`**

In `pump()`, the `match` on the received `DaemonToClient` currently handles `Output`, `PaneExited | None`, `Attached`, `AttentionChanged`. Add an arm so the headless pump ignores the new variant (a control client, not the pump, consumes `AgentList`):
```rust
Some(DaemonToClient::AgentList { .. }) => {}
```
Place it beside the existing `Some(DaemonToClient::AttentionChanged { .. }) => {}` arm.

- [ ] **Step 4: Write the failing tests (append to `message.rs`'s `#[cfg(test)] mod tests`)**

```rust
#[test]
fn list_agents_roundtrips() {
    let m = ClientToDaemon::ListAgents;
    let bytes = postcard::to_stdvec(&m).unwrap();
    assert_eq!(m, postcard::from_bytes::<ClientToDaemon>(&bytes).unwrap());
}

#[test]
fn agent_list_roundtrips() {
    let m = DaemonToClient::AgentList {
        agents: vec![AgentInfo {
            pane: PaneId(2),
            project: "muxy".into(),
            task: "task-a".into(),
            state: AttentionState::NeedsInput,
        }],
    };
    let bytes = postcard::to_stdvec(&m).unwrap();
    assert_eq!(m, postcard::from_bytes::<DaemonToClient>(&bytes).unwrap());
}
```

- [ ] **Step 5: Run tests (whole workspace — catches the pump match break)**

Run: `source "$HOME/.cargo/env" && cargo test`
Expected: PASS — muxy-proto has the 2 new tests; `muxy-client` still compiles (the new arm); all 23 prior tests green (25 total).

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-proto/src/message.rs crates/muxy-proto/src/lib.rs crates/muxy-client/src/lib.rs
git commit -m "feat(proto): add AgentInfo + ListAgents/AgentList control messages"
```

---

### Task 2: `muxy-daemon` — agent metadata registry + `list_agents()`

**Files:**
- Modify: `crates/muxy-daemon/src/server.rs` (add `agents` field + `AgentMeta`; populate in `spawn_agent`; remove in `teardown_agent`; add `list_agents()`)
- Test: inline `#[cfg(test)]` in `server.rs`

**Interfaces:**
- Consumes: `AgentInfo`, `AttentionState`, `PaneId` (proto); existing `Daemon` fields.
- Produces:
  - private `struct AgentMeta { project: String, task: String }`.
  - field `agents: Arc<Mutex<HashMap<PaneId, AgentMeta>>>` on `Daemon`.
  - `pub fn list_agents(&self) -> Vec<AgentInfo>` — one entry per registered agent, joined with its current attention (default `AttentionState::Working` if none recorded), sorted by `(project, pane.0)` for stable output.

- [ ] **Step 1: Add `AgentMeta` + the `agents` field + populate/remove**

Add near the top of `server.rs` (after imports):
```rust
struct AgentMeta {
    project: String,
    task: String,
}
```
Add the field to `struct Daemon`:
```rust
    agents: Arc<Mutex<HashMap<PaneId, AgentMeta>>>,
```
Initialize it in `new_with` (alongside the other `Arc::new(Mutex::new(HashMap::new()))` fields):
```rust
            agents: Arc::new(Mutex::new(HashMap::new())),
```
In `spawn_agent`, after `self.workspaces.lock().unwrap().insert(id, ws);` and before `set_attention`, record metadata (derive a display project name from the path's final component):
```rust
        let project_name = project
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| project.to_string_lossy().to_string());
        self.agents.lock().unwrap().insert(
            id,
            AgentMeta { project: project_name, task: task.to_string() },
        );
```
In `teardown_agent`, alongside the existing `self.attention.lock().unwrap().remove(&pane);`, also:
```rust
        self.agents.lock().unwrap().remove(&pane);
```

- [ ] **Step 2: Add `list_agents()`**

```rust
    pub fn list_agents(&self) -> Vec<muxy_proto::AgentInfo> {
        let agents = self.agents.lock().unwrap();
        let attention = self.attention.lock().unwrap();
        let mut out: Vec<muxy_proto::AgentInfo> = agents
            .iter()
            .map(|(pane, meta)| muxy_proto::AgentInfo {
                pane: *pane,
                project: meta.project.clone(),
                task: meta.task.clone(),
                state: attention.get(pane).copied().unwrap_or(muxy_proto::AttentionState::Working),
            })
            .collect();
        out.sort_by(|a, b| (a.project.as_str(), a.pane.0).cmp(&(b.project.as_str(), b.pane.0)));
        out
    }
```

- [ ] **Step 3: Write the failing test (append to `server.rs`'s `tests` module)**

Uses a temp git repo + the synthetic adapter (mirrors the `agent_e2e` setup, but inline here for `list_agents`). Add helper imports at the top of the test as needed (`FakeNotifier`, `SyntheticAdapter`, `GitWorktreeDriver`, `PaneCommand`, `tempfile`).

```rust
#[tokio::test]
async fn list_agents_reports_project_task_and_state() {
    use crate::{FakeNotifier, SyntheticAdapter};
    use muxy_proto::AttentionState;
    use muxy_workspace::GitWorktreeDriver;
    use std::process::Command as PCommand;
    use std::sync::Arc as StdArc;

    // temp git repo
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

    let daemon = StdArc::new(Daemon::new_with(
        StdArc::new(GitWorktreeDriver),
        StdArc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-listagents.sock"),
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
    daemon.set_attention(pane, AttentionState::NeedsInput);

    let list = daemon.list_agents();
    assert_eq!(list.len(), 1);
    let a = &list[0];
    assert_eq!(a.pane, pane);
    assert_eq!(a.task, "task-a");
    // project display name is the repo dir's basename
    assert_eq!(a.project, repo.path().file_name().unwrap().to_string_lossy());
    assert_eq!(a.state, AttentionState::NeedsInput);

    daemon.teardown_agent(pane).unwrap();
    assert!(daemon.list_agents().is_empty());
}
```

- [ ] **Step 4: Run tests**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon list_agents` then `source "$HOME/.cargo/env" && cargo test` (whole workspace green).
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/muxy-daemon/src/server.rs
git commit -m "feat(daemon): agent metadata registry + list_agents()"
```

---

### Task 3: `muxy-daemon` — control connection (ListAgents → AgentList + global attention stream)

**Files:**
- Modify: `crates/muxy-daemon/src/server.rs` (`handle_conn`: branch on `ListAgents` for control mode)
- Test: inline `#[cfg(test)]` in `server.rs`

**Interfaces:**
- Consumes: `list_agents()`, `subscribe_attention()`, `MsgStream`, `ClientToDaemon`, `DaemonToClient`.
- Produces: control-mode behavior on `handle_conn` — a connection whose first message is `ListAgents` receives one `AgentList`, then every `AttentionChanged` (all panes) until it disconnects.

- [ ] **Step 1: Branch `handle_conn`'s opening loop on `ListAgents`**

`handle_conn`'s initial loop currently waits for `Attach` (ignoring other messages). Add a `ListAgents` arm that enters control mode. In the `match msgs.recv::<ClientToDaemon>().await?` block that selects the pane, add before the `Some(_) => continue` catch-all:

```rust
                Some(ClientToDaemon::ListAgents) => {
                    return self.handle_control(msgs).await;
                }
```

Then add the control handler method (a sibling of `handle_conn`):

```rust
    async fn handle_control<S>(self: Arc<Self>, mut msgs: MsgStream<S>) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        // Snapshot the agent list, then stream every attention change.
        let mut att_rx = self.subscribe_attention();
        msgs.send(&DaemonToClient::AgentList { agents: self.list_agents() }).await?;
        loop {
            tokio::select! {
                att = att_rx.recv() => {
                    match att {
                        Ok((pane, state)) => {
                            msgs.send(&DaemonToClient::AttentionChanged { pane, state }).await?;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break, // attention channel closed
                    }
                }
                incoming = msgs.recv::<ClientToDaemon>() => {
                    match incoming? {
                        Some(ClientToDaemon::ListAgents) => {
                            // Client asked to refresh the list.
                            msgs.send(&DaemonToClient::AgentList { agents: self.list_agents() }).await?;
                        }
                        Some(_) => continue,     // control conn ignores pane ops
                        None => break,           // client disconnected
                    }
                }
            }
        }
        Ok(())
    }
```

(Note: `handle_control` takes the `MsgStream` by value after `handle_conn` created it; adjust `handle_conn` so `msgs` is moved into `handle_control` on the `ListAgents` branch — it already `return`s there, so no later use conflicts.)

- [ ] **Step 2: Write the failing test (append to `server.rs`'s `tests` module)**

Drives a control connection over an in-memory duplex against an in-process daemon with one agent.

```rust
#[tokio::test]
async fn control_conn_lists_agents_and_streams_attention() {
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
        std::path::PathBuf::from("/tmp/unused-control.sock"),
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

    // First reply is the agent list.
    match client.recv::<DaemonToClient>().await.unwrap().unwrap() {
        DaemonToClient::AgentList { agents } => {
            assert_eq!(agents.len(), 1);
            assert_eq!(agents[0].pane, pane);
            assert_eq!(agents[0].task, "task-a");
        }
        other => panic!("expected AgentList, got {other:?}"),
    }

    // A later attention change streams over the SAME control connection,
    // even though this client is not "attached" to the pane.
    daemon.set_attention(pane, AttentionState::NeedsInput);
    let mut saw = None;
    for _ in 0..40 {
        if let Ok(Ok(Some(DaemonToClient::AttentionChanged { pane: p, state }))) =
            tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await
        {
            if p == pane { saw = Some(state); break; }
        }
    }
    assert_eq!(saw, Some(AttentionState::NeedsInput));

    daemon.teardown_agent(pane).unwrap();
}
```

- [ ] **Step 3: Run tests**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon control_conn` then `source "$HOME/.cargo/env" && cargo test` (whole workspace).
Expected: PASS. Existing `handle_conn` attach tests must stay green (the `ListAgents` branch only triggers on that message).

- [ ] **Step 4: Commit**

```bash
git add crates/muxy-daemon/src/server.rs
git commit -m "feat(daemon): control connection — ListAgents + global attention stream"
```

---

## What M0c-1 excludes (M0c-3)

- Pump `SIGWINCH`→`Resize` (only exercised when libghostty drives the pump's PTY).
- `muxy-control-ffi` C-ABI shim for Swift to consume this feed.
- The SwiftUI sidebar/terminal view + libghostty surface embedding.

## Self-Review

- **Spec coverage:** control feed = agent list grouped-able by project (`AgentInfo.project`, sorted) ✓ + subscribe-all attention (control conn streams every `AttentionChanged`, no pane filter) ✓ — the two M0b-review prerequisites for the sidebar. Registry populated in `spawn_agent`, removed in `teardown_agent` ✓.
- **Placeholder scan:** every step has complete code; no TBD.
- **Type consistency:** `AgentInfo{pane,project,task,state}` identical in proto (Task 1) and `list_agents()` (Task 2) and the control test (Task 3); `ListAgents`/`AgentList` used identically; `handle_control` consumes `list_agents()` + `subscribe_attention()` as defined.
- **Regression guard:** Task 1 Step 3 adds the `AgentList` arm to `pump()`'s match (the M0b-style break) and Step 5 builds the whole workspace to catch it.
- **M0a/M0b preservation:** new proto variants are additive; `handle_conn`'s existing attach path is unchanged except an added `ListAgents` branch that only fires on that message; `Daemon::new()`/`spawn_pane`/`spawn_agent` signatures unchanged (the `agents` insert is internal).
