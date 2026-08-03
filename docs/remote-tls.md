# Remote TLS + token auth

Phase A of clowder's remote daemon support (see
`docs/superpowers/specs/2026-07-31-clowder-m7-remote-daemon-design.md`) lets a daemon accept connections
over TCP instead of only local Unix sockets, so a client can attach to agents running on another
machine. By default that TCP listener is **plaintext** — the documented, supported way to use it safely
is behind a trusted tunnel (SSH `-L` or a Tailscale tailnet).

This doc covers the **opt-in** hardening on top of that: TLS encryption plus a bearer token, so the
listener can be reasoned about even when bound somewhere less trusted. It does not change the plaintext
default — `[remote] tls` must be turned on explicitly.

## Daemon setup

Enable the remote listener and TLS in `config.toml` (`$XDG_CONFIG_HOME/clowder/config.toml`, else
`~/.config/clowder/config.toml`):

```toml
[remote]
listen = "0.0.0.0:7777"
tls = true
```

or via env: `CLOWDER_LISTEN=0.0.0.0:7777 CLOWDER_REMOTE_TLS=1`.

`listen` alone (with `tls` unset/false) serves the remote listener in **plaintext** — there is no
`allow_plaintext` key; plaintext is simply the absence of `tls = true`. Binding to anything other than
loopback or a Tailscale tailnet address (CGNAT `100.64.0.0/10`, or IPv6 `fd7a:115c:a1e0::/48`) without
TLS logs a startup warning (`should_warn_exposed`, in `crates/clowder-daemon/src/remote.rs`) because
Phase A plaintext has no authentication at all.

With `tls = true`, on first start the daemon generates and persists, under
`$XDG_STATE_HOME/clowder` (else `~/.local/state/clowder`), mode `0600`:

- `remote-cert.pem` / `remote-key.pem` — a self-signed TLS keypair
- `remote-token` — a random bearer token

and logs both the token and the certificate's SHA-256 fingerprint once at startup. Re-print them anytime
without restarting the daemon:

```sh
clowder remote-token
# token:       <token>
# fingerprint: <sha256 hex>
```

**Rotation:**
- Rotate the token: delete `remote-token`; the daemon generates a new one on next start.
- Rotate the cert: delete both `remote-cert.pem` and `remote-key.pem`; the daemon generates a new
  keypair on next start. Every client that already trusted the old cert must re-trust the new one (see
  TOFU below) — a stale entry in a client's known-hosts file will refuse the connection until removed.

Note that generating a new cert or token does not, by itself, delete the other — deleting all three
files together mid-lifecycle is the clean way to fully reset remote credentials.

## Client setup

Point the client at the daemon's host and the token from `remote-token`:

```toml
[remote]
host = "<addr:port>"
token = "<token>"
```

or via env: `CLOWDER_REMOTE_HOST=<addr:port> CLOWDER_REMOTE_TOKEN=<token>`. Then:

```sh
clowder connect <host:port>   # host:port optional if [remote] host / CLOWDER_REMOTE_HOST is set
```

`clowder connect` runs a local forwarder that dials the remote daemon (TLS-wrapped and
token-authenticated when the daemon has `tls = true`); once it's up, use ordinary `clowder attach
<pane-id>` against the forwarded local socket as usual.

**Trust-on-first-use (TOFU):** on first connect to a given host, the client records the daemon's
certificate fingerprint in `<state>/clowder/remote_known_hosts` (`$XDG_STATE_HOME` or
`~/.local/state`), keyed by host. On every later connect it recomputes the fingerprint and compares —
a match proceeds silently; a **mismatch is refused loudly** (the connection is not made) since it could
mean either a legitimate cert rotation on the daemon side or a man-in-the-middle. To reconnect after a
legitimate rotation, remove the corresponding line from `remote_known_hosts` so the client re-records
the new fingerprint on the next connect.

## Threat model

- **Encryption:** TLS 1.2/1.3 (via `rustls`, `ring` crypto provider) encrypts the connection, so a
  passive network observer can't read pane content or the bearer token in transit.
- **Server identity:** pinned-after-first-use via the certificate fingerprint (SSH host-key style, not a
  CA chain — the cert is self-signed). This protects against impersonation on connections *after* the
  first — see the residual risk below for the first connection itself.
- **Client authentication:** the bearer token, sent once as part of the initial channel hello and
  compared with a constant-time equality check, proves the client is one that has read
  `remote-token` (i.e., has access to the daemon's state dir or was told the token out of band).

**Residual risks:**
- **First-connect TOFU window.** The very first connection to a host has no prior fingerprint to check
  against, so a MITM present at that moment could present its own cert. This is mitigated in two ways:
  the MITM still doesn't have the daemon's bearer token, so it can't complete authenticated requests as
  the real daemon even if the client accepts its cert; and for higher assurance, compare the fingerprint
  printed by `clowder remote-token` (ideally read from the daemon's own console/logs, not relayed by the
  network path being trusted) against what the client records before relying on the connection.
- **Token leakage.** The token is a bearer credential — anyone who reads it can authenticate as the
  client. Credential files are written `0600` (owner-only); treat leakage the same as any other secret
  and rotate (delete `remote-token`, restart the daemon) if you suspect exposure.

**Deferred (not in this phase):** mutual TLS (client certs), QUIC transport, a pinned-pairing UX (e.g.
QR/short-code exchange instead of manual fingerprint comparison), and OS Keychain storage for the token
(it currently lives in a plain `0600` file).
