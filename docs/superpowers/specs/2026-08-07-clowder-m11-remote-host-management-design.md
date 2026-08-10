# clowder M11 — remote host management, connection status, and switching

## Context

M7 gave clowder a remote daemon: an opt-in TCP listener (M7a), a `clowder connect` forwarder that proxies
local Unix sockets to it (M7b), an app "remote mode" (M7c1) and a live local↔remote swap (M7c2), then TLS
+ bearer token + TOFU cert pinning (M7d).

What M7 did **not** give it is a *list*. The app knows about exactly **one** remote daemon, resolved once
at startup by shelling out to `clowder remote-host` (`macos/Sources/ClowderApp/App.swift:116`), which
prints the config's `[remote] host`. The menu bar shows a static `Local` / `Remote: <host>` header and a
single toggle to the other mode. There is no way to add, name, edit, or choose between hosts — in the app
or on the CLI — and no persistent indication in the main window of which daemon you are looking at.

M11 adds that: **manage a list of remote daemons from the UI, always see which one you are connected to,
and switch between them.**

### What exists (ground truth, verified 2026-08-07 at `26e0f86`)

- **Config** (`crates/clowder-config/src/lib.rs`): `remote_listen` / `remote_host` / `remote_tls` /
  `remote_token`, resolved **env › file › default** by the pure, table-tested `Config::resolve`. Path
  helpers `runtime_dir()`, `remote_state_dir()` (`$XDG_STATE_HOME › ~/.local/state` + `/clowder`),
  `remote_cert_path()` / `remote_key_path()` / `remote_token_path()`.
- **Forwarder** (`crates/clowder-client/src/forward.rs`): `forward(host, dir, token)` binds
  `dir/clowder.sock` + `dir/clowder-control.sock` and proxies each accepted connection via
  `forward_stream`; `dial_with_backoff` retries 6× (0.5 s→8 s). **TLS is selected purely by
  `token.is_some()`.**
- **Trust** (`crates/clowder-client/src/tofu.rs`): `known_hosts_path()` =
  `<remote_state_dir>/remote_known_hosts`, lines `<host> <sha256-hex>`; `check()` records on first sight
  and refuses on change; `TofuVerifier` also performs real TLS 1.2/1.3 signature verification. Keyed on
  **the host string as typed**.
- **CLI** (`crates/clowder-client/src/main.rs`): hand-rolled `std::env::args()` dispatch, no clap.
  `connect [host:port]`, `remote-host`, `remote-token`, `spawn`, `project add|list|rm`, `attach`.
- **Daemon** (`crates/clowder-daemon/src/remote.rs`): `handle_remote_conn<S>` is generic over the stream —
  reads the hello, constant-time-compares the token, dispatches to control or render. The control handler
  emits a `worktreeList` event **unprompted** the moment it dispatches; a bad token bails **before**
  dispatch.
- **App** (`macos/`): `AppDelegate.switchBackend(to remoteHost: String?)` does the live swap —
  `makeBackendSupervisor` → `daemonSupervisor?.stop()` → `start()` → `appModel.reconnect(makeTransport:)`
  → `surfaceHost.retarget(socketPath:)`. `DaemonSupervisor` has `State { stopped, running, relaunching,
  yielded }`; exit code 3 (lost the single-instance flock) → `.yielded`, anything else relaunches with
  backoff. `ClowderCore/RemotePaths.swift::forwarderSocketDir(controlPath:)` re-derives the forwarder's
  socket directory in Swift to match Rust's `parent()/remote`.
- **App persistence**: essentially none — one `UserDefaults` key, `clowder.sidebar.expandedProjects`.
  There is **no `Settings` scene**.
- **Testing shape**: `ClowderCore` is unit-tested (21 XCTest files, injected fakes, a `SleepController`
  virtual clock); the `clowder-app` executable target has **no tests** and `swift test` never compiles it.
  This is why testable remote logic was pushed down into `ClowderCore` in M7c.

### Two defects this work must fix

1. **Switching away from Local kills every running local agent.** `switchBackend` calls
   `daemonSupervisor?.stop()`, and agents are PTY children of the daemon that do not survive a restart
   (documented in `AGENTS.md`). Tolerable for a rarely-used toggle; unacceptable once there is a host
   picker people use casually.
