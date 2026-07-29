# muxy M3b — Client Lifecycle UX (Land / Discard) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface the M3a daemon lifecycle in the app — **Land** and **Discard** the selected agent from the palette + menu, each behind a **confirmation** (Discard/Land irreversibly change repo state), so a finished agent's work gets integrated or thrown away from the UI.

**Architecture:** MuxyCore gains the `landAgent`/`discardAgent` control requests, the two `CommandID`s, and an `AppModel` confirmation flow (`run` → sets a pending confirmation → `confirmLifecycle` sends the request). MuxyApp adds the menu entries + a SwiftUI confirmation dialog. On success the daemon's `AgentRemoved` drops the agent from the store (already handled).

**Tech Stack:** Swift 6 (v5 mode, macOS 14), SwiftUI + AppKit, Combine; MuxyCore (Foundation/Combine only).

## Global Constraints

- **Every destructive/lifecycle action confirms.** `run(.landAgent)`/`run(.discardAgent)` do NOT send immediately — they set a pending confirmation; only `confirmLifecycle()` sends. (This is the M3a-review carry-forward: Discard irreversibly deletes an unmerged branch; Land finalizes + removes the agent.)
- **MuxyCore stays libghostty- and SwiftUI-free.**
- No new close-agent path: Land/Discard are the agent-removal actions (both confirmed); the existing `closeFocused` still refuses to close the agent pane, so no unconfirmed destructive path exists.
- Commit after each task; conventional messages + standard trailers.

**Test commands:** Core: `cd macos && swift test`. App build gate: `cd macos && swift build`.

---

## Task 1: Control requests + commands + confirmation flow (MuxyCore)

**Files:**
- Modify: `macos/Sources/MuxyCore/Models.swift` (`ControlRequest` cases)
- Modify: `macos/Sources/MuxyCore/Keymap.swift` (`CommandID` + defaults + registry)
- Modify: `macos/Sources/MuxyCore/AppModel.swift` (`LifecycleAction`/`PendingLifecycle` + `run` + confirm/cancel)
- Test: `macos/Tests/MuxyCoreTests/LifecycleTests.swift`

**Interfaces:**
- Produces: `ControlRequest.{landAgent, discardAgent}`; `CommandID.{landAgent, discardAgent}`; `AppModel.pendingLifecycle`, `requestLifecycle(_:)`, `confirmLifecycle()`, `cancelLifecycle()`, `LifecycleAction`, `PendingLifecycle`.

- [ ] **Step 1: Write the failing tests** (`LifecycleTests.swift`) — reuse the test target's `FakeControlTransport`:

```swift
import XCTest
@testable import MuxyCore

@MainActor
final class LifecycleTests: XCTestCase {
    private func modelWithAgent() -> (AppModel, FakeControlTransport) {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        fake.deliver(#"{"type":"agentList","agents":[{"pane":1,"project":"/p","task":"fix-bug","state":"Completed"}]}"#)
        model.selectedPane = 1
        return (model, fake)
    }

    func testLandEncodes() throws {
        let o = try JSONSerialization.jsonObject(with: JSONEncoder().encode(ControlRequest.landAgent(pane: 3))) as! [String: Any]
        XCTAssertEqual(o["type"] as? String, "landAgent")
        XCTAssertEqual(o["pane"] as? Int, 3)
        let d = try JSONSerialization.jsonObject(with: JSONEncoder().encode(ControlRequest.discardAgent(pane: 4))) as! [String: Any]
        XCTAssertEqual(d["type"] as? String, "discardAgent")
        XCTAssertEqual(d["pane"] as? Int, 4)
    }

    func testRunLandSetsPendingConfirmationAndDoesNotSend() {
        let (model, fake) = modelWithAgent()
        model.run(.landAgent)
        XCTAssertEqual(model.pendingLifecycle, PendingLifecycle(action: .land, pane: 1, task: "fix-bug"))
        XCTAssertFalse(fake.sentLines.contains { $0.contains("\"type\":\"landAgent\"") }, "must not send before confirm")
    }

    func testConfirmSendsLandThenClears() {
        let (model, fake) = modelWithAgent()
        model.run(.landAgent)
        model.confirmLifecycle()
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"landAgent\"") && $0.contains("\"pane\":1") })
        XCTAssertNil(model.pendingLifecycle)
    }

    func testDiscardFlow() {
        let (model, fake) = modelWithAgent()
        model.run(.discardAgent)
        XCTAssertEqual(model.pendingLifecycle?.action, .discard)
        model.confirmLifecycle()
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"discardAgent\"") && $0.contains("\"pane\":1") })
    }

    func testCancelClearsWithoutSending() {
        let (model, fake) = modelWithAgent()
        model.run(.discardAgent)
        model.cancelLifecycle()
        XCTAssertNil(model.pendingLifecycle)
        XCTAssertFalse(fake.sentLines.contains { $0.contains("discardAgent") })
    }

    func testNoSelectionNoPending() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()   // selectedPane is nil
        model.run(.landAgent)
        XCTAssertNil(model.pendingLifecycle)
    }

    func testKeymapAndRegistryCarryLifecycle() {
        XCTAssertEqual(Keymap().binding(for: .landAgent), KeyBinding("l", .command))
        let ids = CommandRegistry.all(keymap: Keymap()).map(\.id)
        XCTAssertTrue(ids.contains(.landAgent) && ids.contains(.discardAgent))
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter LifecycleTests`
Expected: FAIL — the cases/members don't exist.

