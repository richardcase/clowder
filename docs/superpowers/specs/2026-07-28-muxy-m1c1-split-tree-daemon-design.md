# muxy M1c-1 — Split-Tree Daemon Core

## Context

### The companion-splits feature (overall design — brainstormed & approved)

Companion split panes let the user split the focused area into the agent's terminal plus one
or more **companion shells** — plain `$SHELL` panes rooted in the *same* worktree cwd — for
running tests/git/poking around without disturbing the agent. Confirmed shape:

- **Nested, daemon-owned split tree.** Each agent owns a binary split tree; leaves are panes
  (the agent pane + companions), internal nodes are H/V splits with a divider ratio. The tree
  lives in **daemon state** so the whole layout survives window close / detach / reattach.
- **Companion panes** are plain shells in the agent's worktree: **no hook injection, not
  attention-tracked, not a sidebar row** (the pane-vs-agent model). A workspace has exactly one
  agent pane and zero-or-more companions.
- **Attach is uniform:** every leaf is a `muxy attach <leafPane>` in its own libghostty
  surface — the existing pump/attach path already handles any pane.
- **Draggable dividers** (resize → persisted ratio), and **commands** wired into the M1a
  keymap/palette: Split Right (⌘D), Split Down (⌘⇧D), Close Pane (⌘⇧W + palette), Focus Next
  Pane.

Implementation is decomposed into three sub-slices, each its own spec→plan→PR:
- **M1c-1 (THIS SPEC)** — the daemon split-tree core + protocol (Rust, fully unit-tested, no UI).
- **M1c-2** — client render: recursive split view, attach companions, focus, 50/50 splits.
- **M1c-3** — draggable dividers (`SetSplitRatio` from drags) + the split/close/focus commands.

### What exists (ground truth)

`muxy-daemon` `Daemon` (`server.rs`): `panes: Mutex<HashMap<PaneId, Arc<Pane>>>`,
`workspaces: Mutex<HashMap<PaneId /*agent*/, Workspace>>`, `spawn_pane(cmd: PaneCommand,
cols, rows) -> Result<PaneId>`, `spawn_agent(project, adapter, task) -> Result<PaneId>`
(provisions a `Workspace{path, branch, project}`, spawns the agent pane, inserts the
workspace keyed by the agent pane id), `teardown_agent(pane)`, `alloc_id()`, attention +
`removed` broadcasts. `PaneCommand{ program: String, args: Vec<String>, cwd: Option<PathBuf>,
env: Vec<(String,String)> }`. `serve_control_json` `select!`s over the request stream +
`att_rx` (attention) + `removed_rx` broadcasts.

`muxy-proto` (`control.rs`): `ControlRequest` / `ControlEvent`, both `#[serde(tag="type",
rename_all="camelCase")]`; `PaneId(pub u64)` serializes as a **bare number**;
`AttentionState` as PascalCase. Existing requests: `ListAgents`, `SpawnAgent`. Existing
events: `AgentList`, `AttentionChanged`, `AgentRemoved`, `AgentSpawned`, `Error`.

`muxy-client` `pump` attaches to **any** `PaneId` — companions need no client changes to
attach.

## Goals / Non-goals (M1c-1)

