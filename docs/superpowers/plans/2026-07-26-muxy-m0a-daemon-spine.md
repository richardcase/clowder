# muxy M0a — Daemon/Client/PTY Spine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Rust daemon that owns a PTY running a child process, plus a client library that attaches over a Unix socket, streams I/O bidirectionally, and can detach and reattach while the child keeps running.

**Architecture:** A Cargo workspace with three crates. `muxy-proto` defines the wire messages and a length-delimited + postcard framed transport. `muxy-daemon` owns PTYs via `portable-pty` (blocking I/O bridged to tokio with a std thread + channels), keeps a capped output byte-log per pane for replay-on-attach, and serves clients over a `tokio::net::UnixListener`. `muxy-client` is a library `pump()` (testable over pipes) plus a thin binary that wires stdin/stdout in raw mode. M0a is deliberately single-pane, single-project, no GUI — it exists to retire the daemon/client-split + PTY-ownership + detached-survival risk end to end.

**Tech Stack:** Rust (stable), tokio, tokio-util (codec), portable-pty, serde + postcard, bytes, anyhow. Tests use `#[tokio::test]` and `tokio::io::duplex` for in-memory transports.

## Global Constraints

- **Language:** Rust stable only; no nightly features. (Spec: "Rust for the daemon and all shared logic.")
- **Crate naming:** all crates are prefixed `muxy-` and live under `crates/` in one Cargo workspace. (Spec module layout.)
- **Transport abstraction:** the wire transport must sit behind a trait so a future `QuicTransport` can replace the Unix-socket one without touching daemon/client logic. (Spec: `muxy-proto` "Transport trait (Unix socket now, QUIC later)".)
- **Agent identity:** panes are addressed by an opaque `PaneId` (newtype over `u64`), never by pid/cwd. (Spec: identity via injected id, never cwd/session-id.)
- **Daemon owns every fd:** clients never touch a PTY directly; all pane I/O flows through daemon messages. (Spec: "single owner of every PTY fd".)
- **Dependency versions (pin in each Cargo.toml):** `tokio = { version = "1", features = ["full"] }`, `tokio-util = { version = "0.7", features = ["codec"] }`, `portable-pty = "0.8"`, `serde = { version = "1", features = ["derive"] }`, `postcard = { version = "1", features = ["use-std"] }`, `bytes = "1"`, `anyhow = "1"`.

---

### Task 1: Workspace scaffold + `muxy-proto` message types

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `crates/muxy-proto/Cargo.toml`
- Create: `crates/muxy-proto/src/lib.rs`
- Create: `crates/muxy-proto/src/message.rs`
- Test: inline `#[cfg(test)]` in `crates/muxy-proto/src/message.rs`

**Interfaces:**
- Consumes: nothing (first task).
- Produces:
  - `pub struct PaneId(pub u64)` — `Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize`.
  - `pub enum ClientToDaemon { Attach { pane: PaneId }, Input { pane: PaneId, bytes: Vec<u8> }, Resize { pane: PaneId, cols: u16, rows: u16 }, Detach }` — `Serialize, Deserialize, Debug, Clone, PartialEq`.
  - `pub enum DaemonToClient { Attached { pane: PaneId, cols: u16, rows: u16 }, Output { pane: PaneId, bytes: Vec<u8> }, PaneExited { pane: PaneId, code: Option<i32> } }` — `Serialize, Deserialize, Debug, Clone, PartialEq`.

- [ ] **Step 1: Create the workspace root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["crates/muxy-proto", "crates/muxy-daemon", "crates/muxy-client"]

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["codec"] }
portable-pty = "0.8"
serde = { version = "1", features = ["derive"] }
postcard = { version = "1", features = ["use-std"] }
bytes = "1"
anyhow = "1"
```

- [ ] **Step 2: Create `crates/muxy-proto/Cargo.toml`**

```toml
[package]
name = "muxy-proto"
version = "0.0.0"
edition = "2021"

