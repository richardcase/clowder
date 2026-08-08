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
loopback or a Tailscale tailnet address (CGNAT `100.64.0.0/10`, or IPv6 `fd7a:115c:a1e0::/48`) logs a
startup line (`should_warn_exposed`, in `crates/clowder-daemon/src/remote.rs`) — this check runs
**unconditionally**, before TLS is even considered, so it fires the same way whether `tls` is on or off.
The daemon picks the level to match: with `tls = false` it's a `warn!` (no authentication at all is
genuinely worth flagging); with `tls = true` it's an `info!` (token auth is active, so exposure alone
isn't an error condition, just worth a note in the log).

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

**Rotation:** there is no partial (token-only or cert-only) rotation — `load_or_generate()` only takes
the "load existing" path when **all three** files (`remote-cert.pem`, `remote-key.pem`, `remote-token`)
are present; if *any one* is missing, it regenerates and overwrites **all three** together (fresh cert
**and** fresh token). So to rotate, delete the credential files (simplest: delete `remote-token` and the
pem pair together, or the whole state-dir remote-cred set) and restart the daemon — it generates a fresh
cert and token on next start. Because the cert always changes too, every client's TOFU pin breaks: they
must remove the stale line from their `remote_known_hosts` (see below) before they can reconnect.

## Client setup

Point the client at the daemon's host and the token from `remote-token`:

```toml
[remote]
host = "<addr:port>"
token = "<token>"
```

or via env: `CLOWDER_REMOTE_HOST=<addr:port> CLOWDER_REMOTE_TOKEN=<token>`. Or skip config entirely
and give the daemon a nickname in the host registry (see "Managing hosts" below). Then:

```sh
clowder connect <name-or-host:port>   # selector optional if [remote] host / CLOWDER_REMOTE_HOST is set
```

`clowder connect` runs a local forwarder that dials the remote daemon (TLS-wrapped and
token-authenticated when the target has `tls = true`); once it's up, use ordinary `clowder attach
<pane-id>` against the forwarded local socket as usual. It resolves its argument against the host
registry first (an exact name, then an exact address), and falls back to treating an unrecognized
`host:port`-shaped argument as an ad-hoc TOFU target — the same thing `clowder connect <host:port>`
has always done, so nothing in this paragraph is a compatibility break.

The forwarder's local socket directory is **flat by default** — `<the control socket's parent>/remote`,
the same path `clowder connect` has always used — **not** one subdirectory per host. That's
deliberate: the macOS app spawns `clowder connect <host>` without any socket-dir flag and expects
that exact flat path, so changing the default would have broken its remote mode. Pass `--socket-dir
<dir>` if you want per-host isolation (e.g. two `clowder connect` processes for two hosts at once);
the app opts into this itself when it needs it, everyone else keeps the flat default.

