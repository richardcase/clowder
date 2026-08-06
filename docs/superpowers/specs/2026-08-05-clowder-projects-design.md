# clowder M10 — projects and worktrees as first-class entities

## Context

clowder has no concept of a project. The sidebar's project headers are a `Dictionary(grouping:)` over
agent rows keyed by the project's **basename**, so a "project" materialises when its first agent spawns
and vanishes when the last one lands. Starting work means typing a raw filesystem path into a free-text
field with no validation of any kind: the daemon provisions `.clowder/worktrees/<task>` for whatever
string arrives.

The natural workflow — *"here are my repos; give me a worktree in this one"* — is therefore
inexpressible. The app also can't show whether a repo is git or jj, even though the daemon already
determines exactly that at spawn time.

M10 makes both projects and worktrees explicit and durable: the user adds repos once, they persist in
the sidebar with a git/jj badge, and each project is both a place to start worktrees *and* a place to
get a terminal at the repo root.

### The target flow

1. The user adds a project with the toolbar `+` and picks a directory.
2. The directory must be a git or jj repo; the project is then listed in the sidebar with a kind badge.
3. A per-project `+` (or the row's context menu) starts a new worktree/workspace.
4. The user gives a feature name and picks an agent; the existing spawn flow runs unchanged.
5. The new worktree is listed **under** its project in the sidebar.
6. Clicking a **project** gives a terminal at the repo root. Clicking a **worktree** gives the agent.

### The central modelling decision

**A sidebar row under a project is a *worktree*; the agent is a process running inside it.** This is
already how the system behaves — `AttentionState::Exited` exists, the app shows an "Agent exited"
placeholder (`ContentView.swift:109`), and the registry record, the worktree on disk and the branch all
outlive the agent process. Naming the row after the agent was the accident; M10 corrects it.

The immediate payoff is **Restart**: an exited worktree stops being a tombstone whose only actions are
land and discard.

### What exists (ground truth, verified 2026-08-05)

- **`AgentInfo { pane, project, task, state }`** (`crates/clowder-proto/src/lib.rs`) — `project` is the
  **basename**, set in `finalize_agent` (`server.rs:271`) via `ws.project.file_name()`. Two repos named
  `api` collapse into one sidebar group today.
- **Sidebar grouping** is `AgentStore.byProject` (`AgentStore.swift:61`). `orderedAgents` flattens it and
  is the stable index order for Cmd-1…9 and the palette; `attentionCount` derives from it.
- **Selection** is `AppModel.selectedPane: UInt64?` (`AppModel.swift:31`), threaded through
  `currentTree`, `focusedPane`, `splitFocused`, `closeFocused`, `requestLifecycle`, `SplitContainer`.
- **Pane ids are durable.** `reconcile` re-spawns each agent under `PaneId(rec.agent_id)`
  (`server.rs:150`), with `bump_next_id_above` (`server.rs:137`) preventing collisions with freshly
  allocated companions. The pane id **is** the persisted worktree identity.
- **`finalize_agent` already seeds a root pane's split state**: `trees[id] = Leaf { pane: id }` and
  `owner[id] = id` (`server.rs:340`). Project terminals reuse this exact pattern.
- **`finish_agent` tolerates a missing workspace** — `if let Some(ws) = self.workspace_of(pane)`
  (`server.rs:389`). So land/discard on a workspace-less pane would *silently succeed and kill it*
  rather than erroring. **`close_pane` decides agent-ness by `trees.contains_key(&pane)`**
  (`server.rs:619`). Both need explicit guards once terminals have trees.
- **`set_split_ratio` scans every tree** (`server.rs:655`) and `reap_companion` early-returns when
  `owner[pane] == pane` (`server.rs:600`) — both already correct for a terminal root.
- **`driver_for(path)`** (`clowder-workspace/src/lib.rs:158`) walks ancestors for `.jj` (wins) then
  `.git`, and **falls back to `GitWorktreeDriver` when neither is found** — so it cannot answer
  "is this a repo?".
- **`resume_command` defaults to `launch_command`** (`agent.rs:14`); only Claude overrides it
  (`claude --continue`, `agent.rs:82`). Codex and shell have no true resume.
- **Persistence** is `Registry` → `agents.json` (`registry.rs`): a write-lock around
  load-modify-write, unique temp file + atomic rename, corrupt→empty.
- **`reconcile` prunes the registry record when resume fails** (`server.rs:193`) but leaves the worktree
  on disk — an invisible orphan that later collides at `git worktree add`.
- **Remote transport is transparent**: the forwarder is `copy_bidirectional` (`forward.rs:55`) and
  `remote.rs:105` dispatches `Channel::Control` into the same `handle_control_json`. New control
  variants need **no** forwarding work.
- **The app is unsandboxed** — no entitlements in `scripts/build-app.sh` or `docs/code-signing.md` — so
  `NSOpenPanel` yields real paths with no security-scoped bookmarks.
- **`PaletteSearch` already calls `lastPathComponent` on `a.project`** (`PaletteSearch.swift:50`), so a
  basename→path change is transparent there.
- **`Cmd-N` is `spawnAgent`** (`Keymap.swift:53`).

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Sidebar child entity | **Worktree**; the agent is a process in it | Matches how the system already behaves; makes Restart expressible. |
| Wire type | **`AgentInfo` → `WorktreeInfo`** now | `project` is already becoming a breaking change; one migration instead of two. |
| Selection key | **Pane id** | It *is* the durable worktree identity, persisted as `agent_id` and reused by `reconcile`. |
| Exited worktree | **Offer Restart** via `resume_command` | Same path as reconcile, factored into one shared function. |
| Row source of truth | **Live `agents` map**, plus a spawn-time collision pre-check | Keeps `pane` non-optional; makes orphans legible without a speculative shape. |
| Where projects live | **Daemon-side** `projects.json` | Projects are paths on the *daemon's* host; anything app-side is wrong in remote mode and invisible to the CLI. |
| Project terminal | **Lazy** login shell at the repo root, reusing the split tree | Splitting works for free; nothing spawns for projects never opened. |
| Terminal survival | **No** — respawn on next click | Scrollback is lost on restart anyway; `reconcile` stays untouched. |
| Unregistered projects | **Spawn is rejected** | One coherent rule: work happens inside a known project. |
| Migration | **Fresh start** | Pre-existing agents keep running but are hidden until their project is added. No destructive auto-adoption. |
| Removing a project | **Refused while it has worktrees** | No path by which removing a row abandons live work. |
| Feature-name rules | **Validated and rejected daemon-side** | The CLI gets the same guarantee as the app. |
| Collapsed projects | **Attention rollup badge** | Attention routing is the product; a parent row must never hide it. |
| New Worktree entry | **Sheet with a prefilled project picker** | `Cmd-N` stays unconditional, as today. |
| Add Project entry | **One sheet: path field + Browse** | Browse hides in remote mode; one code path for both. |

The migration choice has one consequence worth stating plainly: after upgrading, worktrees whose project
has not been added are **omitted from the sidebar and from `attentionCount`**, though they keep running
and are returned by `listWorktrees`. Adding the project restores them intact.

## Architecture

### 1. `clowder-workspace` — two pure functions

**`detect_kind(path) -> Option<WorkspaceKind>`** — the strict variant of `driver_for`, returning `None`
when neither `.git` nor `.jj` is found at the path or an ancestor. `driver_for` is then rewritten as
`detect_kind(p).map(driver_for_kind).unwrap_or_else(|| Arc::new(GitWorktreeDriver))`, preserving current
behaviour and keeping the `.jj`-wins-over-`.git` rule in one place.

**`validate_workspace_name(name) -> Result<()>`** — the name becomes both a git ref (`clowder/<name>`)
and a path component (`.clowder/worktrees/<name>`), so it must be safe as both. Rules: non-empty, ≤64
chars, `[A-Za-z0-9._-]` only, no leading `.` or `-`, not `.` or `..`, no `..` sequence, no `.lock`
suffix (a git ref rule). Called from `spawn_agent` **before** `driver.provision`.

### 2. `clowder-proto` — `WorktreeInfo`

```rust
pub struct WorktreeInfo {
    project: String,          // canonical path (was a basename)
    name:    String,          // was `task` — the worktree's identity
    branch:  String,          // NEW: clowder/<name>
    pane:    PaneId,          // the agent process; durable, reused across restarts
    state:   AttentionState,
}
```

`pane` stays non-optional: nothing produces a worktree without one, and an `Option` no test could
exercise is speculative. `branch` comes from `workspaces[pane].branch`, already in memory.

### 3. `clowder-daemon` — persistence

**`store.rs` (new)** — extract the mechanics already in `registry.rs` (write-lock, load-modify-write,
unique temp file + atomic rename, corrupt→empty) into `JsonStore<T>` exposing one operation:

```rust
pub fn mutate<R>(&self, f: impl FnOnce(&mut Vec<T>) -> R) -> R
```

`Registry`'s `upsert` / `remove` / `set_tree` are three specialisations of it, and the project store is
a fourth. This is deduplication of code that would otherwise exist twice, not speculative
generalisation; `registry.rs`'s existing concurrency and corrupt-file tests pin the behaviour through
the refactor.

**`projects.rs` (new)**

```rust
pub struct ProjectRecord { pub path: PathBuf, pub kind: String }   // canonical path = identity
```

Stored at `projects.json` beside `agents.json`, same `XDG_STATE_HOME` derivation as
`Registry::default_path`, with a `CLOWDER_PROJECTS_FILE` override for tests.

`add_project(path)`:

1. **`canonicalize()`** — load-bearing on macOS, where `/tmp` resolves to `/private/tmp`. Spawn matches
   on the canonical form, so if only one side canonicalises, every spawn into a `/tmp` project fails
   the registration check.
2. must exist and be a directory;
3. `detect_kind` must return `Some`, else `"not a git or jj repository: <path>"`;
4. reject a path inside any registered project's `.clowder/worktrees/`;
5. already present → idempotent, returns the existing record.

`remove_project(path)`: error if any worktree's project matches; otherwise kill its terminal pane (and
any companions) and drop the record.

