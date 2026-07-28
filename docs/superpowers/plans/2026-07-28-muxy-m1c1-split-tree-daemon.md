# muxy M1c-1 — Split-Tree Daemon Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The daemon-side of companion split panes — a per-agent binary split tree the daemon maintains: split a pane (spawning a companion shell in the agent's worktree), close a companion (collapsing the tree), resize a divider, and answer/broadcast the tree; agent teardown cascades to companions. No UI.

**Architecture:** New protocol types in `muxy-proto` (a recursive `PaneTree` + four requests + one event). A pure, unit-tested tree-algebra module in `muxy-daemon` (`split_leaf`/`remove_leaf`/`set_ratio`/`leaves`). The `Daemon` gains `trees`/`owner` state + a `split_tx` broadcast and the split/close/ratio operations, plus a teardown cascade. `control_json` handles the new requests and streams `SplitTreeChanged`.

**Tech Stack:** Rust, tokio, serde/serde_json, portable-pty (existing).

## Global Constraints

- **Match the existing wire conventions** (`crates/muxy-proto/src/control.rs`): `#[serde(tag="type", rename_all="camelCase")]` on request/event enums; `PaneId`/`SplitId` serialize as **bare numbers** (newtype tuple structs); `Axis`/`SplitDirection` are `rename_all="camelCase"` (lowercase single words); `PaneTree` is internally tagged on `"kind"`.
- **Companions are plain panes:** spawned via `spawn_pane` with `cwd` = the agent's worktree; **NO** hook injection, **NO** attention tracking, **NO** watcher, **NO** sidebar/agent registration. They are not agents.
- **No leaks on teardown:** every companion pane must be `kill()`ed and removed from `panes`/`owner`, and the tree dropped — mirror how `teardown_agent` kills the agent pane (`if let Some(p) = self.get(pane) { let _ = p.kill(); }`).
- **Locking:** keep each `trees`/`owner`/`panes` critical section short; never hold one of these `std::sync::Mutex` guards across an `.await` (matches existing daemon style).
- Commit after each task with a conventional message + the standard trailers.

**Test command:** `cargo test` (whole workspace). Per-crate: `cargo test -p muxy-proto`, `cargo test -p muxy-daemon`.

---

## Task 1: Protocol types + requests/events (muxy-proto)

**Files:**
- Modify: `crates/muxy-proto/src/control.rs` (add types, request/event variants, tests)
- Modify: `crates/muxy-proto/src/lib.rs` (re-export the new public types)

**Interfaces:**
- Produces: `SplitId`, `Axis`, `SplitDirection`, `PaneTree`; `ControlRequest::{SplitPane, ClosePane, SetSplitRatio, GetSplitTree}`; `ControlEvent::SplitTreeChanged`.

- [ ] **Step 1: Write the failing tests** — append to the `#[cfg(test)] mod tests` in `control.rs`:

```rust
    #[test]
    fn split_pane_request_json_shape() {
        let r = ControlRequest::SplitPane { pane: PaneId(2), direction: SplitDirection::Right };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains(r#""type":"splitPane""#), "{s}");
        assert!(s.contains(r#""pane":2"#), "{s}");
        assert!(s.contains(r#""direction":"right""#), "{s}");
        assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap());
    }

    #[test]
    fn close_setratio_gettree_requests_roundtrip() {
        for r in [
            ControlRequest::ClosePane { pane: PaneId(5) },
            ControlRequest::SetSplitRatio { split: SplitId(3), ratio: 0.4 },
            ControlRequest::GetSplitTree { agent: PaneId(1) },
        ] {
            let s = serde_json::to_string(&r).unwrap();
            assert_eq!(r, serde_json::from_str::<ControlRequest>(&s).unwrap(), "{s}");
        }
    }

    #[test]
    fn split_tree_changed_event_nested_roundtrip() {
        let tree = PaneTree::Split {
            id: SplitId(1), axis: Axis::Horizontal, ratio: 0.5,
            first: Box::new(PaneTree::Leaf { pane: PaneId(1) }),
            second: Box::new(PaneTree::Split {
                id: SplitId(2), axis: Axis::Vertical, ratio: 0.3,
                first: Box::new(PaneTree::Leaf { pane: PaneId(2) }),
                second: Box::new(PaneTree::Leaf { pane: PaneId(3) }),
            }),
        };
        let e = ControlEvent::SplitTreeChanged { agent: PaneId(1), tree };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains(r#""type":"splitTreeChanged""#), "{s}");
        assert!(s.contains(r#""kind":"leaf""#) && s.contains(r#""kind":"split""#), "{s}");
        assert!(s.contains(r#""axis":"horizontal""#) && s.contains(r#""axis":"vertical""#), "{s}");
        assert!(s.contains(r#""pane":1"#) && s.contains(r#""id":2"#), "bare numbers: {s}");
        assert_eq!(e, serde_json::from_str::<ControlEvent>(&s).unwrap());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p muxy-proto`
