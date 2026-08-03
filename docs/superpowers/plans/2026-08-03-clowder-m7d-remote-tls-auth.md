# M7d — remote TLS + token auth Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An opt-in TLS+bearer-token mode for the clowder remote listener/forwarder so the daemon can be exposed without a tunnel, with TOFU cert trust.

**Architecture:** Wrap only the existing seam — the daemon's TCP accept (`remote.rs`) and the forwarder's TCP dial (`forward.rs`) — in `tokio-rustls`. The daemon auto-provisions a self-signed cert + token in its state dir; the token rides on the (extended) remote-only `Hello`; the client verifies the cert TOFU and presents the token. Handlers, the local Unix path, and the macOS app are untouched.

**Tech Stack:** Rust 2021, `tokio-rustls`/`rustls` (ring provider), `rcgen` (self-signed cert), `sha2`+`hex` (fingerprint), `base64`+`getrandom` (token).

## Global Constraints

- **Prefix every cargo command** with `source "$HOME/.cargo/env" && ` (rustup not auto-sourced here).
- **Edition 2021, stable.** CI runs `cargo test --workspace --locked` — commit the regenerated `Cargo.lock`.
- **Use the rustls `ring` crypto provider** (NOT aws-lc-rs — avoids C/cmake build deps). Build TLS configs with `…::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider())).with_safe_default_protocol_versions()?` so no global provider install is needed. Set crate features to `ring` explicitly.
- **Opt-in:** plaintext remote (Phase A) stays the **default**. TLS is enabled daemon-side by `[remote] tls = true`; the client enables TLS when `[remote] token` is set. `tls` = daemon switch, `token` = client credential (mirrors `listen`=daemon / `host`=client).
- **Never crash on IO/handshake/verify failure** — log + drop the one connection. The daemon accept loop must survive a transient `accept()` error (log + continue).
- **Fail closed:** `tls=true` with missing/unwritable creds → fatal daemon startup error, no plaintext fallback.
- **TOFU:** first connect records the daemon fingerprint; a later change is a **hard refuse** with a loud warning; never silently retried into a loop.
- **Constant-time token compare;** creds files are `0600`.
- **No handler / local-Unix-path / macOS-app change.** The only protocol touch is the remote-only `Hello` gaining a token; the local sockets never send a `Hello`.
- **rustls 0.23 / tokio-rustls 0.26 / rcgen 0.13 APIs are version-sensitive.** The code below targets those versions; if a builder method name differs in the resolved patch, adjust it to achieve the same shape (ring provider, single self-signed cert, no client auth, custom TOFU verifier) — the TDD round-trip test is the correctness gate.

---

### Task 1: Shared foundation — config fields + paths, `Hello` token, auth primitives

**Files:**
- Modify: `crates/clowder-config/src/lib.rs` (`Remote` struct + resolved fields + cred path helpers)
- Modify: `crates/clowder-proto/src/remote.rs` (`write_hello`/`read_hello` gain a token) + `crates/clowder-proto/src/lib.rs` (re-exports)
- Create: `crates/clowder-proto/src/auth.rs` (`constant_time_eq`, `cert_fingerprint_hex`) + wire into `lib.rs`
- Modify: `crates/clowder-proto/Cargo.toml` (add `sha2`, `hex`)

**Interfaces:**
- Produces:
  - `Config` gains `pub remote_tls: bool` and `pub remote_token: Option<String>`.
  - `clowder_config::remote_state_dir() -> PathBuf`, and `remote_cert_path()/remote_key_path()/remote_token_path() -> PathBuf` (all under the state dir).
  - `write_hello<W>(w, channel: Channel, token: Option<&str>)` and `read_hello<R>(r) -> Result<(Channel, Option<String>)>`.
  - `clowder_proto::constant_time_eq(a: &[u8], b: &[u8]) -> bool`; `clowder_proto::cert_fingerprint_hex(cert_der: &[u8]) -> String` (lowercase hex SHA-256).

- [ ] **Step 1: Write the failing tests**