2. **`ContentView(isRemote:)` is already stale.** It is computed once in the scene body as
   `delegate.configuredRemoteHost != nil` and never updated by `switchBackend`, so
   `AddProjectSheet(canBrowse:)` is wrong after any swap.

### User decisions (brainstorm, 2026-08-07)

- **One active backend at a time** — switching replaces the current one — but the model carries a
  `BackendID` identity and forwarder sockets become per-host, so multi-connect is additive later.
- **Rust owns the host registry**; the app only ever shells out to `clowder remote …`, as it already does
  for `remote-host`.
- **Host identity is a required unique nickname**; the address is editable underneath.
- **Per-host token + TLS flag**, plus a **pairing flow** — probe shows the daemon's cert fingerprint, the
  user confirms, then it is pinned.
- **All four UI surfaces**: a sidebar-footer connection chip, a `Settings` window (⌘,), the menu-bar list,
  and command-palette entries.
- **Switching away from Local detaches rather than terminates**, so local agents survive; **quit still
  terminates everything**, as today.

Two decisions were revised during design, on evidence from the code:

- **The trust pin moves onto the host record** rather than being keyed by address (see Component design
  §2). Address-keyed pins mean editing a host's address silently reverts it to trust-on-first-use and
  re-pins whatever answers at the new address — a security downgrade with no user-visible signal, which
  is precisely the window the pairing flow exists to close.
- **`clowder connect` gains exit code 4** for "the first dial never landed" (see Risks §1). Without it,
  an unreachable host becomes an unbounded supervisor relaunch loop behind a permanent "Reconnecting…".

## Goals / Non-goals

**Goals:** (1) a persistent, nicknamed list of remote daemons with per-host address, TLS flag, token, and
trust pin; (2) a CLI to manage it that works with no daemon running; (3) a probe/trust pairing flow that
shows the daemon's fingerprint before anything is pinned; (4) a live, always-visible indication of which
backend the app is connected to, and its health; (5) switching between Local and any host from four
surfaces, **without killing running agents**; (6) full back-compat for `[remote] host`-based setups and
for `clowder connect <host:port>`.

**Non-goals:** connecting to several daemons at once (the model is shaped for it, the feature is not
built); a merged cross-host sidebar; macOS Keychain token storage (still deferred from M7d); mTLS or
QUIC; a file watcher on the registry; syncing hosts between machines; any change to the daemon's
listener, the wire protocol, or the local Unix path.

## Component design

### 1. The host registry (`crates/clowder-config/src/hosts.rs`, new)

**Not daemon-owned.** `projects.json` is mutated through the control socket, but the host list must be
readable and writable when **nothing is reachable** — that is its entire purpose. `clowder remote …`
therefore reads and writes the file directly, in-process, with no daemon. "Rust owns the registry" must
not be read as "the daemon owns the registry".

**In `clowder-config`**, because it already owns `remote_state_dir()` and the `[remote]` keys the registry
has to merge with, both binaries can reach it without a dependency inversion, and the merge rule is
exactly the kind of pure resolution `Config::resolve` already embodies. Cost: a `serde_json` dependency,
and warnings go to `eprintln!` matching `read_file`'s existing style.

```rust
#[derive(Serialize, Deserialize, …)]
#[serde(rename_all = "camelCase")]
pub struct HostRecord {
    pub name: String,                            // unique nickname; the identity
    pub address: String,                         // host:port (or [v6]:port); editable
    #[serde(default)] pub tls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub fingerprint: Option<String>,
}
```

- **Location:** `$CLOWDER_HOSTS_FILE › <remote_state_dir()>/hosts.json` — the same directory that already
  holds `remote_known_hosts`, `remote-cert.pem`, `agents.json`, `projects.json`, and which
  `ClowderPaths.stateDir()` mirrors. The env override matches `Registry::default_path` /
  `ProjectStore::default_path` and is what makes the store testable without touching `$HOME`.
- **Format:** a bare JSON array, matching `agents.json` / `projects.json`, evolved by additive
  `#[serde(default)]` fields — the mechanism already proven by `AgentRecord::tree`. No version key; this
  repo's precedent is additive fields, and a version key invites migration code nobody will write.
  `camelCase` matches the house JSON convention and lets Swift `Codable` decode with default key coding.
