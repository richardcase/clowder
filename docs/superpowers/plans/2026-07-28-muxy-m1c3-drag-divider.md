# muxy M1c-3 — Draggable Dividers + Focus Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make split dividers draggable (drag → `setSplitRatio`, persisted; clamped; no snap-back), and fold in the two M1c-2 carry-forwards: reset `focusedPane` when its leaf disappears, and tie libghostty surface focus to `isFocused` so only the focused pane shows a live cursor.

**Architecture:** A small `MuxyCore` layer (`AppModel.setDividerRatio` + `reconcileFocus`, wired into the store subscription) — unit-tested. In `MuxyApp`, `SurfaceView.setFocused` driven by `TerminalContainer.isFocused`, and the `.split` case extracted into a `SplitNode` view owning a draggable divider with a local optimistic ratio that syncs from the daemon echo.

**Tech Stack:** Swift 6 (v5 mode, macOS 14), SwiftUI + AppKit, Combine; `MuxyCore` (Foundation/Combine only).

## Global Constraints

- **`MuxyCore` stays libghostty- and SwiftUI-free.** The new `AppModel` logic imports only Foundation/Combine.
- **Ratio is clamped to `[0.05, 0.95]`** client-side (matching the daemon), both when dragging and in `setDividerRatio`.
- **No snap-back:** the dragged divider stays put after release — the local ratio syncs from the tree's ratio (the daemon echo) via `.onChange`, it is never cleared to `nil` on end.
- Commit after each task; conventional messages + standard trailers.

**Test commands:** Core: `cd macos && swift test`. App build gate: `cd macos && swift build`.

---

## Task 1: `setDividerRatio` + focus reconciliation (MuxyCore)

**Files:**
- Modify: `macos/Sources/MuxyCore/AppModel.swift`
- Test: `macos/Tests/MuxyCoreTests/DividerFocusTests.swift`

**Interfaces:**
- Consumes: `ControlRequest.setSplitRatio`, `currentTree`/`focusedPane`/`selectedPane`, the existing `storeSubscription`.
- Produces: `AppModel.setDividerRatio(split:ratio:)`, `AppModel.reconcileFocus()`.

- [ ] **Step 1: Write the failing tests** (`DividerFocusTests.swift`) — reuse the test target's `FakeControlTransport` (with `deliver`):

```swift
import XCTest
@testable import MuxyCore

@MainActor
final class DividerFocusTests: XCTestCase {
    private func liveModel() -> (AppModel, FakeControlTransport) {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        return (model, fake)
    }
    // agent 1 tree with leaves [1,2,3]
    private let tree123 = #"{"type":"splitTreeChanged","agent":1,"tree":{"kind":"split","id":1,"axis":"horizontal","ratio":0.5,"first":{"kind":"leaf","pane":1},"second":{"kind":"split","id":2,"axis":"vertical","ratio":0.5,"first":{"kind":"leaf","pane":2},"second":{"kind":"leaf","pane":3}}}}"#
    // agent 1 tree with leaves [1,3] (pane 2 gone)
    private let tree13 = #"{"type":"splitTreeChanged","agent":1,"tree":{"kind":"split","id":1,"axis":"horizontal","ratio":0.5,"first":{"kind":"leaf","pane":1},"second":{"kind":"leaf","pane":3}}}"#

    func testSetDividerRatioClampsAndSends() {
        let (model, fake) = liveModel()
        model.setDividerRatio(split: 5, ratio: 2.0)          // clamps to 0.95
        XCTAssertTrue(fake.sentLines.contains {
            $0.contains("\"type\":\"setSplitRatio\"") && $0.contains("\"split\":5") && $0.contains("0.95")
        })
        model.setDividerRatio(split: 5, ratio: -1.0)         // clamps to 0.05
        XCTAssertTrue(fake.sentLines.contains { $0.contains("0.05") })
    }

    func testReconcileFocusResetsWhenLeafGone() {
        let (model, fake) = liveModel()
        model.selectedPane = 1
        fake.deliver(tree123)
        model.focusedPane = 2
        fake.deliver(tree13)            // pane 2 no longer a leaf
        model.reconcileFocus()
        XCTAssertEqual(model.focusedPane, 1)   // reset to the agent (selectedPane)
    }

    func testReconcileFocusKeepsValidFocus() {
        let (model, fake) = liveModel()
        model.selectedPane = 1
        fake.deliver(tree123)
        model.focusedPane = 2
        model.reconcileFocus()
        XCTAssertEqual(model.focusedPane, 2)   // still a leaf → unchanged
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter DividerFocusTests`
Expected: FAIL — the methods don't exist.

- [ ] **Step 3: Add the methods** to `AppModel.swift`:

