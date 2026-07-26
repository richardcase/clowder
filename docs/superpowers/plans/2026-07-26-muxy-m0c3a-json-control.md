# muxy M0c-3a — JSON Control Channel + Spawn Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the daemon a **JSON-lines control socket** (so the M0c-3b SwiftUI app reads the agent feed with `Codable` and spawns agents — no FFI, no postcard-in-Swift), plus a `muxy spawn` CLI.

**Architecture:** Pure Rust, TDD, on M0c-1 (`list_agents`, `subscribe_attention`) + reaper (`subscribe_removed`, `AttentionState::Exited`). `muxy-proto::control` defines JSON-tagged `ControlRequest`/`ControlEvent`. The daemon serves a **third** Unix socket (`serve_control_json`) with newline-delimited JSON: snapshot `AgentList` on connect, then stream `AttentionChanged`/`AgentRemoved`, and handle `ListAgents`/`SpawnAgent`. A `muxy spawn` subcommand drives it. The render path (libghostty → `muxy attach`) and the postcard client/hook sockets are untouched.

**Tech Stack:** Rust stable, tokio (io split + `BufReader::lines`), serde + serde_json, muxy-proto, muxy-daemon.

## Global Constraints

- **Cargo is not on PATH.** Prefix every cargo command: `source "$HOME/.cargo/env" && cargo ...`
- **Rust stable only**; crates prefixed `muxy-` under `crates/`.
- **Do not break M0a/M0b/M0c-1/reaper.** All existing tests (currently **32**) stay green; run `source "$HOME/.cargo/env" && cargo test` (whole workspace) after every task.
- **No `Daemon` constructor change** — the control socket path is a `main.rs` concern (not stored on `Daemon`), so `new_with` callers are untouched.
- **JSON shape is a contract for Swift (M0c-3b):** enums use `#[serde(tag = "type", rename_all = "camelCase")]`; `PaneId` (newtype) serializes as a bare number; `AttentionState` serializes as its variant name (`"NeedsInput"`, `"Exited"`, …).
- **Explicit `git add`** of changed files (+ `Cargo.lock` if it changed); never `git add .`.
- **Deferred to M0c-3b:** the SwiftUI app (sidebar, libghostty surface view, GUI spawn button).

---

### Task 1: `muxy-proto::control` — JSON `ControlRequest`/`ControlEvent`

**Files:**
- Create: `crates/muxy-proto/src/control.rs`
- Modify: `crates/muxy-proto/src/lib.rs` (add `pub mod control;` + re-exports)
- Modify: `crates/muxy-proto/Cargo.toml` (add `serde_json`)
- Test: inline `#[cfg(test)]` in `control.rs`

**Interfaces:**
- Consumes: `AgentInfo`, `AttentionState`, `PaneId`.
- Produces:
  - `pub enum ControlRequest { ListAgents, SpawnAgent { project: String, task: String, adapter: String } }`
  - `pub enum ControlEvent { AgentList { agents: Vec<AgentInfo> }, AttentionChanged { pane: PaneId, state: AttentionState }, AgentRemoved { pane: PaneId }, AgentSpawned { pane: PaneId }, Error { message: String } }`
  - Both `Debug, Clone, PartialEq, Serialize, Deserialize` with `#[serde(tag = "type", rename_all = "camelCase")]`.

- [ ] **Step 1: Add `serde_json` to `crates/muxy-proto/Cargo.toml`**

```toml
serde_json = "1"
```
(under `[dependencies]`.)

- [ ] **Step 2: Write `crates/muxy-proto/src/control.rs`**

```rust
use crate::{AgentInfo, AttentionState, PaneId};
use serde::{Deserialize, Serialize};

/// GUI/CLI → daemon, over the JSON-lines control socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ControlRequest {
    ListAgents,
    SpawnAgent { project: String, task: String, adapter: String },
}

/// daemon → GUI/CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ControlEvent {
    AgentList { agents: Vec<AgentInfo> },
    AttentionChanged { pane: PaneId, state: AttentionState },
    AgentRemoved { pane: PaneId },
    AgentSpawned { pane: PaneId },
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_agents_request_json_shape() {
        let r = ControlRequest::ListAgents;
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"type":"listAgents"}"#);
        assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap());
    }

    #[test]
    fn spawn_agent_request_json_shape() {
        let r = ControlRequest::SpawnAgent {
            project: "/p".into(),
            task: "t".into(),
            adapter: "shell".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""type":"spawnAgent""#), "{s}");
        assert!(s.contains(r#""adapter":"shell""#), "{s}");
        assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap());
    }

    #[test]
    fn agent_spawned_event_pane_is_bare_number() {
        let e = ControlEvent::AgentSpawned { pane: PaneId(7) };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains(r#""type":"agentSpawned""#), "{s}");
        assert!(s.contains(r#""pane":7"#), "PaneId must serialize as a bare number: {s}");
        assert_eq!(e, serde_json::from_str::<ControlEvent>(&s).unwrap());
    }

    #[test]
    fn attention_changed_event_roundtrips() {
        let e = ControlEvent::AttentionChanged { pane: PaneId(3), state: AttentionState::Exited };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains(r#""state":"Exited""#), "{s}");
        assert_eq!(e, serde_json::from_str::<ControlEvent>(&s).unwrap());
    }
}
```

