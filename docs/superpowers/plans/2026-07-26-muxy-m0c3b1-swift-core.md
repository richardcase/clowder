# muxy M0c-3b1 — Swift Testable Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the daemon-facing logic of the macOS client as a **fully `swift test`-verifiable** SwiftPM library `MuxyCore`: Codable models mirroring the JSON control feed, an `AgentStore` implementing the refresh-driven contract, and a `ControlSession` over an abstracted transport. No UI, no libghostty, no real socket — those are M0c-3b2.

**Architecture:** A SwiftPM package at `macos/` (Swift 6.3 / Command Line Tools; `.v5` language mode to avoid Swift-6 strict-concurrency churn). `MuxyCore` library + `MuxyCoreTests` (XCTest). `ControlEvent`/`ControlRequest` use custom `Codable` for the internally-tagged `{"type":…}` JSON; `AgentStore.apply` encodes the refresh-driven rules; `ControlSession` wires an inbound line stream → decode → `store.apply` and auto-sends `listAgents` when the store needs a refresh, all behind a `ControlTransport` protocol tested with a fake.

**Tech Stack:** Swift 6.3 (via CLT), SwiftPM, Foundation, Combine (ObservableObject), XCTest.

## Global Constraints

- **Swift is on PATH** (Swift 6.3). Tests run with `cd macos && swift test` — this is the M0c-3b1 equivalent of `cargo test`. (The Rust `cargo test` is unaffected — `macos/` is a separate build system beside `crates/`.)
- **`macos/` SwiftPM package**, `// swift-tools-version:6.0`, `swiftLanguageModes: [.v5]`. One library target `MuxyCore` (`macos/Sources/MuxyCore/`), one test target `MuxyCoreTests` (`macos/Tests/MuxyCoreTests/`).
- **JSON is a contract with the daemon (M0c-3a):** internally tagged (`type` discriminator, camelCase tag values like `"spawnAgent"`); `pane` is a bare number (`UInt64`); `AttentionState` values are PascalCase strings (`"Working"`, `"NeedsInput"`, `"Exited"`).
- **No UI / no libghostty / no real socket in M0c-3b1** — those are M0c-3b2. This milestone is 100% `swift test`-verifiable.
- **Explicit `git add`** of changed files; never `git add .`. (SwiftPM creates `macos/.build/` — do NOT commit it; add a `macos/.gitignore` with `.build/` in Task 1.)

---

### Task 1: SwiftPM package + Codable models

**Files:**
- Create: `macos/Package.swift`
- Create: `macos/.gitignore`
- Create: `macos/Sources/MuxyCore/Models.swift`
- Create: `macos/Tests/MuxyCoreTests/ModelsTests.swift`

**Interfaces:**
- Produces: `AttentionState`, `AgentInfo`, `ControlRequest` (Encodable), `ControlEvent` (Decodable).

- [ ] **Step 1: Create `macos/Package.swift`**

```swift
// swift-tools-version:6.0
import PackageDescription

let package = Package(
    name: "MuxyCore",
    products: [
        .library(name: "MuxyCore", targets: ["MuxyCore"]),
    ],
    targets: [
        .target(name: "MuxyCore"),
        .testTarget(name: "MuxyCoreTests", dependencies: ["MuxyCore"]),
    ],
    swiftLanguageModes: [.v5]
)
```

- [ ] **Step 2: Create `macos/.gitignore`**

```
.build/
```

- [ ] **Step 3: Create `macos/Sources/MuxyCore/Models.swift`**

