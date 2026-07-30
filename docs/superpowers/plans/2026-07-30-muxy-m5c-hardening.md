# muxy M5c — Hardening (parking_lot + tracing)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two hardening changes to the daemon: (1) swap `std::sync::Mutex` → `parking_lot::Mutex`
daemon-wide, killing the lock-poison cascade (a panic in a locked section can no longer permanently
poison a map so every later access panics); (2) add `tracing` + a subscriber, replacing the silent
`let _ =` on per-connection task results with an error log and logging daemon start / single-instance
refusal.

**Architecture:** Behavior-identical, daemon-only. `parking_lot::Mutex::lock()` returns the guard
directly — no `Result`, no poisoning — so `.lock().unwrap()` becomes `.lock()` and `pane.rs`'s three
`.lock().map_err(|_| anyhow!("… poisoned"))?` sites lose their poison arm. A new `logging` module
holds a testable `conn_error_line` helper (error → message, ok → None) used at the accept-loop spawn
sites, plus a thin `init()` that installs the `tracing_subscriber` fmt layer (RUST_LOG-aware, default
`info`). `main.rs` calls `logging::init()` first and logs start/refusal via `tracing`.

**Tech Stack:** Rust; `muxy-daemon` only. `parking_lot` (0.12), `tracing` (0.1),
`tracing-subscriber` (0.3, `env-filter`). Spec:
`docs/superpowers/specs/2026-07-30-muxy-m5-robustness-design.md` (§3 Robustness hardening).

## Global Constraints

- **Behavior-identical swap.** `parking_lot::Mutex::lock()` returns the `MutexGuard` directly (no
  `Result`, no poisoning). Every `.lock().unwrap()` becomes `.lock()`. **All existing daemon tests
  must stay green** — this is the gate for Task 1 (it is a refactor; the unchanged suite is its test).
- **Scope: `muxy-daemon` only** — `server.rs`, `pane.rs`, `notify.rs`, `main.rs`, plus a new
  `logging.rs`. No proto / `muxy-client` / Swift changes; do **not** add `parking_lot` to any other
  crate. No M5d (client reconnect) work.
- **`parking_lot` guards are `!Send`** — holding one across an `.await` will not compile. The daemon
  already never does this (verified in M5b review), so the swap compiles unchanged. If a genuine
  hold-across-await surfaces, that is a real bug to fix by narrowing the lock scope, not to work around.
