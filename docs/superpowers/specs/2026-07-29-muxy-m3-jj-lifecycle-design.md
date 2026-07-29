# muxy M3 — jj Driver + Workspace Lifecycle UX

## Context

muxy provisions an isolated working copy per agent (a git worktree on branch `muxy/<task>`)
and, on teardown, removes the worktree — but **leaves the branch dangling** (never merged or
deleted) and offers **no user-facing way to integrate or clean up** an agent's work. M3 adds
the lifecycle UX (**Land** / **Discard**) and a second workspace backend (**jujutsu**).

Brainstormed & approved decisions:
- **"Land" = finalize + hand off.** Commit any uncommitted work into a clean `muxy/<task>`
  branch (git) / bookmark (jj), remove the working copy, and **keep the branch/bookmark**. The
  user integrates it with their own tools (merge / PR / rebase). muxy never auto-merges
  unreviewed agent work, never touches the main checkout, and needs no remote — safe and
  universal. **"Discard"** throws the work away (remove working copy + delete branch/change).
- **`JjDriver` shells out to the `jj` CLI** (like `GitWorktreeDriver` shells out to `git`) —
  consistent, and dodges `jj-lib`'s API instability.
- **Design the whole feature now, decompose at planning** into three PR-sized slices
  (M3a → M3b → M3c below).

### What exists (ground truth)

`muxy-workspace`: `Workspace { path, branch, project }`; `WorkspaceDriver` trait
(`provision(project, name) -> Workspace`, `teardown(ws)`); `GitWorktreeDriver` shells to `git`
(`worktree add -b muxy/<name>`, `worktree remove --force` + `prune`). `muxy-daemon`: one global
`driver: Arc<dyn WorkspaceDriver>`; `workspaces: HashMap<PaneId, Workspace>` + `workspace_of()`;
`teardown_agent` calls `driver.teardown` (worktree remove, **branch kept**) then removes the
agent + broadcasts `AgentRemoved`. `muxy-proto`: `ControlRequest`/`ControlEvent`
(`#[serde(tag="type", camelCase)]`). Client: agents accumulate in the sidebar (exited ones
stay); the M1a keymap/palette/menu is the place to add commands.

## Goals / Non-goals

**Goals:** from a finished agent, **Land** (finalize its work onto a clean `muxy/<task>`
branch/bookmark and remove the agent) or **Discard** (throw it away + delete the branch/change
and remove the agent), as client commands with confirmation; and **jj repos work** — a
`JjDriver` behind the same trait, auto-selected per project.

**Non-goals (deferred):** auto-merge / rebase / squash / PR strategies (finalize+handoff only —
other strategies could return later behind a choice); jj op-log undo / capability-flag UI
degradation; landing into a moving base or resolving conflicts (muxy never merges, so no
conflicts); per-agent commit-message customization beyond `"muxy: <task>"`.

## Component design

### `muxy-workspace` — trait + git + jj (M3a git, M3c jj)

```rust
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceKind { Git, Jj }

pub struct Workspace { pub path: PathBuf, pub branch: String, pub project: PathBuf, pub kind: WorkspaceKind }

pub trait WorkspaceDriver: Send + Sync {
    fn kind(&self) -> WorkspaceKind;
    fn provision(&self, project: &Path, name: &str) -> Result<Workspace>;
    /// Finalize: commit any uncommitted work onto the branch/bookmark, remove the working
    /// copy, and KEEP the branch/bookmark for the user to integrate.
    fn land(&self, ws: &Workspace) -> Result<()>;
    /// Throw away: remove the working copy and DELETE the branch/change.
    fn discard(&self, ws: &Workspace) -> Result<()>;
}
```
`Workspace` gains a `kind` field (set by `provision`).

**`GitWorktreeDriver`** (M3a):
- `land`: if `git -C <ws.path> status --porcelain` is non-empty →
  `git -C <ws.path> add -A` + `git -C <ws.path> commit -m "muxy: <task>"`; then
  `git -C <project> worktree remove <ws.path>` (clean copy, no `--force`) + `worktree prune`.
  **Keeps** branch `muxy/<task>`. (`<task>` is derivable from `ws.branch` = `muxy/<task>`.)
- `discard`: `git -C <project> worktree remove --force <ws.path>` + `worktree prune` +
  `git -C <project> branch -D muxy/<task>` (force-delete the unmerged branch).
- (The old `teardown` is replaced by `discard`.)

**`JjDriver`** (M3c, `jj` CLI):
- `provision`: `jj -R <project> workspace add --name muxy-<name> <path>` (a jj workspace with
  its own working-copy commit); `Workspace{ kind: Jj, branch: "muxy/<name>", … }`.
- `land`: jj auto-records the working copy as a commit, so there's nothing to `git add`. Set a
  bookmark so the change is findable + kept: `jj -R <path> bookmark set muxy/<name> -r @`; then
  `jj -R <project> workspace forget muxy-<name>` + remove the dir. The bookmark/change remains.
- `discard`: `jj -R <path> abandon -r @` (drop the working-copy change) +
  `jj -R <project> workspace forget muxy-<name>` + remove the dir.
