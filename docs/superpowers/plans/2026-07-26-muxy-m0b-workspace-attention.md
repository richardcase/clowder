# muxy M0b — Workspace Provisioning + Hook Attention Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn an M0a "pane" into an "agent": the daemon provisions an isolated git worktree per agent, spawns a coding agent there with attention hooks injected, routes hook-fired events into per-pane attention state, surfaces them via a new `AttentionChanged` protocol message + an OS notification, and tears the worktree down on request. Headless; proven end-to-end with a synthetic agent.

**Architecture:** Two new crates — `muxy-workspace` (a `WorkspaceDriver` trait + `GitWorktreeDriver` that shells out to `git worktree`) and `muxy-hook` (the tiny relay binary agents call). `muxy-proto` gains `HookEvent`/`HookKind`/`AttentionState` and a `DaemonToClient::AttentionChanged` variant. `muxy-daemon` gains a `Notifier` trait (`notify.rs`), an `AgentAdapter` trait + `spawn_agent` (`agent.rs`), and a hook-socket receiver + per-pane attention state (`attention.rs`), plus pane-exit wiring that also fixes the M0a child-exit hang, and `teardown_agent`.

**Tech Stack:** Rust stable, tokio, tokio-util, portable-pty, serde/postcard, notify-rust, serde_json, anyhow; tests use `tempfile` + `#[tokio::test]` + in-memory `tokio::io::duplex`.

## Global Constraints

- **Cargo is not on PATH.** Every cargo command (impl + tests) MUST be prefixed: `source "$HOME/.cargo/env" && cargo ...`
- **Rust stable only**; no nightly. All crates prefixed `muxy-` under `crates/`; the workspace root uses `members = ["crates/*"]` (a new crate dir is auto-included — no root edits).
- **Daemon owns every fd**; clients and `muxy-hook` exchange only framed `muxy-proto` messages over `MsgStream`.
- **Agent identity** is `MUXY_AGENT_ID` = the agent's `PaneId`, pinned in the environment at spawn — never cwd/session-id.
- **Do not break M0a.** `Daemon::new()` must keep its no-arg signature (existing M0a tests call it); new dependencies are added via a `Daemon::new_with(...)` constructor. `Pane`, `PaneCommand`, `spawn_pane`, `pump()`, and all existing tests stay green. Run `source "$HOME/.cargo/env" && cargo test` after every task — the M0a suite (10 tests) plus new tests must all pass.
- **Explicit `git add`** of changed files per commit; never `git add .` (do not commit `target/`).
- **Final `Daemon` shape** (built up across Tasks 5–6; all tasks target this exact shape):
  ```rust
  pub struct Daemon {
      panes: Arc<Mutex<HashMap<PaneId, Arc<Pane>>>>,
      next_id: AtomicU64,
      attention: Arc<Mutex<HashMap<PaneId, AttentionState>>>,
      attention_tx: tokio::sync::broadcast::Sender<(PaneId, AttentionState)>,
      workspaces: Arc<Mutex<HashMap<PaneId, muxy_workspace::Workspace>>>,
      driver: Arc<dyn muxy_workspace::WorkspaceDriver>,
      notifier: Arc<dyn Notifier>,
      hook_sock: std::path::PathBuf,
  }
  ```

---

### Task 1: `muxy-proto` — hook + attention message types

**Files:**
- Modify: `crates/muxy-proto/src/message.rs` (add types + a variant; keep existing)
- Test: inline `#[cfg(test)]` in `message.rs`

**Interfaces:**
- Consumes: existing `PaneId`, `DaemonToClient`.
- Produces:
  - `pub enum HookKind { Notification, Stop }` — `Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize`.
  - `pub struct HookEvent { pub agent_id: PaneId, pub kind: HookKind }` — `Clone, Debug, PartialEq, Serialize, Deserialize`.
  - `pub enum AttentionState { Idle, Working, NeedsInput, Completed }` — `Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize`.
  - New variant `DaemonToClient::AttentionChanged { pane: PaneId, state: AttentionState }`.

- [ ] **Step 1: Add the types and the variant to `crates/muxy-proto/src/message.rs`**

Add after the existing `DaemonToClient` enum, and add the new variant inside `DaemonToClient`:

```rust
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookKind {
    Notification,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HookEvent {
    pub agent_id: PaneId,
    pub kind: HookKind,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttentionState {
    Idle,
    Working,
    NeedsInput,
    Completed,
}
```

And extend `DaemonToClient` (keep all existing variants, add one):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DaemonToClient {
    Attached { pane: PaneId, cols: u16, rows: u16 },
    Output { pane: PaneId, bytes: Vec<u8> },
    PaneExited { pane: PaneId, code: Option<i32> },
    AttentionChanged { pane: PaneId, state: AttentionState },
}
```

- [ ] **Step 2: Update `crates/muxy-proto/src/lib.rs` re-exports**

```rust
pub use message::{
    AttentionState, ClientToDaemon, DaemonToClient, HookEvent, HookKind, PaneId,
};
```
(Keep the existing `pub mod message;`, `pub mod transport;`, and `pub use transport::{MsgStream, Transport};`.)

- [ ] **Step 3: Write the failing tests (append to `message.rs`'s `#[cfg(test)] mod tests`)**

```rust
#[test]
fn hook_event_roundtrips() {
    let e = HookEvent { agent_id: PaneId(3), kind: HookKind::Notification };
    let bytes = postcard::to_stdvec(&e).unwrap();
    assert_eq!(e, postcard::from_bytes::<HookEvent>(&bytes).unwrap());
}

#[test]
fn attention_changed_roundtrips() {
    let m = DaemonToClient::AttentionChanged { pane: PaneId(9), state: AttentionState::NeedsInput };
    let bytes = postcard::to_stdvec(&m).unwrap();
    assert_eq!(m, postcard::from_bytes::<DaemonToClient>(&bytes).unwrap());
}
```