- **Permissions:** the file holds bearer tokens. Directory `0700`, file `0600`, reusing the discipline in
  `remote_tls.rs::write_0600`. The atomic temp file must be **created** `0600` before the rename, not
  chmod'd after — `JsonStore::try_write` uses `std::fs::write` for its temp and so does not satisfy this.
  On load, a too-wide mode is warned about and tightened rather than refused (refusing would wedge the app).
- **`HostsStore { load(), try_mutate() }`** is shaped like `clowder-daemon`'s `JsonStore` — `load` never
  panics (corrupt ⇒ empty + warn), `try_mutate` surfaces write errors because they answer a user request —
  but is **not** the same code: wrong crate, wrong permissions, and `JsonStore` leans on the daemon's
  single-instance flock for cross-process safety, which the CLI does not have. `try_mutate` therefore
  holds an advisory `flock` across the whole load-modify-write. Without it, a shell `clowder remote add`
  concurrent with a Settings-pane save is silently lost, and two interactive writers is the normal case
  here.

**Merging `[remote] host` — virtual, never written:**

```rust
pub enum HostSource { Registry, Config }
pub struct HostEntry { pub record: HostRecord, pub source: HostSource }
pub fn merged_hosts(file: Vec<HostRecord>, cfg: &Config) -> Vec<HostEntry>;   // pure, no I/O
```

File records first, in file order. If `cfg.remote_host` is set and no file record shares that address, a
synthetic entry is appended (`name: "config"`, `tls: cfg.remote_tls || cfg.remote_token.is_some()`, the
config token, no pin) with `source: Config`. If a file record already has that address, the file record
wins **entirely** — no per-field merging, because nobody can debug "why is my config token overriding my
registry token". A name collision on `"config"` falls back to naming the entry by its address.

A one-time migration was rejected: it means a later hand-edit of `config.toml` silently stops taking
effect, and the migration itself can clobber. Virtual merge keeps `config.toml` authoritative forever and
is idempotent. `Config`-sourced entries are **read-only in the UI**, labelled "defined in config.toml".

### 2. Trust: three policies, and the pin moves to the record

`tofu.rs`'s single record-on-first-sight verifier becomes a three-armed policy. The TLS
signature-verification impls (the actual proof of key possession) stay shared across all three.

| Arm | Behavior |
|---|---|
| `Trust::Pinned(fp)` | Strict compare against the entry's pin. Never records, never consults `remote_known_hosts`. |
| `Trust::Tofu { host, known_hosts }` | Today's behavior verbatim — `check()` and its existing test are unchanged. Used by unpinned and legacy entries. |
| `Trust::Capture(sink)` | Probe only: accept, publish the fingerprint to the sink, **persist nothing**. |

**The entry's `fingerprint` field is authoritative when present**; `remote_known_hosts` is still written
on `trust` (so a shell `clowder connect <addr>` agrees) and is still read for entries with no pin. The
consequences are the right ones in both directions: editing an address keeps the pin, so "same box, new
DNS name" just works, while "different box, reused nickname" fails loudly with a fingerprint-changed
error and a re-pair affordance.

### 3. Target resolution and the token/TLS decoupling (`forward.rs`)

`forward` and `forward_stream` take a `RemoteTarget { label, address, token, tls, trust }`, and
`forward_stream` branches on `target.tls` rather than `token.is_some()`. Two rules keep that safe and
backward-compatible:

- **A token is only ever sent over TLS.** `token && !tls` is refused with a fix-it message. Sending a
  bearer token in cleartext is worse than not sending it, and the daemon ignores it in plaintext mode
  anyway (`serve_remote` passes `expected_token: None`).
- **The config-derived path computes `tls = cfg.remote_tls || cfg.remote_token.is_some()`.**
  `docs/remote-tls.md` documents `tls` as a *daemon* key and tells clients to set only `host` + `token`,
  so **every existing TLS user has `remote_tls == false`** and would otherwise be silently downgraded.

`resolve_target(selector, &hosts, &cfg) -> Result<RemoteTarget>` is pure and table-tested: no selector →
the config entry; exact name match; exact address match; a selector containing `:` that parses as an
address → an ad-hoc TOFU target using the config token (verbatim back-compat with today's documented
`clowder connect host:port`); otherwise an error naming `clowder remote list`.

