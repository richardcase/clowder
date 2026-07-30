# muxy M5d — Client Auto-Reconnect

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When the macOS app's control connection drops (daemon killed/restarted), instead of a
terminal "Disconnected" state, `AppModel` enters a **bounded exponential-backoff reconnect loop**
that rebuilds the transport and re-hydrates (`listAgents` + `listAdapters`) on success, surfacing a
"Reconnecting…" state; the loop is cancelled on explicit quit. Resilient to a daemon restart.

**Architecture:** Pure `MuxyCore` logic (unit-testable). `AppModel` gains a `.reconnecting`
connection state, an injectable async `sleep` seam (real `Task.sleep` in production; a gated
controller in tests), and a `reconnectTask` running the backoff loop. The transport's close callback
now routes to a `handleClose()` that schedules the loop (unless shutting down); each attempt reuses
the existing `makeTransport` closure (which already builds a fresh `UnixSocketConnection` per call)
and the same hydration `connect()` does. `MuxyApp`'s `ContentView` adds a "Reconnecting…" banner. No
daemon/proto change — surfaces re-attach via their own `muxy attach`; the daemon already replays a
fresh `AgentList` on each new control connection, and `AgentStore.apply(.agentList)` replaces the map
so stale agents clear automatically.

**Tech Stack:** Swift 6 / SwiftUI; `macos/` package (`MuxyCore` lib + `MuxyApp` exe). Spec:
`docs/superpowers/specs/2026-07-30-muxy-m5-robustness-design.md` (§4 Client auto-reconnect).

## Global Constraints

- **Scope: `macos/` only** (`MuxyCore/AppModel.swift`, `MuxyCore` tests, `MuxyApp/ContentView.swift`).
  No Rust/proto/daemon change. The daemon already sends a fresh `AgentList` on each new control
  connection; the client just needs to reconnect and re-hydrate.
- **Backoff:** exponential, `delay(attempt) = min(10.0, 0.5 * 2^attempt)` → `0.5, 1, 2, 4, 8, 10, 10…`
  seconds. **Bounded** at 10s. Cancellable.