```swift
import Foundation

/// Mirrors the Rust `AttentionState` (serialized as its PascalCase variant name).
public enum AttentionState: String, Codable, Equatable, Sendable {
    case idle = "Idle"
    case working = "Working"
    case needsInput = "NeedsInput"
    case completed = "Completed"
    case exited = "Exited"
}

/// Mirrors the Rust `AgentInfo` (`pane` is a bare number).
public struct AgentInfo: Codable, Identifiable, Equatable, Sendable {
    public let pane: UInt64
    public let project: String
    public let task: String
    public var state: AttentionState
    public var id: UInt64 { pane }

    public init(pane: UInt64, project: String, task: String, state: AttentionState) {
        self.pane = pane
        self.project = project
        self.task = task
        self.state = state
    }
}

/// GUI/CLI → daemon. Custom `Encodable` for the internally-tagged JSON shape.
public enum ControlRequest: Encodable, Equatable, Sendable {
    case listAgents
    case spawnAgent(project: String, task: String, adapter: String)

    private enum CodingKeys: String, CodingKey { case type, project, task, adapter }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .listAgents:
            try c.encode("listAgents", forKey: .type)
        case let .spawnAgent(project, task, adapter):
            try c.encode("spawnAgent", forKey: .type)
            try c.encode(project, forKey: .project)
            try c.encode(task, forKey: .task)
            try c.encode(adapter, forKey: .adapter)
        }
    }
}

/// daemon → GUI/CLI. Custom `Decodable` discriminating on `type`.
public enum ControlEvent: Decodable, Equatable, Sendable {
    case agentList([AgentInfo])
    case attentionChanged(pane: UInt64, state: AttentionState)
    case agentRemoved(pane: UInt64)
    case agentSpawned(pane: UInt64)
    case error(message: String)

    private enum CodingKeys: String, CodingKey { case type, agents, pane, state, message }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)
        switch type {
        case "agentList":
            self = .agentList(try c.decode([AgentInfo].self, forKey: .agents))
        case "attentionChanged":
            self = .attentionChanged(
                pane: try c.decode(UInt64.self, forKey: .pane),
                state: try c.decode(AttentionState.self, forKey: .state))
        case "agentRemoved":
            self = .agentRemoved(pane: try c.decode(UInt64.self, forKey: .pane))
        case "agentSpawned":
            self = .agentSpawned(pane: try c.decode(UInt64.self, forKey: .pane))
        case "error":
            self = .error(message: try c.decode(String.self, forKey: .message))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .type, in: c, debugDescription: "unknown control event type: \(type)")
        }
    }
}
```

- [ ] **Step 4: Create `macos/Tests/MuxyCoreTests/ModelsTests.swift`**

```swift
import XCTest
@testable import MuxyCore

final class ModelsTests: XCTestCase {
    func testDecodeAgentList() throws {
        let json = #"{"type":"agentList","agents":[{"pane":2,"project":"muxy","task":"t","state":"NeedsInput"}]}"#
        let ev = try JSONDecoder().decode(ControlEvent.self, from: Data(json.utf8))
        XCTAssertEqual(ev, .agentList([AgentInfo(pane: 2, project: "muxy", task: "t", state: .needsInput)]))
    }

    func testDecodeAttentionChangedExited() throws {
        let json = #"{"type":"attentionChanged","pane":5,"state":"Exited"}"#
        let ev = try JSONDecoder().decode(ControlEvent.self, from: Data(json.utf8))
        XCTAssertEqual(ev, .attentionChanged(pane: 5, state: .exited))
    }

    func testDecodeRemovedAndSpawned() throws {
        let removed = try JSONDecoder().decode(ControlEvent.self, from: Data(#"{"type":"agentRemoved","pane":9}"#.utf8))
        XCTAssertEqual(removed, .agentRemoved(pane: 9))
        let spawned = try JSONDecoder().decode(ControlEvent.self, from: Data(#"{"type":"agentSpawned","pane":3}"#.utf8))
        XCTAssertEqual(spawned, .agentSpawned(pane: 3))
    }

    func testEncodeListAgentsRequest() throws {
        let data = try JSONEncoder().encode(ControlRequest.listAgents)
        XCTAssertEqual(String(decoding: data, as: UTF8.self), #"{"type":"listAgents"}"#)
    }

    func testEncodeSpawnAgentRequest() throws {
        let data = try JSONEncoder().encode(ControlRequest.spawnAgent(project: "/p", task: "t", adapter: "shell"))
        let s = String(decoding: data, as: UTF8.self)
        XCTAssertTrue(s.contains(#""type":"spawnAgent""#), s)
        XCTAssertTrue(s.contains(#""adapter":"shell""#), s)
    }

    func testUnknownEventTypeThrows() {
        XCTAssertThrowsError(
            try JSONDecoder().decode(ControlEvent.self, from: Data(#"{"type":"bogus"}"#.utf8)))
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cd macos && swift test`
Expected: PASS (6 tests). (Also confirm the Rust side is unaffected: `source "$HOME/.cargo/env" && cargo test` from the repo root still 39/39.)