Expected: FAIL — the new types/variants don't exist.

- [ ] **Step 3: Add the types + variants** to `control.rs` (above the `ControlRequest` enum):

```rust
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub struct SplitId(pub u64);

#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Axis { Horizontal, Vertical }

#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SplitDirection { Right, Down }

/// A binary split tree for one agent's workspace. Internally tagged on "kind".
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PaneTree {
    Leaf { pane: PaneId },
    Split {
        id: SplitId,
        axis: Axis,
        ratio: f32,
        first: Box<PaneTree>,
        second: Box<PaneTree>,
    },
}
```

Add to `ControlRequest` (after `SpawnAgent`):
```rust
    SplitPane { pane: PaneId, direction: SplitDirection },
    ClosePane { pane: PaneId },
    SetSplitRatio { split: SplitId, ratio: f32 },
    GetSplitTree { agent: PaneId },
```

Add to `ControlEvent` (after `Error`):
```rust
    SplitTreeChanged { agent: PaneId, tree: PaneTree },
```

- [ ] **Step 4: Re-export from `lib.rs`.** Change the control re-export line to:

```rust
pub use control::{Axis, ControlEvent, ControlRequest, PaneTree, SplitDirection, SplitId};
```

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test -p muxy-proto`
Expected: PASS — existing + the 3 new tests.

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-proto/src/control.rs crates/muxy-proto/src/lib.rs
git commit -m "feat(proto): PaneTree + split/close/ratio/get-tree control messages"
```

---

## Task 2: Pure split-tree algebra (muxy-daemon)

**Files:**
- Create: `crates/muxy-daemon/src/split_tree.rs`
- Modify: `crates/muxy-daemon/src/lib.rs` (add `mod split_tree;` — check whether it needs to be `pub mod`; `pub(crate)` use inside the daemon is enough, so `mod split_tree;`)

**Interfaces:**
- Consumes: `muxy_proto::{PaneTree, PaneId, SplitId, Axis, SplitDirection}`.
- Produces: `leaves`, `contains`, `split_leaf`, `remove_leaf`, `set_ratio` (all `pub(crate)`).