`clowder connect` also gains **`--socket-dir`**, so the app tells the forwarder where to bind instead of
`RemotePaths.swift` re-deriving Rust's rule in Swift. One authority instead of two, and it makes the
directory per-host (`<runtime>/clowder/remote/<name>/`) — the concrete step that keeps a future
multi-connect from being a rewrite.

### 4. Probe (`crates/clowder-client/src/probe.rs`, new)

`probe(target, timeout) -> ProbeResult { reachable, fingerprint, authenticated, error }`:

1. `TcpStream::connect` under a 3 s timeout — deliberately **not** `dial_with_backoff`, which takes ~15 s
   to give up. A probe must fail in seconds.
2. If `target.tls`, handshake with `Trust::Capture` and read the fingerprint out of the sink. A handshake
   failure yields `reachable: true, fingerprint: None` plus the error.
3. `write_hello(Control, token)`, then read **one line** under the timeout. Per the daemon's behavior
   above (unprompted `worktreeList` on dispatch, bail-before-dispatch on a bad token — both already
   asserted by existing daemon tests), a line ⇒ authenticated, EOF/timeout ⇒ not.
4. Drop. Nothing is persisted — not `remote_known_hosts`, not `hosts.json`.

Against a **plaintext** daemon `expected_token` is `None`, so any token "authenticates". This is reported
honestly rather than as a success: the pairing UI says "no authentication (plaintext daemon)".

`clowder remote trust <name> --fingerprint <hex>` records the human's decision without re-probing
(`--verify` re-probes and refuses on mismatch). The probe→trust TOCTOU window is real and accepted: the
UI passes back verbatim the fingerprint it displayed, so a cert swapped between the two calls yields a pin
that fails loudly on the very next connect.

### 5. CLI surface (`crates/clowder-client/src/remote_cli.rs`, new)

`main.rs` gains exactly one arm — `Some("remote") => remote_cli::run(&args[2..])` — the same treatment
`forward` already gets. Flags are handled by a ~30-line `parse_flags` supporting `--k v` / `--k=v` /
`--bool` / `--tls`/`--no-tls`, plus `reject_unknown` so typos fail loudly. That is the whole "argument
framework"; no clap.

| Command | Notes |
|---|---|
| `remote list`, `remote show <name>` | emit `hasToken: bool` — **the token is never printed** |
| `remote add <name> <address>` | `--token-stdin` / `--token`, `--tls`/`--no-tls` |
| `remote set <name>` | `--rename`, `--address`, `--token-stdin`/`--no-token`, `--tls`/`--no-tls` |
| `remote rm <name>` | prunes the `remote_known_hosts` line only if no other entry shares that address |
| `remote probe <name>` / `--address <a>` | `--timeout` (default 3 s); reports `reachable`, `fingerprint`, `fingerprintMatch ∈ new\|match\|changed`, `authenticated`. Writes nothing |
| `remote trust <name> --fingerprint <hex>` | `--verify` re-probes first |
| `remote untrust <name>` | clears the pin |

`remote-host` and `remote-token` are unchanged (both are documented in `docs/remote-tls.md`).

**stdout contract:** human output is TSV, matching `clowder project list`. With `--json`, a single JSON
object on stdout **including for errors** (`{"error": "…"}`) plus a non-zero exit. Swift decodes stdout
first and only falls back to the exit code, so a stray warning from some future library cannot corrupt the
parse. This replaces the fragile "one trimmed line" contract `resolveRemoteHost` uses today.

**The app always passes tokens via `--token-stdin`**, never argv, because argv is world-readable via `ps`.

**Cross-language fixtures** (`docs/protocol/` already institutionalizes these): `remote-host-list.json`
and `remote-probe.json` (Rust encodes byte-exact, Swift decodes), and `host-names.json` in the
`worktree-names.json` shape, checked against **both** Rust's `validate_name` and Swift's
`HostDraft.nameError` so the two validators cannot drift.

### 6. App: identity, plans, and detach

`ClowderApp` has no tests and `swift test` never compiles it, so every decision below lives in
`ClowderCore`; `ClowderApp` gets renderers and `Process` plumbing only. The acceptance criterion for each
new `ClowderApp` file is "contains no branch a test could meaningfully cover".

New in `ClowderCore`:

