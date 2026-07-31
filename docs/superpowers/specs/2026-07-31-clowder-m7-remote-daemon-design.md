# clowder M7 — Remote daemon (connect from another machine)

## Context

clowder's daemon and clients talk over **local Unix sockets** (`clowder.sock` render, `clowder-control.sock`
control, `clowder-hook.sock` hooks). M7 lets the daemon run on a **remote host** and the macOS app attach
over the network, realizing the north-star's remote seam (a remote-desktop daemon; the phone client stays
post-v1). Agents, worktrees, and hooks stay **local to the daemon host**; only the render + control
channels cross the network.

### What exists (ground truth, verified 2026-07-31)

- **The protocol is already transport-generic.** `MsgStream<S>` (`crates/clowder-proto/src/transport.rs:10`)
  wraps `Framed<S, LengthDelimitedCodec>` (length-prefix + postcard) over **any** `S: AsyncRead + AsyncWrite
  + Unpin`. Every daemon handler — `handle_conn` (render, `server.rs`), `handle_control_json`
  (`control_json.rs`), `handle_hook_conn` (`attention.rs`) — and the client `pump` (`client/src/lib.rs`) are
  generic over the byte stream. The **only** Unix-specific code is the three `UnixListener::bind`/`accept`
  loops (`daemon/src/main.rs:37-39`) and the client's `UnixStream::connect` call sites.
- The `Transport` trait (`transport.rs:8`) is an empty, **unused** marker (`pub trait Transport: Send {}`).
- **Two wire channels:** render is binary postcard (`ClientToDaemon`/`DaemonToClient`); control is
  newline-delimited JSON (`ControlRequest`/`ControlEvent`). Hooks are one-shot `HookEvent` (local only).
- **The app connects to local sockets:** Swift `UnixSocketConnection` (POSIX `AF_UNIX`) for control, behind
  the `ControlTransport` protocol + injected `makeTransport` (`AppModel.swift`); render is per-pane
  `clowder attach <pane>` (Rust CLI) run inside libghostty surfaces. **M5d reconnect** (control-only, bounded
  backoff 0.5→10 s) lives in `AppModel`.
- **`Config` is filesystem-only** (`clowder-config/src/lib.rs`): `client_sock`/`control_sock`/`hook_sock`
  paths, no host/port. The Swift side mirrors path resolution in `ClowderPaths` (`DaemonLaunch.swift`).
- **Network stack is greenfield** — no TLS/QUIC/auth deps or code anywhere.

### User decisions (brainstorm, 2026-07-31)

- **Phased security (C): build Phase A now, design Phase B behind the same seam.** Phase A leans on a
  **user-provided tunnel** (SSH `-L` and Tailscale, both targeted) for encryption + access control; clowder
  adds no crypto/auth. Phase B (native TLS/QUIC + device pairing/token auth) is designed but deferred.
- **App-integrated (C): config-driven now, richer UI later.** A `[remote]` config selects local vs remote;
  a minimal menu-bar status + "Use local" item; a fuller connection UI comes later.
- **Connection model:** per-connection TCP + a channel `Hello` frame (one port), not a mux layer (deferred).

## Goals / Non-goals

**Goals:** (1) the daemon can **listen on TCP** (opt-in, off by default, bound to loopback/tailnet); (2) a
**client forwarder** (`clowder connect <host:port>`) presents the usual **local** Unix sockets and pipes
them to the remote over one TCP port; (3) the **macOS app** gains a config-driven **remote mode** — its
backend supervisor runs the forwarder instead of a local daemon, keeping all socket-speaking + M5d reconnect
unchanged — plus a menu-bar status/disconnect; (4) full feature parity over the link (spawn, attach, splits,
land/discard) since they all ride the existing control+render channels; (5) reconnect that composes with M5d.

