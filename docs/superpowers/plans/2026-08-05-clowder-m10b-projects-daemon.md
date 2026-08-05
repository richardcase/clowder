# M10b — Projects Daemon + CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make projects a durable, daemon-owned entity: add/remove/list over the control socket, worktrees that can only be spawned inside a registered project, a lazy terminal at each project root, and restart for an exited worktree — all reachable from the `clowder` CLI.

**Architecture:** A `ProjectStore` built on M10a's `JsonStore<T>` persists canonical project paths beside `agents.json`. The daemon gains project CRUD, a `projects_tx` broadcast mirroring the existing attention/removed/split channels, and two maps tracking lazily-spawned project-terminal panes. Project terminals reuse the existing split machinery by seeding `trees`/`owner` exactly as `finalize_agent` already does. `reconcile`'s per-record body is factored into a shared `resume_agent` that the new `restart_worktree` also calls, so restart-by-click and restart-by-daemon-restart cannot drift.

**Tech Stack:** Rust (edition 2021, stable, `anyhow`, `serde`, `tokio`), Swift 5 / SwiftPM (`ClowderCore` protocol mirror only — no UI).

## Global Constraints

- **Prefix every cargo command** with `source "$HOME/.cargo/env" && ` — rustup is not auto-sourced in this environment.
- **Branch:** all work lands on `feat/m10b-projects-daemon`, already checked out, cut from `feat/m10a-worktree-model`. Its PR targets **`feat/m10a-worktree-model`**, not `main` (stacked PR).
- **Spec:** `docs/superpowers/specs/2026-08-05-clowder-projects-design.md` §3–§8. Read it before starting. The "Notes for M10b" section of `docs/superpowers/plans/2026-08-05-clowder-m10a-worktree-model.md` carries findings from M10a's review that this plan acts on.
- **Swift:** run `cd macos && swift test`. **NEVER run `swift build`** — it links a gitignored 189 MB vendored libghostty that is absent here. This PR touches only `Models.swift` and tests; no UI (that is M10c).
- **Ignore stale SourceKit diagnostics.** Trust `cargo` / `swift test` CLI output only.
- **`git` is proxied through a filtering wrapper** — piping `git` output into `grep` can silently mislead. Use `rtk proxy git <args>` for raw output.
- **When renaming anything that crosses the wire, grep BOTH the Rust identifier and its serde camelCase spelling.** M10a shipped a stale `{"type":"listAgents"}` literal because its straggler grep covered only identifiers.
- **Every commit message ends with these two trailers**, separated from the body by a blank line:
  ```
  Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC
  ```
