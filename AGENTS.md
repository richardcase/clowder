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
| `crates/clowder-daemon` | Headless daemon: agent PTYs in panes, attention routing/notify, control-JSON + hook servers, split-tree, single-instance lock | **`clowder-daemon`** |
| `crates/clowder-client` | Client library + interactive attach (raw-mode terminal); the `clowder` CLI | **`clowder`** |
| `crates/clowder-hook` | Sends exactly one `HookEvent` to the daemon's hook socket (agent lifecycle shim) | **`clowder-hook`** |
| `crates/clowder-vt` | Headless scanner for terminal attention signals (BEL, OSC 9, OSC 777) via `vte` — signal detection only, no cell grid | lib |
| `crates/clowder-workspace` | Per-agent worktree provisioning: `WorkspaceDriver` (`GitWorktreeDriver` / jj), `WorkspaceKind {Git, Jj}`, provision/land/discard; `WorktreeLayout` owns where worktrees go (outside the project) | lib |
| `macos/` | SwiftPM package: `ClowderCore` (lib, libghostty-free, unit-tested) + `clowder-app` (exe, links vendored libghostty via `GhosttyKit`) | — |
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
cd macos && swift test         # ClowderCore unit tests — fast, does NOT need libghostty
cd macos && swift build        # builds clowder-app — REQUIRES the vendored libghostty (see gotchas)
cd macos && swift build -c release
```

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
`CLOWDER_BACKLOG_CAP` (default 262144), `SHELL`, default 80×24.

**Worktrees live outside the project** (`[worktrees] base` / `CLOWDER_WORKTREE_BASE`), defaulting to
`$XDG_DATA_HOME/clowder/worktrees` › `~/.local/share/clowder/worktrees`. The per-agent path is
`<base>/<project-basename>-<hash12>/<name>`, so two repos with the same name never collide. Pre-#65
worktrees at `<project>/.clowder/worktrees/<name>` are **not migrated** — they keep working, since
the daemon resumes from the absolute path in `agents.json`. The app runs `clowder attach <pane>` in a
libghostty surface. **Adapters:** `claude` (Claude Code), `codex` (OpenAI Codex), `shell` (plain shell,
no hooks). The `clowder` CLI: `clowder spawn <project> <task> [adapter]` and `clowder attach <pane-id>`.
An optional remote TCP listener (`[remote] listen`/`host`) can be hardened with `[remote] tls`/`token`
(bearer-token auth + TOFU-pinned TLS) — see `docs/remote-tls.md` for setup and the threat model. Remote
daemons the client knows about are managed as a nicknamed registry (`clowder remote add|list|show|set|
rm|probe|trust|untrust`) in `$XDG_STATE_HOME/clowder/hosts.json` (`CLOWDER_HOSTS_FILE` overrides), a file
kept `0600` because it holds bearer tokens; `[remote] host` in `config.toml` still works and appears in
the registry as a read-only entry (`source: config`).

## Gotchas

- **Cargo:** always `source "$HOME/.cargo/env" && cargo …`.
- **libghostty:** `clowder-app` links a gitignored 189 MB `macos/vendor/libghostty/ghostty-internal.a`.
  Build it with `scripts/build-libghostty.sh` — needs **zig 0.16.0** and **full Xcode** (Metal shader
  compiler; not in CLT). `ClowderCore`/`swift test` do **not** need it.
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