- [ ] **Step 6: Commit**

```bash
git add macos/Package.swift macos/.gitignore macos/Sources/MuxyCore/Models.swift macos/Tests/MuxyCoreTests/ModelsTests.swift
git commit -m "feat(macos): MuxyCore SwiftPM package + Codable control models"
```

---

### Task 2: `AgentStore` — the refresh-driven model

**Files:**
- Create: `macos/Sources/MuxyCore/AgentStore.swift`
- Create: `macos/Tests/MuxyCoreTests/AgentStoreTests.swift`

**Interfaces:**
- Consumes: `AgentInfo`, `ControlEvent`, `AttentionState`.
- Produces: `final class AgentStore: ObservableObject` with `agents: [UInt64: AgentInfo]` (published, private-set), `needsRefresh: Bool` (published, private-set), `apply(_ event: ControlEvent)`, `clearRefresh()`, and `byProject: [(project: String, agents: [AgentInfo])]`.

- [ ] **Step 1: Create `macos/Sources/MuxyCore/AgentStore.swift`**

```swift
import Foundation
import Combine

/// The client-side agent model. Refresh-driven: events that can't fully hydrate a
/// row (a pane-only `agentSpawned`, or any event for an unknown pane) set `needsRefresh`,
/// which the session/UI answers with a `ControlRequest.listAgents`.
public final class AgentStore: ObservableObject {
    @Published public private(set) var agents: [UInt64: AgentInfo] = [:]
    @Published public private(set) var needsRefresh: Bool = false

    public init() {}

    public func apply(_ event: ControlEvent) {
        switch event {
        case let .agentList(list):
            agents = Dictionary(uniqueKeysWithValues: list.map { ($0.pane, $0) })
            needsRefresh = false
        case let .attentionChanged(pane, state):
            if var a = agents[pane] {
                a.state = state
                agents[pane] = a
            } else {
                needsRefresh = true // unknown pane — re-list to learn about it
            }
        case .agentSpawned:
            needsRefresh = true // pane-only — re-list to hydrate project/task/state
        case let .agentRemoved(pane):
            agents[pane] = nil // idempotent
        case .error:
            break
        }
    }

    public func clearRefresh() { needsRefresh = false }

    /// Agents grouped by project (projects sorted; agents within a project sorted by pane).
    public var byProject: [(project: String, agents: [AgentInfo])] {
        Dictionary(grouping: agents.values, by: { $0.project })
            .map { (project: $0.key, agents: $0.value.sorted { $0.pane < $1.pane }) }
            .sorted { $0.project < $1.project }
    }
}
```

- [ ] **Step 2: Create `macos/Tests/MuxyCoreTests/AgentStoreTests.swift`**