- [ ] **Step 3: Wire into `crates/muxy-proto/src/lib.rs`**

Add (keep existing modules/exports):
```rust
pub mod control;
pub use control::{ControlEvent, ControlRequest};
```

- [ ] **Step 4: Run tests**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-proto control` then `source "$HOME/.cargo/env" && cargo test`.
Expected: PASS (4 new; whole workspace green).

- [ ] **Step 5: Commit**

```bash
git add crates/muxy-proto/src/control.rs crates/muxy-proto/src/lib.rs crates/muxy-proto/Cargo.toml Cargo.lock
git commit -m "feat(proto): add JSON control ControlRequest/ControlEvent"
```

---

### Task 2: `muxy-daemon` — `serve_control_json` + spawn handling + control socket in the binary

**Files:**
- Create: `crates/muxy-daemon/src/control_json.rs`
- Modify: `crates/muxy-daemon/src/lib.rs` (add `pub mod control_json;`)
- Modify: `crates/muxy-daemon/src/main.rs` (bind + serve the control socket)
- Test: inline `#[cfg(test)]` in `control_json.rs`

**Interfaces:**
- Consumes: `Daemon::{list_agents, subscribe_attention, subscribe_removed, spawn_agent}`; `ClaudeAdapter`, `SyntheticAdapter`, `PaneCommand`; `ControlRequest`, `ControlEvent`, `PaneId`.
- Produces on `Daemon`:
  - `pub async fn serve_control_json(self: Arc<Self>, listener: UnixListener) -> Result<()>`
  - `pub async fn handle_control_json<S: AsyncRead + AsyncWrite + Unpin + Send>(self: Arc<Self>, stream: S) -> Result<()>`
  - private `fn spawn_from_control(self: &Arc<Self>, project: &str, task: &str, adapter: &str) -> Result<PaneId>`.

- [ ] **Step 1: Write `crates/muxy-daemon/src/control_json.rs`**

