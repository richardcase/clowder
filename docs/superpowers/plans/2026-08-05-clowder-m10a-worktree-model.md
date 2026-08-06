# M10a — Worktree Model Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lay the foundations for M10 — repo detection, worktree-name validation, a reusable atomic JSON store, and the `AgentInfo` → `WorktreeInfo` rename — with no user-visible behaviour change.

**Architecture:** Three independent additions plus one repo-wide rename. `detect_kind` becomes the single place that answers "is this a repo, and which kind?", with `driver_for` rebuilt on top of it. `JsonStore<T>` absorbs the atomic load-modify-write machinery currently inlined in `Registry`, which is then rebuilt on it. `WorktreeInfo` replaces `AgentInfo` across the postcard wire, the JSON control protocol, the daemon, and the Swift mirror.

**Tech Stack:** Rust (edition 2021, stable, `anyhow`, `serde`, `postcard`, `tokio`), Swift 5 / SwiftPM (`ClowderCore`, XCTest).

## Global Constraints

- **Every cargo command must be prefixed** with `source "$HOME/.cargo/env" && ` — rustup is not auto-sourced in this environment.
- **Branch:** all work lands on `feat/m10a-worktree-model`, which is already checked out and already contains the spec commit (`c8cbf0b`). Do not commit to `main`.
- **Spec:** `docs/superpowers/specs/2026-08-05-clowder-projects-design.md` §1, §2, §3. Read it before starting.
- **Swift tests do not need libghostty** — `cd macos && swift test` builds `ClowderCore` only. Never run `swift build` (it needs the vendored 189 MB `ghostty-internal.a` and full Xcode).
- **Every commit message ends with these two trailers**, separated from the body by a blank line:
  ```
  Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC
  ```
