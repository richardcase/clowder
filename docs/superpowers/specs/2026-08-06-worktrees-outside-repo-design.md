# Worktrees outside the project (issue #65)

**Status:** accepted · **Date:** 2026-08-06 · **Issue:** [#65](https://github.com/richardcase/clowder/issues/65)

## Problem

Every agent worktree is created *inside* the user's repo at `<project>/.clowder/worktrees/<name>`.

That has three costs:

- It pollutes the project. Every managed repo needs its own `.gitignore` entry, and nothing in
  clowder adds one — so by default the agent fleet shows up as untracked noise in the very repo the
  agents are editing.
- Agent state lives in the tree the agent is modifying. An agent running `git clean` or a
  broad `rm` in its own project can reach its siblings.
- The path is duplicated in four places (see below), so the daemon's collision pre-check and the
  drivers agree only by coincidence.

## Goals

- A fresh `clowder spawn` leaves the project directory completely untouched.
- Worktrees default to an XDG location.
- The location is overridable by settings, so a user can put the fleet on a fast external volume.
- Existing worktrees keep working. No migration, no flag day.

## Non-goals

- Migrating pre-existing in-repo worktrees. They stay where they are.
- Per-project worktree bases. Global configuration only.
- Changing worktree *naming* rules (`validate_workspace_name` and its Swift mirror are untouched).

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Default base | `$XDG_DATA_HOME/clowder/worktrees` › `$HOME/.local/share/clowder/worktrees` › `/tmp/clowder/worktrees` | Worktrees hold uncommitted user work. `DATA` is the correct XDG category; `STATE` is for logs/history and `CACHE` is deletable. |
| Layout | `<base>/<slug>-<hash12>/<name>` | Readable *and* collision-free — two different repos both named `api` must not share a directory. |
| Migration | None | Existing agents resume from the absolute path persisted in `AgentRecord::worktree_path`, so they keep working untouched. Old directories drain naturally as agents are landed or discarded. |
| Override | `[worktrees] base` in `config.toml` + `CLOWDER_WORKTREE_BASE` | Matches the established env › file › default convention. Smallest surface that satisfies the issue. |
| Enclosing-repo hazard | Write a `*` `.gitignore` at the base on first provision | See "Hazards" below. |

## The duplication being removed

`project.join(".clowder").join("worktrees").join(name)` appears in four places today:

| Site | Role |
|---|---|
| `clowder-workspace/src/lib.rs` `GitWorktreeDriver::provision` | authoritative |
| `clowder-workspace/src/lib.rs` `JjDriver::provision` | authoritative |
| `clowder-daemon/src/server.rs` `spawn_agent` | pre-flight collision check |
| `clowder-daemon/src/projects.rs` `ProjectStore::add` | "don't add a worktree as a project" guard |

The branch scheme `clowder/{name}` and the jj workspace name `clowder-{name}` are likewise inlined
at seven sites between them. Consolidating all of this behind one value type is the core of the
change: the collision check and the driver must agree **by construction**, not by two copies of the
same expression drifting apart.

## Design

### `WorktreeLayout` — one place that knows where worktrees go

A new `clowder-workspace/src/layout.rs` holds the path policy as a value type:

```rust
pub struct WorktreeLayout { base: PathBuf }

impl WorktreeLayout {
    pub fn new(base: impl Into<PathBuf>) -> Self;
    pub fn base(&self) -> &Path;
    pub fn project_dir(&self, project: &Path) -> PathBuf;      // <base>/<slug>-<hash>
    pub fn worktree_path(&self, project: &Path, name: &str) -> PathBuf;
    pub fn prepare(&self, project: &Path, name: &str) -> Result<PathBuf>;
}
```

Concerns split by whether they need the base:

| Concern | Needs base? | Home |
|---|---|---|
| `project_dir`, `worktree_path`, `prepare` | yes | `WorktreeLayout` methods |
| `branch_name` / `task_from_branch` | no | free functions |
| `jj_workspace_name` | no | free function |

That split is what keeps `land` and `discard` unchanged: they need only the base-independent half.
They already operate on the stored `ws.path` / `ws.project` and never recompute a path, so relocating
worktrees does not affect them. Git worktrees and jj workspaces both work across a filesystem
boundary — a linked worktree's `.git` is a pointer file, not a hardlink.

The base arrives as **data**, so `clowder-workspace` gains no dependency on `clowder-config` and
stays `anyhow`-only.

### The driver seam

```rust
fn provision(&self, layout: &WorktreeLayout, project: &Path, name: &str) -> Result<Workspace>;
```

Two alternatives were rejected:

- **Base in the driver constructors.** `driver_for_kind` exists solely to route `land`/`discard`,
  neither of which needs a base — every such call would carry a dead parameter that misrepresents
  the type's dependencies.
- **A precomputed `dest: &Path`.** This splits naming policy across crates (destination in the
  daemon, branch and workspace names in the driver) and creates an unenforced correspondence
  invariant: `provision(project, "feat", "/somewhere/else/other")` would type-check.

`prepare` also folds in the `create_dir_all` that only the jj driver does today, so the git driver
stops relying on `git worktree add`'s implicit leading-directory creation. One behaviour, one place.

### Canonical paths are load-bearing

`/tmp/api` and `/private/tmp/api` hash differently. `WorktreeLayout` deliberately does **not**
canonicalize internally — doing so would make the path depend on the directory currently existing,
so a temporarily-unmounted project would silently relocate its own worktrees. Both real callers
canonicalize before they reach the layout, and the contract is documented on the method.

### The hash

Inline FNV-1a 64, keeping the leading 12 hex digits.

`std::collections::hash_map::DefaultHasher` is explicitly documented as *not* stable across Rust
releases. Since this value lands in a path, a toolchain bump would silently relocate every project
directory and orphan every worktree on disk while the persisted `AgentRecord::worktree_path` still
pointed at the old ones. So the algorithm is spelled out and pinned by a golden test.

Leading digits rather than a truncated low word, because FNV propagates low→high and the high bits
are better mixed. 12 hex (48 bits) rather than git's conventional 8 (32 bits, which gives roughly a
one-in-a-million collision at only ~100 projects); four extra characters cost nothing in a path
nobody types.