- **`RemoteHost.swift`** — `HostID`, `BackendID { local, remote(HostID) }`, `HostSource`,
  `RemoteHost { name, address, tls, hasToken, fingerprint, source }`, `HostProbe`. `BackendID` is the
  identity carried on state and selection; `RemoteHost` is the value needed to launch a backend. Keeping
  them separate is what makes multi-connect additive rather than a rewrite.
- **`HostRegistry.swift`** — the shell-out layer behind an injectable
  `protocol CommandRunner { run(_ args:, stdin:) -> CommandResult }`, so `list` / `add` / `update` /
  `remove` / `probe` / `trust` are unit-testable with a fake that asserts exact argv and returns canned
  JSON. Because the CLI exposes only `hasToken`, **the app never holds a token at rest**.
- **`BackendPlan.swift`** (grown from `RemotePaths.swift`) — a pure
  `backendPlan(target:sockets:) -> BackendPlan { id, executable, args, envOverlay, controlPath, renderPath }`.
  `makeBackendSupervisor` shrinks to "resolve the binary, build a `ProcessDaemon` from the plan". The
  existing `forwarderSocketDir(controlPath:)` and its test stay; a host-scoped overload is added.
- **`DaemonSupervisor` detach/resume** — `DaemonProcess` gains `isRunning`; `State` gains `.detached` and
  `.failed(String)`. `detach()` cancels the relaunch task and **keeps the process handle without
  terminating it**; `resume()` re-adopts a still-live process or relaunches. Keeping the handle (rather
  than orphaning) is what avoids respawning a doomed daemon on every switch-back. `.failed` is entered on
  exit code 4, in exactly the shape of the existing code-3 → `.yielded` rule.
- **`AppModel`** — `@Published activeBackend: BackendID`, `reconnect(to:makeTransport:)`, and a
  `BackendSwitching` protocol (`hosts`, `activeBackend`, `switchBackend(to:)`, `refreshHosts()`) that
  `AppDelegate` conforms to, so all four surfaces read one source. Plus
  `lastSelection: [BackendID: SidebarSelection]`: `reconnect` clears `selection`, so without it switching
  back loses your place — with it, switching feels like tabs rather than a restart.
- **`ConnectionChip.swift`** — a pure `connectionChip(backend:hosts:state:) -> ConnectionChip
  { title, detail, symbol, tone }` covering live, connecting (pending, no banner — mirroring the startup
  grace period), reconnecting, closed, and a host id no longer present in the list.
- **`PaletteSearch`** — a `.backend(BackendID)` item kind and a `hosts:` parameter defaulted to `[]`, so
  existing tests and `CommandPaletteView` keep compiling. Deliberately **not** a new `CommandID`:
  `CommandRegistry.all` is static and `AppModel.run` / `isEnabled` stay untouched.
- **`HostDraft`** in `SheetForms.swift` beside `AddProjectForm` / `NewWorktreeForm`, and a
  `HostsViewModel` owning the Settings pane's state and operations — so the whole pane is driven
  end-to-end in `swift test` with the fake runner.

In `ClowderApp`: a `ProcessCommandRunner` keeping `resolveRemoteHost`'s read-before-wait discipline
(which is then deleted); `AppDelegate` holding `supervisors: [BackendID: DaemonSupervisor]` where
`switchBackend` **detaches** a local daemon but **stops** a forwarder (forwarders hold no state and would
collide on rebind); `StatusBarController`'s three closures collapsing into one `BackendSwitching`
reference with `NSMenuItem.state` checkmarks replacing the disabled header; a sidebar-footer
`ConnectionChipView` (a `Menu` with `SettingsLink`) attached via `.safeAreaInset` **on the sidebar `List`**
— the existing bottom inset is the window-wide error banner and must stay where it is; and `ContentView`
dropping `isRemote:` for `model.activeBackend == .local`, fixing defect 2.

### 7. Settings scene

A `Settings` scene (none exists today; ⌘, comes free) rendering a `TabView` with one Hosts tab, so
General/Keys can be added later without restructuring. The Settings body cannot see the `WindowGroup`'s
`@EnvironmentObject`, so the view model is passed explicitly from the idempotent `bootstrap()`.