```swift
import XCTest
@testable import MuxyCore

final class AgentStoreTests: XCTestCase {
    func testAgentListReplacesAndClearsRefresh() {
        let s = AgentStore()
        s.apply(.agentSpawned(pane: 1))
        XCTAssertTrue(s.needsRefresh)
        s.apply(.agentList([AgentInfo(pane: 1, project: "p", task: "t", state: .working)]))
        XCTAssertFalse(s.needsRefresh)
        XCTAssertEqual(s.agents.count, 1)
        XCTAssertEqual(s.agents[1]?.task, "t")
    }

    func testAttentionChangedUpdatesKnownPane() {
        let s = AgentStore()
        s.apply(.agentList([AgentInfo(pane: 1, project: "p", task: "t", state: .working)]))
        s.apply(.attentionChanged(pane: 1, state: .needsInput))
        XCTAssertEqual(s.agents[1]?.state, .needsInput)
        XCTAssertFalse(s.needsRefresh)
    }

    func testAttentionChangedUnknownPaneTriggersRefresh() {
        let s = AgentStore()
        s.apply(.attentionChanged(pane: 99, state: .working))
        XCTAssertTrue(s.needsRefresh)
        XCTAssertNil(s.agents[99])
    }

    func testAgentRemovedIsIdempotent() {
        let s = AgentStore()
        s.apply(.agentList([AgentInfo(pane: 1, project: "p", task: "t", state: .working)]))
        s.apply(.agentRemoved(pane: 1))
        XCTAssertNil(s.agents[1])
        s.apply(.agentRemoved(pane: 1)) // no crash, still absent
        XCTAssertNil(s.agents[1])
    }

    func testByProjectGroupsAndSorts() {
        let s = AgentStore()
        s.apply(.agentList([
            AgentInfo(pane: 3, project: "b", task: "t", state: .working),
            AgentInfo(pane: 1, project: "a", task: "t", state: .working),
            AgentInfo(pane: 2, project: "a", task: "t", state: .working),
        ]))
        let bp = s.byProject
        XCTAssertEqual(bp.map { $0.project }, ["a", "b"])
        XCTAssertEqual(bp[0].agents.map { $0.pane }, [1, 2])
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cd macos && swift test`
Expected: PASS (Models 6 + AgentStore 5 = 11).

- [ ] **Step 4: Commit**

```bash
git add macos/Sources/MuxyCore/AgentStore.swift macos/Tests/MuxyCoreTests/AgentStoreTests.swift
git commit -m "feat(macos): AgentStore with the refresh-driven event model"
```

---

### Task 3: `ControlSession` + `ControlTransport` protocol

**Files:**
- Create: `macos/Sources/MuxyCore/ControlSession.swift`
- Create: `macos/Tests/MuxyCoreTests/ControlSessionTests.swift`

**Interfaces:**
- Consumes: `AgentStore`, `ControlEvent`, `ControlRequest`.
- Produces:
  - `protocol ControlTransport: AnyObject { func setReceiver(_ receiver: @escaping (String) -> Void); func send(line: String) throws }`
  - `final class ControlSession` — `init(transport:store:)`, `send(_ request: ControlRequest) throws`, `let store: AgentStore`. On each inbound line it decodes → `store.apply`, and if `store.needsRefresh` it clears the flag and sends `listAgents`.

- [ ] **Step 1: Create `macos/Sources/MuxyCore/ControlSession.swift`**

```swift
import Foundation

/// A source/sink of newline-delimited JSON control lines. The real Unix-socket
/// implementation is added in M0c-3b2 (it's only meaningful against a live daemon);
/// M0c-3b1 tests use a fake.
public protocol ControlTransport: AnyObject {
    /// Register a callback invoked once per inbound line (newline stripped).
    func setReceiver(_ receiver: @escaping (String) -> Void)
    /// Send one request line (the implementation appends the newline).
    func send(line: String) throws
}

/// Drives the control channel: inbound lines → decode → AgentStore; auto-refreshes
/// (sends `listAgents`) whenever the store can't fully hydrate from a streamed event.
public final class ControlSession {
    private let transport: ControlTransport
    public let store: AgentStore

    public init(transport: ControlTransport, store: AgentStore = AgentStore()) {
        self.transport = transport
        self.store = store
        transport.setReceiver { [weak self] line in self?.handle(line: line) }
    }

    private func handle(line: String) {
        let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              let event = try? JSONDecoder().decode(ControlEvent.self, from: Data(trimmed.utf8))
        else { return }
        store.apply(event)
        if store.needsRefresh {
            store.clearRefresh()
            try? send(.listAgents)
        }
    }

    public func send(_ request: ControlRequest) throws {
        let data = try JSONEncoder().encode(request)
        try transport.send(line: String(decoding: data, as: UTF8.self))
    }
}
```

- [ ] **Step 2: Create `macos/Tests/MuxyCoreTests/ControlSessionTests.swift`**