- [ ] **Step 4: Run tests**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-proto`
Expected: PASS (existing 4 + 2 new = 6).

- [ ] **Step 5: Commit**

```bash
git add crates/muxy-proto/src/message.rs crates/muxy-proto/src/lib.rs
git commit -m "feat(proto): add HookEvent/HookKind/AttentionState + AttentionChanged"
```

---

### Task 2: `muxy-workspace` crate — WorkspaceDriver + GitWorktreeDriver

**Files:**
- Create: `crates/muxy-workspace/Cargo.toml`
- Create: `crates/muxy-workspace/src/lib.rs`
- Test: inline `#[cfg(test)]` in `lib.rs`

**Interfaces:**
- Consumes: nothing from other new crates.
- Produces:
  - `pub struct Workspace { pub path: PathBuf, pub branch: String }` — `Clone, Debug`.
  - `pub trait WorkspaceDriver: Send + Sync { fn provision(&self, project: &Path, name: &str) -> anyhow::Result<Workspace>; fn teardown(&self, ws: &Workspace) -> anyhow::Result<()>; }`
  - `pub struct GitWorktreeDriver;` implementing it (shells out to `git`).

- [ ] **Step 1: Create `crates/muxy-workspace/Cargo.toml`**

```toml
[package]
name = "muxy-workspace"
version = "0.0.0"
edition = "2021"

[dependencies]
anyhow = { workspace = true }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write the failing test + implementation in `crates/muxy-workspace/src/lib.rs`**

```rust
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct Workspace {
    pub path: PathBuf,
    pub branch: String,
}

pub trait WorkspaceDriver: Send + Sync {
    /// Create an isolated working copy on a fresh branch under `project`'s repo.
    fn provision(&self, project: &Path, name: &str) -> Result<Workspace>;
    /// Remove the working copy (best-effort prune of stale registrations).
    fn teardown(&self, ws: &Workspace) -> Result<()>;
}

pub struct GitWorktreeDriver;

impl GitWorktreeDriver {
    fn git(project: &Path, args: &[&str]) -> Result<()> {
        let out = Command::new("git")
            .arg("-C")
            .arg(project)
            .args(args)
            .output()
            .with_context(|| format!("failed to run git {args:?}"))?;
        if !out.status.success() {
            bail!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }
}

impl WorkspaceDriver for GitWorktreeDriver {
    fn provision(&self, project: &Path, name: &str) -> Result<Workspace> {
        let branch = format!("muxy/{name}");
        let path = project.join(".muxy").join("worktrees").join(name);
        let path_str = path.to_string_lossy().to_string();
        // `git worktree add <path> -b <branch>` creates the dir + a new branch off HEAD.
        Self::git(project, &["worktree", "add", &path_str, "-b", &branch])?;
        Ok(Workspace { path, branch })
    }