- *(Exact `jj` invocations validated in M3c's plan against the pinned jj version.)*

**Driver selection** (M3c): `pub fn driver_for(project: &Path) -> Arc<dyn WorkspaceDriver>` —
`JjDriver` if the project (or an ancestor up to the repo root) has a `.jj` dir, else
`GitWorktreeDriver`. The daemon calls this at spawn; `workspace.kind` lets land/discard route
to the matching driver later.

### Protocol (`muxy-proto`, M3a)

New `ControlRequest` variants:
```rust
LandAgent { pane: PaneId },
DiscardAgent { pane: PaneId },
```
On success the daemon emits the existing `AgentRemoved { pane }`; a failure (e.g. jj/git error)
emits `Error { message }` and the agent is kept.

### Daemon (M3a git wiring, M3c multi-driver)

- Keep a **git** and (M3c) **jj** driver available; select per project at `spawn_agent` via
  `driver_for(project)`; store the resulting `Workspace{kind}`.
- `land_agent(pane)`: `workspace_of(pane)` → the driver for `ws.kind` → `driver.land(ws)`; on
  Ok, remove the agent (as `teardown_agent` does, minus the workspace-driver call already made)
  + broadcast `AgentRemoved`; on Err, return it (agent kept).
- `discard_agent(pane)`: same shape with `driver.discard(ws)`.
- `teardown_agent` (spawn-failure + `ClosePane(agent)`): route to `discard` — so the
  failure/close path now **cleans up the branch** instead of leaving cruft.
- `control_json`: handle `LandAgent`/`DiscardAgent` → the above → reply `AgentRemoved` / `Error`.

### Client (`MuxyApp`, M3b)

- New `CommandID`s `.landAgent` / `.discardAgent` + keymap defaults + palette rows + menu items,
  acting on `selectedPane`. `AppModel.landSelected()` / `discardSelected()` send the requests.
- **Confirmation sheet** before executing (Discard is destructive; Land modifies repo state):
  Land → "Finalize `<task>` onto branch `muxy/<task>`?"; Discard → "Discard `<task>` — this
  deletes the branch and its work. This can't be undone." On success the row leaves the sidebar
  (via `AgentRemoved`); on `error`, the existing `lastError` banner shows it.
- *(Optional, low priority: a small git/jj kind marker on the sidebar row.)*

## Data flow
```
finished agent ─► Land or Discard command (confirm) ─► LandAgent/DiscardAgent{pane}
   ─► daemon: driver_for(ws.kind).land|discard(ws)
        land:    commit dirty ─► remove worktree/workspace ─► KEEP muxy/<task> branch/bookmark
        discard: remove --force ─► DELETE branch/change
   ─► AgentRemoved ─► row leaves the sidebar        (Error ─► agent kept, banner shows it)
```

## Decomposition (each its own plan → SDD → PR)

- **M3a — lifecycle core (git):** `WorkspaceKind` + `land`/`discard` on the trait +
  `GitWorktreeDriver` impls (replacing `teardown`); `LandAgent`/`DiscardAgent` proto; daemon
  `land_agent`/`discard_agent` + `teardown_agent`→discard + control-channel wiring. Rust,
  unit-tested. (Daemon keeps the single git driver here; multi-driver selection is M3c.)
- **M3b — client lifecycle UX:** Land/Discard commands (keymap + palette + menu) + confirmation
  sheet + `AppModel` actions. Swift (MuxyCore actions unit-tested; UI build+manual).
- **M3c — jj driver + auto-detect:** `JjDriver` (jj CLI) + `driver_for(project)` per-project
  selection in the daemon; kind-routed land/discard. Rust, tested against a temp jj repo (skip
  if `jj` isn't installed).

## Testing

- **M3a (`cargo test`):** git `land` on a dirty worktree commits + removes the worktree + keeps
  the branch (assert branch exists, worktree gone, commit present); `land` on a clean worktree
  keeps the branch, no empty commit; `discard` removes the worktree + deletes the branch;
  `LandAgent`/`DiscardAgent` over the control channel → `AgentRemoved`; the workspace/agent maps
  are cleaned up.
- **M3b (`swift test` + build):** `landSelected`/`discardSelected` send the right requests
  (fake transport); confirmation gating; the agent leaves the store on `AgentRemoved`.
- **M3c (`cargo test`, gated on `jj` present):** in a temp jj repo, `provision` → `land` leaves
  a bookmark + no workspace; `discard` abandons; `driver_for` picks jj for a `.jj` project and
  git otherwise.

## Risks

1. **jj CLI semantics** (change-based, not branch-based) — the finalize (bookmark-set +
   `workspace forget`) and discard (`abandon`) flows must be validated against the pinned jj in
   M3c; contained to `JjDriver`. Gate jj tests on `jj` being installed.
2. **git `land` on a clean vs dirty worktree** — only commit when `status --porcelain` is
   non-empty, so a clean agent doesn't get a spurious empty commit.
3. **Destructive Discard** — irreversible (deletes an unmerged branch/change); the client
   confirmation is the guard. Land is non-destructive (keeps the branch).
4. **Concurrent land/discard while an agent still running** — the agent process may hold the
   worktree; `land`/`discard` should still work after the agent exits. Scope Land/Discard to
   finished (exited/completed) agents in the UI, or accept that landing a live agent kills it
   (the daemon can `discard_agent`'s teardown already kills the pane). Define in M3a: land/discard
   tears the pane down first, then runs the driver op.

## Verification gate

Per slice: `cargo test` / `swift test` green for that slice's tests + all existing. End state:
from a finished agent, Land leaves a clean `muxy/<task>` branch (git) or bookmark (jj) and the
row disappears; Discard deletes it; jj repos auto-use the jj driver. Manual confirmation by the
user running the app (Land/Discard from the palette; inspect the repo).
