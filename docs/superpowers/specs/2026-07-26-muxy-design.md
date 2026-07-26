# muxy — Design

## Context

Running several CLI coding agents (Claude Code, codex, aider, goose) in parallel is
increasingly how real work gets done, but today it's a mess: agents stomp on each
other's working copies, you lose track of which one is blocked waiting on you, and
switching between them means hunting through tmux windows or terminal tabs. muxy is a
purpose-built desktop app that orchestrates a *fleet* of coding agents — each isolated
in its own auto-provisioned jj workspace or git worktree — and tells you, wherever you
are, the moment one needs your attention.

It is **agent-orchestrator first**, not a general terminal: an opinionated IDE-like shell
(sidebar of agents grouped by project + one focused terminal pane) that happens to embed
libghostty for rendering, rather than a terminal that happens to manage agents.

Intended outcome: start N agents across N projects with one keystroke each, glance at a
sidebar to see who's working / blocked / done, jump to whoever needs you, and land or
discard their work — without ever hand-managing jj/git worktrees or losing an agent that
finished while you were away.

## Confirmed product decisions

- **Core identity:** agent orchestrator first; opinionated non-terminal UI.
- **What's an agent:** purpose-built for CLI coding agents (claude, codex, aider, goose) — muxy can be smart about them, not a generic command runner.
- **Attention detection:** PRIMARY = tool-native hooks (Claude Code `Notification`/`Stop`); FALLBACK = terminal signals in the PTY stream (BEL, OSC 9 / OSC 777, title changes). Noisy heuristics (output-idle, prompt-regex) deliberately excluded.
- **Workspace lifecycle:** muxy CREATES and TEARS DOWN an isolated workspace per agent (provision on spawn, offer cleanup/land/discard on completion). User rarely touches jj/git directly.
- **Process model:** headless **daemon** owns PTYs, agent processes, and workspace state; the GUI is a client that attaches/detaches (tmux-like). Agents survive the window closing. This is also the seam for future remote/phone push.
- **Notifications:** OS desktop notifications + a menu-bar/tray icon with an attention count. Remote/phone push is post-v1.
- **Layout:** sidebar list of agents grouped by project (status + attention badges) + a focused area. The focused area shows the selected agent's pane and can be **split horizontally or vertically** to open companion terminal panes (plain shells rooted in the *same* worktree cwd) alongside the agent — for running tests, git, etc. without disturbing the agent. Scales to tens of agents.
- **Keyboard:** command palette (Cmd/Ctrl-K) + a few direct hotkeys (next-attention, switch-agent 1–9); every action is a named command in a rebindable keymap.
- **Language:** Rust for the daemon and all shared logic. Zig appears only as a vendored build step for libghostty.
- **Client strategy:** NATIVE per platform — SwiftUI/AppKit on macOS (built first), `gtk4-rs` on Linux (second). Both are thin views speaking `muxy-proto` to the shared Rust daemon. The core/daemon stays cross-platform Rust regardless.

## Architecture

```
┌──────────────────────── muxy daemon (Rust, background) ─────────────────────────┐
│  supervisor         workspace driver        attention engine        muxy-vt      │
│  · PTY + process     · jj-lib / git worktree  · hook receiver          · headless  │
│    lifecycle           behind one trait          (semantic, primary)     VT grid   │
│  · owns every fd     · provision on spawn      · VT-signal scanner       (authorit-│
│  · output fan-out    · teardown/land/discard     (BEL/OSC, fallback)     ative)    │
└───────────────▲──────────────────────────────────────────────────┬───────────────┘
     muxy-proto  │  attach/detach · GridSnapshot · byte frames · events · commands
   (Unix socket, │                                                    │
    Transport    │            ┌───────────────────────────────────────▼────────────┐
    trait for    │            │  hook socket ◄── muxy-hook (tiny injected relay)     │
    future QUIC) │            └──────────────────────────────────────────────────────┘
┌───────────────┴──────────────────────────┐   ┌──────────────────────────────────────┐
│  macOS client (SwiftUI/AppKit)            │   │  Linux client (gtk4-rs)   [M-later]  │
│  embeds libghostty via NSView/Metal       │   │  embeds libghostty via GLArea        │
│  [ sidebar › projects/agents + badges ]   │   │  [ same layout ]                     │
│  [ focused workspace: agent pane │ split  │   │                                      │
│    companion shell(s), H/V ] Cmd-K·tray·notifs│ │                                    │
└────────────────────────────────────────────┘   └──────────────────────────────────────┘
```

