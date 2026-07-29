# muxy M1d — Menu-Bar Attention Count

## Context

muxy tells you when an agent needs you. It already flips a sidebar badge and fires an OS
notification (daemon-side `OsNotifier`), but only while you're looking at the app. **M1d adds
an ambient menu-bar (status-bar) item showing how many agents need attention**, so you can
glance up from anywhere and jump straight to whoever's waiting — even with the muxy window
closed.

Brainstormed & approved decisions:
- **Count = `NeedsInput` + `Completed`** — agents that want a response (waiting for input, or
  finished a turn and want review). `Exited` is excluded (terminal status, not a pending ask);
  `Idle`/`Working` are not attention.
- **The app stays menu-bar-resident** — closing the window no longer quits muxy; the status
  item persists and the count keeps updating. Reopen from the tray or the dock.
- **Clicking the status item opens a dropdown menu** of the agents needing attention; clicking
  one selects it and brings the window forward. Plus "Show muxy Window" and "Quit muxy".

### What exists (ground truth)

`MuxyCore`: `AgentStore` (`@Published agents: [UInt64: AgentInfo]`, `byProject`,
`orderedAgents`, reactive), `AgentInfo {pane, project, task, state}`, `AttentionState
{idle, working, needsInput, completed, exited}`, `AppModel` (`@MainActor`; `store`,
`selectedPane`, republishes the store via `storeSubscription` so observing `AppModel`
catches store changes). `MuxyApp`: `AppDelegate` (`NSApplicationDelegate` via
`@NSApplicationDelegateAdaptor`; `bootstrap()` creates + retains `appModel`/`surfaceHost`;
`applicationDidFinishLaunching` sets `NSApp.setActivationPolicy(.regular)` + activates;
`applicationShouldTerminateAfterLastWindowClosed → true`), a single `WindowGroup` hosting
`ContentView`. No status-bar item yet.

## Goals / Non-goals

**Goals:** a status-bar item whose number reflects, live, how many agents are `NeedsInput`
or `Completed`; a menu listing those agents (project — task, with a state marker) that
selects one + shows the window on click; the app survives closing its window (the item and
count persist), and the window reopens from the item, its menu, or the dock.

**Non-goals (later):** deep-linking OS notification clicks to focus an agent (the daemon
already fires them — wiring the click is separate); per-project rollups / submenus in the
menu; an `.accessory` (no-dock-icon) mode; a preference to choose which states count.

## Global constraints

- **`MuxyCore` stays libghostty- and SwiftUI-free** — the count/list live on `AgentStore`
  (Foundation/Combine), unit-tested. The status item, menu, and window handling are `MuxyApp`
  (AppKit), verified by build + a manual run.

## Component design

### MuxyCore (pure, unit-tested)

Add to `AgentStore`:
```swift
/// Agents that want a response — NeedsInput or Completed — in sidebar order.
public var agentsNeedingAttention: [AgentInfo] {
    orderedAgents.filter { $0.state == .needsInput || $0.state == .completed }
}
/// How many agents need attention (the menu-bar count).
public var attentionCount: Int { agentsNeedingAttention.count }
```

### MuxyApp — `StatusBarController`

A `@MainActor final class StatusBarController: NSObject` that owns the status-bar UI and
observes the model:

- `init(appModel: AppModel, showWindow: @escaping () -> Void)`: creates
  `NSStatusBar.system.statusItem(withLength: .variableLength)`; subscribes to
  `appModel.objectWillChange` (Combine) and calls `refresh()` on each change (deferred to the
  next runloop tick so the store's `@Published` value has landed, mirroring the app's existing
  pattern); calls `refresh()` once initially.
- **`refresh()`** (main thread):
  - **Button:** `let n = appModel.store.attentionCount`. When `n > 0`, show an
    attention symbol + the number as the title (e.g. `image = bell.badge`, `title = " \(n)"`);
    when `n == 0`, a muted plain symbol (`bell`) and no title. (Exact symbols are an
    implementation detail; `n > 0` MUST be visually distinct.)
  - **Menu:** rebuild an `NSMenu`:
    - If `n == 0`, a single disabled item "No agents need attention".
    - Else one `NSMenuItem` per `appModel.store.agentsNeedingAttention`: title
      `"<projectBasename> — <task>"`, an image/marker distinguishing `NeedsInput` (loud) from
      `Completed`, `target = self`, `action = #selector(selectAgent(_:))`, and
      `representedObject = agent.pane`.
    - A separator, then **"Show muxy Window"** (`showWindow`), and **"Quit muxy"**
      (`NSApp.terminate`).
  - Set `statusItem.menu = menu` (a menu-bearing status item opens it on click).
