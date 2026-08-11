# AGENTS.md

Guidance for AI coding agents and contributors working in the clowder repo. (`CLAUDE.md` imports this
file.)

## Overview

clowder is a cross-platform agent-orchestrator terminal. A headless **Rust daemon** (`clowder-daemon`) runs a
fleet of CLI coding agents, each isolated in its own git worktree / jj workspace, with attention
routing; a native **SwiftUI macOS app** (`clowder-app`) embeds **libghostty** and renders each agent by
running `clowder attach <pane>` as a terminal command (a tmux-style client/server split). A JSON control
socket drives the app's sidebar, spawning, and splits.

## Repo layout

| Crate / dir | Role | Binary |
|---|---|---|
| `crates/clowder-proto` | Wire protocol: `ClientToDaemon`/`DaemonToClient`, `HookEvent`, `PaneId`, `MsgStream` (postcard), and control types (`ControlRequest`/`ControlEvent`, `PaneTree`, splits) | lib |
| `crates/clowder-config` | Fully-resolved `Config` (sockets, backlog cap, shell, pane size, worktree base); loads `config.toml` then env overrides (**env › file › default**) | lib |
| `crates/clowder-daemon` | Headless daemon: agent PTYs in panes, attention routing/notify, control-JSON + hook servers, split-tree, single-instance lock, `agent_profiles.rs` (the agent-profile store served over the control socket) | **`clowder-daemon`** |
| `crates/clowder-client` | Client library + interactive attach (raw-mode terminal); the `clowder` CLI | **`clowder`** |
| `crates/clowder-hook` | Sends exactly one `HookEvent` to the daemon's hook socket (agent lifecycle shim) | **`clowder-hook`** |
| `crates/clowder-vt` | Headless scanner for terminal attention signals (BEL, OSC 9, OSC 777) via `vte` — signal detection only, no cell grid | lib |
| `crates/clowder-workspace` | Per-agent worktree provisioning: `WorkspaceDriver` (`GitWorktreeDriver` / jj), `WorkspaceKind {Git, Jj}`, provision/land/discard; `WorktreeLayout` owns where worktrees go (outside the project) | lib |
| `macos/` | SwiftPM package: `ClowderCore` (lib, libghostty-free, unit-tested) + `clowder-app` (exe, links vendored libghostty via `GhosttyKit`). The Settings window (⌘,) has two panes: `SettingsView` → `HostsSettingsView` (list + editor) → `HostEditorView` → `PairingSheet`, and `SettingsView` → `AgentsSettingsView` → `AgentEditorView`. All of them render only — every decision (validation, add/edit/remove/pair, argument parsing) lives in `ClowderCore`'s `HostsViewModel` / `AgentsViewModel` / `AgentArgs`, since `clowder-app` has no test target | — |
| `scripts/` | `build-app.sh`, `build-libghostty.sh`, `set-version.sh`, `gen-icon.swift` | — |
| `docs/` | `superpowers/` (design specs + plans), `versioning.md`, `building-libghostty.md`, `code-signing.md` | — |

## Build & test

**Rust — prefix every cargo command with `source "$HOME/.cargo/env" && `** (rustup is not auto-sourced
in this environment). Edition 2021; stable toolchain (no in-repo `rust-toolchain` file).

```sh
source "$HOME/.cargo/env" && cargo build                 # debug
source "$HOME/.cargo/env" && cargo test --workspace      # CI runs this with --locked
```

**Swift** (run inside `macos/`):

```sh
cd macos && swift test         # ClowderCore unit tests — builds the WHOLE graph first (see below)
cd macos && swift build        # builds + LINKS clowder-app — REQUIRES the vendored libghostty
cd macos && swift build -c release
```

**`swift test` builds the whole package graph, `ClowderApp` included.** Only `ClowderCoreTests`
runs, but **a compile error anywhere in `ClowderApp` aborts `swift test` before a single test
runs** — you get a compiler error, not a test failure. The practical consequence: a change to a
`ClowderCore` signature that breaks a `ClowderApp` call site must fix that call site in the *same*
commit, or the commit neither tests nor builds.

**`swift test` also LINKS `clowder-app`, so it does need the vendored libghostty.** Verified
2026-08-10 in a fresh worktree with no `macos/vendor/` and an empty `.build`: it fails at
`[45/71] Linking clowder-app` with `no such file or directory: …/ghostty-internal.a`. (This
supersedes an earlier note here claiming otherwise — that was measured against a warm `.build`,
where the executable was already linked and SwiftPM had nothing to relink.)

To compile-check Swift **without** the archive, build the targets individually — a `--target` build
stops at the module, so there is no link step:

```sh
cd macos && swift build --target ClowderApp        # catches the ClowderApp break described above
cd macos && swift build --target ClowderCoreTests  # + ClowderCore, transitively
```