### Configuration

`Config` gains `worktree_base: PathBuf`, resolved by the existing pure `Config::resolve` (env only
via the injected `get_env`, so tests drive the same code path as production). An empty value from
*either* source is treated as unset — unlike the socket keys — because an empty base would silently
provision relative to the daemon's working directory.

The `/tmp` last-resort fallback is a data-loss footgun, since macOS periodically purges `/tmp`. It is
kept for consistency with the existing `remote_state_dir()` chain, and documented as such.

### The "don't add a worktree as a project" guard

`ProjectStore` receives the same `WorktreeLayout` instance the daemon uses, constructed once in
`Daemon::new_with_paths`, so the store and the spawner cannot disagree about where worktrees live.

The guard gains a third rule and keeps the first two:

1. inside a registered project's `layout.project_dir(...)` — the new location;
2. inside a registered project's legacy `<project>/.clowder/worktrees` — kept indefinitely, since
   there is no migration;
3. anywhere under `layout.base()` — a worktree whose project is no longer registered is still not a
   project.

Rule 3 is not merely defensive. If the base sits inside another repo (see below), `detect_kind`
returns `Some` for *any* subdirectory of the base, so without it `add` would happily register
`<base>/api-abc123def456` as a git project.

## Hazards

### The base can sit inside someone else's repo

The default base is under `$XDG_DATA_HOME`, and people version-control dotfiles with `~` or
`~/.local/share` as the repo root (chezmoi, yadm). Consequences:

- **git:** every agent worktree shows as untracked in the outer repo. Noisy but survivable.
- **jj:** materially worse. jj auto-snapshots the working copy on every command, so a colocated
  outer jj repo would suck the entire agent fleet — `node_modules`, build artifacts, everything —
  into its operation log.
- **`ProjectStore::add`:** every worktree directory looks like a valid project (closed by rule 3
  above).

Mitigation: `prepare` writes a `.gitignore` containing `*` at the base, best-effort and only if
absent. Both git and jj honour it, and it sits *above* the linked worktrees, so neither their
contents nor their own indexes are affected. A startup warning was considered and rejected — the jj
failure mode is bad enough that a log line nobody reads is not adequate.

### The collision pre-check weakens if the base changes

`spawn_agent`'s path check only looks under the *current* base, so editing `[worktrees] base` means a
name colliding with a worktree under the *old* base is no longer caught by the path check.
`branch_exists` still catches it for git. For jj it may not, since `land` sets a jj bookmark while
`branch_exists` shells out to `git show-ref`, which only sees it in a colocated repo. This is a
pre-existing weakness, marginally widened; noted in a comment, not fixed here.

## Compatibility

- **Existing worktrees keep working.** `resume_agent` rebuilds a `Workspace` purely from the
  persisted record and never re-derives the path, so pre-#65 agents survive a daemon restart
  unchanged. `land`/`discard` on them work for the same reason.
- **The registry format does not change.** `AgentRecord` is untouched; old records round-trip.
- **The wire protocol does not change.** `WorktreeInfo` carries `project`/`name`/`branch`, no path.
- **The macOS app does not change.** Nothing under `macos/` constructs a worktree path.
- **`.gitignore`'s `/.clowder/` entry stays**, covering pre-#65 in-repo worktrees, which are
  deliberately not migrated.