In `crates/clowder-proto/src/auth.rs` (new file), add a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_and_mismatches() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));   // length mismatch
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn fingerprint_is_lowercase_hex_sha256() {
        // SHA-256("") = e3b0c442... ; 64 hex chars.
        let fp = cert_fingerprint_hex(b"");
        assert_eq!(fp.len(), 64);
        assert_eq!(&fp[..8], "e3b0c442");
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
```

In `crates/clowder-proto/src/remote.rs`, replace the existing `hello_roundtrips_both_channels` test body and add a token case:

```rust
#[tokio::test]
async fn hello_roundtrips_channel_and_token() {
    for (ch, tok) in [
        (Channel::Control, None),
        (Channel::Render, Some("s3cr3t-token".to_string())),
    ] {
        let (mut a, mut b) = tokio::io::duplex(64);
        write_hello(&mut a, ch, tok.as_deref()).await.unwrap();
        let (rch, rtok) = read_hello(&mut b).await.unwrap();
        assert_eq!(rch, ch);
        assert_eq!(rtok, tok);
    }
}
```

In `crates/clowder-config/src/lib.rs` tests, add:

```rust
#[test]
fn remote_tls_and_token_resolve_env_over_file() {
    let f = FileConfig { remote: Some(Remote {
        listen: None, host: None, tls: Some(true), token: Some("filetok".into()),
    }), ..Default::default() };
    let env = |k: &str| match k { "CLOWDER_REMOTE_TOKEN" => Some("envtok".to_string()), _ => None };
    let c = Config::resolve(f, &env);
    assert!(c.remote_tls);
    assert_eq!(c.remote_token.as_deref(), Some("envtok"));
}

#[test]
fn remote_tls_defaults_false_and_token_none() {
    let c = Config::resolve(FileConfig::default(), &|_| None);
    assert!(!c.remote_tls);
    assert_eq!(c.remote_token, None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-proto -p clowder-config 2>&1 | tail -20`
Expected: compile errors (`auth` module missing; `write_hello` arity; `Remote.tls`/`remote_tls` missing).

- [ ] **Step 3: Implement**

`crates/clowder-proto/Cargo.toml` — add under `[dependencies]`:

```toml
sha2 = "0.10"
hex = "0.4"
```

Create `crates/clowder-proto/src/auth.rs`:

```rust
//! Small auth primitives shared by the remote daemon (token check) and the client (cert TOFU).

use sha2::{Digest, Sha256};

/// Length-checked, data-independent byte comparison for secret material.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Lowercase-hex SHA-256 of a certificate's DER bytes — the TOFU fingerprint.
pub fn cert_fingerprint_hex(cert_der: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(cert_der);
    hex::encode(h.finalize())
}
```

In `crates/clowder-proto/src/lib.rs`, add `mod auth; pub use auth::{constant_time_eq, cert_fingerprint_hex};` and extend the existing remote re-export to keep `read_hello, write_hello, Channel`.

In `crates/clowder-proto/src/remote.rs`, replace `write_hello`/`read_hello` (keep `Channel`/`to_byte`/`from_byte` as-is). The wire is `[channel: u8][token_len: u16 BE][token: token_len bytes UTF-8]`; `token_len == 0` ⇒ `None`:

```rust
use tokio::io::AsyncReadExt; // for read_u8/read_u16/read_exact

/// Write the channel hello (channel byte + length-prefixed optional token) that prefixes a
/// remote connection. The token is present only on the TLS path; plaintext sends `None`.
pub async fn write_hello<W: AsyncWrite + Unpin>(
    w: &mut W,
    channel: Channel,
    token: Option<&str>,
) -> Result<()> {
    w.write_u8(channel.to_byte()).await?;
    let bytes = token.map(str::as_bytes).unwrap_or(&[]);
    if bytes.len() > u16::MAX as usize {
        bail!("hello token too long");
    }
    w.write_u16(bytes.len() as u16).await?;
    w.write_all(bytes).await?;
    w.flush().await?;
    Ok(())
}

/// Read the channel hello + optional token. Bounds the token length so a hostile peer cannot
/// force a large allocation.
pub async fn read_hello<R: AsyncRead + Unpin>(r: &mut R) -> Result<(Channel, Option<String>)> {
    let channel = Channel::from_byte(r.read_u8().await?)?;
    let len = r.read_u16().await? as usize;
    if len > 4096 {
        bail!("hello token length {len} exceeds limit");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    let token = if len == 0 { None } else { Some(String::from_utf8(buf)?) };
    Ok((channel, token))
}
```

In `crates/clowder-config/src/lib.rs`:
- Extend `struct Remote` to `struct Remote { listen: Option<String>, host: Option<String>, tls: Option<bool>, token: Option<String> }` (keep `#[derive(...Default...)]` / serde as the struct already has).
- Add resolved fields to `Config`: `pub remote_tls: bool,` and `pub remote_token: Option<String>,`.
- In `resolve`, after the existing remote lines:
  ```rust
  remote_tls: env_bool("CLOWDER_REMOTE_TLS").unwrap_or(r.tls.unwrap_or(false)),
  remote_token: nonempty("CLOWDER_REMOTE_TOKEN").or(r.token.filter(|s| !s.is_empty())),
  ```
  where `env_bool` mirrors the existing `nonempty` helper's env access. Add it next to `nonempty` (adapt to how `nonempty` reads the env closure in this file):
  ```rust
  // parses CLOWDER_REMOTE_TLS: "1"/"true" → Some(true), "0"/"false" → Some(false), else None
  let env_bool = |k: &str| env(k).and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
      "1" | "true" => Some(true),
      "0" | "false" => Some(false),
      _ => None,
  });
  ```
  (`env` is the same env-lookup closure `resolve` already uses for `nonempty`; if `nonempty` is a free fn capturing a different binding, match that style.)
- Add the cred-path helpers (state dir mirrors M9a's precedence):
  ```rust
  /// The durable per-user dir holding remote TLS creds: `$XDG_STATE_HOME/clowder` › `$HOME/.local/state/clowder` › `/tmp/clowder`.
  pub fn remote_state_dir() -> std::path::PathBuf {
      let base = std::env::var("XDG_STATE_HOME").ok().filter(|s| !s.is_empty())
          .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.local/state")))
          .unwrap_or_else(|| "/tmp".to_string());
      std::path::PathBuf::from(base).join("clowder")
  }
  pub fn remote_cert_path() -> std::path::PathBuf { remote_state_dir().join("remote-cert.pem") }
  pub fn remote_key_path() -> std::path::PathBuf { remote_state_dir().join("remote-key.pem") }
  pub fn remote_token_path() -> std::path::PathBuf { remote_state_dir().join("remote-token") }
  ```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-proto -p clowder-config 2>&1 | tail -20`
Expected: PASS. (The workspace will not fully build yet — `remote.rs`/`forward.rs` callers of `write_hello`/`read_hello` now have the wrong arity; those are fixed in Tasks 3–4. Build just these two crates here.)

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-proto crates/clowder-config Cargo.lock
git commit -m "feat(m7d): config tls/token + cred paths; Hello token; auth primitives"
```

---

### Task 2: Daemon credential provisioning (cert + token + fingerprint)

**Files:**
- Create: `crates/clowder-daemon/src/remote_tls.rs` (+ `mod remote_tls;` in `crates/clowder-daemon/src/lib.rs`)
- Modify: `crates/clowder-daemon/Cargo.toml` (add `rcgen`, `base64`, `getrandom`, `sha2` if needed; `clowder-proto` already a dep)

**Interfaces:**
- Consumes: `clowder_config::{remote_cert_path, remote_key_path, remote_token_path}`, `clowder_proto::cert_fingerprint_hex`.
- Produces:
  - `pub struct RemoteCreds { pub cert_pem: String, pub key_pem: String, pub token: String, pub cert_der: Vec<u8> }`.
  - `pub fn load_or_generate() -> anyhow::Result<RemoteCreds>` — loads the three state-dir files if all present, else generates + writes them `0600` (dir `0700`). Idempotent.
  - `pub fn fingerprint(creds: &RemoteCreds) -> String` (delegates to `cert_fingerprint_hex(&creds.cert_der)`).

- [ ] **Step 1: Write the failing test**

In `crates/clowder-daemon/src/remote_tls.rs` (new), add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_is_idempotent_and_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", dir.path());  // redirect remote_state_dir()

        let a = load_or_generate().unwrap();
        assert_eq!(a.token.len() >= 32, true, "token is non-trivial");
        // second call loads the SAME creds (no regeneration)
        let b = load_or_generate().unwrap();
        assert_eq!(a.token, b.token);
        assert_eq!(a.cert_pem, b.cert_pem);
        // fingerprint is a 64-char lowercase hex string, stable across loads
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_eq!(fingerprint(&a).len(), 64);
        // files are 0600
        for p in [clowder_config::remote_cert_path(), clowder_config::remote_key_path(), clowder_config::remote_token_path()] {
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} perms", p.display());
        }
        std::env::remove_var("XDG_STATE_HOME");
    }
}
```

> Note: this test mutates the process-global `XDG_STATE_HOME`. Guard it with the crate-wide `STATE_FILE_ENV_LOCK` added in M9b — acquire `let _g = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());` as the first line (it serializes all tests that mutate the process env for state-dir resolution). If `remote_state_dir` reads `XDG_STATE_HOME` (it does), this is required to avoid races with the M9a/M9b registry tests.

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon remote_tls:: 2>&1 | tail -20`
Expected: compile error (`load_or_generate`/`fingerprint`/`RemoteCreds` undefined).

- [ ] **Step 3: Implement**

`crates/clowder-daemon/Cargo.toml` — add:

```toml
rcgen = { version = "0.13", default-features = false, features = ["ring", "pem"] }
base64 = "0.22"
getrandom = "0.2"
```

Create `crates/clowder-daemon/src/remote_tls.rs`:

```rust
//! Remote TLS credential lifecycle: load-or-generate a self-signed cert + a bearer token in the
//! daemon state dir. Generation is idempotent; files are 0600.

use anyhow::{Context, Result};
use base64::Engine;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub struct RemoteCreds {
    pub cert_pem: String,
    pub key_pem: String,
    pub token: String,
    pub cert_der: Vec<u8>,
}

/// Load the three state-dir cred files if all exist; otherwise generate + persist them.
pub fn load_or_generate() -> Result<RemoteCreds> {
    let cert_p = clowder_config::remote_cert_path();
    let key_p = clowder_config::remote_key_path();
    let tok_p = clowder_config::remote_token_path();

    if cert_p.exists() && key_p.exists() && tok_p.exists() {
        let cert_pem = std::fs::read_to_string(&cert_p)?;
        let key_pem = std::fs::read_to_string(&key_p)?;
        let token = std::fs::read_to_string(&tok_p)?.trim().to_string();
        let cert_der = pem_cert_to_der(&cert_pem)?;
        return Ok(RemoteCreds { cert_pem, key_pem, token, cert_der });
    }

    // Generate a self-signed cert (SAN "clowder") + a 32-byte base64url token.
    let cert = rcgen::generate_simple_self_signed(vec!["clowder".to_string()])
        .context("generate self-signed cert")?;
    let cert_pem = cert.cert.pem();
    let key_pem = cert.key_pair.serialize_pem();
    let cert_der = cert.cert.der().to_vec();

    let mut raw = [0u8; 32];
    getrandom::getrandom(&mut raw).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);

    if let Some(dir) = cert_p.parent() {
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    write_0600(&cert_p, cert_pem.as_bytes())?;
    write_0600(&key_p, key_pem.as_bytes())?;
    write_0600(&tok_p, token.as_bytes())?;

    Ok(RemoteCreds { cert_pem, key_pem, token, cert_der })
}

pub fn fingerprint(creds: &RemoteCreds) -> String {
    clowder_proto::cert_fingerprint_hex(&creds.cert_der)
}

fn write_0600(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true).create(true).truncate(true).mode(0o600)
        .open(path)
        .with_context(|| format!("write {}", path.display()))?;
    f.write_all(bytes)?;
    // Enforce mode even if the file pre-existed with other perms.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Extract the first certificate's DER from a PEM string (for fingerprinting on load).
fn pem_cert_to_der(pem: &str) -> Result<Vec<u8>> {
    let mut rd = std::io::BufReader::new(pem.as_bytes());
    let first = rustls_pemfile::certs(&mut rd).next()
        .ok_or_else(|| anyhow::anyhow!("no certificate in PEM"))??;
    Ok(first.to_vec())
}
```

> **Implementer note (deps):** `pem_cert_to_der` uses `rustls-pemfile = "2"` — add it to `Cargo.toml` too (it's also needed in Task 3). If `rcgen 0.13`'s API differs (`cert.cert` / `cert.key_pair` / `.pem()` / `.der()` / `.serialize_pem()`), adjust to the resolved version's equivalents; the contract is: PEM cert, PEM key, and the cert DER bytes. Keep the `ring` feature (no aws-lc-rs).

Add `mod remote_tls;` to `crates/clowder-daemon/src/lib.rs` (near the other `mod` lines; make it `pub mod remote_tls;` so tests/main reach it).

- [ ] **Step 4: Run test to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon remote_tls:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/Cargo.toml crates/clowder-daemon/src/remote_tls.rs crates/clowder-daemon/src/lib.rs Cargo.lock
git commit -m "feat(m7d): daemon self-signed cert + token provisioning (0600, idempotent)"
```

---

### Task 3: Daemon TLS accept + token verify + accept-loop hardening

**Files:**
- Modify: `crates/clowder-daemon/src/remote.rs` (`serve_remote` signature + TLS accept + hardened loop; `handle_remote_conn` token check)
- Modify: `crates/clowder-daemon/src/main.rs` (build the `RemoteTls` when `remote_tls`, pass to `serve_remote`)
- Modify: `crates/clowder-daemon/Cargo.toml` (add `tokio-rustls`, `rustls`, `rustls-pemfile`)

**Interfaces:**
- Consumes: `remote_tls::{load_or_generate, fingerprint, RemoteCreds}`, `clowder_proto::{read_hello, constant_time_eq, Channel}`.
- Produces:
  - `pub struct RemoteTls { pub acceptor: tokio_rustls::TlsAcceptor, pub token: String }` (Clone).
  - `pub fn build_remote_tls(creds: &RemoteCreds) -> Result<RemoteTls>` — parses the PEM cert+key, builds a rustls `ServerConfig` (ring provider, single cert, no client auth), returns the acceptor + token.
  - `serve_remote(self: Arc<Self>, listener: TcpListener, tls: Option<RemoteTls>)` — plaintext when `None`; TLS-wrapped + token-checked when `Some`; the accept loop survives transient errors.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `remote.rs` (loopback TLS round-trip for the Control channel):

```rust
#[tokio::test]
async fn tls_control_channel_round_trips_with_token() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let _g = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_STATE_HOME", dir.path());

    let creds = crate::remote_tls::load_or_generate().unwrap();
    let token = creds.token.clone();
    let fp = crate::remote_tls::fingerprint(&creds);
    let tls = build_remote_tls(&creds).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let daemon = test_daemon();
    tokio::spawn(daemon.serve_remote(listener, Some(tls)));

    // Client side: connect with a verifier pinned to the known fingerprint + the token.
    let connector = crate::remote::test_support::connector_pinned(fp);
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let name = tokio_rustls::rustls::pki_types::ServerName::try_from("clowder").unwrap();
    let mut stream = connector.connect(name, tcp).await.unwrap();
    clowder_proto::write_hello(&mut stream, clowder_proto::Channel::Control, Some(&token)).await.unwrap();
    // A control client sends a JSON line request; assert we get a JSON line back (handler engaged).
    stream.write_all(b"{\"type\":\"listAgents\"}\n").await.unwrap();
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf)).await.unwrap().unwrap();
    assert!(n >= 1, "control handler responded over TLS");

    std::env::remove_var("XDG_STATE_HOME");
}

