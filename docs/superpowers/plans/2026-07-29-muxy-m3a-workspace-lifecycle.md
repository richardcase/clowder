# muxy M3a — Workspace Lifecycle Core (git) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The git half of M3's lifecycle: `land` (finalize an agent's work onto a clean `muxy/<task>` branch, keep it) and `discard` (throw it away + delete the branch) on the workspace driver, plus `LandAgent`/`DiscardAgent` control requests wired through the daemon. (Client UX = M3b; jj driver = M3c.)

**Architecture:** `WorkspaceDriver` grows `kind`/`land`/`discard`; `GitWorktreeDriver` implements them (shelling to `git`). `muxy-proto` gains the two requests. The daemon refactors `teardown_agent` into `finish_agent(pane, land)` and exposes `land_agent`/`discard_agent`, routed from `control_json`.

**Tech Stack:** Rust, tokio, the `git` CLI.

## Global Constraints

- **"Land" = finalize + hand off:** commit dirty work → remove the worktree → **keep** `muxy/<task>`. Only commit when `git status --porcelain` is non-empty (no spurious empty commit on a clean worktree). **"Discard":** remove worktree `--force` → **delete** the branch.
- **Keep every task's build green.** Task 1 KEEPS the old `teardown` so `muxy-daemon` still compiles; Task 2 adds the proto variants + a one-line daemon stopgap arm; Task 3 wires the real handling and removes `teardown`.
- The daemon keeps its single git `driver` in M3a (per-project driver selection is M3c).
- Commit after each task; conventional messages + standard trailers.

**Test command:** `cargo test` (workspace). Per-crate: `cargo test -p muxy-workspace`, `-p muxy-proto`, `-p muxy-daemon`.

---

## Task 1: `land`/`discard` on the driver (muxy-workspace)

**Files:**
- Modify: `crates/muxy-workspace/src/lib.rs` (WorkspaceKind + Workspace.kind + trait + GitWorktreeDriver + tests)

**Interfaces:**
- Produces: `WorkspaceKind {Git, Jj}`; `Workspace.kind`; `WorkspaceDriver::{kind, land, discard}` (with `teardown` KEPT for now); `GitWorktreeDriver` impls.

- [ ] **Step 1: Write the failing tests** — append to the `#[cfg(test)] mod tests` (reuse the existing `init_repo()` helper):

```rust
    fn branch_exists(repo: &Path, name: &str) -> bool {
        let out = Command::new("git").arg("-C").arg(repo).args(["branch", "--list", name]).output().unwrap();
        !out.stdout.is_empty()
    }

    #[test]
    fn land_commits_dirty_removes_worktree_keeps_branch() {
        let repo = init_repo();
        let d = GitWorktreeDriver;
        let ws = d.provision(repo.path(), "task-a").unwrap();
        std::fs::write(ws.path.join("work.txt"), b"agent output").unwrap();   // dirty
        d.land(&ws).unwrap();
        assert!(!ws.path.exists(), "worktree removed");
        assert!(branch_exists(repo.path(), "muxy/task-a"), "branch kept");
        let log = Command::new("git").arg("-C").arg(repo.path()).args(["log", "muxy/task-a", "--oneline"]).output().unwrap();
        assert!(String::from_utf8_lossy(&log.stdout).contains("muxy: task-a"), "dirty work committed");
    }

    #[test]
    fn land_clean_worktree_makes_no_extra_commit() {
        let repo = init_repo();
        let d = GitWorktreeDriver;
        let ws = d.provision(repo.path(), "task-c").unwrap();          // no changes
        d.land(&ws).unwrap();
        assert!(branch_exists(repo.path(), "muxy/task-c"));
        let count = Command::new("git").arg("-C").arg(repo.path()).args(["rev-list", "--count", "muxy/task-c"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1", "only the initial commit — no muxy: commit");
    }

    #[test]
    fn discard_removes_worktree_and_deletes_branch() {
        let repo = init_repo();
        let d = GitWorktreeDriver;
        let ws = d.provision(repo.path(), "task-b").unwrap();
        d.discard(&ws).unwrap();
        assert!(!ws.path.exists(), "worktree removed");
        assert!(!branch_exists(repo.path(), "muxy/task-b"), "branch deleted");
    }

    #[test]
    fn provision_sets_git_kind() {
        let repo = init_repo();
        let ws = GitWorktreeDriver.provision(repo.path(), "task-k").unwrap();
        assert_eq!(ws.kind, WorkspaceKind::Git);
        assert_eq!(GitWorktreeDriver.kind(), WorkspaceKind::Git);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p muxy-workspace`