- [ ] **Step 1: Write the failing tests** — in `split_tree.rs`, a `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use muxy_proto::{PaneId, SplitDirection, SplitId};

    fn leaf(n: u64) -> PaneTree { PaneTree::Leaf { pane: PaneId(n) } }

    #[test]
    fn split_a_leaf_makes_a_binary_split() {
        let mut t = leaf(1);
        assert!(split_leaf(&mut t, PaneId(1), PaneId(2), SplitDirection::Right, SplitId(1)));
        match &t {
            PaneTree::Split { axis, first, second, .. } => {
                assert_eq!(*axis, Axis::Horizontal);
                assert_eq!(**first, leaf(1));
                assert_eq!(**second, leaf(2));
            }
            _ => panic!("expected split"),
        }
        assert_eq!(leaves(&t), vec![PaneId(1), PaneId(2)]);
    }

    #[test]
    fn split_down_is_vertical_and_nests() {
        let mut t = leaf(1);
        split_leaf(&mut t, PaneId(1), PaneId(2), SplitDirection::Right, SplitId(1));
        // split the companion (pane 2) downward
        assert!(split_leaf(&mut t, PaneId(2), PaneId(3), SplitDirection::Down, SplitId(2)));
        assert_eq!(leaves(&t), vec![PaneId(1), PaneId(2), PaneId(3)]);
        // the second child is now a vertical split of 2 and 3
        if let PaneTree::Split { second, .. } = &t {
            assert!(matches!(**second, PaneTree::Split { axis: Axis::Vertical, .. }));
        } else { panic!() }
    }

    #[test]
    fn split_unknown_target_is_false() {
        let mut t = leaf(1);
        assert!(!split_leaf(&mut t, PaneId(9), PaneId(2), SplitDirection::Right, SplitId(1)));
    }

    #[test]
    fn remove_collapses_parent_to_sibling() {
        let mut t = leaf(1);
        split_leaf(&mut t, PaneId(1), PaneId(2), SplitDirection::Right, SplitId(1));
        assert!(remove_leaf(&mut t, PaneId(2)));
        assert_eq!(t, leaf(1)); // collapsed back to a lone leaf
    }

    #[test]
    fn remove_in_nested_promotes_sibling() {
        let mut t = leaf(1);
        split_leaf(&mut t, PaneId(1), PaneId(2), SplitDirection::Right, SplitId(1));
        split_leaf(&mut t, PaneId(2), PaneId(3), SplitDirection::Down, SplitId(2));
        assert!(remove_leaf(&mut t, PaneId(3)));
        assert_eq!(leaves(&t), vec![PaneId(1), PaneId(2)]);
    }

    #[test]
    fn remove_last_or_absent_is_false() {
        let mut t = leaf(1);
        assert!(!remove_leaf(&mut t, PaneId(1))); // sole leaf can't be removed
        split_leaf(&mut t, PaneId(1), PaneId(2), SplitDirection::Right, SplitId(1));
        assert!(!remove_leaf(&mut t, PaneId(9))); // absent
    }

    #[test]
    fn set_ratio_finds_and_clamps() {
        let mut t = leaf(1);
        split_leaf(&mut t, PaneId(1), PaneId(2), SplitDirection::Right, SplitId(1));
        assert!(set_ratio(&mut t, SplitId(1), 2.0)); // clamps
        if let PaneTree::Split { ratio, .. } = &t { assert_eq!(*ratio, 0.95); } else { panic!() }
        assert!(!set_ratio(&mut t, SplitId(9), 0.5)); // unknown id
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p muxy-daemon split_tree`
Expected: FAIL — module/functions don't exist.

- [ ] **Step 3: Implement `split_tree.rs`:**

```rust
//! Pure algebra over a per-agent binary split tree (no daemon state / I/O).

use muxy_proto::{Axis, PaneId, PaneTree, SplitDirection, SplitId};

/// All pane ids, left-to-right / top-to-bottom.
pub(crate) fn leaves(tree: &PaneTree) -> Vec<PaneId> {
    match tree {
        PaneTree::Leaf { pane } => vec![*pane],
        PaneTree::Split { first, second, .. } => {
            let mut v = leaves(first);
            v.extend(leaves(second));
            v
        }
    }
}

pub(crate) fn contains(tree: &PaneTree, pane: PaneId) -> bool {
    leaves(tree).contains(&pane)
}

/// Replace `Leaf(target)` with a fresh `Split` of `target` (first) and `companion` (second).
/// `Right` → Horizontal, `Down` → Vertical. Returns false if `target` isn't a leaf here.
pub(crate) fn split_leaf(
    tree: &mut PaneTree,
    target: PaneId,
    companion: PaneId,
    direction: SplitDirection,
    id: SplitId,
) -> bool {
    match tree {
        PaneTree::Leaf { pane } if *pane == target => {
            let axis = match direction {
                SplitDirection::Right => Axis::Horizontal,
                SplitDirection::Down => Axis::Vertical,
            };
            *tree = PaneTree::Split {
                id,
                axis,
                ratio: 0.5,
                first: Box::new(PaneTree::Leaf { pane: target }),
                second: Box::new(PaneTree::Leaf { pane: companion }),
            };
            true
        }
        PaneTree::Leaf { .. } => false,
        PaneTree::Split { first, second, .. } => {
            split_leaf(first, target, companion, direction, id)
                || split_leaf(second, target, companion, direction, id)
        }
    }
}

/// Remove `Leaf(pane)`, collapsing its parent split by promoting the sibling. Returns false
/// if `pane` is absent or is the tree's sole leaf.
pub(crate) fn remove_leaf(tree: &mut PaneTree, pane: PaneId) -> bool {
    match tree {
        PaneTree::Leaf { .. } => false, // a lone leaf cannot remove itself
        PaneTree::Split { first, second, .. } => {
            let first_is_target = matches!(first.as_ref(), PaneTree::Leaf { pane: p } if *p == pane);
            let second_is_target = matches!(second.as_ref(), PaneTree::Leaf { pane: p } if *p == pane);
            if first_is_target {
                let sibling = std::mem::replace(second.as_mut(), PaneTree::Leaf { pane });
                *tree = sibling;
                true
            } else if second_is_target {
                let sibling = std::mem::replace(first.as_mut(), PaneTree::Leaf { pane });
                *tree = sibling;
                true
            } else {
                remove_leaf(first, pane) || remove_leaf(second, pane)
            }
        }
    }
}

/// Set the divider ratio (clamped to [0.05, 0.95]) on the split with `id`.
pub(crate) fn set_ratio(tree: &mut PaneTree, id: SplitId, ratio: f32) -> bool {
    match tree {
        PaneTree::Leaf { .. } => false,
        PaneTree::Split { id: sid, ratio: r, first, second, .. } => {
            if *sid == id {
                *r = ratio.clamp(0.05, 0.95);
                true
            } else {
                set_ratio(first, id, ratio) || set_ratio(second, id, ratio)
            }
        }
    }
}
```