- **No `debug_assert!`s remain** in the daemon (M5b's tolerance fixes removed them) — there is nothing
  to clean up on the tree-mutation paths, and **do not add any**.
- **Logging is minimal and off the hot path.** Convert ONLY: the daemon start banner + single-instance
  refusal (currently `eprintln!` in `main.rs`) → `tracing`; and the per-connection / per-server
  accept-loop task results (the 3 inner `handle_*_conn` spawns + the 2 `main.rs` server spawns).
  **Do NOT** convert the intentional best-effort `let _ =` sites (`p.kill()`, `*_tx.send()`,
  `pane.write_input`/`resize`, `split_leaf`/`remove_leaf`) — those are deliberate. **Do NOT** touch
  any `let _ = … .await` inside `#[cfg(test)]` modules.
- New deps (add to `crates/muxy-daemon/Cargo.toml`): `parking_lot = "0.12"`, `tracing = "0.1"`,
  `tracing-subscriber = { version = "0.3", features = ["env-filter"] }`. `anyhow::Result`.
  Build/test: `source "$HOME/.cargo/env" && cargo test` (whole workspace must stay green;
  `cargo test -p muxy-daemon` for the daemon crate).

---

## Task 1: Swap `std::sync::Mutex` → `parking_lot::Mutex` daemon-wide

**Files:**
- Modify: `crates/muxy-daemon/Cargo.toml` (add `parking_lot`)
- Modify: `crates/muxy-daemon/src/server.rs` (import + 50 `.lock().unwrap()` sites)
- Modify: `crates/muxy-daemon/src/pane.rs` (import + 5 `.lock().unwrap()` sites + 3 `map_err` poison
  sites + the `Drop` `if let Ok` + the now-unused `anyhow!` import)
- Modify: `crates/muxy-daemon/src/notify.rs` (import + 2 `.lock().unwrap()` sites)

**Interfaces:**
- Consumes: nothing new.
- Produces: no public API change. `Daemon`/`Pane`/`FakeNotifier` fields switch from `std::sync::Mutex`
  to `parking_lot::Mutex`; all lock acquisitions return guards directly. Behavior identical.

- [ ] **Step 1: Add the `parking_lot` dependency.** In `crates/muxy-daemon/Cargo.toml`, under
`[dependencies]`, add:

```toml
parking_lot = "0.12"
```

- [ ] **Step 2: Swap the imports.** Make these exact edits:

`crates/muxy-daemon/src/server.rs` line 13 — change:
```rust
use std::sync::{Arc, Mutex};
```
to:
```rust
use parking_lot::Mutex;
use std::sync::Arc;
```

`crates/muxy-daemon/src/pane.rs` line 5 — change:
```rust
use std::sync::{Arc, Mutex};
```
to:
```rust
use parking_lot::Mutex;
use std::sync::Arc;
```

`crates/muxy-daemon/src/notify.rs` line 2 — change:
```rust
use std::sync::Mutex;
```
to:
```rust
use parking_lot::Mutex;
```

- [ ] **Step 3: Mechanically drop `.unwrap()` from every lock call** in the three files. Run this
sweep (macOS `sed`):

```bash
cd /Users/richard/code/muxy
sed -i '' 's/\.lock()\.unwrap()/.lock()/g' \
  crates/muxy-daemon/src/server.rs \
  crates/muxy-daemon/src/pane.rs \
  crates/muxy-daemon/src/notify.rs
```

Then confirm none remain in those files:

```bash
grep -rn "\.lock()\.unwrap()" crates/muxy-daemon/src/server.rs crates/muxy-daemon/src/pane.rs crates/muxy-daemon/src/notify.rs
```
Expected: no matches.

- [ ] **Step 4: Fix `pane.rs`'s three poison-arm sites** (parking_lot `lock()` returns the guard, so
`.map_err(...)?` no longer applies). Make these exact edits in `crates/muxy-daemon/src/pane.rs`:

`write_input`:
```rust
        let mut w = self.writer.lock().map_err(|_| anyhow!("writer poisoned"))?;
```
→
```rust
        let mut w = self.writer.lock();
```

`resize`:
```rust
        let m = self.master.lock().map_err(|_| anyhow!("master poisoned"))?;
```
→
```rust
        let m = self.master.lock();
```

`kill`:
```rust
        self.killer.lock().map_err(|_| anyhow!("killer poisoned"))?.kill()?;
```
→
```rust
        self.killer.lock().kill()?;
```

