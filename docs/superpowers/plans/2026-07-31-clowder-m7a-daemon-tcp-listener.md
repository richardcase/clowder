# clowder M7a — Daemon TCP listener + channel Hello Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the daemon optionally accept the render + control channels over **TCP** (one port, off by default), so a remote client (M7b forwarder) can reach it through a tunnel.

**Architecture:** A new opt-in `TcpListener` in the daemon `main`. Each accepted TCP connection begins with a **one-byte channel hello** (`Control`/`Render`); the daemon reads it and hands the *same* stream to the existing, already-stream-generic `handle_control_json` / `handle_conn`. No handler or protocol-body changes. Config gains `[remote] listen`. The hook channel is never exposed over TCP.

**Tech Stack:** Rust (edition 2021, stable), tokio (`net::TcpListener`/`TcpStream`, `io` ext), the existing `clowder-proto` `MsgStream` (length-delimited postcard) and JSON-lines control protocol.

## Global Constraints

- **Prefix every cargo command** with `source "$HOME/.cargo/env" && ` (rustup isn't auto-sourced here).
- CI runs `cargo test --workspace --locked`; keep `Cargo.lock` in sync if deps change (none are added here).
- **TCP is OFF by default.** Absent/empty `[remote] listen` ⇒ no TCP bind (today's behavior, zero network surface).
- **Phase A has no auth/encryption.** The daemon must be bound to **loopback or a Tailscale (CGNAT `100.64.0.0/10`) address** only; a startup **warning** is emitted otherwise. Real auth is M7d.
- The **one-byte hello** is the concrete form of the spec's channel `Hello` (a byte read via `read_u8` doesn't over-read, so the stream stays cleanly framed for the body — unlike a codec-buffered frame). Values: `Control = 1`, `Render = 2`.
- Hooks stay local — **never** route the hook channel over TCP.
- `Daemon` handlers reused verbatim: `pub async fn handle_conn<S: AsyncRead+AsyncWrite+Unpin+Send>(self: Arc<Self>, stream: S) -> Result<()>` (render, `server.rs`) and `pub async fn handle_control_json<S: …>(self: Arc<Self>, stream: S) -> Result<()>` (`control_json.rs`).

---

## Task 1: Channel hello in `clowder-proto`

**Files:**
- Create: `crates/clowder-proto/src/remote.rs`
- Modify: `crates/clowder-proto/src/lib.rs` (add `pub mod remote;` + re-export)

**Interfaces:**
- Produces: `pub enum Channel { Control, Render }` (Copy, Eq, Debug); `pub async fn write_hello<W: AsyncWrite + Unpin>(w: &mut W, channel: Channel) -> anyhow::Result<()>`; `pub async fn read_hello<R: AsyncRead + Unpin>(r: &mut R) -> anyhow::Result<Channel>`. Used by M7a (daemon) and M7b (forwarder).

- [ ] **Step 1: Write the failing test.** Create `crates/clowder-proto/src/remote.rs` with the implementation absent but the test present:

```rust
use anyhow::{bail, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hello_roundtrips_both_channels() {
        for ch in [Channel::Control, Channel::Render] {
            let (mut a, mut b) = tokio::io::duplex(64);
            write_hello(&mut a, ch).await.unwrap();
            assert_eq!(read_hello(&mut b).await.unwrap(), ch);
        }
    }

    #[tokio::test]
    async fn unknown_channel_byte_errors() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_u8(9).await.unwrap();
        assert!(read_hello(&mut b).await.is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-proto remote 2>&1 | tail -20`
Expected: FAIL — `cannot find type Channel` / `write_hello` not found.

- [ ] **Step 3: Write minimal implementation.** Add above the `#[cfg(test)]` block in `remote.rs`:

```rust
/// Which channel a remote (TCP) connection carries. Sent as a single byte at the
/// very start of the connection, before any channel-specific framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Control,
    Render,
}

impl Channel {
    fn to_byte(self) -> u8 {
        match self {
            Channel::Control => 1,
            Channel::Render => 2,
        }
    }
    fn from_byte(b: u8) -> Result<Channel> {
        match b {
            1 => Ok(Channel::Control),
            2 => Ok(Channel::Render),
            other => bail!("unknown channel hello byte {other}"),
        }
    }
}

/// Write the one-byte channel hello that prefixes a remote connection.
pub async fn write_hello<W: AsyncWrite + Unpin>(w: &mut W, channel: Channel) -> Result<()> {
    w.write_u8(channel.to_byte()).await?;
    w.flush().await?;
    Ok(())
}

/// Read the one-byte channel hello from the start of a remote connection.
/// `read_u8` reads exactly one byte (no over-read), so the remaining stream stays
/// correctly framed for the channel body.
pub async fn read_hello<R: AsyncRead + Unpin>(r: &mut R) -> Result<Channel> {
    let b = r.read_u8().await?;
    Channel::from_byte(b)
}
```

Then in `crates/clowder-proto/src/lib.rs` add `pub mod remote;` (after `pub mod control;`) and `pub use remote::{read_hello, write_hello, Channel};`.

- [ ] **Step 4: Run test to verify it passes.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-proto remote 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit.**

```bash
git add crates/clowder-proto/src/remote.rs crates/clowder-proto/src/lib.rs
git commit -m "feat(proto): channel hello for remote TCP connections"
```

---

## Task 2: `remote_listen` config

**Files:**
- Modify: `crates/clowder-config/src/lib.rs`

**Interfaces:**
- Consumes: `Config::resolve(f: FileConfig, get_env: &dyn Fn(&str) -> Option<String>) -> Config` (existing pure resolver).
- Produces: `Config.remote_listen: Option<String>` — the daemon TCP bind address (e.g. `"127.0.0.1:7777"`); `None` = TCP off. Sourced env `CLOWDER_LISTEN` › file `[remote] listen` › `None`.

- [ ] **Step 1: Write the failing test.** Add to the `#[cfg(test)] mod tests` in `crates/clowder-config/src/lib.rs`:

```rust
#[test]
fn remote_listen_env_over_file_then_none() {
    // env wins over file
    let f = FileConfig { remote: Some(Remote { listen: Some("127.0.0.1:1".into()) }), ..Default::default() };
    let env = |k: &str| (k == "CLOWDER_LISTEN").then(|| "127.0.0.1:2".to_string());
    assert_eq!(Config::resolve(f, &env).remote_listen.as_deref(), Some("127.0.0.1:2"));

    // file only
    let f2 = FileConfig { remote: Some(Remote { listen: Some("127.0.0.1:3".into()) }), ..Default::default() };
    assert_eq!(Config::resolve(f2, &|_| None).remote_listen.as_deref(), Some("127.0.0.1:3"));

    // neither → None (TCP off)
    assert_eq!(Config::resolve(FileConfig::default(), &|_| None).remote_listen, None);
}
```

- [ ] **Step 2: Run test to verify it fails.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config remote_listen 2>&1 | tail -20`
Expected: FAIL — no field `remote` on `FileConfig`, no `Remote` type, no `remote_listen` on `Config`.

- [ ] **Step 3: Write minimal implementation.** In `crates/clowder-config/src/lib.rs`:

Add the field to the `Config` struct (after `default_rows`):
```rust
    pub remote_listen: Option<String>,
```
Add the file section type + field. Extend `FileConfig`:
```rust
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    sockets: Option<Sockets>,
    pane: Option<PaneCfg>,
    remote: Option<Remote>,
}
#[derive(Debug, Default, Deserialize)]
struct Remote { listen: Option<String> }
```
In `resolve`, after `let p = f.pane.unwrap_or_default();` add:
```rust
        let r = f.remote.unwrap_or_default();
