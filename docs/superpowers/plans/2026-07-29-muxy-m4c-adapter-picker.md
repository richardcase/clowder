# muxy M4c — Client Adapter Picker

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consume M4b's `ListAdapters`/`AdapterList` in the client — request the adapter list
on connect, hold it in the store, and replace the SpawnSheet's free-text Adapter field with a
**`Picker`** — so spawning offers claude/codex/shell instead of typed strings. Completes M4.

**Architecture:** `MuxyCore` gains a Swift `AdapterInfo`, `ControlRequest.listAdapters`,
`ControlEvent.adapterList` decode, and `AgentStore.adapters` (set by `apply(.adapterList)`,
matching how `agents`/`trees` are handled — inbound events route through `ControlSession →
store.apply`). `AppModel` sends `.listAdapters` on connect and exposes `adapters`. `MuxyApp`'s
SpawnSheet becomes a `Picker` fed by `model.adapters`.

**Tech Stack:** Swift 6 (v5 mode, macOS 14), SwiftUI, Combine; `MuxyCore` (Foundation/Combine
only), `MuxyApp`. Spec: `docs/superpowers/specs/2026-07-29-muxy-m4-codex-adapter-design.md` (§Client).

## Global Constraints

- Wire compatibility with M4b (already on main): request `{"type":"listAdapters"}`; event
  `{"type":"adapterList","adapters":[{"id":"codex","displayName":"OpenAI Codex"},...]}`. The
  Swift `AdapterInfo` uses property names `id`/`displayName` so default `Codable` matches the
  camelCase wire keys directly (no `CodingKeys` needed).
- `MuxyCore` stays Foundation/Combine only (no SwiftUI/AppKit). Adapter state lives in
  `AgentStore` (like `agents`/`trees`/`lastError`), because `ControlSession` applies inbound
  events to the store; `AppModel` forwards it. (The spec says "AppModel.adapters" — implemented
  as `AgentStore.adapters` + an `AppModel.adapters` forwarder, honoring the event-routing design.)
- Default the store's adapters to a single `claude` entry so the picker is never empty before
  the daemon's reply arrives.
- The spawn request is unchanged (still `spawnAgent(project, task, adapter)` with the chosen id
  string) — only how the adapter is chosen changes.
- Tests: `cd macos && swift test`. App gate: `cd macos && swift build`.

---

## Task 1: `AdapterInfo` + request/event + store + connect (MuxyCore)

**Files:**
- Modify: `macos/Sources/MuxyCore/Models.swift` (`AdapterInfo`, `ControlRequest.listAdapters`, `ControlEvent.adapterList`)
- Modify: `macos/Sources/MuxyCore/AgentStore.swift` (`adapters` + apply arm)
- Modify: `macos/Sources/MuxyCore/AppModel.swift` (send on connect + forwarder)
- Test: `macos/Tests/MuxyCoreTests/AdapterPickerTests.swift`

**Interfaces:**
- Produces: `AdapterInfo { id: String, displayName: String }`; `ControlRequest.listAdapters`;
  `ControlEvent.adapterList([AdapterInfo])`; `AgentStore.adapters`; `AppModel.adapters`.

- [ ] **Step 1: Write the failing tests** (`AdapterPickerTests.swift`) — reuse the test target's
`FakeControlTransport` (as `LifecycleTests` does):