Expected: FAIL — `WorkspaceKind`/`kind`/`land`/`discard` don't exist.

- [ ] **Step 3: Add `WorkspaceKind` + the `kind` field:**

```rust
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceKind { Git, Jj }
```
Add `pub kind: WorkspaceKind` to `Workspace`.

- [ ] **Step 4: Extend the trait** — add `kind`/`land`/`discard`, **keep** `teardown`:

```rust
pub trait WorkspaceDriver: Send + Sync {
    fn kind(&self) -> WorkspaceKind;
    fn provision(&self, project: &Path, name: &str) -> Result<Workspace>;
    /// Finalize: commit any uncommitted work, remove the working copy, KEEP the branch.
    fn land(&self, ws: &Workspace) -> Result<()>;
    /// Throw away: remove the working copy and DELETE the branch.
    fn discard(&self, ws: &Workspace) -> Result<()>;
    /// DEPRECATED (removed in M3a Task 3): remove the working copy, keep the branch.
    fn teardown(&self, ws: &Workspace) -> Result<()>;
}
```

- [ ] **Step 5: Implement on `GitWorktreeDriver`** — `kind`, set `kind` in `provision`, and `land`/`discard` (keep `teardown` unchanged):

```rust
    fn kind(&self) -> WorkspaceKind { WorkspaceKind::Git }
```
In `provision`, change the returned `Workspace` to include `kind: WorkspaceKind::Git`.
Add:
```rust
    fn land(&self, ws: &Workspace) -> Result<()> {
        let task = ws.branch.strip_prefix("muxy/").unwrap_or(&ws.branch);
        // Commit any uncommitted work onto the branch (only if dirty).
        let status = Command::new("git").arg("-C").arg(&ws.path).args(["status", "--porcelain"])
            .output().with_context(|| "git status")?;
        if !status.stdout.is_empty() {
            Self::git(&ws.path, &["add", "-A"])?;
            Self::git(&ws.path, &["commit", "-m", &format!("muxy: {task}")])?;
        }
        // Remove the (now clean) worktree; KEEP the branch.
        let path_str = ws.path.to_string_lossy().to_string();
        Self::git(&ws.project, &["worktree", "remove", &path_str])?;
        let _ = Command::new("git").arg("-C").arg(&ws.project).args(["worktree", "prune"]).output();
        Ok(())
    }

    fn discard(&self, ws: &Workspace) -> Result<()> {
        let path_str = ws.path.to_string_lossy().to_string();
        Self::git(&ws.project, &["worktree", "remove", "--force", &path_str])?;
        let _ = Command::new("git").arg("-C").arg(&ws.project).args(["worktree", "prune"]).output();
        Self::git(&ws.project, &["branch", "-D", &ws.branch])?;   // force-delete the unmerged branch
        Ok(())
    }
```

- [ ] **Step 6: Run to verify they pass**

Run: `cargo test -p muxy-workspace` then `cargo test`
Expected: PASS — the 4 new tests + all existing; `muxy-daemon` still compiles (it still calls `teardown`).

- [ ] **Step 7: Commit**

```bash
git add crates/muxy-workspace/src/lib.rs
git commit -m "feat(workspace): WorkspaceKind + land/discard on the driver (git)"
```

---

## Task 2: `LandAgent`/`DiscardAgent` proto + daemon stopgap (muxy-proto + muxy-daemon)

**Files:**
- Modify: `crates/muxy-proto/src/control.rs` (variants + tests)
- Modify: `crates/muxy-daemon/src/control_json.rs` (one-line stopgap so it compiles)