**Goals:** a fully working *server side* for companion splits — the daemon maintains a binary
split tree per agent, can split a pane (spawning a companion shell in the agent's worktree),
close a companion (collapsing the tree), resize a divider, and answer/broadcast the current
tree; agent teardown cascades to companions. All Rust, unit- and integration-tested against
a live daemon, with the protocol types the client will mirror in M1c-2.

**Non-goals (later slices):** any Swift/UI (M1c-2); driving splits from drags/commands
(M1c-3); the client-side `PaneTree` mirror; persistence to disk (the tree lives in daemon
memory — it survives detach/reattach while the daemon runs, not a daemon restart).

## Component design

### Protocol (`muxy-proto`)

New shared types (all `Serialize`/`Deserialize`, matching the existing tagging conventions):

```rust
pub struct SplitId(pub u64);            // bare number, like PaneId

#[serde(rename_all = "camelCase")]
pub enum Axis { Horizontal, Vertical }  // "horizontal" | "vertical"

#[serde(rename_all = "camelCase")]
pub enum SplitDirection { Right, Down } // "right" | "down"

// The split tree, internally tagged on "kind". PaneId stays a bare number.
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PaneTree {
    Leaf  { pane: PaneId },
    Split { id: SplitId, axis: Axis, ratio: f32, first: Box<PaneTree>, second: Box<PaneTree> },
}
```

- `Right` → `axis: Horizontal` (side-by-side, vertical divider), companion on the **right**
  (`second`). `Down` → `axis: Vertical` (stacked), companion **below** (`second`).
- JSON: leaf `{"kind":"leaf","pane":7}`; split
  `{"kind":"split","id":2,"axis":"horizontal","ratio":0.5,"first":{…},"second":{…}}`.

New `ControlRequest` variants:
```rust
SplitPane    { pane: PaneId, direction: SplitDirection },  // split the (focused) leaf
ClosePane    { pane: PaneId },                             // close a companion (or teardown the agent)
SetSplitRatio{ split: SplitId, ratio: f32 },              // move a divider
GetSplitTree { agent: PaneId },                            // request an agent's current tree
```

New `ControlEvent` variant:
```rust
SplitTreeChanged { agent: PaneId, tree: PaneTree },        // broadcast on change; also the GetSplitTree reply
```

Round-trip tests (proto): each new request/event JSON-shape asserted (tag + camelCase +
`pane`/`split` as bare numbers + `axis`/`direction` lowercase), mirroring the existing
`control.rs` tests, plus a nested `PaneTree` round-trip.

### The pure tree algebra (`muxy-daemon`, unit-tested without a daemon)

A module of pure functions over `PaneTree` — the testable heart, no I/O:

```rust
fn leaves(tree: &PaneTree) -> Vec<PaneId>                       // all pane ids, in order
fn contains(tree: &PaneTree, pane: PaneId) -> bool
/// Replace Leaf(target) with Split{ new id, axis, 0.5, first: Leaf(target), second: Leaf(companion) }.
/// Returns false if target isn't a leaf in the tree.
fn split_leaf(tree: &mut PaneTree, target: PaneId, companion: PaneId, direction: SplitDirection, id: SplitId) -> bool
/// Remove Leaf(pane); collapse its parent Split by promoting the sibling. Returns false if
/// pane isn't present or is the tree's sole leaf (can't remove the last pane this way).
fn remove_leaf(tree: &mut PaneTree, pane: PaneId) -> bool
/// Set ratio (clamped to [0.05, 0.95]) on the Split with `id`. Returns false if not found.
fn set_ratio(tree: &mut PaneTree, id: SplitId, ratio: f32) -> bool
```

Unit tests: split a leaf → correct nested shape + axis + both leaves present; split a
companion leaf (nested split) → deeper tree; remove a companion → parent collapses, sibling
promoted, root reduces to a leaf when only one remains; remove-last / remove-absent → false;
set_ratio finds the node and clamps; `leaves` order.

### Daemon integration (`muxy-daemon` `server.rs` + `control_json.rs`)

Daemon state additions:
```rust
trees: Mutex<HashMap<PaneId /*agent*/, PaneTree>>,   // agent pane -> its split tree
owner: Mutex<HashMap<PaneId /*any leaf*/, PaneId /*agent*/>>,
next_split_id: Mutex<u64>,
split_tx: broadcast::Sender<(PaneId /*agent*/, PaneTree)>,
```
`subscribe_splits()` mirrors `subscribe_attention`.

- **On `spawn_agent`:** initialize `trees[agent] = Leaf{pane: agent}`, `owner[agent] = agent`.
- **`split_pane(target: PaneId, direction) -> Result<PaneId>`:** `agent = owner[target]`
  (else error "unknown pane"); `path = workspaces[agent].path`; spawn a companion via
  `spawn_pane(PaneCommand{ program: $SHELL (env SHELL, else /bin/sh), args: [], cwd:
  Some(path), env: [] }, 80, 24)`; `split_leaf(trees[agent], target, companion, direction,
  next id)`; `owner[companion] = agent`; broadcast `(agent, trees[agent].clone())`; return
  companion. On spawn failure, leave the tree unchanged.
- **`close_pane(pane) -> Result<()>`:** if `pane` is an agent (in `trees`) → `teardown_agent`
  (below). Else a companion: kill its process (drop it from `panes`, closing the PTY),
  `remove_leaf(trees[agent], pane)`, `owner.remove(pane)`, broadcast the new tree.
- **`set_split_ratio(split, ratio) -> Result<()>`:** find the agent tree containing `split`
  (scan `trees`), `set_ratio`, broadcast.
- **`split_tree_of(agent) -> Option<PaneTree>`** getter for `GetSplitTree`.
- **`teardown_agent` cascade:** before removing the agent, kill every **companion** leaf in
  `trees[agent]` (all leaves except the agent pane), drop them from `panes`, clear their
  `owner` entries; then remove `trees[agent]` and the agent's `owner`/workspace as today.
- **`control_json`:** handle the four new requests (mutations reply/emit `SplitTreeChanged`;
  `GetSplitTree` replies with the current tree or an `Error` for an unknown agent); add a
  `select!` arm on `split_rx` that writes `SplitTreeChanged { agent, tree }` to the client.

## Data flow (server side)

```
client: SplitPane{pane, Right} ─► daemon.split_pane
   owner[pane]→agent ─► spawn_pane($SHELL, cwd=workspace.path) ─► companion PaneId
   split_leaf(trees[agent], pane, companion, Right, newId) ─► broadcast SplitTreeChanged{agent, tree}
client attaches the companion exactly like an agent: `muxy attach <companion>` (pump path, unchanged)
client: ClosePane{companion} ─► kill process + remove_leaf + collapse ─► SplitTreeChanged
client: SetSplitRatio{split, r} ─► set_ratio ─► SplitTreeChanged
teardown_agent ─► cascade-kill companions ─► drop tree
```

## Testing (`cargo test`)

- **Proto round-trips:** JSON shape of each new request/event + a nested `PaneTree`.
- **Pure tree algebra:** the unit tests listed above (split/remove/collapse/set_ratio/leaves).
- **Daemon integration (live daemon, temp git repo, `shell` agent):**
  - spawn agent → `split_tree_of(agent)` is `Leaf(agent)`.
  - `split_pane(agent, Right)` → returns a companion pane that exists in `panes`; the tree is
    a `Split` with the agent + companion as leaves; a `SplitTreeChanged` is broadcast; the
    companion's cwd is the agent's worktree (assert by running e.g. `pwd`/writing a marker via
    the pane, or asserting the spawned `PaneCommand.cwd` == workspace path through a seam).
  - `split_pane` again on the companion → nested tree, three leaves.
  - `close_pane(companion)` → tree collapses back to `Leaf(agent)`; companion gone from
    `panes`/`owner`; `SplitTreeChanged` broadcast.
  - `set_split_ratio` → the divider ratio updates and broadcasts.
  - `teardown_agent` with companions → all companion panes removed from `panes`, tree dropped
    (no leaks), matching the existing teardown tests' style.
  - control-channel: a `SplitPane` request over the JSON socket yields a `SplitTreeChanged`
    reply/broadcast (mirrors the existing `attentionChanged` control-stream test).

## Risks

1. **Tree-mutation correctness** (collapse/promote on close, nested splits). Mitigated by the
   pure algebra with focused unit tests — no daemon needed to exercise the tricky cases.
2. **Teardown leaks.** A companion left in `panes` after teardown leaks a PTY/process.
   Mitigated by an explicit cascade + a teardown-with-companions integration test asserting
   `panes` is clean.
3. **Lock ordering.** New `trees`/`owner` mutexes alongside `panes`/`workspaces` — keep each
   critical section short and never hold two of these locks across an await, matching the
   existing style.

## Verification gate

`cargo test` green across the workspace (existing + proto round-trips + tree-algebra unit
tests + daemon integration tests). No client/UI in this slice; the daemon is exercised
directly and over the JSON control socket.
