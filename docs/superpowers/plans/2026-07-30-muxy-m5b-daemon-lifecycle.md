# muxy M5b — Daemon Lifecycle

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Harden the daemon's process lifecycle: a **single-instance guard** (PID file + advisory
`flock`) so a second daemon can't hijack a running one, **graceful shutdown** (SIGTERM/SIGINT → kill
every child PTY + clean up sockets/PID) with a `Drop for Pane` backstop, and **companion-crash
reaping** (a crashed companion pane auto-removes its leaf from the split tree and broadcasts
`SplitTreeChanged`).

**Architecture:** Additive daemon changes. `Pane` gains a `Drop` that kills its child (backstop).
`Daemon` gains `shutdown()` (kills all child PTYs + aborts background watchers/scanners) and
companion-crash reaping (`reap_companion` + a per-companion exit watcher registered in `split_pane`,
mirroring the existing per-agent watcher). A new `instance` module owns the `flock`-based
single-instance lock. `main.rs` acquires the lock (refuses to start if held), then serves until a
signal, then calls `shutdown()` and removes the sockets + PID file. Agents keep their existing
mark-`Exited`-and-stay policy; shutdown does **not** land/discard workspaces (agent-survival across
restart is deferred).

**Tech Stack:** Rust; `muxy-daemon` only (no proto/client/Swift change). `rustix` (advisory
`flock`), `tokio::signal` (already available via tokio `full`). Spec:
`docs/superpowers/specs/2026-07-30-muxy-m5-robustness-design.md` (§2 Daemon lifecycle).

## Global Constraints

- **Platform:** Unix (macOS/Linux) only. The single-instance lock is an advisory `flock(2)` that the
  OS **releases automatically on process death** — so a crashed daemon's lock is reclaimable.
- **No behavior change** to existing daemon tests. `Daemon::new`/`new_with`/`new_from_config` keep
  their signatures. `shutdown()`, `reap_companion`, and the companion watcher are additive. The one
  signature change is `Daemon::split_pane(&self, …)` → `split_pane(self: &Arc<Self>, …)` (every caller
  — `control_json.rs` and the tests — already holds an `Arc<Daemon>`, so call sites are unchanged).
- **Companion-crash reap must be idempotent** with explicit `close_pane` and with `teardown_agent`:
  whichever runs first wins; the other path is a safe no-op (never a panic, never a double-broadcast,
  never a spurious `SplitTreeChanged` after the tree is gone).
- **Reap targets companions only.** `reap_companion` must never remove an agent's own leaf (`owner`
  maps an agent leaf → itself); agents continue to use the mark-`Exited`-and-stay watcher unchanged.
- **Lock/PID path:** `<runtime_dir>/muxy/daemon.pid`, where `runtime_dir` = `$XDG_RUNTIME_DIR`, else
  `$TMPDIR`, else `/tmp`. The PID file's contents are informational; the `flock`, not the contents, is
  authoritative.