> Note: `contains` may be unused until Task 3; if the compiler warns dead-code, add `#[allow(dead_code)]` or wait — Task 3 uses it. Prefer to keep it and let Task 3 consume it.

- [ ] **Step 4: Register the module** in `crates/muxy-daemon/src/lib.rs`: add `mod split_tree;` next to the other `mod` declarations.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test -p muxy-daemon split_tree`
Expected: PASS — the 7 algebra tests.

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-daemon/src/split_tree.rs crates/muxy-daemon/src/lib.rs
git commit -m "feat(daemon): pure split-tree algebra (split/remove/collapse/set-ratio)"
```

---

## Task 3: Daemon split-tree state + operations (muxy-daemon)

**Files:**
- Modify: `crates/muxy-daemon/src/server.rs` (Daemon struct + `new_with` + methods + `spawn_agent` init + `teardown_agent` cascade + tests)

**Interfaces:**
- Consumes: `split_tree::*` (Task 2), `muxy_proto::{PaneTree, SplitDirection, SplitId}`, `PaneCommand`, `Pane::kill`, `spawn_pane`.
- Produces: `Daemon::split_pane`, `close_pane`, `set_split_ratio`, `split_tree_of`, `tree_event`, `subscribe_splits`; `companion_command` helper; teardown cascade.

- [ ] **Step 1: Write the failing tests** — add a test module section in `server.rs` (mirror the existing `spawn_agent`-based tests: temp git repo, `GitWorktreeDriver`, `FakeNotifier`). Include a pure helper test + integration tests:

```rust
    #[test]
    fn companion_command_uses_shell_and_worktree_cwd() {
        let cmd = companion_command("/bin/zsh".into(), std::path::PathBuf::from("/tmp/wt"));
        assert_eq!(cmd.program, "/bin/zsh");
        assert_eq!(cmd.cwd, Some(std::path::PathBuf::from("/tmp/wt")));
        assert!(cmd.args.is_empty());
        assert!(cmd.env.is_empty()); // no hook env on a companion
    }

    #[tokio::test]
    async fn split_close_and_teardown_manage_the_tree() {
        // temp git repo + daemon with the shell adapter (reuse the existing helpers/pattern)
        let (daemon, repo) = daemon_with_repo();               // <- factor from existing tests, or inline
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter {
            command: PaneCommand { program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] },
        }, "task").unwrap();

        // fresh tree is a lone leaf
        assert_eq!(daemon.split_tree_of(agent), Some(PaneTree::Leaf { pane: agent }));

        // split → companion pane exists, tree is a split with two leaves
        let mut rx = daemon.subscribe_splits();
        let comp = daemon.split_pane(agent, SplitDirection::Right).unwrap();
        assert!(daemon.get(comp).is_some(), "companion pane must exist");
        let tree = daemon.split_tree_of(agent).unwrap();
        assert_eq!(split_tree::leaves(&tree), vec![agent, comp]);
        let (bagent, _btree) = rx.try_recv().expect("SplitTreeChanged broadcast");
        assert_eq!(bagent, agent);

        // nested split on the companion → 3 leaves
        let comp2 = daemon.split_pane(comp, SplitDirection::Down).unwrap();
        assert_eq!(split_tree::leaves(&daemon.split_tree_of(agent).unwrap()).len(), 3);

        // close one companion → collapses, pane gone
        daemon.close_pane(comp2).unwrap();
        assert!(daemon.get(comp2).is_none());
        assert_eq!(split_tree::leaves(&daemon.split_tree_of(agent).unwrap()), vec![agent, comp]);

        // teardown the agent → all companions gone, tree dropped
        daemon.teardown_agent(agent).unwrap();
        assert!(daemon.get(comp).is_none(), "companion must be killed on teardown");
        assert!(daemon.split_tree_of(agent).is_none());
    }

    #[tokio::test]
    async fn set_ratio_updates_and_broadcasts() {
        let (daemon, repo) = daemon_with_repo();
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter {
            command: PaneCommand { program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] },
        }, "t").unwrap();
        let _comp = daemon.split_pane(agent, SplitDirection::Right).unwrap();
        // the split created has id 1 (first split allocated)
        daemon.set_split_ratio(SplitId(1), 0.7).unwrap();
        if let Some(PaneTree::Split { ratio, .. }) = daemon.split_tree_of(agent) {
            assert!((ratio - 0.7).abs() < 1e-6);
        } else { panic!("expected a split") }
    }
```