Both verified 2026-08-10 with no `macos/vendor/`: they succeed, and a deliberate type error in
`ClowderApp` makes the first one fail. They do **not** run any tests — for that you still need the
archive.

**Scripts** (`scripts/`):

| Script | Purpose | Usage |
|---|---|---|
| `build-libghostty.sh` | Reproducibly build the vendored `ghostty-internal.a` + `ghostty.h` from pinned ghostty (zig 0.16 + full Xcode) | `scripts/build-libghostty.sh` |
| `build-app.sh` | Build release binaries + app, assemble `dist/Clowder.app` (bundles `clowder-daemon`/`clowder`/`clowder-hook`) | `scripts/build-app.sh [out-dir]` |
| `set-version.sh` | Set the version everywhere from `VERSION` (Cargo `[workspace.package]` + refresh `Cargo.lock`) | `scripts/set-version.sh <X.Y.Z>` |
| `gen-icon.swift` | Render the placeholder app icon PNG (called by `build-app.sh`) | `swift scripts/gen-icon.swift <out.png> [size]` |
| `check-commit-messages.sh` | Verify this branch's non-merge commits are Conventional Commits (CI runs it on every PR) | `scripts/check-commit-messages.sh [base] [head]` |
| `next-version.sh` | Derive the next release version from the commits since the last tag (used by `release.yml`) | `scripts/next-version.sh [--notes\|--self-test]` |
| `check-runs-state.sh` | Classify a commit's check runs against the ruleset's required set (the release workflow's merge gate) | `scripts/check-runs-state.sh --sha <sha>\|--self-test` |
| `lib/conventional.sh` | The Conventional Commits grammar, sourced by both of the above so they can't drift | (sourced, not run) |

## Runtime model

The daemon owns the agent PTYs and binds three Unix sockets under `<runtime_dir>/clowder/`, where
`runtime_dir = $XDG_RUNTIME_DIR › $TMPDIR › /tmp`:

- `clowder.sock` (client / render) — env override `CLOWDER_SOCK`
- `clowder-control.sock` (JSON control) — `CLOWDER_CONTROL_SOCK`
- `clowder-hook.sock` (agent hooks) — `CLOWDER_HOOK_SOCK`

Config file: `$XDG_CONFIG_HOME/clowder/config.toml` (else `~/.config/clowder/config.toml`); other keys:
`CLOWDER_BACKLOG_CAP` (default 262144), `SHELL`, `[env] capture_login` / `timeout_ms`, default 80×24.

**Pane environment.** A GUI-launched `.app` is started by launchd, whose environment is
`PATH=/usr/bin:/bin:/usr/sbin:/sbin` and *no* `SHELL` — useless to an agent. So after binding its
sockets (and before `reconcile()`), the daemon runs `<login shell> -l -i -c` once, reads the result
back as a NUL-delimited `env -0` dump framed by per-run nonce markers, and uses it as the base
environment for **every** PTY child. Both flags matter: `-l` gets `/etc/zprofile` (`path_helper`) and
`~/.zprofile` where Homebrew lands, `-i` gets `~/.zshrc` where nvm/mise and the Claude native
installer land. The daemon still wins on `CLOWDER_*` and `TERM`, and prepends its own bin dir to
`PATH`; `PWD`/`OLDPWD`/`SHLVL`/`_`/`COLUMNS`/`LINES` describe the capture rather than the pane and are
stripped. Disable with `[env] capture_login = false` / `CLOWDER_CAPTURE_LOGIN_ENV=0`; timeout via
`[env] timeout_ms` / `CLOWDER_LOGIN_ENV_TIMEOUT_MS` (3 s, clamped to 1–30 s). On failure or timeout
the daemon warns to `daemon.log` and panes inherit its own environment (the pre-#76 behaviour).

The **login shell** resolves `$SHELL` › `[pane] shell` › `getpwuid(getuid())->pw_shell` › `/bin/sh`.
The passwd tier is not optional: launchd sets no `SHELL`, and a login `zsh` does not export one
either, so capturing a login environment cannot supply it. See
`docs/superpowers/specs/2026-08-10-clowder-login-env-capture-design.md`.

**The clipboard is libghostty's, routed through the app.** libghostty owns no clipboard — it calls
back into the app for every read and write, and each callback is handed the *surface's* userdata
(which `SurfaceView` sets to itself), so there is no app-level surface registry. Consequences worth
knowing: **copy-on-select is libghostty's own** (`copy-on-select` is default-true on macOS) and only
works because `supports_selection_clipboard = true`; both clipboard kinds land on
`NSPasteboard.general` since macOS has no primary selection. **Paste protection is on**, so a
multi-line paste outside bracketed-paste mode prompts — a paste that "does nothing" is usually a
missed confirmation, not a broken callback. **OSC 52** follows ghostty's defaults: writes allowed,
reads prompt. Declining any prompt completes the request with empty text but `confirmed: true` on
purpose: completing an OSC 52 read with `false` re-raises `UnauthorizedPaste` and loops the prompt
forever. Copy/Paste/Select All ride AppKit's **stock** Edit menu via `copy:`/`paste:`/`selectAll:` on
`SurfaceView`; Cut is greyed out because `cut:` is deliberately not implemented (scrollback is not
editable). Right-click shows a context menu only when `ghostty_surface_mouse_button` reports the
press unconsumed — a mouse-reporting program like vim consumes it. See
`docs/superpowers/specs/2026-08-11-clowder-clipboard-design.md`.

**Worktrees live outside the project** (`[worktrees] base` / `CLOWDER_WORKTREE_BASE`), defaulting to
`$XDG_DATA_HOME/clowder/worktrees` › `~/.local/share/clowder/worktrees`. The per-agent path is
`<base>/<project-basename>-<hash12>/<name>`, so two repos with the same name never collide. Pre-#65
worktrees at `<project>/.clowder/worktrees/<name>` are **not migrated** — they keep working, since
the daemon resumes from the absolute path in `agents.json`. The app runs `clowder attach <pane>` in a
libghostty surface. **Adapters:** `claude` (Claude Code), `codex` (OpenAI Codex), `shell` (plain shell,
no hooks). The `clowder` CLI: `clowder spawn <project> <task> [adapter]` and `clowder attach <pane-id>`.

The spawnable list is **not** the adapter list: it is the set of enabled **agent profiles** — named
wrappers around those adapters, each with an argument template appended to the adapter's own args —
stored per-daemon in `$XDG_STATE_HOME/clowder/agent-profiles.json` (`CLOWDER_AGENT_PROFILES_FILE`
overrides) and managed with `clowder agent list|add|set|enable|disable|rm` or the Settings window's
Agents tab. The file holds only deltas: built-ins always exist (disable-able, not deletable) and
appear even if the file is empty. Template tokens (`{{project_name}}`, `{{project_path}}`,
`{{workspace_name}}`, `{{workspace_path}}`, `{{branch}}`) are substituted **per already-split
argument** at spawn, and the resolved arguments are recorded on the agent, so editing or deleting a
profile never changes what a running agent resumes with.

An optional remote TCP listener (`[remote] listen`/`host`) can be hardened with `[remote] tls`/`token`
(bearer-token auth + TOFU-pinned TLS) — see `docs/remote-tls.md` for setup and the threat model. Remote
daemons the client knows about are managed as a nicknamed registry
(`clowder remote add|list|show|set|rm|probe|trust|untrust`) in `$XDG_STATE_HOME/clowder/hosts.json`
(`CLOWDER_HOSTS_FILE` overrides), a file kept `0600` because it holds bearer tokens; `[remote] host` in `config.toml` still works and appears in
the registry as a read-only entry (`source: config`).

**The macOS app supervises one backend per host** (`AppDelegate.supervisors: [BackendID: DaemonSupervisor]`)
and switches which one is active — from the sidebar connection chip, the menu bar, or the command
palette (⌘K). Switching *away* from Local **detaches** its supervisor rather than terminating it:
local agents are PTY children of the local `clowder-daemon`, and they do not survive that process
dying, so the daemon is left running unsupervised and switching back **re-adopts the same daemon**
(`resume()`) instead of relaunching it. Switching away from a *remote* host instead **stops and drops**
its supervisor, since a `clowder connect` forwarder holds no state of its own. **Quitting the app always
terminates every backend it launched, including a detached local daemon** — `applicationWillTerminate`
calls `stop()` on every supervisor it holds, detached or not, so a switch-and-quit never leaves an
orphaned daemon behind. `DaemonSupervisor` also treats `clowder connect`'s exit code 4 ("the first dial
never landed") as terminal: instead of relaunching forever, it enters `.failed` and waits for the user to
retry.

For a remote backend, the app launches `clowder connect <host> --socket-dir <dir>` with
`dir = <runtime_dir>/clowder/remote/<host>` (`forwarderSocketDir` in
`macos/Sources/ClowderCore/BackendPlan.swift`), so each host's forwarder gets its own socket directory
and two hosts' forwarders never collide. `ClowderCore/RemotePaths.swift` is gone — it used to duplicate
that path rule in Swift alongside the Rust forwarder's own (flat, non-per-host) default; `BackendPlan`
is now the one place that computes it, and the app is the only caller that passes `--socket-dir` at all.

## Gotchas

- **Cargo:** always `source "$HOME/.cargo/env" && cargo …`.
- **libghostty:** `clowder-app` links a gitignored 189 MB `macos/vendor/libghostty/ghostty-internal.a`.
  Build it with `scripts/build-libghostty.sh` — needs **zig 0.16.0** and **full Xcode** (Metal shader
  compiler; not in CLT). `swift test` does **not** need the archive (it never links the executable),
  but it **does compile `ClowderApp`** — so a compile error there breaks `swift test` even though no
  app code is under test. See the Swift section under Build & test.
- **Dev run:** an unbundled build (`swift run clowder-app`) does **not** auto-spawn the daemon — run
  `cargo run -p clowder-daemon` yourself and set `CLOWDER_BIN` to the `clowder` binary
  (`CLOWDER_BIN="$PWD/../target/debug/clowder"`). The packaged `.app` auto-spawns + supervises its bundled
  daemon.
- **Tags:** release tags are created by CI — don't tag by hand. A bare `git tag vX.Y.Z` fails locally
  with "no tag message?" because of `tag.gpgsign = true` in the maintainer's **global** git config,
  *not* a repo hook; CI has no such config, which is why `release.yml` passes `-a` explicitly (without
  it CI would silently create a lightweight tag).
- **Editor diagnostics:** ignore stale SourceKit "No such module" / "cannot find type" errors — trust
  the actual `swift build` / `swift test` output.
- **Versioning:** `VERSION` is canonical; bump via `scripts/set-version.sh` and commit the regenerated
  `Cargo.lock` (CI runs `cargo test --workspace --locked`).
- **Known behavior:** agents do **not** survive a daemon restart (PTYs are daemon children); the daemon
  holds a single-instance advisory `flock` and exits **code 3** *only* when another instance already
  owns it (any other startup failure exits 1, which the app retries — code 3 makes it yield for good).
- **Daemon logs:** the app redirects the daemon's stdout/stderr to
  `$XDG_STATE_HOME/clowder/daemon.log` › `~/.local/state/clowder/daemon.log`. A GUI-launched `.app`
  has no terminal, so this is the *only* place startup failures are visible — check it first when the
  app can't reach the daemon. Appends across relaunches; truncated past 4 MB.
- **Agent "command not found":** almost always the pane environment, not the adapter. `portable-pty`
  resolves a bare program name (`claude`, `codex`) **in the parent, before forking**, against the
  `CommandBuilder`'s *own* `PATH` — which `Pane::spawn` sets from the daemon's `PaneEnv` after an
  `env_clear()`. So the answer is always the `login-env captured` line in `daemon.log`, never the
  daemon's inherited environment. (It also tries `<cwd>/<program>` *before* `PATH` for a relative
  name, so a file named `claude` in a worktree would win — pre-existing, not currently guarded.)

## CI

- `.github/workflows/ci.yml` (**CI**) — on push to `main` + PRs, `runs-on: macos-15`: select Xcode 16,
  install zig (Homebrew) + Rust, cache/build libghostty, `cargo test --workspace --locked`,
  `swift test` (ClowderCore), assemble the unsigned `Clowder.app`, upload it as an artifact. **Must be green.**
- `.github/workflows/release.yml` (**Release**) — **manually dispatched** (Actions → Release → Run
  workflow), never on a tag. Computes the next version from the Conventional Commit types since the
  last `v*` tag, opens + merges a `chore: vX.Y.Z` bump PR, then builds, tags, and publishes. Does
  nothing when no `feat`/`fix`/`perf` commits have landed. See `docs/versioning.md`.
- Anything touching libghostty/the app requires full Xcode; the libghostty build is cached by the
  pin/patch hash.

## Conventions

- The design/implementation workflow lives in `docs/superpowers/` — **spec → plan → subagent-driven
  execution → PR**, one milestone per cycle. Read the relevant spec/plan before non-trivial changes.
- Work on feature branches; open a PR into `main`; keep CI green. Don't commit to `main` directly.
- **Commit messages are Conventional Commits** — `type(scope): subject`, with `type` one of `feat`,
  `fix`, `docs`, `test`, `refactor`, `perf`, `ci`, `chore`, `build`, `style`, `revert`; scope
  optional and free-form (`daemon`, `app`, `m10c`, `proto,daemon` are all fine); `!` before the colon
  for a breaking change. The type also **drives the released version** — `feat` → minor, `fix`/`perf`
  → patch, `!` → major (folded to minor while the version is `0.x`); see `docs/versioning.md`. So a
  wrong type is not just a lint failure, it mis-versions the next release.
  PRs merge with a merge commit, so **every** commit on the branch keeps its
  subject in `main`'s history — CI checks them individually. Run
  `scripts/check-commit-messages.sh` before pushing.
