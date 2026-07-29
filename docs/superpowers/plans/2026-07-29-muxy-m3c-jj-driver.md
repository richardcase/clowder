# muxy M3c — jj Driver + Per-Project Auto-Detect

## Context

muxy provisions an isolated working copy per agent behind the `WorkspaceDriver`
trait. Today only **git** is implemented (`GitWorktreeDriver`, git worktrees on
`muxy/<task>` branches). M3a added `land`/`discard` to the trait and the daemon
lifecycle; M3b surfaced them in the app. **M3c — the final M3 slice — adds the
second backend: a `JjDriver` (jujutsu, via the `jj` CLI) plus `driver_for(project)`
auto-detection**, so a repo managed with jj "just works" and the daemon picks the
right backend automatically. This is a pure Rust change (`muxy-workspace` +
`muxy-daemon`); no protocol or client change. It branches off `main` (which has
M3a) and is independent of the open M3b PR #24.

Design is already approved in the overall spec
`docs/superpowers/specs/2026-07-29-muxy-m3-jj-lifecycle-design.md` (the "M3c" slice
and the `JjDriver`/`driver_for` sections). jj **0.40.0** is installed locally.

**Ground truth (verified):** `muxy-workspace/src/lib.rs` — `WorkspaceDriver` trait
(`kind`/`provision`/`land`/`discard`, `anyhow::Result`); `Workspace{path,branch,project,kind}`
(all `pub`); `WorkspaceKind{Git,Jj}` (Jj variant already present); `GitWorktreeDriver`
is a unit struct with an inherent `fn git(project,&[&str]) -> Result<()>` helper
(`Command::new("git").arg("-C").arg(project)…`; `bail!` on non-zero). Daemon
`server.rs`: `driver: Arc<dyn WorkspaceDriver>` field (line 37), built in `new()` as
`Arc::new(GitWorktreeDriver)` (line 51), injected via `new_with` (57-84); read at
spawn_agent provision (127) + cleanup discard (146) and finish_agent land/discard
(245/247); `workspaces: HashMap<PaneId, Workspace>` with `workspace_of(pane)` (208).
Every daemon+workspace test uses a real temp git repo; no fake driver, no
skip-if-absent gate exists yet.

## Approach

1. **`JjDriver`** in `muxy-workspace/src/lib.rs`, a sibling to `GitWorktreeDriver`,
   shelling out to `jj` (its own `Self::jj` helper). Each agent gets a `jj workspace`;
   **land** pins the work under a bookmark then forgets the workspace (keeps the
   change); **discard** abandons the change then forgets the workspace.
2. **`driver_for(project)`** + **`driver_for_kind(kind)`** in the same crate: detect
   jj-vs-git by walking ancestors for `.jj`/`.git`; map a stored `WorkspaceKind` back
   to a driver for land/discard routing.
3. **Daemon**: drop the single injected `driver` field; `spawn_agent` selects via
   `driver_for(project)`, `finish_agent` routes via `driver_for_kind(ws.kind)`. Tests
   already build real repos, so detection replaces injection with no behavior change
   for git.

**jj semantics asymmetry (intentional, document it):** git `provision` creates the
branch eagerly (`worktree add -b`), so git `discard` deletes it (`branch -D`). jj
`provision` does **not** create a bookmark — the `muxy/<name>` bookmark is created
only at **land**. So jj `discard` has no bookmark to delete; it only abandons + forgets.

## Global Constraints

- All VCS ops shell out via `std::process::Command` (no jj-lib / git2); mirror
  `GitWorktreeDriver::git`'s pattern (`.output()` + `bail!` with `stderr` on non-zero).
- `anyhow::Result` throughout; reuse `bail!`/`.with_context`.
- **jj tests are gated on `jj` being installed** — a `jj_available()` helper; if it
  returns false the test early-returns (passes), never fails. (New pattern; git tests
  stay ungated.)
- Naming: git branch `muxy/<name>`; jj workspace `muxy-<name>` + bookmark `muxy/<name>`;
  `<name>` derived from `ws.branch.strip_prefix("muxy/").unwrap_or(&ws.branch)` (same as git).
- No `muxy-proto` and no `macos/` changes. Add no new crate dependencies (jj is a CLI).
- Test: `source "$HOME/.cargo/env" && cargo test -p muxy-workspace` / `-p muxy-daemon`.

---

## Task 1 — `JjDriver` (muxy-workspace)

**Files:** modify `crates/muxy-workspace/src/lib.rs` (add `JjDriver` + tests).

