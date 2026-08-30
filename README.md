# clowder

[![CI](https://github.com/richardcase/clowder/actions/workflows/ci.yml/badge.svg)](https://github.com/richardcase/clowder/actions/workflows/ci.yml)
[![Release](https://github.com/richardcase/clowder/actions/workflows/release.yml/badge.svg)](https://github.com/richardcase/clowder/actions/workflows/release.yml)
![macOS 14+](https://img.shields.io/badge/macOS-14%2B-black?logo=apple)
![Rust 2021](https://img.shields.io/badge/rust-2021-orange?logo=rust)
![Swift 6](https://img.shields.io/badge/swift-6-orange?logo=swift)
![version 0.1.0](https://img.shields.io/badge/version-0.1.0-informational)

**A cross-platform agent-orchestrator terminal.** A headless Rust daemon runs a fleet of CLI coding
agents — each isolated in its own git worktree or jj workspace — with attention routing, while a
native SwiftUI macOS app renders every agent's live terminal via [libghostty](https://ghostty.org).

> clowder is open source under the **Apache License 2.0** — see [License](#license).

## Features

- **Orchestrate a fleet of CLI coding agents** — Claude Code, OpenAI Codex, or a plain shell — from
  one native app.
- **Per-agent isolation** — each agent runs in its own **git worktree or jj workspace** (auto-detected
  per project), created **outside your repo** under `~/.local/share/clowder/worktrees` so the project
  stays clean. Finish work with **Land** (commit + keep the branch/bookmark, hand off to you) or throw
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
- **Switch between Local and remote hosts** — pick which backend is active from the sidebar's
  connection chip, the menu bar, or the command palette (⌘K); switching away from Local detaches its
  daemon rather than killing it, so agents on both sides keep running while you're elsewhere.
- **Manage remote hosts from the app** — add, edit, remove and pair remote daemons in Settings
  (**⌘,** ▸ Hosts), with the same probe-then-trust pairing flow as the CLI, instead of dropping to
  `clowder remote`.
- **Robust daemon** — a `~/.config/clowder/config.toml` config file (sockets, pane defaults, and
  `[worktrees] base` to put worktrees wherever you like), per-user sockets, a single-instance guard,
  graceful shutdown that kills child PTYs, companion-crash reaping, and structured `tracing` logs.

## Architecture

clowder uses a tmux-style **client / server** split:

- The **daemon** (`clowder-daemon`) owns the agent PTYs, the per-agent worktrees, attention state, the
  split-pane tree, and process survival.
- The **macOS app** embeds a libghostty surface whose launched command is `clowder attach <pane>` (the
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

### Homebrew (recommended)

```sh
brew install --cask richardcase/tap/clowder
```

Installs the signed + notarized `Clowder.app` and puts the `clowder` CLI on `PATH`. See
[`docs/homebrew.md`](docs/homebrew.md).

### From a release

Download `Clowder-vX.Y.Z-macos.dmg` from the repo's [Releases](https://github.com/richardcase/clowder/releases),
open it, and drag **Clowder** to **Applications**. Releases are **signed with a Developer ID and
notarized by Apple**, so Gatekeeper accepts them with no workaround.

> Older releases (before signing was set up) shipped as an unsigned `.zip`; if you have one of those,
> clear the quarantine first: `xattr -dr com.apple.quarantine Clowder.app`.

### From source

```sh
git clone git@github.com:richardcase/clowder.git && cd clowder
scripts/build-libghostty.sh    # zig 0.16 + full Xcode; builds the vendored libghostty (once)
scripts/build-app.sh           # → dist/Clowder.app
open dist/Clowder.app
```

## Usage

Double-click `Clowder.app` — it launches and **supervises its own daemon** (no manual steps). Then:

- **⌘N** — spawn an agent: pick a project (a git/jj repo), a task, and an adapter (`claude`, `codex`,
  or `shell`).
- Drive the agent's terminal directly; **⌘D / ⌘⇧D** split a companion shell; **⌘L** Lands the agent,
  Discard is in the menu.
- The menu-bar item shows how many agents need attention.
- **⌘,** opens Settings ▸ Hosts to add, edit, remove and pair remote daemons without touching the
  CLI — the connection chip's "Manage Hosts…" item opens the same window.

The bundled **`clowder` CLI** also works headlessly against a running daemon:

```sh
clowder spawn <project> <task> [adapter]   # adapter defaults to "claude"; prints the new pane id
clowder attach <pane-id>                    # attach to a pane in your terminal
clowder remote add|list|show|set|rm         # manage a nicknamed registry of remote daemons (or Settings ▸ Hosts)
clowder remote probe|trust|untrust          # pair with a remote daemon over TLS (see docs/remote-tls.md)
clowder connect <name-or-host:port>         # forward to a remote daemon, then clowder attach as usual
```

## Development

See [`AGENTS.md`](AGENTS.md) for the full contributor/agent reference (build commands, gotchas,
conventions).

```sh
# Rust workspace (rustup is not auto-sourced here — prefix cargo with the env):
source "$HOME/.cargo/env" && cargo test --workspace     # CI runs this with --locked

# Swift core (fast — ClowderCore doesn't need libghostty):
cd macos && swift test

# Run in dev (unbundled builds don't auto-spawn the daemon — start it by hand):
source "$HOME/.cargo/env" && cargo run -p clowder-daemon
cd macos && CLOWDER_BIN="$PWD/../target/debug/clowder" swift run clowder-app
```

Repo layout:

| Path | What |
|---|---|
| `crates/clowder-proto` | Wire protocol + control-plane types (postcard/JSON) |
| `crates/clowder-config` | Config resolution (env › file › default) |
| `crates/clowder-daemon` | The headless daemon (binary `clowder-daemon`) |
| `crates/clowder-client` | Client lib + the `clowder` CLI (binary `clowder`) |
| `crates/clowder-hook` | Agent lifecycle hook shim (binary `clowder-hook`) |
| `crates/clowder-vt` | Terminal attention-signal scanner (BEL/OSC) |
| `crates/clowder-workspace` | Per-agent git/jj worktree provisioning |
| `macos/` | SwiftPM app — `ClowderCore` (lib) + `clowder-app` (exe), links libghostty |
| `scripts/` | `build-app.sh`, `build-libghostty.sh`, `set-version.sh`, `gen-icon.swift` |
| `docs/` | Design specs/plans (`superpowers/`), `versioning.md`, `building-libghostty.md`, `code-signing.md`, `homebrew.md` |

The marketing site for [getclowder.app](https://getclowder.app) lives in [`site/`](site/) — see
[`site/README.md`](site/README.md).

## Versioning & releases

The top-level [`VERSION`](VERSION) file is the single source of truth; `scripts/set-version.sh <X.Y.Z>`
propagates it into the Cargo workspace and the app's Info.plist. Pushing a `vX.Y.Z` tag runs
[`release.yml`](.github/workflows/release.yml), which builds the app and publishes a GitHub Release. When
signing is configured it attaches a **signed + notarized `.dmg`** (`scripts/sign-app.sh` →
`scripts/package-dmg.sh`); otherwise it falls back to an unsigned `.zip`. Signing material is fetched from
**Doppler** at build time; GitHub stores only a single read-only Doppler **service token**. See
[`docs/versioning.md`](docs/versioning.md) and [`docs/code-signing.md`](docs/code-signing.md).

## Status

Built and green in CI: the daemon/client spine, the native SwiftUI + libghostty client, split panes,
the Land/Discard lifecycle (git + jj), Claude/Codex/shell adapters, robustness (config, single-instance,
reconnect), packaging (a self-contained `.app`, reproducible libghostty build, versioned releases),
**code-signing → notarization → signed DMG** (Developer ID), and a **Homebrew cask** with auto-bump.

Not yet done: an authoritative daemon-side VT grid (scrollback reflow-on-resize), and agent survival
across a daemon restart.

## License

Apache License 2.0. © 2026 Richard Case. See [`LICENSE`](LICENSE).