- [ ] **Step 5: Fix `Drop for Pane`** (parking_lot `lock()` is not a `Result`, so `if let Ok(...)`
won't compile). In `crates/muxy-daemon/src/pane.rs`, change:

```rust
    fn drop(&mut self) {
        if let Ok(mut k) = self.killer.lock() {
            let _ = k.kill();
        }
    }
```
to:
```rust
    fn drop(&mut self) {
        let _ = self.killer.lock().kill();
    }
```

- [ ] **Step 6: Drop the now-unused `anyhow!` import in `pane.rs`.** After Step 4, `anyhow!` is no
longer used. In `crates/muxy-daemon/src/pane.rs` line 1, change:

```rust
use anyhow::{anyhow, Result};
```
to:
```rust
use anyhow::Result;
```

(If `cargo build` in Step 7 reports `anyhow!` still used somewhere, revert this one line — but a grep
of `pane.rs` for `anyhow!` should show zero uses after Step 4.)

- [ ] **Step 7: Build and confirm the swap compiles cleanly.**

Run: `source "$HOME/.cargo/env" && cargo build -p muxy-daemon 2>&1 | tail -20`
Expected: builds with no errors. In particular, no "cannot borrow", no "held across await", no
"method `unwrap` not found" — a clean compile is the proof the swap is complete and no guard is held
across an `.await`. Fix any leftover `.lock().unwrap()` the sweep missed (e.g. multi-line) or unused
imports it surfaced.

- [ ] **Step 8: Run the whole daemon suite — this is the task's gate (no behavior change).**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon 2>&1 | tail -30`
Expected: every existing daemon test PASSES, same counts as before the swap (concurrency tests
`companion_crash_removes_leaf_and_broadcasts_tree`, `shutdown_kills_children_and_clears_panes`,
`teardown_kills_multiple_live_companions`, the reap/split/attention tests, and the `instance` lock
tests all green). No test should need editing — if one does, the swap changed behavior and is wrong.

- [ ] **Step 9: Run the whole workspace suite (no cross-crate breakage).**

Run: `source "$HOME/.cargo/env" && cargo test 2>&1 | grep -E 'test result|error' | tail -30`
Expected: all suites `0 failed`.

- [ ] **Step 10: Commit**

```bash
git add crates/muxy-daemon/Cargo.toml Cargo.lock \
  crates/muxy-daemon/src/server.rs crates/muxy-daemon/src/pane.rs crates/muxy-daemon/src/notify.rs
git commit -m "refactor(daemon): swap std::sync::Mutex -> parking_lot::Mutex (no poison cascade)"
```

---

## Task 2: `tracing` + error logging of connection tasks

**Files:**
- Create: `crates/muxy-daemon/src/logging.rs`
- Modify: `crates/muxy-daemon/src/lib.rs` (add `pub mod logging;`)
- Modify: `crates/muxy-daemon/Cargo.toml` (add `tracing`, `tracing-subscriber`)
- Modify: `crates/muxy-daemon/src/main.rs` (`logging::init()`; `eprintln!` → `tracing`; log the 2 server
  spawns)
- Modify: `crates/muxy-daemon/src/server.rs` (log the `handle_conn` accept-loop spawn — production only)
- Modify: `crates/muxy-daemon/src/control_json.rs` (log the `handle_control_json` accept-loop spawn)
- Modify: `crates/muxy-daemon/src/attention.rs` (log the `handle_hook_conn` accept-loop spawn)
- Test: `crates/muxy-daemon/src/logging.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `muxy_daemon::logging::conn_error_line(kind: &str, result: anyhow::Result<()>) -> Option<String>`
    — `Some("<kind> connection task ended with error: <e>")` on `Err`, `None` on `Ok`.
  - `muxy_daemon::logging::init()` — installs the global `tracing_subscriber` fmt subscriber
    (RUST_LOG-aware, default `info`). Call once at startup.

- [ ] **Step 1: Add the `tracing` dependencies.** In `crates/muxy-daemon/Cargo.toml`, under
`[dependencies]`, add:

```toml
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

- [ ] **Step 2: Register the module.** In `crates/muxy-daemon/src/lib.rs`, add alongside the other
`pub mod` lines:

```rust
pub mod logging;
```

- [ ] **Step 3: Write the failing test** — create `crates/muxy-daemon/src/logging.rs` with just the
helper's signature stub + the tests (impl filled in Step 4):

```rust
//! Structured logging setup + a testable helper for connection-task error reporting.

use anyhow::Result;

/// The warning line for a finished connection task, or `None` if it ended cleanly.
pub fn conn_error_line(kind: &str, result: Result<()>) -> Option<String> {
    let _ = (kind, result);
    unimplemented!()
}

/// Install the global tracing subscriber (RUST_LOG-aware, default `info`). Call once at startup.
pub fn init() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn ok_result_produces_no_line() {
        assert_eq!(conn_error_line("client", Ok(())), None);
    }

    #[test]
    fn err_result_produces_a_line_naming_kind_and_error() {
        let line = conn_error_line("control", Err(anyhow!("boom"))).expect("Err must produce a line");
        assert!(line.contains("control"), "line should name the connection kind: {line}");
        assert!(line.contains("boom"), "line should include the error: {line}");
    }
}
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon --lib logging`
Expected: FAIL — `conn_error_line` is `unimplemented!()` (panics).

- [ ] **Step 5: Implement `conn_error_line`.** In `logging.rs`, replace the stub body:

```rust
pub fn conn_error_line(kind: &str, result: Result<()>) -> Option<String> {
    let _ = (kind, result);
    unimplemented!()
}
```
with:
```rust
pub fn conn_error_line(kind: &str, result: Result<()>) -> Option<String> {
    result
        .err()
        .map(|e| format!("{kind} connection task ended with error: {e}"))
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon --lib logging`
Expected: PASS for both `ok_result_produces_no_line` and `err_result_produces_a_line_naming_kind_and_error`.

- [ ] **Step 7: Wire the three inner accept-loop spawns to log on error** (production only — leave all
`#[cfg(test)]` spawns untouched).

`crates/muxy-daemon/src/server.rs` — in `serve` (the client accept loop), change:
```rust
            tokio::spawn(async move {
                let _ = me.handle_conn(stream).await;
            });
```
to:
```rust
            tokio::spawn(async move {
                if let Some(line) = crate::logging::conn_error_line("client", me.handle_conn(stream).await) {
                    tracing::warn!("{line}");
                }
            });
```

`crates/muxy-daemon/src/control_json.rs` — in `serve_control_json`, change:
```rust
            tokio::spawn(async move {
                let _ = me.handle_control_json(stream).await;
            });
```
to:
```rust
            tokio::spawn(async move {
                if let Some(line) = crate::logging::conn_error_line("control", me.handle_control_json(stream).await) {
                    tracing::warn!("{line}");
                }
            });
```

`crates/muxy-daemon/src/attention.rs` — in `serve_hooks`, change:
```rust
            tokio::spawn(async move {
                let _ = me.handle_hook_conn(stream).await;
            });
```
to:
```rust
            tokio::spawn(async move {
                if let Some(line) = crate::logging::conn_error_line("hook", me.handle_hook_conn(stream).await) {
                    tracing::warn!("{line}");
                }
            });
```

- [ ] **Step 8: Wire `main.rs`** — init tracing first, log the two server spawns, and convert the
start banner + single-instance refusal to `tracing`. In `crates/muxy-daemon/src/main.rs`:

At the very start of `main`, before `Config::load()`, add:
```rust
    muxy_daemon::logging::init();
```

Change the single-instance refusal:
```rust
        Err(e) => {
            eprintln!("muxy-daemon: {e}");
            std::process::exit(1);
        }
```
to:
```rust
        Err(e) => {
            tracing::error!("{e}");
            std::process::exit(1);
        }
```

Change the start banner:
```rust
    eprintln!(
        "muxy-daemon: client={} hook={} control={} pid_lock={}",
        sock_path.display(),
        hook_path.display(),
        control_path.display(),
        lock.path().display()
    );
```
to:
```rust
    tracing::info!(
        client = %sock_path.display(),
        hook = %hook_path.display(),
        control = %control_path.display(),
        pid_lock = %lock.path().display(),
        "muxy-daemon listening"
    );
```

Change the two server spawns:
```rust
    let hooks = daemon.clone();
    tokio::spawn(async move { let _ = hooks.serve_hooks(hook_listener).await; });

    let control = daemon.clone();
    tokio::spawn(async move { let _ = control.serve_control_json(control_listener).await; });
```
to:
```rust
    let hooks = daemon.clone();
    tokio::spawn(async move {
        if let Some(line) = muxy_daemon::logging::conn_error_line("hook server", hooks.serve_hooks(hook_listener).await) {
            tracing::error!("{line}");
        }
    });

    let control = daemon.clone();
    tokio::spawn(async move {
        if let Some(line) = muxy_daemon::logging::conn_error_line("control server", control.serve_control_json(control_listener).await) {
            tracing::error!("{line}");
        }
    });
```

And convert the shutdown-signal notice — change:
```rust
        _ = shutdown_signal() => {
            eprintln!("muxy-daemon: received shutdown signal, stopping");
            Ok(())
        }
```
to:
```rust
        _ = shutdown_signal() => {
            tracing::info!("received shutdown signal, stopping");
            Ok(())
        }
```

- [ ] **Step 9: Build and run the whole workspace suite**

Run: `source "$HOME/.cargo/env" && cargo build -p muxy-daemon && cargo test 2>&1 | grep -E 'test result|error' | tail -30`
Expected: builds clean; all suites `0 failed` (the two new `logging` tests plus the full daemon +
workspace suites). No `unused import`/`dead_code` warnings from the new module.

- [ ] **Step 10: Manual smoke — confirm logs appear** (record the outcome):

```bash
source "$HOME/.cargo/env"
cargo run -p muxy-daemon &        # expect an INFO "muxy-daemon listening client=… pid_lock=…" line
sleep 1
cargo run -p muxy-daemon          # expect an ERROR line "another muxy-daemon is already running…" + exit 1
kill -TERM %1                     # expect an INFO "received shutdown signal, stopping"
```
Expected: a structured INFO listening line on start, an ERROR line + exit 1 on the second instance,
and an INFO shutdown line on SIGTERM. (If backgrounding a daemon isn't feasible in your environment,
say so and defer this manual smoke to the controller/user — the automated build+suite is the hard gate.)

- [ ] **Step 11: Commit**

```bash
git add crates/muxy-daemon/Cargo.toml Cargo.lock crates/muxy-daemon/src/logging.rs \
  crates/muxy-daemon/src/lib.rs crates/muxy-daemon/src/main.rs \
  crates/muxy-daemon/src/server.rs crates/muxy-daemon/src/control_json.rs crates/muxy-daemon/src/attention.rs
git commit -m "feat(daemon): tracing subscriber + error logging of connection tasks"
```

---

## Self-Review Notes (author)

- **Spec §3 coverage:** parking_lot swap (kill the poison cascade) → Task 1; `tracing` + subscriber
  init + error logging of dropped connections/tasks + start/refusal logging → Task 2. Spec §3 testing
  bullets map: "whole daemon suite green after the swap (no behavior change)" → Task 1 Steps 8-9; "a
  task-level error is logged (capture) rather than silently dropped" → Task 2 `conn_error_line` unit
  tests (error → line, ok → None) + wiring at the 3+2 accept-loop sites.
- **M5b carry-forward already satisfied:** the "drop remaining `debug_assert!`s on split/set-ratio
  paths" item is moot — M5b's tolerance fixes (`let _ = split_leaf(...)` / `let _ = remove_leaf(...)`)
  already removed every daemon `debug_assert!`. Task 1 confirms none remain and adds none.
- **Deliberate `let _ =` sites preserved:** `p.kill()`, `*_tx.send()`, `pane.write_input`/`resize`,
  `split_leaf`/`remove_leaf`, and every `#[cfg(test)]` `handle_*` spawn stay as-is — only the 3 inner
  production accept-loop spawns + the 2 `main.rs` server spawns get error logging.
- **Type consistency:** `conn_error_line(kind, result) -> Option<String>` and `init()` are used
  identically across `main.rs`/`server.rs`/`control_json.rs`/`attention.rs`. The parking_lot swap
  changes no signatures; the only source-visible ripple is `pane.rs` losing its 3 "poisoned" error
  arms (the methods still return `Result` because the underlying I/O — write/flush/resize/kill — can
  still fail).
- **Deferred to M5d:** Swift client auto-reconnect (out of scope here).
