# muxy M5 — Robustness: Config + Lifecycle Hardening

## Context

muxy works end-to-end on macOS (M0–M4), but the daemon is dev-grade: everything is env-vars
+ hardcoded constants (no config file), a second daemon blindly steals the first's sockets,
shutdown orphans child processes, companion panes that crash are never cleaned up, the whole
daemon shares poison-prone `std::sync::Mutex` locks (one panic in a locked section →
permanent panic-on-every-access), and the macOS app gives up permanently on a dropped
connection. M5 — **Robustness** — hardens exactly these: a **config-file system** and a set of
**lifecycle/crash** fixes.

Two north-star robustness items are **deliberately deferred** (each is a milestone-sized
subsystem, out of M5): the authoritative daemon-side VT grid → scrollback **reflow-on-resize**
(muxy has no cell grid — `muxy-vt` is signal-only, libghostty reflows the live screen
client-side), and **agents surviving a daemon restart** (PTYs are daemon child processes; true
survival needs re-parenting/persistence).

### What exists (ground truth)

- **Backlog:** `crates/muxy-daemon/src/pane.rs:8` `const BACKLOG_CAP = 256*1024` (hardcoded);
  raw byte-tail ring buffer `Arc<Mutex<Vec<u8>>>`; `snapshot_and_subscribe()` (`pane.rs:132`).
- **Daemon state:** `server.rs` — `Arc<Mutex<HashMap>>` for `panes`/`attention`/`workspaces`/
  `agents`/`watchers`/`trees`/`owner`/`hookless`/`scanners`, all accessed via bare
  `.lock().unwrap()`. `Daemon::new_with(notifier, hook_sock)` (driver is auto-detected per M3c);
  hook socket hardcoded `/tmp/muxy-hook.sock` at `Daemon::new` (`server.rs:49`).
- **Agent reap:** per-agent watcher (`server.rs:187-194`) → `wait_exit()` → `AttentionState::Exited`
  (mark-and-keep); handle in `watchers`; aborted on teardown (`server.rs:227-229`).
- **Companion panes:** `spawn_pane` (`server.rs:292-320`) registers **no** watcher — a crashed
  companion is never detected/removed (only explicit `close_pane` cleans it). `owner: HashMap<PaneId,
  PaneId>` maps a companion/leaf → its agent; the per-agent `trees` hold the split layout.
- **Startup/shutdown:** `main.rs` binds 3 Unix sockets, blindly `remove_file`s each before bind
  (no single-instance guard); no signal handler, no `Drop`, no child-kill on exit → children
  orphaned. `MUXY_SOCK`→`/tmp/muxy.sock`, `MUXY_CONTROL_SOCK`→`/tmp/muxy-control.sock`.
- **Client:** `muxy-client/src/lib.rs` reads `MUXY_SOCK`, SIGWINCH→`Resize`. Swift `AppModel`
  `connect()` sends `listAgents`/`listAdapters`; on disconnect → `.closed(reason:)` with **no**
  reconnect (`AppModel.swift:65-77`).
- **Errors:** per-conn tasks are `let _ =`-spawned (silent death, no logging); broadcast `Lagged`
  is handled everywhere (`=> continue`). No `tracing`/logging. No config crate anywhere.

## Goals / Non-goals

**Goals:** (1) a **config file** (`toml`) layering under env + defaults so users can set socket
paths, backlog cap, shell, and default pane size; (2) a **single-instance guard** so a second
daemon can't hijack a running one; (3) **graceful shutdown** that kills child PTYs + cleans up;
(4) **companion-crash reaping** (auto-remove the leaf + notify); (5) **no lock-poison cascade**
(swap to `parking_lot`); (6) basic **logging** of task/connection errors; (7) macOS app
**auto-reconnect** on a dropped daemon connection.

**Non-goals (deferred):** the authoritative VT grid / scrollback reflow-on-resize; agents
surviving a daemon restart (PTY re-parenting/persistence); the QUIC/remote transport (M7);
cross-language config (the Swift app keeps env-based socket resolution — config is Rust-side);
per-agent config; hot config reload; a config-editing UI.

## Component design

### 1. Config system — `muxy-config` (new crate) + wiring

A small crate (or `muxy-daemon::config` module) using `serde` + `toml`. **Precedence per field:
env var › config file › hardcoded default** — existing env/dev flows are unchanged; the file is
purely additive.