[dependencies]
serde = { workspace = true }
postcard = { workspace = true }
bytes = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
anyhow = { workspace = true }
```

- [ ] **Step 3: Write the failing test in `crates/muxy-proto/src/message.rs`**

```rust
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub struct PaneId(pub u64);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientToDaemon {
    Attach { pane: PaneId },
    Input { pane: PaneId, bytes: Vec<u8> },
    Resize { pane: PaneId, cols: u16, rows: u16 },
    Detach,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DaemonToClient {
    Attached { pane: PaneId, cols: u16, rows: u16 },
    Output { pane: PaneId, bytes: Vec<u8> },
    PaneExited { pane: PaneId, code: Option<i32> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_roundtrips_through_postcard() {
        let msg = ClientToDaemon::Input { pane: PaneId(7), bytes: b"ls\n".to_vec() };
        let bytes = postcard::to_stdvec(&msg).unwrap();
        let back: ClientToDaemon = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn daemon_message_roundtrips_through_postcard() {
        let msg = DaemonToClient::Output { pane: PaneId(7), bytes: b"file.txt\n".to_vec() };
        let bytes = postcard::to_stdvec(&msg).unwrap();
        let back: DaemonToClient = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(msg, back);
    }
}
```

- [ ] **Step 4: Create `crates/muxy-proto/src/lib.rs`**

```rust
pub mod message;
pub use message::{ClientToDaemon, DaemonToClient, PaneId};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p muxy-proto message`
Expected: PASS (2 tests). If this is the very first `cargo` invocation and members `muxy-daemon`/`muxy-client` don't exist yet, temporarily set workspace `members = ["crates/muxy-proto"]`, run, then restore — OR create empty stub crates in Task 2/6 before first build. Simplest: for this task only, run `cargo test -p muxy-proto --manifest-path crates/muxy-proto/Cargo.toml message` which builds just this crate.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/muxy-proto
git commit -m "feat(proto): add muxy-proto message types with postcard roundtrip"
```

---

### Task 2: `muxy-proto` framed transport + `Transport` trait

**Files:**
- Create: `crates/muxy-proto/src/transport.rs`
- Modify: `crates/muxy-proto/src/lib.rs` (add `pub mod transport;` and re-exports)
- Test: inline `#[cfg(test)]` in `crates/muxy-proto/src/transport.rs`

**Interfaces:**
- Consumes: `ClientToDaemon`, `DaemonToClient` from Task 1.
- Produces:
  - `pub trait Transport: Send { }` marker plus two concrete framed helpers below (the trait exists so a future QUIC transport slots in; M0a only implements the Unix-stream side).
  - `pub struct MsgStream<S> { framed: Framed<S, LengthDelimitedCodec> }` where `S: AsyncRead + AsyncWrite + Unpin`.
  - `impl<S> MsgStream<S>`: `pub fn new(io: S) -> Self`, `pub async fn send<M: Serialize>(&mut self, msg: &M) -> anyhow::Result<()>`, `pub async fn recv<M: DeserializeOwned>(&mut self) -> anyhow::Result<Option<M>>` (returns `Ok(None)` on clean EOF).

- [ ] **Step 1: Write the failing test in `crates/muxy-proto/src/transport.rs`**

```rust
use crate::{ClientToDaemon, DaemonToClient, PaneId};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub trait Transport: Send {}

pub struct MsgStream<S> {
    framed: Framed<S, LengthDelimitedCodec>,
}

impl<S> MsgStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub fn new(io: S) -> Self {
        Self { framed: Framed::new(io, LengthDelimitedCodec::new()) }
    }

    pub async fn send<M: Serialize>(&mut self, msg: &M) -> Result<()> {
        let bytes = postcard::to_stdvec(msg)?;
        self.framed.send(bytes.into()).await?;
        Ok(())
    }

    pub async fn recv<M: DeserializeOwned>(&mut self) -> Result<Option<M>> {
        match self.framed.next().await {
            Some(frame) => {
                let frame = frame?;
                let msg = postcard::from_bytes(&frame)?;
                Ok(Some(msg))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn messages_roundtrip_over_duplex() {
        // in-memory bidirectional pipe standing in for a UnixStream
        let (a, b) = tokio::io::duplex(4096);
        let mut client = MsgStream::new(a);
        let mut server = MsgStream::new(b);

        let sent = ClientToDaemon::Attach { pane: PaneId(1) };
        client.send(&sent).await.unwrap();
        let got: ClientToDaemon = server.recv().await.unwrap().unwrap();
        assert_eq!(got, sent);

        let reply = DaemonToClient::Attached { pane: PaneId(1), cols: 80, rows: 24 };
        server.send(&reply).await.unwrap();
        let got: DaemonToClient = client.recv().await.unwrap().unwrap();
        assert_eq!(got, reply);
    }

    #[tokio::test]
    async fn recv_returns_none_on_eof() {
        let (a, b) = tokio::io::duplex(64);
        let client = MsgStream::new(a);
        drop(client); // close one end
        let mut server: MsgStream<_> = MsgStream::new(b);
        let got: Option<ClientToDaemon> = server.recv().await.unwrap();
        assert!(got.is_none());
    }
}
```

- [ ] **Step 2: Add `futures-util` to `crates/muxy-proto/Cargo.toml`**

```toml
futures-util = "0.3"
```

- [ ] **Step 3: Wire the module in `crates/muxy-proto/src/lib.rs`**

```rust
pub mod message;
pub mod transport;
pub use message::{ClientToDaemon, DaemonToClient, PaneId};
pub use transport::{MsgStream, Transport};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p muxy-proto transport`
Expected: PASS (2 tests: `messages_roundtrip_over_duplex`, `recv_returns_none_on_eof`).

- [ ] **Step 5: Commit**

```bash
git add crates/muxy-proto
git commit -m "feat(proto): add length-delimited postcard MsgStream transport"
```

---

### Task 3: `muxy-daemon` PTY pane (spawn, output byte-log, write)

**Files:**
- Create: `crates/muxy-daemon/Cargo.toml`
- Create: `crates/muxy-daemon/src/lib.rs`
- Create: `crates/muxy-daemon/src/pane.rs`
- Test: inline `#[cfg(test)]` in `crates/muxy-daemon/src/pane.rs`

**Interfaces:**
- Consumes: `PaneId` from `muxy-proto`.
- Produces:
  - `pub struct Pane` with fields hidden. Constructor `pub fn spawn(id: PaneId, cmd: PaneCommand, cols: u16, rows: u16) -> anyhow::Result<Pane>`.
  - `pub struct PaneCommand { pub program: String, pub args: Vec<String>, pub cwd: Option<std::path::PathBuf>, pub env: Vec<(String, String)> }`.
  - `pub fn id(&self) -> PaneId`, `pub fn write_input(&self, bytes: &[u8]) -> anyhow::Result<()>`, `pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()>`.
  - `pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Vec<u8>>` — live output stream.
  - `pub fn backlog(&self) -> Vec<u8>` — capped replay buffer of all output so far (for reattach).
  - `pub async fn wait_exit(&self) -> Option<i32>` — resolves when the child exits.

**Note on blocking bridge:** `portable-pty` reader/writer are blocking `std::io`. Spawn a dedicated `std::thread` that reads the master in a loop and forwards chunks to (a) a `tokio::sync::broadcast::Sender<Vec<u8>>` for live subscribers and (b) an `Arc<Mutex<Vec<u8>>>` backlog capped at 256 KiB (drop-oldest). The writer is guarded by a `std::sync::Mutex`.

- [ ] **Step 1: Create `crates/muxy-daemon/Cargo.toml`**

```toml
[package]
name = "muxy-daemon"
version = "0.0.0"
edition = "2021"

[dependencies]
muxy-proto = { path = "../muxy-proto" }
tokio = { workspace = true }
portable-pty = { workspace = true }
anyhow = { workspace = true }

[[bin]]
name = "muxy-daemon"
path = "src/main.rs"
```

- [ ] **Step 2: Write the failing test in `crates/muxy-daemon/src/pane.rs`**

```rust
use anyhow::{anyhow, Result};
use muxy_proto::PaneId;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

const BACKLOG_CAP: usize = 256 * 1024;

#[derive(Clone, Debug)]
pub struct PaneCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<std::path::PathBuf>,
    pub env: Vec<(String, String)>,
}

pub struct Pane {
    id: PaneId,
    writer: Mutex<Box<dyn std::io::Write + Send>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    output_tx: broadcast::Sender<Vec<u8>>,
    backlog: Arc<Mutex<Vec<u8>>>,
    exit_rx: tokio::sync::watch::Receiver<Option<Option<i32>>>,
}

impl Pane {
    pub fn spawn(id: PaneId, cmd: PaneCommand, cols: u16, rows: u16) -> Result<Pane> {
        let pty = native_pty_system();
        let pair = pty.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

        let mut builder = CommandBuilder::new(&cmd.program);
        builder.args(&cmd.args);
        if let Some(cwd) = &cmd.cwd {
            builder.cwd(cwd);
        }
        for (k, v) in &cmd.env {
            builder.env(k, v);
        }
        let mut child = pair.slave.spawn_command(builder)?;
        drop(pair.slave);

        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;

        let (output_tx, _) = broadcast::channel::<Vec<u8>>(1024);
        let backlog = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (exit_tx, exit_rx) = tokio::sync::watch::channel(None);

        // Blocking reader thread -> broadcast + backlog.
        let tx = output_tx.clone();
        let bl = backlog.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = buf[..n].to_vec();
                        {
                            let mut b = bl.lock().unwrap();
                            b.extend_from_slice(&chunk);
                            if b.len() > BACKLOG_CAP {
                                let drop = b.len() - BACKLOG_CAP;
                                b.drain(0..drop);
                            }
                        }
                        let _ = tx.send(chunk);
                    }
                }
            }
        });

        // Blocking wait thread -> watch channel.
        std::thread::spawn(move || {
            let status = child.wait().ok();
            let code = status.map(|s| s.exit_code() as i32);
            let _ = exit_tx.send(Some(code));
        });

        Ok(Pane {
            id,
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            output_tx,
            backlog,
            exit_rx,
        })
    }

    pub fn id(&self) -> PaneId {
        self.id
    }

    pub fn write_input(&self, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        let mut w = self.writer.lock().map_err(|_| anyhow!("writer poisoned"))?;
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let m = self.master.lock().map_err(|_| anyhow!("master poisoned"))?;
        m.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    pub fn backlog(&self) -> Vec<u8> {
        self.backlog.lock().unwrap().clone()
    }

    pub async fn wait_exit(&self) -> Option<i32> {
        let mut rx = self.exit_rx.clone();
        loop {
            if let Some(code) = *rx.borrow() {
                return code;
            }
            if rx.changed().await.is_err() {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::time::Duration;

    fn sh(script: &str) -> PaneCommand {
        PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: vec![("PS1".into(), "".into())],
        }
    }

    #[tokio::test]
    async fn pane_captures_child_output_in_backlog() {
        let pane = Pane::spawn(PaneId(1), sh("printf muxy-hello"), 80, 24).unwrap();
        // give the reader thread time to drain
        for _ in 0..50 {
            if pane.backlog().windows(10).any(|w| w == b"muxy-hello") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let out = pane.backlog();
        assert!(
            out.windows(10).any(|w| w == b"muxy-hello"),
            "backlog missing output: {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[tokio::test]
    async fn pane_forwards_input_to_child() {
        // `cat` echoes stdin back to stdout
        let pane = Pane::spawn(PaneId(2), sh("cat"), 80, 24).unwrap();
        let mut sub = pane.subscribe();
        pane.write_input(b"ping\n").unwrap();
        let mut seen = Vec::new();
        for _ in 0..50 {
            if let Ok(chunk) = tokio::time::timeout(Duration::from_millis(50), sub.recv()).await {
                if let Ok(bytes) = chunk {
                    seen.extend_from_slice(&bytes);
                    if seen.windows(4).any(|w| w == b"ping") {
                        break;
                    }
                }
            }
        }
        assert!(seen.windows(4).any(|w| w == b"ping"), "child did not echo input");
    }
}
```

- [ ] **Step 3: Create `crates/muxy-daemon/src/lib.rs`**

```rust
pub mod pane;
pub use pane::{Pane, PaneCommand};
```

- [ ] **Step 4: Add the missing `Read` import used by the reader thread**

At the top of `src/pane.rs`, ensure `use std::io::Read;` is present (the reader thread calls `reader.read`). Add it to the existing `use` lines.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p muxy-daemon pane`
Expected: PASS (2 tests: `pane_captures_child_output_in_backlog`, `pane_forwards_input_to_child`).

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-daemon
git commit -m "feat(daemon): add PTY pane with output backlog and input forwarding"
```

---

### Task 4: `muxy-daemon` server — accept clients, attach, stream, input

**Files:**
- Create: `crates/muxy-daemon/src/server.rs`
- Create: `crates/muxy-daemon/src/main.rs`
- Modify: `crates/muxy-daemon/src/lib.rs` (add `pub mod server;`)
- Modify: `crates/muxy-daemon/Cargo.toml` (add `muxy-proto` already present; no new deps)
- Test: inline `#[cfg(test)]` in `crates/muxy-daemon/src/server.rs`

**Interfaces:**
- Consumes: `Pane`, `PaneCommand` (Task 3); `MsgStream`, `ClientToDaemon`, `DaemonToClient`, `PaneId` (proto).
- Produces:
  - `pub struct Daemon { panes: Arc<Mutex<HashMap<PaneId, Arc<Pane>>>>, next_id: AtomicU64 }`.
  - `pub fn new() -> Daemon`.
  - `pub fn spawn_pane(&self, cmd: PaneCommand, cols: u16, rows: u16) -> anyhow::Result<PaneId>`.
  - `pub async fn serve(self: Arc<Self>, listener: UnixListener) -> anyhow::Result<()>` — accept loop.
  - `pub async fn handle_conn<S>(self: Arc<Self>, stream: S)` where `S: AsyncRead + AsyncWrite + Unpin + Send` — one client session (extracted so tests drive it over `duplex` without a real socket).

**Session semantics for M0a:** a client sends `Attach { pane }`. The daemon replies `Attached { pane, cols, rows }`, immediately sends one `Output` frame containing the pane's current `backlog()`, then forwards every live `subscribe()` chunk as `Output`. Concurrently it reads `Input`/`Resize`/`Detach` from the client and applies them to the pane. On `Detach` or client EOF, the session ends but the pane keeps running.

- [ ] **Step 1: Write the failing test in `crates/muxy-daemon/src/server.rs`**

```rust
use crate::{Pane, PaneCommand};
use anyhow::Result;
use muxy_proto::{ClientToDaemon, DaemonToClient, MsgStream, PaneId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixListener;

pub struct Daemon {
    panes: Arc<Mutex<HashMap<PaneId, Arc<Pane>>>>,
    next_id: AtomicU64,
}

impl Daemon {
    pub fn new() -> Daemon {
        Daemon {
            panes: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn spawn_pane(&self, cmd: PaneCommand, cols: u16, rows: u16) -> Result<PaneId> {
        let id = PaneId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let pane = Pane::spawn(id, cmd, cols, rows)?;
        self.panes.lock().unwrap().insert(id, Arc::new(pane));
        Ok(id)
    }

    fn get(&self, id: PaneId) -> Option<Arc<Pane>> {
        self.panes.lock().unwrap().get(&id).cloned()
    }

    pub async fn serve(self: Arc<Self>, listener: UnixListener) -> Result<()> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                let _ = me.handle_conn(stream).await;
            });
        }
    }

    pub async fn handle_conn<S>(self: Arc<Self>, stream: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let mut msgs = MsgStream::new(stream);
        // First message must be Attach.
        let pane = loop {
            match msgs.recv::<ClientToDaemon>().await? {
                Some(ClientToDaemon::Attach { pane }) => match self.get(pane) {
                    Some(p) => break p,
                    None => return Ok(()), // unknown pane: end session
                },
                Some(_) => continue, // ignore until attached
                None => return Ok(()),
            }
        };

        let (cols, rows) = pane.size();
        msgs.send(&DaemonToClient::Attached { pane: pane.id(), cols, rows }).await?;
        msgs.send(&DaemonToClient::Output { pane: pane.id(), bytes: pane.backlog() }).await?;

        let mut sub = pane.subscribe();
        loop {
            tokio::select! {
                live = sub.recv() => {
                    match live {
                        Ok(bytes) => msgs.send(&DaemonToClient::Output { pane: pane.id(), bytes }).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                incoming = msgs.recv::<ClientToDaemon>() => {
                    match incoming? {
                        Some(ClientToDaemon::Input { bytes, .. }) => { let _ = pane.write_input(&bytes); }
                        Some(ClientToDaemon::Resize { cols, rows, .. }) => { let _ = pane.resize(cols, rows); }
                        Some(ClientToDaemon::Detach) | None => break,
                        Some(ClientToDaemon::Attach { .. }) => continue,
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sh(script: &str) -> PaneCommand {
        PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: vec![],
        }
    }

    #[tokio::test]
    async fn client_attaches_and_receives_output() {
        let daemon = Arc::new(Daemon::new());
        let pane = daemon.spawn_pane(sh("cat"), 80, 24).unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::Attach { pane }).await.unwrap();

        // Expect Attached, then a (possibly empty) backlog Output.
        let attached: DaemonToClient = client.recv().await.unwrap().unwrap();
        assert!(matches!(attached, DaemonToClient::Attached { .. }));
        let _backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

        client
            .send(&ClientToDaemon::Input { pane, bytes: b"echo hi\n".to_vec() })
            .await
            .unwrap();

        let mut seen = Vec::new();
        for _ in 0..50 {
            if let Ok(Ok(Some(DaemonToClient::Output { bytes, .. }))) =
                tokio::time::timeout(Duration::from_millis(50), client.recv::<DaemonToClient>()).await
            {
                seen.extend_from_slice(&bytes);
                if seen.windows(2).any(|w| w == b"hi") {
                    break;
                }
            }
        }
        assert!(seen.windows(2).any(|w| w == b"hi"), "did not receive echoed output");
    }
}
```

- [ ] **Step 2: Add `size()` accessor to `Pane` in `crates/muxy-daemon/src/pane.rs`**

Store the last-known size on the pane and expose it (needed by `Attached`). Add a field `size: Mutex<(u16, u16)>`, set it in `spawn` to `(cols, rows)`, update it in `resize`, and add:

```rust
pub fn size(&self) -> (u16, u16) {
    *self.size.lock().unwrap()
}
```

- [ ] **Step 3: Create `crates/muxy-daemon/src/main.rs`**

```rust
use anyhow::Result;
use muxy_daemon::server::Daemon;
use muxy_daemon::PaneCommand;
use std::sync::Arc;
use tokio::net::UnixListener;

#[tokio::main]
async fn main() -> Result<()> {
    let sock_path = std::env::var("MUXY_SOCK").unwrap_or_else(|_| "/tmp/muxy.sock".into());
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)?;

    let daemon = Arc::new(Daemon::new());
    // M0a: launch a single login shell pane so a client has something to attach to.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let pane = daemon.spawn_pane(
        PaneCommand { program: shell, args: vec![], cwd: None, env: vec![] },
        80,
        24,
    )?;
    eprintln!("muxy-daemon listening on {sock_path}, pane {pane:?}");

    daemon.serve(listener).await
}
```

- [ ] **Step 4: Update `crates/muxy-daemon/src/lib.rs`**

```rust
pub mod pane;
pub mod server;
pub use pane::{Pane, PaneCommand};
pub use server::Daemon;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p muxy-daemon server`
Expected: PASS (1 test: `client_attaches_and_receives_output`).

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-daemon
git commit -m "feat(daemon): serve clients over unix socket with attach/input/output"
```

---

### Task 5: Detached survival + replay-on-reattach

**Files:**
- Test: inline `#[cfg(test)]` in `crates/muxy-daemon/src/server.rs` (add one test to the existing module)

**Interfaces:**
- Consumes: everything from Task 4. No new production interfaces — this task proves the survival property the whole architecture rests on, so it is its own reviewer gate.

- [ ] **Step 1: Write the failing test (append to the `tests` module in `server.rs`)**

```rust
#[tokio::test]
async fn pane_survives_detach_and_replays_on_reattach() {
    use std::time::Duration;

    let daemon = Arc::new(Daemon::new());
    // A shell that appends a line every 100ms to prove it keeps running while detached.
    let pane = daemon
        .spawn_pane(sh("i=0; while true; do i=$((i+1)); echo line$i; sleep 0.1; done"), 80, 24)
        .unwrap();

    // First client attaches, collects some output, then detaches.
    {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        let h = tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::Attach { pane }).await.unwrap();
        let _attached: DaemonToClient = client.recv().await.unwrap().unwrap();
        let _backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

        // Read a little live output.
        let mut seen = Vec::new();
        for _ in 0..30 {
            if let Ok(Ok(Some(DaemonToClient::Output { bytes, .. }))) =
                tokio::time::timeout(Duration::from_millis(100), client.recv::<DaemonToClient>()).await
            {
                seen.extend_from_slice(&bytes);
                if seen.windows(5).any(|w| w == b"line1") {
                    break;
                }
            }
        }
        assert!(seen.windows(5).any(|w| w == b"line1"), "first attach saw no output");

        client.send(&ClientToDaemon::Detach).await.unwrap();
        let _ = h.await; // session ends; pane must keep running
    }

    // Let the detached pane run a bit more.
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Second client reattaches; the backlog replay must contain later lines
    // that were produced WHILE no client was attached.
    {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

        let mut client = MsgStream::<_>::new(client_io);
        client.send(&ClientToDaemon::Attach { pane }).await.unwrap();
        let _attached: DaemonToClient = client.recv().await.unwrap().unwrap();
        let backlog: DaemonToClient = client.recv().await.unwrap().unwrap();

        let bytes = match backlog {
            DaemonToClient::Output { bytes, .. } => bytes,
            other => panic!("expected backlog Output, got {other:?}"),
        };
        // At least line4+ should exist, proving the pane produced output while detached.
        assert!(
            bytes.windows(5).any(|w| w == b"line4"),
            "reattach backlog did not include output produced while detached: {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p muxy-daemon pane_survives_detach_and_replays_on_reattach -- --nocapture`
Expected: PASS. If it flakes on timing, increase the `sleep(400ms)` — but do NOT lower the assertion; the property (output accrues while detached) must hold.

- [ ] **Step 3: Commit**

```bash
git add crates/muxy-daemon
git commit -m "test(daemon): prove pane survives detach and replays on reattach"
```

---

### Task 6: `muxy-client` — testable `pump()` + thin raw-mode binary

**Files:**
- Create: `crates/muxy-client/Cargo.toml`
- Create: `crates/muxy-client/src/lib.rs`
- Create: `crates/muxy-client/src/main.rs`
- Test: inline `#[cfg(test)]` in `crates/muxy-client/src/lib.rs`

**Interfaces:**
- Consumes: `MsgStream`, `ClientToDaemon`, `DaemonToClient`, `PaneId` (proto).
- Produces:
  - `pub async fn pump<S, R, W>(io: S, pane: PaneId, mut input: R, mut output: W) -> anyhow::Result<()>` where `S: AsyncRead + AsyncWrite + Unpin + Send`, `R: AsyncRead + Unpin + Send`, `W: AsyncWrite + Unpin + Send`. Sends `Attach`, then concurrently pumps `input` bytes → `Input` messages and `Output` messages → `output`. Returns on EOF of `input` (sends `Detach`) or when the connection closes.

The binary wires `pump()` to a real `UnixStream` plus stdin/stdout in raw mode; the library is what tests exercise (no TTY needed).

- [ ] **Step 1: Create `crates/muxy-client/Cargo.toml`**

```toml
[package]
name = "muxy-client"
version = "0.0.0"
edition = "2021"

[dependencies]
muxy-proto = { path = "../muxy-proto" }
tokio = { workspace = true }
anyhow = { workspace = true }

[target.'cfg(unix)'.dependencies]
# raw-mode terminal control for the interactive binary only
crossterm = "0.28"

[[bin]]
name = "muxy"
path = "src/main.rs"
```

- [ ] **Step 2: Write the failing test in `crates/muxy-client/src/lib.rs`**

```rust
use anyhow::Result;
use muxy_proto::{ClientToDaemon, DaemonToClient, MsgStream, PaneId};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub async fn pump<S, R, W>(io: S, pane: PaneId, mut input: R, mut output: W) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let mut msgs = MsgStream::new(io);
    msgs.send(&ClientToDaemon::Attach { pane }).await?;

    let mut buf = [0u8; 4096];
    loop {
        tokio::select! {
            n = input.read(&mut buf) => {
                match n {
                    Ok(0) => { let _ = msgs.send(&ClientToDaemon::Detach).await; break; }
                    Ok(n) => msgs.send(&ClientToDaemon::Input { pane, bytes: buf[..n].to_vec() }).await?,
                    Err(_) => break,
                }
            }
            msg = msgs.recv::<DaemonToClient>() => {
                match msg? {
                    Some(DaemonToClient::Output { bytes, .. }) => {
                        output.write_all(&bytes).await?;
                        output.flush().await?;
                    }
                    Some(DaemonToClient::PaneExited { .. }) | None => break,
                    Some(DaemonToClient::Attached { .. }) => {}
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxy_daemon::{Daemon, PaneCommand};
    use std::sync::Arc;
    use std::time::Duration;

    fn sh(script: &str) -> PaneCommand {
        PaneCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: vec![],
        }
    }

    #[tokio::test]
    async fn pump_forwards_input_and_renders_output() {
        let daemon = Arc::new(Daemon::new());
        let pane = daemon.spawn_pane(sh("cat"), 80, 24).unwrap();

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { d.handle_conn(server_io).await.unwrap() });

        // Feed "hello\n" then EOF; capture rendered output into a Vec.
        let input = std::io::Cursor::new(b"hello\n".to_vec());
        let output = Vec::new();
        // Wrap output so we can read it back after pump returns.
        let output = std::io::Cursor::new(output);

        // Run pump with a timeout so the test can't hang.
        let handle = tokio::spawn(async move {
            let mut out = output;
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                pump(client_io, pane, input, &mut out),
            )
            .await;
            out.into_inner()
        });

        let rendered = handle.await.unwrap();
        assert!(
            rendered.windows(5).any(|w| w == b"hello"),
            "pump did not render echoed output: {:?}",
            String::from_utf8_lossy(&rendered)
        );
    }
}
```

- [ ] **Step 3: Add `muxy-daemon` as a dev-dependency of `muxy-client`**

The test spins up an in-process `Daemon`. In `crates/muxy-client/Cargo.toml` add:

```toml
[dev-dependencies]
muxy-daemon = { path = "../muxy-daemon" }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p muxy-client pump_forwards_input_and_renders_output`
Expected: PASS.

- [ ] **Step 5: Write the thin interactive binary `crates/muxy-client/src/main.rs`**

```rust
use anyhow::Result;
use muxy_client::pump;
use muxy_proto::PaneId;
use tokio::net::UnixStream;

#[tokio::main]
async fn main() -> Result<()> {
    let sock = std::env::var("MUXY_SOCK").unwrap_or_else(|_| "/tmp/muxy.sock".into());
    let pane = PaneId(
        std::env::args()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(1),
    );

    let stream = UnixStream::connect(&sock).await?;

    // Put the real terminal in raw mode so keystrokes reach the pane unbuffered.
    crossterm::terminal::enable_raw_mode()?;
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let result = pump(stream, pane, stdin, stdout).await;
    crossterm::terminal::disable_raw_mode()?;
    result
}
```

- [ ] **Step 6: Verify the whole workspace builds and all tests pass**

Run: `cargo build && cargo test`
Expected: workspace builds; all tests across `muxy-proto`, `muxy-daemon`, `muxy-client` PASS.

- [ ] **Step 7: Manual end-to-end smoke test**

```bash
# terminal 1
cargo run -p muxy-daemon
# terminal 2
cargo run -p muxy-client -- 1
# type `echo hi`, see output; Ctrl-C to quit the client;
# reconnect with `cargo run -p muxy-client -- 1` and confirm the shell state persisted.
```

Expected: the client shows a live shell; quitting the client and reconnecting shows the same pane still alive (scrollback replayed).

- [ ] **Step 8: Commit**

```bash
git add crates/muxy-client
git commit -m "feat(client): add testable pump() and raw-mode interactive binary"
```

---

## What M0a deliberately excludes (handled by later plans)

- **Multiple panes / projects, sidebar, command palette, keymap** → M1.
- **Workspace provisioning (`GitWorktreeDriver`) + `muxy-hook` relay + attention events** → M0b.
- **SwiftUI client + libghostty embedding + real terminal rendering** → M0c (gated on a libghostty C-API research pass; the `pump()`/`MsgStream` interfaces here are exactly what that client will consume, so the daemon is ready for it).
- **`muxy-vt` authoritative grid + snapshot-on-attach** → M1 (M0a replays the raw byte-log instead of a grid snapshot, which is sufficient to prove survival).

## Self-Review

- **Spec coverage (M0a slice of M0):** daemon owns PTY (Task 3) ✓; daemon/client split over a socket (Tasks 2, 4) ✓; attach/detach with survival + replay (Task 5) ✓; `Transport` seam for future QUIC (Task 2 trait) ✓; identity via opaque `PaneId` (Task 1) ✓. Workspace/hook/GUI are explicitly out of this slice and carried to M0b/M0c.
- **Placeholder scan:** every code step contains complete, compilable code; no "add error handling"/"TBD" left. The one behavioral caveat (timing in Task 5) gives an explicit adjustment instruction, not a placeholder.
- **Type consistency:** `PaneId`, `ClientToDaemon`, `DaemonToClient`, `MsgStream`, `Pane`, `PaneCommand`, `Daemon`, and `pump()` signatures are used identically across tasks. `Pane::size()` is introduced in Task 4 Step 2 before `handle_conn` consumes it. `PaneCommand` fields (`program/args/cwd/env`) match everywhere.
```