> The exact repo/daemon setup helper (`daemon_with_repo`) should follow whatever the existing `server.rs` tests already use to build a temp git repo + `Daemon::new_with(GitWorktreeDriver, FakeNotifier, …)`. Reuse that pattern rather than inventing a new one; if there's an existing helper, call it. Using `SyntheticAdapter` running `sleep 30` keeps a live agent pane without needing the `claude` binary.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p muxy-daemon split_close_and_teardown -- --nocapture`
Expected: FAIL — methods don't exist.

- [ ] **Step 3: Add state to the `Daemon` struct** (in `server.rs`):

```rust
    trees: Arc<Mutex<HashMap<PaneId, PaneTree>>>,       // agent pane -> split tree
    owner: Arc<Mutex<HashMap<PaneId, PaneId>>>,          // any leaf pane -> its agent
    next_split_id: AtomicU64,
    split_tx: broadcast::Sender<(PaneId, PaneTree)>,
```
(Import `muxy_proto::{PaneTree, SplitDirection, SplitId}` at the top; `AtomicU64`/`Ordering` are already imported for `next_id`.)

- [ ] **Step 4: Initialize them in `new_with`:**

```rust
        let (split_tx, _) = broadcast::channel(256);
        Daemon {
            // …existing fields…
            trees: Arc::new(Mutex::new(HashMap::new())),
            owner: Arc::new(Mutex::new(HashMap::new())),
            next_split_id: AtomicU64::new(1),
            split_tx,
        }
```

- [ ] **Step 5: Initialize the tree in `spawn_agent`.** After the agent pane is registered and its workspace inserted (near `self.set_attention(id, AttentionState::Working);`), add:

```rust
        self.trees.lock().unwrap().insert(id, PaneTree::Leaf { pane: id });
        self.owner.lock().unwrap().insert(id, id);
```

- [ ] **Step 6: Add the operations + helper** (in `impl Daemon`):

