# clowder M9b — Full split-layout restore across a daemon restart

## Context

M9a made **agents** survive a daemon restart: a durable `agents.json` registry, reconcile-on-startup,
and adapter session-resume bring each agent back in its worktree. M9a is **agent-pane-only** — a
restored agent comes back as a single pane; its companion shells and split arrangement are lost. M9b
closes that gap: after a restart, each agent reappears **in the same split layout**, with its companion
shells re-spawned in the same arrangement (structure + axes + ratios).

### What exists (ground truth, verified 2026-08-02)

- **The split tree is in-memory only.** `Daemon.trees: HashMap<PaneId, PaneTree>` (agent pane → tree)
  and `owner: HashMap<PaneId, PaneId>` (any leaf → its agent) hold the layout
  (`crates/clowder-daemon/src/server.rs:40-41`). Nothing is persisted; M9a reconcile resets each agent
  to `PaneTree::Leaf { pane: id }` (`server.rs:299`).
- **`PaneTree` is the wire type and already serializes** (`crates/clowder-proto/src/control.rs:16-27`):
  `Leaf { pane: PaneId }` or `Split { id: SplitId, axis: Axis, ratio: f32, first, second }`, internally
  tagged on `"kind"`, camelCase.
- **Companions are plain login shells.** `companion_command` = the login shell rooted in the worktree
  cwd, **no hook env, no adapter** (`server.rs:24-26`). A companion has a runtime-allocated `PaneId`
  from `alloc_id()` and spawns at `default_cols`×`default_rows` (`server.rs:440-444`); the client surface
  resizes it. There is exactly **one** non-agent leaf kind: a companion shell.
- **The agent's own leaf uses the agent's `PaneId`**, which M9a keeps **stable** across restart. So in a
  persisted tree the agent leaf's id is still valid after reconcile; only companion leaf ids are stale.
- **Tree-mutating methods each know the affected agent:** `split_pane` (add companion),
  `close_pane -> Result<Option<PaneId>>` (close companion), `reap_companion` (companion crash),
  `set_split_ratio(split, ratio) -> Result<PaneId>` (divider drag). Each ends by calling
  `broadcast_tree(agent)` (`server.rs:417`), which emits `ControlEvent::SplitTreeChanged`.
- **`next_id` bump ordering:** M9a `reconcile` (`server.rs:134-183`) calls `bump_next_id_above(max_id)`
  **after** the restore loop. Safe in M9a (agents re-spawn under their fixed `PaneId(rec.agent_id)`, never
  `alloc_id()`), but unsafe once the loop allocates companion ids.
- **The app needs no change:** on M5d reconnect it lists agents and `GetSplitTree`s the selected agent;
  `set_split_ratio`/split ids are re-learned from the tree the daemon reports. M9a required no app or
  wire-protocol change, and neither does M9b.

### User decisions (brainstorm, 2026-08-02)

- **Layout fidelity: full (structure + axes + ratios), coalesced.** Persist structural changes
  immediately; coalesce ratio-drag writes behind a lightweight periodic flush (no per-tick disk writes).
- **Restore failure: best-effort collapse.** If a companion fails to re-spawn, drop just that leaf and
  keep the rest; the agent leaf always survives.
- **Scope boundaries:** restored companions are **fresh empty shells** (no scrollback / command history —
  that is M9c); the daemon persists **layout only**, not app-side selection/focus (the app owns those).

## Goals / Non-goals

**Goals:** (1) persist each agent's split tree durably alongside its M9a record; (2) keep the on-disk tree
current as the layout changes, without hammering the disk on a divider drag; (3) on reconcile, rebuild
each agent's companion shells in the same arrangement (structure + axes + ratios), best-effort; (4)
never crash the daemon and never let a restored companion id collide with an agent id; (5) back-compat —
M9a records with no tree restore exactly as before.

**Non-goals (M9b):** preserving companion **scrollback / running processes** (M9c); persisting app-side
**selection or focus**; any new **companion leaf kind** beyond the shell; any **wire-protocol or macOS-app**
change; changing the M9a agent-resume behavior.

## Component design

### 1. Registry record gains the tree

Extend `AgentRecord` (`crates/clowder-daemon/src/registry.rs`) with:

```rust
#[serde(default)]
pub tree: Option<PaneTree>,   // None = single agent leaf (also how M9a records deserialize)
```

`PaneTree` comes from `clowder_proto`. `#[serde(default)]` is what makes old M9a records — which have no
`tree` key — deserialize to `None` and restore as today. The **literal** tree is stored, agent leaf
included. New method:

