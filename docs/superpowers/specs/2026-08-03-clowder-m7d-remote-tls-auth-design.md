# clowder M7d — native encrypted + authenticated remote transport

## Context

M7 gave the daemon an **opt-in remote TCP listener** (`[remote] listen`) and a client **forwarder**
(`clowder connect <host:port>`) that pipes local Unix-socket connections to the remote daemon behind a
one-frame `Hello { channel }`. M7's **Phase A** ships plaintext TCP and relies on the user wrapping it in
an SSH `-L` / Tailscale tunnel for encryption + access control; the daemon adds no crypto or auth, and a
startup warning fires if it binds a non-loopback/non-tailnet address (Risk #1: accidental public exposure).

M7d is the deferred **Phase B**: wrap the same seam with **TLS + a bearer token** so the daemon can be
exposed on a public address without a tunnel. The M7 spec deliberately preserved this seam
(`docs/superpowers/specs/2026-07-31-clowder-m7-remote-daemon-design.md` §M7d).

### What exists (ground truth, verified 2026-08-03)

- **Daemon accept** (`crates/clowder-daemon/src/remote.rs`): `serve_remote(self, listener)` loops
  `listener.accept()?` → spawns `handle_remote_conn<S: AsyncRead+AsyncWrite+Unpin+Send>(stream)`, which
  reads the channel `Hello` (with an existing `HELLO_TIMEOUT`) then dispatches to `handle_control_json`
  (Control) or `handle_conn` (Render). The hook channel is never exposed on TCP.
- **Accept loop uses `?`** (`remote.rs:20`): a single `accept()` error terminates the whole listener —
  the hardening item M7d folds in.
- **Client dial** (`crates/clowder-client/src/forward.rs`): `forward_stream<L>(local, host, channel)`
  dials `TcpStream::connect(host)` (with bounded retry, `:14`), sends the `Hello`, then
  `tokio::io::copy_bidirectional(local, remote)` (`:39`). The client accept loop already logs+continues
  on error (`:64`).
- **Config** (`crates/clowder-config/src/lib.rs`): `struct Remote { listen: Option<String>, host:
  Option<String> }` → resolved `remote_listen` / `remote_host` (env › file › default: `CLOWDER_LISTEN` /
  `CLOWDER_REMOTE_HOST`).
- **`should_warn_exposed(addr)`** (`remote.rs:48`) warns on non-loopback/non-tailnet binds — kept.
- **No crypto deps** anywhere: `rustls`/`tokio-rustls`/`rcgen`/`sha2`/`subtle`/`base64` are all new.
- State dir precedent (M9a): `$CLOWDER_STATE_FILE › $XDG_STATE_HOME/clowder/ › ~/.local/state/clowder/`.

### User decisions (brainstorm, 2026-08-03)

- **Transport: TLS over TCP (`tokio-rustls`/`rustls`)** — wrap the `TcpStream` in a `TlsStream`; the seam
  stays an `AsyncRead+AsyncWrite` stream, so handlers/forwarder are unchanged. (Not QUIC.)
- **Client auth: a pre-shared bearer token** the daemon issues; client presents it. (Not mTLS.)
- **Daemon cert trust: TOFU** (trust-on-first-use, SSH host-key style) — client records the daemon's cert
  fingerprint on first connect, refuses on later change. (Not a configured-fingerprint pin.)
- **Mode: opt-in** — plaintext TCP stays the default (backward compatible); TLS+token is enabled by config.

## Goals / Non-goals

**Goals:** (1) an **opt-in** TLS+token mode for the remote listener/forwarder, wrapping only the dial and
accept; (2) daemon auto-provisions a self-signed cert + token, printed once + re-printable; (3) TOFU cert
verification on the client with a hard refuse on fingerprint change; (4) constant-time token check, drop
before dispatch on failure; (5) `serve_remote`'s accept loop survives a transient `accept()` error;
(6) never crash the daemon/forwarder on any handshake/verify/IO failure.

**Non-goals:** mTLS / client certs; QUIC; a configured-fingerprint pin or a full pairing UX; macOS
**Keychain** storage (the Rust forwarder uses config/env; the app is untouched); any **handler, local
Unix-path, or macOS-app** change; multiplexing; making TLS the default (plaintext stays default).

## Component design

### 1. Config + the `Hello` token field

- `clowder-config`: `Remote { listen, host, tls: bool, token: Option<String> }`; resolved fields
  `remote_tls: bool` (env `CLOWDER_REMOTE_TLS`, default false) and `remote_token: Option<String>` (env
  `CLOWDER_REMOTE_TOKEN`), same env › file › default resolution as the existing keys. **Role split**
  (mirrors the existing `listen`=daemon / `host`=client split): `tls` is the **daemon-side** enable switch
  (the daemon uses its own state-dir token); `token` is the **client-side** credential (the daemon's token,
  copied by the user). The client turns TLS on when `remote_token` is set — a token is only useful over
  TLS — so it needs no separate client `tls` flag.
- `clowder-proto`: the **remote-only** `Hello { channel }` gains `token: Option<String>` →
  `Hello { channel: Channel, token: Option<String> }`. Plaintext path sends `None` (today's behavior); the
  TLS client sends the configured token. This is the only protocol touch, and it is remote-path-only —
  the local Unix sockets never send a `Hello`, and handlers/app are unchanged. (Postcard is not
  self-describing; M7d ships daemon+client together, so the field addition needs no version negotiation.)

### 2. Cert + token provisioning (daemon)

A new `clowder-daemon` module (e.g. `remote_tls.rs`) owns credential lifecycle:
- On daemon start with `remote_tls = true`, load-or-generate in the state dir (`0600`):
  `remote-cert.pem`, `remote-key.pem` (self-signed via `rcgen`, CN/SAN `clowder`), `remote-token`
  (32 random bytes → base64url). Reused verbatim on restart.
- **Fingerprint** = `sha2::Sha256` of the cert **DER**, lowercase hex. Printed once at startup alongside
  the token; a new `clowder remote-token` subcommand re-prints `token` + `fingerprint` (reads the same
  state files). Rotation = delete `remote-token` (regenerated next start); a new cert = delete the pem
  pair (clients then hit the TOFU-change refuse and must re-trust).
- If `remote_tls = true` but the state dir is unwritable / generation fails → the daemon logs a clear
  fatal error and does **not** fall back to plaintext (fail closed).

### 3. Daemon TLS accept + token verify (`remote.rs`)

- `serve_remote` builds a `tokio_rustls::TlsAcceptor` from the loaded cert+key when `remote_tls`, else
  stays plaintext. Per accepted TCP connection: if TLS, `acceptor.accept(tcp).await` (bounded by a
  handshake timeout) → the resulting `TlsStream` flows into the **unchanged** `handle_remote_conn<S>`.
- `handle_remote_conn` reads `Hello` as today; when the connection is TLS, it **requires**
  `hello.token == Some(t)` with `t` matching the daemon token **constant-time** (`subtle` or a hand-rolled
  compare) — mismatch/absent ⇒ drop before any dispatch. Plaintext listener: token ignored (Phase A).
- **Accept-loop hardening:** replace `listener.accept()?` with a match that logs a transient error and
  `continue`s (as the client forwarder does), so the listener survives a bad accept.
- Every failure path (handshake timeout, bad token, IO) logs via the existing `conn_error_line` and drops
  only that connection.

### 4. Client TLS dial + TOFU verify (`forward.rs`)

- The forwarder uses TLS when a token is configured (`remote_token.is_some()`), else plaintext (today).
- Build a `tokio_rustls::TlsConnector` with a **custom `ServerCertVerifier`** implementing TOFU against a
  `known_hosts`-style file in the client state dir (`clowder/remote_known_hosts`, lines `<host> <sha256>`):
  first sight of a host records its cert fingerprint and accepts; a later mismatch **refuses** with a loud,
  SSH-style warning ("REMOTE DAEMON IDENTIFICATION HAS CHANGED"); a match accepts. (Verifier checks only
  the pinned fingerprint — not a CA chain — since the cert is self-signed.)
- After the handshake, the client sends `Hello { channel, token: Some(<configured>) }`, then
  `copy_bidirectional` over the `TlsStream`. A missing token with a TLS host, or a fingerprint-change
  refuse, is a clear connect error (retried with the existing backoff for transient cases; a
  fingerprint-change is logged prominently, not silently retried into a loop).

## Data flow

```
daemon start ([remote] tls=true):
  load/gen state-dir cert+key+token (0600) → print token + sha256 fingerprint
  serve_remote → TlsAcceptor; accept loop survives transient accept() errors
client (remote_token set):
  TcpStream::connect(host) → TlsConnector(TOFU verifier).connect(tcp)
     first connect: record host→fingerprint; later: verify or REFUSE on change
  → send Hello{channel, token} → copy_bidirectional over TlsStream
daemon per TLS conn:
  acceptor.accept(tcp) → read Hello → constant-time token check (drop on fail) → handle_control_json/handle_conn
plaintext path (tls unset): unchanged Phase A behavior (token None, no TLS)
```

## Error handling

- **Fail closed:** `tls=true` + missing/unwritable creds → fatal daemon startup error; no plaintext
  fallback. Client `tls` host + no token → clear connect error.
- **Per-connection failures** (handshake timeout, cert-verify refuse, bad/absent token, IO) → log + drop
  that connection only; the daemon/forwarder keep running.
- **TOFU fingerprint change** → hard refuse + prominent warning; not silently retried (a MITM or a
  legitimately-rotated daemon cert both surface loudly; the user deletes the `known_hosts` line to
  re-trust).
- **Transient `accept()` error** → logged + loop continues (listener never dies on one bad accept).

## Testing

- **Config:** `remote_tls` / `remote_token` resolve env › file › default (mirror existing `[remote]` tests).
- **`Hello` token field:** round-trips through postcard with `Some`/`None`.
- **Cert/token gen:** generation writes `0600` files, is idempotent (reused on second call), and the
  fingerprint is the SHA-256 of the cert DER (stable across reloads). `remote-token` prints them.
- **Constant-time token compare:** equal/unequal, length-mismatch — returns correct result (behavior test).
- **Daemon TLS round-trip (loopback):** with a generated cert+token, a client using the matching
  fingerprint+token completes a **Control** and a **Render** channel and the handler sees the same bytes
  as the plaintext path; a **wrong/absent token** is rejected (connection dropped, no dispatch).
- **TOFU verifier:** first connect records the fingerprint + accepts; a second connect with the same cert
  accepts; a connect with a **different** cert for the same host refuses; a new host records fresh.
- **Accept-loop resilience:** an injected transient `accept()` error does not terminate `serve_remote`
  (e.g. via a test seam / a fake listener), and a subsequent good connection still succeeds.
- **`should_warn_exposed`** predicate unchanged (existing test stays green).
- Full suite green (`cargo test --workspace --locked`).

## Risks

1. **TOFU first-connect MITM window** — accepted (user's choice). Mitigated by: the token still gates
   client auth (a MITM without the daemon token can't impersonate the daemon to the client *and* can't
   present a valid token to the daemon), the fingerprint-change refuse catches later MITM, and the daemon
   fingerprint is printed for optional out-of-band verification. Documented in the threat-model doc.
2. **Token leakage** (config/env readable) — `0600` on the daemon side; the client token lives in the
   user's config/env like other secrets; rotation is a documented one-liner. Keychain is a future option.
3. **New crypto deps** (`rustls`/`tokio-rustls`/`rcgen`/`sha2`) enlarge the build + audit surface — all
   are widely-used pure-Rust crates; pinned via `Cargo.lock` (CI `--locked`).
4. **Postcard `Hello` field addition** breaks wire-compat with a Phase-A-only forwarder — acceptable:
   M7d ships both sides; plaintext still sends a (now two-field) `Hello` that a matching daemon reads.
5. **Fail-closed on missing creds** could surprise (daemon won't start) — deliberate: better than silently
   serving plaintext when the user asked for TLS. Clear error message.

## Verification gate

With `[remote] tls = true`, the daemon serves the remote listener over TLS using an auto-provisioned
self-signed cert + token (printed once, re-printable via `clowder remote-token`); a forwarder configured
with the token completes control+render channels over TLS, recording the daemon's fingerprint on first use
(TOFU) and **refusing** on a later fingerprint change; a wrong/absent token is rejected before dispatch;
the accept loop survives a transient error; and nothing — handlers, the local Unix path, or the macOS app
— changed. Plaintext remote (Phase A) still works unchanged when `tls` is unset. Deferred: mTLS, QUIC,
configured-pin/pairing UX, and Keychain storage.