```rust
    pub fn subscribe_splits(&self) -> broadcast::Receiver<(PaneId, PaneTree)> {
        self.split_tx.subscribe()
    }

    pub fn split_tree_of(&self, agent: PaneId) -> Option<PaneTree> {
        self.trees.lock().unwrap().get(&agent).cloned()
    }

    /// SplitTreeChanged for `agent`, or an Error event if it has no tree.
    pub fn tree_event(&self, agent: PaneId) -> muxy_proto::ControlEvent {
        match self.split_tree_of(agent) {
            Some(tree) => muxy_proto::ControlEvent::SplitTreeChanged { agent, tree },
            None => muxy_proto::ControlEvent::Error { message: format!("no split tree for {agent:?}") },
        }
    }

    fn broadcast_tree(&self, agent: PaneId) {
        if let Some(tree) = self.split_tree_of(agent) {
            let _ = self.split_tx.send((agent, tree));
        }
    }

    fn alloc_split_id(&self) -> SplitId {
        SplitId(self.next_split_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Split `target` (a leaf) by spawning a companion shell in its agent's worktree.
    pub fn split_pane(&self, target: PaneId, direction: SplitDirection) -> Result<PaneId> {
        let agent = *self.owner.lock().unwrap().get(&target)
            .ok_or_else(|| anyhow::anyhow!("unknown pane {target:?}"))?;
        let path = self.workspaces.lock().unwrap().get(&agent).map(|w| w.path.clone())
            .ok_or_else(|| anyhow::anyhow!("no workspace for agent {agent:?}"))?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let companion = self.spawn_pane(companion_command(shell, path), 80, 24)?;
        let sid = self.alloc_split_id();
        {
            let mut trees = self.trees.lock().unwrap();
            let tree = trees.get_mut(&agent)
                .ok_or_else(|| anyhow::anyhow!("no split tree for {agent:?}"))?;
            crate::split_tree::split_leaf(tree, target, companion, direction, sid);
        }
        self.owner.lock().unwrap().insert(companion, agent);
        self.broadcast_tree(agent);
        Ok(companion)
    }

    /// Close a companion pane (collapsing the tree), or teardown the agent if `pane` is one.
    /// Returns Some(agent) if a companion was closed, None if an agent was torn down.
    pub fn close_pane(&self, pane: PaneId) -> Result<Option<PaneId>> {
        if self.trees.lock().unwrap().contains_key(&pane) {
            self.teardown_agent(pane)?;
            return Ok(None);
        }
        let agent = *self.owner.lock().unwrap().get(&pane)
            .ok_or_else(|| anyhow::anyhow!("unknown pane {pane:?}"))?;
        if let Some(p) = self.get(pane) {
            let _ = p.kill();
        }
        self.panes.lock().unwrap().remove(&pane);
        if let Some(tree) = self.trees.lock().unwrap().get_mut(&agent) {
            crate::split_tree::remove_leaf(tree, pane);
        }
        self.owner.lock().unwrap().remove(&pane);
        self.broadcast_tree(agent);
        Ok(Some(agent))
    }

    /// Move a divider. Returns the owning agent so callers can emit its tree.
    pub fn set_split_ratio(&self, split: SplitId, ratio: f32) -> Result<PaneId> {
        let mut found = None;
        {
            let mut trees = self.trees.lock().unwrap();
            for (agent, tree) in trees.iter_mut() {
                if crate::split_tree::set_ratio(tree, split, ratio) { found = Some(*agent); break; }
            }
        }
        let agent = found.ok_or_else(|| anyhow::anyhow!("unknown split {split:?}"))?;
        self.broadcast_tree(agent);
        Ok(agent)
    }
```

And a free function near the top of `server.rs` (module level, `pub(crate)` so the test can call it):

```rust
/// The command for a companion pane: the login shell, rooted in the worktree, with no hook env.
pub(crate) fn companion_command(shell: String, cwd: std::path::PathBuf) -> PaneCommand {
    PaneCommand { program: shell, args: vec![], cwd: Some(cwd), env: vec![] }
}
```

- [ ] **Step 7: Add the teardown cascade** to `teardown_agent`. At the top of the method (before removing the agent pane), kill + drop all companion leaves and drop the tree:

```rust
    pub fn teardown_agent(&self, pane: PaneId) -> Result<()> {
        // Cascade: kill every companion pane in this agent's tree.
        let companions: Vec<PaneId> = self.trees.lock().unwrap().get(&pane)
            .map(|t| crate::split_tree::leaves(t).into_iter().filter(|p| *p != pane).collect())
            .unwrap_or_default();
        for c in &companions {
            if let Some(p) = self.get(*c) { let _ = p.kill(); }
            self.panes.lock().unwrap().remove(c);
            self.owner.lock().unwrap().remove(c);
        }
        self.trees.lock().unwrap().remove(&pane);
        self.owner.lock().unwrap().remove(&pane);

        // …the existing teardown_agent body (kill agent pane, abort watcher, teardown
        //   workspace, remove from panes/attention/agents, send removed) stays below…
    }
```

- [ ] **Step 8: Run to verify all pass**

Run: `cargo test -p muxy-daemon` then `cargo test`
Expected: PASS — the companion-command unit test, the split/close/teardown integration test, the set-ratio test, and every pre-existing test.

- [ ] **Step 9: Commit**

```bash
git add crates/muxy-daemon/src/server.rs
git commit -m "feat(daemon): split-tree state, split/close/ratio ops, teardown cascade"
```

---

## Task 4: Control-channel wiring (muxy-daemon `control_json.rs`)

Handle the four new requests and stream `SplitTreeChanged`. Gate: `cargo test`.