```
and in the returned `Config { … }` add (after `default_rows`):
```rust
            remote_listen: nonempty("CLOWDER_LISTEN").or(r.listen),
```
(`nonempty` is the existing closure that reads an env key and drops empty strings.)

- [ ] **Step 4: Run test to verify it passes.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-config 2>&1 | tail -20`
Expected: PASS (new test + existing config tests still green).

- [ ] **Step 5: Commit.**

```bash
git add crates/clowder-config/src/lib.rs
git commit -m "feat(config): [remote] listen (CLOWDER_LISTEN) for the daemon TCP listener"
```

---

## Task 3: `serve_remote` + `handle_remote_conn` on `Daemon`

**Files:**
- Create: `crates/clowder-daemon/src/remote.rs`
- Modify: `crates/clowder-daemon/src/lib.rs` (add `pub mod remote;`)

**Interfaces:**
- Consumes: `clowder_proto::{read_hello, Channel}` (Task 1); `Daemon::handle_conn` / `Daemon::handle_control_json` (existing, `pub`); `Daemon::new_with(notifier: Arc<dyn Notifier>, hook_sock: PathBuf)` + `clowder_daemon::FakeNotifier` (existing test constructor, see `clowder-client` tests).
- Produces: `pub async fn Daemon::serve_remote(self: Arc<Self>, listener: tokio::net::TcpListener) -> anyhow::Result<()>`; `async fn Daemon::handle_remote_conn<S: AsyncRead+AsyncWrite+Unpin+Send>(self: Arc<Self>, stream: S) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing test.** Create `crates/clowder-daemon/src/remote.rs`:

```rust
use crate::server::Daemon;
use anyhow::Result;
use clowder_proto::{read_hello, Channel};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeNotifier;
    use clowder_proto::{write_hello, ClientToDaemon, MsgStream, PaneId};
    use std::path::PathBuf;
    use tokio::io::{AsyncBufReadExt, BufReader};

    fn test_daemon() -> Arc<Daemon> {
        Arc::new(Daemon::new_with(Arc::new(FakeNotifier::new()), PathBuf::from("/tmp/unused-m7a.sock")))
    }

    #[tokio::test]
    async fn control_hello_routes_to_control_handler() {
        let daemon = test_daemon();
        let (client, server) = tokio::io::duplex(4096);
        let h = tokio::spawn(async move { daemon.handle_remote_conn(server).await });

        let mut client = client;
        write_hello(&mut client, Channel::Control).await.unwrap();
        // The control handler's first action is to emit an AgentList event as a JSON line.
        let (rd, _wr) = tokio::io::split(client);
        let mut lines = BufReader::new(rd).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        assert!(line.contains("agentList"), "expected agentList event, got: {line}");
        h.abort();
    }

    #[tokio::test]
    async fn render_hello_routes_to_render_handler() {
        let daemon = test_daemon();
        let (client, server) = tokio::io::duplex(4096);
        let h = tokio::spawn(async move { daemon.handle_remote_conn(server).await });

        let mut client = client;
        write_hello(&mut client, Channel::Render).await.unwrap();
        // Render handler reads Attach first; an unknown pane ends the session with Ok(()).
        let mut msgs = MsgStream::new(client);
        msgs.send(&ClientToDaemon::Attach { pane: PaneId(999_999) }).await.unwrap();
        let res = h.await.unwrap();
        assert!(res.is_ok(), "render route returned: {res:?}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon remote 2>&1 | tail -25`
Expected: FAIL — `handle_remote_conn`/`serve_remote` not found, `remote` module not declared.

- [ ] **Step 3: Write minimal implementation.** Add above the `#[cfg(test)]` block in `crates/clowder-daemon/src/remote.rs`:

```rust
impl Daemon {
    /// Accept loop for the opt-in remote TCP listener. Each connection is prefixed
    /// with a one-byte channel hello, then routed to the same per-connection handler
    /// as the local Unix sockets. The hook channel is never exposed here.
    pub async fn serve_remote(self: Arc<Self>, listener: TcpListener) -> Result<()> {
        loop {
            let (stream, _addr) = listener.accept().await?;
            let me = self.clone();
            tokio::spawn(async move {
                if let Some(line) = crate::logging::conn_error_line("remote", me.handle_remote_conn(stream).await) {
                    tracing::warn!("{line}");
                }
            });
        }
    }

    /// Read the channel hello, then dispatch to the existing control/render handler.
    async fn handle_remote_conn<S>(self: Arc<Self>, mut stream: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        match read_hello(&mut stream).await? {
            Channel::Control => self.handle_control_json(stream).await,
            Channel::Render => self.handle_conn(stream).await,
        }
    }
}
```

Then add `pub mod remote;` to `crates/clowder-daemon/src/lib.rs` (near the other `pub mod` lines). If `handle_remote_conn` being private trips the test (same-crate `#[cfg(test)]` submodule can call it), keep it private; the test is in the same crate so it resolves.

- [ ] **Step 4: Run test to verify it passes.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon remote 2>&1 | tail -25`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit.**

```bash
git add crates/clowder-daemon/src/remote.rs crates/clowder-daemon/src/lib.rs
git commit -m "feat(daemon): serve_remote — route TCP connections by channel hello"
```

---

## Task 4: Wire the TCP listener into `main`, with an exposure warning

**Files:**
- Modify: `crates/clowder-daemon/src/main.rs`
- Modify: `crates/clowder-daemon/src/remote.rs` (add + test `should_warn_exposed`)

**Interfaces:**
- Produces: `pub fn should_warn_exposed(addr: &std::net::SocketAddr) -> bool` in `remote.rs` — true unless the bind IP is loopback or Tailscale CGNAT (`100.64.0.0/10`). Used by `main` to warn about a no-auth exposed bind.

- [ ] **Step 1: Write the failing test.** Add to `#[cfg(test)] mod tests` in `crates/clowder-daemon/src/remote.rs`:

```rust
    #[test]
    fn exposure_warning_predicate() {
        use std::net::SocketAddr;
        let addr = |s: &str| s.parse::<SocketAddr>().unwrap();
        // loopback and tailnet (100.64/10) are the sanctioned Phase-A binds → no warning
        assert!(!should_warn_exposed(&addr("127.0.0.1:7777")));
        assert!(!should_warn_exposed(&addr("[::1]:7777")));
        assert!(!should_warn_exposed(&addr("100.101.102.103:7777")));
        // anything else (all-interfaces / LAN / public) has no auth in Phase A → warn
        assert!(should_warn_exposed(&addr("0.0.0.0:7777")));
        assert!(should_warn_exposed(&addr("192.168.1.10:7777")));
    }
```

- [ ] **Step 2: Run test to verify it fails.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon exposure_warning_predicate 2>&1 | tail -20`
Expected: FAIL — `should_warn_exposed` not found.

- [ ] **Step 3: Write minimal implementation.** Add to `crates/clowder-daemon/src/remote.rs` (outside `impl Daemon`, above the tests):

```rust
use std::net::{IpAddr, SocketAddr};

/// Phase A has no auth, so binding anywhere but loopback or the Tailscale CGNAT
/// range (100.64.0.0/10) deserves a startup warning. Returns true = warn.
pub fn should_warn_exposed(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            let is_tailnet = o[0] == 100 && (64..=127).contains(&o[1]); // 100.64.0.0/10
            !(v4.is_loopback() || is_tailnet)
        }
        IpAddr::V6(v6) => !v6.is_loopback(),
    }
}
```

- [ ] **Step 4: Run test to verify it passes.**
Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon exposure_warning_predicate 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Wire it into `main.rs`.** In `crates/clowder-daemon/src/main.rs`, capture the config value before the `Daemon::new_from_config(config)` move (it consumes `config`). After `let control_path = config.control_sock.clone();` (line 12) add:

```rust
    let remote_listen = config.remote_listen.clone();