**Interfaces:**
- Produces: `ControlRequest::{LandAgent, DiscardAgent}`.

- [ ] **Step 1: Write the failing test** — in `control.rs`'s test module:

```rust
    #[test]
    fn land_discard_requests_roundtrip() {
        for r in [
            ControlRequest::LandAgent { pane: PaneId(3) },
            ControlRequest::DiscardAgent { pane: PaneId(4) },
        ] {
            let s = serde_json::to_string(&r).unwrap();
            assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap(), "{s}");
        }
        assert!(serde_json::to_string(&ControlRequest::LandAgent { pane: PaneId(3) }).unwrap()
            .contains(r#""type":"landAgent""#));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p muxy-proto`
Expected: FAIL — variants don't exist.

- [ ] **Step 3: Add the variants** to `ControlRequest` (after `GetSplitTree`):

```rust
    LandAgent { pane: PaneId },
    DiscardAgent { pane: PaneId },
```

- [ ] **Step 4: Add the daemon stopgap.** Adding those variants makes `control_json.rs`'s request match non-exhaustive. Add a temporary arm alongside the others (Task 3 replaces it with real handling):

```rust
                                Ok(ControlRequest::LandAgent { .. }) | Ok(ControlRequest::DiscardAgent { .. }) =>
                                    ControlEvent::Error { message: "land/discard not yet wired".into() },
```

- [ ] **Step 5: Run to verify all pass**

Run: `cargo test -p muxy-proto` then `cargo test`
Expected: PASS — the new proto test + all existing (the daemon compiles via the stopgap).

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-proto/src/control.rs crates/muxy-daemon/src/control_json.rs
git commit -m "feat(proto): LandAgent/DiscardAgent requests (+ daemon stopgap)"
```

---

## Task 3: Daemon land/discard wiring (muxy-daemon + muxy-workspace)

Refactor `teardown_agent` into `finish_agent(pane, land)`, expose `land_agent`/`discard_agent`, wire the control channel, and remove the now-unused `teardown`. Gate: `cargo test`.

**Files:**
- Modify: `crates/muxy-daemon/src/server.rs` (finish_agent refactor + land_agent/discard_agent + tests)
- Modify: `crates/muxy-daemon/src/control_json.rs` (replace the stopgap with real arms)
- Modify: `crates/muxy-workspace/src/lib.rs` (remove `teardown` from the trait + `GitWorktreeDriver`)

**Interfaces:**
- Consumes: `WorkspaceDriver::{land, discard}` (Task 1), `ControlRequest::{LandAgent, DiscardAgent}` (Task 2).
- Produces: `Daemon::{land_agent, discard_agent}`.

- [ ] **Step 1: Write the failing tests** — in `server.rs`'s test module (reuse the temp-repo + `Arc<Daemon>` + `SyntheticAdapter` harness from `split_close_and_teardown_manage_the_tree`; the daemon's `driver` is `GitWorktreeDriver` in tests):

```rust
    fn branch_exists(repo: &std::path::Path, name: &str) -> bool {
        let out = std::process::Command::new("git").arg("-C").arg(repo).args(["branch", "--list", name]).output().unwrap();
        !out.stdout.is_empty()
    }

    #[tokio::test]
    async fn land_agent_keeps_branch_and_removes_agent() {
        let (daemon, repo) = /* existing harness */;
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter {
            command: PaneCommand { program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] },
        }, "task-a").unwrap();
        // write some work into the worktree
        let ws = daemon.workspace_of(agent).unwrap();
        std::fs::write(ws.path.join("out.txt"), b"work").unwrap();

        daemon.land_agent(agent).unwrap();
        assert!(daemon.workspace_of(agent).is_none(), "agent workspace removed");
        assert!(daemon.get(agent).is_none(), "agent pane removed");
        assert!(branch_exists(repo.path(), "muxy/task-a"), "land keeps the branch");
    }

    #[tokio::test]
    async fn discard_agent_deletes_branch_and_removes_agent() {
        let (daemon, repo) = /* existing harness */;
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter {
            command: PaneCommand { program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] },
        }, "task-b").unwrap();
        daemon.discard_agent(agent).unwrap();
        assert!(daemon.workspace_of(agent).is_none());
        assert!(daemon.get(agent).is_none());
        assert!(!branch_exists(repo.path(), "muxy/task-b"), "discard deletes the branch");
    }