**Files:**
- Modify: `crates/muxy-daemon/src/control_json.rs`

**Interfaces:**
- Consumes: `Daemon::{split_pane, close_pane, set_split_ratio, split_tree_of, tree_event, subscribe_splits}`, `ControlRequest`/`ControlEvent` (Task 1).

- [ ] **Step 1: Write the failing test** — add to the `control_json.rs` test module (mirror the existing `attentionChanged`-over-the-stream test; reuse its temp-repo + `serve_control_json` setup):

```rust
    #[tokio::test]
    async fn split_pane_over_control_stream_yields_split_tree_changed() {
        // Build a daemon + control socket exactly like the existing control-json test does,
        // spawn a shell agent, then send a SplitPane request and read events until a
        // SplitTreeChanged arrives whose tree has two leaves.
        // (reuse the existing helper/setup in this file's tests)
        // 1. connect the control socket, read the initial AgentList
        // 2. send ControlRequest::SplitPane { pane: agent, direction: Right }
        // 3. assert a ControlEvent::SplitTreeChanged { agent, tree } arrives with 2 leaves
    }
```

> Follow the exact shape of the existing control-stream test in this file (whatever it's named) for socket setup, request-writing, and line-reading — this test only adds: send `SplitPane`, expect `SplitTreeChanged`. Assert the tree via `muxy-daemon`'s `split_tree::leaves` or by structural match on the decoded `PaneTree` (2 leaves: the agent + a companion).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p muxy-daemon split_pane_over_control_stream`
Expected: FAIL — the request isn't handled (falls through / errors).

- [ ] **Step 3: Handle the new requests.** In the request-decoding match (where `ListAgents`/`SpawnAgent` are handled → an `ev` is produced and `write_event`-ed), add arms:

```rust
                                Ok(ControlRequest::SplitPane { pane, direction }) =>
                                    match self.split_pane(pane, direction) {
                                        Ok(companion) => {
                                            match self.owner_of(companion) {
                                                Some(agent) => self.tree_event(agent),
                                                None => ControlEvent::Error { message: "split produced no owner".into() },
                                            }
                                        }
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::ClosePane { pane }) =>
                                    match self.close_pane(pane) {
                                        Ok(Some(agent)) => self.tree_event(agent),
                                        Ok(None) => ControlEvent::AgentRemoved { pane },
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::SetSplitRatio { split, ratio }) =>
                                    match self.set_split_ratio(split, ratio) {
                                        Ok(agent) => self.tree_event(agent),
                                        Err(e) => ControlEvent::Error { message: e.to_string() },
                                    },
                                Ok(ControlRequest::GetSplitTree { agent }) => self.tree_event(agent),
```

Add a tiny `owner_of` getter to `Daemon` (in `server.rs`) if one doesn't already exist:
```rust
    pub fn owner_of(&self, pane: PaneId) -> Option<PaneId> {
        self.owner.lock().unwrap().get(&pane).copied()
    }
```

> The requester will also receive the `SplitTreeChanged` from the broadcast arm (Step 4) — a harmless duplicate for the single-client case. Note it in the report; de-duping per-connection is out of scope for M1c-1.

- [ ] **Step 4: Add the broadcast `select!` arm.** Near where `att_rx`/`removed_rx` are subscribed at the top of the control-connection handler, add `let mut split_rx = self.subscribe_splits();`, and add an arm alongside the `att`/`removed` arms:

```rust
                sp = split_rx.recv() => {
                    match sp {
                        Ok((agent, tree)) =>
                            write_event(&mut wr, &ControlEvent::SplitTreeChanged { agent, tree }).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p muxy-daemon` then `cargo test`
Expected: PASS — the new control-stream test + the whole workspace.

- [ ] **Step 6: Commit**

```bash
git add crates/muxy-daemon/src/control_json.rs crates/muxy-daemon/src/server.rs
git commit -m "feat(daemon): handle split/close/ratio/get-tree over the control channel"
```

---

## Final verification

- `cargo test` → whole workspace green: proto round-trips (Task 1), split-tree algebra unit tests (Task 2), daemon split/close/teardown + set-ratio integration tests (Task 3), and the control-stream `SplitTreeChanged` test (Task 4).
- No client/UI in this slice; the daemon is exercised directly and over the JSON control socket. Companions are spawned as plain shells in the agent's worktree with no hooks/attention/watcher, and teardown leaves no panes behind.