```rust
// muxy-config: all fields optional in the file; resolver applies env override then default.
pub struct Config {
    pub client_sock: PathBuf,   // MUXY_SOCK        | sockets.client  | /tmp/muxy.sock
    pub control_sock: PathBuf,  // MUXY_CONTROL_SOCK| sockets.control | /tmp/muxy-control.sock
    pub hook_sock: PathBuf,     // MUXY_HOOK_SOCK   | sockets.hook    | /tmp/muxy-hook.sock
    pub backlog_cap: usize,     // MUXY_BACKLOG_CAP | pane.backlog_cap| 262144
    pub shell: Option<String>,  // SHELL            | pane.shell      | $SHELL/ /bin/sh at use
    pub default_cols: u16,      //                  | pane.cols       | 80
    pub default_rows: u16,      //                  | pane.rows       | 24
}
impl Config {
    /// Load ~/.config/muxy/config.toml (honoring $XDG_CONFIG_HOME), then apply env overrides;
    /// a missing/partial/invalid file is non-fatal (log + fall back to defaults).
    pub fn load() -> Config;
}
```

- **Location:** `$XDG_CONFIG_HOME/muxy/config.toml`, else `$HOME/.config/muxy/config.toml`
  (explicit XDG-style, consistent on macOS; via the `directories` crate or manual `$HOME` join).
- **`config.toml` shape:** `[sockets] client/control/hook`, `[pane] backlog_cap/shell/cols/rows`.
- **Wiring:** `main.rs` calls `Config::load()` once and uses it for the three socket binds;
  passes it (or its fields) into `Daemon::new_with` (which drops the hardcoded hook path) so the
  daemon knows `backlog_cap`/`shell`/pane size. `pane.rs` `BACKLOG_CAP` const → a `Pane` field
  set from config (thread the cap through `Pane::spawn`). `muxy-client` reads the same socket
  precedence (env › file › default) so `muxy attach` finds a relocated socket.
- **Boundary:** the Swift app is **out of scope** for config — it keeps env-based socket
  resolution (set by dev / M6 packaging). Documented, not a regression.

### 2. Daemon lifecycle — `muxy-daemon` (`main.rs` + `server.rs`)

- **Single-instance guard:** a **PID file + advisory exclusive `flock`** at
  `<runtime_dir>/muxy/daemon.pid` (`$XDG_RUNTIME_DIR` › `$TMPDIR` › `/tmp`). On startup, try to
  take the lock: held by a live daemon → **refuse to start** (clear error, non-zero exit); lock
  free / stale → take it, then clean up stale sockets and bind. Replaces the current blind
  `remove_file`. (Advisory `flock` via `rustix`/`fs4`; the plan pins the crate.)
- **Graceful shutdown:** a `tokio::signal` SIGTERM/SIGINT handler triggers a shutdown routine —
  **kill every child PTY process** (a `Daemon::shutdown()` that iterates `panes` and kills), then
  remove the sockets + PID file, then exit. Backstop: **`Drop for Pane`** kills its child, so a
  dropped pane never leaks a process. (Ordering: signal → stop accepting → kill children → unlink
  → exit; the reaper watchers are aborted, not raced.)
- **Companion-crash reap:** `spawn_pane` gains an exit watcher (mirroring the agent watcher) whose
  handle is stored (in `watchers` or a `companion_watchers` map). On companion process exit it calls
  a new **`reap_companion(pane)`**: look up the `owner` agent, **remove the leaf from that agent's
  `trees` (remove_leaf → collapse)**, drop the pane + maps, and **broadcast `SplitTreeChanged`** so
  the client drops the subtree. Idempotent with explicit `close_pane` (whichever runs first wins;
  the other is a no-op). Agents keep their mark-Exited-and-stay policy (unchanged).

### 3. Robustness hardening — `muxy-daemon`

- **Kill the poison cascade:** swap the daemon's `std::sync::Mutex` → **`parking_lot::Mutex`**
  (its `lock()` returns the guard directly, no poisoning) — `.lock().unwrap()` becomes `.lock()`
  across `server.rs`/`pane.rs`. Behavior-identical except a panic can no longer permanently poison
  a map. Mechanical but repo-wide; all existing daemon tests must stay green.
- **Observability:** add `tracing` + a `tracing-subscriber` init in `main.rs`; replace the silent
  `let _ =` on per-connection task results with an error log (dropped connection / task error), and
  log daemon start/stop + single-instance refusal. Minimal, structured, off the hot path.

### 4. Client auto-reconnect — `MuxyCore`/`MuxyApp` (Swift)

- `AppModel`: on the transport's close callback, instead of a terminal `.closed`, enter a
  **reconnect loop with exponential backoff** (e.g. 0.5s → cap ~10s, cancellable), surfacing a
  "reconnecting…" state; on a successful reconnect, **re-hydrate** (`listAgents` + `listAdapters`,
  as `connect()` does). Cancel the loop on explicit quit/`disconnect()`. Resilient to a daemon
  restart (and pairs with M6's app-launches-daemon). Pure `MuxyCore` logic (unit-testable via a
  fake transport that fails then succeeds); the surfaces re-attach via their own `muxy attach`.