- **This PR is intentionally behaviour-neutral.** `validate_workspace_name` and `detect_kind`'s `None` case are consumed in M10b, not here. Leaving them unused-but-tested is correct for this stack layer — do not wire them into `spawn_agent`.
- **Ignore stale SourceKit diagnostics** ("No such module", "cannot find type"). Trust `cargo` / `swift test` CLI output only.
- Three daemon tests are known to flake under parallel load (`docs` / prior milestones record this). If a failure looks timing-related, re-run before investigating.

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/clowder-workspace/src/lib.rs` | Add `detect_kind`, `validate_workspace_name`; rebuild `driver_for` on `detect_kind` | 1, 2 |
| `crates/clowder-daemon/src/store.rs` (new) | `JsonStore<T>`: a durable `Vec<T>` in one atomically-written JSON file | 3 |
| `crates/clowder-daemon/src/registry.rs` | `Registry` rebuilt on `JsonStore<AgentRecord>`; behaviour identical | 3 |
| `crates/clowder-daemon/src/lib.rs` | Declare `pub mod store;` | 3 |
| `crates/clowder-proto/src/message.rs` | `AgentInfo` → `WorktreeInfo` (+ `name`, `branch`); `DaemonToClient::AgentList` → `WorktreeList` | 4 |
| `crates/clowder-proto/src/control.rs` | `ControlRequest::ListAgents` → `ListWorktrees`; `ControlEvent::AgentList` → `WorktreeList` | 4 |
| `crates/clowder-proto/src/lib.rs` | Re-export `WorktreeInfo` | 4 |
| `crates/clowder-daemon/src/server.rs` | `AgentMeta` gains `branch`, `task`→`name`, `project` becomes a full path; `list_agents` → `list_worktrees` | 4 |
| `crates/clowder-daemon/src/{control_json.rs,remote.rs}` | Follow the rename | 4 |
| `crates/clowder-client/src/lib.rs` | Follow the rename in `pump`'s match and `spawn_via_control`'s comments | 4 |
| `macos/Sources/ClowderCore/Models.swift` | `WorktreeInfo` mirror; `listWorktrees` / `worktreeList` codings | 5 |
| `macos/Sources/ClowderCore/{AgentStore,AppModel,ControlSession,PaletteSearch}.swift` | Follow the rename | 5 |
| `macos/Sources/ClowderApp/{ContentView,CommandPaletteView,StatusBarController}.swift` | Follow the rename | 5 |
| `macos/Tests/ClowderCoreTests/*.swift` | Follow the rename (9 files) | 5 |

---

### Task 1: `detect_kind` in clowder-workspace

`driver_for` (`crates/clowder-workspace/src/lib.rs:158`) walks ancestors looking for `.jj` (which wins) then `.git`, but **falls back to `GitWorktreeDriver` when neither is found** — so it cannot answer "is this directory a repo at all?". M10b's `add_project` needs exactly that answer. Extract the walk; rebuild `driver_for` on it so the `.jj`-wins rule lives in one place.

**Files:**
- Modify: `crates/clowder-workspace/src/lib.rs:156-170` (the `driver_for` fn) and its `mod tests`
- Test: `crates/clowder-workspace/src/lib.rs` (in-file `#[cfg(test)] mod tests`, matching this crate's convention)

**Interfaces:**
- Consumes: `WorkspaceKind`, `driver_for_kind`, `GitWorktreeDriver` (all already in this file)
- Produces: `pub fn detect_kind(project: &Path) -> Option<WorkspaceKind>` — used by M10b's `add_project`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/clowder-workspace/src/lib.rs`:

```rust
#[test]
fn detect_kind_none_when_not_a_repo() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(detect_kind(dir.path()), None);
}

#[test]
fn detect_kind_git_and_jj_with_jj_winning() {
    let git = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(git.path().join(".git")).unwrap();
    assert_eq!(detect_kind(git.path()), Some(WorkspaceKind::Git));

    let jj = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(jj.path().join(".jj")).unwrap();
    assert_eq!(detect_kind(jj.path()), Some(WorkspaceKind::Jj));

    // Colocated: .jj wins, matching jj's own behaviour.
    let both = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(both.path().join(".git")).unwrap();
    std::fs::create_dir_all(both.path().join(".jj")).unwrap();
    assert_eq!(detect_kind(both.path()), Some(WorkspaceKind::Jj));
}

#[test]
fn detect_kind_finds_marker_in_ancestor() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    let nested = dir.path().join("a").join("b");
    std::fs::create_dir_all(&nested).unwrap();
    assert_eq!(detect_kind(&nested), Some(WorkspaceKind::Git));
}

#[test]
fn detect_kind_treats_git_file_as_git() {
    // A linked worktree has `.git` as a FILE, not a dir — `.exists()` must accept both.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".git"), b"gitdir: /elsewhere").unwrap();
    assert_eq!(detect_kind(dir.path()), Some(WorkspaceKind::Git));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-workspace detect_kind`
Expected: FAIL — `cannot find function 'detect_kind' in this scope`

- [ ] **Step 3: Implement `detect_kind` and rebuild `driver_for` on it**

Replace the body of `driver_for` (`crates/clowder-workspace/src/lib.rs:156-170`) with:

```rust
/// The workspace kind for `project`, or `None` if it is not a repo at all.
/// Walks `project` and its ancestors; `.jj` wins over `.git` (colocated repos), matching
/// jj's own behaviour. Note `.git` is `.exists()` rather than `.is_dir()`: a linked git
/// worktree records `.git` as a FILE.
pub fn detect_kind(project: &Path) -> Option<WorkspaceKind> {
    let mut cur = Some(project);
    while let Some(dir) = cur {
        if dir.join(".jj").is_dir() {
            return Some(WorkspaceKind::Jj);
        }
        if dir.join(".git").exists() {
            return Some(WorkspaceKind::Git);
        }
        cur = dir.parent();
    }
    None
}

/// Pick a workspace driver for `project`. Falls back to git when `project` is not a repo,
/// preserving the pre-M10 contract; callers that need to REJECT a non-repo use `detect_kind`.
pub fn driver_for(project: &Path) -> Arc<dyn WorkspaceDriver> {
    detect_kind(project).map(driver_for_kind).unwrap_or_else(|| Arc::new(GitWorktreeDriver))
}
```

- [ ] **Step 4: Run the whole crate's tests**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-workspace`
Expected: PASS — including the pre-existing `driver_for_picks_git_for_a_git_project`, `driver_for_picks_jj_when_dot_jj_present_even_with_git`, `driver_for_finds_marker_in_ancestor`, which pin that the refactor changed nothing.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-workspace/src/lib.rs
git commit -m "feat(workspace): add detect_kind and rebuild driver_for on it

driver_for falls back to git when a path is not a repo at all, so it cannot
answer 'is this a repo?'. detect_kind returns None in that case; driver_for
keeps its old contract by mapping None to the git driver.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 2: `validate_workspace_name`

A worktree name becomes **both** a git ref (`clowder/<name>`) and a path component (`.clowder/worktrees/<name>`), so it must be safe as both. Today there is no validation at all. This task adds the function; **M10b wires it into `spawn_agent`** — leave `spawn_agent` alone here.

**Files:**
- Modify: `crates/clowder-workspace/src/lib.rs` (add fn near `detect_kind`; add tests to `mod tests`)

**Interfaces:**
- Consumes: `anyhow::{bail, Result}` (already imported at the top of the file)
- Produces: `pub fn validate_workspace_name(name: &str) -> Result<()>` — called by M10b's `spawn_agent`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests`:

```rust
#[test]
fn validate_workspace_name_accepts_reasonable_names() {
    for ok in ["a", "add-projects", "fix_bug", "v1.2", "M10a", "a-b_c.d"] {
        assert!(validate_workspace_name(ok).is_ok(), "should accept {ok:?}");
    }
}

#[test]
fn validate_workspace_name_rejects_unsafe_names() {
    let too_long = "a".repeat(65);
    let cases: [&str; 11] = [
        "",              // empty
        &too_long,       // > 64 chars
        ".",             // path component
        "..",            // path component
        "a..b",          // traversal fragment
        "x.lock",        // git reserves the .lock suffix
        "my feature",    // space
        "feat/x",        // slash would nest the path AND the ref
        ".hidden",       // leading dot
        "-dash",         // leading dash reads as a flag
        "caf\u{e9}",     // non-ASCII
    ];
    for bad in cases {
        assert!(validate_workspace_name(bad).is_err(), "should reject {bad:?}");
    }
}

#[test]
fn validate_workspace_name_errors_name_the_problem() {
    // The message is user-facing (it surfaces in the app's error banner), so it must say
    // what is wrong, not just that something is.
    let e = validate_workspace_name("my feature").unwrap_err().to_string();
    assert!(e.contains("letters"), "unhelpful message: {e}");
    let e = validate_workspace_name(&"a".repeat(65)).unwrap_err().to_string();
    assert!(e.contains("64"), "unhelpful message: {e}");
}
```

Note the array is typed `[&str; 11]` because `too_long` is a `String` being coerced alongside literals.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-workspace validate_workspace_name`
Expected: FAIL — `cannot find function 'validate_workspace_name' in this scope`

- [ ] **Step 3: Implement**

Add immediately after `detect_kind` in `crates/clowder-workspace/src/lib.rs`:

```rust
/// Validate a worktree/workspace name. The name becomes BOTH a git ref (`clowder/<name>`)
/// and a path component (`.clowder/worktrees/<name>`), so it must be safe as both.
/// Messages are user-facing — they surface directly in the app's error banner.
pub fn validate_workspace_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("worktree name must not be empty");
    }
    let len = name.chars().count();
    if len > 64 {
        bail!("worktree name must be 64 characters or fewer (got {len})");
    }
    if name == "." || name == ".." {
        bail!("worktree name must not be {name:?}");
    }
    if name.contains("..") {
        bail!("worktree name must not contain '..'");
    }
    if name.ends_with(".lock") {
        bail!("worktree name must not end with '.lock' (git reserves that suffix)");
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-'))
    {
        bail!("worktree name must contain only letters, digits, '.', '_' or '-' (found {c:?})");
    }
    // Checked last so the charset message wins for e.g. "  x": a leading '.' or '-' is legal
    // in a path but reads as a hidden file or a CLI flag.
    let first = name.chars().next().unwrap();
    if first == '.' || first == '-' {
        bail!("worktree name must not start with {first:?}");
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-workspace`
Expected: PASS (all tests in the crate)

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-workspace/src/lib.rs
git commit -m "feat(workspace): validate worktree names

A worktree name becomes both a git ref and a path component, so it must be
safe as both. Wired into spawn_agent in M10b.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 3: `JsonStore<T>` and rebuild `Registry` on it

`registry.rs` holds a write-lock around load-modify-write, a unique temp file plus atomic rename, and corrupt-file tolerance. M10b's project store needs precisely the same machinery. Extract it; `upsert` / `remove` / `set_tree` become three specialisations of one `mutate`.

**The existing `registry.rs` tests are the safety net — do not modify them.** If they pass unchanged, the refactor preserved behaviour.

**Files:**
- Create: `crates/clowder-daemon/src/store.rs`
- Modify: `crates/clowder-daemon/src/lib.rs:11` (add `pub mod store;`)
- Modify: `crates/clowder-daemon/src/registry.rs:25-107` (the `Registry` struct and its impl)

**Interfaces:**
- Consumes: nothing from earlier tasks
- Produces:
  - `pub struct JsonStore<T>` with `pub fn new(path: PathBuf) -> Self`, `pub fn load(&self) -> Vec<T>`, `pub fn mutate<R>(&self, f: impl FnOnce(&mut Vec<T>) -> R) -> R`
  - `Registry`'s public API is **unchanged**: `new`, `default_path`, `load`, `upsert`, `remove`, `set_tree`

- [ ] **Step 1: Write the failing tests**

Create `crates/clowder-daemon/src/store.rs` containing only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Item { id: u64, label: String }

    #[test]
    fn missing_file_loads_empty_and_mutate_creates_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("items.json");
        let store: JsonStore<Item> = JsonStore::new(p.clone());
        assert!(store.load().is_empty());
        store.mutate(|all| all.push(Item { id: 1, label: "a".into() }));
        assert!(p.exists(), "mutate must create the file and its parent dir");
        assert_eq!(store.load(), vec![Item { id: 1, label: "a".into() }]);
    }

    #[test]
    fn mutate_returns_the_closures_value() {
        let dir = tempfile::tempdir().unwrap();
        let store: JsonStore<Item> = JsonStore::new(dir.path().join("items.json"));
        store.mutate(|all| all.push(Item { id: 7, label: "x".into() }));
        let found = store.mutate(|all| all.iter().any(|i| i.id == 7));
        assert!(found);
    }

    #[test]
    fn corrupt_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("items.json");
        std::fs::write(&p, b"not json").unwrap();
        let store: JsonStore<Item> = JsonStore::new(p);
        assert!(store.load().is_empty(), "must never panic on a corrupt file");
    }

    #[test]
    fn concurrent_mutates_do_not_lose_records() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<JsonStore<Item>> = Arc::new(JsonStore::new(dir.path().join("items.json")));
        let handles: Vec<_> = (0..16u64)
            .map(|i| {
                let s = Arc::clone(&store);
                std::thread::spawn(move || s.mutate(|all| all.push(Item { id: i, label: "c".into() })))
            })
            .collect();
        for h in handles { h.join().unwrap(); }
        let mut ids: Vec<u64> = store.load().iter().map(|i| i.id).collect();
        ids.sort();
        assert_eq!(ids, (0..16).collect::<Vec<_>>());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Add `pub mod store;` to `crates/clowder-daemon/src/lib.rs` immediately after `pub mod registry;` (line 11), then run:

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon store::`
Expected: FAIL — `cannot find type 'JsonStore' in this scope`

- [ ] **Step 3: Implement `JsonStore<T>`**

Prepend to `crates/clowder-daemon/src/store.rs` (above the test module):

```rust
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// A durable `Vec<T>` held in one JSON file, written atomically.
///
/// All mutation goes through `mutate`, which holds `write_lock` across the whole
/// load-modify-write. The daemon is the sole writer, but its control handlers run as
/// concurrent Tokio tasks (app, CLI, remote client), so two unsynchronized load-append-write
/// cycles would race and drop one update. One `Arc<JsonStore>` is shared per file, and a
/// single-instance flock guarantees one daemon, so this in-process mutex is the whole story.
pub struct JsonStore<T> {
    path: PathBuf,
    write_lock: Mutex<()>,
    /// `fn() -> T` rather than `T` so the store is `Send + Sync` regardless of `T`.
    _marker: PhantomData<fn() -> T>,
}

impl<T: Serialize + DeserializeOwned> JsonStore<T> {
    pub fn new(path: PathBuf) -> Self {
        Self { path, write_lock: Mutex::new(()), _marker: PhantomData }
    }

    /// The current contents. A missing file is empty; an unreadable one warns and is empty.
    /// Never panics — a corrupt state file must not wedge the daemon.
    pub fn load(&self) -> Vec<T> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("state file {} is unreadable ({e}); starting empty", self.path.display());
                Vec::new()
            }),
            Err(_) => Vec::new(), // missing = empty
        }
    }

    /// Load, apply `f`, write back — all under one lock. Returns `f`'s value.
    pub fn mutate<R>(&self, f: impl FnOnce(&mut Vec<T>) -> R) -> R {
        // Recover a poisoned lock rather than wedging the daemon (load/write never panic,
        // so poisoning is not expected).
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = self.load();
        let out = f(&mut all);
        if let Err(e) = self.try_write(&all) {
            tracing::warn!("failed to persist state file {}: {e}", self.path.display());
        }
        out
    }

    fn try_write(&self, all: &[T]) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Unique temp name (pid + counter) so a write never clobbers another writer's temp
        // file before its rename — belt-and-suspenders alongside `write_lock`.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let tmp = self.path.with_extension(format!(
            "json.{}.{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&tmp, serde_json::to_vec_pretty(all)?)?;
        std::fs::rename(&tmp, &self.path)?; // atomic replace
        Ok(())
    }
}
```

- [ ] **Step 4: Run the store tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon store::`
Expected: PASS (4 tests)

- [ ] **Step 5: Rebuild `Registry` on `JsonStore`**

In `crates/clowder-daemon/src/registry.rs`, replace the `Registry` struct and its `impl` block (lines 25-107) with the following. **Leave `AgentRecord` and the entire `mod tests` untouched.**

```rust
/// Durable, restart-surviving list of live agents, stored as one atomically-written JSON file.
pub struct Registry {
    store: crate::store::JsonStore<AgentRecord>,
}

impl Registry {
    pub fn new(path: PathBuf) -> Self {
        Self { store: crate::store::JsonStore::new(path) }
    }

    /// `$CLOWDER_STATE_FILE` › `$XDG_STATE_HOME/clowder/agents.json` › `$HOME/.local/state/clowder/agents.json`.
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("CLOWDER_STATE_FILE") {
            if !p.is_empty() { return PathBuf::from(p); }
        }
        let base = std::env::var("XDG_STATE_HOME").ok().filter(|s| !s.is_empty())
            .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.local/state")))
            .unwrap_or_else(|| "/tmp".to_string());
        PathBuf::from(base).join("clowder").join("agents.json")
    }

    pub fn load(&self) -> Vec<AgentRecord> {
        self.store.load()
    }

    pub fn upsert(&self, rec: AgentRecord) {
        self.store.mutate(|all| {
            all.retain(|r| r.agent_id != rec.agent_id);
            all.push(rec);
        });
    }

    pub fn remove(&self, agent_id: u64) {
        self.store.mutate(|all| all.retain(|r| r.agent_id != agent_id));
    }

    /// Update just one agent's persisted split tree (no-op if the agent isn't in the registry —
    /// e.g. it was landed between a tree change and this call).
    pub fn set_tree(&self, agent_id: u64, tree: Option<PaneTree>) {
        self.store.mutate(|all| {
            if let Some(rec) = all.iter_mut().find(|r| r.agent_id == agent_id) {
                rec.tree = tree;
            }
        });
    }
}
```

Then delete the now-unused imports at the top of `registry.rs`: `anyhow::Result`, `std::sync::atomic::{AtomicU64, Ordering}`, `std::sync::Mutex`. Keep `clowder_proto::PaneTree`, `serde::{Deserialize, Serialize}`, `std::path::PathBuf`.

- [ ] **Step 6: Run the registry tests unchanged — the safety net**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon registry::`
Expected: PASS — all 7 pre-existing tests, notably `concurrent_upserts_do_not_lose_records`, `corrupt_file_loads_empty`, and `set_tree_updates_one_record_and_noops_on_absent`. If any needed editing to pass, the refactor changed behaviour — revert and reconsider.

- [ ] **Step 7: Run the whole workspace**

Run: `source "$HOME/.cargo/env" && cargo test --workspace`
Expected: PASS. Re-run once if a daemon timing test fails (see Global Constraints).

- [ ] **Step 8: Commit**

```bash
git add crates/clowder-daemon/src/store.rs crates/clowder-daemon/src/registry.rs crates/clowder-daemon/src/lib.rs
git commit -m "refactor(daemon): extract JsonStore<T> from Registry

Registry's write-lock, atomic temp+rename write and corrupt-file tolerance are
exactly what M10b's project store needs. upsert/remove/set_tree collapse into
three specialisations of one mutate(). Registry's public API and its tests are
unchanged, which is what pins the behaviour.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 4: `AgentInfo` → `WorktreeInfo` (Rust)

A sidebar row is a **worktree**; the agent is a process inside it. Rename the wire type and give it the two fields the sidebar needs — `name` (the identity, formerly `task`) and `branch`. `project` changes from a **basename** to the **full path**, which nesting requires and which fixes the collision where two repos named `api` merge into one group.

`branch` is already available at the one call site that builds the metadata (`finalize_agent` receives `ws: Workspace`), so it goes into `AgentMeta` rather than costing a second lock in the list call.

**Note:** M10a does **not** canonicalize `project` — that arrives in M10b with `add_project`. Here the path is exactly what the caller passed.

**Files:**
- Modify: `crates/clowder-proto/src/message.rs:21,49-55,105-117`
- Modify: `crates/clowder-proto/src/control.rs:1,33,48` and its `mod tests`
- Modify: `crates/clowder-proto/src/lib.rs:7`
- Modify: `crates/clowder-daemon/src/server.rs:19-22,271-281,676-690,812,835,1435-1440,1150-1157`
- Modify: `crates/clowder-daemon/src/control_json.rs:44,54,173,199-205,241,337`
- Modify: `crates/clowder-daemon/src/remote.rs:149,158`
- Modify: `crates/clowder-client/src/lib.rs:90,96,141`

**Interfaces:**
- Consumes: nothing from earlier tasks
- Produces:
  - `clowder_proto::WorktreeInfo { pane: PaneId, project: String, name: String, branch: String, state: AttentionState }`
  - `ControlRequest::ListWorktrees` (JSON `{"type":"listWorktrees"}`)
  - `ControlEvent::WorktreeList { worktrees: Vec<WorktreeInfo> }` (JSON `{"type":"worktreeList","worktrees":[…]}`)
  - `DaemonToClient::WorktreeList { worktrees: Vec<WorktreeInfo> }`
  - `Daemon::list_worktrees(&self) -> Vec<WorktreeInfo>`

- [ ] **Step 1: Write the failing tests**

Replace `agent_list_roundtrips` in `crates/clowder-proto/src/message.rs` (lines 105-117) with:

```rust
    #[test]
    fn worktree_list_roundtrips() {
        let m = DaemonToClient::WorktreeList {
            worktrees: vec![WorktreeInfo {
                pane: PaneId(2),
                project: "/Users/x/code/clowder".into(),
                name: "task-a".into(),
                branch: "clowder/task-a".into(),
                state: AttentionState::NeedsInput,
            }],
        };
        let bytes = postcard::to_stdvec(&m).unwrap();
        assert_eq!(m, postcard::from_bytes::<DaemonToClient>(&bytes).unwrap());
    }
```

Add to `mod tests` in `crates/clowder-proto/src/control.rs`:

```rust
    #[test]
    fn worktree_list_event_json_shape() {
        let ev = ControlEvent::WorktreeList {
            worktrees: vec![WorktreeInfo {
                pane: PaneId(2),
                project: "/Users/x/code/clowder".into(),
                name: "task-a".into(),
                branch: "clowder/task-a".into(),
                state: AttentionState::Working,
            }],
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains(r#""type":"worktreeList""#), "{s}");
        assert!(s.contains(r#""pane":2"#), "pane must be a bare number: {s}");
        assert!(s.contains(r#""branch":"clowder/task-a""#), "{s}");
        assert_eq!(ev, serde_json::from_str::<ControlEvent>(&s).unwrap());
    }

    #[test]
    fn list_worktrees_request_json_shape() {
        let r = ControlRequest::ListWorktrees;
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"type":"listWorktrees"}"#);
        assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap());
    }
```

Delete the now-obsolete `list_agents_request_json_shape` test in `control.rs` (lines 61-67).

In `crates/clowder-daemon/src/server.rs`, update the assertions of `list_agents_reports_project_task_and_state` (lines 1150-1157) and rename the test:

```rust
        let list = daemon.list_worktrees();
        assert_eq!(list.len(), 1);
        let a = &list[0];
        assert_eq!(a.pane, pane);
        assert_eq!(a.name, "task-a");
        assert_eq!(a.branch, "clowder/task-a");
        // project is now the FULL path, not a basename — two repos with the same dir name
        // must not collapse into one sidebar group.
        assert_eq!(a.project, repo.path().to_string_lossy());
        assert_eq!(a.state, AttentionState::NeedsInput);

        daemon.teardown_agent(pane).unwrap();
        assert!(daemon.list_worktrees().is_empty());
```

Rename the fn on line 1118 to `list_worktrees_reports_project_name_branch_and_state`.

- [ ] **Step 2: Run to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test --workspace`
Expected: FAIL to compile — `cannot find type 'WorktreeInfo'`, `no variant named 'WorktreeList'`

- [ ] **Step 3: Rename in clowder-proto**

`crates/clowder-proto/src/message.rs` — replace the `AgentInfo` struct (lines 49-55):

```rust
/// One worktree under a project. The agent is a process running inside it: `pane` is that
/// process's pane, and `state` is its attention. `pane` is durable — `reconcile` re-spawns
/// each agent under its original id — so it doubles as the worktree's stable identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub pane: PaneId,
    /// Full path to the project root (NOT a basename).
    pub project: String,
    /// The worktree's name — also the suffix of its branch.
    pub name: String,
    /// `clowder/<name>`.
    pub branch: String,
    pub state: AttentionState,
}
```

Line 21: `AgentList { agents: Vec<AgentInfo> },` → `WorktreeList { worktrees: Vec<WorktreeInfo> },`

`crates/clowder-proto/src/control.rs` — line 1 import `AgentInfo` → `WorktreeInfo`; line 33 `ListAgents,` → `ListWorktrees,`; line 48 `AgentList { agents: Vec<AgentInfo> },` → `WorktreeList { worktrees: Vec<WorktreeInfo> },`

`crates/clowder-proto/src/lib.rs` line 7: `AgentInfo` → `WorktreeInfo` in the re-export list.

- [ ] **Step 4: Rename in clowder-daemon**

`server.rs` lines 19-22 — `AgentMeta`:

```rust
struct AgentMeta {
    /// Full path to the project root.
    project: String,
    name: String,
    branch: String,
}
```

`server.rs` lines 271-281 — in `finalize_agent`, replace the `project_name` binding and the `agents` insert. Also rename the fn's `task: &str` parameter (line 268) to `name: &str`; it has exactly one use in the body, in the `AgentMeta` construction below. The two call sites (`spawn_agent` and `reconcile`) pass positionally and need no change.

```rust
        let project = ws.project.to_string_lossy().to_string();
        let branch = ws.branch.clone();
        self.register_pane(id, pane);
        self.workspaces.lock().insert(id, ws);
        self.agents.lock().insert(
            id,
            AgentMeta { project, name: name.to_string(), branch },
        );
        self.set_attention(id, AttentionState::Working);
```

Note `ws` is moved into `workspaces` on the following line, so `project` and `branch` must be read **before** that insert — as written above.

`server.rs` lines 676-690 — rename `list_agents` to `list_worktrees`:

```rust
    pub fn list_worktrees(&self) -> Vec<clowder_proto::WorktreeInfo> {
        let agents = self.agents.lock();
        let attention = self.attention.lock();
        let mut out: Vec<clowder_proto::WorktreeInfo> = agents
            .iter()
            .map(|(pane, meta)| clowder_proto::WorktreeInfo {
                pane: *pane,
                project: meta.project.clone(),
                name: meta.name.clone(),
                branch: meta.branch.clone(),
                state: attention.get(pane).copied().unwrap_or(clowder_proto::AttentionState::Working),
            })
            .collect();
        out.sort_by(|a, b| (a.project.as_str(), a.pane.0).cmp(&(b.project.as_str(), b.pane.0)));
        out
    }
```

Then update every remaining reference. Find them with:

```bash
source "$HOME/.cargo/env" && cargo build --workspace 2>&1 | grep -E "^error" -A3 | head -60
```

The known sites: `server.rs:812,835` (`DaemonToClient::AgentList { agents: self.list_agents() }` → `WorktreeList { worktrees: self.list_worktrees() }`), `server.rs:1435-1440` (test match arm), `control_json.rs:44,54` (`ControlEvent::AgentList` → `WorktreeList`; `ControlRequest::ListAgents` → `ListWorktrees`), and `control_json.rs:173,199-205,241,337` (tests: the `"type":"agentList"` string literals become `"worktreeList"`, `{"type":"listAgents"}` becomes `{"type":"listWorktrees"}`, and `listed[0].task` becomes `listed[0].name`).

`remote.rs:149,158` — update the comment and the assertion string from `agentList` to `worktreeList`.

- [ ] **Step 5: Rename in clowder-client**

`crates/clowder-client/src/lib.rs:141`: `Some(DaemonToClient::AgentList { .. }) => {}` → `Some(DaemonToClient::WorktreeList { .. }) => {}`. Update the comments on lines 90 and 96 to say `WorktreeList`.

- [ ] **Step 6: Run the whole workspace**

Run: `source "$HOME/.cargo/env" && cargo test --workspace`
Expected: PASS. Re-run once if a daemon timing test fails.

- [ ] **Step 7: Verify no stragglers**

Run: `grep -rn "AgentInfo\|AgentList\|ListAgents\|list_agents" crates --include="*.rs"`
Expected: no output.

- [ ] **Step 8: Commit**

```bash
git add crates/
git commit -m "refactor(proto): AgentInfo becomes WorktreeInfo

A sidebar row is a worktree; the agent is a process inside it. The type gains
name (the identity, was 'task') and branch, and project changes from a basename
to a full path — nesting under project rows requires it, and it fixes two repos
with the same dir name collapsing into one group.

pane stays non-optional and doubles as the worktree's durable identity:
reconcile already re-spawns each agent under PaneId(rec.agent_id).

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 5: Mirror the rename in Swift

`macos/Sources/ClowderCore/Models.swift` hand-rolls `Codable` conformances matching the Rust JSON shapes, so the rename must be mirrored by hand. **Swift tests are self-consistent** — they build JSON strings and decode them — so they would keep passing against the old names; the point of this task is that the app talks to the M10a daemon.

**Files:**
- Modify: `macos/Sources/ClowderCore/{Models,AgentStore,AppModel,ControlSession,PaletteSearch}.swift`
- Modify: `macos/Sources/ClowderApp/{ContentView,CommandPaletteView,StatusBarController}.swift`
- Test: `macos/Tests/ClowderCoreTests/{AgentStore,AppModel,AttentionCount,ControlSession,Lifecycle,Models,Navigation,PaletteSearch,UnixSocketConnection}Tests.swift`

**Interfaces:**
- Consumes: the wire shapes produced by Task 4 — `{"type":"listWorktrees"}`, `{"type":"worktreeList","worktrees":[{"pane":1,"project":"/p","name":"t","branch":"clowder/t","state":"Working"}]}`
- Produces: `WorktreeInfo`, `ControlRequest.listWorktrees`, `ControlEvent.worktreeList([WorktreeInfo])`, `AgentStore.worktrees`, `AgentStore.orderedWorktrees`, `AgentStore.worktreesNeedingAttention`, `PendingLifecycle.name` (was `.task`), `paletteResults(query:commands:worktrees:)`

- [ ] **Step 1: Write the failing test**

In `macos/Tests/ClowderCoreTests/ModelsTests.swift`, add:

```swift
    func testWorktreeListDecodesNameAndBranch() throws {
        let json = #"{"type":"worktreeList","worktrees":[{"pane":2,"project":"/Users/x/code/clowder","name":"task-a","branch":"clowder/task-a","state":"NeedsInput"}]}"#
        let ev = try JSONDecoder().decode(ControlEvent.self, from: Data(json.utf8))
        guard case let .worktreeList(list) = ev else {
            return XCTFail("expected worktreeList, got \(ev)")
        }
        XCTAssertEqual(list.count, 1)
        XCTAssertEqual(list[0].pane, 2)
        XCTAssertEqual(list[0].project, "/Users/x/code/clowder")
        XCTAssertEqual(list[0].name, "task-a")
        XCTAssertEqual(list[0].branch, "clowder/task-a")
        XCTAssertEqual(list[0].state, .needsInput)
    }

    func testListWorktreesRequestEncodesTypeOnly() throws {
        let data = try JSONEncoder().encode(ControlRequest.listWorktrees)
        XCTAssertEqual(String(decoding: data, as: UTF8.self), #"{"type":"listWorktrees"}"#)
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter ModelsTests`
Expected: FAIL to compile — `cannot find 'worktreeList' in scope`

- [ ] **Step 3: Update `Models.swift`**

Replace the `AgentInfo` struct (lines 12-26) with:

```swift
/// Mirrors the Rust `WorktreeInfo` (`pane` is a bare number). One worktree under a project;
/// the agent is a process inside it, so `state` is that process's attention.
public struct WorktreeInfo: Codable, Identifiable, Equatable, Sendable {
    public let pane: UInt64
    /// Full path to the project root (NOT a basename).
    public let project: String
    /// The worktree's name — also the suffix of its branch.
    public let name: String
    /// `clowder/<name>`.
    public let branch: String
    public var state: AttentionState
    public var id: UInt64 { pane }

    public init(pane: UInt64, project: String, name: String, branch: String, state: AttentionState) {
        self.pane = pane
        self.project = project
        self.name = name
        self.branch = branch
        self.state = state
    }
}
```

In `ControlRequest`: rename `case listAgents` to `case listWorktrees`, and in `encode(to:)` change the `case .listAgents:` arm to encode `"listWorktrees"`.

In `ControlEvent`: rename `case agentList([AgentInfo])` to `case worktreeList([WorktreeInfo])`; in `CodingKeys` replace `agents` with `worktrees`; in `init(from:)` change the `case "agentList":` arm to `case "worktreeList": self = .worktreeList(try c.decode([WorktreeInfo].self, forKey: .worktrees))`.

- [ ] **Step 4: Update the rest of ClowderCore and ClowderApp**

`AgentStore.swift`: `agents` → `worktrees` (type `[UInt64: WorktreeInfo]`); the `.agentList` case → `.worktreeList`; `byProject`'s tuple label `agents:` → `worktrees:` and its element type → `[WorktreeInfo]`; `orderedAgents` → `orderedWorktrees`; `agentsNeedingAttention` → `worktreesNeedingAttention`. Leave the type name `AgentStore` and the file name alone — the sidebar rework in M10c revisits this file wholesale.

`AppModel.swift`:
- line 113: `.listAgents` → `.listWorktrees` (and the same on `ControlSession.swift:43`)
- lines 162, 170: `store.orderedAgents` → `store.orderedWorktrees`
- line 239: `store.agents[pane]` → `store.worktrees[pane]`, and rebind `let agent` → `let worktree`
- line 240: `task: agent.task` → `name: worktree.name`
- lines 9-16: rename `PendingLifecycle`'s `task` property to `name` (and its `init` label), so the whole domain reads consistently. `ContentView.swift`'s `confirmationDialog` reads `pending.task` twice in its `message:` closure — update both to `pending.name`.

`PaletteSearch.swift`: the `agents: [AgentInfo]` parameter → `worktrees: [WorktreeInfo]`; inside, `a.task` → `a.name`. Keep `PaletteItemKind.agent(pane:)` as-is — it names a palette row, not the domain type, and M10c revisits the palette.

`ContentView.swift`: `exitedPlaceholder(_ agent: AgentInfo)` → `(_ worktree: WorktreeInfo)` with `agent.task` → `worktree.name` in its body; in the sidebar `ForEach`, `agent.task` → `worktree.name`; `model.store.agents.isEmpty` → `model.store.worktrees.isEmpty`; plus the two `pending.task` → `pending.name` sites noted above.

`CommandPaletteView.swift:14` — the argument label changes with `PaletteSearch`'s parameter:

```swift
        paletteResults(query: query,
                       commands: CommandRegistry.all(keymap: keymap),
                       worktrees: model.store.orderedWorktrees)
```

`StatusBarController.swift:95-107` — note line 103 already binds a local `name` for the project; rename it to `projName` so the worktree's `name` doesn't read ambiguously:

```swift
        let needy = appModel.store.worktreesNeedingAttention
        if needy.isEmpty {
            let item = NSMenuItem(title: "No agents need attention", action: nil, keyEquivalent: "")
            item.isEnabled = false
            menu.addItem(item)
        } else {
            for worktree in needy {
                let proj = (worktree.project as NSString).lastPathComponent
                let projName = proj.isEmpty ? worktree.project : proj
                let marker = worktree.state == .needsInput ? "🔴" : "🔵"   // NeedsInput vs Completed
                let item = NSMenuItem(title: "\(marker) \(projName) — \(worktree.name)",
                                      action: #selector(selectAgent(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = worktree.pane
                menu.addItem(item)
            }
        }
```

- [ ] **Step 5: Update the tests**

In each of the 9 test files, replace `AgentInfo(pane:project:task:state:)` constructions with `WorktreeInfo(pane:project:name:branch:state:)` — supply `branch: "clowder/<name>"` to match. Replace `.agentList(` with `.worktreeList(`, `store.agents` with `store.worktrees`, `orderedAgents` with `orderedWorktrees`.

In the three files carrying raw JSON — `LifecycleTests.swift:10`, `UnixSocketConnectionTests.swift:41`, and any in `ControlSessionTests.swift` — update the literals, e.g.:

```swift
#"{"type":"worktreeList","worktrees":[{"pane":1,"project":"/p","name":"fix-bug","branch":"clowder/fix-bug","state":"Completed"}]}"#
```

and `listAgents` → `listWorktrees` in the assertion on `UnixSocketConnectionTests.swift:38`.

- [ ] **Step 6: Run the Swift tests**

Run: `cd macos && swift test`
Expected: PASS (all suites)

- [ ] **Step 7: Verify no stragglers**

Run: `cd macos && grep -rn "AgentInfo\|agentList\|listAgents\|orderedAgents\|agentsNeedingAttention" Sources Tests`
Expected: no output.

- [ ] **Step 8: Commit**

```bash
git add macos/
git commit -m "refactor(app): mirror WorktreeInfo rename in Swift

Models.swift hand-rolls the Codable conformances matching Rust's JSON shapes,
so the rename is mirrored by hand: WorktreeInfo with name + branch, the
listWorktrees request and the worktreeList event.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

## Final verification

- [ ] **Full workspace test:** `source "$HOME/.cargo/env" && cargo test --workspace --locked` — PASS (this is what CI runs)
- [ ] **Swift test:** `cd macos && swift test` — PASS
- [ ] **Behaviour-neutrality check:** `git diff main --stat` should show changes only in `crates/clowder-workspace`, `crates/clowder-proto`, `crates/clowder-daemon`, `crates/clowder-client`, `macos/`, and `docs/superpowers/`. No `Cargo.toml` version bumps, no `VERSION` change.
- [ ] **Manual smoke test** — the rename touches the live protocol, so confirm the daemon and CLI still talk:
  ```bash
  source "$HOME/.cargo/env" && cargo run -p clowder-daemon &
  # in another shell, from a git repo:
  ./target/debug/clowder spawn "$PWD" smoke shell   # prints a pane id
  ./target/debug/clowder attach <pane-id>           # a shell in .clowder/worktrees/smoke
  ```
  Then kill the daemon.
- [ ] **Open the stacked PR:**
  ```bash
  git push -u origin feat/m10a-worktree-model
  gh pr create --base main --title "M10a: worktree model foundations" --body "$(cat <<'EOF'
  First of three stacked PRs implementing M10 (projects + worktrees). Spec:
  `docs/superpowers/specs/2026-08-05-clowder-projects-design.md`.

  Behaviour-neutral foundations:
  - `detect_kind` answers "is this a repo?"; `driver_for` rebuilt on it
  - `validate_workspace_name` (wired into spawn in M10b)
  - `JsonStore<T>` extracted from `Registry`, which is rebuilt on it
  - `AgentInfo` → `WorktreeInfo` (+ `name`, `branch`; `project` becomes a full path)

  Stack: **M10a** → M10b (projects daemon + CLI) → M10c (app).

  🤖 Generated with [Claude Code](https://claude.com/claude-code)

  https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC
  EOF
  )"
  ```

## Notes for M10b

- `validate_workspace_name` and `detect_kind`'s `None` case are both unused after M10a — M10b wires them into `add_project` and `spawn_agent`.
- `project` is **not** canonicalized in M10a. M10b's `add_project` canonicalizes, and `spawn_agent`'s registered-project check must canonicalize its argument the same way or every `/tmp` project fails on macOS (`/tmp` → `/private/tmp`).
- ~~`JsonStore::mutate` always rewrites the file…~~ **This note was wrong and was corrected during execution.** Always-writing silently changed `set_tree`'s no-op case from zero I/O to a full atomic rewrite, which broke the PR's behaviour-neutrality contract. `JsonStore::mutate_if` was added; `set_tree` routes through it. The lesson generalises: the plan framed this as a *performance* question when it was a *behaviour* question.

### Carried forward from the M10a whole-branch review

**Do in M10b:**

- **`JsonStore::mutate` swallows write failures** (`store.rs`) — it warns and returns the closure's value as though it persisted. Faithful to the old `Registry::write`, so correct for M10a, but `add_project` will report success for a project that never reached disk and the user only finds out after a daemon restart. Add a `try_mutate -> Result<R>` for paths answering a user request.
- **`remove` still writes when the id is absent**, now inconsistent with `set_tree`. Pre-existing behaviour, deliberately preserved in a behaviour-neutral PR — decide in M10b whether it should match.
- **Keep `JsonStore` policy-free.** `remove_project`'s "refuse while worktrees exist" check belongs above the store, not inside it.
- **`task` vs `name` is split.** The same string is `task` in `ControlRequest::SpawnAgent`, `AgentRecord.task` and Swift `spawnAgent(task:)`, but `name` in `WorktreeInfo`. Intentional (the spec freezes `SpawnAgent`'s wire shape, and `AgentRecord.task` is what keeps `agents.json` readable across the upgrade) — but M10b should decide whether the spawn request follows or the split becomes permanent.
- **Add a trailing-dot-style audit to any new validation.** M10a's `validate_workspace_name` originally accepted `v1.`, which `git check-ref-format` rejects; the rule list in the spec had the same omission. Check new rules against `git check-ref-format` directly rather than reasoning about them.

**Process lessons that cost real time here:**

- **Grep both spellings when renaming a serde type.** M10a's straggler check grepped Rust identifiers only (`AgentInfo|AgentList|ListAgents|list_agents`) and reported clean, while `crates/clowder-client/src/tofu.rs:152` still sent the camelCase wire literal `{"type":"listAgents"}` — caught only by the final whole-branch review. Grep the identifier **and** its camelCase JSON spelling.
- **Enumerate both directions of a protocol.** The plan listed `DaemonToClient::AgentList` but missed `ClientToDaemon::ListAgents`, a live request handled at three sites.
- **The Rust↔Swift JSON seam has no mechanical guard.** Both sides hand-write the shape and both suites are self-consistent, so the Swift tests would pass unchanged against a diverged Rust encoder. M10b/M10c add `ProjectInfo` plus five requests and five events. A checked-in golden fixture set (`docs/protocol/fixtures/*.json`) that the Rust test asserts its encoder *produces* and the Swift test *decodes* turns divergence into a test failure. Cheap now, expensive after M10c.
- **`swift test` never compiles `Sources/ClowderApp/`** (it needs the vendored libghostty). Edits there are unverified by any local compiler; CI is the first real check. Budget for that, or keep app-layer edits small and re-read them.

**Deferred cosmetics** (harmless, batch whenever): pre-existing `unused imports` warning at `crates/clowder-proto/src/transport.rs:1`; stale Swift test names (`testAgentListReplacesAndClearsRefresh` et al.); `ModelsTests.testDecodeWorktreeList` is nearly subsumed by `testWorktreeListDecodesNameAndBranch`; `control.rs`'s `worktree_list_event_json_shape` never asserts the `"worktrees"` array key, which is exactly the key the Swift decoder depends on.