### 4. Daemon — spawn guards

`spawn_agent` gains three pre-checks before `driver.provision`, each with a message that says what to do:

1. the project must be registered — `"unknown project: <path> — add it first"`;
2. `validate_workspace_name(name)`;
3. the worktree dir and the branch must both be free — `"a worktree named 'x' already exists at
   <path>; land/discard it or choose another name"`, instead of a raw `git worktree add` failure.

Check 3 is what makes `reconcile`'s orphaned worktrees legible rather than a trap.

### 5. Daemon — restart

`reconcile`'s per-record body factors into `resume_agent(rec) -> Result<()>`: `provision_hooks`,
`resume_command`, `Pane::spawn` under `PaneId(rec.agent_id)`, `finalize_agent`, `restore_layout`.
`reconcile` becomes a loop over `resume_agent`, and the new `restart_worktree(pane)` looks up the
registry record and calls the same function — so restart-by-click and restart-by-daemon-restart cannot
drift apart.

`restart_worktree` additionally: refuses unless the agent's state is `Exited`; aborts the stale exit
watcher; replaces the dead `Arc<Pane>` in `panes` under the same id; leaves `trees`/`owner` untouched so
**live companions survive the restart**; sets attention back to `Working`.

### 6. Daemon — project terminal panes

New state: `project_terms: Mutex<HashMap<PathBuf, PaneId>>` and the reverse
`term_project: Mutex<HashMap<PaneId, PathBuf>>`.

