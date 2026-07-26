# muxy M0c-3b1 — Swift Testable Core

## Context

M0c-3b is the native macOS SwiftUI app. It splits into a **testable Swift core** (this,
M0c-3b1) and the **app itself** (M0c-3b2 — window, sidebar UI, libghostty surface view, verified
by running it). M0c-3b1 is the daemon-facing logic: decode the JSON control feed into an agent
model with the **refresh-driven** contract the M0c-3a/reaper reviews nailed down. It is pure Swift
with no UI and no libghostty, so `swift test` verifies it exactly like `cargo test` — subagent-executable.

Consumes M0c-3a's JSON control channel (`MUXY_CONTROL_SOCK`, newline-delimited `ControlEvent`/
`ControlRequest`, internally tagged `{"type":…}`, `PaneId`→bare number, `AttentionState`→PascalCase
string).

## Package

A SwiftPM package at `macos/` (Swift 6.3 / Command Line Tools — no Xcode, no libghostty needed for
the core). `// swift-tools-version:6.0`, **`.v5` language mode** to avoid Swift-6 strict-concurrency
churn in the core. One library target `MuxyCore` + one XCTest target `MuxyCoreTests`. The executable
app target is added in M0c-3b2. (Cargo's `crates/` workspace is untouched — separate build system.)

## Components

### Models (`Models.swift`) — mirror the Rust JSON exactly

```swift
enum AttentionState: String, Codable { case idle="Idle", working="Working",
    needsInput="NeedsInput", completed="Completed", exited="Exited" }

struct AgentInfo: Codable, Identifiable, Equatable {
    let pane: UInt64; let project: String; let task: String; var state: AttentionState
    var id: UInt64 { pane }
}

enum ControlRequest { case listAgents; case spawnAgent(project:String, task:String, adapter:String) }
enum ControlEvent { case agentList([AgentInfo]); case attentionChanged(pane:UInt64, state:AttentionState);
    case agentRemoved(pane:UInt64); case agentSpawned(pane:UInt64); case error(message:String) }
```

`ControlRequest: Encodable` and `ControlEvent: Decodable` need **custom** coding to produce/parse
the internally-tagged shape (`{"type":"spawnAgent",…}` / discriminate on `"type"`), since Swift
`Codable` doesn't do internally-tagged enums automatically.

### `AgentStore` (`AgentStore.swift`) — the refresh-driven model

An `ObservableObject` holding `agents: [UInt64: AgentInfo]` and a `needsRefresh` flag. `apply(_ event:)`:
- `agentList` → replace the whole map; clear `needsRefresh`.
- `attentionChanged(pane,state)` → update that agent's `state`; **if the pane is unknown → set
  `needsRefresh`** (a 2nd GUI learns of new agents only via a stray attention event).
- `agentSpawned(pane)` → pane-only, so **set `needsRefresh`** (re-list to hydrate project/task).
- `agentRemoved(pane)` → remove (idempotent).
- `error` → ignore (or surface later).
- `byProject` computed grouping (agents grouped by project, sorted by pane) for the sidebar.

The consumer (M0c-3b2) reacts to `needsRefresh` by sending `ControlRequest.listAgents`.

### `ControlSession` + transport (`ControlSession.swift`, `UnixSocketConnection.swift`)

- A **transport protocol** (`ControlTransport`: an async sequence of inbound lines + `send(line:)`),
  so the session's decode→apply→send logic is `swift test`-able against a **fake** in-memory transport.
- `ControlSession` drives it: for each inbound line, `ControlDecoder.decode` → `store.apply`; exposes
  `send(_ request: ControlRequest)`; on `store.needsRefresh`, sends `listAgents` and clears the flag.
- `UnixSocketConnection` is the thin real transport (POSIX `socket`/`connect` to a `sockaddr_un`,
  buffered line reads/writes). Verified manually / in M0c-3b2 against the live daemon — not a unit test.

## Testability (`swift test`)

- **Models:** decode fixture JSON strings for each `ControlEvent` (incl. `pane` as a bare number,
  `state` as `"Exited"`); encode each `ControlRequest` and assert the tagged JSON.
- **`AgentStore`:** apply event sequences → assert `agents`/`byProject`; the refresh-driven cases
  (`agentSpawned` → `needsRefresh`; `attentionChanged` for an unknown pane → `needsRefresh`;
  `agentRemoved` idempotent; a following `agentList` clears refresh).
- **`ControlSession`:** with a fake transport feeding lines, assert the store updates and that
  `needsRefresh` triggers a `listAgents` send.

The Swift↔Rust-daemon round trip is a manual check (run daemon; a small Swift tool connects) — it
belongs to M0c-3b2, where the app runs for real.

## Deferred (M0c-3b2)

The SwiftUI app: `NSApplication`/window, the sidebar view (driven by `AgentStore.byProject` +
badges), the libghostty surface view running `muxy attach <pane>` (proves surface-in-`NSView`
compositing), the spawn button (sends `SpawnAgent`), and building/linking libghostty via the spike
recipe. Verified by you running it.

## Verification

`cd macos && swift test` — all core tests green. (Manual, later: `swift build`, run a tiny tool that
connects to a live daemon's control socket and prints the decoded `AgentList`.)