```swift
import XCTest
@testable import MuxyCore

@MainActor
final class AdapterPickerTests: XCTestCase {
    func testListAdaptersEncodes() throws {
        let data = try JSONEncoder().encode(ControlRequest.listAdapters)
        let o = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        XCTAssertEqual(o["type"] as? String, "listAdapters")
    }

    func testAdapterListDecodes() throws {
        let json = #"{"type":"adapterList","adapters":[{"id":"codex","displayName":"OpenAI Codex"}]}"#
        let ev = try JSONDecoder().decode(ControlEvent.self, from: Data(json.utf8))
        XCTAssertEqual(ev, .adapterList([AdapterInfo(id: "codex", displayName: "OpenAI Codex")]))
    }

    func testStoreApplyAdapterListSetsAdapters() {
        let store = AgentStore()
        store.apply(.adapterList([AdapterInfo(id: "codex", displayName: "OpenAI Codex")]))
        XCTAssertEqual(store.adapters, [AdapterInfo(id: "codex", displayName: "OpenAI Codex")])
    }

    func testConnectRequestsAdapters() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"listAdapters\"") },
                      "connect must request the adapter list")
    }

    func testAppModelForwardsStoreAdapters() {
        let store = AgentStore()
        let model = AppModel(store: store, makeTransport: { FakeControlTransport() })
        store.apply(.adapterList([AdapterInfo(id: "shell", displayName: "Shell")]))
        XCTAssertEqual(model.adapters, [AdapterInfo(id: "shell", displayName: "Shell")])
    }
}
```
(If `FakeControlTransport`'s real API differs — property/method names for "deliver a line" or
"sent lines", or the `AppModel` init — adapt to the real helper as `LifecycleTests` uses it;
keep each assertion's intent identical.)

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter AdapterPickerTests`
Expected: FAIL — the types/members don't exist.

- [ ] **Step 3: Add `AdapterInfo`** in `Models.swift` (next to `AgentInfo`):
```swift
public struct AdapterInfo: Codable, Identifiable, Equatable, Sendable {
    public let id: String
    public let displayName: String
    public init(id: String, displayName: String) {
        self.id = id
        self.displayName = displayName
    }
}
```

- [ ] **Step 4: Add the request** in `ControlRequest` (`Models.swift`):
```swift
    case listAdapters
```
and its encode arm (alongside `.listAgents`):
```swift
        case .listAdapters:
            try c.encode("listAdapters", forKey: .type)
```

- [ ] **Step 5: Add the event** in `ControlEvent` (`Models.swift`):
```swift
    case adapterList([AdapterInfo])
```
add `adapters` to that enum's `CodingKeys` (the `ControlEvent` `CodingKeys` currently lacks it):
```swift
    private enum CodingKeys: String, CodingKey { case type, agents, adapters, pane, state, message, agent, tree }
```
and the decode arm (alongside `"agentList"`):
```swift
        case "adapterList":
            self = .adapterList(try c.decode([AdapterInfo].self, forKey: .adapters))
```

- [ ] **Step 6: Add store state** in `AgentStore.swift`:
```swift
    @Published public private(set) var adapters: [AdapterInfo] = [AdapterInfo(id: "claude", displayName: "Claude Code")]
```
and an apply arm in `apply(_:)`:
```swift
        case let .adapterList(list):
            adapters = list
```

- [ ] **Step 7: Request on connect + forward** in `AppModel.swift`. In `connect()`, right after
`try session.send(.listAgents)`:
```swift
            try session.send(.listAdapters)
```
and add a forwarder (near the other computed accessors):
```swift
    public var adapters: [AdapterInfo] { store.adapters }
```

- [ ] **Step 8: Run to verify it passes**

Run: `cd macos && swift test`
Expected: PASS — `AdapterPickerTests` + all existing.

- [ ] **Step 9: Commit**

```bash
git add macos/Sources/MuxyCore/Models.swift macos/Sources/MuxyCore/AgentStore.swift macos/Sources/MuxyCore/AppModel.swift macos/Tests/MuxyCoreTests/AdapterPickerTests.swift
git commit -m "feat(core): adapter list request/event + store + connect-time request"
```

---

## Task 2: SpawnSheet Picker (MuxyApp)

Gate: `swift build` (+ `swift test` stays green).

**Files:**
- Modify: `macos/Sources/MuxyApp/SpawnSheet.swift` (free-text → Picker + `adapters` param)
- Modify: `macos/Sources/MuxyApp/ContentView.swift` (pass `model.adapters`)

**Interfaces:**
- Consumes: `AppModel.adapters` (`[AdapterInfo]`).

- [ ] **Step 1: Give SpawnSheet the adapter list + a Picker.** Add a stored `adapters` property
and replace the `TextField("Adapter", …)` with a `Picker`:
```swift
    let adapters: [AdapterInfo]
    let onSpawn: (String, String, String) -> Void
    // ... existing @State project/task ...
    @State private var adapter = "claude"
```
Replace the adapter `TextField` line in the `Form` with:
```swift
                Picker("Adapter", selection: $adapter) {
                    ForEach(adapters) { a in
                        Text(a.displayName).tag(a.id)
                    }
                }
```
The Spawn button still passes `adapter` (already a valid id — drop the now-moot
`a.isEmpty ? "claude"` fallback; pass `adapter` directly). `SpawnSheet` uses `MuxyCore` types,
so ensure `import MuxyCore` is present (add if missing).

- [ ] **Step 2: Thread `adapters` from ContentView.** At the `SpawnSheet { … }` call site
(inside the `.sheet`), pass the list:
```swift
            SpawnSheet(adapters: model.adapters) { project, task, adapter in
                model.spawn(project: project, task: task, adapter: adapter)
            }
```

- [ ] **Step 3: Build + test**

Run: `cd macos && swift build` then `cd macos && swift test`
Expected: builds clean; MuxyCore suite green.

- [ ] **Step 4: Manual smoke (recorded; user runs it).** With the daemon running: open the
spawn sheet (⌘N) → the Adapter field is now a dropdown listing **Claude Code / OpenAI Codex /
Shell** (populated from the daemon on connect); pick **OpenAI Codex**, spawn → a `codex` agent
launches. The list reflects whatever `list_adapters` returns.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/MuxyApp/SpawnSheet.swift macos/Sources/MuxyApp/ContentView.swift
git commit -m "feat(app): SpawnSheet adapter Picker fed by the daemon's adapter list"
```

---

## Final verification

- `cd macos && swift test` → existing + `AdapterPickerTests`, all green.
- `cd macos && swift build` → clean on macOS 14.
- Manual (user): the spawn sheet's Adapter control is a dropdown of claude/codex/shell sourced
  from the daemon; selecting Codex spawns a `codex` agent. This completes M4 — Codex is
  spawnable end-to-end from a discoverable picker.
