# clowder M7b — Client forwarder (`clowder connect`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `clowder connect <host:port>` forwarder that presents the usual **local** Unix sockets (render + control) and pipes each connection to a remote daemon's TCP listener (M7a), so the existing macOS app + `clowder attach` reach a remote daemon unchanged.

**Architecture:** Two local `UnixListener`s (render, control) in a dedicated per-user subdir. For each accepted local connection: dial the remote TCP (bounded-backoff), send the one-byte channel `Hello` (from M7a: `clowder_proto::write_hello`), then `tokio::io::copy_bidirectional` local⇄remote until either side closes. Security is the user's tunnel (SSH `-L` / Tailscale). This is the single seam where Phase B (M7d) will wrap the TCP dial in TLS/QUIC.

**Tech Stack:** Rust (edition 2021, stable), tokio (`net::{UnixListener,TcpStream}`, `io::copy_bidirectional`), `clowder-proto` (`Channel`, `write_hello`).

## Global Constraints

- **Prefix every cargo command** with `source "$HOME/.cargo/env" && ` (rustup isn't auto-sourced here).
- CI runs `cargo test --workspace --locked`; keep `Cargo.lock` in sync if deps change (none new expected — client already depends on `tokio` full, `clowder-proto`, `clowder-config`).
- The forwarder binds a **dedicated socket subdir** so it never collides with a local daemon: `<control_sock parent>/remote/` — i.e. `Config::load().control_sock.parent()/remote/{clowder.sock, clowder-control.sock}`.
- **Channel mapping:** the render socket forwards as `Channel::Render`; the control socket as `Channel::Control`. Interfaces reused from M7a (already in `main`): `clowder_proto::{Channel, write_hello}`.
- The forwarder holds **no single-instance flock** and adds no auth (Phase A). It **prints** the two local socket paths on startup so a pure-CLI flow works (`export CLOWDER_SOCK=… CLOWDER_CONTROL_SOCK=…`).
- Client `host` may be a hostname (`TcpStream::connect` does DNS) — unlike the daemon's numeric `listen`.
- Reconnect composes with M5d: a mid-stream remote drop ends `copy_bidirectional`, closing the local conn; the app/CLI re-dials the local socket → the forwarder re-dials the remote. The forwarder's own bounded backoff covers the **initial dial** per connection.

---

## Task 1: `remote_host` config

**Files:**
- Modify: `crates/clowder-config/src/lib.rs`

**Interfaces:**
- Produces: `Config.remote_host: Option<String>` — the remote daemon address the forwarder dials (e.g. `"localhost:7777"` / `"100.101.102.103:7777"`); `None` = not configured. Sourced env `CLOWDER_REMOTE_HOST` › file `[remote] host` › `None`, empty ⇒ `None` (mirrors `remote_listen`).

- [ ] **Step 1: Write the failing test.** Add to the `#[cfg(test)] mod tests` in `crates/clowder-config/src/lib.rs`:

```rust
#[test]
fn remote_host_env_over_file_then_none() {
    let f = FileConfig { remote: Some(Remote { listen: None, host: Some("h:1".into()) }), ..Default::default() };
    let env = |k: &str| (k == "CLOWDER_REMOTE_HOST").then(|| "h:2".to_string());
    assert_eq!(Config::resolve(f, &env).remote_host.as_deref(), Some("h:2"));

    let f2 = FileConfig { remote: Some(Remote { listen: None, host: Some("h:3".into()) }), ..Default::default() };
    assert_eq!(Config::resolve(f2, &|_| None).remote_host.as_deref(), Some("h:3"));

    assert_eq!(Config::resolve(FileConfig::default(), &|_| None).remote_host, None);

    // empty file value is "off"
    let f4 = FileConfig { remote: Some(Remote { listen: None, host: Some("".into()) }), ..Default::default() };
    assert_eq!(Config::resolve(f4, &|_| None).remote_host, None);
}
```
(If the existing `remote_listen_env_over_file_then_none` test constructs `Remote { listen: ... }` without a `host` field, update those literals to `Remote { listen: ..., host: None }` so they still compile.)

- [ ] **Step 2: Run test to verify it fails.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config remote_host 2>&1 | tail -20`
Expected: FAIL — no `host` field on `Remote`, no `remote_host` on `Config`.

- [ ] **Step 3: Write minimal implementation.** In `crates/clowder-config/src/lib.rs`:
Add to the `Config` struct (after `remote_listen`):
```rust
    pub remote_host: Option<String>,
```
Add the field to the `Remote` file struct:
```rust
struct Remote { listen: Option<String>, host: Option<String> }
```
In `resolve`'s returned `Config { … }` (after `remote_listen`):
```rust
            remote_host: nonempty("CLOWDER_REMOTE_HOST").or(r.host.filter(|s| !s.is_empty())),
```

- [ ] **Step 4: Run test to verify it passes.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config 2>&1 | tail -20`
Expected: PASS (new test + existing config tests green).

- [ ] **Step 5: Commit.**
```bash
git add crates/clowder-config/src/lib.rs
git commit -m "feat(config): [remote] host (CLOWDER_REMOTE_HOST) for the client forwarder"
```

---

## Task 2: `forward_stream` — dial + hello + bidirectional copy

**Files:**
- Create: `crates/clowder-client/src/forward.rs`
- Modify: `crates/clowder-client/src/lib.rs` (add `pub mod forward;`)

**Interfaces:**
- Consumes: `clowder_proto::{Channel, write_hello}`.
- Produces:
  - `pub async fn dial_with_backoff(host: &str) -> anyhow::Result<tokio::net::TcpStream>` — connect to `host`, retrying with bounded backoff (0.5→cap 8s, up to 6 attempts) before erroring.
  - `pub async fn forward_stream<L>(local: L, host: &str, channel: Channel) -> anyhow::Result<()>` where `L: AsyncRead + AsyncWrite + Unpin + Send` — dial the remote, `write_hello(channel)`, then `copy_bidirectional(local, remote)`.

- [ ] **Step 1: Write the failing test.** Create `crates/clowder-client/src/forward.rs`:

```rust
use anyhow::Result;
use clowder_proto::{write_hello, Channel};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // A fake remote: reads the 1-byte hello, records it, then echoes the rest back.
    async fn echo_remote_recording_hello() -> (std::net::SocketAddr, tokio::sync::oneshot::Receiver<u8>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let hello = sock.read_u8().await.unwrap();
            let _ = tx.send(hello);
            let mut buf = [0u8; 64];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => { if sock.write_all(&buf[..n]).await.is_err() { break; } }
                }
            }
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn forwards_hello_then_pipes_bytes() {
        let (addr, hello_rx) = echo_remote_recording_hello().await;
        let (mut client, server) = tokio::io::duplex(4096); // client = test side, server = forwarder's local side
        let fwd = tokio::spawn(async move {
            forward_stream(server, &addr.to_string(), Channel::Control).await
        });

        client.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");                       // bytes round-tripped through the remote echo
        assert_eq!(hello_rx.await.unwrap(), 1);          // Control hello byte (Control == 1) reached the remote

        drop(client);
        let _ = fwd.await;
    }

    #[tokio::test]
    async fn dial_with_backoff_errors_on_dead_host() {
        // 127.0.0.1:1 refuses quickly; assert we surface an error rather than hang forever.
        let r = dial_with_backoff("127.0.0.1:1").await;
        assert!(r.is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client forward 2>&1 | tail -25`
Expected: FAIL — `forward_stream`/`dial_with_backoff` not found.

- [ ] **Step 3: Write minimal implementation.** Add above the `#[cfg(test)]` block in `forward.rs`:

```rust
/// Connect to `host`, retrying transient failures with bounded exponential backoff
/// (0.5s → cap 8s, up to 6 attempts) before giving up.
pub async fn dial_with_backoff(host: &str) -> Result<TcpStream> {
    let mut delay = Duration::from_millis(500);
    let mut last_err = None;
    for _ in 0..6 {
        match TcpStream::connect(host).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(8));
            }
        }
    }
    Err(anyhow::anyhow!("could not connect to remote {host}: {}", last_err.unwrap()))
}

/// Forward one local connection to the remote daemon: dial, send the channel hello, then
/// pipe bytes both ways until either side closes.
pub async fn forward_stream<L>(mut local: L, host: &str, channel: Channel) -> Result<()>
where
    L: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut remote = dial_with_backoff(host).await?;
    write_hello(&mut remote, channel).await?;
    tokio::io::copy_bidirectional(&mut local, &mut remote).await?;
    Ok(())
}
```
Then add `pub mod forward;` to `crates/clowder-client/src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client forward 2>&1 | tail -25`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit.**
```bash
git add crates/clowder-client/src/forward.rs crates/clowder-client/src/lib.rs
git commit -m "feat(client): forward_stream — dial remote, send hello, pipe bytes"
```

---

## Task 3: `forward` — bind the two local Unix sockets

**Files:**
- Modify: `crates/clowder-client/src/forward.rs`

**Interfaces:**
- Consumes: `forward_stream`, `Channel` (Task 2).
- Produces: `pub async fn forward(host: String, dir: std::path::PathBuf) -> anyhow::Result<()>` — creates `dir`, binds `dir/clowder.sock` (render) + `dir/clowder-control.sock` (control), prints both paths, and runs two accept loops that forward each connection with the matching `Channel`. Runs until cancelled/error.

- [ ] **Step 1: Write the failing test.** Add to `forward.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[tokio::test]
    async fn control_socket_forwards_with_control_hello() {
        use tokio::net::UnixStream;

        let (addr, hello_rx) = echo_remote_recording_hello().await;
        let dir = tempfile::tempdir().unwrap();
        let dirpath = dir.path().to_path_buf();
        let host = addr.to_string();

        let srv = tokio::spawn(async move { forward(host, dirpath).await });
        // wait for the control socket to exist
        let ctl = dir.path().join("clowder-control.sock");
        for _ in 0..50 { if ctl.exists() { break; } tokio::time::sleep(Duration::from_millis(20)).await; }

        let mut c = UnixStream::connect(&ctl).await.unwrap();
        c.write_all(b"hi").await.unwrap();
        let mut got = [0u8; 2];
        c.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hi");                   // round-tripped through the remote echo
        assert_eq!(hello_rx.await.unwrap(), 1);    // the control socket sent a Control hello (== 1)

        srv.abort();
    }
```
(`tempfile` is already a client dev-dependency; `AsyncReadExt`/`AsyncWriteExt` are imported in the test module.)

- [ ] **Step 2: Run test to verify it fails.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client control_socket_forwards 2>&1 | tail -25`
Expected: FAIL — `forward` not found.

- [ ] **Step 3: Write minimal implementation.** Add to `forward.rs` (above the tests):

```rust
use std::path::PathBuf;
use tokio::net::UnixListener;

/// Bind the local render + control Unix sockets under `dir` and forward every connection to the
/// remote daemon at `host` (render → Channel::Render, control → Channel::Control). Prints the two
/// paths so callers can point CLOWDER_SOCK / CLOWDER_CONTROL_SOCK at them.
pub async fn forward(host: String, dir: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&dir)?;
    let render_path = dir.join("clowder.sock");
    let control_path = dir.join("clowder-control.sock");
    let _ = std::fs::remove_file(&render_path);
    let _ = std::fs::remove_file(&control_path);

    let render = UnixListener::bind(&render_path)?;
    let control = UnixListener::bind(&control_path)?;
    println!("clowder connect: forwarding to {host}");
    println!("  export CLOWDER_SOCK={}", render_path.display());
    println!("  export CLOWDER_CONTROL_SOCK={}", control_path.display());

    let accept = |listener: UnixListener, host: String, channel: Channel| async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => { tracing::warn!("forwarder accept error: {e}"); continue; }
            };
            let host = host.clone();
            tokio::spawn(async move {
                if let Err(e) = forward_stream(stream, &host, channel).await {
                    tracing::warn!("forward {channel:?} connection ended: {e}");
                }
            });
        }
    };

    tokio::select! {
        _ = accept(render, host.clone(), Channel::Render) => Ok(()),
        _ = accept(control, host, Channel::Control) => Ok(()),
    }
}
```
(If `Channel` needs `Debug` for the `{channel:?}` log, it already derives `Debug` from M7a.)

- [ ] **Step 4: Run test to verify it passes.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client forward 2>&1 | tail -25`
Expected: PASS (3 tests in `forward`).