#[tokio::test]
async fn tls_wrong_token_is_rejected() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let _g = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_STATE_HOME", dir.path());
    let creds = crate::remote_tls::load_or_generate().unwrap();
    let fp = crate::remote_tls::fingerprint(&creds);
    let tls = build_remote_tls(&creds).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(test_daemon().serve_remote(listener, Some(tls)));

    let connector = crate::remote::test_support::connector_pinned(fp);
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let name = tokio_rustls::rustls::pki_types::ServerName::try_from("clowder").unwrap();
    let mut stream = connector.connect(name, tcp).await.unwrap();
    clowder_proto::write_hello(&mut stream, clowder_proto::Channel::Control, Some("wrong")).await.unwrap();
    stream.write_all(b"{\"type\":\"listAgents\"}\n").await.unwrap();
    // The daemon drops the connection before dispatch → read returns 0 (EOF).
    let mut buf = [0u8; 8];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf)).await.unwrap().unwrap_or(0);
    assert_eq!(n, 0, "wrong token must be rejected with no handler response");
    std::env::remove_var("XDG_STATE_HOME");
}
```

> The test needs a client-side connector pinned to the daemon's fingerprint. Provide it as a small `#[cfg(test)] pub(crate) mod test_support` in `remote.rs` exposing `connector_pinned(fp: String) -> tokio_rustls::TlsConnector` built with a verifier that accepts only that fingerprint (the same verifier shape Task 4 ships for the real client; here it's a fixed-fingerprint pin, no file). Include it in Step 3.

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon tls_ 2>&1 | tail -20`
Expected: compile error (`serve_remote` arity, `build_remote_tls`, `test_support` missing).

- [ ] **Step 3: Implement**

`crates/clowder-daemon/Cargo.toml` — add:

```toml
rustls = { version = "0.23", default-features = false, features = ["ring", "std", "tls12"] }
tokio-rustls = { version = "0.26", default-features = false, features = ["ring", "tls12"] }
rustls-pemfile = "2"
```

In `crates/clowder-daemon/src/remote.rs`, add imports and the TLS builder + struct:

```rust
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

