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
| `crates/clowder-config` | Fully-resolved `Config` (sockets, backlog cap, shell, pane size); loads `config.toml` then env overrides (**env › file › default**) | lib |
| `crates/clowder-daemon` | Headless daemon: agent PTYs in panes, attention routing/notify, control-JSON + hook servers, split-tree, single-instance lock | **`clowder-daemon`** |
| `crates/clowder-client` | Client library + interactive attach (raw-mode terminal); the `clowder` CLI | **`clowder`** |
| `crates/clowder-hook` | Sends exactly one `HookEvent` to the daemon's hook socket (agent lifecycle shim) | **`clowder-hook`** |
| `crates/clowder-vt` | Headless scanner for terminal attention signals (BEL, OSC 9, OSC 777) via `vte` — signal detection only, no cell grid | lib |
| `crates/clowder-workspace` | Per-agent worktree provisioning: `WorkspaceDriver` (`GitWorktreeDriver` / jj), `WorkspaceKind {Git, Jj}`, provision/land/discard | lib |
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

## Runtime model

The daemon owns the agent PTYs and binds three Unix sockets under `<runtime_dir>/clowder/`, where
`runtime_dir = $XDG_RUNTIME_DIR › $TMPDIR › /tmp`:

- `clowder.sock` (client / render) — env override `CLOWDER_SOCK`
- `clowder-control.sock` (JSON control) — `CLOWDER_CONTROL_SOCK`
- `clowder-hook.sock` (agent hooks) — `CLOWDER_HOOK_SOCK`

Config file: `$XDG_CONFIG_HOME/clowder/config.toml` (else `~/.config/clowder/config.toml`); other keys:
`CLOWDER_BACKLOG_CAP` (default 262144), `SHELL`, default 80×24. The app runs `clowder attach <pane>` in a
libghostty surface. **Adapters:** `claude` (Claude Code), `codex` (OpenAI Codex), `shell` (plain shell,
no hooks). The `clowder` CLI: `clowder spawn <project> <task> [adapter]` and `clowder attach <pane-id>`.
An optional remote TCP listener (`[remote] listen`/`host`) can be hardened with `[remote] tls`/`token`
(bearer-token auth + TOFU-pinned TLS) — see `docs/remote-tls.md` for setup and the threat model.

## Gotchas

- **Cargo:** always `source "$HOME/.cargo/env" && cargo …`.
- **libghostty:** `clowder-app` links a gitignored 189 MB `macos/vendor/libghostty/ghostty-internal.a`.
  Build it with `scripts/build-libghostty.sh` — needs **zig 0.16.0** and **full Xcode** (Metal shader
  compiler; not in CLT). `ClowderCore`/`swift test` do **not** need it.
- **Dev run:** an unbundled build (`swift run clowder-app`) does **not** auto-spawn the daemon — run
  `cargo run -p clowder-daemon` yourself and set `CLOWDER_BIN` to the `clowder` binary
  (`CLOWDER_BIN="$PWD/../target/debug/clowder"`). The packaged `.app` auto-spawns + supervises its bundled
  daemon.
- **Tags:** this repo requires **annotated** tags — `git tag -a vX.Y.Z -m "…"` (plain `git tag vX.Y.Z`
  fails with "no tag message?").
- **Editor diagnostics:** ignore stale SourceKit "No such module" / "cannot find type" errors — trust
  the actual `swift build` / `swift test` output.
- **Versioning:** `VERSION` is canonical; bump via `scripts/set-version.sh` and commit the regenerated
  `Cargo.lock` (CI runs `cargo test --workspace --locked`).
- **Known behavior:** agents do **not** survive a daemon restart (PTYs are daemon children); the daemon
  holds a single-instance advisory `flock` and exits **code 3** if another instance already owns it.

## CI

- `.github/workflows/ci.yml` (**CI**) — on push to `main` + PRs, `runs-on: macos-15`: select Xcode 16,
  install zig (Homebrew) + Rust, cache/build libghostty, `cargo test --workspace --locked`,
  `swift test` (ClowderCore), assemble the unsigned `Clowder.app`, upload it as an artifact. **Must be green.**
- `.github/workflows/release.yml` (**Release**) — on `v*` tags, builds + publishes a GitHub Release with
  the unsigned zipped app.
- Anything touching libghostty/the app requires full Xcode; the libghostty build is cached by the
  pin/patch hash.

## Conventions

- The design/implementation workflow lives in `docs/superpowers/` — **spec → plan → subagent-driven
  execution → PR**, one milestone per cycle. Read the relevant spec/plan before non-trivial changes.
- Work on feature branches; open a PR into `main`; keep CI green. Don't commit to `main` directly.