The pane is a list with a `+`/`−` footer beside an editor: name, address, TLS toggle, a `SecureField`
token showing "•••• (stored)" when `hasToken`, and a trust row showing the fingerprint in monospaced
4-character groups (or "Not paired") with a **Pair…** button. `source == .config` renders read-only.

The pairing sheet probes on appear, then shows the fingerprint, the token result, and — critically —
**names the out-of-band source of truth** (`clowder remote-token` on the daemon host, or its startup log),
with an optional paste-to-compare field that disables **Trust** on mismatch. Without that, pairing is TOFU
with extra clicks: the window only closes if the user actually compares.

## Data flow

```
manage:   Settings/CLI → clowder remote add|set|rm  → flock → hosts.json (0600)
list:     app → clowder remote list --json → merged_hosts(file, config) → [RemoteHost] → chip/menu/palette/Settings
pair:     app → clowder remote probe --json → TCP+TLS(Capture) → fingerprint + auth  (persists nothing)
          user compares out-of-band → clowder remote trust → record.fingerprint + remote_known_hosts
switch:   BackendID → probe (3s) → local? detach : stop  → backendPlan → supervisor.start/resume
                    → appModel.reconnect(to:) → surfaceHost.retarget → restore lastSelection
connect:  clowder connect <name> --socket-dir <dir> → resolve_target → Trust::Pinned|Tofu → forward
quit:     every supervisor stopped, including a detached local daemon (unchanged behavior)
```

## Error handling

- **Unreachable host** — `switchBackend` probes first (3 s) and refuses with an explanation rather than
  tearing down a healthy local session. If a forwarder is started anyway and its first dial never lands, it
  exits **4** → `.failed` → a red chip naming the address, with Retry. No unbounded relaunch loop.
- **Fingerprint changed** — a pinned entry refuses loudly, without consulting or writing
  `remote_known_hosts`, and surfaces a Re-pair affordance.
- **Token without TLS** — refused at target resolution with a fix-it message.
- **Corrupt or unreadable `hosts.json`** — loads as empty with a warning; never panics, never truncates
  on read.
- **Concurrent writers** — the `flock` in `try_mutate` serializes the app and any shell.
- **CLI failure** — `{"error": "…"}` on stdout plus a non-zero exit; the app surfaces the message verbatim
  rather than a generic failure.
- **Empty registry** — the chip offers "Add a remote host…" and Settings opens with a prominent `+`. No
  dead ends.

## Testing

- **Rust, pure** (the house `Config::resolve` style, table-driven): `merged_hosts` across every merge rule
  plus the name collision; `validate_name` / `validate_address` driven by the shared fixture;
  `resolve_target` across all five rules plus the token-without-TLS refusal; `parse_flags` /
  `reject_unknown`; JSON encoders byte-for-byte against the golden fixtures.
- **Rust, I/O:** `HostsStore` round-trip; corrupt file → empty and never panics (mirroring
  `corrupt_file_loads_empty`); concurrent `try_mutate` loses no records (mirroring
  `concurrent_upserts_do_not_lose_records`); the file **and its temp** are `0600`; `$CLOWDER_HOSTS_FILE`
  is honored. Any test setting `XDG_STATE_HOME` must hold the crate's existing env mutex for its whole
  span.
- **Rust, integration** (`clowder-client` already dev-depends on `clowder-daemon`): probe against a real
  `serve_remote` — good token → authenticated; wrong token → not, with the fingerprint still captured;
  plaintext daemon → no fingerprint; dead port → fast `reachable: false`. Plus: rotate the daemon cert and
  assert `Trust::Pinned` refuses **without** reading or writing `remote_known_hosts`.
- **Swift `ClowderCore`:** a `FakeCommandRunner` asserting exact argv and returning canned JSON and the
  golden fixtures (loaded the way `ModelsTests.testDecodesEveryGoldenFixture` already does);
  `FakeDaemonProcess` extended with `isRunning` for the detach/resume/failed matrix; `SleepController` +
  `eventually()` for supervisor timing; `FakeControlTransport` for `reconnect(to:)` / `activeBackend` /
  `lastSelection`; pure assertions for `backendPlan`, `connectionChip`, `paletteResults(hosts:)`,
  `HostDraft`, and the full `HostsViewModel` including every error path.