```rust
use crate::server::Daemon;
use crate::{ClaudeAdapter, PaneCommand, SyntheticAdapter};
use anyhow::{anyhow, Result};
use muxy_proto::{ControlEvent, ControlRequest, PaneId};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

async fn write_event<W: AsyncWrite + Unpin>(wr: &mut W, ev: &ControlEvent) -> Result<()> {
    let mut s = serde_json::to_string(ev)?;
    s.push('\n');
    wr.write_all(s.as_bytes()).await?;
    wr.flush().await?;
    Ok(())
}

impl Daemon {
    /// Accept loop for the JSON-lines control socket.
    pub async fn serve_control_json(self: Arc<Self>, listener: UnixListener) -> Result<()> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                let _ = me.handle_control_json(stream).await;
            });
        }
    }

    /// One JSON-lines control connection: snapshot AgentList, then stream events
    /// and handle ListAgents/SpawnAgent requests (newline-delimited JSON both ways).
    pub async fn handle_control_json<S>(self: Arc<Self>, stream: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let (rd, mut wr) = tokio::io::split(stream);
        let mut lines = BufReader::new(rd).lines();
        let mut att_rx = self.subscribe_attention();
        let mut removed_rx = self.subscribe_removed();

        write_event(&mut wr, &ControlEvent::AgentList { agents: self.list_agents() }).await?;

        loop {
            tokio::select! {
                line = lines.next_line() => {
                    match line? {
                        Some(l) if l.trim().is_empty() => continue,
                        Some(l) => {
                            let ev = match serde_json::from_str::<ControlRequest>(&l) {
                                Ok(ControlRequest::ListAgents) =>
                                    ControlEvent::AgentList { agents: self.list_agents() },
                                Ok(ControlRequest::SpawnAgent { project, task, adapter }) =>
                                    match self.spawn_from_control(&project, &task, &adapter) {
                                        Ok(pane) => ControlEvent::AgentSpawned { pane },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Err(e) => ControlEvent::Error { message: format!("bad request: {e}") },
                            };
                            write_event(&mut wr, &ev).await?;
                        }
                        None => break, // client disconnected
                    }
                }
                att = att_rx.recv() => {
                    match att {
                        Ok((pane, state)) =>
                            write_event(&mut wr, &ControlEvent::AttentionChanged { pane, state }).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                removed = removed_rx.recv() => {
                    match removed {
                        Ok(pane) => write_event(&mut wr, &ControlEvent::AgentRemoved { pane }).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            }
        }
        Ok(())
    }

    fn spawn_from_control(self: &Arc<Self>, project: &str, task: &str, adapter: &str) -> Result<PaneId> {
        let project_path = Path::new(project);
        match adapter {
            "claude" => self.spawn_agent(project_path, &ClaudeAdapter, task),
            "shell" => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
                let a = SyntheticAdapter {
                    command: PaneCommand { program: shell, args: vec![], cwd: None, env: vec![] },
                };
                self.spawn_agent(project_path, &a, task)
            }
            other => Err(anyhow!("unknown adapter: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeNotifier;
    use muxy_proto::AttentionState;
    use muxy_workspace::GitWorktreeDriver;
    use std::process::Command as PCommand;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let run = |args: &[&str]| {
            assert!(PCommand::new("git").arg("-C").arg(p).args(args).status().unwrap().success());
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(p.join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        dir
    }

    #[tokio::test]
    async fn control_json_lists_spawns_and_streams() {
        let repo = init_repo();
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cjson.sock"),
        ));

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });

        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();

        // Initial snapshot: empty AgentList.
        let first = clines.next_line().await.unwrap().unwrap();
        assert!(first.contains(r#""type":"agentList""#), "{first}");

        // Spawn a shell agent (build the request via the typed enum to escape the path safely).
        let req = ControlRequest::SpawnAgent {
            project: repo.path().to_string_lossy().to_string(),
            task: "demo".into(),
            adapter: "shell".into(),
        };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        // Read events until AgentSpawned.
        let pane = loop {
            let l = clines.next_line().await.unwrap().unwrap();
            if let Ok(ControlEvent::AgentSpawned { pane }) = serde_json::from_str::<ControlEvent>(&l) {
                break pane;
            }
        };

        // listAgents now includes it.
        cwr.write_all(b"{\"type\":\"listAgents\"}\n").await.unwrap();
        let listed = loop {
            let l = clines.next_line().await.unwrap().unwrap();
            if let Ok(ControlEvent::AgentList { agents }) = serde_json::from_str::<ControlEvent>(&l) {
                if !agents.is_empty() { break agents; }
            }
        };
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].pane, pane);
        assert_eq!(listed[0].task, "demo");

        // An attention change streams as JSON.
        daemon.set_attention(pane, AttentionState::NeedsInput);
        let mut saw = false;
        for _ in 0..40 {
            if let Ok(Ok(Some(l))) =
                tokio::time::timeout(Duration::from_millis(50), clines.next_line()).await
            {
                if let Ok(ControlEvent::AttentionChanged { pane: p, state }) =
                    serde_json::from_str::<ControlEvent>(&l)
                {
                    if p == pane && state == AttentionState::NeedsInput { saw = true; break; }
                }
            }
        }
        assert!(saw, "did not receive attentionChanged over the control JSON stream");

        daemon.teardown_agent(pane).unwrap();
    }

    #[tokio::test]
    async fn control_json_spawn_unknown_adapter_errors() {
        let repo = init_repo();
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-cjson2.sock"),
        ));
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });

        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();
        let _snapshot = clines.next_line().await.unwrap().unwrap();

        let req = ControlRequest::SpawnAgent {
            project: repo.path().to_string_lossy().to_string(),
            task: "x".into(),
            adapter: "nope".into(),
        };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        let l = clines.next_line().await.unwrap().unwrap();
        assert!(l.contains(r#""type":"error""#), "expected error event: {l}");
        assert!(daemon.list_agents().is_empty());
    }
}
```

- [ ] **Step 2: Add the `muxy-workspace` dev-dep if missing to `crates/muxy-daemon/Cargo.toml`**

The tests use `muxy_workspace::GitWorktreeDriver` + `tempfile` — both are already `[dev-dependencies]` from earlier milestones. If `cargo test` reports either unresolved, add:
```toml
[dev-dependencies]
tempfile = "3"
muxy-workspace = { path = "../muxy-workspace" }
```

- [ ] **Step 3: Wire the module in `crates/muxy-daemon/src/lib.rs`**

Add `pub mod control_json;` (keep existing modules).

