# muxy M1c-2 — Split-Pane Client Render Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render the selected agent's split tree in the detail area (agent pane + companions, each a live terminal), and drive split/close/focus from the keyboard + palette (⌘D / ⌘⇧D / ⌘⇧W / ⌘]), against the daemon built in M1c-1.

**Architecture:** A Swift `PaneTree` model + control messages + per-agent tree storage + focus/actions in `MuxyCore` (unit-tested, no libghostty). A recursive `SplitContainer` view in `MuxyApp` renders the tree, reusing the existing per-pane `SurfaceHost`; click-to-focus via native first responder; the split/close/focus commands wired into the M1a menu + palette + keymap.

**Tech Stack:** Swift 6 (v5 mode, macOS 14), SwiftUI + AppKit, Combine; `MuxyCore` (Foundation/Combine only).

## Global Constraints

- **`MuxyCore` stays libghostty- and SwiftUI-free** — `PaneTree`, the control types, `AgentStore`, and `AppModel` import only Foundation/Combine; `swift test` runs without the vendored lib.
- **Honor the M1c-1 carry-forwards:** (1) apply tree snapshots **idempotently** — `SplitTreeChanged` replaces the stored tree (never append/merge), and it arrives twice per mutation on the initiating connection; (2) `PaneTree` is a recursive internally-tagged enum → a **manual `Codable`** (`indirect enum`, switch on `"kind"`); (3) **no per-companion `AgentRemoved`** — a companion vanishing is learned only from a `SplitTreeChanged` without that leaf; the agent's `AgentRemoved` drops the whole tree.
- **Wire conventions** (from M1c-1): `PaneId`/`SplitId` are bare numbers; `axis` is `"horizontal"`/`"vertical"`; `direction` is `"right"`/`"down"`; `PaneTree` tagged on `"kind"`; requests/events tagged on `"type"` camelCase.
- Commit after each task; conventional messages + standard trailers.

**Test commands:** Core: `cd macos && swift test`. App build gate: `cd macos && swift build`.

---

## Task 1: `PaneTree` model (MuxyCore)

**Files:**
- Create: `macos/Sources/MuxyCore/PaneTree.swift`
- Test: `macos/Tests/MuxyCoreTests/PaneTreeTests.swift`

**Interfaces:**
- Produces: `Axis`, `SplitDirection`, `PaneTree` (+ `PaneTree.leaves`).

- [ ] **Step 1: Write the failing tests** (`PaneTreeTests.swift`):

```swift
import XCTest
@testable import MuxyCore

final class PaneTreeTests: XCTestCase {
    func testDecodeLeaf() throws {
        let t = try JSONDecoder().decode(PaneTree.self, from: Data(#"{"kind":"leaf","pane":7}"#.utf8))
        XCTAssertEqual(t, .leaf(pane: 7))
        XCTAssertEqual(t.leaves, [7])
    }

    func testDecodeNestedSplitInOrder() throws {
        let json = #"{"kind":"split","id":1,"axis":"horizontal","ratio":0.5,"first":{"kind":"leaf","pane":1},"second":{"kind":"split","id":2,"axis":"vertical","ratio":0.3,"first":{"kind":"leaf","pane":2},"second":{"kind":"leaf","pane":3}}}"#
        let t = try JSONDecoder().decode(PaneTree.self, from: Data(json.utf8))
        XCTAssertEqual(t.leaves, [1, 2, 3])
        guard case let .split(id, axis, ratio, first, second) = t else { return XCTFail("expected split") }
        XCTAssertEqual(id, 1)
        XCTAssertEqual(axis, .horizontal)
        XCTAssertEqual(ratio, 0.5, accuracy: 1e-9)
        XCTAssertEqual(first, .leaf(pane: 1))
        if case .split(_, .vertical, _, _, _) = second {} else { XCTFail("second should be a vertical split") }
    }

    func testUnknownKindThrows() {
        XCTAssertThrowsError(try JSONDecoder().decode(PaneTree.self, from: Data(#"{"kind":"blah"}"#.utf8)))
    }

    func testSplitDirectionEncodesLowercase() throws {
        XCTAssertEqual(String(decoding: try JSONEncoder().encode(SplitDirection.right), as: UTF8.self), "\"right\"")
        XCTAssertEqual(String(decoding: try JSONEncoder().encode(SplitDirection.down), as: UTF8.self), "\"down\"")
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter PaneTreeTests`
Expected: FAIL — no `PaneTree`.