```
After the three Unix listeners are bound + the `tracing::info!("clowder-daemon listening")` block (after line 46), add:

```rust
    if let Some(addr_str) = remote_listen {
        let addr: std::net::SocketAddr = addr_str
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid [remote] listen address {addr_str:?}: {e}"))?;
        let tcp = tokio::net::TcpListener::bind(addr).await?;
        if clowder_daemon::remote::should_warn_exposed(&addr) {
            tracing::warn!(%addr, "remote listener bound to a non-loopback/non-tailnet address — Phase A has NO authentication; expose only behind a trusted tunnel (SSH -L / Tailscale)");
        }
        tracing::info!(%addr, "clowder-daemon remote TCP listener enabled");
        let remote = daemon.clone();
        tokio::spawn(async move {
            if let Some(line) = clowder_daemon::logging::conn_error_line("remote server", remote.serve_remote(tcp).await) {
                tracing::error!("{line}");
            }
        });
    }
```

- [ ] **Step 6: Verify it builds + full suite green.**
Run: `source "$HOME/.cargo/env" && cargo build -p clowder-daemon && cargo test --workspace --locked 2>&1 | tail -15`
Expected: builds; all tests pass.

- [ ] **Step 7: Manual smoke (real TCP).** In one shell: `source "$HOME/.cargo/env" && CLOWDER_LISTEN=127.0.0.1:7777 CLOWDER_SOCK=/tmp/m7a-c.sock CLOWDER_CONTROL_SOCK=/tmp/m7a-ctl.sock CLOWDER_HOOK_SOCK=/tmp/m7a-hook.sock cargo run -p clowder-daemon` → log shows "remote TCP listener enabled" and NO exposure warning. In another shell: `printf '\x01' | nc 127.0.0.1 7777 | head -c 200 | cat -v` (send a Control hello byte) → prints an `agentList` JSON line. Ctrl-C the daemon.

- [ ] **Step 8: Commit.**

```bash
git add crates/clowder-daemon/src/main.rs crates/clowder-daemon/src/remote.rs
git commit -m "feat(daemon): bind opt-in remote TCP listener from [remote] listen"
```

---

## Self-Review

- **Spec coverage (M7a slice):** TCP listener opt-in via `[remote] listen` (Tasks 2, 4) ✓; one-byte channel hello demux reusing existing handlers (Tasks 1, 3) ✓; hook channel never exposed (Task 3 routes only Control/Render) ✓; off-by-default (Task 4 `if let Some`) ✓; loopback/tailnet exposure warning (Task 4) ✓. M7b (forwarder) and M7c (app) are separate plans; `Channel`/`write_hello` (Task 1) are the shared interface M7b consumes.
- **Placeholders:** none — every step has runnable code/commands.
- **Type consistency:** `Channel`, `read_hello`/`write_hello` (Task 1) used verbatim in Task 3; `remote_listen: Option<String>` (Task 2) read in Task 4; handler names match the existing `pub` signatures quoted in Global Constraints.