```rust
pub fn set_tree(&self, agent_id: u64, tree: Option<PaneTree>)
```

Loads under the existing `write_lock`, updates that one record's `tree` (no-op if the agent isn't in the
registry — e.g. already landed), writes atomically. A bare agent-leaf tree (`Leaf` whose pane is the
agent) is normalized to `None` to keep records small; `restore_layout` also treats
`Some(Leaf { pane: agent })` as trivial, so both forms are safe.

### 2. Persistence: immediate for structure, coalesced for ratios

- **Structural** (`split_pane`, `close_pane`, `reap_companion`): after mutating `trees`, call
  `registry.set_tree(agent, current_tree)` immediately. These are user-rare and change the *set* of
  companions, which must not be lost.
- **Ratio** (`set_split_ratio`): mark the agent **dirty** in a `Mutex<HashSet<PaneId>>` — do **not** write
  inline. A **periodic flush task**, spawned once at daemon start (same place `reconcile` is wired, in
  `main.rs`), ticks every ~750 ms: drain the dirty set, and for each still-live agent call
  `registry.set_tree(agent, current_tree)`. The tick interval is a module constant. Worst case (a
  daemon kill mid-drag) loses ≤ one tick of the final ratio nudge; the structure is always current.
- The flush task holds only an `Arc<Daemon>` (weak-free is fine; it dies with the process). It skips
  agents no longer in `trees` (landed/discarded between mark and flush).

### 3. Reconcile: id-safety + layout restore

Two changes to `Daemon::reconcile` (`server.rs:134-183`):

- **Bump `next_id` before the restore loop.** Compute `max_id` over all records first and
  `bump_next_id_above(max_id)` up front, so companion `alloc_id()`s during restore can never collide with
  a not-yet-restored agent's fixed id. (Agents still spawn under `PaneId(rec.agent_id)`, unaffected by the
  reorder.)
- **Restore the layout after `finalize_agent`.** For a record whose `tree` is `Some` and not a bare
  agent leaf, call a new `Daemon::restore_layout(agent, tree, worktree_path)`:
  1. `split_tree::rebuild_for_restore(&tree, agent_id, &mut spawn_companion, &mut alloc_split)` — a **pure**
     helper (below) that returns the rebuilt tree + the list of new companion ids.
  2. Register each new companion: `owner.insert(companion, agent)` and a reap watcher (the same
     `wait_exit -> reap_companion` wiring as `split_pane`).
  3. Insert the rebuilt tree into `trees` (replacing the single leaf `finalize_agent` set) and
     `broadcast_tree(agent)`.

  A failure to build any single companion collapses that leaf (best-effort); a total failure still leaves
  the agent as a single leaf. `restore_layout` never returns an error that aborts reconcile.

### 4. The rebuild helper (the crux)

In `crates/clowder-daemon/src/split_tree.rs`:

```rust
/// Rebuild a persisted tree for restore: keep the agent leaf (its id is stable), spawn a fresh
/// companion for every other leaf (substituting the new id), regenerate split ids, preserve
/// axis+ratio. Best-effort: if `spawn_companion` returns None, that leaf collapses to its sibling.
/// Returns the rebuilt tree and the new companion ids (in creation order).
pub(crate) fn rebuild_for_restore(
    tree: &PaneTree,
    agent: PaneId,
    spawn_companion: &mut dyn FnMut() -> Option<PaneId>,
    alloc_split: &mut dyn FnMut() -> SplitId,
) -> (PaneTree, Vec<PaneId>)
```

Recursion:
- `Leaf { pane }` where `pane == agent` → `(Leaf { pane: agent }, [])`.
- `Leaf { pane: _ }` (any other id = a companion) → `spawn_companion()`: `Some(id)` →
  `(Leaf { pane: id }, [id])`; `None` → signal collapse (see below).
- `Split { axis, ratio, first, second, .. }` → rebuild `first` and `second`. If one side collapsed
  (its subtree produced no panes), return the other side in place of the split (dropping the divider),
  concatenating companion ids. Otherwise emit `Split { id: alloc_split(), axis, ratio, first, second }`.

Collapse is represented by an empty result for that subtree; the agent leaf can never collapse, so the
whole rebuild always yields at least `Leaf { pane: agent }`. Fresh `SplitId`s keep `next_split_id`
coherent; the app re-learns them from the broadcast tree.

## Data flow