- **Canonicalize project paths on both sides.** `add_project` canonicalizes; `spawn_agent`'s registered-project check must canonicalize its argument the same way, or every `/tmp` project fails on macOS (`/tmp` → `/private/tmp`).
- Three `clowder-daemon` tests are known to flake under parallel load. If a failure looks timing-related, re-run once before investigating.

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/clowder-daemon/src/store.rs` | Add `try_mutate` (surfaces write failures) | 1 |
| `crates/clowder-daemon/src/projects.rs` (new) | `ProjectRecord` + `ProjectStore`: canonicalization, validation, persistence. **Policy-free** | 1 |
| `crates/clowder-daemon/src/server.rs` | Store wiring + explicit-path constructor, project CRUD with policy, spawn guards, `resume_agent`/`restart_worktree`, project terminals, `root_cwd` | 2, 5, 6, 7 |
| `crates/clowder-proto/src/control.rs` | `ProjectInfo`, 5 requests, 5 events, `SpawnAgent` `task`→`name` | 3 |
| `crates/clowder-daemon/src/control_json.rs` | Dispatch the new requests; `projects_tx` `select!` arm | 4 |
| `crates/clowder-client/src/{lib.rs,main.rs}` | `clowder project add\|list\|rm`; `spawn` sends `name` | 8 |
| `docs/protocol/fixtures/*.json` (new) | Golden wire fixtures, asserted by both languages | 9 |
| `macos/Sources/ClowderCore/Models.swift` | Protocol mirror for the new types (no UI) | 9 |

---

### Task 1: `try_mutate` and the project store

M10a's review flagged that `JsonStore::mutate` warns and returns as though it persisted. That is faithful for `set_tree`, but `add_project` answers a **user request** — reporting success for a project that never reached disk means the user only discovers it after a daemon restart.

`ProjectStore` stays **policy-free**: it validates the path itself and persists. "Refuse to remove while worktrees exist" needs the agent list, so it lives on `Daemon` (Task 2).

**Files:**
- Modify: `crates/clowder-daemon/src/store.rs` (add `try_mutate`; add tests)
- Create: `crates/clowder-daemon/src/projects.rs`
- Modify: `crates/clowder-daemon/src/lib.rs` (add `pub mod projects;` beside `pub mod store;`)

**Interfaces:**
- Consumes: `JsonStore<T>` (`new`, `load`, `mutate`, `mutate_if`) from M10a; `clowder_workspace::{detect_kind, WorkspaceKind}` from M10a.
- Produces:
  - `JsonStore::try_mutate<R>(&self, f: impl FnOnce(&mut Vec<T>) -> R) -> anyhow::Result<R>`
  - `ProjectRecord { pub path: PathBuf, pub kind: String }`
  - `ProjectStore::new(path: PathBuf) -> Self`, `::default_path() -> PathBuf`
  - `ProjectStore::list(&self) -> Vec<ProjectRecord>`
  - `ProjectStore::add(&self, path: &Path) -> Result<ProjectRecord>`
  - `ProjectStore::remove(&self, path: &Path) -> Result<()>`
  - `ProjectStore::contains(&self, canonical: &Path) -> bool`

- [ ] **Step 1: Write the failing tests**

Append to `crates/clowder-daemon/src/store.rs`'s test module:

```rust
    #[test]
    fn try_mutate_surfaces_write_failures() {
        // Point the store at a path whose parent cannot be created (a FILE, not a dir),
        // so create_dir_all fails and the error must reach the caller.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"i am a file").unwrap();
        let store: JsonStore<Item> = JsonStore::new(blocker.join("items.json"));
        let err = store.try_mutate(|all| all.push(Item { id: 1, label: "a".into() }));
        assert!(err.is_err(), "try_mutate must not silently swallow a write failure");
    }

    #[test]
    fn try_mutate_returns_value_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let store: JsonStore<Item> = JsonStore::new(dir.path().join("items.json"));
        let n = store.try_mutate(|all| { all.push(Item { id: 3, label: "x".into() }); all.len() }).unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.load().len(), 1);
    }
```

Create `crates/clowder-daemon/src/projects.rs` containing only its test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A git repo (just the marker dir — `detect_kind` only looks for `.git`).
    fn git_dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        d
    }

    #[test]
    fn add_records_canonical_path_and_kind() {
        let repo = git_dir();
        let state = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(state.path().join("projects.json"));
        let rec = store.add(repo.path()).unwrap();
        // On macOS a tempdir path is NOT canonical (/var -> /private/var); the stored path must be.
        assert_eq!(rec.path, repo.path().canonicalize().unwrap());
        assert_eq!(rec.kind, "git");
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn add_is_idempotent() {
        let repo = git_dir();
        let state = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(state.path().join("projects.json"));
        store.add(repo.path()).unwrap();
        store.add(repo.path()).unwrap();
        assert_eq!(store.list().len(), 1, "adding twice must not duplicate");
    }

    #[test]
    fn add_matches_a_non_canonical_path_to_its_canonical_record() {
        // The whole point of canonicalizing: a caller passing an uncanonical path must
        // hit the SAME record, or spawn's registered-project check fails on macOS.
        let repo = git_dir();
        let state = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(state.path().join("projects.json"));
        let rec = store.add(repo.path()).unwrap();
        assert!(store.contains(&rec.path));
        let link = tempfile::tempdir().unwrap().path().join("link");
        std::os::unix::fs::symlink(repo.path(), &link).unwrap();
        let via_link = store.add(&link).unwrap();
        assert_eq!(via_link.path, rec.path, "symlink must resolve to the same record");
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn add_rejects_a_non_repo() {
        let plain = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(state.path().join("projects.json"));
        let e = store.add(plain.path()).unwrap_err().to_string();
        assert!(e.contains("not a git or jj repository"), "unhelpful message: {e}");
    }

    #[test]
    fn add_rejects_a_missing_path_and_a_file() {
        let state = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(state.path().join("projects.json"));
        assert!(store.add(std::path::Path::new("/nope/does/not/exist")).is_err());
        let f = tempfile::NamedTempFile::new().unwrap();
        let e = store.add(f.path()).unwrap_err().to_string();
        assert!(e.contains("not a directory"), "unhelpful message: {e}");
    }

    #[test]
    fn add_rejects_a_path_inside_a_registered_projects_worktrees() {
        let repo = git_dir();
        let state = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(state.path().join("projects.json"));
        store.add(repo.path()).unwrap();
        let wt = repo.path().join(".clowder").join("worktrees").join("feat");
        std::fs::create_dir_all(&wt).unwrap();
        let e = store.add(&wt).unwrap_err().to_string();
        assert!(e.contains("worktree"), "unhelpful message: {e}");
    }

    #[test]
    fn remove_drops_the_record_and_is_ok_when_absent() {
        let repo = git_dir();
        let state = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(state.path().join("projects.json"));
        store.add(repo.path()).unwrap();
        store.remove(repo.path()).unwrap();
        assert!(store.list().is_empty());
        store.remove(repo.path()).unwrap(); // absent -> Ok, not an error
    }

    #[test]
    fn jj_wins_for_a_colocated_repo() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        std::fs::create_dir_all(d.path().join(".jj")).unwrap();
        let state = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(state.path().join("projects.json"));
        assert_eq!(store.add(d.path()).unwrap().kind, "jj");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `pub mod projects;` to `crates/clowder-daemon/src/lib.rs` immediately after `pub mod store;`, then:

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon projects:: store::try_mutate`
Expected: FAIL to compile — `cannot find type 'ProjectStore'`, `no method named 'try_mutate'`

- [ ] **Step 3: Implement `try_mutate`**

In `crates/clowder-daemon/src/store.rs`, add beside `mutate`. Note `mutate` should now delegate, so the lock/load/write sequence exists once:

```rust
    /// Like `mutate`, but returns the write error instead of only logging it. Use this for
    /// operations answering a user request, where silently failing to persist would report
    /// success for something that never reached disk.
    pub fn try_mutate<R>(&self, f: impl FnOnce(&mut Vec<T>) -> R) -> Result<R> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load();
        let out = f(&mut all);
        self.try_write(&all)?;
        Ok(out)
    }
```

Leave `mutate` and `mutate_if` exactly as they are — `mutate` warns-and-continues, which is correct for the layout/registry paths that must never fail an agent operation.

- [ ] **Step 4: Implement `ProjectStore`**

Prepend to `crates/clowder-daemon/src/projects.rs`:

```rust
use crate::store::JsonStore;
use anyhow::{bail, Context, Result};
use clowder_workspace::detect_kind;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One registered project. `path` is canonical and is the record's identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub path: PathBuf,
    /// `"git"` or `"jj"` — `WorkspaceKind::as_str()` at the time it was added.
    pub kind: String,
}

/// The durable list of projects the user has added. Policy-free: it validates and persists,
/// but knows nothing about agents. "Refuse to remove while worktrees exist" lives on `Daemon`.
pub struct ProjectStore {
    store: JsonStore<ProjectRecord>,
}

impl ProjectStore {
    pub fn new(path: PathBuf) -> Self {
        Self { store: JsonStore::new(path) }
    }

    /// `$CLOWDER_PROJECTS_FILE` › `$XDG_STATE_HOME/clowder/projects.json` ›
    /// `$HOME/.local/state/clowder/projects.json` — the same derivation as the agent registry.
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("CLOWDER_PROJECTS_FILE") {
            if !p.is_empty() { return PathBuf::from(p); }
        }
        let base = std::env::var("XDG_STATE_HOME").ok().filter(|s| !s.is_empty())
            .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.local/state")))
            .unwrap_or_else(|| "/tmp".to_string());
        PathBuf::from(base).join("clowder").join("projects.json")
    }

    pub fn list(&self) -> Vec<ProjectRecord> {
        self.store.load()
    }

    /// Is `canonical` a registered project? The caller must pass an already-canonical path.
    pub fn contains(&self, canonical: &Path) -> bool {
        self.store.load().iter().any(|r| r.path == canonical)
    }

    /// Register `path`. Idempotent: adding an already-registered project returns its record.
    /// Messages are user-facing — they surface in the app's error banner.
    pub fn add(&self, path: &Path) -> Result<ProjectRecord> {
        // Canonicalize FIRST: on macOS /tmp resolves to /private/tmp, and `spawn_agent`'s
        // registered-project check compares canonical paths. If only one side canonicalizes,
        // every spawn into such a project fails the check.
        let canonical = path
            .canonicalize()
            .with_context(|| format!("no such path: {}", path.display()))?;
        if !canonical.is_dir() {
            bail!("not a directory: {}", canonical.display());
        }
        let kind = detect_kind(&canonical)
            .ok_or_else(|| anyhow::anyhow!("not a git or jj repository: {}", canonical.display()))?;

        // Adding a clowder worktree as a project would nest branches inside branches.
        let marker = Path::new(".clowder").join("worktrees");
        for rec in self.store.load() {
            if canonical.starts_with(rec.path.join(&marker)) {
                bail!(
                    "{} is a worktree of project {} — add the project, not its worktree",
                    canonical.display(),
                    rec.path.display()
                );
            }
        }

        let rec = ProjectRecord { path: canonical, kind: kind.as_str().to_string() };
        let out = rec.clone();
        self.store.try_mutate(move |all| {
            if !all.iter().any(|r| r.path == rec.path) {
                all.push(rec);
            }
        })?;
        Ok(out)
    }

    /// Drop `path` from the list. Absent is not an error (idempotent).
    pub fn remove(&self, path: &Path) -> Result<()> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.store.try_mutate(|all| all.retain(|r| r.path != canonical))?;
        Ok(())
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon projects:: store::`
Expected: PASS (8 project tests + 6 store tests)

- [ ] **Step 6: Commit**

```bash
git add crates/clowder-daemon/src/store.rs crates/clowder-daemon/src/projects.rs crates/clowder-daemon/src/lib.rs
git commit -m "feat(daemon): add the project store

ProjectStore persists canonical project paths beside agents.json, rejecting
anything that is not a git or jj repo and anything that is itself a clowder
worktree. Canonicalization is load-bearing: spawn's registered-project check
compares canonical paths, and on macOS /tmp resolves to /private/tmp.

JsonStore::try_mutate surfaces write failures, so add_project cannot report
success for a project that never reached disk.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 2: Wire the store into `Daemon`, with a test-friendly constructor

`Daemon::new_with` currently builds `Registry::new(Registry::default_path())`, which reads env at construction — that is why the crate needs a global `STATE_FILE_ENV_LOCK` and why any test touching state must serialize. Adding a second env-dependent store would double that contention.

Add a constructor taking both paths explicitly. Tests then point at temp dirs with **no env vars and no lock**, and stay parallel.

**Files:**
- Modify: `crates/clowder-daemon/src/server.rs` (`Daemon` struct, constructors, project CRUD, broadcast)

**Interfaces:**
- Consumes: `ProjectStore` from Task 1.
- Produces:
  - `Daemon::new_with_paths(notifier: Arc<dyn Notifier>, hook_sock: PathBuf, registry_path: PathBuf, projects_path: PathBuf) -> Daemon`
  - `Daemon::list_projects(&self) -> Vec<clowder_proto::ProjectInfo>` *(returns `ProjectRecord`-derived data; the proto type lands in Task 3 — until then return `Vec<crate::projects::ProjectRecord>` and switch in Task 3)*
  - `Daemon::add_project(&self, path: &Path) -> Result<ProjectRecord>`
  - `Daemon::remove_project(&self, path: &Path) -> Result<()>` — **refuses while worktrees exist**
  - `Daemon::is_registered_project(&self, path: &Path) -> bool`
  - `Daemon::subscribe_projects(&self) -> broadcast::Receiver<ProjectChange>`
  - `pub enum ProjectChange { Added(ProjectRecord), Removed(PathBuf) }`

- [ ] **Step 1: Write the failing tests**

Add to `server.rs`'s test module:

```rust
    /// A daemon whose registry AND project store live in `dir` — no env vars, no global lock.
    fn test_daemon_in(dir: &std::path::Path) -> StdArc<Daemon> {
        StdArc::new(Daemon::new_with_paths(
            StdArc::new(crate::FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-m10b.sock"),
            dir.join("agents.json"),
            dir.join("projects.json"),
        ))
    }

    #[tokio::test]
    async fn add_and_list_projects_round_trips() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        let rec = d.add_project(repo.path()).unwrap();
        assert_eq!(rec.path, repo.path().canonicalize().unwrap());
        assert_eq!(rec.kind, "git");
        assert_eq!(d.list_projects().len(), 1);
        assert!(d.is_registered_project(repo.path()), "uncanonical path must still match");
    }

    #[tokio::test]
    async fn remove_project_is_refused_while_a_worktree_exists() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter {
            command: crate::PaneCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                cwd: None,
                env: vec![],
            },
        };
        let pane = d.spawn_agent(repo.path(), &adapter, "feat").unwrap();

        let e = d.remove_project(repo.path()).unwrap_err().to_string();
        assert!(e.contains("1"), "message should say how many: {e}");
        assert_eq!(d.list_projects().len(), 1, "project must survive a refused removal");

        d.discard_agent(pane).unwrap();
        d.remove_project(repo.path()).unwrap();      // now allowed
        assert!(d.list_projects().is_empty());
    }

    #[tokio::test]
    async fn project_changes_broadcast_to_subscribers() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        let mut rx = d.subscribe_projects();
        d.add_project(repo.path()).unwrap();
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
            Ok(Ok(crate::server::ProjectChange::Added(rec))) => {
                assert_eq!(rec.path, repo.path().canonicalize().unwrap());
            }
            other => panic!("expected Added, got {other:?}"),
        }
        d.remove_project(repo.path()).unwrap();
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
            Ok(Ok(crate::server::ProjectChange::Removed(p))) => {
                assert_eq!(p, repo.path().canonicalize().unwrap());
            }
            other => panic!("expected Removed, got {other:?}"),
        }
    }
```

`init_repo()` already exists in `server.rs`'s tests — reuse it, do not redefine it.

- [ ] **Step 2: Run to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon add_and_list_projects remove_project_is_refused project_changes_broadcast`
Expected: FAIL to compile — `no function 'new_with_paths'`

- [ ] **Step 3: Add the field, the channel, and the constructor**

In the `Daemon` struct, beside `registry`:

```rust
    projects: Arc<crate::projects::ProjectStore>,
    projects_tx: broadcast::Sender<ProjectChange>,
```

Above the struct:

```rust
/// A change to the project list, broadcast to every connected client.
/// Task 7 adds a third variant, `TerminalClosed(PathBuf)`.
#[derive(Clone, Debug)]
pub enum ProjectChange {
    Added(crate::projects::ProjectRecord),
    Removed(PathBuf),
}
```

Rewrite the constructors so there is exactly one body:

```rust
    pub fn new_with(notifier: Arc<dyn Notifier>, hook_sock: PathBuf) -> Daemon {
        Daemon::new_with_paths(
            notifier,
            hook_sock,
            crate::registry::Registry::default_path(),
            crate::projects::ProjectStore::default_path(),
        )
    }

    /// Like `new_with`, but with both state files given explicitly. Tests use this to point at a
    /// temp dir without setting process-global env vars (which would force them to serialize).
    pub fn new_with_paths(
        notifier: Arc<dyn Notifier>,
        hook_sock: PathBuf,
        registry_path: PathBuf,
        projects_path: PathBuf,
    ) -> Daemon {
        // ... existing body, with these two changes:
        //   registry: Arc::new(crate::registry::Registry::new(registry_path)),
        //   projects: Arc::new(crate::projects::ProjectStore::new(projects_path)),
        //   projects_tx,      // from a fourth `broadcast::channel(256)`
    }
```

- [ ] **Step 4: Add the CRUD methods**

```rust
    pub fn subscribe_projects(&self) -> broadcast::Receiver<ProjectChange> {
        self.projects_tx.subscribe()
    }

    pub fn list_projects(&self) -> Vec<crate::projects::ProjectRecord> {
        let mut out = self.projects.list();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// Is `path` (canonicalized here) a registered project?
    pub fn is_registered_project(&self, path: &Path) -> bool {
        match path.canonicalize() {
            Ok(c) => self.projects.contains(&c),
            Err(_) => false,
        }
    }

    pub fn add_project(&self, path: &Path) -> Result<crate::projects::ProjectRecord> {
        let rec = self.projects.add(path)?;
        let _ = self.projects_tx.send(ProjectChange::Added(rec.clone()));
        Ok(rec)
    }

    /// Remove a project. Refused while any worktree still belongs to it — there must be no path
    /// by which removing a sidebar row abandons live work.
    pub fn remove_project(&self, path: &Path) -> Result<()> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        // Canonicalize BOTH sides. Task 5 makes spawn_agent store a canonical path, but this
        // must be correct before that lands too — otherwise on macOS an uncanonical
        // AgentMeta.project (/var/...) never matches a canonical project (/private/var/...),
        // the count comes back 0, and the guard silently lets the removal through.
        let n = self
            .agents
            .lock()
            .values()
            .filter(|m| {
                let p = Path::new(&m.project);
                p.canonicalize().unwrap_or_else(|_| p.to_path_buf()) == canonical
            })
            .count();
        if n > 0 {
            bail!("project {} still has {n} worktree(s) — land or discard them first", canonical.display());
        }
        // Task 7 adds: kill this project's terminal pane here, before dropping the record.
        self.projects.remove(&canonical)?;
        let _ = self.projects_tx.send(ProjectChange::Removed(canonical));
        Ok(())
    }
```

`AgentMeta.project` is a `String` holding the full path (M10a), and it is **not** canonical until Task 5. Canonicalizing both sides of the comparison, as above, makes this task's test pass now and stay correct after Task 5 — do not rely on Task 5 landing first, and do not compare basenames.

- [ ] **Step 5: Run the tests**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/clowder-daemon/src/server.rs
git commit -m "feat(daemon): own the project list

Daemon gains project CRUD and a projects_tx broadcast mirroring the existing
attention/removed/split channels. remove_project refuses while any worktree
belongs to the project, so removing a sidebar row can never abandon live work.

new_with_paths takes both state-file paths explicitly, so tests point at a temp
dir without process-global env vars — the existing registry default_path forces
a crate-wide lock, and a second env-dependent store would double that.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 3: Control-protocol surface

**Files:**
- Modify: `crates/clowder-proto/src/control.rs` (+ its `mod tests`)
- Modify: `crates/clowder-proto/src/lib.rs` (re-export `ProjectInfo`)
- Modify: `crates/clowder-daemon/src/server.rs` (`list_projects` returns `ProjectInfo`)

**Interfaces:**
- Produces:
  - `ProjectInfo { path: String, name: String, kind: String }`
  - `ControlRequest::{ListProjects, AddProject{path}, RemoveProject{path}, OpenProjectTerminal{path}, RestartWorktree{pane}}`
  - `ControlEvent::{ProjectList{projects}, ProjectAdded{project}, ProjectRemoved{path}, ProjectTerminalOpened{path,pane}, ProjectTerminalClosed{path}}`
  - `ControlRequest::SpawnAgent { project, name, adapter }` — `task` renamed to `name`

- [ ] **Step 1: Write the failing tests**

Add to `control.rs`'s `mod tests`:

```rust
    #[test]
    fn project_requests_round_trip_with_camel_case_types() {
        for (r, tag) in [
            (ControlRequest::ListProjects, "listProjects"),
            (ControlRequest::AddProject { path: "/p".into() }, "addProject"),
            (ControlRequest::RemoveProject { path: "/p".into() }, "removeProject"),
            (ControlRequest::OpenProjectTerminal { path: "/p".into() }, "openProjectTerminal"),
            (ControlRequest::RestartWorktree { pane: PaneId(4) }, "restartWorktree"),
        ] {
            let s = serde_json::to_string(&r).unwrap();
            assert!(s.contains(&format!(r#""type":"{tag}""#)), "{s}");
            assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap());
        }
    }

    #[test]
    fn project_events_round_trip_with_camel_case_types() {
        let p = ProjectInfo { path: "/Users/x/code/clowder".into(), name: "clowder".into(), kind: "git".into() };
        for (e, tag) in [
            (ControlEvent::ProjectList { projects: vec![p.clone()] }, "projectList"),
            (ControlEvent::ProjectAdded { project: p.clone() }, "projectAdded"),
            (ControlEvent::ProjectRemoved { path: "/p".into() }, "projectRemoved"),
            (ControlEvent::ProjectTerminalOpened { path: "/p".into(), pane: PaneId(9) }, "projectTerminalOpened"),
            (ControlEvent::ProjectTerminalClosed { path: "/p".into() }, "projectTerminalClosed"),
        ] {
            let s = serde_json::to_string(&e).unwrap();
            assert!(s.contains(&format!(r#""type":"{tag}""#)), "{s}");
            assert_eq!(e, serde_json::from_str::<ControlEvent>(&s).unwrap());
        }
    }

    #[test]
    fn project_terminal_opened_pane_is_a_bare_number() {
        let e = ControlEvent::ProjectTerminalOpened { path: "/p".into(), pane: PaneId(9) };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains(r#""pane":9"#), "PaneId must serialize as a bare number: {s}");
    }

    #[test]
    fn spawn_agent_uses_name_not_task() {
        let r = ControlRequest::SpawnAgent {
            project: "/p".into(), name: "add-projects".into(), adapter: "claude".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""name":"add-projects""#), "{s}");
        assert!(!s.contains(r#""task""#), "the field is `name` now: {s}");
        assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap());
    }
```

Update the existing `spawn_agent_request_json_shape` test to use `name:` instead of `task:`.

- [ ] **Step 2: Run to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-proto`
Expected: FAIL to compile — `no variant named 'ListProjects'`, `struct ControlRequest::SpawnAgent has no field named 'name'`

- [ ] **Step 3: Implement**

In `crates/clowder-proto/src/control.rs`, add above `ControlRequest`:

```rust
/// One registered project. `name` is derived at the wire boundary (the path's last component)
/// and is not stored — the daemon's record holds only the canonical path and the kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    /// Canonical path to the project root — the identity.
    pub path: String,
    /// Display name: the path's last component.
    pub name: String,
    /// `"git"` or `"jj"`.
    pub kind: String,
}
```

Add to `ControlRequest` (rename `task` to `name` in `SpawnAgent` at the same time):

```rust
    SpawnAgent { project: String, name: String, adapter: String },
    ListProjects,
    AddProject { path: String },
    RemoveProject { path: String },
    OpenProjectTerminal { path: String },
    RestartWorktree { pane: PaneId },
```

Add to `ControlEvent`:

```rust
    ProjectList { projects: Vec<ProjectInfo> },
    ProjectAdded { project: ProjectInfo },
    ProjectRemoved { path: String },
    ProjectTerminalOpened { path: String, pane: PaneId },
    /// The terminal's root pane went away — the user closed it or the shell exited. Clients
    /// drop their `path -> pane` mapping so the next select respawns.
    ProjectTerminalClosed { path: String },
```

Re-export `ProjectInfo` from `crates/clowder-proto/src/lib.rs`'s `pub use control::{…}` list.

In `server.rs`, change `list_projects` to return `Vec<clowder_proto::ProjectInfo>`:

```rust
    pub fn list_projects(&self) -> Vec<clowder_proto::ProjectInfo> {
        let mut recs = self.projects.list();
        recs.sort_by(|a, b| a.path.cmp(&b.path));
        recs.into_iter().map(project_info).collect()
    }
```

with a free function beside it (Task 4 and Task 7 both need it):

```rust
/// Wire form of a project record: display name derived from the path's last component.
pub(crate) fn project_info(rec: crate::projects::ProjectRecord) -> clowder_proto::ProjectInfo {
    let name = rec.path.file_name().map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| rec.path.to_string_lossy().to_string());
    clowder_proto::ProjectInfo {
        path: rec.path.to_string_lossy().to_string(),
        name,
        kind: rec.kind,
    }
}
```

Follow the compiler to fix the `SpawnAgent { task }` destructuring in `control_json.rs` and `crates/clowder-client/src/lib.rs`. **Grep both spellings** afterwards:

```bash
grep -rn '"task"\|task:' crates --include="*.rs" | grep -i spawn
```

`AgentRecord.task` must remain `task` — that is the on-disk field that keeps `agents.json` readable across a daemon restart. Only the *wire* field is renamed.

- [ ] **Step 4: Run the tests**

Run: `source "$HOME/.cargo/env" && cargo test --workspace`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/
git commit -m "feat(proto): project control surface

Adds ProjectInfo plus five requests and five events for project CRUD, the
lazy project terminal, and worktree restart.

Also renames SpawnAgent's task field to name, so the control protocol has one
vocabulary: a worktree has a name, and the agent is a process inside it.
AgentRecord.task is deliberately unchanged — it is the on-disk field that keeps
agents.json readable across a daemon restart.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 4: Dispatch the new requests over the control socket

**Files:**
- Modify: `crates/clowder-daemon/src/control_json.rs` (dispatch + `select!` arm + tests)

**Interfaces:**
- Consumes: Task 2's `Daemon` CRUD + `subscribe_projects`; Task 3's request/event variants.
- Produces: the control socket answers `listProjects` / `addProject` / `removeProject`, and streams `projectAdded` / `projectRemoved` to every connected client.

`OpenProjectTerminal` and `RestartWorktree` are dispatched in Tasks 7 and 6 respectively — this task wires only the three CRUD requests plus the broadcast arm. If you reach `OpenProjectTerminal` here, return `ControlEvent::Error { message: "not implemented".into() }` as a placeholder **only if** the compiler forces exhaustiveness, and note it in your report.

- [ ] **Step 1: Write the failing test**

Add to `control_json.rs`'s test module, following the existing duplex-stream harness:

```rust
    #[tokio::test]
    async fn control_json_adds_lists_and_streams_projects() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-projects.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
        ));

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });
        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();
        let _snapshot = clines.next_line().await.unwrap().unwrap();

        let req = ControlRequest::AddProject { path: repo.path().to_string_lossy().to_string() };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        // The reply is ProjectAdded, with the kind detected and the name derived.
        let added = loop {
            let l = clines.next_line().await.unwrap().unwrap();
            if let Ok(ControlEvent::ProjectAdded { project }) = serde_json::from_str::<ControlEvent>(&l) {
                break project;
            }
        };
        assert_eq!(added.kind, "git");
        assert_eq!(added.path, repo.path().canonicalize().unwrap().to_string_lossy());

        cwr.write_all(b"{\"type\":\"listProjects\"}\n").await.unwrap();
        let listed = loop {
            let l = clines.next_line().await.unwrap().unwrap();
            if let Ok(ControlEvent::ProjectList { projects }) = serde_json::from_str::<ControlEvent>(&l) {
                if !projects.is_empty() { break projects; }
            }
        };
        assert_eq!(listed.len(), 1);
    }

    #[tokio::test]
    async fn control_json_add_project_rejects_a_non_repo() {
        let state = tempfile::tempdir().unwrap();
        let plain = tempfile::tempdir().unwrap();
        let daemon = Arc::new(Daemon::new_with_paths(
            Arc::new(FakeNotifier::new()),
            std::path::PathBuf::from("/tmp/unused-projects2.sock"),
            state.path().join("agents.json"),
            state.path().join("projects.json"),
        ));
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let d = daemon.clone();
        tokio::spawn(async move { let _ = d.handle_control_json(server_io).await; });
        let (crd, mut cwr) = tokio::io::split(client_io);
        let mut clines = BufReader::new(crd).lines();
        let _snapshot = clines.next_line().await.unwrap().unwrap();

        let req = ControlRequest::AddProject { path: plain.path().to_string_lossy().to_string() };
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cwr.write_all(line.as_bytes()).await.unwrap();

        let l = clines.next_line().await.unwrap().unwrap();
        assert!(l.contains("not a git or jj repository"), "expected a helpful error: {l}");
        assert!(daemon.list_projects().is_empty());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon control_json_adds_lists control_json_add_project_rejects`
Expected: FAIL — unknown request variants in the match

- [ ] **Step 3: Implement dispatch**

Add arms to the `match serde_json::from_str::<ControlRequest>(&l)` block in `handle_control_json`:

```rust
                                Ok(ControlRequest::ListProjects) =>
                                    ControlEvent::ProjectList { projects: self.list_projects() },
                                Ok(ControlRequest::AddProject { path }) =>
                                    match self.add_project(Path::new(&path)) {
                                        Ok(rec) => ControlEvent::ProjectAdded { project: crate::server::project_info(rec) },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::RemoveProject { path }) =>
                                    match self.remove_project(Path::new(&path)) {
                                        Ok(()) => ControlEvent::ProjectRemoved { path },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
```

Add the subscription beside the existing three, before the loop:

```rust
        let mut proj_rx = self.subscribe_projects();
```

and a `select!` arm beside the existing three, matching their lagged/closed handling exactly:

```rust
                pc = proj_rx.recv() => {
                    match pc {
                        Ok(crate::server::ProjectChange::Added(rec)) =>
                            write_event(&mut wr, &ControlEvent::ProjectAdded {
                                project: crate::server::project_info(rec) }).await?,
                        Ok(crate::server::ProjectChange::Removed(p)) =>
                            write_event(&mut wr, &ControlEvent::ProjectRemoved {
                                path: p.to_string_lossy().to_string() }).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
```

Note this means the requesting client receives `ProjectAdded` **twice** — once as its direct reply, once via the broadcast. That is the same shape the existing `SplitPane` path already has, and clients treat these events as idempotent state updates. Do not try to suppress it.

- [ ] **Step 4: Run the tests**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/src/control_json.rs
git commit -m "feat(daemon): serve project CRUD over the control socket

listProjects/addProject/removeProject are dispatched, and a projects_tx select!
arm streams projectAdded/projectRemoved to every connected client, mirroring the
existing attention/removed/split arms.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 5: Spawn guards

Three pre-checks before `driver.provision`, each with a message that says what to do. Check 3 is what makes `reconcile`'s orphaned worktrees legible rather than a trap: `reconcile` prunes a registry record when resume fails but leaves the worktree on disk, and re-using that name currently dies inside `git worktree add`.

**This task breaks every existing test that spawns without registering a project first.** That is the point of the guard. Fix them by calling `d.add_project(repo.path())` before the spawn — do not weaken the guard.

**Files:**
- Modify: `crates/clowder-daemon/src/server.rs` (`spawn_agent`, and every test that spawns)
- Modify: `crates/clowder-daemon/src/control_json.rs` (tests that spawn)

**Interfaces:**
- Consumes: `is_registered_project` (Task 2), `clowder_workspace::validate_workspace_name` (M10a).
- Produces: `spawn_agent` canonicalizes `project` before use and stores the canonical form in `AgentMeta`/`AgentRecord`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[tokio::test]
    async fn spawn_rejects_an_unregistered_project_and_leaves_nothing_behind() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] } };

        let e = d.spawn_agent(repo.path(), &adapter, "feat").unwrap_err().to_string();
        assert!(e.contains("unknown project"), "unhelpful message: {e}");
        assert!(!repo.path().join(".clowder/worktrees/feat").exists(), "must not leave a worktree");
        let branches = std::process::Command::new("git").arg("-C").arg(repo.path())
            .args(["branch", "--list", "clowder/feat"]).output().unwrap();
        assert!(branches.stdout.is_empty(), "must not leave a branch");
        assert!(d.list_worktrees().is_empty());
    }

    #[tokio::test]
    async fn spawn_rejects_an_invalid_name_before_provisioning() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] } };
        let e = d.spawn_agent(repo.path(), &adapter, "my feature").unwrap_err().to_string();
        assert!(e.contains("letters"), "should be the name-validation message: {e}");
        assert!(!repo.path().join(".clowder/worktrees").exists());
    }

    #[tokio::test]
    async fn spawn_rejects_a_colliding_worktree_with_a_real_message() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        // Simulate reconcile's orphan: a worktree dir on disk that the daemon knows nothing about.
        std::fs::create_dir_all(repo.path().join(".clowder/worktrees/feat")).unwrap();
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] } };
        let e = d.spawn_agent(repo.path(), &adapter, "feat").unwrap_err().to_string();
        assert!(e.contains("already exists"), "should name the collision, not a raw git error: {e}");
        assert!(!e.contains("fatal:"), "must not surface a raw git error: {e}");
    }

    #[tokio::test]
    async fn spawn_stores_the_canonical_project_path() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] } };
        let pane = d.spawn_agent(repo.path(), &adapter, "feat").unwrap();
        let listed = d.list_worktrees();
        assert_eq!(listed[0].project, repo.path().canonicalize().unwrap().to_string_lossy());
        d.teardown_agent(pane).unwrap();
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon spawn_rejects spawn_stores_the_canonical`
Expected: FAIL — spawns currently succeed

- [ ] **Step 3: Implement the guards**

Replace the head of `spawn_agent` (`crates/clowder-daemon/src/server.rs:214`):

```rust
    pub fn spawn_agent(self: &Arc<Self>, project: &Path, adapter: &dyn AgentAdapter, name: &str) -> Result<PaneId> {
        // Canonicalize first — the registered-project check compares canonical paths, and on
        // macOS /tmp resolves to /private/tmp.
        let project = project
            .canonicalize()
            .with_context(|| format!("no such project path: {}", project.display()))?;
        if !self.projects.contains(&project) {
            bail!("unknown project: {} — add it first", project.display());
        }
        clowder_workspace::validate_workspace_name(name)?;

        // Fail on a collision with a clear message instead of a raw `git worktree add` error.
        // reconcile prunes a registry record when resume fails but leaves the worktree on disk,
        // so an untracked directory here is a real case, not a hypothetical.
        let wt = project.join(".clowder").join("worktrees").join(name);
        if wt.exists() {
            bail!("a worktree named '{name}' already exists at {} — land/discard it or choose another name", wt.display());
        }
        if branch_exists(&project, &format!("clowder/{name}")) {
            bail!("branch clowder/{name} already exists in {} — choose another name", project.display());
        }

        let id = self.alloc_id();
        let driver = driver_for(&project);
        let ws = driver.provision(&project, name)?;
        // ... rest unchanged, using `&project` and `name`
```

Add the helper beside it. It must be kind-aware — `git branch --list` is meaningless in a jj repo:

```rust
/// Does `branch` already exist in `project`? Best-effort: a false negative just means the
/// underlying driver reports the collision instead, which is the pre-M10b behaviour.
fn branch_exists(project: &Path, branch: &str) -> bool {
    use clowder_workspace::WorkspaceKind;
    match clowder_workspace::detect_kind(project) {
        Some(WorkspaceKind::Jj) => std::process::Command::new("jj")
            .arg("-R").arg(project).args(["bookmark", "list", "-r", branch])
            .output().map(|o| o.status.success() && !o.stdout.is_empty()).unwrap_or(false),
        _ => std::process::Command::new("git")
            .arg("-C").arg(project).args(["branch", "--list", branch])
            .output().map(|o| !o.stdout.is_empty()).unwrap_or(false),
    }
}
```

Rename `spawn_agent`'s `task` parameter to `name` throughout, and update `spawn_from_control` in `control_json.rs` to pass the renamed `name` field.

- [ ] **Step 4: Fix every test that now fails**

Run `cargo test -p clowder-daemon` and add `d.add_project(repo.path()).unwrap();` (or the equivalent for that test's daemon binding) before each spawn. Tests constructing a daemon via `Daemon::new_with` need switching to `new_with_paths` with a temp state dir so they get an isolated project store. Expect roughly ten call sites across `server.rs` and `control_json.rs`.

**Do not weaken the guard to make a test pass.** If a test genuinely cannot register a project, report it.

- [ ] **Step 5: Run the whole workspace**

Run: `source "$HOME/.cargo/env" && cargo test --workspace`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/
git commit -m "feat(daemon): guard spawn on project, name and collision

Spawning now requires a registered project, a valid worktree name, and a free
worktree dir + branch. Each failure names what to do instead of surfacing a raw
git error — the collision case is reachable in practice because reconcile prunes
a registry record when resume fails but leaves the worktree on disk.

spawn_agent canonicalizes the project path and stores the canonical form, so the
registered-project check and remove_project's worktree count agree on macOS.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 6: `resume_agent` extraction and `restart_worktree`

Factoring `reconcile`'s per-record body into a shared function is what stops restart-by-click and restart-by-daemon-restart from drifting apart.

**Files:**
- Modify: `crates/clowder-daemon/src/server.rs` (`reconcile`, new `resume_agent` + `restart_worktree`)
- Modify: `crates/clowder-daemon/src/control_json.rs` (dispatch `RestartWorktree`)

**Interfaces:**
- Produces:
  - `Daemon::resume_agent(&Arc<Self>, rec: &AgentRecord) -> Result<PaneId>` — spawns under `PaneId(rec.agent_id)`, calls `finalize_agent`, restores layout
  - `Daemon::restart_worktree(&Arc<Self>, pane: PaneId) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

```rust
    #[tokio::test]
    async fn restart_revives_an_exited_agent_under_the_same_pane_id() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        // An agent that exits immediately.
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "exit 0".into()], cwd: None, env: vec![] } };
        let pane = d.spawn_agent(repo.path(), &adapter, "feat").unwrap();

        // Wait for the exit watcher to mark it Exited.
        for _ in 0..100 {
            if d.attention_of(pane) == Some(AttentionState::Exited) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(d.attention_of(pane), Some(AttentionState::Exited), "agent should have exited");

        d.restart_worktree(pane).unwrap();
        assert_eq!(d.attention_of(pane), Some(AttentionState::Working), "restart resets attention");
        let listed = d.list_worktrees();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].pane, pane, "restart must reuse the pane id — it is the worktree identity");
        d.teardown_agent(pane).unwrap();
    }

    #[tokio::test]
    async fn restart_is_refused_while_the_agent_is_alive() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] } };
        let pane = d.spawn_agent(repo.path(), &adapter, "feat").unwrap();
        let e = d.restart_worktree(pane).unwrap_err().to_string();
        assert!(e.contains("still running"), "unhelpful message: {e}");
        d.teardown_agent(pane).unwrap();
    }

    #[tokio::test]
    async fn restart_of_an_unknown_pane_errors() {
        let state = tempfile::tempdir().unwrap();
        let d = test_daemon_in(state.path());
        assert!(d.restart_worktree(PaneId(999)).is_err());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon restart_`
Expected: FAIL — `no method named 'restart_worktree'`

- [ ] **Step 3: Extract `resume_agent`**

Move the body of `reconcile`'s `for rec in records` loop (`server.rs:151`–~197) into:

```rust
    /// Re-spawn one recorded agent under its original pane id: provision hooks, run the adapter's
    /// resume command, finalize, restore its companion layout. Shared by `reconcile` (daemon
    /// restart) and `restart_worktree` (user request), so the two cannot drift apart.
    fn resume_agent(self: &Arc<Self>, rec: &crate::registry::AgentRecord) -> Result<PaneId> {
        let id = PaneId(rec.agent_id);
        if !rec.worktree_path.exists() {
            bail!("worktree {} is gone", rec.worktree_path.display());
        }
        let kind = clowder_workspace::WorkspaceKind::from_str(&rec.workspace_kind)
            .ok_or_else(|| anyhow::anyhow!("unknown workspace kind {:?}", rec.workspace_kind))?;
        let adapter = crate::agent::build_adapter(&rec.adapter_id)
            .ok_or_else(|| anyhow::anyhow!("unknown adapter {:?}", rec.adapter_id))?;
        let ws = Workspace {
            path: rec.worktree_path.clone(),
            branch: rec.branch.clone(),
            project: rec.project.clone(),
            kind,
        };
        adapter.provision_hooks(&ws.path, id, &self.hook_sock)?;
        let mut cmd = adapter.resume_command(&ws.path);
        cmd.cwd = Some(ws.path.clone());
        cmd.env.push(("CLOWDER_AGENT_ID".into(), id.0.to_string()));
        cmd.env.push(("CLOWDER_HOOK_SOCK".into(), self.hook_sock.to_string_lossy().to_string()));
        let pane = Pane::spawn(id, cmd, rec.cols, rec.rows, self.backlog_cap)?;
        let restore_cwd = ws.path.clone();
        self.finalize_agent(id, pane, ws, &rec.task, adapter.as_ref());
        if let Some(tree) = rec.tree.clone() {
            self.restore_layout(id, tree, restore_cwd);
        }
        Ok(id)
    }
```

`reconcile` becomes the loop plus its existing pruning policy — on `Err`, log the warning and `self.registry.remove(rec.agent_id)` exactly as today. Keep `bump_next_id_above` where it is, before the loop.

- [ ] **Step 4: Implement `restart_worktree`**

```rust
    /// Re-run an exited agent in its existing worktree, keeping its pane id (the worktree's
    /// durable identity) and any live companion panes.
    pub fn restart_worktree(self: &Arc<Self>, pane: PaneId) -> Result<()> {
        if self.attention_of(pane) != Some(AttentionState::Exited) {
            bail!("agent {} is still running — land or discard it instead", pane.0);
        }
        let rec = self.registry.load().into_iter().find(|r| r.agent_id == pane.0)
            .ok_or_else(|| anyhow::anyhow!("no worktree with pane {}", pane.0))?;

        // Drop the dead pane and its stale exit watcher; `resume_agent` installs fresh ones under
        // the same id. The split tree and any live companions are deliberately left alone.
        if let Some(h) = self.watchers.lock().remove(&pane) { h.abort(); }
        if let Some(h) = self.scanners.lock().remove(&pane) { h.abort(); }
        self.hookless.lock().remove(&pane);
        self.panes.lock().remove(&pane);

        self.resume_agent(&rec)?;
        Ok(())
    }
```

`finalize_agent` re-inserts `trees`/`owner` for the root leaf, which is idempotent — the existing tree (including companions) is replaced by a bare leaf only if `restore_layout` does not run. **Verify this against the live companions case**: if `rec.tree` is `Some`, `restore_layout` rebuilds it; if the tree in memory has companions but `rec.tree` is `None`, they would be orphaned. Read `finalize_agent` and `restore_layout` before implementing, and if the orphan case is reachable, capture the in-memory tree before the restart and restore from that instead of `rec.tree`. Report which you did and why.

- [ ] **Step 5: Dispatch it**

In `control_json.rs`:

```rust
                                Ok(ControlRequest::RestartWorktree { pane }) =>
                                    match self.restart_worktree(pane) {
                                        Ok(()) => self.tree_event(pane),
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
```

- [ ] **Step 6: Run the tests**

Run: `source "$HOME/.cargo/env" && cargo test --workspace`
Expected: PASS. `reconcile`'s existing tests are the safety net for the extraction — they must pass unmodified.

- [ ] **Step 7: Commit**

```bash
git add crates/
git commit -m "feat(daemon): restart an exited worktree

reconcile's per-record body becomes resume_agent, shared with the new
restart_worktree, so restart-by-click and restart-by-daemon-restart cannot
drift apart.

Restart keeps the original pane id — it is the worktree's durable identity, and
reconcile already re-spawns under it — and leaves live companion panes alone.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 7: Project terminals

A lazily-spawned login shell at the project root. Seeding `trees`/`owner` is the whole trick — the existing split/close/ratio machinery is keyed on a root pane and applies unchanged, exactly as `finalize_agent` already does for agents.

Two guards are needed because the surrounding code is permissive rather than restrictive:
- `finish_agent` skips finalization when there is no workspace (`if let Some(ws)`), so `land`/`discard` on a terminal would **silently succeed and kill it**.
- `close_pane` decides agent-ness by `trees.contains_key(&pane)`, which the seeding makes true.

**Files:**
- Modify: `crates/clowder-daemon/src/server.rs` (state, `open_project_terminal`, `root_cwd`, guards, `remove_project` cascade)
- Modify: `crates/clowder-daemon/src/control_json.rs` (dispatch `OpenProjectTerminal`)

**Interfaces:**
- Produces:
  - `Daemon::open_project_terminal(&Arc<Self>, path: &Path) -> Result<PaneId>` — idempotent
  - `Daemon::project_of_terminal(&self, pane: PaneId) -> Option<PathBuf>`
  - `root_cwd(&self, agent: PaneId) -> Option<PathBuf>` — workspace path, else project-terminal path

- [ ] **Step 1: Write the failing tests**

```rust
    #[tokio::test]
    async fn open_project_terminal_is_idempotent() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let a = d.open_project_terminal(repo.path()).unwrap();
        let b = d.open_project_terminal(repo.path()).unwrap();
        assert_eq!(a, b, "a second select must attach to the same shell");
        assert!(d.list_worktrees().is_empty(), "a terminal is not a worktree");
    }

    #[tokio::test]
    async fn project_terminal_can_be_split() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let term = d.open_project_terminal(repo.path()).unwrap();
        let companion = d.split_pane(term, clowder_proto::SplitDirection::Right).unwrap();
        let tree = d.trees.lock().get(&term).cloned().unwrap();
        assert_eq!(crate::split_tree::leaves(&tree).len(), 2);
        assert_eq!(d.owner_of(companion), Some(term));
    }

    #[tokio::test]
    async fn land_and_discard_refuse_a_project_terminal() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let term = d.open_project_terminal(repo.path()).unwrap();
        // finish_agent tolerates a missing workspace, so without a guard these would silently
        // succeed and kill the terminal.
        assert!(d.land_agent(term).is_err(), "land must refuse a project terminal");
        assert!(d.discard_agent(term).is_err(), "discard must refuse a project terminal");
        assert!(d.get(term).is_some(), "the terminal must still be alive");
    }

    #[tokio::test]
    async fn removing_a_project_kills_its_terminal() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();
        let term = d.open_project_terminal(repo.path()).unwrap();
        d.remove_project(repo.path()).unwrap();
        assert!(d.get(term).is_none(), "the terminal pane must be gone");
        assert!(d.project_of_terminal(term).is_none());
    }

    #[tokio::test]
    async fn open_project_terminal_rejects_an_unregistered_project() {
        let state = tempfile::tempdir().unwrap();
        let repo = init_repo();
        let d = test_daemon_in(state.path());
        assert!(d.open_project_terminal(repo.path()).is_err());
    }
```

`self.get(pane)` and `self.trees` are private — these tests live in the same module, so that is fine.

- [ ] **Step 2: Run to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon project_terminal open_project_terminal land_and_discard_refuse removing_a_project_kills`
Expected: FAIL — `no method named 'open_project_terminal'`

- [ ] **Step 3: Add state and `open_project_terminal`**

New fields on `Daemon` (initialize both in `new_with_paths`):

```rust
    project_terms: Arc<Mutex<HashMap<PathBuf, PaneId>>>,
    term_project: Arc<Mutex<HashMap<PaneId, PathBuf>>>,
```

```rust
    /// The shell pane rooted at a project. Lazy and idempotent: a second caller attaches to the
    /// same shell. Not persisted — a daemon restart drops it and the next select respawns.
    pub fn open_project_terminal(self: &Arc<Self>, path: &Path) -> Result<PaneId> {
        let root = path.canonicalize()
            .with_context(|| format!("no such project path: {}", path.display()))?;
        if !self.projects.contains(&root) {
            bail!("unknown project: {} — add it first", root.display());
        }
        if let Some(existing) = self.project_terms.lock().get(&root).copied() {
            if self.get(existing).is_some() {
                return Ok(existing);
            }
        }
        let id = self.spawn_pane(
            companion_command(self.shell.clone(), root.clone()),
            self.default_cols,
            self.default_rows,
        )?;
        // Seed exactly what finalize_agent seeds for an agent root, so the split/close/ratio
        // machinery — which is keyed on a root pane — applies unchanged.
        self.trees.lock().insert(id, PaneTree::Leaf { pane: id });
        self.owner.lock().insert(id, id);
        self.project_terms.lock().insert(root.clone(), id);
        self.term_project.lock().insert(id, root.clone());

        // When the shell exits, forget it so the next select respawns.
        if let Some(pane_arc) = self.get(id) {
            let me = Arc::clone(self);
            let handle = tokio::spawn(async move {
                pane_arc.wait_exit().await;
                me.forget_project_terminal(id);
            });
            self.watchers.lock().insert(id, handle);
        }
        Ok(id)
    }

    pub fn project_of_terminal(&self, pane: PaneId) -> Option<PathBuf> {
        self.term_project.lock().get(&pane).cloned()
    }

    /// Drop all state for a project terminal whose pane is gone, and tell clients.
    pub(crate) fn forget_project_terminal(&self, pane: PaneId) {
        let Some(root) = self.term_project.lock().remove(&pane) else { return };
        self.project_terms.lock().remove(&root);
        self.trees.lock().remove(&pane);
        self.owner.lock().remove(&pane);
        self.panes.lock().remove(&pane);
        let _ = self.projects_tx.send(ProjectChange::TerminalClosed(root));
    }
```

Add `TerminalClosed(PathBuf)` to `ProjectChange`, and a `select!`-arm case in `control_json.rs` emitting `ControlEvent::ProjectTerminalClosed { path }`.

- [ ] **Step 4: Extract `root_cwd` and add the guards**

`split_pane` currently reads the cwd from `workspaces[agent]` and errors `"no workspace for agent"` (`server.rs:558`). Replace that lookup with:

```rust
    /// The working directory for a companion of `root`: an agent's worktree, or a project
    /// terminal's project root.
    fn root_cwd(&self, root: PaneId) -> Option<PathBuf> {
        if let Some(ws) = self.workspaces.lock().get(&root) {
            return Some(ws.path.clone());
        }
        self.term_project.lock().get(&root).cloned()
    }
```

In `land_agent` and `discard_agent`, before calling `finish_agent`:

```rust
        if self.term_project.lock().contains_key(&pane) {
            bail!("pane {} is a project terminal — it has no workspace to land", pane.0);
        }
```

In `close_pane`, **before** the existing `let is_agent = self.trees.lock().contains_key(&pane);`:

```rust
        if self.term_project.lock().contains_key(&pane) {
            // A project terminal's root: kill it and forget it, rather than taking the agent
            // teardown path (which would emit AgentRemoved for something that is not a worktree).
            if let Some(p) = self.get(pane) { let _ = p.kill(); }
            self.forget_project_terminal(pane);
            return Ok(None);
        }
```

In `remove_project` (Task 2), before dropping the record, kill the terminal and its companions:

```rust
        if let Some(term) = self.project_terms.lock().get(&canonical).copied() {
            let companions: Vec<PaneId> = self.trees.lock().get(&term)
                .map(|t| crate::split_tree::leaves(t).into_iter().filter(|p| *p != term).collect())
                .unwrap_or_default();
            for c in companions {
                if let Some(p) = self.get(c) { let _ = p.kill(); }
                self.panes.lock().remove(&c);
                self.owner.lock().remove(&c);
                if let Some(h) = self.companion_watchers.lock().remove(&c) { h.abort(); }
            }
            if let Some(h) = self.watchers.lock().remove(&term) { h.abort(); }
            if let Some(p) = self.get(term) { let _ = p.kill(); }
            self.forget_project_terminal(term);
        }
```

- [ ] **Step 5: Dispatch it**

```rust
                                Ok(ControlRequest::OpenProjectTerminal { path }) =>
                                    match self.open_project_terminal(Path::new(&path)) {
                                        Ok(pane) => ControlEvent::ProjectTerminalOpened { path, pane },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
```

- [ ] **Step 6: Run the whole workspace**

Run: `source "$HOME/.cargo/env" && cargo test --workspace`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/
git commit -m "feat(daemon): lazy project terminals

A shell rooted at the project, spawned on first select and shared by every
client. Seeding trees/owner exactly as finalize_agent does makes the existing
split machinery apply unchanged; only split_pane's cwd lookup needed widening,
via root_cwd.

Guards were required because the surrounding code is permissive: finish_agent
tolerates a missing workspace, so land/discard would have silently killed a
terminal, and close_pane decides agent-ness by trees.contains_key, which the
seeding makes true.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 8: `clowder project` CLI

Spawning now requires a registered project, so the CLI must be able to register one.

**Files:**
- Modify: `crates/clowder-client/src/lib.rs` (helpers beside `spawn_via_control`)
- Modify: `crates/clowder-client/src/main.rs` (subcommand)

**Interfaces:**
- Produces:
  - `add_project_via_control(sock: &Path, path: &str) -> Result<ProjectInfo>`
  - `list_projects_via_control(sock: &Path) -> Result<Vec<ProjectInfo>>`
  - `remove_project_via_control(sock: &Path, path: &str) -> Result<()>`
  - CLI: `clowder project add <path>`, `clowder project list`, `clowder project rm <path>`

- [ ] **Step 1: Write the failing test**

Add to `crates/clowder-client/src/lib.rs`'s test module — model it on the existing `spawn_via_control` test if one is present; otherwise drive a `UnixListener` that replies with a canned event:

```rust
    #[tokio::test]
    async fn add_project_via_control_returns_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("c.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = tokio::io::split(stream);
            let mut lines = tokio::io::BufReader::new(rd).lines();
            let _req = lines.next_line().await.unwrap().unwrap();
            wr.write_all(
                b"{\"type\":\"projectAdded\",\"project\":{\"path\":\"/p\",\"name\":\"p\",\"kind\":\"git\"}}\n"
            ).await.unwrap();
        });
        let p = add_project_via_control(&sock, "/p").await.unwrap();
        assert_eq!(p.kind, "git");
        assert_eq!(p.name, "p");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client add_project_via_control`
Expected: FAIL — `cannot find function`

- [ ] **Step 3: Implement the helpers**

All three follow `spawn_via_control`'s existing shape: connect, write one JSON line, then read lines until the awaited event or an `Error`, ignoring unparseable lines defensively. Write `add_project_via_control` exactly as below, then write the other two by the same pattern, each awaiting its own event (`ProjectList`, `ProjectRemoved`) and returning `Vec<ProjectInfo>` / `()` respectively:

```rust
pub async fn add_project_via_control(
    control_sock: &std::path::Path,
    path: &str,
) -> anyhow::Result<ProjectInfo> {
    let stream = UnixStream::connect(control_sock).await?;
    let (rd, mut wr) = tokio::io::split(stream);
    let mut lines = BufReader::new(rd).lines();

    let req = ControlRequest::AddProject { path: path.to_string() };
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    wr.write_all(line.as_bytes()).await?;

    // Skip the initial WorktreeList / any streamed events until our result.
    loop {
        match lines.next_line().await? {
            Some(l) => match serde_json::from_str::<ControlEvent>(&l) {
                Ok(ControlEvent::ProjectAdded { project }) => return Ok(project),
                Ok(ControlEvent::Error { message }) => return Err(anyhow::anyhow!(message)),
                Ok(_) => continue,
                Err(_) => continue, // ignore unparseable lines defensively
            },
            None => return Err(anyhow::anyhow!("control socket closed before the result")),
        }
    }
}
```

Add `ProjectInfo` to the crate's `clowder_proto::{…}` import list.

- [ ] **Step 4: Add the subcommand**

In `crates/clowder-client/src/main.rs`, beside the existing `spawn`/`attach` arms:

```rust
        Some("project") => {
            let sock = clowder_config::Config::load().control_sock;
            match args.get(2).map(|s| s.as_str()) {
                Some("add") => {
                    let path = args.get(3).ok_or_else(|| anyhow!("usage: clowder project add <path>"))?;
                    let p = add_project_via_control(&sock, path).await?;
                    println!("{} ({})", p.path, p.kind);
                    Ok(())
                }
                Some("list") => {
                    for p in list_projects_via_control(&sock).await? {
                        println!("{}\t{}\t{}", p.kind, p.name, p.path);
                    }
                    Ok(())
                }
                Some("rm") => {
                    let path = args.get(3).ok_or_else(|| anyhow!("usage: clowder project rm <path>"))?;
                    remove_project_via_control(&sock, path).await?;
                    Ok(())
                }
                _ => Err(anyhow!("usage: clowder project <add|list|rm> [path]")),
            }
        }
```

Update the top-level usage string to include `project`, and rename `spawn`'s positional from `task` to `name` in its usage text.

- [ ] **Step 5: Run the tests**

Run: `source "$HOME/.cargo/env" && cargo test --workspace`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/clowder-client/
git commit -m "feat(cli): clowder project add|list|rm

Spawning now requires a registered project, so the CLI needs a way to register
one. Helpers sit beside spawn_via_control and follow its shape.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 9: Golden wire fixtures and the Swift protocol mirror

The Rust and Swift sides both hand-write the JSON shapes, and **both test suites are self-consistent** — the Swift tests build their own JSON strings, so they would pass unchanged against a diverged Rust encoder. M10a survived that only because reviewers audited keys by eye. This task makes divergence a test failure.

Swift gets the *protocol* types only. No UI — that is M10c.

**Files:**
- Create: `docs/protocol/fixtures/*.json`
- Modify: `crates/clowder-proto/src/control.rs` (fixture assertions in `mod tests`)
- Modify: `macos/Sources/ClowderCore/Models.swift` (new types + `spawnAgent` `task`→`name`)
- Modify: `macos/Tests/ClowderCoreTests/ModelsTests.swift` (fixture decoding)
- Modify: `macos/Sources/ClowderCore/AppModel.swift` and any caller of `spawn(project:task:adapter:)`

**Interfaces:**
- Produces: Swift `ProjectInfo`, `ControlRequest.{listProjects, addProject, removeProject, openProjectTerminal, restartWorktree}`, `ControlEvent.{projectList, projectAdded, projectRemoved, projectTerminalOpened, projectTerminalClosed}`, and `ControlRequest.spawnAgent(project:name:adapter:)`.

- [ ] **Step 1: Create the fixtures**

Create `docs/protocol/fixtures/` with one file per event, each a single line of JSON exactly as the Rust encoder emits it (no pretty-printing, keys in declaration order):

`project-list.json`
```json
{"type":"projectList","projects":[{"path":"/Users/x/code/clowder","name":"clowder","kind":"git"}]}
```

`project-added.json`
```json
{"type":"projectAdded","project":{"path":"/Users/x/code/clowder","name":"clowder","kind":"git"}}
```

`project-removed.json`
```json
{"type":"projectRemoved","path":"/Users/x/code/clowder"}
```

`project-terminal-opened.json`
```json
{"type":"projectTerminalOpened","path":"/Users/x/code/clowder","pane":9}
```

`project-terminal-closed.json`
```json
{"type":"projectTerminalClosed","path":"/Users/x/code/clowder"}
```

`worktree-list.json`
```json
{"type":"worktreeList","worktrees":[{"pane":2,"project":"/Users/x/code/clowder","name":"task-a","branch":"clowder/task-a","state":"NeedsInput"}]}
```

Add `docs/protocol/README.md` explaining the contract in three sentences: these files are the wire format; the Rust test asserts its encoder **produces** each one; the Swift test asserts it **decodes** each one; changing a shape means changing the fixture, which fails both suites until both sides agree.

- [ ] **Step 2: Assert them from Rust**

In `crates/clowder-proto/src/control.rs`'s `mod tests`:

```rust
    /// The fixture directory, relative to this crate's manifest.
    fn fixture(name: &str) -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/protocol/fixtures").join(name);
        std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("missing fixture {}: {e}", p.display()))
            .trim().to_string()
    }

    #[test]
    fn encoder_matches_the_golden_fixtures() {
        let p = ProjectInfo {
            path: "/Users/x/code/clowder".into(), name: "clowder".into(), kind: "git".into(),
        };
        let cases: Vec<(&str, ControlEvent)> = vec![
            ("project-list.json", ControlEvent::ProjectList { projects: vec![p.clone()] }),
            ("project-added.json", ControlEvent::ProjectAdded { project: p }),
            ("project-removed.json", ControlEvent::ProjectRemoved { path: "/Users/x/code/clowder".into() }),
            ("project-terminal-opened.json", ControlEvent::ProjectTerminalOpened {
                path: "/Users/x/code/clowder".into(), pane: PaneId(9) }),
            ("project-terminal-closed.json", ControlEvent::ProjectTerminalClosed {
                path: "/Users/x/code/clowder".into() }),
            ("worktree-list.json", ControlEvent::WorktreeList { worktrees: vec![WorktreeInfo {
                pane: PaneId(2),
                project: "/Users/x/code/clowder".into(),
                name: "task-a".into(),
                branch: "clowder/task-a".into(),
                state: AttentionState::NeedsInput,
            }] }),
        ];
        for (file, ev) in cases {
            assert_eq!(serde_json::to_string(&ev).unwrap(), fixture(file),
                       "encoder drifted from {file} — update BOTH the fixture and the Swift mirror");
        }
    }
```

- [ ] **Step 3: Run it**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-proto encoder_matches_the_golden_fixtures`
Expected: PASS. If it fails, the fixture is wrong — fix the **fixture** to match the encoder, since the Rust type is the source of truth for the shape.

- [ ] **Step 4: Mirror in Swift**

In `macos/Sources/ClowderCore/Models.swift`:

```swift
/// Mirrors the Rust `ProjectInfo`.
public struct ProjectInfo: Codable, Identifiable, Equatable, Sendable {
    /// Canonical path to the project root — the identity.
    public let path: String
    /// Display name: the path's last component.
    public let name: String
    /// `"git"` or `"jj"`.
    public let kind: String
    public var id: String { path }

    public init(path: String, name: String, kind: String) {
        self.path = path
        self.name = name
        self.kind = kind
    }
}
```

Add to `ControlRequest`: `case listProjects`, `case addProject(path: String)`, `case removeProject(path: String)`, `case openProjectTerminal(path: String)`, `case restartWorktree(pane: UInt64)`, and rename `spawnAgent(project:task:adapter:)` to `spawnAgent(project:name:adapter:)` — encoding the key `"name"`. Extend `CodingKeys` with `path` and `project` as needed (`project` already exists).

Add to `ControlEvent`: `case projectList([ProjectInfo])`, `case projectAdded(ProjectInfo)`, `case projectRemoved(path: String)`, `case projectTerminalOpened(path: String, pane: UInt64)`, `case projectTerminalClosed(path: String)`, with `CodingKeys` gaining `projects`, `project`, `path`.

Update `AppModel.spawn(project:task:adapter:)` to `spawn(project:name:adapter:)` and its call sites (`ContentView.swift`'s `SpawnSheet` closure). Do **not** restyle the sheet — that is M10c.

- [ ] **Step 5: Decode the fixtures from Swift**

Add to `macos/Tests/ClowderCoreTests/ModelsTests.swift`:

```swift
    /// Resolve `docs/protocol/fixtures` from this source file's location, so the test does not
    /// depend on the working directory `swift test` happens to run in.
    private func fixture(_ name: String, file: StaticString = #filePath) throws -> Data {
        let here = URL(fileURLWithPath: "\(file)")
        let repo = here.deletingLastPathComponent()   // ClowderCoreTests
            .deletingLastPathComponent()              // Tests
            .deletingLastPathComponent()              // macos
            .deletingLastPathComponent()              // repo root
        return try Data(contentsOf: repo.appendingPathComponent("docs/protocol/fixtures/\(name)"))
    }

    func testDecodesEveryGoldenFixture() throws {
        let d = JSONDecoder()

        guard case let .projectList(ps) = try d.decode(ControlEvent.self, from: fixture("project-list.json")) else {
            return XCTFail("project-list.json did not decode to .projectList")
        }
        XCTAssertEqual(ps, [ProjectInfo(path: "/Users/x/code/clowder", name: "clowder", kind: "git")])

        guard case let .projectAdded(p) = try d.decode(ControlEvent.self, from: fixture("project-added.json")) else {
            return XCTFail("project-added.json did not decode to .projectAdded")
        }
        XCTAssertEqual(p.kind, "git")

        guard case let .projectRemoved(path) = try d.decode(ControlEvent.self, from: fixture("project-removed.json")) else {
            return XCTFail("project-removed.json did not decode to .projectRemoved")
        }
        XCTAssertEqual(path, "/Users/x/code/clowder")

        guard case let .projectTerminalOpened(tPath, pane) =
                try d.decode(ControlEvent.self, from: fixture("project-terminal-opened.json")) else {
            return XCTFail("project-terminal-opened.json did not decode")
        }
        XCTAssertEqual(tPath, "/Users/x/code/clowder")
        XCTAssertEqual(pane, 9)

        guard case let .projectTerminalClosed(cPath) =
                try d.decode(ControlEvent.self, from: fixture("project-terminal-closed.json")) else {
            return XCTFail("project-terminal-closed.json did not decode")
        }
        XCTAssertEqual(cPath, "/Users/x/code/clowder")

        guard case let .worktreeList(ws) = try d.decode(ControlEvent.self, from: fixture("worktree-list.json")) else {
            return XCTFail("worktree-list.json did not decode to .worktreeList")
        }
        XCTAssertEqual(ws.count, 1)
        XCTAssertEqual(ws[0].branch, "clowder/task-a")
        XCTAssertEqual(ws[0].state, .needsInput)
    }

    func testSpawnAgentEncodesNameNotTask() throws {
        let data = try JSONEncoder().encode(
            ControlRequest.spawnAgent(project: "/p", name: "add-projects", adapter: "claude"))
        let s = String(decoding: data, as: UTF8.self)
        XCTAssertTrue(s.contains("\"name\":\"add-projects\""), s)
        XCTAssertFalse(s.contains("\"task\""), s)
    }
```

- [ ] **Step 6: Run both suites**

Run: `source "$HOME/.cargo/env" && cargo test --workspace --locked` then `cd macos && swift test`
Expected: PASS both.

- [ ] **Step 7: Commit**

```bash
git add docs/protocol/ crates/clowder-proto/ macos/
git commit -m "feat(proto): golden wire fixtures guarding the Rust<->Swift seam

Both sides hand-write the JSON shapes and both suites are self-consistent, so
the Swift tests would pass unchanged against a diverged Rust encoder — M10a
survived that only because reviewers audited keys by eye.

docs/protocol/fixtures/*.json is now the wire format: the Rust test asserts its
encoder produces each file, the Swift test asserts it decodes each one. A
renamed key fails both suites.

Also mirrors the new project types in Swift and renames spawnAgent's task field
to name. Protocol only — the UI lands in M10c.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

## Final verification

- [ ] **Full workspace test:** `source "$HOME/.cargo/env" && cargo test --workspace --locked` — PASS (this is what CI runs)
- [ ] **Swift test:** `cd macos && swift test` — PASS
- [ ] **Straggler grep, both spellings:**
  ```bash
  grep -rn '"task"' crates macos/Sources macos/Tests docs | grep -v AgentRecord
  ```
  Expect no hits outside `registry.rs` (`AgentRecord.task` is deliberately unchanged — it keeps `agents.json` readable across a daemon restart).
- [ ] **End-to-end smoke test.** Socket paths must be short — `/tmp/clw-m10b`, not the scratchpad, or you hit `SUN_LEN`:
  ```bash
  SC=/tmp/clw-m10b; rm -rf "$SC"; mkdir -p "$SC/run" "$SC/repo"
  (cd "$SC/repo" && git init -q . && git config user.email t@t.test && git config user.name t \
     && echo hi > README.md && git add . && git commit -qm init)
  export XDG_RUNTIME_DIR="$SC/run" CLOWDER_STATE_FILE="$SC/agents.json" CLOWDER_PROJECTS_FILE="$SC/projects.json"
  ./target/debug/clowder-daemon > "$SC/daemon.log" 2>&1 &
  # then, once the control socket appears:
  ./target/debug/clowder spawn "$SC/repo" feat shell      # expect: "unknown project — add it first"
  ./target/debug/clowder project add "$SC/repo"           # expect: <path> (git)
  ./target/debug/clowder project list
  ./target/debug/clowder spawn "$SC/repo" feat shell      # expect: a pane id
  ./target/debug/clowder spawn "$SC/repo" "bad name" shell # expect: the name-validation message
  ./target/debug/clowder project rm "$SC/repo"            # expect: refused, 1 worktree
  ```
  Kill the daemon and `rm -rf "$SC"` afterwards.
- [ ] **Open the stacked PR — base is `feat/m10a-worktree-model`, NOT `main`:**
  ```bash
  git push -u origin feat/m10b-projects-daemon
  gh pr create --base feat/m10a-worktree-model --title "M10b: projects daemon + CLI" --body "..."
  ```

## Notes for M10c

- The app must send `openProjectTerminal` on selecting a project and store the returned `path → pane` mapping; drop it on `projectTerminalClosed`.
- `restartWorktree` is refused unless the agent's state is `Exited` — the UI should only offer Restart in that state.
- Project terminals are not in the worktree list and carry no attention state; the project row shows a kind badge and an attention rollup over its children, never its own dot.
- The `AgentStore` type name and `PaletteItemKind.agent` were left alone in M10a and M10b — M10c revisits that file wholesale.
- Add a fixture to `docs/protocol/fixtures/` for any new message M10c introduces; the contract is described in `docs/protocol/README.md`.