- **Manual (GUI):** pair a real second machine end to end; switch local↔remote with agents running on both
  sides and confirm both survive; switch back and confirm selection is restored; point a host at a dead
  address and confirm a red chip with Retry rather than a spin; edit a paired host's address; enter a
  deliberately wrong expected fingerprint and confirm Trust is refused.
- Full suites green: `cargo test --workspace --locked`, `cd macos && swift test`.

## Risks

1. **An unreachable host becomes an infinite relaunch loop.** `clowder connect` retries ~15 s then exits
   1; `handleExit` treats any non-3 code as a crash and relaunches forever, with the chip stuck on
   "Reconnecting…" and no way to distinguish "wrong address" from "daemon is down". Mitigated by exit
   code 4 + `.failed`, and by probing before the switch.
2. **Switching with agents running.** Local→remote detaches, so the PTYs live and `retarget` frees only
   the `clowder attach` clients; remote→local drops the TCP link while the remote daemon keeps its agents.
   Both directions are lossless. The residual wart — `reconnect`'s `store.reset()` clearing selection — is
   what `lastSelection` covers.
3. **Two sources of truth for a pin.** Stated once in `tofu.rs`: the entry's pin wins;
   `remote_known_hosts` is write-on-trust and read-only-when-unpinned; `remote rm` prunes its line only
   when no other entry uses that address.
4. **The registry racing hand-edits of `config.toml`.** Virtual merge means config edits always apply and
   are never clobbered; the `flock` covers two processes writing `hosts.json`. No file watcher — the app
   refreshes on menu / Settings / palette open, an explicit non-goal.
5. **The single-instance flock on reattach.** Detach/resume means switching away and back never respawns
   the local daemon, so there is no contention in the common path. The exit-3 → `.yielded` path still
   covers an externally-started daemon, and in that state the chip should read "Local (external daemon)"
   rather than an error — it is a healthy state.
6. **Token exposure.** Always `--token-stdin`, never argv. At rest a `0600` plaintext file, identical to
   today's `remote-token`; because the CLI exposes only `hasToken`, the deferred Keychain move touches
   Rust only.
7. **Pairing only closes the MITM window if the user actually compares out-of-band** — otherwise it is
   TOFU with extra clicks. Mitigated by naming the source of truth in the sheet and offering
   paste-to-compare so the comparison is done by software rather than by eye.
8. **Probe→trust TOCTOU** — accepted; a cert swapped between the two calls produces a pin that fails
   loudly on the very next connect.

## Decomposition

- **M11a — Rust registry, CLI, probe/pairing primitives.** `hosts.rs` (record, store, `merged_hosts`),
  the three-armed `Trust`, `RemoteTarget` + the TLS/token decoupling, `resolve_target`, `probe.rs`,
  `remote_cli.rs` and the `connect` changes (`--socket-dir`, exit 4), fixtures, docs.
- **M11b — the app consumes the list.** `RemoteHost`, `HostRegistry`, `BackendPlan`, supervisor
  detach/resume/failed, `AppModel.activeBackend` + `BackendSwitching` + `lastSelection`, `connectionChip`,
  palette entries; then the chip, the menu-bar host list, multi-supervisor `switchBackend`, and the
  `isRemote` fix.
- **M11c — Settings window.** `HostDraft`, `HostsViewModel`, the `Settings` scene, the hosts pane and
  editor, and the pairing sheet.

Each ships a usable increment: after M11a the CLI alone manages hosts; after M11b the app can switch
between hosts defined on the CLI; M11c makes it self-service.

## Verification gate

`clowder remote add|set|rm|list|show|probe|trust` manages a nicknamed host list in a `0600`
`hosts.json` with no daemon running, safely interleaved with a concurrent writer, while `[remote] host`
still appears as a read-only entry and `clowder connect <host:port>` behaves exactly as before. The app
shows a live connection chip naming the active backend, lists every host in the sidebar chip, the menu
bar, and the command palette, and switches between them **without killing running agents on either side**,
restoring the previous selection on return. A new host can be added, probed, compared against an
out-of-band fingerprint, and trusted entirely from the Settings window; a pinned host whose certificate
changes is refused loudly; and an unreachable host produces a red chip with Retry rather than an endless
reconnect. Deferred: simultaneous multi-host connections, a merged cross-host sidebar, and Keychain token
storage.