- [ ] **Step 5: Commit.**
```bash
git add crates/clowder-client/src/forward.rs
git commit -m "feat(client): forward — bind local render+control sockets, route by channel"
```

---

## Task 4: `clowder connect` CLI subcommand

**Files:**
- Modify: `crates/clowder-client/src/main.rs`

**Interfaces:**
- Consumes: `clowder_client::forward::forward`; `clowder_config::Config`.
- Produces: `clowder connect [host:port]` — resolves the remote host from the arg else `Config.remote_host`, computes the forwarder dir as `<control_sock parent>/remote`, and runs `forward`.

- [ ] **Step 1: Implement the subcommand.** In `crates/clowder-client/src/main.rs`, add a match arm before the fallback (this is CLI wiring exercised by the manual smoke below + the lib tests from Tasks 2–3; no new unit test needed for the arg glue):

```rust
        Some("connect") => {
            let cfg = clowder_config::Config::load();
            let host = args.get(2).cloned()
                .or(cfg.remote_host.clone())
                .ok_or_else(|| anyhow!("usage: clowder connect <host:port>  (or set [remote] host / CLOWDER_REMOTE_HOST)"))?;
            let dir = cfg.control_sock.parent()
                .ok_or_else(|| anyhow!("cannot derive forwarder socket dir"))?
                .join("remote");
            clowder_client::forward::forward(host, dir).await
        }
```
Add `connect` to the top-level usage string (`"usage: clowder <spawn|attach|connect> ..."`).

