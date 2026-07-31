# muxy

[![CI](https://github.com/richardcase/muxy/actions/workflows/ci.yml/badge.svg)](https://github.com/richardcase/muxy/actions/workflows/ci.yml)
[![Release](https://github.com/richardcase/muxy/actions/workflows/release.yml/badge.svg)](https://github.com/richardcase/muxy/actions/workflows/release.yml)
![macOS 14+](https://img.shields.io/badge/macOS-14%2B-black?logo=apple)
![Rust 2021](https://img.shields.io/badge/rust-2021-orange?logo=rust)
![Swift 6](https://img.shields.io/badge/swift-6-orange?logo=swift)
![version 0.1.0](https://img.shields.io/badge/version-0.1.0-informational)

**A cross-platform agent-orchestrator terminal.** A headless Rust daemon runs a fleet of CLI coding
agents — each isolated in its own git worktree or jj workspace — with attention routing, while a
native SwiftUI macOS app renders every agent's live terminal via [libghostty](https://ghostty.org).

> This repository is **private**. The status badges above render for collaborators. muxy is
> proprietary — see [License](#license).

## Features

- **Orchestrate a fleet of CLI coding agents** — Claude Code, OpenAI Codex, or a plain shell — from
  one native app.
- **Per-agent isolation** — each agent runs in its own **git worktree or jj workspace** (auto-detected
  per project). Finish work with **Land** (commit + keep the branch/bookmark, hand off to you) or throw
  it away with **Discard** — both from the UI, with confirmation.
- **Attention routing** — know which agent needs you: native tool hooks (Claude/Codex turn-complete)
  plus a VT-signal fallback (BEL, OSC 9, OSC 777) drive sidebar badges and a **menu-bar attention
  count**.
- **Native macOS client** — SwiftUI + an embedded **libghostty** surface per agent: real terminals with
  keys/mouse/IME/resize, **split panes** (companion shells rooted in the agent's worktree), a **command
  palette** (⌘K) with a rebindable keymap, and switch-agent hotkeys.
- **Survivable** — the daemon owns the agent PTYs and keeps them running while the window is closed; the
  app **launches + supervises its own daemon** and **auto-reconnects** (bounded backoff) if the
  connection drops.
- **Robust daemon** — a `~/.config/muxy/config.toml` config file, per-user sockets, a single-instance
  guard, graceful shutdown that kills child PTYs, companion-crash reaping, and structured `tracing`
  logs.

## Architecture

muxy uses a tmux-style **client / server** split:

- The **daemon** (`muxy-daemon`) owns the agent PTYs, the per-agent worktrees, attention state, the
  split-pane tree, and process survival.
- The **macOS app** embeds a libghostty surface whose launched command is `muxy attach <pane>` (the
  render pump). Ghostty renders the agent natively client-side while the daemon remains the mux.
- A JSON control socket drives the app's sidebar, agent spawning, and split-pane operations.

This keeps best-in-class Ghostty rendering + a native Mac app while the daemon provides orchestration
and survival.

## Requirements

- **macOS 14+** to run the app.
- To **build from source**:
  - **Full Xcode 16** — the Metal shader compiler (`xcrun metal`) ships with Xcode, not the Command
    Line Tools, and libghostty's renderer needs it.
  - **zig 0.16.0** — to build the vendored libghostty.
  - **Rust** (stable, edition 2021) and **Swift 6**.
- The ~189 MB vendored `libghostty` static lib is gitignored and produced reproducibly by a script
  (see [`docs/building-libghostty.md`](docs/building-libghostty.md)).

## Installation

### From a release (collaborators)

Download `Muxy-vX.Y.Z-macos.zip` from the repo's [Releases](https://github.com/richardcase/muxy/releases)
and unzip it. The build is **unsigned / un-notarized**, so macOS Gatekeeper will quarantine it — clear
the quarantine, then move it to `/Applications`:

```sh
xattr -dr com.apple.quarantine Muxy.app   # or: right-click Muxy.app → Open
mv Muxy.app /Applications/
```

### From source

```sh
git clone git@github.com:richardcase/muxy.git && cd muxy
scripts/build-libghostty.sh    # zig 0.16 + full Xcode; builds the vendored libghostty (once)
scripts/build-app.sh           # → dist/Muxy.app
open dist/Muxy.app
```

## Usage

Double-click `Muxy.app` — it launches and **supervises its own daemon** (no manual steps). Then:

- **⌘N** — spawn an agent: pick a project (a git/jj repo), a task, and an adapter (`claude`, `codex`,
  or `shell`).
- Drive the agent's terminal directly; **⌘D / ⌘⇧D** split a companion shell; **⌘L** Lands the agent,
  Discard is in the menu.
- The menu-bar item shows how many agents need attention.

The bundled **`muxy` CLI** also works headlessly against a running daemon:

```sh
muxy spawn <project> <task> [adapter]   # adapter defaults to "claude"; prints the new pane id
muxy attach <pane-id>                    # attach to a pane in your terminal
```

## Development

See [`AGENTS.md`](AGENTS.md) for the full contributor/agent reference (build commands, gotchas,
conventions).

```sh
# Rust workspace (rustup is not auto-sourced here — prefix cargo with the env):
source "$HOME/.cargo/env" && cargo test --workspace     # CI runs this with --locked

# Swift core (fast — MuxyCore doesn't need libghostty):
cd macos && swift test

# Run in dev (unbundled builds don't auto-spawn the daemon — start it by hand):
source "$HOME/.cargo/env" && cargo run -p muxy-daemon
cd macos && MUXY_BIN="$PWD/../target/debug/muxy" swift run muxy-app
```

Repo layout:

| Path | What |
|---|---|
| `crates/muxy-proto` | Wire protocol + control-plane types (postcard/JSON) |
| `crates/muxy-config` | Config resolution (env › file › default) |
| `crates/muxy-daemon` | The headless daemon (binary `muxy-daemon`) |
| `crates/muxy-client` | Client lib + the `muxy` CLI (binary `muxy`) |
| `crates/muxy-hook` | Agent lifecycle hook shim (binary `muxy-hook`) |
| `crates/muxy-vt` | Terminal attention-signal scanner (BEL/OSC) |
| `crates/muxy-workspace` | Per-agent git/jj worktree provisioning |
| `macos/` | SwiftPM app — `MuxyCore` (lib) + `muxy-app` (exe), links libghostty |
| `scripts/` | `build-app.sh`, `build-libghostty.sh`, `set-version.sh`, `gen-icon.swift` |
| `docs/` | Design specs/plans (`superpowers/`), `versioning.md`, `building-libghostty.md` |

## Versioning & releases

The top-level [`VERSION`](VERSION) file is the single source of truth; `scripts/set-version.sh <X.Y.Z>`
propagates it into the Cargo workspace and the app's Info.plist. Pushing a `vX.Y.Z` tag runs
[`release.yml`](.github/workflows/release.yml), which builds the app and publishes a GitHub Release with
the (unsigned) `Muxy.app` attached. See [`docs/versioning.md`](docs/versioning.md).

## Status

Built and green in CI: the daemon/client spine, the native SwiftUI + libghostty client, split panes,
the Land/Discard lifecycle (git + jj), Claude/Codex/shell adapters, robustness (config, single-instance,
reconnect), packaging (a self-contained `.app`, reproducible libghostty build, versioned releases).

Not yet done: **code-signing → notarization → DMG** and a **Homebrew cask** (both need an Apple
Developer ID), an authoritative daemon-side VT grid (scrollback reflow-on-resize), and agent survival
across a daemon restart.

## License

Proprietary. © Richard Case. **All rights reserved.** No open-source license is granted.