```
split / close / reap  → mutate trees → registry.set_tree(agent, tree)  (immediate atomic write)
set_split_ratio        → mutate trees → dirty.insert(agent)             (no write)
flush task (~750ms)    → drain dirty → registry.set_tree(agent, tree)   (coalesced write)
land / discard         → registry.remove(agent)                          (M9a; drops the tree too)

daemon startup → reconcile:
  max_id = max(record.agent_id); bump_next_id_above(max_id)   ← before the loop
  for record:
    finalize_agent(...)                                       ← M9a: agent back as single leaf
    if record.tree is Some(non-trivial):
      (rebuilt, companions) = rebuild_for_restore(tree, agent, spawn_companion, alloc_split)
      register owner + reap watcher per companion
      trees[agent] = rebuilt; broadcast_tree(agent)
app (M5d reconnect) → listAgents → getSplitTree(selected) → daemon returns the restored tree → UI repaints
```

## Error handling

- **Companion spawn failure:** best-effort collapse of that leaf; agent survives (§4).
- **Corrupt / malformed tree in a record:** `serde` already fails the *whole* `agents.json` load to empty
  on corruption (M9a, never panics). A structurally-odd but valid tree (e.g. an unknown leaf id) is fine —
  any non-agent leaf is treated as a companion.
- **Agent landed between mark-dirty and flush:** `set_tree` is a no-op for an absent record; the flush
  skips agents no longer in `trees`.
- **Registry write failure:** logged and swallowed exactly as in M9a (`try_write` → `warn!`).

## Testing

- **`rebuild_for_restore` (pure unit, fake closures):** agent-leaf id preserved; a companion leaf gets a
  substituted fresh id; axes + ratios preserved; a `None` from `spawn_companion` collapses exactly one
  leaf and keeps the sibling; a deep/nested tree (companion split into companions) recurses correctly;
  an agent-only `Leaf` returns a single leaf with no companions.
- **Registry:** `AgentRecord` with a `tree` round-trips; a record JSON **without** a `tree` key
  deserializes to `tree: None` (M9a back-compat); `set_tree` updates one record and leaves others intact;
  `set_tree` on an absent agent is a no-op.
- **Reconcile integration:** spawn an agent, `split_pane` it (one companion), confirm the record's tree
  persisted; construct a fresh `Daemon` over the same registry + worktree, `reconcile`, and assert the
  agent is back with a 2-leaf tree, the **agent leaf id preserved**, axis + ratio matching, and the
  companion re-spawned (a live pane).
- **Id-collision regression:** two agents where the lower-id agent has a deeply-split layout; after
  reconcile, the restored companion ids do not equal the higher-id agent's id, and a subsequent
  `spawn_agent` gets an id above all restored ids.
- **Persistence wiring:** after `split_pane`, `registry.load()` shows the new tree; after `close_pane`,
  it shows the collapsed tree. A **direct** flush-function test: mark an agent dirty after
  `set_split_ratio`, invoke the flush once, assert the persisted ratio matches (no reliance on wall-clock
  timing).
- **Existing suites stay green** (`cargo test --workspace --locked`).

## Risks

1. **Write amplification on drag** — mitigated by the dirty-set + periodic flush (§2); ratio ticks never
   touch disk directly.
2. **Companion-id collision with an agent id** — mitigated by bumping `next_id` before the restore loop
   (§3); covered by the id-collision regression test.
3. **Stale companion ids in the persisted tree** — expected; the rebuild substitutes fresh ids and only
   the (stable) agent leaf id is reused (§4).
4. **Back-compat with M9a records** — `#[serde(default)]` on `tree` (§1); covered by the missing-field
   test.
5. **A companion that had been split further (nested companions)** — the recursive rebuild handles
   arbitrary depth; covered by the deep-tree test.

## Decomposition

**One slice, one PR (M9b).** Suggested SDD tasks: (1) registry `tree` field + `set_tree` + back-compat;
(2) `rebuild_for_restore` helper; (3) persist-on-structural-change wiring; (4) dirty-set + periodic flush
task; (5) reconcile bump-reorder + `restore_layout`; (6) reconcile/id-collision integration tests.

## Verification gate

**M9b end state:** an agent with companion shells in a non-trivial split survives a daemon `kill` +
supervised relaunch — it reappears in the app in the **same arrangement** (structure + axes + ratios),
companions re-spawned as fresh shells in the worktree, agent leaf id preserved, driven by the `tree` field
of its `agents.json` record which the daemon persists (immediately on structural change, coalesced on
ratio drag) and rebuilds on reconcile; a failed companion collapses only its leaf; no restored companion
id ever collides with an agent id; M9a records with no tree restore unchanged. Deferred: **M9c** PTY-host
zero-disruption survival (companion scrollback + running processes).