- [ ] **Step 3: Implement `PaneTree.swift`:**

```swift
import Foundation

public enum Axis: String, Decodable, Equatable, Sendable {
    case horizontal, vertical
}

public enum SplitDirection: String, Encodable, Equatable, Sendable {
    case right, down
}

/// Mirrors the Rust `PaneTree` (internally tagged on "kind"). Recursive → `indirect`,
/// with a manual decoder (Swift can't synthesize Codable for a tagged recursive enum).
public indirect enum PaneTree: Decodable, Equatable, Sendable {
    case leaf(pane: UInt64)
    case split(id: UInt64, axis: Axis, ratio: Double, first: PaneTree, second: PaneTree)

    private enum CodingKeys: String, CodingKey { case kind, pane, id, axis, ratio, first, second }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "leaf":
            self = .leaf(pane: try c.decode(UInt64.self, forKey: .pane))
        case "split":
            self = .split(
                id: try c.decode(UInt64.self, forKey: .id),
                axis: try c.decode(Axis.self, forKey: .axis),
                ratio: try c.decode(Double.self, forKey: .ratio),
                first: try c.decode(PaneTree.self, forKey: .first),
                second: try c.decode(PaneTree.self, forKey: .second))
        case let other:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: c, debugDescription: "unknown PaneTree kind: \(other)")
        }
    }

    /// Pane ids in render order (first-then-second).
    public var leaves: [UInt64] {
        switch self {
        case let .leaf(pane): return [pane]
        case let .split(_, _, _, first, second): return first.leaves + second.leaves
        }
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd macos && swift test`
Expected: PASS — existing 47 + the 4 new.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/MuxyCore/PaneTree.swift macos/Tests/MuxyCoreTests/PaneTreeTests.swift
git commit -m "feat(core): PaneTree model (manual recursive Codable) + Axis/SplitDirection"
```

---

## Task 2: Split control messages (MuxyCore `Models.swift`)

**Files:**
- Modify: `macos/Sources/MuxyCore/Models.swift` (`ControlRequest` cases + `ControlEvent` case)
- Test: `macos/Tests/MuxyCoreTests/SplitControlMessagesTests.swift`

**Interfaces:**
- Consumes: `PaneTree`, `SplitDirection` (Task 1).
- Produces: `ControlRequest.{splitPane, closePane, setSplitRatio, getSplitTree}`; `ControlEvent.splitTreeChanged`.

- [ ] **Step 1: Write the failing tests** (`SplitControlMessagesTests.swift`):

```swift
import XCTest
@testable import MuxyCore

final class SplitControlMessagesTests: XCTestCase {
    private func obj(_ req: ControlRequest) throws -> [String: Any] {
        try JSONSerialization.jsonObject(with: JSONEncoder().encode(req)) as! [String: Any]
    }

    func testSplitPaneEncodes() throws {
        let o = try obj(.splitPane(pane: 3, direction: .right))
        XCTAssertEqual(o["type"] as? String, "splitPane")
        XCTAssertEqual(o["pane"] as? Int, 3)
        XCTAssertEqual(o["direction"] as? String, "right")
    }

