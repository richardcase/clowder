# muxy M1d — Menu-Bar Attention Count Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A menu-bar (status-bar) item showing how many agents are `NeedsInput` or `Completed`, with a menu that jumps to any needy agent; the app survives closing its window (stays menu-bar-resident) and reopens on demand.

**Architecture:** The count/list live on `AgentStore` in `MuxyCore` (unit-tested). In `MuxyApp`: hide-on-close window survival (a `WindowAccessor` captures the `NSWindow`; the close button hides it) + `applicationShouldTerminateAfterLastWindowClosed = false`, and a `StatusBarController` that owns an `NSStatusItem`, observes the model reactively, and drives the button count + menu.

**Tech Stack:** Swift 6 (v5 mode, macOS 14), SwiftUI + AppKit, Combine; `MuxyCore` (Foundation/Combine only).

## Global Constraints

- **`MuxyCore` stays libghostty- and SwiftUI-free.** The count/list are pure `AgentStore` computed properties; `swift test` runs without the vendored lib. The status item, menu, and window handling are all `MuxyApp` (AppKit), verified by `swift build` + a manual run.
- **Deferred reactive refresh:** `objectWillChange` fires *before* the `@Published` update, so the status refresh must be deferred (`DispatchQueue.main.async`) to read the new count — the same pattern as `AppModel.reconcileFocus`.
- Commit after each task; conventional messages + standard trailers.

**Test commands:** Core: `cd macos && swift test`. App build gate: `cd macos && swift build`.

---

## Task 1: Attention count + list (MuxyCore)

**Files:**
- Modify: `macos/Sources/MuxyCore/AgentStore.swift`
- Test: `macos/Tests/MuxyCoreTests/AttentionCountTests.swift`

**Interfaces:**
- Consumes: `orderedAgents`, `AttentionState`, `AgentInfo`.
- Produces: `AgentStore.agentsNeedingAttention: [AgentInfo]`, `AgentStore.attentionCount: Int`.

- [ ] **Step 1: Write the failing tests** (`AttentionCountTests.swift`):

```swift
import XCTest
@testable import MuxyCore

final class AttentionCountTests: XCTestCase {
    func testCountsNeedsInputAndCompletedInOrder() {
        let store = AgentStore()
        store.apply(.agentList([
            AgentInfo(pane: 1, project: "/a", task: "t1", state: .idle),
            AgentInfo(pane: 2, project: "/a", task: "t2", state: .working),
            AgentInfo(pane: 3, project: "/a", task: "t3", state: .needsInput),
            AgentInfo(pane: 4, project: "/b", task: "t4", state: .completed),
            AgentInfo(pane: 5, project: "/b", task: "t5", state: .exited),
        ]))
        XCTAssertEqual(store.attentionCount, 2)                              // needsInput + completed
        XCTAssertEqual(store.agentsNeedingAttention.map(\.pane), [3, 4])     // orderedAgents order, needy only
    }

    func testZeroWhenNoneNeedy() {
        let store = AgentStore()
        store.apply(.agentList([
            AgentInfo(pane: 1, project: "/a", task: "t", state: .working),
            AgentInfo(pane: 2, project: "/a", task: "t", state: .exited),
        ]))
        XCTAssertEqual(store.attentionCount, 0)
        XCTAssertTrue(store.agentsNeedingAttention.isEmpty)
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter AttentionCountTests`
Expected: FAIL — the properties don't exist.

- [ ] **Step 3: Add the computed properties** to `AgentStore.swift` (near `byProject`/`orderedAgents`):

```swift
    /// Agents that want a response — NeedsInput or Completed — in sidebar order.
    public var agentsNeedingAttention: [AgentInfo] {
        orderedAgents.filter { $0.state == .needsInput || $0.state == .completed }
    }

    /// How many agents need attention (the menu-bar count).
    public var attentionCount: Int { agentsNeedingAttention.count }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd macos && swift test`
