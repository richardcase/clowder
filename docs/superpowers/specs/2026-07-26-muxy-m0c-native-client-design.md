# muxy M0c — Native macOS Client (SwiftUI + libghostty)

## Context

M0a/M0b built a headless daemon — muxy's **mux**: it owns agent PTYs in isolated git
worktrees, survives the client closing, and routes attention (tool hooks → OS notifications).
M0c is the first **GUI**: a native macOS app.

A research pass on the libghostty embedding API (`docs/superpowers/research/2026-07-26-libghostty-embedding.md`,
cited to a pinned Ghostty commit) surfaced a make-or-break fact: **a `ghostty_surface_t` always
creates and owns its own PTY and spawns a command — there is no way to feed externally-supplied
bytes into its parser, and no "no-pty" mode.** So the M0a assumption "the client embeds
libghostty to render daemon-streamed bytes" does **not** hold.

**Resolution (the tmux client/server pattern):** don't push bytes *into* libghostty — let it
*pull* them the way it's designed to. libghostty owns its PTY and runs a **command**; make that
command muxy's existing attach client, `muxy attach <pane-id>` (the M0a `pump`). The pump streams
the daemon's bytes to its stdout → libghostty's PTY → libghostty renders natively; keystrokes and
resize flow back through the pump to the daemon. This is exactly how a `tmux` client works, over
a **server** — the muxy daemon — that owns the real sessions and survives.

Outcome: **native libghostty rendering** (ghostty DNA) **+ a native SwiftUI mac app + survival**,
all at once, reusing the daemon and pump unchanged. The research's #1 risk (byte-feed) is
dissolved; the build/ABI/packaging risks remain.

## Architecture (reshaped)

```
┌ muxy daemon (the mux — M0a/M0b, unchanged) ─────────────────────────────┐
│ owns agent PTYs in worktrees · attach/detach · attention · workspaces    │
└───────▲──────────────────────────────────────────────▲──────────────────┘
        │ pump: byte stream (render) + input + resize    │ control channel:
        │ via `muxy attach <pane>` running as a PTY child │ agent list + attention feed
┌───────┴─────────────────────────────────┐   ┌──────────┴───────────────────────────┐
│ libghostty surface (per focused agent)   │   │ SwiftUI chrome                        │
│ · owns a PTY, command = `muxy attach N`  │   │ · sidebar: projects/agents + badges   │
│ · renders the agent's terminal natively  │   │ · fed by the control channel          │
└──────────────────────────────────────────┘   └───────────────────────────────────────┘
        └──────────────── one native SwiftUI macOS app ────────────────┘
```

- **Daemon = the mux**, untouched. Closing the app kills only pump clients; agents keep running;
  reopening re-runs `muxy attach` and re-renders. Survival preserved.
- **Render path** goes through libghostty's PTY (the pump child) — **no** Swift↔daemon glue on the
  hot path, no byte injection.
- **Control path** (sidebar, attention badges) is the only thing needing Swift↔daemon glue: the
  app reads the daemon's **control channel** (agent list + a global attention feed). A small
  `muxy-control-ffi` Rust C-ABI shim (reusing muxy-proto) is the likely seam, so Swift doesn't
  reimplement the wire protocol.
- **Supersedes** the M0a "two-parser: libghostty renders bytes client-side" model. The daemon's
  byte-log/backlog stays daemon-side; the daemon-side authoritative VT grid (`muxy-vt`, M1) is for
  snapshots + attention scanning, not for feeding the client renderer.

## Decomposition (M0c is 3 sub-pieces; build order = spike first)

- **M0c-2 — libghostty spike (FIRST, exploratory, not TDD).** A minimal SwiftUI/AppKit window
  embedding one libghostty surface whose command is `muxy attach <pane>`, rendering a live agent
  from a running daemon, taking keyboard input, with resize propagating. Retires the real remaining
  risks: building libghostty with Zig, linking the static lib **without full Xcode**, hosting the
  surface in an `NSView`, configuring the surface command, and running the pump cleanly as a PTY
  child. **This one spike proves native-render-with-survival end to end.**
- **M0c-1 — Rust prerequisites (plannable / TDD-able).** (a) The **global attention feed** on the
  daemon control channel (list agents grouped by project + subscribe to *all* agents' attention —
  the M0b-review item the sidebar needs; today attention is only forwarded to a client attached to
  that exact pane). (b) **Pump adaptations** to run as a libghostty PTY child: forward `SIGWINCH`
  → `Resize`, correct raw-mode/PTY handling, clean attach/detach. (c) Optional **`muxy-control-ffi`**
  C-ABI shim for Swift to consume the feed.
- **M0c-3 — SwiftUI shell.** Sidebar (agents grouped by project + status/attention badges) + a
  focused libghostty terminal pane; click / hotkey (1–9, next-attention) switching; consuming the
  control feed and the attach-client rendering.

**Deferred (later):** command palette + rebindable keymap (M0c+/M1), splits/companion panes (M1),
tray/menu-bar item (daemon already fires OS notifications from M0b), Linux gtk client (M5),
dashboard-grid overview (post-v1).

## The spike (M0c-2) — approach

1. Vendor a **pinned** libghostty (exact Ghostty commit; matches Zig 0.16.0 per `minimum_zig_version`).
2. Build the static lib with `zig build` → `libghostty-internal.a` + `ghostty.h` (raw lib path, since
   the xcframework packaging needs `xcodebuild`, which this machine lacks — CLT only).
3. Minimal **SwiftPM** macOS executable (programmatic `NSApplication`, no `.xcodeproj`): a window +
   an `NSView`; a `GhosttyKit` **module map** exposing `ghostty.h`; hand-link Metal/QuartzCore/
   Foundation/AppKit.
4. Init the libghostty app/config, create a surface bound to the view, set its **command** to
   `muxy attach <pane>` (env `MUXY_SOCK` pointing at a running daemon), wire the required callback
   vtable minimally (title/clipboard/bell no-ops).
5. Run against a live daemon with one spawned agent; confirm the agent's terminal renders, typing
   works, resize propagates, and **closing the window leaves the agent alive** (reopen re-renders).

Visual confirmation is inherently interactive — expect to run/observe the window (or screenshot);
some steps may need the user's hands if the libghostty build or windowing hits environment friction.

## Risks (re-scoped from the research)

1. **(DISSOLVED)** byte-feed into libghostty — resolved by the attach-pump pattern.
2. **Alpha, hand-synced C ABI** (structs mirrored from Zig by hand; large action-callback enum) —
   pin an exact commit, isolate all FFI in one module, audit header-vs-lib.
3. **No full Xcode** — link the raw `libghostty-internal.a` via a module map + hand-link frameworks;
   SwiftPM executable, not an Xcode app.
4. **Pump-as-PTY-child correctness** — size propagation (SIGWINCH→Resize), raw-mode, clean detach.

## Verification

- **Spike:** the libghostty window shows a live agent's terminal from the daemon; typing reaches the
  agent; resizing the window resizes the agent's terminal; closing the window leaves the agent
  running in the daemon (survival), and reopening re-attaches and re-renders.
- **M0c-1:** Rust tests for the global attention feed (list + subscribe-all) and pump SIGWINCH→Resize.
- **M0c-3:** the sidebar lists agents by project with live attention badges; selecting one shows its
  libghostty terminal; the next-attention hotkey jumps to a blocked agent.