```swift
import XCTest
@testable import MuxyCore

/// In-memory transport: `feed` drives inbound lines; `sent` records outbound lines.
final class FakeTransport: ControlTransport {
    private var receiver: ((String) -> Void)?
    private(set) var sent: [String] = []
    func setReceiver(_ receiver: @escaping (String) -> Void) { self.receiver = receiver }
    func send(line: String) throws { sent.append(line) }
    func feed(_ line: String) { receiver?(line) }
}

final class ControlSessionTests: XCTestCase {
    func testInboundAgentListUpdatesStore() {
        let t = FakeTransport()
        let s = ControlSession(transport: t)
        t.feed(#"{"type":"agentList","agents":[{"pane":1,"project":"p","task":"t","state":"Working"}]}"#)
        XCTAssertEqual(s.store.agents[1]?.task, "t")
    }

    func testUnknownPaneEventTriggersListAgentsSend() {
        let t = FakeTransport()
        _ = ControlSession(transport: t)
        t.feed(#"{"type":"attentionChanged","pane":42,"state":"Working"}"#)
        XCTAssertEqual(t.sent, [#"{"type":"listAgents"}"#])
    }

    func testAgentSpawnedTriggersListAgentsSend() {
        let t = FakeTransport()
        _ = ControlSession(transport: t)
        t.feed(#"{"type":"agentSpawned","pane":7}"#)
        XCTAssertEqual(t.sent, [#"{"type":"listAgents"}"#])
    }

    func testSendSpawnAgentEncodesRequest() throws {
        let t = FakeTransport()
        let s = ControlSession(transport: t)
        try s.send(.spawnAgent(project: "/p", task: "demo", adapter: "shell"))
        XCTAssertEqual(t.sent.count, 1)
        XCTAssertTrue(t.sent[0].contains(#""type":"spawnAgent""#), t.sent[0])
    }

    func testMalformedLineIsIgnored() {
        let t = FakeTransport()
        let s = ControlSession(transport: t)
        t.feed("not json")
        t.feed("")
        XCTAssertTrue(s.store.agents.isEmpty)
        XCTAssertTrue(t.sent.isEmpty)
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cd macos && swift test`
Expected: PASS (Models 6 + AgentStore 5 + ControlSession 5 = 16).

- [ ] **Step 4: Commit**

```bash
git add macos/Sources/MuxyCore/ControlSession.swift macos/Tests/MuxyCoreTests/ControlSessionTests.swift
git commit -m "feat(macos): ControlSession over a testable ControlTransport"
```

---

## What M0c-3b1 excludes (M0c-3b2)

- The real `UnixSocketConnection` transport (POSIX socket to `MUXY_CONTROL_SOCK`) — verified against the live daemon when the app runs.
- The SwiftUI app: window, sidebar view (driven by `AgentStore.byProject` + badges), the libghostty surface view running `muxy attach <pane>`, the spawn button, and building/linking libghostty.

## Self-Review

- **Spec coverage:** Codable models with the internally-tagged shape + `pane`-as-number + PascalCase `AttentionState` (Task 1) ✓; `AgentStore` with the refresh-driven contract — spawn→refresh, unknown-pane→refresh, removed-idempotent, list-clears-refresh, byProject grouping (Task 2) ✓; `ControlSession` decode→apply→auto-`listAgents`, behind a testable `ControlTransport` (Task 3) ✓. Real socket + UI explicitly deferred to M0c-3b2.
- **Placeholder scan:** every step has complete Swift code; no TBD.
- **Type consistency:** `AttentionState`/`AgentInfo`/`ControlRequest`/`ControlEvent` defined in Task 1 and used identically in Tasks 2–3; `ControlSession` consumes `AgentStore.apply`/`needsRefresh`/`clearRefresh` exactly as defined in Task 2; `ControlTransport` defined and consumed with the fake in Task 3.
- **Verifiability:** all three tasks are 100% `swift test` (`cd macos && swift test`), no UI/libghostty/socket. `.v5` language mode avoids Swift-6 concurrency friction on `ObservableObject`. `.build/` is gitignored.
- **JSON contract:** the tag names (`agentList`, `attentionChanged`, `agentRemoved`, `agentSpawned`, `error`, `listAgents`, `spawnAgent`), `pane` as a bare number, and `AttentionState` PascalCase values exactly match what the M0c-3a daemon emits/accepts.