```swift
    /// Send a new divider ratio (clamped to [0.05, 0.95], matching the daemon) to the daemon.
    public func setDividerRatio(split: UInt64, ratio: Double) {
        guard session != nil else { return }
        let r = min(0.95, max(0.05, ratio))
        try? session?.send(.setSplitRatio(split: split, ratio: r))
    }

    /// If the focused pane is no longer a leaf of the current tree (a companion closed, or an
    /// external tree change), move focus back to the agent pane.
    func reconcileFocus() {
        guard let leaves = currentTree?.leaves else { return }   // no tree → leave focus as-is
        if let f = focusedPane, !leaves.contains(f) {
            focusedPane = selectedPane
        }
    }
```

- [ ] **Step 4: Wire `reconcileFocus` into the store subscription.** Find the existing `storeSubscription` assignment and add a deferred `reconcileFocus` call (deferred because `objectWillChange` fires *before* the store's `@Published` property updates, so `currentTree` must be read on the next runloop tick):

```swift
        self.storeSubscription = store.objectWillChange.sink { [weak self] _ in
            self?.objectWillChange.send()
            DispatchQueue.main.async { self?.reconcileFocus() }
        }
```

- [ ] **Step 5: Run to verify it passes**

Run: `cd macos && swift test`
Expected: PASS — existing 61 + the 3 new.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/MuxyCore/AppModel.swift macos/Tests/MuxyCoreTests/DividerFocusTests.swift
git commit -m "feat(core): setDividerRatio (clamped) + focus reconciliation on tree change"
```

---

## Task 2: Surface focus follows `isFocused` (MuxyApp)

Only the focused pane should show a live libghostty cursor. Gate: `swift build`.

**Files:**
- Modify: `macos/Sources/MuxyApp/SurfaceView.swift`
- Modify: `macos/Sources/MuxyApp/TerminalContainer.swift`

**Interfaces:**
- Consumes: `TerminalContainer.isFocused` (M1c-2).
- Produces: `SurfaceView.setFocused(_:)`.

- [ ] **Step 1: Add `setFocused` + a stored flag to `SurfaceView.swift`.** Add the property and method, and make `createSurface` seed the initial focus from the flag instead of unconditional `true`:

```swift
    private var wantsFocus = false

    /// Tie libghostty surface focus to whether this pane is the focused split leaf.
    func setFocused(_ focused: Bool) {
        wantsFocus = focused
        if let surface { ghostty_surface_set_focus(surface, focused) }
    }
```
Then change the line in `createSurface` from:
```swift
        ghostty_surface_set_focus(surface, true)
```
to:
```swift
        ghostty_surface_set_focus(surface, wantsFocus)
```

> `TerminalContainer.updateNSView` runs after the view is made and calls `setFocused(isFocused)`, which sets `wantsFocus` (and applies it live if the surface already exists). When the surface is created later in `viewDidMoveToWindow`, it seeds the initial focus from `wantsFocus`.

- [ ] **Step 2: Drive `setFocused` from `TerminalContainer.swift`.** In `updateNSView`, call `setFocused` alongside the existing first-responder logic:

```swift
    func updateNSView(_ nsView: SurfaceView, context: Context) {
        nsView.onFocus = onFocus
        nsView.setFocused(isFocused)
        if isFocused, nsView.window?.firstResponder !== nsView {
            DispatchQueue.main.async { nsView.window?.makeFirstResponder(nsView) }
        }
    }
```

> Keep the rest of `TerminalContainer` (the `isFocused`/`onFocus` params, `makeNSView`) as-is; only add the `nsView.setFocused(isFocused)` line.

- [ ] **Step 3: Build**

Run: `cd macos && swift build`
Expected: builds clean.

- [ ] **Step 4: Manual smoke (recorded).** With an agent split into ≥2 panes: only the focused pane shows a blinking cursor; clicking another pane moves the live cursor to it (and the focus ring follows). A single-pane agent still shows its cursor when focused.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/MuxyApp/SurfaceView.swift macos/Sources/MuxyApp/TerminalContainer.swift
git commit -m "feat(app): tie libghostty surface focus to the focused pane"
```

---

## Task 3: Draggable divider (`SplitNode`) + leaf identity (MuxyApp)

Make the divider draggable with a local optimistic ratio that syncs from the daemon echo. Gate: `swift build`.

**Files:**
- Modify: `macos/Sources/MuxyApp/SplitContainer.swift`

**Interfaces:**
- Consumes: `AppModel.setDividerRatio` (Task 1), `PaneTree`, `Axis`, `SurfaceHost`, `TerminalContainer`.

- [ ] **Step 1: Replace `SplitContainer.swift`** with the version that adds `.id(pane)` to leaves and delegates splits to a draggable `SplitNode`:

```swift
import SwiftUI
import AppKit
import MuxyCore

/// Recursively renders a PaneTree: leaves become terminals, splits delegate to SplitNode
/// (which owns a draggable divider).
struct SplitContainer: View {
    let node: PaneTree
    let surfaceHost: SurfaceHost
    @Binding var focusedPane: UInt64?

    var body: some View {
        switch node {
        case let .leaf(pane):
            TerminalContainer(pane: pane, surfaceHost: surfaceHost,
                              isFocused: focusedPane == pane,
                              onFocus: { focusedPane = pane })
                .overlay(
                    RoundedRectangle(cornerRadius: 3)
                        .strokeBorder(focusedPane == pane ? Color.accentColor : Color.clear, lineWidth: 2)
                        .allowsHitTesting(false)
                )
                .id(pane)
        case let .split(id, axis, ratio, first, second):
            SplitNode(id: id, axis: axis, ratio: ratio, first: first, second: second,
                      surfaceHost: surfaceHost, focusedPane: $focusedPane)
        }
    }
}

/// One split node: two children along `axis` with a draggable divider. The rendered ratio is
/// `localRatio ?? ratio`; dragging updates `localRatio` (clamped) and, on release, sends
/// `setSplitRatio`; `.onChange(of: ratio)` syncs `localRatio` to the daemon echo so there's no
/// snap-back and external changes are honored.
private struct SplitNode: View {
    let id: UInt64
    let axis: Axis
    let ratio: Double
    let first: PaneTree
    let second: PaneTree
    let surfaceHost: SurfaceHost
    @Binding var focusedPane: UInt64?
    @EnvironmentObject var model: AppModel

    @State private var localRatio: Double?
    @State private var dragStart: Double?

    private let thickness: CGFloat = 6

    private var effective: Double { localRatio ?? ratio }

    var body: some View {
        GeometryReader { geo in
            let horizontal = axis == .horizontal
            let total = horizontal ? geo.size.width : geo.size.height
            let firstLen = max(0, total * effective - thickness / 2)
            Group {
                if horizontal {
                    HStack(spacing: 0) {
                        SplitContainer(node: first, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                            .frame(width: firstLen)
                        divider(total: total, horizontal: true)
                        SplitContainer(node: second, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                    }
                } else {
                    VStack(spacing: 0) {
                        SplitContainer(node: first, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                            .frame(height: firstLen)
                        divider(total: total, horizontal: false)
                        SplitContainer(node: second, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                    }
                }
            }
            .onChange(of: ratio) { _, newValue in localRatio = newValue }   // sync from daemon echo / external (macOS 14 two-param form)
        }
    }

    @ViewBuilder
    private func divider(total: CGFloat, horizontal: Bool) -> some View {
        Rectangle()
            .fill(Color.gray.opacity(0.35))
            .frame(width: horizontal ? thickness : nil, height: horizontal ? nil : thickness)
            .contentShape(Rectangle())
            .onHover { inside in
                if inside { (horizontal ? NSCursor.resizeLeftRight : NSCursor.resizeUpDown).set() }
                else { NSCursor.arrow.set() }
            }
            .gesture(
                DragGesture()
                    .onChanged { value in
                        let start = dragStart ?? effective
                        if dragStart == nil { dragStart = start }
                        let delta = horizontal ? value.translation.width : value.translation.height
                        localRatio = min(0.95, max(0.05, start + delta / max(total, 1)))
                    }
                    .onEnded { _ in
                        if let r = localRatio { model.setDividerRatio(split: id, ratio: r) }
                        dragStart = nil
                    }
            )
    }
}
```

> `SplitNode` reads `AppModel` via `@EnvironmentObject` — `ContentView` already injects it, and child views inherit the environment. The `DragGesture` uses `dragStart` (captured once per drag) + the cumulative `translation` so the ratio doesn't compound across `onChanged` frames.

- [ ] **Step 2: Build**

Run: `cd macos && swift build`
Expected: builds clean. Also `cd macos && swift test` — MuxyCore suite still green.

- [ ] **Step 3: Manual smoke (recorded; user runs the full pass).** With an agent split into ≥2 panes: hover the divider → resize cursor; drag it → the panes reflow smoothly under the cursor; release → the ratio holds (no snap-back). Dragging to an extreme stops at ~5%/95% (a pane can't collapse to zero). Close + reopen the window → the dragged ratio is restored (daemon-owned). Terminal clicks right next to a divider still register in the pane.

- [ ] **Step 4: Commit**

```bash
git add macos/Sources/MuxyApp/SplitContainer.swift
git commit -m "feat(app): draggable split dividers (setSplitRatio, no snap-back) + leaf id"
```

---

## Final verification

- `cd macos && swift test` → existing 61 + `DividerFocusTests` (3), all green.
- `cd macos && swift build` → clean on macOS 14.
- Manual (user): dividers drag smoothly and the ratio persists across window reopen; only the focused pane shows a live cursor; `focusedPane` never sticks on a gone pane. This completes M1c (companion split panes).