`clowder connect` exits **4** when the very first dial to the target never lands (bad address, daemon
down, wrong port) — distinct from exit 3 (`clowder-daemon`'s "another instance already owns the
lock") and the plain exit 1 used for a user error such as an unrecognized host name. The app's
supervisor (M11b) relies on 4 to tell "typo/unreachable" apart from a daemon that's merely slow to
come up.

**Interim note (until M11b lands):** exit 4 is already app-visible, and the app does not know about
it yet. `macos/Sources/ClowderApp/DaemonLaunch.swift` supervises `clowder connect <host>` and
relaunches it on every exit code except 3 (bounded backoff, `min(10s, 0.5s × 2^attempt)`), so an
unreachable remote host now makes the app respawn the forwarder every ~10 seconds once that backoff
saturates. Before exit 4 existed, the forwarder burned ~15s of internal
`dial_with_backoff` retries before exiting, so the loop is the same shape, just faster. Nothing is
corrupted by it — it's a busier log and a warmer laptop until the supervisor learns to stop on 4.

**Trust-on-first-use (TOFU):** on first connect to a given host, the client records the daemon's
certificate fingerprint in `<state>/clowder/remote_known_hosts` (`$XDG_STATE_HOME` or
`~/.local/state`), keyed by address. On every later connect it recomputes the fingerprint and compares —
a match proceeds silently; a **mismatch is refused loudly** (the connection is not made) since it could
mean either a legitimate cert rotation on the daemon side or a man-in-the-middle. To reconnect after a
legitimate rotation, remove the corresponding line from `remote_known_hosts` so the client re-records
the new fingerprint on the next connect.

**The registry pin is authoritative over `remote_known_hosts`.** A host managed through `clowder
remote …` (below) carries its own `fingerprint` field once paired, and that field — not
`remote_known_hosts` — is what `clowder connect <name>` checks. `remote_known_hosts` is only
*consulted* for a target with no pin (a bare `clowder connect <address>` with no matching registry
entry, or a registry entry that has never been trusted). `clowder remote trust` *writes* both: the
registry pin and the matching `remote_known_hosts` line, so that a plain-shell `clowder connect
<address>` — which has no registry entry to look up and so falls onto the TOFU path — ends up trusting
the exact same fingerprint the registry already pinned. If you hand-edit `remote_known_hosts`, only
the unpinned hosts will notice.

## Managing hosts

`clowder remote add|list|show|set|rm` manage a nicknamed registry of remote daemons
(`$XDG_STATE_HOME/clowder/hosts.json`, `0600`, no daemon required — it's a pure client-side file).
This is the backing store for the macOS app's Settings pane in M11b; the CLI is the same code path.

```sh
clowder remote add studio studio.tailnet:7777           # plaintext entry
clowder remote add pi 10.0.0.9:7777 --token-stdin <<<"$TOKEN"   # reads the token from stdin, not argv
clowder remote list                                       # name, address, tls/plain, paired/unpaired, source
clowder remote list --json                                 # machine-readable — see docs/protocol/fixtures/remote-host-list.json
clowder remote show studio [--json]
clowder remote set studio --rename studio-mini --tls
clowder remote rm studio
```

A few things worth knowing:

- **A token requires TLS.** `--token`/`--token-stdin` implies `--tls` by default on `add`, precisely
  because a bearer token must never cross the network in cleartext. Asking for the combination
  anyway is refused **by the command that would create it** — `clowder remote add … --no-tls
  --token …` and `clowder remote set … --no-tls` on an entry that still holds a token both fail,
  naming the two ways out (drop `--no-tls`, or clear the token with `--no-token`). `resolve_target`
  refuses the same combination again at connect time; that second check stays because the registry
  is hand-editable and a file edited by hand never went through `add`/`set`.
- **A fingerprint must be lowercase hex.** `clowder remote trust --fingerprint` lowercases what you
  pass and then validates it (even number of `[0-9a-f]` digits). Anything else — notably a value
  with whitespace in it — is rejected rather than stored: the matching `remote_known_hosts` line is
  `"<address> <fingerprint>"` and is read back by whitespace-splitting, so a fingerprint containing
  a space would be truncated on read and the pin and the known-hosts line would disagree forever.
- **A corrupt `hosts.json` is never overwritten.** `add`/`set`/`rm`/`trust`/`untrust` refuse to run
  against a registry file that exists but does not parse, with an error naming the file — the
  tokens and pins stay on disk for you to rescue. (Reading is more forgiving: a corrupt registry
  degrades to an empty host list rather than stopping the app reaching its *local* daemon.) A
  zero-length file is treated as an empty registry, since it has nothing to preserve.
- **Boolean flags don't take inline values.** Use `--tls` / `--no-tls`, never `--tls=<value>` —
  `--tls=false` is a hard error ("`--tls` does not take a value"), on purpose: before this was
  enforced it silently parsed as *enabling* TLS, the exact opposite of what it looked like it did.
- The token never appears in `list`/`show` output (only a `hasToken` boolean) or, via `--token-stdin`,
  in argv (which is world-readable through `ps`).
- **`[remote] host` in `config.toml` still works** and shows up in `clowder remote list` as a
  read-only entry (`source` = `config`) — `clowder remote set`/`rm` refuse to touch it and name the
  fix (edit `config.toml`, or add a separate registry entry alongside it). If a registry entry's
  address matches `[remote] host`'s, the registry entry wins entirely and the config entry is hidden
  from `list` until that registry entry is removed.
- `untrust` and `rm` prune the matching `remote_known_hosts` line, but **only when no other entry —
  including the `[remote] host` virtual entry — still dials that same address.** Two nicknames can
  point at one daemon; removing or untrusting one must not silently un-trust the other.

## Pairing

`clowder remote probe` reaches a daemon and reports what it presented, **without saving anything** —
not the registry, not `remote_known_hosts`. That's the point of splitting pairing into two steps:
observing and trusting are separate acts, with a human in between.

```sh
clowder remote probe studio                                  # a saved registry entry
clowder remote probe --address 10.0.0.9:7777 --tls           # a host not yet in the registry
clowder remote probe studio --json
```

Prints reachability, whether TLS was seen, the observed fingerprint, how it compares to any existing
pin (`new` / `match` / `changed`), and whether the token authenticated — `auth` reads `none
(plaintext daemon)` against a plaintext listener, since a plaintext daemon accepts any token and
reporting "authenticated" there would be a lie.

`--timeout` (default `3`, in seconds) bounds the TCP connect, the TLS handshake, and the read of the
daemon's first line **separately, each by the same value** — so one probe can take up to roughly 3×
what you pass, about 9 seconds worst-case at the default. That's expected, not a bug to file.

Once you've seen a fingerprint you trust, record it:

```sh
clowder remote trust studio --fingerprint <hex>            # from the probe output above
clowder remote trust studio --fingerprint <hex> --verify   # re-probes and refuses on any mismatch
clowder remote untrust studio                               # clear the pin (see the pruning rule above)
```

**Pairing only closes the MITM window if the fingerprint is compared out-of-band** — that is, through
a channel the network path you're pairing over isn't also carrying. Don't compare the fingerprint
`probe` just showed you against itself; compare it against a source that didn't come over that same
wire:

- `clowder remote-token`, run **on the daemon host itself** (SSH in, or a local terminal there), or
- the daemon's own startup log line (`remote TLS enabled — token: … cert fingerprint (sha256): …`).

Without that out-of-band check, `probe` → `trust` is trust-on-first-use with extra clicks: a MITM
sitting on the connection at probe time can hand you its own certificate, and you'd dutifully pin it.
`--verify` closes the *probe-to-trust* TOCTOU window (a cert swapped in the moment between the two
commands) but does nothing for a MITM that was there for both.

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
  and rotate (see Rotation above — deleting `remote-token` and restarting also regenerates the cert, so
  clients will need to re-trust) if you suspect exposure.

**Deferred (not in this phase):** mutual TLS (client certs), QUIC transport, a pinned-pairing UX (e.g.
QR/short-code exchange instead of manual fingerprint comparison), and OS Keychain storage for the token
(it currently lives in a plain `0600` file).