Expected: PASS — existing 64 + the 2 new.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/MuxyCore/AgentStore.swift macos/Tests/MuxyCoreTests/AttentionCountTests.swift
git commit -m "feat(core): attentionCount + agentsNeedingAttention (NeedsInput + Completed)"
```

---

## Task 2: Window survival — hide-on-close + reopen (MuxyApp)

Keep the app alive when its window closes (hide it, keep the `NSWindow`), and reopen from the dock. Gate: `swift build`.

**Files:**
- Create: `macos/Sources/MuxyApp/WindowAccessor.swift`
- Modify: `macos/Sources/MuxyApp/App.swift` (adopt the window, hide-on-close delegate, `showWindow`, lifecycle)

**Interfaces:**
- Produces: `AppDelegate.showWindow()`, `AppDelegate.adoptWindow(_:)`; the app no longer quits on last window close.

- [ ] **Step 1: Create `WindowAccessor.swift`** — captures the host `NSWindow` and hands it to a callback:

```swift
import SwiftUI
import AppKit

/// A zero-size background view that reports its host NSWindow once it's attached.
struct WindowAccessor: NSViewRepresentable {
    let onWindow: (NSWindow) -> Void

    func makeNSView(context: Context) -> NSView { CaptureView(onWindow: onWindow) }
    func updateNSView(_ nsView: NSView, context: Context) {}

    private final class CaptureView: NSView {
        let onWindow: (NSWindow) -> Void
        init(onWindow: @escaping (NSWindow) -> Void) {
            self.onWindow = onWindow
            super.init(frame: .zero)
        }
        required init?(coder: NSCoder) { fatalError("not used") }
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if let window { onWindow(window) }
        }
    }
}
```

- [ ] **Step 2: Add a hide-on-close window delegate + adoption + `showWindow` to `AppDelegate`** in `App.swift`. Add the stored refs and methods:

```swift
    private var mainWindow: NSWindow?
    private var windowCloseDelegate: HideOnCloseDelegate?

    /// Called by WindowAccessor when the window attaches. Runs once; makes the red close
    /// button hide (not destroy) the window so the app stays menu-bar-resident.
    func adoptWindow(_ window: NSWindow) {
        guard mainWindow == nil else { return }
        mainWindow = window
        window.isReleasedWhenClosed = false
        let d = HideOnCloseDelegate()
        windowCloseDelegate = d
        window.delegate = d
    }

    /// Bring the (possibly hidden) window to the front.
    func showWindow() {
        mainWindow?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }
```
And a small delegate class (top-level in `App.swift`):
```swift
/// Hides the window on close instead of destroying it, so the app stays alive in the menu bar.
final class HideOnCloseDelegate: NSObject, NSWindowDelegate {
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        sender.orderOut(nil)
        return false
    }
}
```

- [ ] **Step 3: Wire the accessor + lifecycle.** In the `WindowGroup` scene body, add the accessor as a background so the delegate adopts the window:

```swift
        WindowGroup {
            let boot = delegate.bootstrap()
            ContentView(surfaceHost: boot.surfaceHost)
                .environmentObject(boot.appModel)
                .frame(minWidth: 900, minHeight: 560)
                .background(WindowAccessor { window in delegate.adoptWindow(window) })
        }
```
Change the terminate-on-close policy and add reopen handling in `AppDelegate`:
```swift
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        showWindow()
        return true
    }
```
(Replace the existing `applicationShouldTerminateAfterLastWindowClosed` returning `true`.)

- [ ] **Step 4: Build**

Run: `cd macos && swift build`
Expected: builds clean.

> If setting `window.delegate` visibly breaks SwiftUI window behavior at runtime (a known risk — SwiftUI may use its own delegate), the fallback is to forward unhandled `NSWindowDelegate` calls, but first verify: `windowShouldClose` returning false simply prevents the close, so SwiftUI never tears the content down. Note any adaptation in the report.

- [ ] **Step 5: Manual smoke (recorded).** Launch the app: close the window with the red button (or ⌘W) → the window hides and the app keeps running (dock icon stays; process alive). Click the dock icon → the window reopens (`applicationShouldHandleReopen`). ⌘Q still quits. (The status item that reopens it comes in Task 3.)

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/MuxyApp/WindowAccessor.swift macos/Sources/MuxyApp/App.swift
git commit -m "feat(app): stay menu-bar-resident — hide window on close, reopen from dock"
```

---

## Task 3: `StatusBarController` (MuxyApp)

The status-bar item: count + menu of needy agents, reactive. Gate: `swift build`.

**Files:**
- Create: `macos/Sources/MuxyApp/StatusBarController.swift`
- Modify: `macos/Sources/MuxyApp/App.swift` (create + retain the controller in `bootstrap`)

**Interfaces:**
- Consumes: `AppModel` (`store.attentionCount`/`agentsNeedingAttention`, `selectedPane`), `AppDelegate.showWindow` (Task 2), `AttentionState`.