## Data flow

```
startup:  Config::load() (env›file›default) ─► flock <runtime>/muxy/daemon.pid
             held-live → refuse+exit ; free/stale → take lock, unlink stale socks, bind 3 socks
runtime:  companion PTY exits ─► watcher ─► reap_companion ─► remove leaf + SplitTreeChanged
          any locked-section panic ─► parking_lot: no poison, other locks fine ; task err ─► tracing
shutdown: SIGTERM/SIGINT ─► Daemon::shutdown(): kill child PTYs ─► rm socks+pid ─► exit
          (Drop for Pane kills child as backstop)
client:   transport close ─► AppModel reconnect(backoff) ─► on success: listAgents+listAdapters
```

## Decomposition (each its own plan → SDD → PR)

- **M5a — Config system.** `muxy-config` (serde+toml, `directories`), env›file›default resolver,
  wire into `main.rs`/`Daemon`/`Pane` (configurable `backlog_cap`, sockets, shell, pane size) +
  `muxy-client` socket resolution. Rust, unit-tested.
- **M5b — Daemon lifecycle.** Single-instance PID+flock guard, graceful shutdown (signal → kill
  children + cleanup, `Drop for Pane`), companion-crash reap (`reap_companion` + `SplitTreeChanged`).
  Rust, unit/integration-tested.
- **M5c — Hardening.** `std::sync::Mutex` → `parking_lot::Mutex` daemon-wide; `tracing` + error
  logging of dropped connections/tasks. Rust; all existing tests stay green.
- **M5d — Client auto-reconnect.** Swift `AppModel` reconnect-with-backoff + re-hydrate on a
  dropped connection. MuxyCore unit-tested; UI build + manual.

(M5a → M5b/M5c can proceed in parallel-ish but land sequentially; M5c's parking_lot swap should
land before/with M5b's new watchers to avoid churn. Order at planning: M5a, M5c, M5b, M5d.)

## Testing

- **M5a (`cargo test`):** a `config.toml` sets `backlog_cap`/sockets/shell/pane-size and `Config::load`
  reflects it; **env overrides the file** (set both → env wins); a missing/invalid file → defaults
  (non-fatal); `Pane` uses the configured cap (small cap → backlog drains sooner).
- **M5b (`cargo test`):** a second `Daemon` acquiring the same PID lock fails while the first holds
  it; a `shutdown()` kills a spawned child (assert the process is gone) and removes the socket/PID;
  a companion pane whose process exits is removed from its agent's tree and a `SplitTreeChanged` is
  broadcast (reuse the split-tree test harness); explicit `close_pane` after a reap is a no-op.
- **M5c (`cargo test`):** whole daemon suite green after the `parking_lot` swap (no behavior change);
  a task-level error is logged (capture) rather than silently dropped.
- **M5d (`swift test` + build):** a transport that fails then succeeds drives `AppModel` from
  reconnecting → live with a re-hydration (`listAgents`/`listAdapters`) sent; backoff is bounded;
  explicit quit cancels the loop.
- **Manual (user):** `kill` the daemon while the app is attached → the app shows "reconnecting…" and
  recovers when the daemon is back; start a 2nd daemon → it refuses; ⌘-split a companion then `exit`
  it → the pane disappears from the split.

## Risks

1. **Config precedence must not break env/dev.** Env always wins over the file; a missing/invalid
   file is non-fatal. Covered by the env-overrides-file test.
2. **`flock` portability.** Advisory `flock` differs across platforms; scope to macOS/Linux Unix
   locks; a stale lock from a crashed daemon must be reclaimable (lock is released on process death).
3. **Graceful child-kill vs the reaper.** Shutdown must kill children without racing the exit
   watchers into spurious events; stop accepting + abort watchers before/inside `shutdown()`.
4. **`parking_lot` swap breadth.** Mechanical but repo-wide; land as one reviewable PR with the full
   daemon suite green (behavior identical minus poisoning).
5. **Reconnect storms.** Bounded exponential backoff + cancel-on-quit so a permanently-down daemon
   doesn't spin the client.

## Verification gate

Per slice: `cargo test` / `swift test` green for that slice + all existing. End state: the daemon
reads `~/.config/muxy/config.toml` (env still wins), refuses a second instance, shuts down cleanly
killing its children, auto-removes crashed companion panes, no longer cascades on a poisoned lock,
logs errors, and the macOS app transparently reconnects across a daemon restart. Deferred (own
efforts): the VT grid / scrollback reflow and agent-survival-across-restart. Manual confirmation by
the user (kill/restart daemon → app recovers; 2nd-daemon refusal; companion auto-remove).