    func testCloseGetRatioEncode() throws {
        XCTAssertEqual(try obj(.closePane(pane: 5))["type"] as? String, "closePane")
        XCTAssertEqual(try obj(.closePane(pane: 5))["pane"] as? Int, 5)
        XCTAssertEqual(try obj(.getSplitTree(agent: 1))["type"] as? String, "getSplitTree")
        XCTAssertEqual(try obj(.getSplitTree(agent: 1))["agent"] as? Int, 1)
        XCTAssertEqual(try obj(.setSplitRatio(split: 2, ratio: 0.4))["type"] as? String, "setSplitRatio")
    }

    func testSplitTreeChangedDecodes() throws {
        let json = #"{"type":"splitTreeChanged","agent":1,"tree":{"kind":"split","id":1,"axis":"horizontal","ratio":0.5,"first":{"kind":"leaf","pane":1},"second":{"kind":"leaf","pane":2}}}"#
        let e = try JSONDecoder().decode(ControlEvent.self, from: Data(json.utf8))
        guard case let .splitTreeChanged(agent, tree) = e else { return XCTFail("expected splitTreeChanged") }
        XCTAssertEqual(agent, 1)
        XCTAssertEqual(tree.leaves, [1, 2])
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter SplitControlMessagesTests`
Expected: FAIL — the cases don't exist.

- [ ] **Step 3: Extend `ControlRequest`** in `Models.swift` — add the cases and encoder arms, and widen `CodingKeys`:

```swift
    case splitPane(pane: UInt64, direction: SplitDirection)
    case closePane(pane: UInt64)
    case setSplitRatio(split: UInt64, ratio: Double)
    case getSplitTree(agent: UInt64)
```
CodingKeys becomes: `case type, project, task, adapter, pane, direction, split, ratio, agent`.
Add to `encode(to:)`:
```swift
        case let .splitPane(pane, direction):
            try c.encode("splitPane", forKey: .type)
            try c.encode(pane, forKey: .pane)
            try c.encode(direction, forKey: .direction)
        case let .closePane(pane):
            try c.encode("closePane", forKey: .type)
            try c.encode(pane, forKey: .pane)
        case let .setSplitRatio(split, ratio):
            try c.encode("setSplitRatio", forKey: .type)
            try c.encode(split, forKey: .split)
            try c.encode(ratio, forKey: .ratio)
        case let .getSplitTree(agent):
            try c.encode("getSplitTree", forKey: .type)
            try c.encode(agent, forKey: .agent)
```

- [ ] **Step 4: Extend `ControlEvent`** in `Models.swift` — add the case, widen `CodingKeys`, add the decode arm:

```swift
    case splitTreeChanged(agent: UInt64, tree: PaneTree)
```
CodingKeys adds `tree` and `agent` (it already has `pane`; add `case ... , agent, tree`).
In `init(from:)`:
```swift
        case "splitTreeChanged":
            self = .splitTreeChanged(
                agent: try c.decode(UInt64.self, forKey: .agent),
                tree: try c.decode(PaneTree.self, forKey: .tree))
```

- [ ] **Step 5: Run to verify it passes**

Run: `cd macos && swift test`
Expected: PASS — all prior + the 3 new.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/MuxyCore/Models.swift macos/Tests/MuxyCoreTests/SplitControlMessagesTests.swift
git commit -m "feat(core): split/close/ratio/get-tree requests + splitTreeChanged event"
```

---

## Task 3: `AgentStore` tree storage (MuxyCore)

**Files:**
- Modify: `macos/Sources/MuxyCore/AgentStore.swift`
- Test: `macos/Tests/MuxyCoreTests/AgentStoreTreesTests.swift`

**Interfaces:**
- Consumes: `PaneTree`, `ControlEvent.splitTreeChanged` (Tasks 1–2).
- Produces: `AgentStore.trees: [UInt64: PaneTree]`.

- [ ] **Step 1: Write the failing tests** (`AgentStoreTreesTests.swift`):

```swift
import XCTest
@testable import MuxyCore

final class AgentStoreTreesTests: XCTestCase {
    private func tree() throws -> PaneTree {
        try JSONDecoder().decode(PaneTree.self, from: Data(
            #"{"kind":"split","id":1,"axis":"horizontal","ratio":0.5,"first":{"kind":"leaf","pane":1},"second":{"kind":"leaf","pane":2}}"#.utf8))
    }

    func testSplitTreeChangedStoresTreeIdempotently() throws {
        let store = AgentStore()
        let t = try tree()
        store.apply(.splitTreeChanged(agent: 1, tree: t))
        XCTAssertEqual(store.trees[1], t)
        store.apply(.splitTreeChanged(agent: 1, tree: t))   // same event twice → one tree
        XCTAssertEqual(store.trees.count, 1)
        XCTAssertEqual(store.trees[1], t)
    }

    func testAgentRemovedClearsTree() throws {
        let store = AgentStore()
        store.apply(.splitTreeChanged(agent: 1, tree: try tree()))
        store.apply(.agentRemoved(pane: 1))
        XCTAssertNil(store.trees[1])
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter AgentStoreTreesTests`
Expected: FAIL — `trees` doesn't exist.

- [ ] **Step 3: Add `trees` + apply handling** in `AgentStore.swift`. Add the published property next to the others:

```swift
    @Published public private(set) var trees: [UInt64: PaneTree] = [:]
```
In `apply(_:)`, add a case:
```swift
        case let .splitTreeChanged(agent, tree):
            trees[agent] = tree        // idempotent replace (carry-forward #1)
```
And in the existing `.agentRemoved(pane)` case, also clear the tree:
```swift
        case let .agentRemoved(pane):
            agents[pane] = nil
            trees[pane] = nil          // no per-companion AgentRemoved; drop the agent's tree
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd macos && swift test`
Expected: PASS — all prior + the 2 new.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/MuxyCore/AgentStore.swift macos/Tests/MuxyCoreTests/AgentStoreTreesTests.swift
git commit -m "feat(core): AgentStore.trees — store split trees, clear on agentRemoved"
```

---

## Task 4: `AppModel` focus + actions + split commands (MuxyCore)

**Files:**
- Modify: `macos/Sources/MuxyCore/AppModel.swift` (focus/tree/actions + `run` cases + getSplitTree-on-select)
- Modify: `macos/Sources/MuxyCore/Keymap.swift` (`CommandID` cases, `Keymap.defaults`, `CommandRegistry.all`)
- Test: `macos/Tests/MuxyCoreTests/SplitNavigationTests.swift`

**Interfaces:**
- Consumes: `AgentStore.trees`, `ControlRequest` split cases, `PaneTree`, `CommandID`.
- Produces: `AppModel.focusedPane`, `currentTree`, `splitFocused`/`closeFocused`/`focusNextPane`; `CommandID.{splitRight, splitDown, closePane, focusNextPane}`; keymap defaults + registry rows.

- [ ] **Step 1: Write the failing tests** (`SplitNavigationTests.swift`) — reuse the test target's `FakeControlTransport` (with `deliver`):

```swift
import XCTest
@testable import MuxyCore

@MainActor
final class SplitNavigationTests: XCTestCase {
    private let treeJSON = #"{"type":"splitTreeChanged","agent":1,"tree":{"kind":"split","id":1,"axis":"horizontal","ratio":0.5,"first":{"kind":"leaf","pane":1},"second":{"kind":"split","id":2,"axis":"vertical","ratio":0.5,"first":{"kind":"leaf","pane":2},"second":{"kind":"leaf","pane":3}}}}"#

    private func liveModel() -> (AppModel, FakeControlTransport) {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        return (model, fake)
    }

    func testSelectingAgentRequestsTreeAndFocusesAgent() {
        let (model, fake) = liveModel()
        model.selectedPane = 1
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"getSplitTree\"") && $0.contains("\"agent\":1") })
        XCTAssertEqual(model.focusedPane, 1)
    }

    func testSplitFocusedSendsSplitPane() {
        let (model, fake) = liveModel()
        model.selectedPane = 1
        model.focusedPane = 1
        model.run(.splitRight)
        XCTAssertTrue(fake.sentLines.contains {
            $0.contains("\"type\":\"splitPane\"") && $0.contains("\"pane\":1") && $0.contains("\"direction\":\"right\"")
        })
    }

    func testFocusNextCyclesTreeLeaves() {
        let (model, fake) = liveModel()
        model.selectedPane = 1
        fake.deliver(treeJSON)              // trees[1] leaves = [1,2,3]
        model.focusedPane = 1
        model.focusNextPane(); XCTAssertEqual(model.focusedPane, 2)
        model.focusNextPane(); XCTAssertEqual(model.focusedPane, 3)
        model.focusNextPane(); XCTAssertEqual(model.focusedPane, 1)
    }

    func testCloseFocusedOnlyClosesCompanions() {
        let (model, fake) = liveModel()
        model.selectedPane = 1
        fake.deliver(treeJSON)
        model.focusedPane = 1               // the agent pane
        model.closeFocused()
        XCTAssertFalse(fake.sentLines.contains { $0.contains("\"type\":\"closePane\"") })
        model.focusedPane = 2               // a companion
        model.closeFocused()
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"closePane\"") && $0.contains("\"pane\":2") })
    }

    func testKeymapAndRegistryCarrySplitCommands() {
        let k = Keymap()
        XCTAssertEqual(k.binding(for: .splitRight), KeyBinding("d", .command))
        XCTAssertEqual(k.binding(for: .splitDown), KeyBinding("d", [.command, .shift]))
        XCTAssertEqual(k.binding(for: .closePane), KeyBinding("w", [.command, .shift]))
        XCTAssertEqual(k.binding(for: .focusNextPane), KeyBinding("]", .command))
        let ids = CommandRegistry.all(keymap: k).map(\.id)
        XCTAssertTrue([.splitRight, .splitDown, .closePane, .focusNextPane].allSatisfy { ids.contains($0) })
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter SplitNavigationTests`
Expected: FAIL — the members/cases don't exist.

- [ ] **Step 3: Add the `CommandID` cases + keymap + registry** in `Keymap.swift`. Extend `CommandID`:
```swift
    case splitRight
    case splitDown
    case closePane
    case focusNextPane
```
Add to `Keymap.defaults`:
```swift
        .splitRight:    KeyBinding("d", .command),
        .splitDown:     KeyBinding("d", [.command, .shift]),
        .closePane:     KeyBinding("w", [.command, .shift]),
        .focusNextPane: KeyBinding("]", .command),
```
Add to `CommandRegistry.all` (after the existing rows):
```swift
            Command(id: .splitRight, title: "Split Right", subtitle: "Split the focused pane rightward",
                    defaultShortcut: keymap.binding(for: .splitRight)),
            Command(id: .splitDown, title: "Split Down", subtitle: "Split the focused pane downward",
                    defaultShortcut: keymap.binding(for: .splitDown)),
            Command(id: .closePane, title: "Close Pane", subtitle: "Close the focused companion pane",
                    defaultShortcut: keymap.binding(for: .closePane)),
            Command(id: .focusNextPane, title: "Focus Next Pane", subtitle: "Move focus to the next pane",
                    defaultShortcut: keymap.binding(for: .focusNextPane)),
```

- [ ] **Step 4: Add focus/tree/actions to `AppModel.swift`.** Add the published focus and give `selectedPane` a `didSet` that fetches the tree:

```swift
    @Published public var selectedPane: UInt64? {
        didSet {
            focusedPane = selectedPane            // focus the agent pane on (re)select
            if let agent = selectedPane { try? session?.send(.getSplitTree(agent: agent)) }
        }
    }
    @Published public var focusedPane: UInt64?
```
(Replace the existing plain `@Published public var selectedPane: UInt64?` declaration with this one; keep it a stored `var`.)

Add the computed tree + actions:
```swift
    /// The selected agent's split tree, or nil (the detail falls back to a lone leaf).
    public var currentTree: PaneTree? { selectedPane.flatMap { store.trees[$0] } }

    public func splitFocused(_ direction: SplitDirection) {
        guard let target = focusedPane ?? selectedPane, session != nil else { return }
        try? session?.send(.splitPane(pane: target, direction: direction))
    }

    public func closeFocused() {
        // Only companions are closable here; closing the agent pane is teardown (out of scope).
        guard let f = focusedPane, f != selectedPane, session != nil else { return }
        try? session?.send(.closePane(pane: f))
        focusedPane = selectedPane            // optimistic: the leaf is going away
    }

    public func focusNextPane() {
        guard let leaves = currentTree?.leaves, !leaves.isEmpty else { return }
        if let f = focusedPane, let i = leaves.firstIndex(of: f) {
            focusedPane = leaves[(i + 1) % leaves.count]
        } else {
            focusedPane = leaves.first
        }
    }
```

Extend `run(_:)` with the new cases:
```swift
        case .splitRight: splitFocused(.right)
        case .splitDown: splitFocused(.down)
        case .closePane: closeFocused()
        case .focusNextPane: focusNextPane()
```

> `session` is the existing `private var session: ControlSession?`. These methods live on the `@MainActor` `AppModel` alongside the existing ones.

- [ ] **Step 5: Run to verify it passes**

Run: `cd macos && swift test`
Expected: PASS — all prior + `SplitNavigationTests`.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/MuxyCore/AppModel.swift macos/Sources/MuxyCore/Keymap.swift macos/Tests/MuxyCoreTests/SplitNavigationTests.swift
git commit -m "feat(core): split/close/focus actions + commands, fetch tree on select"
```

---

## Task 5: Recursive split render + focus + menu (MuxyApp)

Render the tree, focus a pane via native first responder, and add the split/close/focus menu items. Gate: `swift build`.

**Files:**
- Modify: `macos/Sources/MuxyApp/SurfaceView.swift` (add `onFocus`)
- Modify: `macos/Sources/MuxyApp/TerminalContainer.swift` (pass `onFocus`)
- Create: `macos/Sources/MuxyApp/SplitContainer.swift`
- Modify: `macos/Sources/MuxyApp/ContentView.swift` (detail → `SplitContainer`)
- Modify: `macos/Sources/MuxyApp/App.swift` (menu items for the new commands)

**Interfaces:**
- Consumes: `AppModel` (`currentTree`, `focusedPane`, `run`), `PaneTree`, `Axis`, `SurfaceHost`, `TerminalContainer`.

- [ ] **Step 1: Add focus reporting to `SurfaceView.swift`.** Add a stored closure and fire it when the view becomes first responder (AppKit makes a clicked terminal the first responder natively):

```swift
    /// Called when this surface becomes first responder (e.g. the user clicks it).
    var onFocus: (() -> Void)?

    override func becomeFirstResponder() -> Bool {
        let ok = super.becomeFirstResponder()
        if ok { onFocus?() }
        return ok
    }
```

- [ ] **Step 2: Thread `onFocus` + `isFocused` through `TerminalContainer.swift`.** With multiple split leaves in one window, only the **focused** leaf should grab first responder — an unconditional `makeFirstResponder` in `makeNSView` (as M1a had it, correct for a single pane) makes N leaves race for focus. So drive first-responder from `isFocused` (which follows `focusedPane`), and report clicks via `onFocus`:

```swift
struct TerminalContainer: NSViewRepresentable {
    let pane: UInt64
    let surfaceHost: SurfaceHost
    var isFocused: Bool = false
    var onFocus: (() -> Void)? = nil

    func makeNSView(context: Context) -> SurfaceView {
        let view = surfaceHost.view(for: pane)
        view.onFocus = onFocus
        return view
    }

    func updateNSView(_ nsView: SurfaceView, context: Context) {
        nsView.onFocus = onFocus
        // Only the focused leaf claims first responder (native click-focus handles the rest).
        if isFocused, nsView.window?.firstResponder !== nsView {
            DispatchQueue.main.async { nsView.window?.makeFirstResponder(nsView) }
        }
    }
}
```

> This replaces the whole struct. Note `makeNSView` no longer force-focuses (that was the M1a single-pane behavior); focus now follows `isFocused`, so the initially-selected agent pane (which `AppModel` focuses on select) still gets first responder via `updateNSView`.

- [ ] **Step 3: Create `SplitContainer.swift`:**

```swift
import SwiftUI
import MuxyCore

/// Recursively renders a PaneTree: leaves become terminals, splits lay their two children
/// along the axis at the tree's ratio (fixed divider — dragging is M1c-3).
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
                )
        case let .split(_, axis, ratio, first, second):
            GeometryReader { geo in
                let horizontal = axis == .horizontal
                let total = horizontal ? geo.size.width : geo.size.height
                let firstLen = max(0, total * ratio - 0.5)
                if horizontal {
                    HStack(spacing: 0) {
                        SplitContainer(node: first, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                            .frame(width: firstLen)
                        Divider()
                        SplitContainer(node: second, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                    }
                } else {
                    VStack(spacing: 0) {
                        SplitContainer(node: first, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                            .frame(height: firstLen)
                        Divider()
                        SplitContainer(node: second, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 4: Render the tree in `ContentView.swift`.** In the `detail` view, replace the live-agent `TerminalContainer(pane: pane, surfaceHost: surfaceHost).id(pane)` branch with the split container (keep the `exitedPlaceholder` branch and the outer `if let pane … agent` shape):

```swift
            } else {
                SplitContainer(node: model.currentTree ?? .leaf(pane: pane),
                               surfaceHost: surfaceHost,
                               focusedPane: $model.focusedPane)
                    .id(pane)   // rebuild when switching agents; same agent's tree changes diff in place
            }