### Pane vs. agent model

The supervisor's unit is a **pane** — a PTY + process rooted in a workspace cwd, whose
output feeds a `muxy-vt` grid. Two kinds:

- **Agent pane:** the primary pane of a workspace; runs a CLI coding agent via an `AgentAdapter`, gets hook injection + attention tracking, and owns the sidebar row.
- **Companion pane:** a plain shell (or arbitrary command) the user opens in the *same* workspace/worktree via a horizontal/vertical split, for tests/git/poking around. **No hook injection, not a sidebar row, not attention-tracked** — it's scratch space bound to the agent's isolation, so nothing it runs collides with a different agent's working copy.

A workspace therefore has exactly one agent pane and zero-or-more companion panes; the
client renders that workspace's panes as a split layout in the focused area. Pane identity,
workspace association, and the split tree live in daemon state (so splits survive
detach/reattach); the client renders the tree and routes focus/input per pane.

### Two-parser terminal model (resolves the highest-risk decision)

libghostty exposes **no supported C ABI to read out its cell grid**, so running it
headless in the daemon (mosh model) would require forking and maintaining libghostty —
rejected. Instead:

- **Client-side:** libghostty, one surface per focused terminal, renders bytes → pixels, embedded via each platform's native path (the way Ghostty's own apps do it).
- **Daemon-side:** a *separate* lightweight Rust VT crate (`alacritty_terminal` or `termwiz`) maintains the authoritative screen grid — correctness only, no rendering.

The daemon grid pays for itself three ways: cheap **snapshot-on-attach** (ship a
screenful + a bounded byte tail, not megabytes of replay), it's where the **BEL/OSC
attention scanner** lives, and it keeps "what's on screen" correct **while detached** (and
is what a future phone client reads). It is also the escape hatch: if libghostty embedding
ever gets ugly, render cells directly from that grid (the Zed model) — so **libghostty is
never load-bearing**.

### Hook injection (muxy's differentiator)

Because muxy owns the provisioned workspace, at spawn the supervisor sets env on the agent
process (`MUXY_AGENT_ID=<uuid>`, `MUXY_SOCK=<hook socket>`) and writes a scoped hook config
into the workspace. For Claude Code: a git-ignored `.claude/settings.local.json` whose
`Notification` and `Stop` hooks run `muxy-hook --event <kind>`. `muxy-hook` (~100 lines)
reads Claude's hook JSON from stdin, reads `MUXY_AGENT_ID`/`MUXY_SOCK` from env, posts one
message to the daemon hook socket, and exits. **Agent identity comes from the injected env
var, never from cwd/session-id** (a subagent or `cd` would break those). Each tool gets an
`AgentAdapter` impl; tools without hooks return a no-op and rely on the VT-signal fallback.

## Module layout (Cargo workspace + native clients)

| Crate / module | Purpose |
|---|---|
| `muxy-proto` | Wire protocol types + `Transport` trait (Unix socket now, QUIC later — the remote seam) |
| `muxy-daemon` | Daemon core: global agent/pane/workspace state; serves clients; hosts supervisor + attention |
| `muxy-daemon::supervisor` | Pane (PTY + process) lifecycle; single owner of every PTY fd; output fan-out; tracks pane→workspace association + the per-workspace split tree (`portable-pty` + tokio) |
| `muxy-daemon::attention` | Fuses hook events (primary) + VT signals (fallback) → debounced per-agent attention state |
| `muxy-workspace` | `WorkspaceDriver` trait; `GitWorktreeDriver` (gix/git2) + `JjDriver` (jj-lib), capability flags |
| `muxy-vt` | Headless authoritative VT grid + signal scanner (`alacritty_terminal`/`termwiz`) |
| `muxy-ghostty-sys` | Pinned FFI bindings to libghostty; isolates ABI drift (used by the Linux client; macOS embeds via Swift) |
| `muxy-hook` | Tiny relay binary injected into agents' hook config |
| `muxy-keymap` | Command registry + rebindable keymap (drives palette + hotkeys, incl. split-right/split-down/close-pane/focus-pane); shared by both clients |
| macOS client | SwiftUI/AppKit app: sidebar, focused terminal (libghostty via NSView/Metal), palette, tray, Notification Center |
| Linux client `[later]` | `gtk4-rs` app: same layout, libghostty via GLArea, libnotify + tray |