- [ ] **Step 4: Bind + serve the control socket in `crates/muxy-daemon/src/main.rs`**

Add, alongside the client + hook sockets (keep those):
```rust
    let control_path =
        std::env::var("MUXY_CONTROL_SOCK").unwrap_or_else(|_| "/tmp/muxy-control.sock".into());
    let _ = std::fs::remove_file(&control_path);
    let control_listener = UnixListener::bind(&control_path)?;
    let control = daemon.clone();
    tokio::spawn(async move { let _ = control.serve_control_json(control_listener).await; });
```
and extend the startup log line to mention `control={control_path}`.

- [ ] **Step 5: Run tests + build**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon control_json`, then `source "$HOME/.cargo/env" && cargo test`, then `source "$HOME/.cargo/env" && cargo build`.
Expected: 2 new tests pass; whole workspace green; binary builds.

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-daemon/src/control_json.rs crates/muxy-daemon/src/lib.rs crates/muxy-daemon/src/main.rs crates/muxy-daemon/Cargo.toml Cargo.lock
git commit -m "feat(daemon): JSON control socket (list/spawn/stream) + binary wiring"
```

---

### Task 3: `muxy-client` — `muxy spawn` CLI

**Files:**
- Modify: `crates/muxy-client/src/lib.rs` (add `spawn_via_control`)
- Modify: `crates/muxy-client/src/main.rs` (subcommand dispatch: `spawn` / `attach` / legacy)
- Modify: `crates/muxy-client/Cargo.toml` (add `serde_json`; ensure dev-deps for the test)
- Test: inline `#[cfg(test)]` in `lib.rs`

**Interfaces:**
- Consumes: `ControlRequest`, `ControlEvent`, `PaneId` (proto).
- Produces: `pub async fn spawn_via_control(control_sock: &Path, project: &str, task: &str, adapter: &str) -> anyhow::Result<PaneId>`.

- [ ] **Step 1: Add `serde_json` to `crates/muxy-client/Cargo.toml`**

```toml
serde_json = "1"
```
(under `[dependencies]`; `muxy-proto`, `tokio`, `anyhow` are already deps.)

- [ ] **Step 2: Add `spawn_via_control` to `crates/muxy-client/src/lib.rs`**

```rust
use muxy_proto::{ControlEvent, ControlRequest, PaneId};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Connect the JSON control socket, request a spawn, and return the new pane id.
pub async fn spawn_via_control(
    control_sock: &Path,
    project: &str,
    task: &str,
    adapter: &str,
) -> anyhow::Result<PaneId> {
    let stream = UnixStream::connect(control_sock).await?;
    let (rd, mut wr) = tokio::io::split(stream);
    let mut lines = BufReader::new(rd).lines();

    let req = ControlRequest::SpawnAgent {
        project: project.to_string(),
        task: task.to_string(),
        adapter: adapter.to_string(),
    };
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    wr.write_all(line.as_bytes()).await?;

    // Skip the initial AgentList / any streamed events until the spawn result.
    loop {
        match lines.next_line().await? {
            Some(l) => match serde_json::from_str::<ControlEvent>(&l) {
                Ok(ControlEvent::AgentSpawned { pane }) => return Ok(pane),
                Ok(ControlEvent::Error { message }) => return Err(anyhow::anyhow!(message)),
                Ok(_) => continue, // AgentList / AttentionChanged / AgentRemoved
                Err(_) => continue, // ignore unparyseable lines defensively
            },
            None => return Err(anyhow::anyhow!("control socket closed before spawn result")),
        }
    }
}
```
(Fix the typo `unparyseable` → `unparseable` in the comment.)

- [ ] **Step 3: Refactor `crates/muxy-client/src/main.rs` for subcommand dispatch**

Extract the existing pump/raw-mode flow into an `attach(pane_id: u64) -> Result<()>` helper (moving the current body of `main` into it, using `pane_id` instead of parsing args inside). Then:

```rust
use anyhow::{anyhow, Result};
use muxy_client::{attach, spawn_via_control};
use std::path::Path;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("spawn") => {
            let project = args.get(2).ok_or_else(|| anyhow!("usage: muxy spawn <project> <task> [adapter]"))?;
            let task = args.get(3).ok_or_else(|| anyhow!("usage: muxy spawn <project> <task> [adapter]"))?;
            let adapter = args.get(4).map(|s| s.as_str()).unwrap_or("claude");
            let sock = std::env::var("MUXY_CONTROL_SOCK").unwrap_or_else(|_| "/tmp/muxy-control.sock".into());
            let pane = spawn_via_control(Path::new(&sock), project, task, adapter).await?;
            println!("{}", pane.0);
            Ok(())
        }
        Some("attach") => {
            let pane = args.get(2).and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow!("usage: muxy attach <pane-id>"))?;
            attach(pane).await
        }
        // Legacy: `muxy <pane-id>` still attaches.
        Some(other) if other.parse::<u64>().is_ok() => attach(other.parse().unwrap()).await,
        _ => Err(anyhow!("usage: muxy <spawn|attach> ...")),
    }
}
```
Make `attach` and `spawn_via_control` `pub` in `lib.rs`. `attach` must contain the existing `UnixStream::connect(MUXY_SOCK)` + `RawModeGuard` + `pump(...)` logic, taking the `pane` id as its argument (build `PaneId(pane)` for `pump`).