- **Reconnect triggers on a DROP of a live connection** (the transport's `onClose`). An **initial**
  `connect()` failure keeps its current terminal `.closed(reason:)` behavior (the app is launched
  before any daemon exists only in the M6 app-launches-daemon flow, out of scope here). The reconnect
  loop itself still handles `makeTransport()` throwing (daemon still down between restart attempts).
- **Explicit quit cancels the loop:** `shutdown()` sets a shutting-down flag, cancels `reconnectTask`,
  and disconnects; the disconnect's `onClose` must NOT re-arm reconnect.
- **Testability:** `AppModel.init` gains a defaulted `sleep: (TimeInterval) async -> Void` seam so the
  backoff loop is driven deterministically in tests (no real waiting). Production default is
  `Task.sleep`. `AppModel` is `@MainActor`; the loop, the sleep, and all state runs on the main actor.
- Re-hydration on a successful (re)connect is exactly what `connect()` sends today: `listAgents`
  then `listAdapters`. No more, no less (keeps parity; the daemon auto-sends `AgentList` on connect too).
- Swift; `@MainActor`. Build/test: `cd macos && swift test` (MuxyCore — fast, no libghostty) for
  Task 1; `cd macos && swift build` (compiles MuxyApp against the vendored libghostty) for Task 2.

---

## Task 1: `AppModel` reconnect loop (`MuxyCore`)

**Files:**
- Modify: `macos/Sources/MuxyCore/AppModel.swift`
- Modify: `macos/Tests/MuxyCoreTests/AppModelTests.swift` (update 1 existing test; add a `SleepController`
  helper + 3 new tests)

**Interfaces:**
- Consumes: `makeTransport: () throws -> ControlTransport` (already re-creates a fresh transport per
  call), `ControlSession`, `AgentStore`, `ControlTransport.setOnClose`.
- Produces:
  - `AppModel.ConnectionState` gains a `case reconnecting`.
  - `AppModel.init(store:makeTransport:sleep:)` — new defaulted `sleep: (TimeInterval) async -> Void`
    parameter (default real `Task.sleep`).
  - Behavior: a live connection dropping enters `.reconnecting` and retries with bounded backoff,
    going `.live` + re-hydrating on success; `shutdown()` cancels the loop.

- [ ] **Step 1: Update the existing on-close test + add the `SleepController` helper and new tests.**
In `macos/Tests/MuxyCoreTests/AppModelTests.swift`:

First, REPLACE the existing `testOnCloseTransitionsToClosed` (it asserts `.closed`; the new behavior is
`.reconnecting`) with:

```swift
    func testOnCloseEntersReconnecting() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })   // default real sleep; we only check the immediate state
        model.connect()
        fake.onClose?()                                  // simulate daemon death
        XCTAssertEqual(model.connectionState, .reconnecting)
        model.shutdown()                                 // cancel the background reconnect task so the test doesn't leak it
    }
```

Then ADD, at file scope (outside `AppModelTests`), a deterministic sleep gate + an async settle helper:

```swift
/// A test double for AppModel's injected `sleep`: records each requested delay and parks the
/// reconnect loop until the test calls `advance()`, giving fully deterministic backoff control.
@MainActor
final class SleepController {
    private(set) var delays: [TimeInterval] = []
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func sleep(_ delay: TimeInterval) async {
        delays.append(delay)
        await withCheckedContinuation { waiters.append($0) }
    }

    /// Wake the oldest parked sleep (advance the reconnect loop by one iteration).
    func advance() {
        guard !waiters.isEmpty else { return }
        waiters.removeFirst().resume()
    }

    var parkedCount: Int { waiters.count }
}

/// Yield the main actor until `cond` holds (or a generous cap), so a scheduled @MainActor Task can run.
@MainActor
func eventually(_ cond: @escaping () -> Bool, maxYields: Int = 2000) async -> Bool {
    for _ in 0..<maxYields {
        if cond() { return true }
        await Task.yield()
    }
    return cond()
}
```

Then ADD the three new tests as methods of `AppModelTests` (note: these are `async` tests):

```swift
    func testDropTriggersReconnectThenGoesLiveAndRehydrates() async {
        let controller = SleepController()
        var transports: [FakeControlTransport] = []
        let model = AppModel(
            makeTransport: { let f = FakeControlTransport(); transports.append(f); return f },
            sleep: { await controller.sleep($0) }
        )

        model.connect()
        XCTAssertEqual(model.connectionState, .live)
        XCTAssertEqual(transports.count, 1)

        transports[0].onClose?()                              // daemon drops the live connection
        XCTAssertEqual(model.connectionState, .reconnecting)

        XCTAssertTrue(await eventually { controller.parkedCount == 1 })   // loop parked at first backoff
        controller.advance()                                  // wake → attemptConnect (fresh transport) → live
        XCTAssertTrue(await eventually { model.connectionState == .live })

        XCTAssertEqual(transports.count, 2)
        XCTAssertTrue(transports[1].sentLines.contains { $0.contains("\"type\":\"listAgents\"") })
        XCTAssertTrue(transports[1].sentLines.contains { $0.contains("\"type\":\"listAdapters\"") })
        model.shutdown()
    }

    func testReconnectBackoffIsBoundedAndNonDecreasing() async {
        let controller = SleepController()
        var transports: [FakeControlTransport] = []
        var call = 0
        struct Down: Error {}
        let model = AppModel(
            makeTransport: {
                call += 1
                // call 1 = initial connect OK; calls 2..7 = reconnect attempts fail; call 8 = success.
                if call == 1 || call >= 8 { let f = FakeControlTransport(); transports.append(f); return f }
                throw Down()
            },
            sleep: { await controller.sleep($0) }
        )

        model.connect()
        XCTAssertEqual(model.connectionState, .live)
        transports[0].onClose?()
        XCTAssertEqual(model.connectionState, .reconnecting)

        // Seven backoff sleeps: six failing attempts (calls 2..7) then a success (call 8).
        for _ in 0..<7 {
            XCTAssertTrue(await eventually { controller.parkedCount == 1 })
            controller.advance()
            await Task.yield()
        }
        XCTAssertTrue(await eventually { model.connectionState == .live })

        XCTAssertEqual(controller.delays, [0.5, 1, 2, 4, 8, 10, 10])   // bounded at 10, non-decreasing
        model.shutdown()
    }

    func testShutdownCancelsReconnect() async {
        let controller = SleepController()
        var transports: [FakeControlTransport] = []
        var call = 0
        struct Down: Error {}
        let model = AppModel(
            makeTransport: {
                call += 1
                if call == 1 { let f = FakeControlTransport(); transports.append(f); return f }
                throw Down()                                  // every reconnect attempt fails
            },
            sleep: { await controller.sleep($0) }
        )

        model.connect()
        transports[0].onClose?()                              // drop → reconnecting, loop parks at first sleep
        XCTAssertEqual(model.connectionState, .reconnecting)
        XCTAssertTrue(await eventually { controller.parkedCount == 1 })

        let callsBefore = call                                // 1 (only the initial connect)
        model.shutdown()                                      // cancels the reconnect task
        controller.advance()                                  // wake the parked sleep; the loop must observe cancel and stop
        for _ in 0..<100 { await Task.yield() }               // give it every chance to (wrongly) attempt again

        XCTAssertEqual(call, callsBefore, "no reconnect attempt may run after shutdown")
        XCTAssertTrue(transports[0].disconnected)
    }
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cd /Users/richard/code/muxy/macos && swift test --filter AppModelTests 2>&1 | tail -30`
Expected: FAIL to COMPILE / FAIL — `.reconnecting` doesn't exist, `AppModel.init` has no `sleep:`
parameter, and the reconnect behavior isn't implemented.

- [ ] **Step 3: Add the `.reconnecting` state + the injected `sleep` seam + reconnect fields.** In
`macos/Sources/MuxyCore/AppModel.swift`:

Add the enum case:
```swift
    public enum ConnectionState: Equatable {
        case connecting
        case live
        case reconnecting
        case closed(reason: String)
    }
```

Add stored properties (next to `makeTransport`/`connection`/`session`):
```swift
    private let sleepFn: (TimeInterval) async -> Void
    private var reconnectTask: Task<Void, Never>?
    private var isShuttingDown = false
```

Change `init` to accept and store the sleep seam:
```swift
    public init(store: AgentStore = AgentStore(),
                makeTransport: @escaping () throws -> ControlTransport,
                sleep: @escaping (TimeInterval) async -> Void = { d in
                    try? await Task.sleep(nanoseconds: UInt64(max(0, d) * 1_000_000_000))
                }) {
        self.store = store
        self.makeTransport = makeTransport
        self.sleepFn = sleep
        self.storeSubscription = store.objectWillChange.sink { [weak self] _ in
            self?.objectWillChange.send()
            DispatchQueue.main.async { self?.reconcileFocus() }
        }
    }
```

- [ ] **Step 4: Refactor `connect()` to share an `attemptConnect()` helper, and route close → reconnect.**
Replace the existing `connect()` with:

```swift
    /// Build the transport + session and hydrate. Initial failure lands in `.closed`; a later DROP
    /// of a live connection enters the reconnect loop (see `handleClose`).
    public func connect() {
        isShuttingDown = false
        connectionState = .connecting
        do {
            try attemptConnect()
        } catch {
            connectionState = .closed(reason: "Could not connect: \(error)")
        }
    }

    /// One connection attempt: build the transport, wire close→reconnect, hydrate. Throws on failure.
    private func attemptConnect() throws {
        let transport = try makeTransport()
        transport.setOnClose { [weak self] in self?.handleClose() }
        let session = ControlSession(transport: transport, store: store)
        self.connection = transport
        self.session = session
        connectionState = .live
        try session.send(.listAgents)
        try session.send(.listAdapters)
    }

    /// The transport closed. Unless we're explicitly shutting down, start reconnecting.
    private func handleClose() {
        guard !isShuttingDown else { return }
        scheduleReconnect()
    }

    private func backoffDelay(_ attempt: Int) -> TimeInterval {
        min(10.0, 0.5 * pow(2.0, Double(attempt)))
    }

    /// Start the bounded exponential-backoff reconnect loop (idempotent while one is running).
    private func scheduleReconnect() {
        guard !isShuttingDown, reconnectTask == nil else { return }
        connectionState = .reconnecting
        reconnectTask = Task { [weak self] in await self?.reconnectLoop() }
    }

    private func reconnectLoop() async {
        var attempt = 0
        while !Task.isCancelled && !isShuttingDown {
            await sleepFn(backoffDelay(attempt))
            if Task.isCancelled || isShuttingDown { break }
            do {
                try attemptConnect()          // sets .live + re-hydrates on success
                reconnectTask = nil
                return
            } catch {
                attempt += 1
            }
        }
        reconnectTask = nil
    }
```

(Note: `pow` comes from `Foundation`, already imported at the top of the file.)

- [ ] **Step 5: Cancel the loop on explicit shutdown.** Replace `shutdown()` with:

```swift
    /// Explicit teardown (F1): cancel any reconnect loop, then disconnect. `isShuttingDown` makes the
    /// disconnect's own `onClose` a no-op so we don't re-arm reconnect while quitting.
    public func shutdown() {
        isShuttingDown = true
        reconnectTask?.cancel()
        reconnectTask = nil
        connection?.disconnect()
        session = nil
        connection = nil
    }
```

- [ ] **Step 6: Run the new + updated tests to verify they pass**

Run: `cd /Users/richard/code/muxy/macos && swift test --filter AppModelTests 2>&1 | tail -30`
Expected: PASS — `testOnCloseEntersReconnecting`, `testDropTriggersReconnectThenGoesLiveAndRehydrates`,
`testReconnectBackoffIsBoundedAndNonDecreasing`, `testShutdownCancelsReconnect`, and all the
pre-existing `AppModelTests` (connect-live, connect-failure-closed, spawn, shutdown-disconnects,
events-flow, republish, dismiss-error).

- [ ] **Step 7: Run the whole MuxyCore suite (no regressions)**

Run: `cd /Users/richard/code/muxy/macos && swift test 2>&1 | tail -20`
Expected: all MuxyCore tests PASS. In particular `testConnectFailureBecomesClosed` still passes
(initial connect failure stays `.closed` — unchanged).

- [ ] **Step 8: Commit**

```bash
git add macos/Sources/MuxyCore/AppModel.swift macos/Tests/MuxyCoreTests/AppModelTests.swift
git commit -m "feat(client): auto-reconnect with bounded backoff + re-hydrate on daemon drop"
```

---

## Task 2: "Reconnecting…" banner (`MuxyApp`)

**Files:**
- Modify: `macos/Sources/MuxyApp/ContentView.swift` (add the `.reconnecting` banner to `statusBar`)

**Interfaces:**
- Consumes: `AppModel.ConnectionState.reconnecting` (Task 1).
- Produces: a persistent "Reconnecting to daemon…" banner while `connectionState == .reconnecting`.

- [ ] **Step 1: Add the `.reconnecting` banner.** In `macos/Sources/MuxyApp/ContentView.swift`, in the
`statusBar` computed view, change:

```swift
    @ViewBuilder private var statusBar: some View {
        VStack(spacing: 0) {
            if case let .closed(reason) = model.connectionState {
                // Live connection state — persists until reconnect, so not dismissable.
                banner(reason, color: .red)
            } else if let err = model.store.lastError {
                // A one-shot error — dismissable.
                banner(err, color: .orange, onDismiss: { model.dismissError() })
            }
        }
    }
```
to:
```swift
    @ViewBuilder private var statusBar: some View {
        VStack(spacing: 0) {
            if case .reconnecting = model.connectionState {
                // Auto-reconnect in progress — persists until we're live again, not dismissable.
                banner("Reconnecting to daemon…", color: .orange)
            } else if case let .closed(reason) = model.connectionState {
                // Terminal connection state — persists, not dismissable.
                banner(reason, color: .red)
            } else if let err = model.store.lastError {
                // A one-shot error — dismissable.
                banner(err, color: .orange, onDismiss: { model.dismissError() })
            }
        }
    }
```

- [ ] **Step 2: Build the app to confirm it compiles** (links the vendored libghostty; slower than
`swift test`).

Run: `cd /Users/richard/code/muxy/macos && swift build 2>&1 | tail -20`
Expected: builds with no errors. (If the vendored `vendor/libghostty/ghostty-internal.a` is absent in
your environment, `swift build` of MuxyApp will fail to link — in that case run
`swift build --target MuxyCore` to confirm the core still compiles, note the MuxyApp link could not
be verified locally, and defer the full build + manual smoke to the controller/user. The
`ContentView` change is a self-contained SwiftUI edit.)

- [ ] **Step 3: Manual smoke** (record the outcome; the user runs the GUI per the project runbook —
build the daemon, run the app, then):
  1. With the app attached to an agent, `kill` the daemon → the app shows the orange
     **"Reconnecting to daemon…"** banner (not a terminal red "Disconnected").
  2. Restart the daemon → the banner clears, the app returns to live, and the agent list re-hydrates
     (`listAgents`/`listAdapters` re-sent); terminals re-attach via their own `muxy attach`.
  3. Quit the app (⌘Q) while reconnecting → the reconnect loop is cancelled cleanly (no spin).

- [ ] **Step 4: Commit**

```bash
git add macos/Sources/MuxyApp/ContentView.swift
git commit -m "feat(client): show a Reconnecting banner while the control channel is down"
```

---

## Self-Review Notes (author)

- **Spec §4 coverage:** reconnect-with-backoff on the transport close callback → Task 1
  (`handleClose`→`scheduleReconnect`→`reconnectLoop`); "reconnecting…" state → Task 1 (`.reconnecting`)
  + Task 2 (banner); re-hydrate on success (`listAgents`/`listAdapters`) → Task 1 `attemptConnect`;
  cancel on explicit quit/`disconnect()` → Task 1 `shutdown` + `isShuttingDown` guard; unit-testable
  via a fail-then-succeed fake transport → Task 1 tests. Spec §4 testing bullets all map: fails-then-
  succeeds → `testDropTriggersReconnectThenGoesLiveAndRehydrates`; bounded backoff →
  `testReconnectBackoffIsBoundedAndNonDecreasing`; explicit quit cancels → `testShutdownCancelsReconnect`.
- **Deliberate scope call:** an INITIAL `connect()` failure keeps `.closed` (unchanged —
  `testConnectFailureBecomesClosed` stays green); only a DROP of a live connection reconnects. The
  loop still handles `makeTransport()` throwing during reconnect (daemon down between restart polls).
  The one behavior change to an existing test is `testOnCloseTransitionsToClosed` →
  `testOnCloseEntersReconnecting`, which is the intended M5d change (a drop is no longer terminal).
- **No stale-agent bug on reconnect:** `AgentStore.apply(.agentList)` replaces the whole map, and the
  daemon replays `AgentList` on every new control connection + our explicit `listAgents` — so agents
  from the dead daemon are cleared on reconnect without any manual store reset.
- **Type consistency:** `.reconnecting`, `attemptConnect`/`handleClose`/`scheduleReconnect`/
  `reconnectLoop`/`backoffDelay`, and the `sleep:` init param are used consistently across the impl
  and tests; the `ContentView` banner reads the same `.reconnecting` case.
- **Deferred (carry-forward, non-blocking):** re-fetch the selected pane's split tree on reconnect
  (currently re-hydrates via `AgentList` + the next `SplitTreeChanged`; `getSplitTree` on reconnect is
  a nice-to-have); reconcile a mid-drag `localRatio` on reconnect (noted since M1c-3). Neither blocks M5d.