- `@objc func selectAgent(_ sender: NSMenuItem)`: `appModel.selectedPane =
  sender.representedObject as? UInt64`; then `showWindow()`.

The controller is created in `AppDelegate.bootstrap()` (after `appModel`), passed a
`showWindow` closure (below), and retained on the delegate.

### MuxyApp — window survival (hide-on-close) + reopen

`WindowGroup` windows destroy on close, and reopening one from AppKit is unreliable — so muxy
**hides** its window instead of closing it, keeping the `NSWindow` alive to re-show:

- A tiny `NSViewRepresentable` (`WindowAccessor`) placed in `ContentView`'s background captures
  the host `NSWindow` on `viewDidMoveToWindow` and hands it to the delegate (once). The
  delegate sets `window.isReleasedWhenClosed = false` and installs a window delegate whose
  `windowShouldClose(_:)` does `sender.orderOut(nil)` and returns `false` — the red close
  button and ⌘W **hide** the window; the app keeps running with the status item.
- **`showWindow()`** (the closure handed to `StatusBarController`, and used by
  `applicationShouldHandleReopen`): `window.makeKeyAndOrderFront(nil)` +
  `NSApp.activate(ignoringOtherApps: true)`.
- `applicationShouldTerminateAfterLastWindowClosed → false` (with hide-on-close the app never
  sees a last-window-close, but this makes the intent explicit and covers any path that truly
  closes the window).
- `applicationShouldHandleReopen(_:hasVisibleWindows:)` → `showWindow(); return true` so
  clicking the dock icon reopens a hidden window.

## Data flow

```
store change ─► AppModel.objectWillChange ─► StatusBarController.refresh (deferred)
   ─► button shows attentionCount (NeedsInput+Completed) ; menu rebuilt from agentsNeedingAttention
click status item ─► menu opens
   click a needy agent ─► appModel.selectedPane = pane ; showWindow()
   "Show muxy Window" ─► showWindow() ;  "Quit muxy" ─► NSApp.terminate
close window (red / ⌘W) ─► windowShouldClose → orderOut, false (hidden; item + count persist)
dock icon ─► applicationShouldHandleReopen → showWindow
```

## Testing

Automated (`swift test`, MuxyCore):
- `attentionCount` / `agentsNeedingAttention` over a store hydrated (via `apply(.agentList(…))`
  or the fake transport) with a mix of states: `Idle`/`Working` contribute 0; each
  `NeedsInput` and `Completed` counts; `Exited` does not; the list is in `orderedAgents` order
  and contains exactly the needy agents.

Manual (**user runs it**; AppKit/status-bar layer): the menu-bar number tracks agents flipping
to/from NeedsInput/Completed live; opening the menu lists the right agents with correct
markers; clicking one selects it and brings the window forward; the count is `0`/muted with
none needy; closing the window hides it while the status item stays and keeps counting;
"Show muxy Window", the dock icon, and clicking a needy agent all reopen it; "Quit muxy" and
⌘Q quit.

## Risks

1. **Reopening a SwiftUI `WindowGroup` window from AppKit.** Mitigated by hide-on-close
   (`windowShouldClose → orderOut, false`, `isReleasedWhenClosed = false`) so the `NSWindow`
   is never destroyed and `makeKeyAndOrderFront` always works — no `openWindow`/reopen dance.
   Risk: capturing the right `NSWindow` (there's exactly one) via `WindowAccessor` at the right
   time — do it in `viewDidMoveToWindow`, guard against a nil/again window.
2. **Reactive refresh timing.** `objectWillChange` fires before the `@Published` update, so
   `refresh()` must be deferred (`DispatchQueue.main.async`) to read the new
   `attentionCount` — same pattern as `AppModel.reconcileFocus`.
3. **Status-item lifetime.** The `StatusBarController` (and its `NSStatusItem`) must be
   retained by the `AppDelegate` for the app's life, or the item vanishes.
4. **Hide-on-close UX surprise.** The red close button hiding (not quitting) is deliberate for
   a menu-bar-resident app; make Quit obviously available (tray item + ⌘Q).

## Verification gate

`swift test` green (existing + `attentionCount`/`agentsNeedingAttention` tests); `swift build`
clean; and the user confirms the manual pass — the count tracks NeedsInput+Completed live, the
menu lists + focuses needy agents, and the window hides on close while the item persists and
reopens on demand.