- [ ] **Step 2: Build + full suite.**
Run: `source "$HOME/.cargo/env" && cargo build -p clowder-client && cargo test --workspace --locked 2>&1 | tail -12`
Expected: builds; all tests pass.

- [ ] **Step 3: Manual end-to-end smoke (real daemon + forwarder).**
Terminal A — a remote-style daemon:
```
source "$HOME/.cargo/env" && CLOWDER_LISTEN=127.0.0.1:7799 \
  CLOWDER_SOCK=/tmp/m7b-d.sock CLOWDER_CONTROL_SOCK=/tmp/m7b-dctl.sock CLOWDER_HOOK_SOCK=/tmp/m7b-dhook.sock \
  XDG_RUNTIME_DIR=/tmp/m7b-daemon cargo run -p clowder-daemon
```
Terminal B — the forwarder:
```
source "$HOME/.cargo/env" && XDG_RUNTIME_DIR=/tmp/m7b-client cargo run -p clowder-client -- connect 127.0.0.1:7799
```
It prints the two local socket paths. Terminal C — drive the control channel through the forwarder's control socket:
```
printf '{"type":"listAgents"}\n' | nc -U /tmp/m7b-client/clowder/remote/clowder-control.sock | head -c 200
```
Expected: an `agentList` JSON line — i.e. control traffic tunneled local-unix → forwarder → TCP → daemon. Ctrl-C both.

- [ ] **Step 4: Commit.**
```bash
git add crates/clowder-client/src/main.rs
git commit -m "feat(client): clowder connect — run the remote forwarder"
```

---

## Self-Review

- **Spec coverage (M7b):** `clowder connect <host:port>` forwarder (Tasks 2–4) ✓; local render+control sockets in a dedicated subdir (Task 3) ✓; channel hello per socket reusing M7a (Tasks 2–3) ✓; bounded-backoff dial (Task 2) ✓; prints socket paths for the pure-CLI flow (Task 3) ✓; `[remote] host` config (Task 1) ✓; no flock/no auth (Phase A) ✓. M7c (app remote mode) is a separate plan; it will run `clowder connect` from the supervisor and point the app at these sockets.
- **Placeholder scan:** none — every step has runnable code/commands.
- **Type consistency:** `Channel`/`write_hello` (M7a) used in Tasks 2–3; `forward_stream`/`dial_with_backoff` (Task 2) used by `forward` (Task 3); `forward` (Task 3) used by the CLI (Task 4); `Config.remote_host` (Task 1) read in Task 4.