    fn teardown(&self, ws: &Workspace) -> Result<()> {
        // project root is two levels up from .muxy/worktrees/<name>? Use git from the worktree itself.
        let path_str = ws.path.to_string_lossy().to_string();
        // `git -C <worktree> worktree remove <worktree> --force` works from inside the worktree.
        Self::git(&ws.path, &["worktree", "remove", &path_str, "--force"])?;
        // prune stale registrations from the main repo (best-effort).
        let _ = Command::new("git")
            .arg("-C")
            .arg(&ws.path)
            .args(["worktree", "prune"])
            .output();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a temp git repo with one commit so `worktree add` has a valid HEAD.
    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let run = |args: &[&str]| {
            let ok = Command::new("git").arg("-C").arg(p).args(args).status().unwrap().success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.test"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(p.join("README.md"), b"hi").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn provision_creates_isolated_worktree_on_new_branch() {
        let repo = init_repo();
        let driver = GitWorktreeDriver;
        let ws = driver.provision(repo.path(), "task-a").unwrap();

        assert!(ws.path.is_dir(), "worktree dir not created");
        assert_eq!(ws.branch, "muxy/task-a");
        // README from the initial commit is present in the isolated copy.
        assert!(ws.path.join("README.md").is_file());
        // A file created only in the worktree is NOT in the main working copy.
        std::fs::write(ws.path.join("only_here.txt"), b"x").unwrap();
        assert!(!repo.path().join("only_here.txt").exists());
    }

    #[test]
    fn teardown_removes_worktree() {
        let repo = init_repo();
        let driver = GitWorktreeDriver;
        let ws = driver.provision(repo.path(), "task-b").unwrap();
        assert!(ws.path.is_dir());
        driver.teardown(&ws).unwrap();
        assert!(!ws.path.exists(), "worktree dir still present after teardown");
    }
}
```

- [ ] **Step 3: Run tests**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-workspace`
Expected: PASS (2 tests). Requires `git` on PATH (present on dev machines).

- [ ] **Step 4: Commit**

```bash
git add crates/muxy-workspace
git commit -m "feat(workspace): add WorkspaceDriver trait + GitWorktreeDriver"
```

---

### Task 3: `muxy-hook` crate — the relay binary

**Files:**
- Create: `crates/muxy-hook/Cargo.toml`
- Create: `crates/muxy-hook/src/lib.rs`
- Create: `crates/muxy-hook/src/main.rs`
- Test: inline `#[cfg(test)]` in `lib.rs`

**Interfaces:**
- Consumes: `HookEvent`, `HookKind`, `PaneId`, `MsgStream` from `muxy-proto`.
- Produces:
  - `pub async fn send_hook(sock: &Path, event: HookEvent) -> anyhow::Result<()>` — connect the hook socket, send one framed `HookEvent`, done.

- [ ] **Step 1: Create `crates/muxy-hook/Cargo.toml`**

```toml
[package]
name = "muxy-hook"
version = "0.0.0"
edition = "2021"

[dependencies]
muxy-proto = { path = "../muxy-proto" }
tokio = { workspace = true }
anyhow = { workspace = true }

[[bin]]
name = "muxy-hook"
path = "src/main.rs"
```

- [ ] **Step 2: Write the failing test + implementation in `crates/muxy-hook/src/lib.rs`**

```rust
use anyhow::Result;
use muxy_proto::{HookEvent, MsgStream};
use std::path::Path;
use tokio::net::UnixStream;

/// Connect to the daemon's hook socket and send exactly one HookEvent.
pub async fn send_hook(sock: &Path, event: HookEvent) -> Result<()> {
    let stream = UnixStream::connect(sock).await?;
    let mut msgs = MsgStream::new(stream);
    msgs.send(&event).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxy_proto::{HookKind, PaneId};

    #[tokio::test]
    async fn send_hook_delivers_one_event() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("hook.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();

        let event = HookEvent { agent_id: PaneId(42), kind: HookKind::Stop };
        let event2 = event.clone();
        let sock2 = sock.clone();
        let client = tokio::spawn(async move { send_hook(&sock2, event2).await.unwrap() });

        let (stream, _) = listener.accept().await.unwrap();
        let mut msgs = MsgStream::new(stream);
        let got: HookEvent = msgs.recv().await.unwrap().unwrap();
        assert_eq!(got, event);
        client.await.unwrap();
    }
}
```

- [ ] **Step 3: Add `tempfile` dev-dependency to `crates/muxy-hook/Cargo.toml`**

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 4: Write `crates/muxy-hook/src/main.rs`**

```rust
use anyhow::{anyhow, Result};
use muxy_hook::send_hook;
use muxy_proto::{HookEvent, HookKind, PaneId};
use std::io::Read;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    // Usage: muxy-hook --event <notification|stop>
    let mut kind = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--event" {
            kind = match args.next().as_deref() {
                Some("notification") => Some(HookKind::Notification),
                Some("stop") => Some(HookKind::Stop),
                other => return Err(anyhow!("unknown --event value: {other:?}")),
            };
        }
    }
    let kind = kind.ok_or_else(|| anyhow!("--event <notification|stop> is required"))?;

    // The tool pipes its hook JSON on stdin; M0b does not need it — drain and discard.
    let mut buf = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut buf);

    let agent_id: u64 = std::env::var("MUXY_AGENT_ID")
        .map_err(|_| anyhow!("MUXY_AGENT_ID not set"))?
        .parse()
        .map_err(|_| anyhow!("MUXY_AGENT_ID not a u64"))?;
    let sock = PathBuf::from(std::env::var("MUXY_HOOK_SOCK").map_err(|_| anyhow!("MUXY_HOOK_SOCK not set"))?);

    send_hook(&sock, HookEvent { agent_id: PaneId(agent_id), kind }).await
}
```

- [ ] **Step 5: Run tests + build the binary**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-hook && source "$HOME/.cargo/env" && cargo build -p muxy-hook`
Expected: test PASS (1); binary builds.

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-hook
git commit -m "feat(hook): add muxy-hook relay (send_hook + binary)"
```

---

### Task 4: `muxy-daemon` — Notifier trait + OsNotifier + FakeNotifier

**Files:**
- Create: `crates/muxy-daemon/src/notify.rs`
- Modify: `crates/muxy-daemon/src/lib.rs` (add `pub mod notify;` + re-export)
- Modify: `crates/muxy-daemon/Cargo.toml` (add `notify-rust`)
- Test: inline `#[cfg(test)]` in `notify.rs`

**Interfaces:**
- Consumes: `PaneId`, `AttentionState` from `muxy-proto`.
- Produces:
  - `pub trait Notifier: Send + Sync { fn notify(&self, pane: PaneId, state: AttentionState); }`
  - `pub struct OsNotifier;` (real desktop notification via `notify-rust`).
  - `pub struct FakeNotifier { calls: Mutex<Vec<(PaneId, AttentionState)>> }` with `new()` and `calls() -> Vec<(PaneId, AttentionState)>` (test double).

- [ ] **Step 1: Add `notify-rust` to `crates/muxy-daemon/Cargo.toml`**

```toml
notify-rust = "4"
```
(under `[dependencies]`, alongside the existing deps.)

- [ ] **Step 2: Write the failing test + implementation in `crates/muxy-daemon/src/notify.rs`**

```rust
use muxy_proto::{AttentionState, PaneId};
use std::sync::Mutex;

pub trait Notifier: Send + Sync {
    fn notify(&self, pane: PaneId, state: AttentionState);
}

/// Real desktop notifications. Only fires for states worth interrupting the user.
pub struct OsNotifier;

impl Notifier for OsNotifier {
    fn notify(&self, pane: PaneId, state: AttentionState) {
        let body = match state {
            AttentionState::NeedsInput => "needs your input",
            AttentionState::Completed => "finished",
            AttentionState::Idle | AttentionState::Working => return, // not interrupt-worthy
        };
        let _ = notify_rust::Notification::new()
            .summary(&format!("muxy · agent {}", pane.0))
            .body(body)
            .show();
    }
}

/// Test double: records calls instead of showing banners.
pub struct FakeNotifier {
    calls: Mutex<Vec<(PaneId, AttentionState)>>,
}

impl FakeNotifier {
    pub fn new() -> Self {
        Self { calls: Mutex::new(Vec::new()) }
    }
    pub fn calls(&self) -> Vec<(PaneId, AttentionState)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Notifier for FakeNotifier {
    fn notify(&self, pane: PaneId, state: AttentionState) {
        self.calls.lock().unwrap().push((pane, state));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_notifier_records_calls() {
        let n = FakeNotifier::new();
        n.notify(PaneId(1), AttentionState::NeedsInput);
        n.notify(PaneId(2), AttentionState::Completed);
        assert_eq!(
            n.calls(),
            vec![(PaneId(1), AttentionState::NeedsInput), (PaneId(2), AttentionState::Completed)]
        );
    }
}
```

- [ ] **Step 3: Update `crates/muxy-daemon/src/lib.rs`**

Add (keep existing `pane`/`server` exports):
```rust
pub mod notify;
pub use notify::{FakeNotifier, Notifier, OsNotifier};
```

- [ ] **Step 4: Run tests**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon notify`
Expected: PASS (1). Also `source "$HOME/.cargo/env" && cargo test` — whole workspace still green.

- [ ] **Step 5: Commit**

```bash
git add crates/muxy-daemon/src/notify.rs crates/muxy-daemon/src/lib.rs crates/muxy-daemon/Cargo.toml
git commit -m "feat(daemon): add Notifier trait with OsNotifier + FakeNotifier"
```

---

### Task 5: `muxy-daemon` — attention state + hook-socket receiver

**Files:**
- Modify: `crates/muxy-daemon/src/server.rs` (extend `Daemon` struct + constructors + attention API)
- Create: `crates/muxy-daemon/src/attention.rs` (hook-socket receiver)
- Modify: `crates/muxy-daemon/src/lib.rs` (add `pub mod attention;`)
- Modify: `crates/muxy-daemon/Cargo.toml` (add `muxy-workspace` dep)
- Test: inline `#[cfg(test)]` in `attention.rs`

**Interfaces:**
- Consumes: `HookEvent`, `HookKind`, `AttentionState`, `MsgStream` (proto); `WorkspaceDriver`, `GitWorktreeDriver`, `Workspace` (muxy-workspace); `Notifier`, `OsNotifier` (Task 4).
- Produces on `Daemon`:
  - `pub fn new_with(driver: Arc<dyn WorkspaceDriver>, notifier: Arc<dyn Notifier>, hook_sock: PathBuf) -> Daemon`
  - `pub fn set_attention(&self, pane: PaneId, state: AttentionState)` — store + broadcast + notify.
  - `pub fn attention_of(&self, pane: PaneId) -> Option<AttentionState>`
  - `pub fn subscribe_attention(&self) -> broadcast::Receiver<(PaneId, AttentionState)>`
  - `pub async fn serve_hooks(self: Arc<Self>, listener: UnixListener) -> Result<()>` (accept loop)
  - `pub async fn handle_hook_conn<S: AsyncRead+AsyncWrite+Unpin+Send>(self: Arc<Self>, stream: S) -> Result<()>`

- [ ] **Step 1: Add the `muxy-workspace` dependency to `crates/muxy-daemon/Cargo.toml`**

```toml
muxy-workspace = { path = "../muxy-workspace" }
```

- [ ] **Step 2: Extend the `Daemon` struct + constructors in `crates/muxy-daemon/src/server.rs`**

Replace the struct and `new()` (keep `spawn_pane`, `get`, `serve`, `handle_conn` as they are for now — `handle_conn` is amended in Task 7). Add the new imports at the top of the file: `use muxy_proto::AttentionState;`, `use muxy_workspace::{Workspace, WorkspaceDriver, GitWorktreeDriver};`, `use crate::notify::{Notifier, OsNotifier};`, `use std::path::PathBuf;`, `use tokio::sync::broadcast;`.

```rust
pub struct Daemon {
    panes: Arc<Mutex<HashMap<PaneId, Arc<Pane>>>>,
    next_id: AtomicU64,
    attention: Arc<Mutex<HashMap<PaneId, AttentionState>>>,
    attention_tx: broadcast::Sender<(PaneId, AttentionState)>,
    workspaces: Arc<Mutex<HashMap<PaneId, Workspace>>>,
    driver: Arc<dyn WorkspaceDriver>,
    notifier: Arc<dyn Notifier>,
    hook_sock: PathBuf,
}

impl Daemon {
    pub fn new() -> Daemon {
        Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            Arc::new(OsNotifier),
            PathBuf::from("/tmp/muxy-hook.sock"),
        )
    }

    pub fn new_with(
        driver: Arc<dyn WorkspaceDriver>,
        notifier: Arc<dyn Notifier>,
        hook_sock: PathBuf,
    ) -> Daemon {
        let (attention_tx, _) = broadcast::channel(256);
        Daemon {
            panes: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            attention: Arc::new(Mutex::new(HashMap::new())),
            attention_tx,
            workspaces: Arc::new(Mutex::new(HashMap::new())),
            driver,
            notifier,
            hook_sock,
        }
    }

    pub fn set_attention(&self, pane: PaneId, state: AttentionState) {
        self.attention.lock().unwrap().insert(pane, state);
        let _ = self.attention_tx.send((pane, state));
        self.notifier.notify(pane, state);
    }

    pub fn attention_of(&self, pane: PaneId) -> Option<AttentionState> {
        self.attention.lock().unwrap().get(&pane).copied()
    }

    pub fn subscribe_attention(&self) -> broadcast::Receiver<(PaneId, AttentionState)> {
        self.attention_tx.subscribe()
    }

    /// Path the daemon injects into agents as MUXY_HOOK_SOCK.
    pub fn hook_sock(&self) -> &std::path::Path {
        &self.hook_sock
    }
}
```

(Note: `driver`/`workspaces` are consumed by Task 6's `spawn_agent`; declaring them now keeps one struct definition. A dead-code warning on them until Task 6 is acceptable.)

- [ ] **Step 3: Write the hook receiver + test in `crates/muxy-daemon/src/attention.rs`**

```rust
use crate::server::Daemon;
use anyhow::Result;
use muxy_proto::{AttentionState, HookEvent, HookKind, MsgStream};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixListener;

impl Daemon {
    /// Accept loop for the hook socket: each connection delivers one HookEvent.
    pub async fn serve_hooks(self: Arc<Self>, listener: UnixListener) -> Result<()> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                let _ = me.handle_hook_conn(stream).await;
            });
        }
    }

    /// Read one HookEvent from a hook connection and apply it to attention state.
    pub async fn handle_hook_conn<S>(self: Arc<Self>, stream: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let mut msgs = MsgStream::new(stream);
        if let Some(event) = msgs.recv::<HookEvent>().await? {
            let state = match event.kind {
                HookKind::Notification => AttentionState::NeedsInput,
                HookKind::Stop => AttentionState::Completed,
            };
            self.set_attention(event.agent_id, state);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::FakeNotifier;
    use muxy_proto::PaneId;
    use muxy_workspace::GitWorktreeDriver;
    use std::path::PathBuf;
    use std::time::Duration;

    #[tokio::test]
    async fn hook_event_updates_attention_broadcasts_and_notifies() {
        let notifier = Arc::new(FakeNotifier::new());
        let daemon = Arc::new(Daemon::new_with(
            Arc::new(GitWorktreeDriver),
            notifier.clone(),
            PathBuf::from("/tmp/unused.sock"),
        ));

        let mut att_rx = daemon.subscribe_attention();

        // Drive one hook connection over an in-memory duplex (stands in for the socket).
        let (client_io, server_io) = tokio::io::duplex(4096);
        let d = daemon.clone();
        tokio::spawn(async move { d.handle_hook_conn(server_io).await.unwrap() });

        let mut client = MsgStream::new(client_io);
        client
            .send(&HookEvent { agent_id: PaneId(7), kind: HookKind::Notification })
            .await
            .unwrap();

        // Broadcast observed.
        let (pane, state) = tokio::time::timeout(Duration::from_secs(2), att_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!((pane, state), (PaneId(7), AttentionState::NeedsInput));
        // State stored.
        assert_eq!(daemon.attention_of(PaneId(7)), Some(AttentionState::NeedsInput));
        // Notifier called.
        assert_eq!(notifier.calls(), vec![(PaneId(7), AttentionState::NeedsInput)]);
    }
}
```

- [ ] **Step 4: Update `crates/muxy-daemon/src/lib.rs`**

Add `pub mod attention;` (keep existing modules/exports).

- [ ] **Step 5: Run tests**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon` (attention test + all prior). Then `source "$HOME/.cargo/env" && cargo test` (whole workspace green).
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-daemon/src/server.rs crates/muxy-daemon/src/attention.rs crates/muxy-daemon/src/lib.rs crates/muxy-daemon/Cargo.toml
git commit -m "feat(daemon): attention state + hook-socket receiver"
```

---

### Task 6: `muxy-daemon` — AgentAdapter, adapters, and `spawn_agent`

**Files:**
- Create: `crates/muxy-daemon/src/agent.rs`
- Modify: `crates/muxy-daemon/src/server.rs` (add `spawn_agent` + a `register_pane` helper; refactor `spawn_pane` to share it)
- Modify: `crates/muxy-daemon/src/lib.rs` (add `pub mod agent;` + re-exports)
- Modify: `crates/muxy-daemon/Cargo.toml` (add `serde_json`)
- Test: inline `#[cfg(test)]` in `agent.rs`

**Interfaces:**
- Consumes: `PaneCommand`, `Pane` (crate), `AttentionState` (proto), `WorkspaceDriver` (workspace).
- Produces:
  - `pub trait AgentAdapter: Send + Sync { fn id(&self) -> &'static str; fn provision_hooks(&self, worktree: &Path, agent_id: PaneId, hook_sock: &Path) -> Result<()>; fn launch_command(&self, worktree: &Path) -> PaneCommand; }`
  - `pub struct ClaudeAdapter;` and `pub struct SyntheticAdapter { pub command: PaneCommand }` (the latter runs a caller-supplied benign command in the worktree; `provision_hooks` is a no-op marker file for tests).
  - On `Daemon`: `pub fn spawn_agent(&self, project: &Path, adapter: &dyn AgentAdapter, task: &str) -> Result<PaneId>`.

- [ ] **Step 1: Add `serde_json` to `crates/muxy-daemon/Cargo.toml`**

```toml
serde_json = "1"
```

- [ ] **Step 2: Add a `register_pane` helper + `spawn_agent` in `crates/muxy-daemon/src/server.rs`**

Refactor `spawn_pane` to use a shared allocate+insert helper, and add `spawn_agent`. Add imports: `use crate::agent::AgentAdapter;`, `use std::path::Path;`.

```rust
impl Daemon {
    fn alloc_id(&self) -> PaneId {
        PaneId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    fn register_pane(&self, id: PaneId, pane: Pane) {
        self.panes.lock().unwrap().insert(id, Arc::new(pane));
    }

    // existing spawn_pane, refactored to reuse the helpers:
    pub fn spawn_pane(&self, cmd: PaneCommand, cols: u16, rows: u16) -> Result<PaneId> {
        let id = self.alloc_id();
        let pane = Pane::spawn(id, cmd, cols, rows)?;
        self.register_pane(id, pane);
        Ok(id)
    }

    /// Provision an isolated worktree, inject the adapter's hooks, and spawn the agent in it.
    pub fn spawn_agent(&self, project: &Path, adapter: &dyn AgentAdapter, task: &str) -> Result<PaneId> {
        let id = self.alloc_id();
        let ws = self.driver.provision(project, task)?;
        adapter.provision_hooks(&ws.path, id, &self.hook_sock)?;

        let mut cmd = adapter.launch_command(&ws.path);
        cmd.cwd = Some(ws.path.clone());
        cmd.env.push(("MUXY_AGENT_ID".into(), id.0.to_string()));
        cmd.env.push(("MUXY_HOOK_SOCK".into(), self.hook_sock.to_string_lossy().to_string()));

        let pane = Pane::spawn(id, cmd, 80, 24)?;
        self.register_pane(id, pane);
        self.workspaces.lock().unwrap().insert(id, ws);
        self.set_attention(id, AttentionState::Working);
        Ok(id)
    }

    pub(crate) fn workspace_of(&self, pane: PaneId) -> Option<Workspace> {
        self.workspaces.lock().unwrap().get(&pane).cloned()
    }
}
```

- [ ] **Step 3: Write `crates/muxy-daemon/src/agent.rs` (trait + adapters + tests)**

```rust
use crate::PaneCommand;
use anyhow::Result;
use muxy_proto::PaneId;
use std::path::Path;

pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    /// Write the tool's hook config into the fresh worktree so its hooks call `muxy-hook`.
    fn provision_hooks(&self, worktree: &Path, agent_id: PaneId, hook_sock: &Path) -> Result<()>;
    /// The command to launch the agent (cwd/env are filled in by the daemon).
    fn launch_command(&self, worktree: &Path) -> PaneCommand;
}

/// Real Claude Code adapter: writes a git-ignored .claude/settings.local.json whose
/// Notification/Stop hooks invoke `muxy-hook`.
pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn provision_hooks(&self, worktree: &Path, _agent_id: PaneId, _hook_sock: &Path) -> Result<()> {
        let dir = worktree.join(".claude");
        std::fs::create_dir_all(&dir)?;
        let hook = |event: &str| {
            serde_json::json!([{ "hooks": [{ "type": "command", "command": format!("muxy-hook --event {event}") }] }])
        };
        let settings = serde_json::json!({
            "hooks": { "Notification": hook("notification"), "Stop": hook("stop") }
        });
        std::fs::write(dir.join("settings.local.json"), serde_json::to_vec_pretty(&settings)?)?;
        Ok(())
    }

    fn launch_command(&self, _worktree: &Path) -> PaneCommand {
        PaneCommand { program: "claude".into(), args: vec![], cwd: None, env: vec![] }
    }
}

/// Test adapter: runs a caller-supplied benign command in the worktree and drops a marker
/// file instead of real hooks. No live agent, no network.
pub struct SyntheticAdapter {
    pub command: PaneCommand,
}

impl AgentAdapter for SyntheticAdapter {
    fn id(&self) -> &'static str {
        "synthetic"
    }

    fn provision_hooks(&self, worktree: &Path, agent_id: PaneId, _hook_sock: &Path) -> Result<()> {
        std::fs::write(worktree.join(".muxy-agent"), agent_id.0.to_string())?;
        Ok(())
    }

    fn launch_command(&self, _worktree: &Path) -> PaneCommand {
        self.command.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_adapter_writes_hook_settings() {
        let dir = tempfile::tempdir().unwrap();
        ClaudeAdapter
            .provision_hooks(dir.path(), PaneId(1), Path::new("/tmp/h.sock"))
            .unwrap();
        let raw = std::fs::read_to_string(dir.path().join(".claude/settings.local.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        // Notification + Stop hooks both call muxy-hook with the right event.
        let notif = v["hooks"]["Notification"][0]["hooks"][0]["command"].as_str().unwrap();
        let stop = v["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(notif, "muxy-hook --event notification");
        assert_eq!(stop, "muxy-hook --event stop");
    }
}
```

- [ ] **Step 4: Add `tempfile` dev-dependency to `crates/muxy-daemon/Cargo.toml`**

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 5: Update `crates/muxy-daemon/src/lib.rs`**

Add:
```rust
pub mod agent;
pub use agent::{AgentAdapter, ClaudeAdapter, SyntheticAdapter};
```

- [ ] **Step 6: Run tests**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon` then `source "$HOME/.cargo/env" && cargo test`.
Expected: PASS (adapter test + all prior; whole workspace green).

- [ ] **Step 7: Commit**

```bash
git add crates/muxy-daemon/src/agent.rs crates/muxy-daemon/src/server.rs crates/muxy-daemon/src/lib.rs crates/muxy-daemon/Cargo.toml
git commit -m "feat(daemon): AgentAdapter + Claude/Synthetic adapters + spawn_agent"
```

---

### Task 7: `muxy-daemon` — forward AttentionChanged + pane-exit wiring (fixes M0a hang)

**Files:**
- Modify: `crates/muxy-daemon/src/pane.rs` (add a `ChildKiller` handle + `Pane::kill()`)
- Modify: `crates/muxy-daemon/src/server.rs` (`handle_conn`: add attention + pane-exit `select!` branches)
- Test: inline `#[cfg(test)]` in `server.rs`

**Interfaces:**
- Consumes: `subscribe_attention()`, `Pane::wait_exit()` (M0a), `AttentionState`.
- Produces: `pub fn kill(&self) -> anyhow::Result<()>` on `Pane`.

- [ ] **Step 1: Add a killer handle to `Pane` in `crates/muxy-daemon/src/pane.rs`**

In `Pane::spawn`, capture a killer BEFORE the child is moved into the wait thread, and store it. `portable_pty::Child::clone_killer()` returns a `Box<dyn ChildKiller + Send + Sync>`.

Add a field to the struct: `killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,`

In `spawn`, right after `let mut child = pair.slave.spawn_command(builder)?;`:
```rust
let killer = child.clone_killer();
```
Add `killer: Mutex::new(killer),` to the `Pane { ... }` constructor. Then add the method:
```rust
pub fn kill(&self) -> anyhow::Result<()> {
    self.killer.lock().map_err(|_| anyhow!("killer poisoned"))?.kill()?;
    Ok(())
}
```

- [ ] **Step 2: Write the failing test (append to `server.rs`'s test module)**

```rust
#[tokio::test]
async fn attached_client_gets_attention_changed() {
    use muxy_proto::AttentionState;
    let daemon = Arc::new(Daemon::new());
    let pane = daemon.spawn_pane(sh("sleep 5"), 80, 24).unwrap();

    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let d = daemon.clone();
    tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

    let mut client = MsgStream::<_>::new(client_io);
    client.send(&ClientToDaemon::Attach { pane }).await.unwrap();
    let _attached: DaemonToClient = client.recv().await.unwrap().unwrap();
    let _backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

    // Flip attention; the attached client must receive AttentionChanged.
    daemon.set_attention(pane, AttentionState::NeedsInput);

    let mut got = None;
    for _ in 0..50 {
        if let Ok(Ok(Some(msg))) =
            tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await
        {
            if let DaemonToClient::AttentionChanged { state, .. } = msg {
                got = Some(state);
                break;
            }
        }
    }
    assert_eq!(got, Some(AttentionState::NeedsInput));
}

#[tokio::test]
async fn client_gets_pane_exited_when_child_exits() {
    let daemon = Arc::new(Daemon::new());
    let pane = daemon.spawn_pane(sh("exit 3"), 80, 24).unwrap();

    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let d = daemon.clone();
    tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

    let mut client = MsgStream::<_>::new(client_io);
    client.send(&ClientToDaemon::Attach { pane }).await.unwrap();

    // Expect a PaneExited to arrive (rather than the session hanging forever).
    let mut exited = false;
    for _ in 0..100 {
        if let Ok(Ok(Some(msg))) =
            tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await
        {
            if let DaemonToClient::PaneExited { .. } = msg {
                exited = true;
                break;
            }
        } else {
            break; // stream closed
        }
    }
    assert!(exited, "client never received PaneExited on child exit");
}
```

- [ ] **Step 3: Amend `handle_conn` in `crates/muxy-daemon/src/server.rs`**

Before the `loop`, subscribe to attention:
```rust
let mut att_rx = self.subscribe_attention();
```
Then add two branches to the existing `tokio::select! { ... }` (keep the `live`/`incoming` branches unchanged):
```rust
att = att_rx.recv() => {
    match att {
        Ok((p, state)) if p == pane.id() => {
            msgs.send(&DaemonToClient::AttentionChanged { pane: p, state }).await?;
        }
        Ok(_) => continue,                        // another pane's attention
        Err(broadcast::error::RecvError::Lagged(_)) => continue,
        Err(_) => {}                              // attention channel closed; ignore
    }
}
code = pane.wait_exit() => {
    msgs.send(&DaemonToClient::PaneExited { pane: pane.id(), code }).await?;
    break;
}
```

- [ ] **Step 4: Run tests**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon` then `source "$HOME/.cargo/env" && cargo test`.
Expected: PASS, including the two new tests and every prior test (the earlier `client_attaches_and_receives_output` and survival tests must still pass — the added `pane.wait_exit()` branch only fires when the child exits, and those tests use long-lived `cat`/loops).

- [ ] **Step 5: Commit**

```bash
git add crates/muxy-daemon/src/pane.rs crates/muxy-daemon/src/server.rs
git commit -m "feat(daemon): forward AttentionChanged + emit PaneExited on child exit"
```

---

### Task 8: `muxy-daemon` — `teardown_agent`, main-binary hook socket, end-to-end test

**Files:**
- Modify: `crates/muxy-daemon/src/server.rs` (add `teardown_agent`)
- Modify: `crates/muxy-daemon/src/main.rs` (bind + serve the hook socket alongside the main socket)
- Create: `crates/muxy-daemon/tests/agent_e2e.rs` (integration test: provision → spawn → simulated hook → teardown)

**Interfaces:**
- Consumes: `spawn_agent`, `set_attention`, `teardown_agent`, `Pane::kill`, `workspace_of`, `handle_hook_conn`.
- Produces: `pub fn teardown_agent(&self, pane: PaneId) -> anyhow::Result<()>` on `Daemon`.

- [ ] **Step 1: Add `teardown_agent` in `crates/muxy-daemon/src/server.rs`**

```rust
impl Daemon {
    /// Kill the agent's process and remove its worktree; drop all per-pane state.
    pub fn teardown_agent(&self, pane: PaneId) -> Result<()> {
        if let Some(p) = self.get(pane) {
            let _ = p.kill();
        }
        let ws = self.workspaces.lock().unwrap().remove(&pane);
        if let Some(ws) = ws {
            self.driver.teardown(&ws)?;
        }
        self.panes.lock().unwrap().remove(&pane);
        self.attention.lock().unwrap().remove(&pane);
        Ok(())
    }
}
```

- [ ] **Step 2: Wire the hook socket into `crates/muxy-daemon/src/main.rs`**

The daemon must listen on BOTH the client socket and the hook socket (the latter at `daemon.hook_sock()`). Replace `main` with:

```rust
use anyhow::Result;
use muxy_daemon::server::Daemon;
use std::sync::Arc;
use tokio::net::UnixListener;

#[tokio::main]
async fn main() -> Result<()> {
    let sock_path = std::env::var("MUXY_SOCK").unwrap_or_else(|_| "/tmp/muxy.sock".into());
    let daemon = Arc::new(Daemon::new());
    let hook_path = daemon.hook_sock().to_path_buf();

    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(&hook_path);
    let client_listener = UnixListener::bind(&sock_path)?;
    let hook_listener = UnixListener::bind(&hook_path)?;
    eprintln!("muxy-daemon: client={sock_path} hook={}", hook_path.display());

    let hooks = daemon.clone();
    tokio::spawn(async move { let _ = hooks.serve_hooks(hook_listener).await; });

    daemon.serve(client_listener).await
}
```
(The M0a demo shell-pane spawn is removed; M0b's daemon is agent-driven. Keep `use muxy_daemon::PaneCommand;` only if still referenced — it is not, so drop it.)

- [ ] **Step 3: Write the end-to-end test in `crates/muxy-daemon/tests/agent_e2e.rs`**

```rust
use muxy_daemon::server::Daemon;
use muxy_daemon::{FakeNotifier, PaneCommand, SyntheticAdapter};
use muxy_proto::{AttentionState, HookEvent, HookKind, MsgStream, PaneId};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let run = |args: &[&str]| {
        assert!(Command::new("git").arg("-C").arg(p).args(args).status().unwrap().success());
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
async fn provision_spawn_hook_teardown_end_to_end() {
    let repo = init_repo();
    let hook_dir = tempfile::tempdir().unwrap();
    let hook_sock = hook_dir.path().join("hook.sock");

    let notifier = Arc::new(FakeNotifier::new());
    let daemon = Arc::new(Daemon::new_with(
        Arc::new(muxy_workspace::GitWorktreeDriver),
        notifier.clone(),
        hook_sock.clone(),
    ));

    // Serve the hook socket so a simulated agent (below) can post a HookEvent.
    let hook_listener = tokio::net::UnixListener::bind(&hook_sock).unwrap();
    let hooks = daemon.clone();
    tokio::spawn(async move { let _ = hooks.serve_hooks(hook_listener).await; });

    // Synthetic agent = a benign long-lived command that runs in the worktree.
    let adapter = SyntheticAdapter {
        command: PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 30".into()],
            cwd: None,
            env: vec![],
        },
    };
    let pane = daemon.spawn_agent(repo.path(), &adapter, "task-e2e").unwrap();

    // Worktree created on a fresh branch, agent's marker + cwd isolation present.
    let ws_path = repo.path().join(".muxy").join("worktrees").join("task-e2e");
    assert!(ws_path.is_dir(), "worktree not provisioned");
    assert!(ws_path.join(".muxy-agent").is_file(), "adapter hook-provision marker missing");
    assert_eq!(daemon.attention_of(pane), Some(AttentionState::Working));

    // Simulate the agent's hook firing (what muxy-hook would send over MUXY_HOOK_SOCK).
    let mut att_rx = daemon.subscribe_attention();
    let stream = tokio::net::UnixStream::connect(&hook_sock).await.unwrap();
    let mut msgs = MsgStream::new(stream);
    msgs.send(&HookEvent { agent_id: pane, kind: HookKind::Notification }).await.unwrap();

    // Attention flips to NeedsInput; broadcast + notifier observe it.
    let mut saw = None;
    for _ in 0..40 {
        if let Ok(Ok((p, s))) = tokio::time::timeout(Duration::from_millis(50), att_rx.recv()).await {
            if p == pane && s == AttentionState::NeedsInput { saw = Some(s); break; }
        }
    }
    assert_eq!(saw, Some(AttentionState::NeedsInput));
    assert!(notifier.calls().contains(&(pane, AttentionState::NeedsInput)));

    // Teardown removes the worktree and drops state.
    daemon.teardown_agent(pane).unwrap();
    assert!(!ws_path.exists(), "worktree not removed on teardown");
    assert_eq!(daemon.attention_of(pane), None);
}
```

- [ ] **Step 4: Ensure the integration test can resolve deps**

`crates/muxy-daemon/Cargo.toml` `[dev-dependencies]` needs `muxy-workspace`, `muxy-proto`, `tempfile`, and `tokio`. Add whichever are missing:
```toml
[dev-dependencies]
tempfile = "3"
muxy-workspace = { path = "../muxy-workspace" }
muxy-proto = { path = "../muxy-proto" }
```
(`tokio` is already a normal dependency, usable in tests.)

- [ ] **Step 5: Run the full suite + build**

Run: `source "$HOME/.cargo/env" && cargo test` (whole workspace: M0a's 10 + all M0b tests, including `agent_e2e`). Then `source "$HOME/.cargo/env" && cargo build` (both binaries build).
Expected: all PASS, clean build.

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-daemon/src/server.rs crates/muxy-daemon/src/main.rs crates/muxy-daemon/tests/agent_e2e.rs crates/muxy-daemon/Cargo.toml
git commit -m "feat(daemon): teardown_agent + hook-socket in binary + e2e agent test"
```

---

## What M0b deliberately excludes (later plans)

- VT-signal fallback (BEL/OSC 9/OSC 777) + hook/VT fusion → **M2**.
- jj `WorkspaceDriver` impl (jj-lib) → **M3**.
- Land/merge/discard UX + client-triggered teardown → **M1/M3**.
- GUI sidebar badges + focused rendering → **M0c** (SwiftUI + libghostty).
- Multi-pane companion terminals → **M1**.

## Self-Review

- **Spec coverage:** `muxy-workspace`/`GitWorktreeDriver` (Task 2) ✓; `muxy-hook` relay (Task 3) ✓; proto `HookEvent`/`AttentionState`/`AttentionChanged` (Task 1) ✓; `Notifier` + OS/fake (Task 4) ✓; attention state + hook receiver (Task 5) ✓; `AgentAdapter` + Claude/Synthetic + `spawn_agent` with env-correlation + worktree cwd (Task 6) ✓; `AttentionChanged` to clients + pane-exit→`PaneExited`/`Completed` fixing the M0a hang (Task 7) ✓; `teardown_agent` + hook socket in the binary + end-to-end synthetic proof (Task 8) ✓. Notification fires from `set_attention` (Task 5), covering NeedsInput/Completed.
- **Placeholder scan:** every code step is complete and compilable; no TBD/"add error handling".
- **Type consistency:** `Daemon` fields declared once (Task 5) and consumed as declared (Tasks 6–8); `spawn_agent`/`teardown_agent`/`set_attention`/`subscribe_attention`/`handle_hook_conn` signatures match across tasks; `PaneCommand{program,args,cwd,env}`, `Workspace{path,branch}`, `HookEvent{agent_id,kind}`, `AttentionState`, and `MUXY_AGENT_ID`/`MUXY_HOOK_SOCK` names are used identically everywhere. `Pane::kill()` (Task 7) is consumed by `teardown_agent` (Task 8).
- **M0a preservation:** `Daemon::new()` keeps its no-arg signature (Task 5); `spawn_pane`/`pump()`/`Pane`/`PaneCommand` unchanged in signature; the pane-exit branch (Task 7) only fires on child exit, so long-lived M0a tests stay green.