- [ ] **Step 1: Create `StatusBarController.swift`:**

```swift
import AppKit
import Combine
import MuxyCore

/// Owns the menu-bar status item: a live attention count + a menu of agents needing attention.
@MainActor
final class StatusBarController: NSObject {
    private let appModel: AppModel
    private let showWindow: () -> Void
    private let statusItem: NSStatusItem
    private var cancellable: AnyCancellable?

    init(appModel: AppModel, showWindow: @escaping () -> Void) {
        self.appModel = appModel
        self.showWindow = showWindow
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        super.init()
        // objectWillChange fires before the @Published update, so refresh on the next tick.
        cancellable = appModel.objectWillChange.sink { [weak self] _ in
            DispatchQueue.main.async { self?.refresh() }
        }
        refresh()
    }

    private func refresh() {
        let needy = appModel.store.agentsNeedingAttention
        let n = needy.count

        if let button = statusItem.button {
            if n > 0 {
                button.image = NSImage(systemSymbolName: "bell.badge.fill", accessibilityDescription: "agents need attention")
                button.imagePosition = .imageLeading
                button.title = " \(n)"
            } else {
                button.image = NSImage(systemSymbolName: "bell", accessibilityDescription: "muxy")
                button.imagePosition = .imageOnly
                button.title = ""
            }
        }

        let menu = NSMenu()
        if needy.isEmpty {
            let item = NSMenuItem(title: "No agents need attention", action: nil, keyEquivalent: "")
            item.isEnabled = false
            menu.addItem(item)
        } else {
            for agent in needy {
                let proj = (agent.project as NSString).lastPathComponent
                let name = proj.isEmpty ? agent.project : proj
                let marker = agent.state == .needsInput ? "🔴" : "🔵"   // NeedsInput vs Completed
                let item = NSMenuItem(title: "\(marker) \(name) — \(agent.task)",
                                      action: #selector(selectAgent(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = agent.pane
                menu.addItem(item)
            }
        }
        menu.addItem(.separator())
        addItem(to: menu, "Show muxy Window", #selector(showWindowAction))
        let quit = addItem(to: menu, "Quit muxy", #selector(quitAction))
        quit.keyEquivalent = "q"

        statusItem.menu = menu
    }

    @discardableResult
    private func addItem(to menu: NSMenu, _ title: String, _ action: Selector) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
        item.target = self
        menu.addItem(item)
        return item
    }

    @objc private func selectAgent(_ sender: NSMenuItem) {
        if let pane = sender.representedObject as? UInt64 { appModel.selectedPane = pane }
        showWindow()
    }
    @objc private func showWindowAction() { showWindow() }
    @objc private func quitAction() { NSApp.terminate(nil) }
}
```

- [ ] **Step 2: Create + retain the controller in `AppDelegate.bootstrap()`.** Add a stored property and instantiate it after `appModel` is set (in the non-early-return path of `bootstrap`):

```swift
    private var statusBar: StatusBarController?
```
After `appModel = model` (and before `return (model, host)`):
```swift
        statusBar = StatusBarController(appModel: model, showWindow: { [weak self] in self?.showWindow() })
```

- [ ] **Step 3: Build**

Run: `cd macos && swift build`
Expected: builds clean. Also `cd macos && swift test` — MuxyCore suite still green.

- [ ] **Step 4: Manual smoke (recorded; user runs the full pass).** With a daemon + agents: a status-bar item appears. As an agent flips to NeedsInput/Completed, the number goes up (and back down when it resumes/exits). Click the item → a menu lists the needy agents (🔴 NeedsInput / 🔵 Completed, project — task); clicking one selects that agent and brings the window forward. With none needy, the item is muted and the menu says "No agents need attention". "Show muxy Window" reopens a hidden window; "Quit muxy" quits. The count keeps updating with the window closed.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/MuxyApp/StatusBarController.swift macos/Sources/MuxyApp/App.swift
git commit -m "feat(app): menu-bar status item — attention count + agent menu"
```

---

## Final verification

- `cd macos && swift test` → existing 64 + `AttentionCountTests` (2), all green.
- `cd macos && swift build` → clean on macOS 14.
- Manual (user): the menu-bar count tracks NeedsInput+Completed live, the menu lists + focuses needy agents, and the window hides on close while the item persists, counts, and reopens on demand.
