# M9b — Full split-layout restore Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** After a daemon restart, each agent reappears in the same split layout — companion shells re-spawned in the same arrangement (structure + axes + ratios).

**Architecture:** Persist each agent's `PaneTree` in its `agents.json` record (immediate on structural change, coalesced behind a periodic flush on ratio drag). On reconcile, bump `next_id` before the restore loop, then rebuild each agent's companions via a pure `rebuild_for_restore` helper that keeps the (stable) agent leaf, spawns a fresh companion shell per other leaf, and preserves axes/ratios best-effort.

**Tech Stack:** Rust (edition 2021), `serde`/`serde_json`, `tokio`, `parking_lot`-style `Mutex` (the crate already uses `parking_lot::Mutex` via `Mutex` alias), `clowder-proto` (`PaneTree`, `SplitId`, `Axis`, `PaneId`).

## Global Constraints

- **Prefix every cargo command** with `source "$HOME/.cargo/env" && ` (rustup is not auto-sourced here).
- **Edition 2021, stable toolchain.** CI runs `cargo test --workspace --locked` and must stay green.
- **No app / wire-protocol change.** `PaneTree` is already the wire type; do not modify `clowder-proto`.
- **Back-compat:** M9a `agents.json` records have no `tree` key — they MUST deserialize to `tree: None` and restore exactly as before (`#[serde(default)]`).
- **All registry writes go through the existing `write_lock`** (M9a) and the atomic temp+rename `try_write`. Do not add a second write path.
- **Restore is best-effort:** a companion that fails to re-spawn collapses only its own leaf; the agent leaf always survives; reconcile never panics.
- **Id-safety:** no restored companion id may collide with an agent id. Bump `next_id` above the max agent id **before** any companion is spawned during reconcile.
- **Restored companions are fresh shells** (no scrollback — that is M9c). The daemon persists layout only, never app-side selection/focus.
- Two pre-existing daemon timing tests (`attached_client_gets_attention_changed`, an exit-under-load test) are flaky under load — they pass on re-run and are NOT regressions.

---

### Task 1: Registry `tree` field + `set_tree` + back-compat