```

- [ ] **Step 5: Add the menu items in `App.swift`.** In the `CommandMenu("muxy")` block, after the existing items (and before/after the Switch-to-Agent group, your choice), add:

```swift
                Divider()
                menuItem("Split Right", .splitRight)
                menuItem("Split Down", .splitDown)
                menuItem("Close Pane", .closePane)
                menuItem("Focus Next Pane", .focusNextPane)
```

- [ ] **Step 6: Build**

Run: `cd macos && swift build`
Expected: builds clean on macOS 14. Also `cd macos && swift test` — MuxyCore suite still green.

- [ ] **Step 7: Manual smoke (recorded; user runs the full pass).** With a daemon + an agent: select it → its terminal renders. **⌘D** → a shell appears to the right (run `pwd` → the agent's worktree). **⌘⇧D** → a shell below. Click between panes and type → input goes to the clicked pane (and the focus ring follows). **⌘⇧W** on a companion → it closes and the layout collapses. Cmd-K palette lists Split/Close/Focus. Close + reopen the window → the split layout is restored (daemon-owned). Commands are no-ops with no agent selected.

- [ ] **Step 8: Commit**

```bash
git add macos/Sources/MuxyApp/SurfaceView.swift macos/Sources/MuxyApp/TerminalContainer.swift macos/Sources/MuxyApp/SplitContainer.swift macos/Sources/MuxyApp/ContentView.swift macos/Sources/MuxyApp/App.swift
git commit -m "feat(app): recursive split render, click-to-focus, split/close/focus commands"
```

---

## Final verification

- `cd macos && swift test` → existing 47 + `PaneTreeTests` + `SplitControlMessagesTests` + `AgentStoreTreesTests` + `SplitNavigationTests`, all green.
- `cd macos && swift build` → clean on macOS 14.
- Manual (user): split right/down spawns companion shells in the worktree, click-to-focus + ⌘⇧W close work, the palette lists the split commands, and the layout survives window close/reopen. Draggable dividers remain for M1c-3.
