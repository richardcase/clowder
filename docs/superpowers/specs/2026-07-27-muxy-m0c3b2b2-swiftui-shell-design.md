# muxy M0c-3b2-b-2 — SwiftUI Shell (+ resize, mouse/IME)

## Context

M0c-3b2-a proved one libghostty surface renders a daemon-owned agent, survives window
close, and takes key input. M0c-3b2-b-1 (merged, PR #8) added the testable socket/threading
core to `MuxyCore`: `LineBuffer`, `AgentStore.lastError`, and `UnixSocketConnection` (a real
`ControlTransport` delivering every line on the main thread). This milestone — **b2-b-2** —
builds the actual macOS app around that core and closes the input gaps, finishing the M0c
"native shell" arc.

Three separable pieces, deliberately scoped together (user chose A+B+C):

- **A — the SwiftUI shell:** an `@main` app with a sidebar of agents grouped by project, a
  terminal pane per selected agent, a spawn sheet, and a correct connection lifecycle.
- **B — resize:** make `muxy attach` propagate its terminal size to the daemon so a resized
  window actually reflows the agent's PTY.
- **C — mouse + IME:** extend the existing `SurfaceView` to forward mouse events and
  composed (IME) text, not just keystrokes.

### What already exists (ground truth — do not rebuild)

`MuxyCore` (used as-is, no changes required by this milestone):
- `AgentStore: ObservableObject` — `@Published agents`, `needsRefresh`, `lastError`;
  `apply(_:)`; `byProject: [(project: String, agents: [AgentInfo])]` (projects sorted,
  agents sorted by pane).
- `AgentInfo { pane: UInt64, project: String, task: String, state: AttentionState }`.
- `AttentionState { idle, working, needsInput, completed, exited }`.
- `ControlRequest { listAgents, spawnAgent(project:task:adapter:) }`.
- `ControlSession(transport:store:)` — retains the transport, wires the receiver, drives
  refresh, exposes `store` and `send(_ request:)`. Holds the receiver via `[weak self]`, so
  **something must retain the session** or inbound lines stop.
- `UnixSocketConnection(path:)` — POSIX `ControlTransport`; background read loop delivers
  each line on `DispatchQueue.main`; `disconnect()` (idempotent shutdown+close); `send`
  guards a closed fd; `deinit { disconnect() }`.

`MuxyApp` (current, to be restructured):
- `main.swift` — top-level statements: `ghostty_init` → `ghostty_config_new`/`finalize` →
  a 6-callback `ghostty_runtime_config_s` (only `wakeup_cb` ticks `ghostty_app_tick`) →
  `ghostty_app_new` → one `NSWindow` hosting one `SurfaceView` → `NSApplication.run()`.
  Takes `<pane-id>` as `argv[1]`; reads `MUXY_SOCK`/`MUXY_BIN` from env.
- `SurfaceView: NSView` — `wantsLayer`; creates one `ghostty_surface_t` bound to its NSView
  running `"<muxyBinary> attach <pane>"` with `MUXY_SOCK` in the surface env; pushes
  size/scale; forwards `keyDown`/`keyUp` via `ghostty_surface_key` (text only for
  codepoints ≥ 0x20). Renders itself (libghostty owns the Metal layer).

Rust daemon/client (ground truth for piece B):
- `muxy-proto`: `ClientToDaemon::Resize { pane, cols, rows }` exists on the pump socket.
- `muxy-daemon` `server.rs`: the pump handler already matches
  `Some(ClientToDaemon::Resize { cols, rows, .. }) => pane.resize(cols, rows)`, and
  `pane.rs::resize` calls `portable-pty`'s `resize(PtySize{ rows, cols, .. })`. **The
  daemon side of resize is complete.**
- `muxy-client` `pump()`: a `tokio::select!` over stdin→`Input` and socket→`Output`. It
  **never reads a winsize and never sends `Resize`** — this is the entire gap for piece B.
  `attach()` connects `MUXY_SOCK`, enables raw mode, and calls `pump`.

libghostty ABI (verified present in `Sources/GhosttyKit/include/ghostty.h`):
- `ghostty_surface_mouse_button(surface, ghostty_input_mouse_state_e,
  ghostty_input_mouse_button_e, ghostty_input_mods_e) -> bool`
- `ghostty_surface_mouse_pos(surface, double x, double y, ghostty_input_mods_e)`
- `ghostty_surface_mouse_scroll(surface, double x, double y, ghostty_input_scroll_mods_t)`
- `ghostty_surface_text(surface, const char*, uintptr_t)` — committed text
- `ghostty_surface_preedit(surface, const char*, uintptr_t)` — marked/preedit text
- `ghostty_surface_ime_point(surface, double*, double*, double*, double*)`
- `ghostty_input_key_s.composing: bool`
- Mouse-state constants `GHOSTTY_MOUSE_PRESS`/`GHOSTTY_MOUSE_RELEASE`; button constants
  `GHOSTTY_MOUSE_LEFT`/`RIGHT`/`MIDDLE`/…

## Goals / Non-goals

**Goals:** a usable single-window macOS app — see every agent grouped by project with a
live attention badge, spawn a new agent from the GUI, focus one and drive its terminal
(keys, mouse, IME, resize), and get a clear disconnected state if the daemon dies.

**Non-goals (later milestones):** companion split panes (M1); the Cmd-K palette / hotkeys
(M1); OS desktop notifications and the tray attention count (later in M0/M1); multi-window;
scrollback/reflow tuning beyond passing the size through; the Linux client.

## Component design

Everything for piece A lives in `macos/Sources/MuxyApp/`. Piece B is in `crates/muxy-client`
(+ a tiny helper the plan may place in `muxy-proto`). Piece C is in the existing
`SurfaceView.swift`.

### A1. `AppModel` — connection + selection state

`@MainActor final class AppModel: ObservableObject`. The single owner of the control
channel and the app's selection. Created once at app launch, retained by the `App`.

- Holds (retains) `private var connection: UnixSocketConnection?` and
  `private var session: ControlSession?` — retaining `session` is what keeps
  `ControlSession`'s `[weak self]` receiver alive.
- Exposes `let store: AgentStore` (the session's store) so views observe it.
- `@Published var selectedPane: UInt64?`
- `@Published private(set) var connectionState: ConnectionState` where
  `enum ConnectionState { case connecting, live, closed(reason: String) }`.
- `func connect()` — resolve the control socket path from `MUXY_CONTROL_SOCK`
  (fallback `/tmp/muxy-control.sock`); construct `UnixSocketConnection(path:)`; on throw set
  `connectionState = .closed(reason:)`; else build `ControlSession(transport:store:)`, set
  `.live`, and send `.listAgents` to hydrate. Registers an on-close handler (A1a) that sets
  `.closed` on the main thread.
- `func spawn(project:task:adapter:)` — `try? session?.send(.spawnAgent(...))`; surface a
  thrown/`nil`-session failure into `connectionState`/`lastError` path.
- `func shutdown()` — **calls `connection?.disconnect()` explicitly** and drops
  `session`/`connection`. This is the F1 fix: `deinit` cannot run while the read loop is
  parked in `read()`, so teardown must be explicit. Called from the app lifecycle (A3).

**A1a. Connection-closed signal (F2).** `ControlTransport` gains one optional hook so the
UI learns when the socket dies:

```swift
public protocol ControlTransport: AnyObject {
    func setReceiver(_ receiver: @escaping (String) -> Void)
    func send(line: String) throws
    func setOnClose(_ handler: @escaping () -> Void)   // NEW — invoked once, on main
}
```

`UnixSocketConnection` stores the handler and invokes it **once, on `DispatchQueue.main`**,
when `readLoop` exits (peer close, error, or `disconnect()`). A default protocol-extension
implementation (`{ }` no-op) keeps the b1 `FakeTransport` and its tests unchanged.
`AppModel.connect()` calls `connection.setOnClose { self.connectionState = .closed(...) }`.
This is the only change to `MuxyCore` in this milestone, and it is additive.

### A2. `SurfaceHost` — retained surface registry

`@MainActor final class SurfaceHost` owns one `SurfaceView` per pane so switching agents
never restarts `muxy attach`:

- `init(app: ghostty_app_t, muxyBinary: String, socketPath: String)`.
- `func view(for pane: UInt64) -> SurfaceView` — returns the cached view or lazily creates
  one (`SurfaceView(app:paneId:muxyBinary:socketPath:)`, unchanged initializer) and caches
  it. Views persist for the app's lifetime (an agent list of tens of panes is fine); no
  eviction in this milestone.

### A3. `MuxyApp` — the `@main` app and libghostty bootstrap

Replace the top-level `main.swift` with a SwiftUI `App`. libghostty must still be
initialized exactly once before any surface is created, and the wakeup callback must tick
the app — so the init sequence moves into an `NSApplicationDelegateAdaptor`:

- `AppDelegate: NSObject, NSApplicationDelegate` runs the existing init sequence in
  `applicationDidFinishLaunching` (or earlier): `ghostty_init` → config → the 6-callback
  runtime (unchanged; `wakeup_cb` still `DispatchQueue.main.async { ghostty_app_tick(gApp) }`)
  → `ghostty_app_new` → `ghostty_app_set_focus(app, true)`. Stores `app` and constructs the
  `SurfaceHost` and `AppModel`; calls `appModel.connect()`. In
  `applicationWillTerminate`, calls `appModel.shutdown()` (F1).
- `gApp` stays a top-level `var ghostty_app_t?` read by the C `wakeup_cb` (the callback
  can't capture Swift context).
- `@main struct MuxyApp: App` exposes the delegate via `@NSApplicationDelegateAdaptor` and
  renders `ContentView().environmentObject(appModel)` in a single `WindowGroup`.
- The old `muxy-app <pane-id>` single-surface entry is retired; the app no longer takes a
  pane argument. `MUXY_BIN`/`MUXY_CONTROL_SOCK` come from env.

### A4. `ContentView` — the sidebar + terminal layout

`NavigationSplitView`:

- **Sidebar** — a `List(selection:)` bound to `appModel.selectedPane`, built from
  `store.byProject`: a `Section` per project (header = project name, or its last path
  component), rows per `AgentInfo` showing task text + an attention badge. Badge = a small
  colored dot from `AttentionState` (`.needsInput` = the loud color, `.working` = active,
  `.completed`/`.exited`/`.idle` = muted); the exact palette is an implementation detail but
  `.needsInput` MUST be visually distinct (it's the whole point of the app).
- **Detail** — if `selectedPane` names a live agent, host
  `surfaceHost.view(for: pane)` via a `TerminalContainer` (A5); else a placeholder
  ("Select an agent" / "No agents yet — spawn one").
- **Toolbar** — a "+" button opening the spawn sheet (A6).
- **Footer / overlay** — when `store.lastError != nil`, show it (dismissable); when
  `connectionState` is `.closed`, show a persistent "Disconnected from daemon" banner (F2).
  While `.connecting`, a lightweight "Connecting…" state is acceptable.

### A5. `TerminalContainer` — `NSViewRepresentable`

Bridges the AppKit `SurfaceView` into SwiftUI:

- `struct TerminalContainer: NSViewRepresentable` with `let pane: UInt64` and a reference to
  the `SurfaceHost`.
- `makeNSView(context:) -> SurfaceView { surfaceHost.view(for: pane) }` — returns the
  retained per-pane view.
- `updateNSView` — no-op (the view is keyed by pane via `.id(pane)` at the SwiftUI call
  site, so selecting a different agent makes a different representable).
- After the view attaches, ensure it becomes first responder so keys route to it
  (`window?.makeFirstResponder(view)` on appearance).

### A6. Spawn sheet

A `SpawnSheet` view with three fields — project path, task, adapter (default `"claude"`) —
and Cancel / Spawn buttons. Spawn calls `appModel.spawn(project:task:adapter:)` and
dismisses. The daemon answers with `agentSpawned` → `needsRefresh` → the session
auto-`listAgents` → the sidebar populates the new row (no client-side optimism needed).
Minimal validation: non-empty project path and task; adapter defaults if blank.

### B. `muxy attach` resize (`crates/muxy-client`)

Make the client tell the daemon its live terminal size:

- **Initial size.** In `attach()` (or at the top of `pump`, after sending `Attach`), read
  the controlling tty's winsize (`nix`/`libc` `ioctl(fd, TIOCGWINSZ, &winsize)` on the
  stdout/stdin fd) and send one `ClientToDaemon::Resize { pane, cols, rows }` before the
  loop. This fixes the agent starting at the stale 80×24 default.
- **On change.** Add a third arm to `pump`'s `tokio::select!`: a
  `tokio::signal::unix::signal(SignalKind::window_change())` stream. On each tick, re-read
  the winsize and send `Resize`.
- **Testability.** Factor the size→message step into a pure, unit-testable function, e.g.
  `fn resize_msg(pane: PaneId, cols: u16, rows: u16) -> ClientToDaemon`, and structure
  `pump` so the resize source is injectable — the existing duplex-based `pump` test can then
  drive a resize through an in-memory channel and assert the daemon side receives
  `Resize { cols, rows }`, without delivering a real OS signal. Reading the actual winsize
  (`ioctl`) stays in `attach()` (the un-unit-tested I/O boundary), mirroring how `attach`
  already wraps the untestable raw-mode/stdio around the testable `pump`.
- `pump`'s signature may grow a resize-source parameter; update the one existing caller
  (`attach`) and the one existing test. No new proto variants (Resize already exists).

### C. Mouse + IME in `SurfaceView` (`macos/Sources/MuxyApp/SurfaceView.swift`)

Extend the existing view; keep the current key handling as-is.

- **Mouse buttons:** override `mouseDown/mouseUp/mouseDragged`,
  `rightMouseDown/Up/Dragged`, `otherMouseDown/Up/Dragged`. Each maps to
  `ghostty_surface_mouse_button(surface, GHOSTTY_MOUSE_PRESS|RELEASE, button, mods)` (button
  from `GHOSTTY_MOUSE_LEFT/RIGHT/MIDDLE`), and a preceding
  `ghostty_surface_mouse_pos(surface, x, y, mods)` for the cursor location. Reuse the
  existing `ghosttyMods(_:)` for `mods`.
- **Mouse move/drag position:** `mouseMoved`/`*Dragged` → `ghostty_surface_mouse_pos`.
  (Add a tracking area if `mouseMoved` is desired; drag positions are enough for MVP —
  hover-move can be an implementation choice.)
- **Coordinate convention:** convert `event.locationInWindow` to view coordinates and pass
  libghostty **point** coordinates with a **top-left origin** (AppKit is bottom-left, so
  flip: `y = bounds.height - localY`), matching Ghostty's own AppKit surface. Verify by
  clicking to position the cursor in a shell.
- **Scroll:** `scrollWheel` → `ghostty_surface_mouse_scroll(surface, dx, dy, scrollMods)`
  using `scrollingDeltaX/Y` (respect `hasPreciseScrollingDeltas`); `scrollMods` may be `0`
  for MVP (precise-delta flag encoding is optional refinement).
- **IME:** conform `SurfaceView` to `NSTextInputClient`. Route
  `setMarkedText`/`unmarkText` → `ghostty_surface_preedit(surface, bytes, len)` (empty on
  unmark), `insertText` → `ghostty_surface_text(surface, bytes, len)`, and implement
  `firstRect(forCharacterRange:)` using `ghostty_surface_ime_point` so the candidate window
  positions correctly. `keyDown` should feed the event to the input context
  (`interpretKeyEvents`/`inputContext?.handleEvent`) so composition routes through
  `NSTextInputClient`; committed non-composed keys continue via the existing
  `ghostty_surface_key` path. The plan must ensure plain ASCII typing still works (no
  regression to the b2-a proof) — this is the one place C could break existing behavior, so
  it gets explicit before/after verification.

## Data flow

```
daemon --JSON control line--> UnixSocketConnection.readLoop
   --DispatchQueue.main--> ControlSession.handle --> AgentStore.apply
   --@Published--> ContentView sidebar (byProject, badges, lastError)

spawn: SpawnSheet -> AppModel.spawn -> ControlSession.send(.spawnAgent)
   -> daemon -> agentSpawned -> needsRefresh -> session auto listAgents -> sidebar row

terminal: SurfaceView(app, "muxy attach <pane>") -- pump/pty --> daemon agent PTY
   keys/mouse/IME --ghostty_surface_*--> libghostty --> muxy attach --> daemon
   window resize --> ghostty PTY --SIGWINCH--> muxy attach --Resize--> daemon pane.resize

close: daemon dies -> readLoop breaks -> onClose (main) -> connectionState=.closed -> banner
app quit: applicationWillTerminate -> AppModel.shutdown -> connection.disconnect()  (F1)
```

## Testing

Automated (must stay/turn green in CI-style runs):

- **`swift test`** — the b1 core suite (25 tests) stays green. The one `MuxyCore` change
  (protocol `setOnClose` + no-op default) must not break `FakeTransport`/existing tests; add
  a small test that `UnixSocketConnection` invokes `onClose` on the main thread when the peer
  closes (extend the existing in-process-server test — server accepts, then closes; assert
  the handler fires on `Thread.isMainThread` via an `XCTestExpectation`).
- **`cargo test`** — the existing suite (39) stays green; add a `pump` resize test using the
  duplex harness + injected resize source asserting the daemon receives
  `Resize { cols, rows }`, plus a unit test for the pure `resize_msg`/winsize mapping.

Manual (**the user runs the app** — the SwiftUI/AppKit/libghostty layer isn't unit-tested):

1. Start the daemon; launch the app → sidebar shows existing agents grouped by project.
2. Click "+", spawn `claude` on a scratch git repo → a worktree/branch is created and a new
   sidebar row appears under that project.
3. Select the agent → its terminal renders; type (keys), click to move the cursor (mouse),
   and — if available — compose CJK via IME; resize the window → the shell reflows to the
   new size (piece B).
4. Trigger a Claude `Notification`/`Stop` → the row's badge flips to the `.needsInput` color.
5. Close and reopen the window → the agent is still listed and its terminal restored
   (survival).
6. Kill the daemon → the sidebar shows the "Disconnected" banner (F2); quitting the app is
   clean (no hang — F1 `disconnect()` ran).

## Risks

1. **SwiftUI + libghostty lifecycle.** libghostty was proven under a hand-rolled
   `NSApplication.run()`; moving init into an `NSApplicationDelegateAdaptor` under a
   `WindowGroup` must preserve the single-init + wakeup-tick invariants. Mitigation: keep the
   exact init sequence, only relocate it; `gApp` stays top-level for `wakeup_cb`. If the
   `WindowGroup` path fights the surface, fall back to the delegate creating the `NSWindow`
   directly and SwiftUI hosting only the sidebar — but try the clean `WindowGroup` first.
2. **IME regressing plain typing (piece C).** Routing keys through `NSTextInputClient` is the
   one change that can break the working ASCII path. Mitigation: explicit before/after manual
   check that Enter/Ctrl-C/normal characters still behave, and keep the non-composed path on
   `ghostty_surface_key`.
3. **Mouse coordinate origin.** Wrong Y flip = cursor lands in the wrong cell. Mitigation:
   follow Ghostty's AppKit mapping (top-left, points) and verify by clicking.
4. **Resize test without a real signal.** Mitigation: injectable resize source so the test
   never needs to raise SIGWINCH; the `ioctl` read stays outside the tested boundary.

## Verification gate

Milestone is done when: `swift test` and `cargo test` are green with the new tests; and the
user confirms the manual pass above — spawn from GUI, drive the terminal (keys/mouse/IME),
resize reflows, badge flips on attention, agent survives window close, and daemon death shows
a disconnected state while quit stays clean.