#[derive(Clone)]
pub struct RemoteTls {
    pub acceptor: TlsAcceptor,
    pub token: String,
}

/// Build a rustls ServerConfig (ring provider, single self-signed cert, no client auth) from creds.
pub fn build_remote_tls(creds: &crate::remote_tls::RemoteCreds) -> anyhow::Result<RemoteTls> {
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut creds.cert_pem.as_bytes()).collect::<Result<_, _>>()?;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut creds.key_pem.as_bytes())?
        .ok_or_else(|| anyhow::anyhow!("no private key in PEM"))?;
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(RemoteTls { acceptor: TlsAcceptor::from(Arc::new(config)), token: creds.token.clone() })
}
```

Replace `serve_remote` and add the token param to `handle_remote_conn`:

```rust
pub async fn serve_remote(self: Arc<Self>, listener: TcpListener, tls: Option<RemoteTls>) -> Result<()> {
    loop {
        let (tcp, _addr) = match listener.accept().await {
            Ok(v) => v,
            // Survive a transient accept() error instead of terminating the listener.
            Err(e) => {
                tracing::warn!("remote accept error: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let me = self.clone();
        match tls.clone() {
            Some(rt) => {
                tokio::spawn(async move {
                    match rt.acceptor.accept(tcp).await {
                        Ok(stream) => {
                            if let Some(line) = crate::logging::conn_error_line(
                                "remote",
                                me.handle_remote_conn(stream, Some(rt.token.as_str())).await,
                            ) {
                                tracing::warn!("{line}");
                            }
                        }
                        Err(e) => tracing::warn!("remote TLS handshake failed: {e}"),
                    }
                });
            }
            None => {
                tokio::spawn(async move {
                    if let Some(line) = crate::logging::conn_error_line(
                        "remote",
                        me.handle_remote_conn(tcp, None).await,
                    ) {
                        tracing::warn!("{line}");
                    }
                });
            }
        }
    }
}

async fn handle_remote_conn<S>(self: Arc<Self>, mut stream: S, expected_token: Option<&str>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let (channel, token) = tokio::time::timeout(HELLO_TIMEOUT, clowder_proto::read_hello(&mut stream))
        .await
        .map_err(|_| anyhow!("timed out waiting for channel hello"))??;
    if let Some(expected) = expected_token {
        let ok = token.as_deref()
            .map(|t| clowder_proto::constant_time_eq(t.as_bytes(), expected.as_bytes()))
            .unwrap_or(false);
        if !ok {
            bail!("remote auth failed (bad or missing token)");
        }
    }
    match channel {
        Channel::Control => self.handle_control_json(stream).await,
        Channel::Render => self.handle_conn(stream).await,
    }
}
```

> Note: `read_hello` now returns `(Channel, Option<String>)` and `write_hello` takes a token — this fixes the arity break from Task 1 on the daemon side. Remove the old `use clowder_proto::...` for `read_hello` if it changed; ensure `Channel` is imported.

Add the test-support connector (used by this task's tests and mirrored by Task 4's real client). Append to `remote.rs`:

```rust
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;
    use tokio_rustls::TlsConnector;
    use tokio_rustls::rustls::ClientConfig;
    use tokio_rustls::rustls::client::danger::{ServerCertVerified, ServerCertVerifier, HandshakeSignatureValid};
    use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use tokio_rustls::rustls::{DigitallySignedStruct, SignatureScheme, Error};

    #[derive(Debug)]
    struct PinnedFp(String);
    impl ServerCertVerifier for PinnedFp {
        fn verify_server_cert(&self, end_entity: &CertificateDer, _i: &[CertificateDer], _n: &ServerName, _o: &[u8], _t: UnixTime) -> Result<ServerCertVerified, Error> {
            if clowder_proto::cert_fingerprint_hex(end_entity) == self.0 {
                Ok(ServerCertVerified::assertion())
            } else {
                Err(Error::General("fingerprint mismatch".into()))
            }
        }
        fn verify_tls12_signature(&self, _m: &[u8], _c: &CertificateDer, _d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> { Ok(HandshakeSignatureValid::assertion()) }
        fn verify_tls13_signature(&self, _m: &[u8], _c: &CertificateDer, _d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> { Ok(HandshakeSignatureValid::assertion()) }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![SignatureScheme::ECDSA_NISTP256_SHA256, SignatureScheme::ED25519, SignatureScheme::RSA_PSS_SHA256, SignatureScheme::RSA_PKCS1_SHA256]
        }
    }

    pub(crate) fn connector_pinned(fp: String) -> TlsConnector {
        let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions().unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedFp(fp)))
            .with_no_client_auth();
        TlsConnector::from(Arc::new(config))
    }
}
```

In `crates/clowder-daemon/src/main.rs`, where the remote listener is set up (the `if let Some(addr_str) = remote_listen { … remote.serve_remote(tcp) … }` block), build TLS when enabled and pass it. Fail closed on cred errors:

```rust
let tls = if config_remote_tls {   // the resolved Config.remote_tls
    let creds = clowder_daemon::remote_tls::load_or_generate()
        .map_err(|e| anyhow::anyhow!("[remote] tls enabled but credential setup failed: {e}"))?;
    tracing::info!(
        "remote TLS enabled — token: {}  cert fingerprint (sha256): {}",
        creds.token, clowder_daemon::remote_tls::fingerprint(&creds)
    );
    Some(clowder_daemon::remote::build_remote_tls(&creds)?)
} else {
    None
};
// … existing bind …
let remote = daemon.clone();
tokio::spawn(async move {
    if let Some(line) = clowder_daemon::logging::conn_error_line("remote server", remote.serve_remote(tcp, tls).await) {
        tracing::error!("{line}");
    }
});
```

> Thread the resolved `remote_tls` bool from the `Config` into `config_remote_tls` at the top of `main` where the other config fields are read. Keep `should_warn_exposed` as-is.

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon tls_ 2>&1 | tail -30`
Then build main: `source "$HOME/.cargo/env" && cargo build -p clowder-daemon 2>&1 | tail -5`
Expected: both TLS tests PASS; clean build. (Re-run once if a known daemon timing flake trips.)

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/Cargo.toml crates/clowder-daemon/src/remote.rs crates/clowder-daemon/src/main.rs Cargo.lock
git commit -m "feat(m7d): daemon TLS accept + constant-time token verify; harden accept loop"
```

---

### Task 4: Client TLS dial + TOFU verifier + `remote-token` CLI

**Files:**
- Create: `crates/clowder-client/src/tofu.rs` (+ `mod tofu;` in `crates/clowder-client/src/lib.rs`)
- Modify: `crates/clowder-client/src/forward.rs` (`dial` returns a boxed stream; TLS when a token is set)
- Modify: `crates/clowder-client/src/main.rs` (`remote-token` subcommand)
- Modify: `crates/clowder-client/Cargo.toml` (add `tokio-rustls`, `rustls`, `clowder-proto` if not present)

**Interfaces:**
- Consumes: `clowder_proto::{write_hello, cert_fingerprint_hex, Channel}`, `clowder_config::{remote_token, remote_cert_path, remote_token_path}`.
- Produces:
  - `tofu::TofuVerifier { host, known_hosts_path }` implementing `ServerCertVerifier` (record-or-verify against a `known_hosts` file); `tofu::known_hosts_path() -> PathBuf` (client state dir `clowder/remote_known_hosts`); `tofu::check(path, host, fp) -> Result<(), String>` (the pure record/verify/refuse logic).
  - `forward.rs` uses TLS when `remote_token` is set: dial TCP → TLS connect (TOFU) → `write_hello(channel, Some(token))` → `copy_bidirectional`.

- [ ] **Step 1: Write the failing tests**

In `crates/clowder-client/src/tofu.rs` (new), add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tofu_records_then_verifies_then_refuses_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        // first sight: record + accept
        assert!(check(&kh, "host:7777", "aa11").is_ok());
        // same fingerprint next time: accept
        assert!(check(&kh, "host:7777", "aa11").is_ok());
        // different fingerprint for the same host: refuse
        let err = check(&kh, "host:7777", "bb22").unwrap_err();
        assert!(err.to_lowercase().contains("changed"), "loud refuse: {err}");
        // a different host records independently
        assert!(check(&kh, "other:7777", "cc33").is_ok());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client tofu:: 2>&1 | tail -20`
Expected: compile error (`tofu` missing).

- [ ] **Step 3: Implement**

`crates/clowder-client/Cargo.toml` — add (match daemon versions/features exactly):

```toml
rustls = { version = "0.23", default-features = false, features = ["ring", "std", "tls12"] }
tokio-rustls = { version = "0.26", default-features = false, features = ["ring", "tls12"] }
```
(`clowder-proto` and `clowder-config` are already deps; confirm and add if missing.)

Create `crates/clowder-client/src/tofu.rs`:

```rust
//! Trust-on-first-use verification of the remote daemon's self-signed cert (SSH host-key style).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{DigitallySignedStruct, Error, SignatureScheme};

/// `<client state dir>/clowder/remote_known_hosts` (lines: `<host> <sha256-hex>`).
pub fn known_hosts_path() -> PathBuf {
    clowder_config::remote_state_dir().join("remote_known_hosts")
}

/// Record-or-verify `fp` for `host`. Ok = trusted (recorded on first sight); Err(msg) = refuse.
pub fn check(path: &Path, host: &str, fp: &str) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    for line in existing.lines() {
        let mut it = line.split_whitespace();
        if let (Some(h), Some(f)) = (it.next(), it.next()) {
            if h == host {
                return if f == fp {
                    Ok(())
                } else {
                    Err(format!(
                        "REMOTE DAEMON IDENTIFICATION HAS CHANGED for {host}: known {f}, got {fp}. \
                         If you rotated the daemon cert, remove the line from {}; otherwise this may be a MITM.",
                        path.display()
                    ))
                };
            }
        }
    }
    // First sight: record and accept.
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') { content.push('\n'); }
    content.push_str(&format!("{host} {fp}\n"));
    std::fs::write(path, content).map_err(|e| format!("write known_hosts: {e}"))?;
    Ok(())
}

#[derive(Debug)]
pub struct TofuVerifier {
    pub host: String,
    pub known_hosts_path: PathBuf,
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(&self, end_entity: &CertificateDer, _i: &[CertificateDer], _n: &ServerName, _o: &[u8], _t: UnixTime) -> Result<ServerCertVerified, Error> {
        let fp = clowder_proto::cert_fingerprint_hex(end_entity);
        check(&self.known_hosts_path, &self.host, &fp)
            .map(|_| ServerCertVerified::assertion())
            .map_err(|msg| Error::General(msg))
    }
    fn verify_tls12_signature(&self, _m: &[u8], _c: &CertificateDer, _d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> { Ok(HandshakeSignatureValid::assertion()) }
    fn verify_tls13_signature(&self, _m: &[u8], _c: &CertificateDer, _d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> { Ok(HandshakeSignatureValid::assertion()) }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ECDSA_NISTP256_SHA256, SignatureScheme::ED25519, SignatureScheme::RSA_PSS_SHA256, SignatureScheme::RSA_PKCS1_SHA256]
    }
}

/// Build a TLS connector that verifies `host` via TOFU.
pub fn connector(host: &str) -> Arc<tokio_rustls::rustls::ClientConfig> {
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let verifier = TofuVerifier { host: host.to_string(), known_hosts_path: known_hosts_path() };
    Arc::new(
        tokio_rustls::rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions().unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth(),
    )
}
```

Add `mod tofu;` to `crates/clowder-client/src/lib.rs`.

In `crates/clowder-client/src/forward.rs`, make `forward_stream` use TLS when a token is configured. Since TLS vs plaintext yields different stream types, box the remote side:

```rust
use clowder_proto::{write_hello, Channel};
use clowder_config::remote_token;   // resolved Option<String>
// ...

pub async fn forward_stream<L>(mut local: L, host: &str, channel: Channel) -> Result<()>
where
    L: AsyncRead + AsyncWrite + Unpin + Send,
{
    let tcp = dial_with_backoff(host).await?;
    let token = remote_token();                 // helper returning the resolved Option<String>
    let mut remote: Box<dyn RemoteStream> = match &token {
        Some(_) => {
            let connector = tokio_rustls::TlsConnector::from(crate::tofu::connector(host));
            let name = tokio_rustls::rustls::pki_types::ServerName::try_from("clowder")
                .map_err(|e| anyhow::anyhow!("server name: {e}"))?;
            Box::new(connector.connect(name, tcp).await?)
        }
        None => Box::new(tcp),
    };
    write_hello(&mut remote, channel, token.as_deref()).await?;
    tokio::io::copy_bidirectional(&mut local, &mut remote).await?;
    Ok(())
}

/// Object-safe alias so the TLS and plaintext streams can share one path.
trait RemoteStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> RemoteStream for T {}
```

> `remote_token()` — add a tiny resolver in `clowder-config` (or reuse an existing `Config` load) that returns the resolved `remote_token`. If the forwarder already loads a `Config`, thread `config.remote_token` into `forward`/`forward_stream` instead of a global getter (cleaner — pass `token: Option<String>` down from `forward(host, dir)`'s caller in `main.rs`, which already resolves config for `remote-host`). Prefer threading it as a parameter over a global.

In `crates/clowder-client/src/main.rs`, add the `remote-token` subcommand (prints the daemon's token + fingerprint by reading the state-dir files — same host):

```rust
Some("remote-token") => {
    let tok_p = clowder_config::remote_token_path();
    let cert_p = clowder_config::remote_cert_path();
    let token = std::fs::read_to_string(&tok_p)
        .map_err(|e| anyhow!("no remote token at {} ({e}); start the daemon with [remote] tls=true first", tok_p.display()))?;
    let cert_pem = std::fs::read_to_string(&cert_p)?;
    let der = {
        let mut rd = std::io::BufReader::new(cert_pem.as_bytes());
        rustls_pemfile::certs(&mut rd).next().ok_or_else(|| anyhow!("no cert"))??.to_vec()
    };
    println!("token:       {}", token.trim());
    println!("fingerprint: {}", clowder_proto::cert_fingerprint_hex(&der));
    Ok(())
}
```

> This needs `rustls-pemfile = "2"` in `clowder-client/Cargo.toml` (add it). Update the usage string in the `_ =>` arm to include `remote-token`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client 2>&1 | tail -20`
Then build: `source "$HOME/.cargo/env" && cargo build -p clowder-client 2>&1 | tail -5`
Expected: `tofu::` tests pass; clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-client Cargo.lock
git commit -m "feat(m7d): client TLS dial + TOFU known_hosts verifier; clowder remote-token"
```

---

### Task 5: End-to-end integration — full-stack TLS + TOFU-change refuse

**Files:**
- Test: `crates/clowder-daemon/src/remote.rs` (`#[cfg(test)]`) — a full round-trip using the REAL client TOFU verifier (not the fixed-pin test helper), driving both a happy path (records fingerprint) and a fingerprint-change refuse. No production changes.

**Interfaces:**
- Consumes: `serve_remote`/`build_remote_tls`/`load_or_generate` (daemon), `clowder_client::tofu` (client).

> **Cross-crate note:** this test lives in `clowder-daemon` but drives `clowder_client::tofu::connector`. Add `clowder-client` as a `[dev-dependencies]` of `clowder-daemon` (dev-only, no production coupling). If a dependency cycle results (clowder-client dev-deps on clowder-daemon?), instead place this test in a new `crates/clowder-daemon/tests/remote_tls_e2e.rs` integration test that dev-depends on both crates, or keep it in `clowder-client` dev-depending on `clowder-daemon`. Pick the direction with no cycle; the plan's intent is one end-to-end test exercising the real daemon accept + real client TOFU verifier.

- [ ] **Step 1: Write the test**

```rust
#[tokio::test]
async fn e2e_tls_tofu_records_then_refuses_on_cert_change() {
    use tokio::io::AsyncWriteExt;
    let _g = crate::STATE_FILE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let state = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_STATE_HOME", state.path());     // daemon creds + client known_hosts both land here

    // Daemon #1
    let creds = crate::remote_tls::load_or_generate().unwrap();
    let token = creds.token.clone();
    let tls = build_remote_tls(&creds).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(test_daemon().serve_remote(listener, Some(tls)));
    let host = addr.to_string();

    // Client connects with the REAL TOFU connector → first sight records the fingerprint + succeeds.
    let connector = tokio_rustls::TlsConnector::from(clowder_client::tofu::connector(&host));
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let name = tokio_rustls::rustls::pki_types::ServerName::try_from("clowder").unwrap();
    let mut s = connector.connect(name, tcp).await.expect("first connect (TOFU record) ok");
    clowder_proto::write_hello(&mut s, clowder_proto::Channel::Control, Some(&token)).await.unwrap();
    s.write_all(b"{\"type\":\"listAgents\"}\n").await.unwrap();

    // Rotate the daemon cert (delete + regenerate) → a new fingerprint for the same host.
    std::fs::remove_file(clowder_config::remote_cert_path()).unwrap();
    std::fs::remove_file(clowder_config::remote_key_path()).unwrap();
    let creds2 = crate::remote_tls::load_or_generate().unwrap();
    let tls2 = build_remote_tls(&creds2).unwrap();
    let listener2 = TcpListener::bind(addr).await.unwrap();   // reuse the same host:port string
    tokio::spawn(test_daemon().serve_remote(listener2, Some(tls2)));

    // Client re-connects to the SAME host → TOFU sees a changed fingerprint → handshake refused.
    let connector2 = tokio_rustls::TlsConnector::from(clowder_client::tofu::connector(&host));
    let tcp2 = tokio::net::TcpStream::connect(addr).await.unwrap();
    let name2 = tokio_rustls::rustls::pki_types::ServerName::try_from("clowder").unwrap();
    let err = connector2.connect(name2, tcp2).await.err().expect("changed cert must be refused");
    assert!(format!("{err}").to_lowercase().contains("changed") || format!("{err}").to_lowercase().contains("mismatch")
        || format!("{err}").to_lowercase().contains("general"), "refuse surfaced: {err}");

    std::env::remove_var("XDG_STATE_HOME");
}
```

> Rebinding the same `addr` may need `SO_REUSEADDR`; if `TcpListener::bind(addr)` fails in the test, bind a fresh `127.0.0.1:0` for daemon #2 and use a **fixed host key** in the client (the TOFU file keys on the host string, so pass the same host label both times by constructing the connector with a constant host name rather than the addr). Simplest robust variant: key TOFU on a constant label `"e2e-host"` for both connectors and dial the two different addrs — the fingerprint-change refuse still triggers because the label is constant while the cert changed. Use whichever compiles cleanly; the assertion is: **connect #2 to the changed cert under the same TOFU host label is refused.**

- [ ] **Step 2: Run the test**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon e2e_tls 2>&1 | tail -30`
Expected: PASS (first connect records + round-trips; second refuses on the changed fingerprint).

- [ ] **Step 3: Full-suite + clippy**

Run: `source "$HOME/.cargo/env" && cargo test --workspace --locked 2>&1 | tail -30` (re-run once if a known daemon timing flake trips: `attached_client_gets_attention_changed`, the exit-under-load test, or `reconcile_restored_companion_ids_never_collide_with_agents`).
Run: `source "$HOME/.cargo/env" && cargo clippy -p clowder-proto -p clowder-config -p clowder-daemon -p clowder-client --all-targets 2>&1 | grep -E "warning:|error" | grep -iE "remote|tofu|auth|hello" | head`
Expected: green suite; no new warnings in the M7d files.

- [ ] **Step 4: Commit**

```bash
git add crates/clowder-daemon Cargo.lock
git commit -m "test(m7d): end-to-end TLS round-trip + TOFU fingerprint-change refuse"
```

---

### Task 6: Docs — setup + threat model

**Files:**
- Create: `docs/remote-tls.md` (setup + threat model)
- Modify: `AGENTS.md` (one line in the runtime/remote section pointing at the new doc + the `[remote] tls`/`token` keys)

**Interfaces:** none (docs only).

- [ ] **Step 1: Write `docs/remote-tls.md`**

Cover, concisely and accurately (match the shipped behavior):
- **What it is:** opt-in TLS + bearer-token for the remote listener; plaintext Phase A (tunnel) still the default.
- **Daemon setup:** set `[remote] listen = "0.0.0.0:7777"` and `[remote] tls = true` (or `CLOWDER_LISTEN` / `CLOWDER_REMOTE_TLS=1`). On first start the daemon generates `remote-cert.pem` / `remote-key.pem` / `remote-token` under `$XDG_STATE_HOME/clowder` (`0600`) and logs the **token** + **SHA-256 fingerprint**. Re-print anytime with `clowder remote-token`. Rotate the token by deleting `remote-token`; rotate the cert by deleting the pem pair (clients then must re-trust).
- **Client setup:** set `[remote] host = "<addr:port>"` and `[remote] token = "<token>"` (or `CLOWDER_REMOTE_HOST` / `CLOWDER_REMOTE_TOKEN`), then `clowder connect`. On first connect the client records the daemon's fingerprint in `<state>/clowder/remote_known_hosts` (TOFU); a later fingerprint change is **refused** loudly — remove the line to re-trust after a legitimate cert rotation.
- **Threat model:** TLS gives encryption + (via the pinned-after-first-use fingerprint) server identity; the token authenticates the client. Residual risks: the **first-connect TOFU window** (mitigated — a MITM lacks the token and can't impersonate the daemon after first trust; verify the printed fingerprint out-of-band for high assurance) and **token leakage** (files are `0600`; rotate as above). Not yet: mTLS, QUIC, a pinned-pairing UX, or Keychain storage.
- **Note:** `allow_plaintext` is NOT a thing — plaintext is simply `tls` unset; the startup exposure warning (`should_warn_exposed`) still fires when binding a non-loopback/non-tailnet address without TLS.

- [ ] **Step 2: Update `AGENTS.md`**

Add one line under the runtime/remote description: the `[remote] tls`/`token` keys and a pointer to `docs/remote-tls.md`.

- [ ] **Step 3: Commit**

```bash
git add docs/remote-tls.md AGENTS.md
git commit -m "docs(m7d): remote TLS + token setup and threat model"
```

---

## Notes for the implementer

- **Crypto provider:** always `ring` (never aws-lc-rs) — set the crate features as shown and build configs with `builder_with_provider(Arc::new(rustls::crypto::ring::default_provider())).with_safe_default_protocol_versions()?`. This avoids a global provider install and C build deps. If two configs are built in one process, constructing a provider per config is fine.
- **rustls 0.23 danger API:** a custom `ServerCertVerifier` must implement `verify_server_cert` + both `verify_tls{12,13}_signature` + `supported_verify_schemes`. Returning the `assertion()` values bypasses chain/signature checks — intended here (TOFU trusts by fingerprint). Do NOT weaken the daemon side (it uses a normal single-cert `ServerConfig`, no custom verifier).
- **Version drift:** the exact builder method names for `rcgen 0.13` / `rustls 0.23` / `tokio-rustls 0.26` may need small adjustments against the resolved patch. Keep the shape; the round-trip tests are the gate. Commit the regenerated `Cargo.lock`.
- **Tests mutating `XDG_STATE_HOME`** must hold `crate::STATE_FILE_ENV_LOCK` (M9b) — the state dir is process-global env-derived, shared with the M9a/M9b registry tests.
- **Arity break:** Task 1 changes `write_hello`/`read_hello` signatures; the workspace won't fully build until Tasks 3 (daemon `handle_remote_conn`) and 4 (client `forward_stream`) update their call sites. Build per-crate within Tasks 1–2; the full `--workspace` build first succeeds at the end of Task 4.
- **No app/handler/local change:** `handle_control_json`, `handle_conn`, the Unix accept loops, and everything in `macos/` stay untouched.