```

> Fill `/* existing harness */` from `split_close_and_teardown_manage_the_tree` (temp git repo + `Arc<Daemon>`). Optionally also add a control-channel test (send `LandAgent`, expect `AgentRemoved`) mirroring `client_attaches_and_receives_output`; not required if the direct-API tests cover it.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p muxy-daemon land_agent`
Expected: FAIL — `land_agent`/`discard_agent` don't exist.

- [ ] **Step 3: Refactor `teardown_agent` into `finish_agent`.** Rename the existing `teardown_agent` body to a private `finish_agent(&self, pane: PaneId, land: bool)`, and replace the single driver call:

```rust
        // was: if let Some(ws) = self.workspace_of(pane) { self.driver.teardown(&ws)?; }
        if let Some(ws) = self.workspace_of(pane) {
            if land { self.driver.land(&ws)?; } else { self.driver.discard(&ws)?; }
        }
```
Then add the three public entry points:
```rust
    pub fn teardown_agent(&self, pane: PaneId) -> Result<()> { self.finish_agent(pane, false) }
    pub fn land_agent(&self, pane: PaneId) -> Result<()> { self.finish_agent(pane, true) }
    pub fn discard_agent(&self, pane: PaneId) -> Result<()> { self.finish_agent(pane, false) }
```

> Ordering is unchanged from today: companions cascade-killed, agent pane killed, watcher/scanner aborted, THEN the driver op (so the worktree isn't locked), then the maps are cleared and `AgentRemoved` broadcast. If the driver op returns `Err`, it propagates before the maps are cleared — the agent stays (now pane-dead); the user can retry Discard.

- [ ] **Step 4: Replace the control-channel stopgap** in `control_json.rs` with real arms:

```rust
                                Ok(ControlRequest::LandAgent { pane }) => match self.land_agent(pane) {
                                    Ok(()) => ControlEvent::AgentRemoved { pane },
                                    Err(e) => ControlEvent::Error { message: e.to_string() },
                                },
                                Ok(ControlRequest::DiscardAgent { pane }) => match self.discard_agent(pane) {
                                    Ok(()) => ControlEvent::AgentRemoved { pane },
                                    Err(e) => ControlEvent::Error { message: e.to_string() },
                                },
```
(Remove the Task-2 `Ok(LandAgent{..}) | Ok(DiscardAgent{..}) => Error` stopgap — leaving it would make these unreachable.) The same connection also gets `AgentRemoved` from the `removed_tx` broadcast arm — a harmless duplicate for a single client (idempotent removal).

- [ ] **Step 5: Remove the now-unused `teardown`** from the `WorkspaceDriver` trait and `GitWorktreeDriver` in `crates/muxy-workspace/src/lib.rs` (nothing calls it now that `finish_agent` uses `land`/`discard`). Delete its trait declaration + the `GitWorktreeDriver` impl + its dedicated test (`teardown_removes_worktree`) if present (the land/discard tests cover the behavior).

- [ ] **Step 6: Run to verify all pass**

Run: `cargo test -p muxy-daemon` then `cargo test`
Expected: PASS — the new land/discard tests + all existing across the workspace.

- [ ] **Step 7: Commit**

```bash
git add crates/muxy-daemon/src/server.rs crates/muxy-daemon/src/control_json.rs crates/muxy-workspace/src/lib.rs
git commit -m "feat(daemon): land_agent/discard_agent wiring; teardown -> discard"
```

---

## Final verification

- `cargo test` → whole workspace green: the git `land`/`discard` tests, the proto round-trips, and the daemon `land_agent`/`discard_agent` tests, plus all existing.
- Land keeps a clean `muxy/<task>` branch (work committed if dirty) and removes the agent; Discard deletes the branch and removes the agent; the daemon's teardown/close path now discards (no dangling branch). Client UX is M3b; the jj driver + per-project selection is M3c.