- [ ] **Step 4: Ensure the test's deps in `crates/muxy-client/Cargo.toml`**

The test spins up an in-process daemon + control socket. `[dev-dependencies]` needs `muxy-daemon`, `muxy-workspace`, `tempfile`, `tokio` (add whichever are missing; `muxy-daemon` is already a dev-dep from M0a):
```toml
[dev-dependencies]
muxy-daemon = { path = "../muxy-daemon" }
muxy-workspace = { path = "../muxy-workspace" }
tempfile = "3"
```

- [ ] **Step 5: Write the failing test (append to `crates/muxy-client/src/lib.rs`'s `#[cfg(test)] mod tests`)**

```rust
#[tokio::test]
async fn spawn_via_control_returns_pane_id() {
    use muxy_daemon::server::Daemon;
    use muxy_daemon::FakeNotifier;
    use muxy_workspace::GitWorktreeDriver;
    use std::process::Command as PCommand;
    use std::sync::Arc;

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

    // daemon + control socket on a temp path
    let sockdir = tempfile::tempdir().unwrap();
    let sock = sockdir.path().join("control.sock");
    let daemon = Arc::new(Daemon::new_with(
        Arc::new(GitWorktreeDriver),
        Arc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-cli.sock"),
    ));
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let d = daemon.clone();
    tokio::spawn(async move { let _ = d.serve_control_json(listener).await; });

    let pane = spawn_via_control(&sock, &repo.path().to_string_lossy(), "demo", "shell")
        .await
        .unwrap();

    let agents = daemon.list_agents();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].pane, pane);
    assert_eq!(agents[0].task, "demo");

    daemon.teardown_agent(pane).unwrap();
}
```

- [ ] **Step 6: Run tests + build**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-client spawn_via_control`, then `source "$HOME/.cargo/env" && cargo test`, then `source "$HOME/.cargo/env" && cargo build`.
Expected: new test passes; whole workspace green; the `muxy` binary builds with the new subcommands.

- [ ] **Step 7: Manual smoke (optional)**

```bash
cargo run -p muxy-daemon &                 # binds client/hook/control sockets
cargo run -p muxy-client -- spawn "$(pwd)" demo shell   # prints a pane id
cargo run -p muxy-client -- attach <pane>  # renders the shell agent; Ctrl-C to detach; it survives
```

- [ ] **Step 8: Commit**

```bash
git add crates/muxy-client/src/lib.rs crates/muxy-client/src/main.rs crates/muxy-client/Cargo.toml Cargo.lock
git commit -m "feat(client): muxy spawn subcommand via the JSON control socket"
```

---

## Self-Review

- **Spec coverage:** JSON `ControlRequest`/`ControlEvent` with the Swift-friendly tagged shape (Task 1) ✓; a dedicated JSON-lines control socket serving snapshot + stream + list/spawn (Task 2) ✓; adapter mapping claude/shell with unknown→Error (Task 2) ✓; `muxy spawn` CLI (Task 3) ✓; render path + postcard sockets untouched ✓; no `Daemon` constructor change ✓.
- **Placeholder scan:** every step has complete code; no TBD.
- **Type consistency:** `ControlRequest`/`ControlEvent` defined once (Task 1) and consumed identically in the daemon handler (Task 2) and the CLI (Task 3); `spawn_from_control` uses `spawn_agent(self: &Arc<Self>)` correctly (called from `handle_control_json`'s `Arc<Self>`); `PaneId` serializes as a bare number (asserted in Task 1). `spawn_via_control` signature matches its call in `main.rs`.
- **Regression guard:** new proto types are standalone (no variant added to `ClientToDaemon`/`DaemonToClient`), so no exhaustive-match breaks; each task runs the whole workspace suite.
- **Testability:** the `"shell"` adapter makes spawn deterministically testable (no `claude` binary needed); all three tasks are covered by Rust tests over in-memory duplex / in-process sockets.