**Step 0 — validate jj invocations.** In a scratch dir, confirm each command below
works on the installed **jj 0.40.0** and adjust flags if the CLI differs, keeping the
semantics identical (create workspace / set bookmark / forget workspace / abandon change):
`jj git init <d>`; `jj -R <d> workspace add --name muxy-x <d>/.muxy/worktrees/x`;
`jj -R <path> bookmark set muxy/x -r @`; `jj -R <d> workspace forget muxy-x`;
`jj -R <path> abandon -r @`; `jj -R <d> bookmark list`. Note the working forms in the report.

**Step 1 — implement** (append after `GitWorktreeDriver`'s impl). Add near the top:
`use std::sync::Arc;` (needed by Task 2 in the same file).

```rust
/// A jujutsu workspace driver: each agent gets its own `jj workspace` (a working copy
/// with its own working-copy commit `@`). Sibling to GitWorktreeDriver; shells out to `jj`.
pub struct JjDriver;

impl JjDriver {
    fn jj(repo: &Path, args: &[&str]) -> Result<()> {
        let out = Command::new("jj")
            .arg("-R")
            .arg(repo)
            .args(args)
            .output()
            .with_context(|| format!("failed to run jj {args:?}"))?;
        if !out.status.success() {
            bail!("jj {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        }
        Ok(())
    }
}

impl WorkspaceDriver for JjDriver {
    fn kind(&self) -> WorkspaceKind {
        WorkspaceKind::Jj
    }

    fn provision(&self, project: &Path, name: &str) -> Result<Workspace> {
        let branch = format!("muxy/{name}");
        let path = project.join(".muxy").join("worktrees").join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| "create worktrees parent dir")?;
        }
        let ws_name = format!("muxy-{name}");
        let path_str = path.to_string_lossy().to_string();
        // Create a jj workspace at `path` with a fresh working-copy commit.
        Self::jj(project, &["workspace", "add", "--name", &ws_name, &path_str])?;
        Ok(Workspace { path, branch, project: project.to_path_buf(), kind: WorkspaceKind::Jj })
    }

    fn land(&self, ws: &Workspace) -> Result<()> {
        let name = ws.branch.strip_prefix("muxy/").unwrap_or(&ws.branch);
        let ws_name = format!("muxy-{name}");
        // jj auto-snapshots the working copy into `@` — nothing to add/commit. Pin the
        // work under a bookmark so it survives forgetting the workspace, then detach + remove.
        Self::jj(&ws.path, &["bookmark", "set", &ws.branch, "-r", "@"])?;
        Self::jj(&ws.project, &["workspace", "forget", &ws_name])?;
        let _ = std::fs::remove_dir_all(&ws.path);
        Ok(())
    }

    fn discard(&self, ws: &Workspace) -> Result<()> {
        let name = ws.branch.strip_prefix("muxy/").unwrap_or(&ws.branch);
        let ws_name = format!("muxy-{name}");
        // Drop the working-copy change (best-effort — an empty `@` needn't block cleanup),
        // then detach + remove the workspace. No bookmark was created, so nothing persists.
        let _ = Self::jj(&ws.path, &["abandon", "-r", "@"]);
        Self::jj(&ws.project, &["workspace", "forget", &ws_name])?;
        let _ = std::fs::remove_dir_all(&ws.path);
        Ok(())
    }
}
```

**Step 2 — tests** (in the existing `#[cfg(test)] mod tests`). Add a gate + jj fixture,
then the driver tests. Adjust the fixture's exact commands to the Step-0-validated forms.

```rust
fn jj_available() -> bool {
    Command::new("jj").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// A fresh jj repo with one snapshotted file. Returns the TempDir (kept alive).
fn init_jj_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let run = |args: &[&str]| {
        let ok = Command::new("jj").arg("-R").arg(p).args(args)
            .env("JJ_USER", "muxy-test").env("JJ_EMAIL", "muxy@test.invalid")
            .status().unwrap().success();
        assert!(ok, "jj {args:?} failed");
    };
    // `jj git init <p>` initialises in place (validate exact form in Step 0).
    let ok = Command::new("jj").args(["git", "init", &p.to_string_lossy()])
        .env("JJ_USER", "muxy-test").env("JJ_EMAIL", "muxy@test.invalid")
        .status().unwrap().success();
    assert!(ok, "jj git init failed");
    std::fs::write(p.join("README.md"), b"init").unwrap();
    run(&["status"]); // force a working-copy snapshot
    dir
}

fn jj_bookmark_exists(repo: &Path, name: &str) -> bool {
    let out = Command::new("jj").arg("-R").arg(repo).args(["bookmark", "list"])
        .env("JJ_USER", "muxy-test").env("JJ_EMAIL", "muxy@test.invalid")
        .output().unwrap();
    String::from_utf8_lossy(&out.stdout).contains(name)
}

#[test]
fn jj_provision_creates_workspace_and_sets_jj_kind() {
    if !jj_available() { return; }
    let repo = init_jj_repo();
    let ws = JjDriver.provision(repo.path(), "task-j").unwrap();
    assert!(ws.path.is_dir(), "jj workspace dir not created");
    assert_eq!(ws.branch, "muxy/task-j");
    assert_eq!(ws.kind, WorkspaceKind::Jj);
    assert_eq!(JjDriver.kind(), WorkspaceKind::Jj);
}

#[test]
fn jj_land_sets_bookmark_forgets_workspace_removes_dir() {
    if !jj_available() { return; }
    let repo = init_jj_repo();
    let ws = JjDriver.provision(repo.path(), "task-l").unwrap();
    std::fs::write(ws.path.join("work.txt"), b"agent output").unwrap();
    JjDriver.land(&ws).unwrap();
    assert!(!ws.path.exists(), "workspace dir should be removed after land");
    assert!(jj_bookmark_exists(repo.path(), "muxy/task-l"), "bookmark should be kept");
}

#[test]
fn jj_discard_forgets_workspace_and_leaves_no_bookmark() {
    if !jj_available() { return; }
    let repo = init_jj_repo();
    let ws = JjDriver.provision(repo.path(), "task-d").unwrap();
    std::fs::write(ws.path.join("work.txt"), b"throwaway").unwrap();
    JjDriver.discard(&ws).unwrap();
    assert!(!ws.path.exists(), "workspace dir should be removed after discard");
    assert!(!jj_bookmark_exists(repo.path(), "muxy/task-d"), "discard must not leave a bookmark");
}
```

**Steps:** write failing tests → `cargo test -p muxy-workspace` (fail) → implement →
`cargo test -p muxy-workspace` (pass) → commit `feat(workspace): JjDriver (jj CLI) — provision/land/discard`.

---

## Task 2 — `driver_for` + `driver_for_kind` (muxy-workspace)

**Files:** modify `crates/muxy-workspace/src/lib.rs`. Depends on Task 1 (`JjDriver`).
These need no jj binary (detection is filesystem-only; the constructors don't shell out),
so their tests are **not** gated.

```rust
/// Pick a workspace driver for `project`: jj if a `.jj` dir is found at `project` or an
/// ancestor, else git. `.jj` wins over `.git` (colocated repos), matching jj's own behaviour.
pub fn driver_for(project: &Path) -> Arc<dyn WorkspaceDriver> {
    let mut cur = Some(project);
    while let Some(dir) = cur {
        if dir.join(".jj").is_dir() {
            return Arc::new(JjDriver);
        }
        if dir.join(".git").exists() {
            return Arc::new(GitWorktreeDriver);
        }
        cur = dir.parent();
    }
    Arc::new(GitWorktreeDriver)
}

/// The driver matching a provisioned workspace's kind — used to route land/discard.
pub fn driver_for_kind(kind: WorkspaceKind) -> Arc<dyn WorkspaceDriver> {
    match kind {
        WorkspaceKind::Git => Arc::new(GitWorktreeDriver),
        WorkspaceKind::Jj => Arc::new(JjDriver),
    }
}
```

**Tests** (ungated — use `std::fs::create_dir_all` to fake the marker dirs):

```rust
#[test]
fn driver_for_picks_git_for_a_git_project() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    assert_eq!(driver_for(dir.path()).kind(), WorkspaceKind::Git);
}

#[test]
fn driver_for_picks_jj_when_dot_jj_present_even_with_git() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::create_dir_all(dir.path().join(".jj")).unwrap();
    assert_eq!(driver_for(dir.path()).kind(), WorkspaceKind::Jj); // .jj wins
}

#[test]
fn driver_for_finds_marker_in_ancestor() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".jj")).unwrap();
    let nested = dir.path().join("a").join("b");
    std::fs::create_dir_all(&nested).unwrap();
    assert_eq!(driver_for(&nested).kind(), WorkspaceKind::Jj);
}

#[test]
fn driver_for_kind_maps_both() {
    assert_eq!(driver_for_kind(WorkspaceKind::Git).kind(), WorkspaceKind::Git);
    assert_eq!(driver_for_kind(WorkspaceKind::Jj).kind(), WorkspaceKind::Jj);
}
```

**Steps:** write failing tests → verify fail → implement → verify pass → commit
`feat(workspace): driver_for + driver_for_kind auto-detect`.

---

## Task 3 — Daemon multi-driver wiring (muxy-daemon)

**Files:** modify `crates/muxy-daemon/src/server.rs` (+ test call sites in `server.rs` and
`control_json.rs`). Depends on Tasks 1 & 2.

**Interfaces consumed:** `muxy_workspace::{driver_for, driver_for_kind, Workspace, WorkspaceKind}`.

**Change set:**
- Import (line 8): `use muxy_workspace::{driver_for, driver_for_kind, Workspace, WorkspaceKind};`
  (drop `GitWorktreeDriver`/`WorkspaceDriver` from the daemon's `use`; keep whatever the
  compiler still needs — the driver is now a local, not a field).
- **Remove** the `driver: Arc<dyn WorkspaceDriver>` field (line 37), its `new_with` param
  and field-init (57-84), and the `Arc::new(GitWorktreeDriver)` arg in `new()` (49-55).
  `new_with` now takes `(notifier, hook_sock)`.
- `spawn_agent` (around line 127): select the driver from the project, use it for both the
  provision and the post-provision-failure cleanup:
  ```rust
  let driver = driver_for(project);
  let ws = driver.provision(project, task)?;
  ```
  and the existing cleanup site (line 146) becomes `let _ = driver.discard(&ws);` (same local).
- `finish_agent` (around 243-250): route by the stored kind:
  ```rust
  if let Some(ws) = self.workspace_of(pane) {
      let driver = driver_for_kind(ws.kind);
      if land {
          driver.land(&ws)?;
      } else {
          driver.discard(&ws)?;
      }
  }
  self.workspaces.lock().unwrap().remove(&pane);
  ```
- **Test call sites:** drop the leading `StdArc::new(GitWorktreeDriver),` argument from every
  `Daemon::new_with(…)` (server.rs:757, 805; control_json.rs:168, 237, 295) and remove any now-unused
  `GitWorktreeDriver` test import. Existing git tests still create real git repos, so
  `driver_for` transparently selects git — no behavior change, all should stay green.

**New test — daemon picks jj + lands** (add to `server.rs` tests; gate on jj). Reuse the
`jj_available()`/`init_jj_repo()`/`jj_bookmark_exists()` approach from Task 1 (duplicate a
small local fixture, consistent with the existing `init_repo` duplication in `control_json.rs`):

```rust
#[test]
fn spawn_in_jj_repo_uses_jj_driver_and_land_keeps_bookmark() {
    if !jj_available() { return; }
    let repo = init_jj_repo();               // local fixture: `jj git init` + a snapshot
    let daemon = StdArc::new(Daemon::new_with(
        StdArc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-jj.sock"),
    ));
    let adapter = SyntheticAdapter { command: PaneCommand {
        program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], ..Default::default()
    }};
    let pane = daemon.spawn_agent(repo.path(), &adapter, "task-a").unwrap();
    assert_eq!(daemon.workspace_of(pane).unwrap().kind, WorkspaceKind::Jj);
    daemon.land_agent(pane).unwrap();
    assert!(daemon.list_agents().is_empty());
    assert!(jj_bookmark_exists(repo.path(), "muxy/task-a"));
}
```
(Match `SyntheticAdapter`/`PaneCommand` construction to the existing spawn tests exactly.)

**Steps:** write the jj daemon test (fails) → refactor field→auto-detect + fix call sites →
`cargo test -p muxy-daemon` (all green, incl. existing git tests) → whole-workspace
`cargo test` → commit `feat(daemon): per-project driver auto-detect (git/jj) via driver_for`.

---

## Verification

- `source "$HOME/.cargo/env" && cargo test -p muxy-workspace -p muxy-daemon` and a full
  `cargo test` — all green (existing git tests unchanged; new jj tests pass with jj 0.40
  installed, and would early-return if jj were absent).
- **Manual (user):** in a real `jj` project, spawn an agent from the app, make a change in
  its workspace, **Land** → `jj bookmark list` shows `muxy/<task>` and `jj workspace list`
  no longer lists `muxy-<task>` (dir gone). Spawn again, **Discard** → no `muxy/<task>`
  bookmark, workspace gone. In a git project, Land/Discard behave exactly as before
  (auto-detect chose git).

## Execution note

This slice is Rust-only and additive (no proto/client, no cross-crate enum-variant break,
so no stopgap arms needed). Branch off `main`. On approval I'll write this to
`docs/superpowers/plans/2026-07-29-muxy-m3c-jj-driver.md`, commit it, and run it via
subagent-driven development (Task 1 → 2 → 3, review gate each, opus final review), matching
the M3a/M3b workflow. The parked M3b "land-on-close" question is untouched here.