**Non-goals:** native TLS/QUIC or any auth/pairing (Phase B — designed here, **not built**; until B lands the
daemon must bind only loopback/tailnet, never public); stream multiplexing (per-connection is fine over a
tunnel); a multi-host connection-manager UI; a mobile/phone client; moving hooks/agents off the daemon host.

## Component design

### M7a — Daemon TCP listener + channel `Hello` (BUILD)

A new **opt-in** TCP listener alongside the existing Unix accept loops. Enabled when `[remote] listen` is
set (e.g. `127.0.0.1:7777`); empty/absent ⇒ off (today's behavior, no network surface). Each accepted TCP
connection begins with a single postcard-framed **`Hello { channel: Control | Render }`** frame (a new type
in `clowder-proto`), read via `MsgStream`; the daemon then hands the raw stream to the **existing**
`handle_control_json` or `handle_conn` — both already generic over `AsyncRead+AsyncWrite+Unpin+Send`, so no
handler changes. The **hook channel is never exposed** on TCP (agents run on the daemon host). **Rationale:**
one port keeps `ssh -L`/Tailscale/firewall config to a single forward; the `Hello` demux is a few lines and
reuses all protocol + handler code. (Alternatives: two ports — more forwards; yamux mux — deferred.)

### M7b — Client forwarder `clowder connect <host:port>` (BUILD)

A new `clowder` subcommand (and reusable lib fn) that binds **local** Unix sockets for render + control in a
**dedicated per-user subdir** (so they never collide with a local daemon's sockets), then, for each local
connection: dial the remote TCP, send the matching `Hello`, and pipe bytes bidirectionally (`tokio::io::copy`
both ways) until either side closes. It reconnects to the remote with bounded backoff and logs clearly; it
holds no single-instance flock. **This is the single seam where Phase B wraps the TCP dial in TLS/QUIC** — no
other component changes when B lands. The forwarder prints the local socket paths (for `CLOWDER_SOCK`/
`CLOWDER_CONTROL_SOCK`) so a **pure-CLI** remote flow works too (run the forwarder, point `clowder attach` /
env at it).

### M7c — macOS app remote mode (BUILD)

The M6a backend supervisor branches on config: **remote configured** ⇒ supervise `clowder connect <host>`
(instead of `clowder-daemon`) and set the surfaces' `CLOWDER_SOCK` + the app's control socket to the
forwarder's local sockets; **unset** ⇒ today's local daemon. The app's `UnixSocketConnection`, `clowder
attach`, and M5d reconnect are **unchanged** — they still speak local sockets, now backed by the forwarder.
UI: a menu-bar status line ("Remote: `<host>`") and a "Use local" item that restarts in local mode. The Swift
config mirror (`ClowderPaths`) gains the `[remote]` read. Kept testable like the M6a supervisor (inject the
spawn; unit-test the local-vs-remote branch).

### Config (`clowder-config` + Swift mirror)

New `[remote]` section + env, resolved env › file › default like existing keys:
- **daemon:** `listen` (socket addr, e.g. `127.0.0.1:7777`; empty = off) — env `CLOWDER_LISTEN`.
- **client:** `host` (e.g. `localhost:7777` for SSH `-L`, `100.x.y.z:7777` for Tailscale) — env `CLOWDER_REMOTE_HOST`.
Added to the Rust `Config` struct and the Swift config read; the forwarder's local socket dir is a fixed
per-user subpath.

### M7d — Phase B: native encrypted+authenticated transport (DESIGN — DEFERRED)

Not built this cycle. Design: replace the plain TCP dial/accept with **TLS (tokio-rustls) or QUIC (quinn)**
plus a **device pairing/token auth** flow (the daemon issues/accepts a pairing token; the client stores it;
the daemon rejects unauthenticated peers), so the daemon can be exposed on a public address. It lands
entirely in the **forwarder's dial** and the **daemon's accept** (behind the `Hello`), with credentials in
config/keychain — **no protocol, handler, or app changes**. This is the seam M7a/M7b deliberately preserve.

## Data flow

```
build:   clowder-daemon (M7a): [remote] listen=127.0.0.1:7777 → TCP listener + Unix listeners
         clowder connect <host:port> (M7b): local render/control Unix socks ⇄ remote TCP
runtime (remote mode):
  macOS app (M7c) ─ supervises → clowder connect <host>            (instead of a local daemon)
      app control  → local control sock ─┐
      clowder attach <pane> → local render sock ─┤ Hello{Control|Render} → TCP (ssh -L / tailscale) → daemon
                                                 └ existing handle_control_json / handle_conn
      hooks: never leave the daemon host (agents co-located)
  TCP drop → local sock drops → M5d backoff re-dials local → forwarder re-dials remote → live
deferred (M7d): TLS/QUIC + pairing-token auth wraps the TCP dial/accept only
```

## Decomposition (each its own plan → SDD → PR)

- **M7a — Daemon TCP listener + `Hello` routing + `[remote] listen`.** BUILD.
- **M7b — Client forwarder `clowder connect` + `[remote] host`.** BUILD.
- **M7c — macOS app remote mode** (supervisor branch + status/menu) + Swift config mirror. BUILD.
- **M7d — Phase B native TLS/QUIC + device auth/pairing.** DEFERRED (design only).

Order: **M7a → M7b → M7c**, then M7d when direct exposure is needed.

## Testing

- **M7a (`cargo test`):** a `Hello{Control}`/`Hello{Render}` over a loopback `TcpStream` routes to the right
  handler and round-trips a request/response (mirrors the existing `messages_roundtrip_over_duplex`); `listen`
  empty ⇒ no TCP bind.
- **M7b (`cargo test`):** stand up a daemon on `127.0.0.1:0`, run the forwarder in front, drive a control
  `SpawnAgent` + an `attach` through the local sockets, assert round-trip; kill the remote mid-stream and
  assert the forwarder re-dials (bounded backoff), reusing M5d-style deterministic seams.
- **M7c (`swift test`, ClowderCore):** the supervisor selects `clowder connect` + forwarder sockets when
  remote is configured and the local daemon otherwise (inject the spawn; assert the branch); AppModel
  reconnect unchanged.
- **Manual (user):** a daemon on a second machine reached via **`ssh -L 7777:localhost:7777 host`** and via a
  **Tailscale IP**; spawn/attach/split/land over the link; drop the network → the app shows "Reconnecting…"
  then recovers; confirm hooks/agents stay on the daemon host.

## Risks

1. **Accidental public exposure** (no auth in Phase A). Mitigation: `listen` **off by default**, docs bind
   loopback/tailnet only, and a startup warning if it binds a non-loopback/non-tailnet address. Phase B adds
   real auth before public exposure is supported.
2. **Reconnect must compose with M5d, not fight it.** The forwarder reconnects the *remote* leg; the app
   reconnects the *local* leg. Covered by the M7b re-dial test + the existing M5d tests; the two compose
   because the local socket drop is what M5d already handles.
3. **Socket collision** between a forwarder and a local daemon on the same machine. Mitigation: the forwarder
   uses a dedicated per-user socket subdir.
4. **Latency/throughput** of byte streaming over a tunnel. Framing is already length-delimited; per-connection
   TCP is adequate for a terminal. Mux (throughput/one-connection) is the deferred optimization if needed.

## Verification gate

Per slice: its tests green + existing suites green. **M7 end state (A across M7a–c):** a daemon started with
`[remote] listen` on a second host is reachable through `ssh -L` **or** Tailscale; the macOS app in remote
mode (config-driven) supervises the forwarder and drives spawn/attach/splits/land live, with agents +
worktrees + hooks staying on the daemon host and the link surviving a brief drop (forwarder re-dial + M5d
reconnect). Deferred: **M7d** native TLS/QUIC + device auth (design here), and (out of M7) the M8 Linux
client and a mobile client.