- [ ] **Step 3: Add the `ControlRequest` cases** in `Models.swift`:
```swift
    case landAgent(pane: UInt64)
    case discardAgent(pane: UInt64)
```
Add to `encode(to:)` (the `CodingKeys` already has `pane`):
```swift
        case let .landAgent(pane):
            try c.encode("landAgent", forKey: .type)
            try c.encode(pane, forKey: .pane)
        case let .discardAgent(pane):
            try c.encode("discardAgent", forKey: .type)
            try c.encode(pane, forKey: .pane)
```

- [ ] **Step 4: Add the `CommandID`s + keymap + registry** in `Keymap.swift`. Extend `CommandID`:
```swift
    case landAgent
    case discardAgent
```
Add to `Keymap.defaults` (Land gets ⌘L; **Discard gets no default binding** — it's destructive, so palette/menu-only, no hotkey):
```swift
        .landAgent: KeyBinding("l", .command),
```
Add to `CommandRegistry.all`:
```swift
            Command(id: .landAgent, title: "Land Agent",
                    subtitle: "Finalize the selected agent's work onto its branch",
                    defaultShortcut: keymap.binding(for: .landAgent)),
            Command(id: .discardAgent, title: "Discard Agent",
                    subtitle: "Throw away the selected agent's work + delete its branch",
                    defaultShortcut: keymap.binding(for: .discardAgent)),   // nil — no hotkey
```

- [ ] **Step 5: Add the confirmation flow to `AppModel.swift`.** Add the types + published state + methods, and the `run` cases:
```swift
    public enum LifecycleAction: Equatable, Sendable { case land, discard }
    public struct PendingLifecycle: Equatable, Sendable {
        public let action: LifecycleAction
        public let pane: UInt64
        public let task: String
    }
    @Published public var pendingLifecycle: PendingLifecycle?

    /// Begin a Land/Discard: capture the selected agent + task and await confirmation.
    public func requestLifecycle(_ action: LifecycleAction) {
        guard let pane = selectedPane, let agent = store.agents[pane] else { return }
        pendingLifecycle = PendingLifecycle(action: action, pane: pane, task: agent.task)
    }
    /// Confirmed: send the request and clear.
    public func confirmLifecycle() {
        guard let p = pendingLifecycle else { return }
        switch p.action {
        case .land: try? session?.send(.landAgent(pane: p.pane))
        case .discard: try? session?.send(.discardAgent(pane: p.pane))
        }
        pendingLifecycle = nil
    }
    public func cancelLifecycle() { pendingLifecycle = nil }
```
Extend `run(_:)`:
```swift
        case .landAgent: requestLifecycle(.land)
        case .discardAgent: requestLifecycle(.discard)
```

- [ ] **Step 6: Run to verify it passes**

Run: `cd macos && swift test`
Expected: PASS — existing + `LifecycleTests`. (Adding `CommandRegistry` rows may shift a hardcoded count in `KeymapTests`/`PaletteSearchTests` — if so, update those assertions to the new set, don't weaken them.)

- [ ] **Step 7: Commit**

```bash
git add macos/Sources/MuxyCore/Models.swift macos/Sources/MuxyCore/Keymap.swift macos/Sources/MuxyCore/AppModel.swift macos/Tests/MuxyCoreTests/LifecycleTests.swift
git commit -m "feat(core): land/discard requests + commands + confirmation flow"
```

---

## Task 2: Menu + confirmation dialog (MuxyApp)

Gate: `swift build`.

**Files:**
- Modify: `macos/Sources/MuxyApp/App.swift` (menu items)
- Modify: `macos/Sources/MuxyApp/ContentView.swift` (confirmation dialog)

**Interfaces:**
- Consumes: `AppModel.run(.landAgent/.discardAgent)`, `pendingLifecycle`, `confirmLifecycle`/`cancelLifecycle`.

- [ ] **Step 1: Add the menu entries** in `App.swift`'s `CommandMenu("muxy")`, after the split items. Land uses `menuItem` (⌘L); Discard is a plain `Button` with no shortcut (destructive — no hotkey) and a `…` to signal a dialog:
```swift
                Divider()
                menuItem("Land Agent", .landAgent)
                Button("Discard Agent…") { delegate.appModel?.run(.discardAgent) }
```

- [ ] **Step 2: Add the confirmation dialog** in `ContentView.swift`. Add a computed title and a `.confirmationDialog` on the `NavigationSplitView` (after the existing `.sheet`/`.overlay`s):
```swift
        .confirmationDialog(
            lifecycleTitle,
            isPresented: Binding(
                get: { model.pendingLifecycle != nil },
                set: { if !$0 { model.cancelLifecycle() } }
            ),
            presenting: model.pendingLifecycle
        ) { pending in
            Button(pending.action == .discard ? "Discard" : "Land",
                   role: pending.action == .discard ? .destructive : nil) {
                model.confirmLifecycle()
            }
            Button("Cancel", role: .cancel) { model.cancelLifecycle() }
        } message: { pending in
            Text(pending.action == .discard
                 ? "Deletes branch muxy/\(pending.task) and its work. This can't be undone."
                 : "Finalizes the work onto branch muxy/\(pending.task) and removes the agent.")
        }
```
Add the computed title to `ContentView`:
```swift
    private var lifecycleTitle: String {
        switch model.pendingLifecycle?.action {
        case .discard: return "Discard this agent?"
        case .land: return "Land this agent?"
        case nil: return ""
        }
    }
```

- [ ] **Step 3: Build + test**

Run: `cd macos && swift build` then `cd macos && swift test`
Expected: builds clean; MuxyCore suite green.

- [ ] **Step 4: Manual smoke (recorded; user runs the full pass).** With a daemon + a finished (git) agent selected: **⌘L** (or Cmd-K → "Land Agent") → a confirmation dialog appears; confirm Land → the agent leaves the sidebar, and the repo has a clean `muxy/<task>` branch with the work committed. Menu → **Discard Agent…** (or palette) → destructive confirmation; confirm → the agent leaves the sidebar and the `muxy/<task>` branch is gone. Cancel in either dialog → nothing happens. Land/Discard with no agent selected → no dialog.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/MuxyApp/App.swift macos/Sources/MuxyApp/ContentView.swift
git commit -m "feat(app): Land/Discard menu + palette commands with confirmation"
```

---

## Final verification

- `cd macos && swift test` → existing + `LifecycleTests`, all green.
- `cd macos && swift build` → clean on macOS 14.
- Manual (user): Land finalizes a finished agent (branch kept, agent gone), Discard throws it away (branch deleted, agent gone), each behind a confirmation; both reachable from the palette and menu (Land also ⌘L). The jj driver + auto-detect is M3c.