Everything hard (supervisor, workspace, attention, VT, adapters, keymap) is written **once**
in the shared Rust daemon/crates; only the thin view layer forks per platform.

## Top risks & mitigations

1. **libghostty embedding + input/HiDPI/resize per platform.** Mitigated by going native — inherit Ghostty's own proven macOS (Metal/NSView) and GTK (GLArea) embedding paths instead of pioneering foreign-surface compositing. Fallback: self-render cells from the daemon grid.
2. **libghostty ABI instability + Zig-in-CI.** Vendor a *pinned* libghostty commit; isolate all FFI in `muxy-ghostty-sys`; CI builds the pinned lib on macOS + Linux.
3. **jj-lib instability + git/jj abstraction leak.** Ship the **git-worktree driver first**; both behind `WorkspaceDriver` with capability flags so the UI degrades where jj-only features (op-log undo) are absent; jj added in M3. Pin jj-lib.

## Build decomposition

Each milestone is its own spec → plan → implementation cycle. **The first plan we write
together is M0.**

- **M0 — Walking skeleton (retire the risks).** macOS SwiftUI client + Rust daemon; spawn **one** `claude` in **one** git worktree; libghostty renders the live terminal in the focused pane and takes keystrokes; detach/reattach works (agent survives window close); one Claude `Notification`/`Stop` hook flips a sidebar dot + fires one OS notification. Proves: native libghostty embed, daemon/client split + PTY ownership + detached survival, workspace provisioning, hook-based attention.
- **M1 — Multi-agent shell + companion panes.** N agents, sidebar grouped by project, status dots, focus switching, per-pane byte logs + snapshot-on-attach, tray count. Command registry + Cmd-K palette + hotkeys (switch-agent 1–9, next-attention) via `muxy-keymap`. **Companion terminal panes:** split the focused area horizontally/vertically to open plain shells in the agent's workspace cwd; split tree persists across detach/reattach; split/close/focus-pane commands.
- **M2 — Attention fusion.** Wire the `muxy-vt` fallback scanner (BEL/OSC 9/OSC 777/title) and fuse with hooks; debounce; per-project + global attention rollups.
- **M3 — jj driver + lifecycle UX.** Add `JjDriver`; auto-detect jj vs git; surface provision/status/integrate/teardown as commands (land/squash/rebase on completion, cleanup prompts).
- **M4 — Adapter breadth.** `AgentAdapter` impls for codex, aider, goose (hooks where available, VT fallback otherwise) — validates the seam.
- **M5 — Linux client.** `gtk4-rs` client over the same `muxy-proto`; libnotify + tray; libghostty via GLArea + `muxy-ghostty-sys`.
- **M6 — Robustness + remote seam.** Scrollback caps + reflow-on-resize, crash/reap handling, daemon persistence/restart, config files; swap `UnixSocketTransport` → authenticated `QuicTransport` (the future phone/push attach point).

**Deliberately post-v1 (YAGNI):** remote/phone push (until M6 seam), dashboard-grid overview mode, plugin API. (Note: splits are *in* v1 but scoped to companion panes within one workspace — free-form tiling of unrelated agents together stays out.)

## Verification

- **M0 end-to-end:** launch daemon; from the macOS client, spawn `claude` on a scratch repo → confirm a git worktree on a fresh branch is created; type into the terminal and see libghostty render responses; close the window and reopen → the agent is still there with its screen restored from `GridSnapshot`; trigger a Claude prompt/stop → confirm the sidebar dot changes and a macOS notification fires and click-focuses the agent.
- **Per-crate:** `muxy-workspace` integration tests exercise provision → dirty → integrate → teardown on the git driver (and jj driver at M3); `muxy-vt` unit tests assert BEL/OSC 9/OSC 777/title events from byte fixtures; `muxy-proto` round-trips messages; `muxy-hook` is tested by feeding it Claude's hook JSON on stdin + env and asserting the daemon receives the correlated event.
- **M1 companion panes:** with an agent focused, split right/down → a shell opens whose `pwd` equals the agent's worktree cwd; run `git status` in it and confirm it reflects the agent's changes (same working copy, no collision with other agents); detach/reattach → the split layout and both panes' screens are restored.
- **Risk gates:** M0 does not close until native libghostty embedding + detached survival are demonstrated on macOS; the Zig/libghostty build runs in CI on both OSes before M5.