`open_project_terminal(path)`:

- a live pane is already mapped → return it. Idempotent, so a second client selecting the project
  attaches to the same shell.
- otherwise `spawn_pane(companion_command(self.shell.clone(), root), cols, rows)`, reusing
  `companion_command` (`server.rs:28`) verbatim, then seed `trees[pane] = Leaf { pane }` and
  `owner[pane] = pane` — the same two lines `finalize_agent` already runs.
- register an exit watcher that clears both maps and emits `ProjectTerminalClosed`, so a shell the user
  exits is simply respawned on the next click.

**One surgical change:** `split_pane` reads the cwd from `workspaces[agent]` and errors
`"no workspace for agent"` (`server.rs:558`). Extract `root_cwd(agent) -> Option<PathBuf>` returning the
workspace path, else the project-terminal path.

**Two guards**, because the code is permissive rather than restrictive here:

- `land_agent` / `discard_agent` must reject a pane in `term_project` — otherwise `finish_agent`'s
  `if let Some(ws)` skips finalisation and silently kills the terminal.
- `close_pane` must check `term_project` before its `trees.contains_key` agent test, killing the
  terminal and emitting `ProjectTerminalClosed` rather than `AgentRemoved`.

Project terminals are deliberately **not** in the `agents` map, so `list_worktrees` never returns them.
They get **no attention tracking** — the hookless VT scanner would paint the user's own shell red at
every prompt. `shutdown` already kills everything in `panes`, so terminals need no special teardown.