- **Shutdown ordering:** on signal → stop accepting (drop the serve future) → abort watchers/scanners
  (so killed children don't race spurious `Exited`/reap events) → kill child PTYs → unlink sockets +
  PID file → exit. `shutdown()` does **not** finalize (land/discard) any workspace.
- New dep: `rustix = { version = "0.38", features = ["fs"] }` in `muxy-daemon`. `tokio` (`full`)
  already provides `tokio::signal::unix`.
- `anyhow::Result`. Build/test: `source "$HOME/.cargo/env" && cargo test` (whole workspace must stay
  green; the daemon crate is `cargo test -p muxy-daemon`).

---

## Task 1: `Drop for Pane` — child-kill backstop

**Files:**
- Modify: `crates/muxy-daemon/src/pane.rs` (add an `impl Drop for Pane`)
- Test: `crates/muxy-daemon/src/pane.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Pane { killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>> }` (existing).
- Produces: dropping a `Pane` sends the child a kill signal. No public API change.

- [ ] **Step 1: Write the failing test** — append to `pane.rs`'s `mod tests`:

```rust
    fn pid_alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn dropping_a_pane_kills_its_child() {
        // The child records its own PID, then execs `sleep` (keeping that PID). After we drop the
        // Pane, `Drop for Pane` must kill that PID.
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let script = format!("echo $$ > {}; exec sleep 30", pidfile.display());
        let pane = Pane::spawn(PaneId(9), sh(&script), 80, 24, 4096).unwrap();

        // Wait for the child to write its PID.
        let mut pid = String::new();
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(&pidfile) {
                if !s.trim().is_empty() {
                    pid = s.trim().to_string();
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!pid.is_empty(), "child never wrote its PID");
        assert!(pid_alive(&pid), "child should be alive before drop");

        drop(pane);

        let mut dead = false;
        for _ in 0..100 {
            if !pid_alive(&pid) {
                dead = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(dead, "child process should be killed when its Pane is dropped");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon dropping_a_pane_kills_its_child`
Expected: FAIL — the child survives the drop (`Drop for Pane` not yet implemented), so `dead` stays false.

- [ ] **Step 3: Implement `Drop for Pane`** — add after the `impl Pane { … }` block in `pane.rs`:

```rust
impl Drop for Pane {
    /// Backstop: a dropped pane must never leak its child process. `kill()` on an
    /// already-exited child is harmless.
    fn drop(&mut self) {
        if let Ok(mut k) = self.killer.lock() {
            let _ = k.kill();
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon dropping_a_pane_kills_its_child`
Expected: PASS.

- [ ] **Step 5: Run the whole daemon suite (no regressions)**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon`
Expected: all existing tests still PASS (Drop is additive; killing an already-torn-down child is a no-op).

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-daemon/src/pane.rs
git commit -m "feat(daemon): Drop for Pane kills the child PTY (leak backstop)"
```

---

## Task 2: `Daemon::shutdown()` — kill all children + `companion_watchers` field

**Files:**
- Modify: `crates/muxy-daemon/src/server.rs` (add `companion_watchers` field + init; add `shutdown()`)
- Test: `crates/muxy-daemon/src/server.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: existing `panes`, `watchers`, `scanners` maps; `Pane::kill()`.
- Produces:
  - `companion_watchers: Arc<Mutex<HashMap<PaneId, tokio::task::JoinHandle<()>>>>` field on `Daemon`
    (initialized empty in `new_with`; **populated in Task 3** — empty here, so `shutdown()` draining
    it is a harmless no-op until then).
  - `Daemon::shutdown(&self)` — aborts all background watchers/scanners/companion-watchers, kills every
    child PTY, clears the `panes` map. Does **not** finalize workspaces.

- [ ] **Step 1: Add the `companion_watchers` field.** In `server.rs`, in the `struct Daemon { … }`
declaration, add (next to `scanners`):

```rust
    companion_watchers: Arc<Mutex<HashMap<PaneId, tokio::task::JoinHandle<()>>>>,
```

And in `new_with`, in the `Daemon { … }` initializer (next to `scanners: …`), add:

```rust
            companion_watchers: Arc::new(Mutex::new(HashMap::new())),
```

- [ ] **Step 2: Write the failing test** — append to `server.rs`'s `mod tests`:

```rust
    #[tokio::test]
    async fn shutdown_kills_children_and_clears_panes() {
        fn pid_alive(pid: &str) -> bool {
            std::process::Command::new("kill")
                .args(["-0", pid])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }

        let daemon = Arc::new(Daemon::new());
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let script = format!("echo $$ > {}; exec sleep 30", pidfile.display());
        let pane = daemon.spawn_pane(sh(&script), 80, 24).unwrap();

        // Wait for the child to record its PID.
        let mut pid = String::new();
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(&pidfile) {
                if !s.trim().is_empty() {
                    pid = s.trim().to_string();
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!pid.is_empty(), "child never wrote its PID");
        assert!(pid_alive(&pid), "child alive before shutdown");

        daemon.shutdown();

        // The pane is removed and its child is killed.
        assert!(daemon.get(pane).is_none(), "shutdown must clear the panes map");
        let mut dead = false;
        for _ in 0..100 {
            if !pid_alive(&pid) {
                dead = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(dead, "shutdown must kill the child PTY process");
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon shutdown_kills_children_and_clears_panes`
Expected: FAIL — `shutdown` does not exist (compile error / method not found).

- [ ] **Step 4: Implement `shutdown()`** — add a method inside `impl Daemon { … }` in `server.rs`
(place it just after `teardown_agent`/`finish_agent` for readability):

```rust
    /// Graceful shutdown: abort all background watchers/scanners so killed children can't race
    /// spurious attention/reap events, then kill every child PTY and drop the pane map. Does NOT
    /// finalize (land/discard) any workspace — agents keep their worktrees.
    pub fn shutdown(&self) {
        for (_, h) in self.watchers.lock().unwrap().drain() {
            h.abort();
        }
        for (_, h) in self.scanners.lock().unwrap().drain() {
            h.abort();
        }
        for (_, h) in self.companion_watchers.lock().unwrap().drain() {
            h.abort();
        }
        let panes: Vec<Arc<Pane>> = self.panes.lock().unwrap().values().cloned().collect();
        for p in panes {
            let _ = p.kill();
        }
        self.panes.lock().unwrap().clear();
    }
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon shutdown_kills_children_and_clears_panes`
Expected: PASS.

- [ ] **Step 6: Run the whole daemon suite (no regressions)**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon`
Expected: all tests PASS. (`companion_watchers` is empty until Task 3, so draining it is a no-op.)

- [ ] **Step 7: Commit**

```bash
git add crates/muxy-daemon/src/server.rs
git commit -m "feat(daemon): Daemon::shutdown() kills child PTYs and aborts watchers"
```

---

## Task 3: Companion-crash reaping

**Files:**
- Modify: `crates/muxy-daemon/src/server.rs` (`split_pane` receiver + watcher registration;
  `reap_companion`; abort companion watchers in `close_pane` and `finish_agent`)
- Test: `crates/muxy-daemon/src/server.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `companion_watchers` (Task 2), `owner`/`trees`/`panes` maps, `crate::split_tree::remove_leaf`,
  `broadcast_tree`, `Pane::wait_exit()`/`kill()`.
- Produces:
  - `Daemon::split_pane(self: &Arc<Self>, target, direction) -> Result<PaneId>` (receiver changed from
    `&self` so it can spawn a watcher task holding an `Arc<Self>`; the returned type and behavior are
    otherwise identical). Registers a per-companion exit watcher after the owner/tree are established.
  - `Daemon::reap_companion(&self, pane: PaneId)` (`pub(crate)`) — idempotently removes a crashed
    companion: looks up its owner agent, removes the leaf (collapsing the tree), drops the pane +
    `owner` + `companion_watchers` entries, and broadcasts `SplitTreeChanged`. No-op if the pane is
    already gone (explicit close won the race) or is an agent's own leaf.
  - `close_pane` and `finish_agent` abort a companion's watcher before removing it, so an explicit
    close/teardown never leaves a watcher that later fires a spurious reap.

- [ ] **Step 1: Write the failing tests** — append to `server.rs`'s `mod tests`:

```rust
    #[tokio::test]
    async fn companion_crash_removes_leaf_and_broadcasts_tree() {
        use crate::split_tree;
        let (daemon, repo) = daemon_with_repo();
        let agent = daemon
            .spawn_agent(
                repo.path(),
                &crate::agent::SyntheticAdapter {
                    command: PaneCommand {
                        program: "/bin/sh".into(),
                        args: vec!["-c".into(), "sleep 30".into()],
                        cwd: None,
                        env: vec![],
                    },
                },
                "task",
            )
            .unwrap();

        let mut rx = daemon.subscribe_splits();
        let comp = daemon.split_pane(agent, SplitDirection::Right).unwrap();
        assert_eq!(split_tree::leaves(&daemon.split_tree_of(agent).unwrap()), vec![agent, comp]);
        let _ = rx.try_recv(); // drain the split broadcast

        // Simulate the companion process crashing.
        daemon.get(comp).unwrap().kill().unwrap();

        // The watcher must reap it: tree collapses back to the lone agent leaf, pane gone.
        let mut collapsed = false;
        for _ in 0..100 {
            if daemon.split_tree_of(agent) == Some(PaneTree::Leaf { pane: agent }) {
                collapsed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(collapsed, "a crashed companion's leaf must be removed from the tree");
        assert!(daemon.get(comp).is_none(), "the crashed companion pane must be dropped");

        // A SplitTreeChanged for this agent was broadcast by the reap.
        let mut saw = false;
        for _ in 0..40 {
            match rx.try_recv() {
                Ok((a, _)) if a == agent => {
                    saw = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        assert!(saw, "reap must broadcast SplitTreeChanged for the owning agent");

        daemon.teardown_agent(agent).unwrap();
    }

    #[tokio::test]
    async fn reap_companion_is_idempotent() {
        use crate::split_tree;
        let (daemon, repo) = daemon_with_repo();
        let agent = daemon
            .spawn_agent(
                repo.path(),
                &crate::agent::SyntheticAdapter {
                    command: PaneCommand {
                        program: "/bin/sh".into(),
                        args: vec!["-c".into(), "sleep 30".into()],
                        cwd: None,
                        env: vec![],
                    },
                },
                "task",
            )
            .unwrap();
        let comp = daemon.split_pane(agent, SplitDirection::Right).unwrap();

        // Explicit close removes the companion.
        daemon.close_pane(comp).unwrap();
        assert_eq!(split_tree::leaves(&daemon.split_tree_of(agent).unwrap()), vec![agent]);

        // A late reap (e.g. the watcher firing after close) must be a safe no-op: the tree stays a
        // lone agent leaf and nothing panics.
        daemon.reap_companion(comp);
        assert_eq!(daemon.split_tree_of(agent), Some(PaneTree::Leaf { pane: agent }));

        // Reaping the agent's own leaf must never remove it.
        daemon.reap_companion(agent);
        assert!(daemon.split_tree_of(agent).is_some(), "reap must never remove an agent's own leaf");

        daemon.teardown_agent(agent).unwrap();
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon reap_companion` (also runs
`companion_crash_removes_leaf_and_broadcasts_tree`)
Expected: FAIL — `reap_companion` does not exist (compile error); the crash test would also fail
because no watcher removes the leaf.

- [ ] **Step 3: Change `split_pane`'s receiver and register the companion watcher.** In `server.rs`,
change the signature:

```rust
    pub fn split_pane(self: &Arc<Self>, target: PaneId, direction: SplitDirection) -> Result<PaneId> {
```

Then, at the **end** of `split_pane` (replace the current tail
`self.owner.lock().unwrap().insert(companion, agent); self.broadcast_tree(agent); Ok(companion)`
with the version below), register the exit watcher **after** the owner + tree are established (so a
fast crash can't race an unset owner):

```rust
        self.owner.lock().unwrap().insert(companion, agent);
        self.broadcast_tree(agent);

        // Reap the companion if its process exits/crashes (mirrors the per-agent watcher). Registered
        // after owner+tree are set so `reap_companion` always finds the owner. `wait_exit()` returns
        // immediately if the child already exited, so no exit is missed even if we register late.
        if let Some(pane_arc) = self.get(companion) {
            let me = Arc::clone(self);
            let handle = tokio::spawn(async move {
                pane_arc.wait_exit().await;
                me.reap_companion(companion);
            });
            self.companion_watchers.lock().unwrap().insert(companion, handle);
        }

        Ok(companion)
```

- [ ] **Step 4: Implement `reap_companion`.** Add inside `impl Daemon { … }` (place it just after
`close_pane`):

```rust
    /// Remove a companion pane whose process exited/crashed: collapse its leaf out of the owning
    /// agent's tree and broadcast the change. Idempotent — a no-op if the pane is already gone
    /// (explicit `close_pane`/`teardown` won the race) or if `pane` is an agent's own leaf.
    pub(crate) fn reap_companion(&self, pane: PaneId) {
        let agent = match self.owner.lock().unwrap().get(&pane).copied() {
            Some(a) => a,
            None => return, // already removed
        };
        if agent == pane {
            return; // an agent's own leaf is never reaped as a companion
        }
        if let Some(p) = self.get(pane) {
            let _ = p.kill();
        }
        self.panes.lock().unwrap().remove(&pane);
        if let Some(tree) = self.trees.lock().unwrap().get_mut(&agent) {
            let _ = crate::split_tree::remove_leaf(tree, pane);
        }
        self.owner.lock().unwrap().remove(&pane);
        self.companion_watchers.lock().unwrap().remove(&pane);
        self.broadcast_tree(agent);
    }
```

- [ ] **Step 5: Abort the companion watcher on explicit close/teardown.** In `close_pane`, after the
`self.panes.lock().unwrap().remove(&pane);` line (for the companion branch), add:

```rust
        if let Some(h) = self.companion_watchers.lock().unwrap().remove(&pane) {
            h.abort();
        }
```

And in `finish_agent`, inside the companion cascade loop (the `for c in &companions { … }` block,
after `self.owner.lock().unwrap().remove(c);`), add:

```rust
            if let Some(h) = self.companion_watchers.lock().unwrap().remove(c) {
                h.abort();
            }
```

- [ ] **Step 6: Run the new tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon reap_companion companion_crash`
Expected: PASS for both `companion_crash_removes_leaf_and_broadcasts_tree` and
`reap_companion_is_idempotent`.

- [ ] **Step 7: Run the whole daemon suite (no regressions)**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon`
Expected: all tests PASS — in particular the existing `split_close_and_teardown_manage_the_tree`,
`teardown_kills_multiple_live_companions`, and `set_ratio_updates_and_broadcasts` (companions now have
watchers; close/teardown abort them, so no spurious late reap).

- [ ] **Step 8: Commit**

```bash
git add crates/muxy-daemon/src/server.rs
git commit -m "feat(daemon): reap crashed companion panes (remove leaf + SplitTreeChanged)"
```

---

## Task 4: Single-instance guard — `instance` module (`flock` + PID file)

**Files:**
- Create: `crates/muxy-daemon/src/instance.rs`
- Modify: `crates/muxy-daemon/src/lib.rs` (add `pub mod instance;`)
- Modify: `crates/muxy-daemon/Cargo.toml` (add the `rustix` dependency)
- Test: `crates/muxy-daemon/src/instance.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `muxy_daemon::instance::InstanceLock` — holds an exclusive advisory `flock` on the PID file for the
    daemon's lifetime (released when dropped / on process death).
  - `InstanceLock::default_path() -> PathBuf` — `<runtime_dir>/muxy/daemon.pid`
    (`$XDG_RUNTIME_DIR` › `$TMPDIR` › `/tmp`).
  - `InstanceLock::acquire(path: &Path) -> anyhow::Result<InstanceLock>` — creates the parent dir,
    opens/creates the file, takes the exclusive lock (Err if another live daemon holds it), and writes
    the current PID.
  - `InstanceLock::path(&self) -> &Path`.

- [ ] **Step 1: Add the `rustix` dependency.** In `crates/muxy-daemon/Cargo.toml`, under
`[dependencies]`, add:

```toml
rustix = { version = "0.38", features = ["fs"] }
```

- [ ] **Step 2: Register the module.** In `crates/muxy-daemon/src/lib.rs`, add alongside the other
`pub mod` lines:

```rust
pub mod instance;
```

- [ ] **Step 3: Write the failing test** — create `crates/muxy-daemon/src/instance.rs` with just the
test module first (the impl comes next):

```rust
//! Single-instance guard: an advisory `flock` on a PID file. The OS releases the lock on process
//! death, so a crashed daemon's lock is reclaimable by the next start.

use anyhow::{bail, Result};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

// (impl added in Step 4)

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_while_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("muxy").join("daemon.pid");

        let lock1 = InstanceLock::acquire(&path).expect("first acquire succeeds");
        assert!(
            InstanceLock::acquire(&path).is_err(),
            "a second acquire must fail while the first lock is held"
        );

        // Releasing the first lock lets a later acquire succeed (stale lock is reclaimable).
        drop(lock1);
        let _lock2 = InstanceLock::acquire(&path).expect("acquire after release succeeds");
    }

    #[test]
    fn default_path_prefers_xdg_runtime_dir() {
        // default_path() just joins muxy/daemon.pid under the chosen base; assert the suffix.
        let p = InstanceLock::default_path();
        assert!(
            p.ends_with("muxy/daemon.pid"),
            "default path should end with muxy/daemon.pid, got {}",
            p.display()
        );
    }
}
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon --lib instance`
Expected: FAIL — `InstanceLock` is undefined (compile error).

- [ ] **Step 5: Implement `InstanceLock`.** In `instance.rs`, replace the `// (impl added in Step 4)`
placeholder with:

```rust
/// An acquired single-instance lock. Holds the locked file open for the process's lifetime; the
/// advisory `flock` is released when this value drops (or when the process dies).
pub struct InstanceLock {
    _file: std::fs::File,
    path: PathBuf,
}

impl InstanceLock {
    /// `<runtime_dir>/muxy/daemon.pid`, where runtime_dir is `$XDG_RUNTIME_DIR`, else `$TMPDIR`,
    /// else `/tmp`.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .or_else(|| std::env::var_os("TMPDIR"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        base.join("muxy").join("daemon.pid")
    }

    /// Try to take the exclusive advisory lock. Returns Err if another live daemon holds it.
    pub fn acquire(path: &Path) -> Result<InstanceLock> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).read(true).write(true).open(path)?;
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(e) if e == rustix::io::Errno::WOULDBLOCK => {
                bail!("another muxy-daemon is already running (lock held at {})", path.display());
            }
            Err(e) => bail!("failed to lock {}: {e}", path.display()),
        }
        // Record our PID (informational; the lock is authoritative).
        use std::io::{Seek, SeekFrom, Write};
        let mut f = &file;
        f.set_len(0)?;
        f.seek(SeekFrom::Start(0))?;
        let _ = writeln!(f, "{}", std::process::id());
        Ok(InstanceLock { _file: file, path: path.to_path_buf() })
    }

    /// The PID-file path this lock holds.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon --lib instance`
Expected: PASS for `second_acquire_fails_while_first_is_held` and `default_path_prefers_xdg_runtime_dir`.

- [ ] **Step 7: Run the whole daemon suite (no regressions)**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon`
Expected: all tests PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/muxy-daemon/src/instance.rs crates/muxy-daemon/src/lib.rs crates/muxy-daemon/Cargo.toml Cargo.lock
git commit -m "feat(daemon): single-instance guard via flock on a PID file"
```

---

## Task 5: Wire the lifecycle into `main.rs` — acquire lock, serve-until-signal, cleanup

**Files:**
- Modify: `crates/muxy-daemon/src/main.rs` (acquire `InstanceLock`; `select!` serve vs. signal;
  `shutdown()`; remove sockets + PID file)
- Modify: `crates/muxy-daemon/src/instance.rs` (add `remove_files` helper + test)

**Interfaces:**
- Consumes: `InstanceLock::{default_path, acquire, path}` (Task 4), `Daemon::shutdown()` (Task 2),
  `Daemon::serve`, `tokio::signal::unix`.
- Produces: `muxy_daemon::instance::remove_files(paths: &[&Path])` — best-effort unlink of each path
  (ignores missing). Used by `main.rs` for socket + PID-file cleanup on exit.

- [ ] **Step 1: Write the failing test** — append to `instance.rs`'s `mod tests`:

```rust
    #[test]
    fn remove_files_unlinks_existing_and_ignores_missing() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("a.sock");
        let missing = dir.path().join("b.sock");
        std::fs::write(&present, b"x").unwrap();
        assert!(present.exists());

        remove_files(&[present.as_path(), missing.as_path()]);

        assert!(!present.exists(), "existing file should be removed");
        assert!(!missing.exists(), "missing file stays missing (no panic)");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon --lib remove_files_unlinks`
Expected: FAIL — `remove_files` is undefined (compile error).

- [ ] **Step 3: Implement `remove_files`.** In `instance.rs`, add a free function (outside the
`impl`/test blocks):

```rust
/// Best-effort unlink of each path; a missing file is not an error.
pub fn remove_files(paths: &[&Path]) {
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-daemon --lib remove_files_unlinks`
Expected: PASS.

- [ ] **Step 5: Rewrite `main.rs` to acquire the lock, serve until a signal, and clean up.** Replace
the entire body of `crates/muxy-daemon/src/main.rs` with:

```rust
use anyhow::Result;
use muxy_daemon::instance::{remove_files, InstanceLock};
use muxy_daemon::server::Daemon;
use std::sync::Arc;
use tokio::net::UnixListener;

#[tokio::main]
async fn main() -> Result<()> {
    let config = muxy_config::Config::load();
    let sock_path = config.client_sock.clone();
    let control_path = config.control_sock.clone();
    let daemon = Arc::new(Daemon::new_from_config(config));
    let hook_path = daemon.hook_sock().to_path_buf();

    // Single-instance guard: refuse to start if another daemon already holds the lock.
    let lock_path = InstanceLock::default_path();
    let lock = match InstanceLock::acquire(&lock_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("muxy-daemon: {e}");
            std::process::exit(1);
        }
    };

    // We own the instance: clear any stale sockets, then bind.
    remove_files(&[&sock_path, &hook_path, &control_path]);
    let client_listener = UnixListener::bind(&sock_path)?;
    let hook_listener = UnixListener::bind(&hook_path)?;
    let control_listener = UnixListener::bind(&control_path)?;
    eprintln!(
        "muxy-daemon: client={} hook={} control={} pid_lock={}",
        sock_path.display(),
        hook_path.display(),
        control_path.display(),
        lock.path().display()
    );

    let hooks = daemon.clone();
    tokio::spawn(async move { let _ = hooks.serve_hooks(hook_listener).await; });

    let control = daemon.clone();
    tokio::spawn(async move { let _ = control.serve_control_json(control_listener).await; });

    // Serve until a shutdown signal arrives, then kill children and clean up.
    let serving = daemon.clone();
    let result = tokio::select! {
        r = serving.serve(client_listener) => r,
        _ = shutdown_signal() => {
            eprintln!("muxy-daemon: received shutdown signal, stopping");
            Ok(())
        }
    };

    daemon.shutdown();
    remove_files(&[&sock_path, &hook_path, &control_path, &lock_path]);
    drop(lock); // release the advisory flock
    result
}

/// Resolve when the daemon receives SIGTERM or SIGINT.
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}
```

- [ ] **Step 6: Build + run the whole workspace suite**

Run: `source "$HOME/.cargo/env" && cargo build -p muxy-daemon && cargo test`
Expected: builds clean; the whole workspace test suite PASSES (main.rs has no unit tests; its pieces
are covered by Tasks 2 and 4, and the `remove_files` test).

- [ ] **Step 7: Manual verification** (record the outcome in the fix/completion notes):

```bash
source "$HOME/.cargo/env"
# 1. Single-instance refusal:
cargo run -p muxy-daemon &      # first daemon takes the lock
sleep 1
cargo run -p muxy-daemon        # second must print "another muxy-daemon is already running" and exit 1
echo "second exit code: $?"     # expect 1
# 2. Graceful shutdown kills children + removes sockets/PID:
kill -TERM %1                   # SIGTERM the first daemon
sleep 1
ls /tmp/muxy.sock /tmp/muxy-control.sock 2>&1   # expect "No such file" (cleaned up)
```

Expected: the second daemon refuses with a clear message and exit code 1; after SIGTERM the first
daemon exits, its child PTYs are gone, and the sockets + PID file are removed. (Companion auto-remove
is exercised by Task 3's tests; the end-to-end GUI check — ⌘-split a companion, `exit` it, watch the
pane disappear — is part of the milestone's user smoke test.)

- [ ] **Step 8: Commit**

```bash
git add crates/muxy-daemon/src/main.rs crates/muxy-daemon/src/instance.rs
git commit -m "feat(daemon): graceful shutdown + single-instance guard wired into main"
```

---

## Self-Review Notes (author)

- **Spec §2 coverage:** single-instance PID+flock guard → Tasks 4+5; graceful shutdown (signal → kill
  children + cleanup) → Tasks 2+5; `Drop for Pane` backstop → Task 1; companion-crash reap
  (`reap_companion` + `SplitTreeChanged`) → Task 3. Spec §2 testing bullets all map to a task test
  (2nd-lock-fails → T4; shutdown-kills-child + removes socket/pid → T2 + T5 manual; companion-exit
  removes leaf + broadcasts → T3; explicit-close-after-reap no-op → T3 idempotency test).
- **Deferred (per spec, NOT in M5b):** the `parking_lot` swap + `tracing` logging are **M5c**; agent
  survival across restart and client auto-reconnect are M5c-adjacent/M5d — deliberately excluded here.
  M5b keeps `std::sync::Mutex` (M5c will swap it), so new `.lock().unwrap()` call sites here match the
  surrounding code; the minor churn is M5c's to absorb.
- **Type consistency:** `reap_companion`/`shutdown`/`InstanceLock::{acquire,default_path,path}` /
  `remove_files` names are used identically across tasks; `split_pane`'s receiver change is called out
  in Global Constraints and every existing call site already holds an `Arc<Daemon>`.