**Files:**
- Modify: `crates/clowder-daemon/src/registry.rs` (struct `AgentRecord`, new method `set_tree`, imports, test helper `rec`)
- Test: same file (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: existing `Registry { path, write_lock }`, `load()`, `write()`, the `write_lock` guard pattern (`self.write_lock.lock().unwrap_or_else(|e| e.into_inner())`).
- Produces:
  - `AgentRecord` gains a public field `pub tree: Option<clowder_proto::PaneTree>` annotated `#[serde(default)]`.
  - `pub fn set_tree(&self, agent_id: u64, tree: Option<clowder_proto::PaneTree>)` — loads, sets that record's `tree` (no-op if the agent id is absent), writes atomically under `write_lock`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `registry.rs`. First update the existing `rec` helper to include the new field, then add three tests:

```rust
// In fn rec(id: u64) -> AgentRecord { ... }, add this field to the struct literal:
//     tree: None,

#[test]
fn record_with_tree_roundtrips() {
    use clowder_proto::{Axis, PaneId, PaneTree, SplitId};
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::new(dir.path().join("agents.json"));
    let tree = PaneTree::Split {
        id: SplitId(1), axis: Axis::Horizontal, ratio: 0.4,
        first: Box::new(PaneTree::Leaf { pane: PaneId(1) }),
        second: Box::new(PaneTree::Leaf { pane: PaneId(2) }),
    };
    reg.upsert(AgentRecord { tree: Some(tree.clone()), ..rec(1) });
    let loaded = reg.load();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].tree, Some(tree));
}

#[test]
fn record_without_tree_key_defaults_to_none() {
    // A record written by M9a has no "tree" key; it must deserialize to None.
    let json = r#"[{"agent_id":1,"project":"/p","task":"t","adapter_id":"claude",
        "worktree_path":"/p/.clowder/worktrees/t","branch":"clowder/t",
        "workspace_kind":"git","cols":80,"rows":24}]"#;
    let recs: Vec<AgentRecord> = serde_json::from_str(json).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].tree, None);
}

#[test]
fn set_tree_updates_one_record_and_noops_on_absent() {
    use clowder_proto::{PaneId, PaneTree};
    let dir = tempfile::tempdir().unwrap();
    let reg = Registry::new(dir.path().join("agents.json"));
    reg.upsert(rec(1));
    reg.upsert(rec(2));
    let t = PaneTree::Leaf { pane: PaneId(1) };
    reg.set_tree(1, Some(t.clone()));
    reg.set_tree(99, Some(PaneTree::Leaf { pane: PaneId(99) })); // absent → no-op, no panic
    let loaded = reg.load();
    assert_eq!(loaded.iter().find(|r| r.agent_id == 1).unwrap().tree, Some(t));
    assert_eq!(loaded.iter().find(|r| r.agent_id == 2).unwrap().tree, None);
    assert_eq!(loaded.len(), 2);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon registry:: 2>&1 | tail -20`
Expected: compile error (`AgentRecord` has no field `tree`; no method `set_tree`).

- [ ] **Step 3: Implement**

At the top of `registry.rs`, add to the imports:

```rust
use clowder_proto::PaneTree;
```

Add the field to `AgentRecord` (after `rows`):

```rust
    pub cols: u16,
    pub rows: u16,
    /// The agent's split layout at last change; `None` = a single agent leaf (also how M9a
    /// records — written before this field existed — deserialize). Rebuilt on reconcile (M9b).
    #[serde(default)]
    pub tree: Option<PaneTree>,
```

Add the method inside `impl Registry` (next to `remove`):

```rust
/// Update just one agent's persisted split tree (no-op if the agent isn't in the registry —
/// e.g. it was landed between a tree change and this call). Atomic, under `write_lock`.
pub fn set_tree(&self, agent_id: u64, tree: Option<PaneTree>) {
    let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = self.load();
    if let Some(rec) = all.iter_mut().find(|r| r.agent_id == agent_id) {
        rec.tree = tree;
        self.write(&all);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon registry:: 2>&1 | tail -20`
Expected: PASS (all registry tests, including the pre-existing `concurrent_upserts_do_not_lose_records`).

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/src/registry.rs
git commit -m "feat(daemon): persist agent split tree in the registry record"
```

---

### Task 2: `rebuild_for_restore` — the pure rebuild helper

**Files:**
- Modify: `crates/clowder-daemon/src/split_tree.rs` (new pub(crate) fn + private recursion + tests)

**Interfaces:**
- Consumes: `clowder_proto::{PaneTree, PaneId, SplitId, Axis}` (already imported in this file).
- Produces:
  - `pub(crate) fn rebuild_for_restore(tree: &PaneTree, agent: PaneId, spawn_companion: &mut dyn FnMut() -> Option<PaneId>, alloc_split: &mut dyn FnMut() -> SplitId) -> (PaneTree, Vec<PaneId>)`
  - Keeps the leaf whose id `== agent`; for every other leaf calls `spawn_companion()` and substitutes the returned id (`None` → collapse that leaf into its sibling); regenerates each `Split` id via `alloc_split()`; preserves `axis` + `ratio`. Returns the rebuilt tree and the new companion ids in creation order. Always yields at least `Leaf { pane: agent }`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `split_tree.rs`:

```rust
// A scripted spawner: returns the next id from `ids`, or None once exhausted / where scripted.
fn spawner(ids: Vec<Option<u64>>) -> impl FnMut() -> Option<PaneId> {
    let mut it = ids.into_iter();
    move || it.next().flatten().map(PaneId)
}
fn splitter() -> impl FnMut() -> SplitId {
    let mut n = 1000u64;
    move || { n += 1; SplitId(n) }
}

#[test]
fn rebuild_agent_only_leaf_is_single_leaf() {
    let t = leaf(1);
    let mut sp = spawner(vec![]);
    let mut al = splitter();
    let (out, comps) = rebuild_for_restore(&t, PaneId(1), &mut sp, &mut al);
    assert_eq!(out, leaf(1));
    assert!(comps.is_empty());
}

#[test]
fn rebuild_substitutes_companion_and_preserves_axis_ratio() {
    // agent=1, one companion leaf (old id 77) under a horizontal split at ratio 0.3.
    let t = PaneTree::Split {
        id: SplitId(5), axis: Axis::Horizontal, ratio: 0.3,
        first: Box::new(leaf(1)), second: Box::new(leaf(77)),
    };
    let mut sp = spawner(vec![Some(500)]);
    let mut al = splitter();
    let (out, comps) = rebuild_for_restore(&t, PaneId(1), &mut sp, &mut al);
    assert_eq!(comps, vec![PaneId(500)]);
    match out {
        PaneTree::Split { id, axis, ratio, first, second } => {
            assert_eq!(id, SplitId(1001));            // regenerated, not the old 5
            assert_eq!(axis, Axis::Horizontal);
            assert_eq!(ratio, 0.3);
            assert_eq!(*first, leaf(1));              // agent leaf preserved
            assert_eq!(*second, leaf(500));           // companion substituted
        }
        _ => panic!("expected split"),
    }
}

#[test]
fn rebuild_failed_companion_collapses_to_agent() {
    let t = PaneTree::Split {
        id: SplitId(5), axis: Axis::Vertical, ratio: 0.5,
        first: Box::new(leaf(1)), second: Box::new(leaf(77)),
    };
    let mut sp = spawner(vec![None]);   // the companion spawn fails
    let mut al = splitter();
    let (out, comps) = rebuild_for_restore(&t, PaneId(1), &mut sp, &mut al);
    assert_eq!(out, leaf(1));            // collapsed to the surviving agent leaf
    assert!(comps.is_empty());
}

#[test]
fn rebuild_nested_recurses_and_one_failure_collapses_inner() {
    // agent=1 ; right side is a split of two companions (88, 99); 88 fails, 99 succeeds.
    let t = PaneTree::Split {
        id: SplitId(5), axis: Axis::Horizontal, ratio: 0.6,
        first: Box::new(leaf(1)),
        second: Box::new(PaneTree::Split {
            id: SplitId(6), axis: Axis::Vertical, ratio: 0.2,
            first: Box::new(leaf(88)), second: Box::new(leaf(99)),
        }),
    };
    let mut sp = spawner(vec![None, Some(501)]);  // 88 → None, 99 → 501
    let mut al = splitter();
    let (out, comps) = rebuild_for_restore(&t, PaneId(1), &mut sp, &mut al);
    assert_eq!(comps, vec![PaneId(501)]);
    // inner split collapsed to leaf(501); outer split keeps agent + that leaf.
    match out {
        PaneTree::Split { axis, ratio, first, second, .. } => {
            assert_eq!(axis, Axis::Horizontal);
            assert_eq!(ratio, 0.6);
            assert_eq!(*first, leaf(1));
            assert_eq!(*second, leaf(501));
        }
        _ => panic!("expected split"),
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon split_tree:: 2>&1 | tail -20`
Expected: compile error (`rebuild_for_restore` not found).

- [ ] **Step 3: Implement**

Add to `split_tree.rs` (after `set_ratio`):

```rust
/// Rebuild a persisted tree for restore: keep the agent leaf (its id is stable across restart),
/// spawn a fresh companion for every other leaf (substituting the new id), regenerate split ids,
/// and preserve axis + ratio. Best-effort: if `spawn_companion` returns None, that leaf collapses
/// into its sibling. Returns the rebuilt tree and the new companion ids in creation order; always
/// yields at least `Leaf { pane: agent }`.
pub(crate) fn rebuild_for_restore(
    tree: &PaneTree,
    agent: PaneId,
    spawn_companion: &mut dyn FnMut() -> Option<PaneId>,
    alloc_split: &mut dyn FnMut() -> SplitId,
) -> (PaneTree, Vec<PaneId>) {
    rebuild(tree, agent, spawn_companion, alloc_split)
        .unwrap_or_else(|| (PaneTree::Leaf { pane: agent }, Vec::new()))
}

/// Recursion for `rebuild_for_restore`. `None` = this subtree produced no panes (fully collapsed);
/// the agent leaf can never collapse, so the top-level call always yields `Some`.
fn rebuild(
    node: &PaneTree,
    agent: PaneId,
    spawn_companion: &mut dyn FnMut() -> Option<PaneId>,
    alloc_split: &mut dyn FnMut() -> SplitId,
) -> Option<(PaneTree, Vec<PaneId>)> {
    match node {
        PaneTree::Leaf { pane } if *pane == agent => {
            Some((PaneTree::Leaf { pane: agent }, Vec::new()))
        }
        PaneTree::Leaf { .. } => {
            let id = spawn_companion()?; // None → collapse
            Some((PaneTree::Leaf { pane: id }, vec![id]))
        }
        PaneTree::Split { axis, ratio, first, second, .. } => {
            let f = rebuild(first, agent, spawn_companion, alloc_split);
            let s = rebuild(second, agent, spawn_companion, alloc_split);
            match (f, s) {
                (Some((ft, mut fc)), Some((st, sc))) => {
                    fc.extend(sc);
                    Some((
                        PaneTree::Split {
                            id: alloc_split(),
                            axis: *axis,
                            ratio: *ratio,
                            first: Box::new(ft),
                            second: Box::new(st),
                        },
                        fc,
                    ))
                }
                // one side collapsed → promote the surviving side (drop the divider)
                (Some(x), None) | (None, Some(x)) => Some(x),
                (None, None) => None,
            }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon split_tree:: 2>&1 | tail -20`
Expected: PASS (all split_tree tests).

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/src/split_tree.rs
git commit -m "feat(daemon): rebuild_for_restore — reshape a persisted tree with fresh companions"
```

---

### Task 3: Persist the tree immediately on structural changes

**Files:**
- Modify: `crates/clowder-daemon/src/server.rs` (new private `persist_tree`; call it in `split_pane`, `close_pane`, `reap_companion`)
- Test: same file (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `self.trees: Arc<Mutex<HashMap<PaneId, PaneTree>>>`, `self.registry: Arc<Registry>`, `Registry::set_tree` (Task 1).
- Produces:
  - `fn persist_tree(&self, agent: PaneId)` — reads the agent's current tree; persists `None` if it is a bare `Leaf { pane: agent }`, else `Some(tree)`; calls `self.registry.set_tree(agent.0, opt)`.
  - `split_pane`, `close_pane` (companion branch), and `reap_companion` call `persist_tree(agent)` after mutating the tree (right after their existing `broadcast_tree(agent)`).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `server.rs` (model the git-repo + `CLOWDER_STATE_FILE` setup on `reconcile_respawns_recorded_agents_and_prunes_missing`):

```rust
#[tokio::test]
async fn split_and_close_persist_the_tree() {
    use crate::{FakeNotifier, SyntheticAdapter};
    use std::process::Command as PCommand;
    use clowder_proto::SplitDirection;

    let repo = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t.test"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "init"]);

    let statedir = tempfile::tempdir().unwrap();
    let state_path = statedir.path().join("agents.json");
    std::env::set_var("CLOWDER_STATE_FILE", &state_path);

    let daemon = Arc::new(Daemon::new_with(
        Arc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-persist.sock"),
    ));
    let adapter = SyntheticAdapter {
        command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()],
            cwd: None, env: vec![],
        },
    };
    let id = daemon.spawn_agent(repo.path(), &adapter, "demo").unwrap();

    // After a split, the record's tree is a 2-leaf split.
    let companion = daemon.split_pane(id, SplitDirection::Right).unwrap();
    let recs = crate::registry::Registry::new(state_path.clone()).load();
    let tree = recs.iter().find(|r| r.agent_id == id.0).unwrap().tree.clone();
    assert!(matches!(tree, Some(clowder_proto::PaneTree::Split { .. })), "split persisted: {tree:?}");
    assert_eq!(crate::split_tree::leaves(tree.as_ref().unwrap()).len(), 2);

    // After closing the companion, the tree collapses back and is persisted as None (bare leaf).
    daemon.close_pane(companion).unwrap();
    let recs = crate::registry::Registry::new(state_path.clone()).load();
    assert_eq!(recs.iter().find(|r| r.agent_id == id.0).unwrap().tree, None);

    daemon.shutdown();
    std::env::remove_var("CLOWDER_STATE_FILE");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon split_and_close_persist_the_tree 2>&1 | tail -20`
Expected: FAIL — the split record's `tree` is `None` (persistence not wired yet).

- [ ] **Step 3: Implement**

Add the helper inside `impl Daemon` (near `broadcast_tree`):

```rust
/// Persist the agent's current split tree to its registry record. A bare agent leaf is stored as
/// `None` (keeps records small); anything with companions is stored literally. Called on every
/// structural tree change (split/close/reap); ratio drags persist via the coalesced flush instead.
fn persist_tree(&self, agent: PaneId) {
    let opt = match self.trees.lock().get(&agent) {
        Some(PaneTree::Leaf { pane }) if *pane == agent => None,
        Some(tree) => Some(tree.clone()),
        None => None,
    };
    self.registry.set_tree(agent.0, opt);
}
```

In `split_pane`, right after `self.broadcast_tree(agent);` (before the reap-watcher block):

```rust
        self.broadcast_tree(agent);
        self.persist_tree(agent);
```

In `close_pane`, in the companion branch, right after `self.broadcast_tree(agent);` (before `Ok(Some(agent))`):

```rust
        self.broadcast_tree(agent);
        self.persist_tree(agent);
        Ok(Some(agent))
```

In `reap_companion`, right after `self.broadcast_tree(agent);`:

```rust
        self.broadcast_tree(agent);
        self.persist_tree(agent);
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon split_and_close_persist_the_tree 2>&1 | tail -20`
Then the whole crate: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon 2>&1 | tail -20`
Expected: PASS (re-run once if `attached_client_gets_attention_changed` flakes).

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/src/server.rs
git commit -m "feat(daemon): persist split tree on structural changes (split/close/reap)"
```

---

### Task 4: Coalesced ratio persistence — dirty set + periodic flush

**Files:**
- Modify: `crates/clowder-daemon/src/server.rs` (new field `layout_dirty`; `mark_layout_dirty`, `flush_dirty_layouts`, `spawn_layout_flusher`; a `LAYOUT_FLUSH_INTERVAL` const; call `mark_layout_dirty` in `set_split_ratio`)
- Modify: `crates/clowder-daemon/src/main.rs` (spawn the flusher after `reconcile`)
- Test: `server.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `self.trees`, `persist_tree` (Task 3), `self.registry`.
- Produces:
  - Field `layout_dirty: Arc<Mutex<std::collections::HashSet<PaneId>>>`, initialized in BOTH `new_with` (to an empty set).
  - `fn mark_layout_dirty(&self, agent: PaneId)` — inserts into the set.
  - `pub fn flush_dirty_layouts(&self)` — drains the set; for each agent still in `self.trees`, calls `persist_tree(agent)`.
  - `pub fn spawn_layout_flusher(self: &Arc<Self>) -> tokio::task::JoinHandle<()>` — a loop ticking every `LAYOUT_FLUSH_INTERVAL` that calls `flush_dirty_layouts`.
  - `set_split_ratio` calls `self.mark_layout_dirty(agent)` after `broadcast_tree`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `server.rs`:

```rust
#[tokio::test]
async fn ratio_change_is_persisted_by_flush() {
    use crate::{FakeNotifier, SyntheticAdapter};
    use std::process::Command as PCommand;
    use clowder_proto::{PaneTree, SplitDirection};

    let repo = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t.test"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "init"]);

    let statedir = tempfile::tempdir().unwrap();
    let state_path = statedir.path().join("agents.json");
    std::env::set_var("CLOWDER_STATE_FILE", &state_path);

    let daemon = Arc::new(Daemon::new_with(
        Arc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-ratio.sock"),
    ));
    let adapter = SyntheticAdapter {
        command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()],
            cwd: None, env: vec![],
        },
    };
    let id = daemon.spawn_agent(repo.path(), &adapter, "demo").unwrap();
    daemon.split_pane(id, SplitDirection::Right).unwrap();

    // Find the split id, move its divider, then flush explicitly (no wall-clock dependence).
    let sid = match daemon.split_tree_of(id).unwrap() {
        PaneTree::Split { id, .. } => id,
        _ => panic!("expected split"),
    };
    daemon.set_split_ratio(sid, 0.3).unwrap();
    daemon.flush_dirty_layouts();

    let recs = crate::registry::Registry::new(state_path.clone()).load();
    let tree = recs.iter().find(|r| r.agent_id == id.0).unwrap().tree.clone().unwrap();
    match tree {
        PaneTree::Split { ratio, .. } => assert!((ratio - 0.3).abs() < 1e-6, "ratio persisted: {ratio}"),
        _ => panic!("expected split"),
    }

    daemon.shutdown();
    std::env::remove_var("CLOWDER_STATE_FILE");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon ratio_change_is_persisted_by_flush 2>&1 | tail -20`
Expected: compile error (`flush_dirty_layouts` not found).

- [ ] **Step 3: Implement**

Near the top of `server.rs` (module level, after the `use` block), add:

```rust
/// How often the coalesced layout flusher persists agents whose divider ratios changed.
const LAYOUT_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(750);
```

Add the field to `struct Daemon` (after `registry`):

```rust
    registry: Arc<crate::registry::Registry>,
    /// Agents whose ratios changed since the last flush; drained by the periodic layout flusher.
    layout_dirty: Arc<Mutex<std::collections::HashSet<PaneId>>>,
```

Initialize it in `new_with` (after the `registry: ...` line in the struct literal):

```rust
            registry: Arc::new(crate::registry::Registry::new(crate::registry::Registry::default_path())),
            layout_dirty: Arc::new(Mutex::new(std::collections::HashSet::new())),
```

Add the methods inside `impl Daemon` (near `persist_tree`):

```rust
/// Mark an agent's layout dirty (a ratio drag). Coalesced: the periodic flusher persists it.
fn mark_layout_dirty(&self, agent: PaneId) {
    self.layout_dirty.lock().insert(agent);
}

/// Persist every dirty agent's current tree, then clear the dirty set. Skips agents no longer
/// live (landed/discarded since being marked). Safe to call directly (used by tests + the flusher).
pub fn flush_dirty_layouts(&self) {
    let dirty: Vec<PaneId> = self.layout_dirty.lock().drain().collect();
    for agent in dirty {
        if self.trees.lock().contains_key(&agent) {
            self.persist_tree(agent);
        }
    }
}

/// Spawn the background task that flushes coalesced ratio changes every `LAYOUT_FLUSH_INTERVAL`.
/// Runs for the daemon's lifetime.
pub fn spawn_layout_flusher(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
    let me = Arc::clone(self);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(LAYOUT_FLUSH_INTERVAL);
        loop {
            ticker.tick().await;
            me.flush_dirty_layouts();
        }
    })
}
```

In `set_split_ratio`, after `self.broadcast_tree(agent);` (before `Ok(agent)`):

```rust
        self.broadcast_tree(agent);
        self.mark_layout_dirty(agent);
        Ok(agent)
```

In `main.rs`, after `daemon.reconcile();` (line ~53):

```rust
    daemon.reconcile();

    // Coalesced layout persistence: ratio drags mark the agent dirty; this task flushes them.
    let _layout_flusher = daemon.spawn_layout_flusher();
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon ratio_change_is_persisted_by_flush 2>&1 | tail -20`
Then confirm main compiles: `source "$HOME/.cargo/env" && cargo build -p clowder-daemon 2>&1 | tail -5`
Expected: PASS + clean build.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/src/server.rs crates/clowder-daemon/src/main.rs
git commit -m "feat(daemon): coalesced ratio persistence via a periodic layout flusher"
```

---

### Task 5: Reconcile — bump `next_id` early + restore the layout

**Files:**
- Modify: `crates/clowder-daemon/src/server.rs` (`reconcile` reorder + per-record `restore_layout` call; new `restore_layout` method)
- Test: same file (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `rebuild_for_restore` (Task 2), `companion_command`, `spawn_pane`, `alloc_split_id`, `get`, `reap_companion`, `broadcast_tree`, `bump_next_id_above`, `AgentRecord.tree` (Task 1).
- Produces:
  - `reconcile` computes `max_id` over all records and calls `bump_next_id_above(max_id)` BEFORE the restore loop; after each successful `finalize_agent`, if the record has a non-trivial tree, calls `restore_layout(id, tree, worktree_path)`.
  - `fn restore_layout(self: &Arc<Self>, agent: PaneId, tree: PaneTree, cwd: std::path::PathBuf)` — rebuilds companions via `rebuild_for_restore`, registers `owner` + a reap watcher per companion, installs the rebuilt tree, broadcasts. No-op for a bare agent leaf.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `server.rs`:

```rust
#[tokio::test]
async fn reconcile_restores_split_layout() {
    use crate::{FakeNotifier, SyntheticAdapter};
    use std::process::Command as PCommand;
    use clowder_proto::{PaneTree, SplitDirection};

    let repo = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t.test"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "init"]);

    let statedir = tempfile::tempdir().unwrap();
    std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));

    let d1 = Arc::new(Daemon::new_with(
        Arc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-restore1.sock"),
    ));
    let adapter = SyntheticAdapter {
        command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()],
            cwd: None, env: vec![],
        },
    };
    let id = d1.spawn_agent(repo.path(), &adapter, "demo").unwrap();
    d1.split_pane(id, SplitDirection::Right).unwrap();
    // set + flush a non-default ratio so we can assert it round-trips.
    let sid = match d1.split_tree_of(id).unwrap() { PaneTree::Split { id, .. } => id, _ => panic!() };
    d1.set_split_ratio(sid, 0.3).unwrap();
    d1.flush_dirty_layouts();

    // Fresh daemon over the same state file → reconcile rebuilds the layout.
    let d2 = Arc::new(Daemon::new_with(
        Arc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-restore2.sock"),
    ));
    d2.reconcile();

    let tree = d2.split_tree_of(id).expect("agent tree restored");
    let ls = crate::split_tree::leaves(&tree);
    assert_eq!(ls.len(), 2, "two leaves restored");
    assert!(ls.contains(&id), "agent leaf id preserved");
    match tree {
        PaneTree::Split { ratio, first, .. } => {
            assert!((ratio - 0.3).abs() < 1e-6, "ratio restored: {ratio}");
            assert_eq!(*first, PaneTree::Leaf { pane: id }, "agent is the first leaf");
        }
        _ => panic!("expected split"),
    }
    d2.shutdown();
    std::env::remove_var("CLOWDER_STATE_FILE");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon reconcile_restores_split_layout 2>&1 | tail -20`
Expected: FAIL — `d2.split_tree_of(id)` is a bare `Leaf` (single pane), so `leaves().len()` is 1.

- [ ] **Step 3: Implement**

Rewrite `reconcile` (`server.rs`) so the bump happens first and each success restores its layout. Replace the current body:

```rust
pub fn reconcile(self: &Arc<Self>) {
    let records = self.registry.load();
    // Bump BEFORE restoring: companion `alloc_id()`s during layout restore must not collide with
    // a not-yet-restored agent's fixed id. (Agents re-spawn under `PaneId(rec.agent_id)`.)
    let max_id = records.iter().map(|r| r.agent_id).max().unwrap_or(0);
    self.bump_next_id_above(max_id);
    for rec in records {
        let id = PaneId(rec.agent_id);
        if !rec.worktree_path.exists() {
            tracing::warn!("agent {} worktree {} is gone; pruning", rec.agent_id, rec.worktree_path.display());
            self.registry.remove(rec.agent_id);
            continue;
        }
        let Some(kind) = clowder_workspace::WorkspaceKind::from_str(&rec.workspace_kind) else {
            tracing::warn!("agent {} has unknown workspace kind {:?}; pruning", rec.agent_id, rec.workspace_kind);
            self.registry.remove(rec.agent_id);
            continue;
        };
        let Some(adapter) = crate::agent::build_adapter(&rec.adapter_id) else {
            tracing::warn!("agent {} has unknown adapter {:?}; pruning", rec.agent_id, rec.adapter_id);
            self.registry.remove(rec.agent_id);
            continue;
        };
        let ws = Workspace {
            path: rec.worktree_path.clone(),
            branch: rec.branch.clone(),
            project: rec.project.clone(),
            kind,
        };
        let spawn = (|| -> Result<Pane> {
            adapter.provision_hooks(&ws.path, id, &self.hook_sock)?;
            let mut cmd = adapter.resume_command(&ws.path);
            cmd.cwd = Some(ws.path.clone());
            cmd.env.push(("CLOWDER_AGENT_ID".into(), id.0.to_string()));
            cmd.env.push(("CLOWDER_HOOK_SOCK".into(), self.hook_sock.to_string_lossy().to_string()));
            Pane::spawn(id, cmd, rec.cols, rec.rows, self.backlog_cap)
        })();
        match spawn {
            Ok(pane) => {
                let restore_cwd = ws.path.clone();
                self.finalize_agent(id, pane, ws, &rec.task, adapter.as_ref());
                if let Some(tree) = rec.tree.clone() {
                    self.restore_layout(id, tree, restore_cwd);
                }
            }
            Err(e) => {
                tracing::warn!("resume agent {} failed: {e}; pruning", rec.agent_id);
                self.registry.remove(rec.agent_id);
            }
        }
    }
}
```

> Note: keep the existing `build_adapter` warn message text as it was (`rec.adapter_id` for the adapter value) — the line above mirrors the current code; do not introduce a different message.

Add the `restore_layout` method (near `split_pane`):

```rust
/// Rebuild an agent's companion layout on reconcile: spawn a fresh shell per companion leaf,
/// wire owner + reap watchers, install the rebuilt tree, and broadcast. Best-effort — a companion
/// that fails to spawn collapses only its leaf; a bare agent leaf is a no-op (finalize already set
/// the single-leaf tree).
fn restore_layout(self: &Arc<Self>, agent: PaneId, tree: PaneTree, cwd: std::path::PathBuf) {
    if matches!(&tree, PaneTree::Leaf { pane } if *pane == agent) {
        return;
    }
    let shell = self.shell.clone();
    let (cols, rows) = (self.default_cols, self.default_rows);
    let mut spawn_companion = || -> Option<PaneId> {
        self.spawn_pane(companion_command(shell.clone(), cwd.clone()), cols, rows).ok()
    };
    let mut alloc_split = || self.alloc_split_id();
    let (rebuilt, companions) =
        crate::split_tree::rebuild_for_restore(&tree, agent, &mut spawn_companion, &mut alloc_split);

    for c in companions {
        self.owner.lock().insert(c, agent);
        if let Some(pane_arc) = self.get(c) {
            let me = Arc::clone(self);
            let handle = tokio::spawn(async move {
                pane_arc.wait_exit().await;
                me.reap_companion(c);
            });
            self.companion_watchers.lock().insert(c, handle);
        }
    }
    self.trees.lock().insert(agent, rebuilt);
    self.broadcast_tree(agent);
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon reconcile 2>&1 | tail -30`
Expected: PASS (the new test + the M9a `reconcile_respawns_recorded_agents_and_prunes_missing`).

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/src/server.rs
git commit -m "feat(daemon): restore split layout on reconcile; bump next_id before restore"
```

---

### Task 6: Integration — id-collision safety + back-compat

**Files:**
- Test: `crates/clowder-daemon/src/server.rs` (`#[cfg(test)] mod tests`) — no production changes; this task proves the id-safety reorder and M9a back-compat end-to-end.

**Interfaces:**
- Consumes: everything above (`spawn_agent`, `split_pane`, `reconcile`, `restore_layout`, `list_agents`, `split_tree_of`).
- Produces: two regression tests.

- [ ] **Step 1: Write the failing/guarding tests**

Add to the `tests` module in `server.rs`:

```rust
#[tokio::test]
async fn reconcile_restored_companion_ids_never_collide_with_agents() {
    use crate::{FakeNotifier, SyntheticAdapter};
    use std::process::Command as PCommand;
    use clowder_proto::SplitDirection;

    let repo = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t.test"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "init"]);

    let statedir = tempfile::tempdir().unwrap();
    std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));

    let d1 = Arc::new(Daemon::new_with(
        Arc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-collide1.sock"),
    ));
    let adapter = SyntheticAdapter {
        command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()],
            cwd: None, env: vec![],
        },
    };
    // Agent A (low id) with a companion, then agent B (higher id).
    let a = d1.spawn_agent(repo.path(), &adapter, "aaa").unwrap();
    d1.split_pane(a, SplitDirection::Right).unwrap();
    let b = d1.spawn_agent(repo.path(), &adapter, "bbb").unwrap();

    // Fresh daemon reconciles A (with layout) then B. Without the early next_id bump, A's
    // restored companion could grab B's id.
    let d2 = Arc::new(Daemon::new_with(
        Arc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-collide2.sock"),
    ));
    d2.reconcile();

    // Both agents came back under their original ids.
    let ids: std::collections::HashSet<_> = d2.list_agents().iter().map(|x| x.pane).collect();
    assert!(ids.contains(&a) && ids.contains(&b), "both agents restored: {ids:?}");

    // A's companion leaf id differs from BOTH agent ids.
    let tree = d2.split_tree_of(a).unwrap();
    let comp = crate::split_tree::leaves(&tree).into_iter().find(|p| *p != a).unwrap();
    assert_ne!(comp, a, "companion != agent A");
    assert_ne!(comp, b, "companion must not collide with agent B");

    d2.shutdown();
    std::env::remove_var("CLOWDER_STATE_FILE");
}

#[tokio::test]
async fn reconcile_m9a_record_without_tree_restores_single_leaf() {
    use crate::{FakeNotifier, SyntheticAdapter};
    use std::process::Command as PCommand;

    let repo = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        assert!(PCommand::new("git").arg("-C").arg(repo.path()).args(args).status().unwrap().success());
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@t.test"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(repo.path().join("README.md"), b"hi").unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "init"]);

    let statedir = tempfile::tempdir().unwrap();
    std::env::set_var("CLOWDER_STATE_FILE", statedir.path().join("agents.json"));

    let d1 = Arc::new(Daemon::new_with(
        Arc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-nolt1.sock"),
    ));
    let adapter = SyntheticAdapter {
        command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()],
            cwd: None, env: vec![],
        },
    };
    // A plain agent, never split → its record's tree is None (the M9a shape).
    let id = d1.spawn_agent(repo.path(), &adapter, "demo").unwrap();

    let d2 = Arc::new(Daemon::new_with(
        Arc::new(FakeNotifier::new()),
        std::path::PathBuf::from("/tmp/unused-nolt2.sock"),
    ));
    d2.reconcile();
    assert_eq!(d2.split_tree_of(id), Some(clowder_proto::PaneTree::Leaf { pane: id }));
    d2.shutdown();
    std::env::remove_var("CLOWDER_STATE_FILE");
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon reconcile 2>&1 | tail -30`
Expected: PASS. (These are guarding tests over Task 5's production code — they should pass immediately. If `reconcile_restored_companion_ids_never_collide_with_agents` fails, the `bump_next_id_above` reorder from Task 5 is wrong.)

- [ ] **Step 3: Full-suite check**

Run: `source "$HOME/.cargo/env" && cargo test --workspace --locked 2>&1 | tail -30`
Expected: green (re-run once if `attached_client_gets_attention_changed` or the exit-under-load test flakes — those are the known pre-existing flakes).

- [ ] **Step 4: Clippy**

Run: `source "$HOME/.cargo/env" && cargo clippy -p clowder-daemon --all-targets 2>&1 | grep -E "warning:|error" | grep -i "registry\|split_tree\|server\|main" | head`
Expected: no NEW warnings attributable to M9b files (pre-existing workspace/proto/vt warnings are out of scope).

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/src/server.rs
git commit -m "test(daemon): id-collision safety + M9a-record back-compat on reconcile"
```

---

## Notes for the implementer

- **`Mutex` in `server.rs`/`registry.rs` is `parking_lot`-style** (`.lock()` returns the guard directly, no `.unwrap()`), EXCEPT `registry.rs`'s `write_lock`, which is a `std::sync::Mutex` and uses `.lock().unwrap_or_else(|e| e.into_inner())`. Follow each file's existing pattern.
- **Tests set the process-global `CLOWDER_STATE_FILE`.** Always `remove_var` at the end (as the existing tests do); each test uses its own `tempdir` so they don't clobber one another's files.
- **`SyntheticAdapter`** is the test adapter; a real `shell` agent persists `adapter_id: "synthetic"` (M9a). Companions are plain shells regardless of adapter.
- The flusher task is fire-and-forget for the daemon's lifetime; `main.rs` binds it to `_layout_flusher` so it isn't dropped/aborted immediately.