### 7. `clowder-proto` — control surface

```rust
pub struct ProjectInfo { path: String, name: String, kind: String }
```

`name` is derived at the wire boundary (the path's last component) and is not stored in
`ProjectRecord`. There is deliberately no `terminal` field: `OpenProjectTerminal` is idempotent, so a
client can simply ask on select.

New `ControlRequest`: `ListProjects`, `AddProject { path }`, `RemoveProject { path }`,
`OpenProjectTerminal { path }`, `RestartWorktree { pane }`.

New `ControlEvent`: `ProjectList { projects }`, `ProjectAdded { project }`, `ProjectRemoved { path }`,
`ProjectTerminalOpened { path, pane }`, `ProjectTerminalClosed { path }`.

`ProjectTerminalClosed` fires when a terminal's root pane goes away — the user closed it or the shell
exited — telling clients to drop their `path → pane` mapping so the next select respawns.

`SpawnAgent` keeps its wire shape. `AgentList` becomes `WorktreeList`.

A `projects_tx` broadcast channel + `subscribe_projects()` mirrors the existing attention/removed/split
channels, with a matching `select!` arm in `handle_control_json` so every connected client stays in sync.

### 8. `clowder` CLI

Because spawn now rejects unknown projects, add `clowder project <add|list|rm> [path]`, backed by
helpers beside `spawn_via_control` in `crates/clowder-client/src/control.rs`.

### 9. macOS app

**Selection.** In `ClowderCore`:

```swift
public enum SidebarSelection: Hashable { case project(String), worktree(UInt64) }
```

`AppModel.selection` replaces the stored `selectedPane`, but **`selectedPane` survives as a computed
property**: `.worktree(p) → p`, `.project(path) → projectTerminals[path]`. This is what keeps the change
small — `currentTree`, `focusedPane`, `splitFocused`, `closeFocused` and `SplitContainer` all already
mean "the root pane of the current selection", which is exactly what the computed property returns.
Only `requestLifecycle` needs a `.worktree`-only guard.

Selecting a project with no known terminal sends `openProjectTerminal(path)` and shows a
"Starting terminal…" placeholder until `ProjectTerminalOpened` arrives.

**`AgentStore`** gains `projects: [ProjectInfo]` and `projectTerminals: [String: UInt64]`, and
`byProject` becomes `tree: [(project: ProjectInfo, worktrees: [WorktreeInfo])]` — projects sorted by
name, worktrees by pane, worktrees whose project is unregistered omitted. `orderedWorktrees` (and so
`attentionCount`) follows the same rule. A new
`attentionCount(forProject:)` backs the rollup badge.

**Sidebar** becomes a `List(selection: $model.selection)` of `DisclosureGroup`s: a tagged, selectable
project label with worktree rows nested beneath. The project row carries the name, a kind badge
(`arrow.triangle.branch` for git, `point.3.connected.trianglepath.dotted` for jj, each with a `.help`
tooltip), an attention rollup badge, a hover-revealed trailing `+`, and a context menu with
*New worktree…*, *Reveal in Finder*, *Remove project*. Expansion state persists per project path in
`UserDefaults`. An exited worktree's row offers **Restart** in its context menu and on its detail
placeholder, replacing today's dead end.

**Sheets.** `AddProjectSheet` is a path field plus a `Browse…` button that opens `NSOpenPanel`; Browse
is hidden in remote mode, where a local picker would return a path the daemon can't see.
`SpawnSheet` becomes `NewWorktreeSheet` — Project (a picker over registered projects, prefilled from the
selection or last use), Name, Agent.

**Keymap.** `Cmd-N` → New Worktree (unconditional, as today); `Cmd-Shift-N` → Add Project.
`CommandID.spawnAgent` becomes `.newWorktree`, joined by `.addProject` and `.restartWorktree`.

## Delivery — three stacked PRs

| Branch | Base | Contents |
|---|---|---|
| `feat/m10a-worktree-model` | `main` | This spec. §1 `clowder-workspace`, §2 `WorktreeInfo` (+ `Models.swift`), §3 `JsonStore<T>`. Mechanical; no behaviour change. |
| `feat/m10b-projects-daemon` | `m10a` | §3 `projects.rs`, §4 spawn guards, §5 restart, §6 terminals, §7 control surface, §8 CLI. Headless, exercised by control-socket tests + the CLI. |
| `feat/m10c-projects-app` | `m10b` | §9 in full. Pure Swift. |

Each PR targets its parent, so its diff shows only its own changes; GitHub retargets the next to `main`
as each merges.

## Testing

**Rust unit** — `detect_kind` returns `None` for a plain directory and `Some(Jj)` for a colocated repo;
`validate_workspace_name` boundary cases; `JsonStore` passes the existing concurrent-upsert and
corrupt-file tests; project add/remove/duplicate round-trips **including a symlinked temp dir** to pin
canonicalisation; `remove_project` refuses while a worktree is present; spawn rejects an unregistered
project, a bad name, and a colliding worktree — **each leaving nothing behind**.

**Control-socket integration** — extending the existing duplex-stream harness in `control_json.rs`:
`addProject` → `projectList` reports the right kind; spawn into it yields a `WorktreeInfo` whose
`project` is canonical and whose `branch` is `clowder/<name>`; `openProjectTerminal` twice returns the
**same** pane; `splitPane` on a terminal yields a two-leaf `splitTreeChanged`; `landAgent` and
`discardAgent` on a terminal **error**; `closePane` on a terminal emits `ProjectTerminalClosed`;
`restartWorktree` on an exited agent revives it under the same pane id **with its companions intact**,
and is refused while the agent is alive.

**Swift unit** (`cd macos && swift test`, no libghostty needed) — `AgentStore.tree` grouping and
omission of unregistered-project worktrees; `attentionCount(forProject:)`; `SidebarSelection` →
`selectedPane` mapping in both cases; lifecycle commands are no-ops under a `.project` selection.

**End-to-end** — `cargo run -p clowder-daemon`, then `swift run clowder-app` with
`CLOWDER_BIN="$PWD/../target/debug/clowder"` (an unbundled build does not auto-spawn the daemon). Walk
the full flow on a git repo, repeat on a jj repo for the badge and driver; exit an agent and restart it;
collapse a project with a waiting agent and confirm the rollup badge; confirm remove-while-populated is
refused and succeeds after landing.

**CLI** — `clowder project add/list`, then `clowder spawn <registered> feat` succeeds while
`clowder spawn /some/unregistered feat` errors cleanly.
