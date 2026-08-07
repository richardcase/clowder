import XCTest
import Combine
@testable import ClowderCore

final class FakeControlTransport: ControlTransport {
    private(set) var sentLines: [String] = []
    private(set) var disconnected = false
    var receiver: ((String) -> Void)?
    var onClose: (() -> Void)?
    /// When true, `send(line:)` throws instead of recording — simulates a transport that was
    /// built successfully (past `makeTransport()`) but whose socket dies before/during hydration.
    var failSend = false
    func setReceiver(_ receiver: @escaping (String) -> Void) { self.receiver = receiver }
    func send(line: String) throws {
        if failSend { struct SendFailed: Error {}; throw SendFailed() }
        sentLines.append(line)
    }
    func setOnClose(_ handler: @escaping () -> Void) { self.onClose = handler }
    /// The real UnixSocketConnection fires onClose ASYNCHRONOUSLY; set this to model that timing.
    var deferClose = false
    func disconnect() {
        disconnected = true
        let cb = onClose
        if deferClose { DispatchQueue.main.async { cb?() } } else { cb?() }
    }
    /// Test helper: simulate the daemon pushing a JSON line.
    func deliver(_ line: String) { receiver?(line) }
}

@MainActor
final class AppModelTests: XCTestCase {
    func testConnectGoesLiveAndRequestsAgentList() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        XCTAssertEqual(model.connectionState, .live)
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"listWorktrees\"") })
    }

    /// Regression: a launch that hydrates `listWorktrees`/`listAdapters` but forgets
    /// `listProjects` leaves `store.projects == []` forever — the sidebar shows "No projects
    /// yet", every project-scoped derived value (attention count, Cmd-1…9, palette, menu-bar
    /// badge) is empty, and the New Worktree sheet's picker has nothing to offer, so Create is
    /// permanently disabled. Covers first connect, the reconnect loop, and a backend swap all at
    /// once since they share `attemptConnect()`.
    func testConnectRequestsAllThreeHydrationLists() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"listWorktrees\"") }, "\(fake.sentLines)")
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"listAdapters\"") }, "\(fake.sentLines)")
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"listProjects\"") }, "\(fake.sentLines)")
    }

    /// THE fresh-machine bug: on first launch the app spawns the daemon and connects immediately, so
    /// `connect(2)` hits ENOENT until the daemon binds. This used to land in `.closed` — a terminal
    /// red banner with no Retry — and the only cure was relaunching the app, which then found the
    /// first launch's daemon already listening ("starting a second time it works").
    ///
    /// Now it retries, and the whole grace period is spent in `.connecting`, which renders no banner.
    /// So the assertion that matters is the NEGATIVE one: the user is never shown anything.
    func testFirstConnectRetriesSilentlyWhileTheDaemonIsStillBinding() async {
        let controller = SleepController()
        var call = 0
        struct NotYet: Error {}     // stands in for POSIXError(.ENOENT)
        var observed: [AppModel.ConnectionState] = []
        let model = AppModel(
            makeTransport: {
                call += 1
                if call <= 3 { throw NotYet() }     // daemon still binding
                return FakeControlTransport()
            },
            sleep: { await controller.sleep($0) }
        )

        model.connect()
        observed.append(model.connectionState)
        for _ in 0..<3 {
            _ = await eventually { controller.parkedCount == 1 }
            observed.append(model.connectionState)
            controller.advance()
        }
        let wentLive = await eventually { model.connectionState == .live }
        XCTAssertTrue(wentLive, "should connect once the daemon is up, got \(model.connectionState)")

        // Never surfaced anything to the user on the way: no red banner, no orange one either.
        XCTAssertFalse(observed.contains { if case .closed = $0 { return true } else { return false } },
                       "must not go .closed on a cold start: \(observed)")
        XCTAssertFalse(observed.contains(.reconnecting),
                       "must stay silent within the grace period: \(observed)")
        model.shutdown()
    }

    /// The grace period is silent but not infinite: a daemon that never comes up must eventually
    /// tell the user something, rather than looking like a healthy app that does nothing.
    func testFirstConnectEscalatesToReconnectingOnceGraceIsExhausted() async {
        let controller = SleepController()
        struct Down: Error {}
        let model = AppModel(makeTransport: { throw Down() }, sleep: { await controller.sleep($0) })

        model.connect()
        XCTAssertEqual(model.connectionState, .connecting)

        // Burn through the grace attempts; the state must stay silent for all of them.
        for i in 0..<5 {
            _ = await eventually { controller.parkedCount == 1 }
            XCTAssertEqual(model.connectionState, .connecting, "attempt \(i) should still be silent")
            controller.advance()
        }
        let escalated = await eventually { model.connectionState == .reconnecting }
        XCTAssertTrue(escalated, "expected .reconnecting once grace ran out, got \(model.connectionState)")

        // The fast ramp is what keeps the grace period short enough to be invisible.
        XCTAssertEqual(controller.delays.prefix(5).map { ($0 * 1000).rounded() },
                       [50, 100, 200, 400, 800])
        model.shutdown()
    }

    /// The happy path must not regress into sleeping: a daemon that is already listening (the
    /// second-launch case, and every launch after this fix) connects on the first attempt.
    func testFirstConnectWhenDaemonIsAlreadyUpDoesNotSleepAtAll() {
        let controller = SleepController()
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake }, sleep: { await controller.sleep($0) })
        model.connect()
        XCTAssertEqual(model.connectionState, .live)
        XCTAssertEqual(controller.delays, [], "no retry loop should start when the first attempt works")
        model.shutdown()
    }

    func testOnCloseEntersReconnecting() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })   // default real sleep; we only check the immediate state
        model.connect()
        fake.onClose?()                                  // simulate daemon death
        XCTAssertEqual(model.connectionState, .reconnecting)
        model.shutdown()                                 // cancel the background reconnect task so the test doesn't leak it
    }

    func testSpawnSendsSpawnAgent() throws {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.spawn(project: "/tmp/repo", name: "demo", adapter: "claude")
        let spawnLine = try XCTUnwrap(fake.sentLines.last)
        let obj = try JSONSerialization.jsonObject(with: Data(spawnLine.utf8)) as? [String: Any]
        XCTAssertEqual(obj?["type"] as? String, "spawnAgent")
        XCTAssertEqual(obj?["project"] as? String, "/tmp/repo")
        XCTAssertEqual(obj?["name"] as? String, "demo")
        XCTAssertEqual(obj?["adapter"] as? String, "claude")
    }

    func testShutdownDisconnectsTransport() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.shutdown()
        XCTAssertTrue(fake.disconnected)
    }

    func testReconnectSwapsTransportClearsStoreAndReconnects() {
        let first = FakeControlTransport()
        let model = AppModel(makeTransport: { first })
        model.connect()
        first.deliver(#"{"type":"worktreeList","worktrees":[{"pane":1,"project":"/p","name":"t","branch":"clowder/t","state":"Working"}]}"#)
        XCTAssertFalse(model.store.worktrees.isEmpty)

        let second = FakeControlTransport()
        model.reconnect(makeTransport: { second })

        XCTAssertTrue(first.disconnected)                                  // old backend torn down
        XCTAssertTrue(model.store.worktrees.isEmpty)                       // old worktrees dropped
        XCTAssertEqual(model.connectionState, .live)                      // connected to the new transport
        XCTAssertTrue(second.sentLines.contains { $0.contains("\"type\":\"listWorktrees\"") })  // hydrated the new one
        model.shutdown()
    }

    func testResetDropsAllPerBackendState() {
        let store = AgentStore()
        store.apply(.adapterList([AdapterInfo(id: "codex", displayName: "Codex")]))
        store.apply(.agentSpawned(pane: 5))   // sets needsRefresh = true
        XCTAssertTrue(store.needsRefresh)
        XCTAssertEqual(store.adapters, [AdapterInfo(id: "codex", displayName: "Codex")])

        store.reset()

        XCTAssertTrue(store.worktrees.isEmpty)
        XCTAssertTrue(store.trees.isEmpty)
        XCTAssertNil(store.lastError)
        XCTAssertFalse(store.needsRefresh)                          // no stale refresh into the new session
        XCTAssertEqual(store.adapters, AgentStore.defaultAdapters)  // no stale adapter list
    }

    func testReconnectIgnoresLateAsyncCloseFromReplacedTransport() {
        let first = FakeControlTransport()
        first.deferClose = true                       // model the real transport's async onClose
        let model = AppModel(makeTransport: { first })
        model.connect()

        let second = FakeControlTransport()
        model.reconnect(makeTransport: { second })    // shutdown() disconnects `first` (defers its close)
        XCTAssertEqual(model.connectionState, .live)

        // Pump the main queue so `first`'s deferred onClose fires: the identity guard must ignore it,
        // leaving the healthy new connection live (not flipped to .reconnecting).
        let exp = expectation(description: "main queue pump")
        DispatchQueue.main.async { exp.fulfill() }
        wait(for: [exp], timeout: 1.0)
        XCTAssertEqual(model.connectionState, .live)
        model.shutdown()
    }

    func testAppliedEventsFlowToStore() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        fake.deliver(#"{"type":"worktreeList","worktrees":[{"pane":1,"project":"/p","name":"t","branch":"clowder/t","state":"Working"}]}"#)
        XCTAssertEqual(model.store.worktrees[1]?.name, "t")
    }

    func testStoreMutationRepublishesThroughModel() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        let exp = expectation(description: "model republished on store mutation")
        exp.assertForOverFulfill = false
        let c = model.objectWillChange.sink { _ in exp.fulfill() }
        fake.deliver(#"{"type":"worktreeList","worktrees":[{"pane":1,"project":"/p","name":"t","branch":"clowder/t","state":"Working"}]}"#)
        wait(for: [exp], timeout: 1.0)
        c.cancel()
    }

    func testDismissErrorClearsLastError() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        fake.deliver(#"{"type":"error","message":"boom"}"#)
        XCTAssertEqual(model.store.lastError, "boom")
        model.dismissError()
        XCTAssertNil(model.store.lastError)
    }

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

        let parkedAtFirstBackoff = await eventually { controller.parkedCount == 1 }
        XCTAssertTrue(parkedAtFirstBackoff)   // loop parked at first backoff
        controller.advance()                                  // wake → attemptConnect (fresh transport) → live
        let wentLive = await eventually { model.connectionState == .live }
        XCTAssertTrue(wentLive)

        XCTAssertEqual(transports.count, 2)
        XCTAssertTrue(transports[1].sentLines.contains { $0.contains("\"type\":\"listWorktrees\"") })
        XCTAssertTrue(transports[1].sentLines.contains { $0.contains("\"type\":\"listAdapters\"") })
        XCTAssertTrue(transports[1].sentLines.contains { $0.contains("\"type\":\"listProjects\"") })
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
            let parked = await eventually { controller.parkedCount == 1 }
            XCTAssertTrue(parked)
            controller.advance()
            await Task.yield()
        }
        let wentLive = await eventually { model.connectionState == .live }
        XCTAssertTrue(wentLive)

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
        let parkedAtFirstBackoff = await eventually { controller.parkedCount == 1 }
        XCTAssertTrue(parkedAtFirstBackoff)

        let callsBefore = call                                // 1 (only the initial connect)
        model.shutdown()                                      // cancels the reconnect task
        controller.advance()                                  // wake the parked sleep; the loop must observe cancel and stop
        for _ in 0..<100 { await Task.yield() }               // give it every chance to (wrongly) attempt again

        XCTAssertEqual(call, callsBefore, "no reconnect attempt may run after shutdown")
        XCTAssertTrue(transports[0].disconnected)
    }

    func testMidHydrationFailureDuringReconnectStaysReconnecting() async {
        let controller = SleepController()
        var transports: [FakeControlTransport] = []
        var call = 0
        let model = AppModel(
            makeTransport: {
                call += 1
                let f = FakeControlTransport()
                if call == 2 { f.failSend = true }   // 1st reconnect attempt: transport builds, hydration send throws
                transports.append(f)
                return f
            },
            sleep: { await controller.sleep($0) }
        )

        model.connect()
        XCTAssertEqual(model.connectionState, .live)
        transports[0].onClose?()                              // drop → reconnecting
        XCTAssertEqual(model.connectionState, .reconnecting)

        let parkedAtFirstBackoff = await eventually { controller.parkedCount == 1 }
        XCTAssertTrue(parkedAtFirstBackoff)
        controller.advance()   // wake → attemptConnect on transports[1] (failSend) → throws mid-hydration

        let parkedAtSecondBackoff = await eventually { controller.parkedCount == 1 }
        XCTAssertTrue(parkedAtSecondBackoff)
        XCTAssertEqual(model.connectionState, .reconnecting,
                        "a mid-hydration failure must not leave state as .live")

        controller.advance()   // wake → attemptConnect on transports[2] (failSend = false) → live
        let wentLive = await eventually { model.connectionState == .live }
        XCTAssertTrue(wentLive)
        model.shutdown()
    }

    func testSelectedPaneIsDerivedFromSelection() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.selection = .worktree(5)
        XCTAssertEqual(model.selectedPane, 5)
        model.selection = .project("/code/alpha")
        XCTAssertNil(model.selectedPane, "a project with no open terminal has no pane yet")
        model.store.apply(.projectTerminalOpened(path: "/code/alpha", pane: 9))
        XCTAssertEqual(model.selectedPane, 9, "once the daemon reports the terminal, it resolves")
        model.selection = nil
        XCTAssertNil(model.selectedPane)
    }

    func testSelectingAProjectWithNoTerminalAsksTheDaemon() throws {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.store.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        model.selection = .project("/code/alpha")
        XCTAssertTrue(fake.sentLines.contains { $0.contains("openProjectTerminal") },
                      "must ask the daemon to open the terminal: \(fake.sentLines)")
    }

    func testSelectingAProjectWithAKnownTerminalDoesNotReask() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.store.apply(.projectTerminalOpened(path: "/code/alpha", pane: 9))
        model.selection = .project("/code/alpha")
        XCTAssertFalse(fake.sentLines.contains { $0.contains("openProjectTerminal") },
                       "already open — selecting must not respawn")
    }

    func testLifecycleCommandsAreNoOpsUnderAProjectSelection() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.store.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        model.store.apply(.projectTerminalOpened(path: "/code/alpha", pane: 9))
        model.selection = .project("/code/alpha")
        model.requestLifecycle(.land)
        XCTAssertNil(model.pendingLifecycle, "land must refuse a project terminal")
        model.requestLifecycle(.discard)
        XCTAssertNil(model.pendingLifecycle)
    }

    func testRestartIsOnlyOfferedForAnExitedWorktree() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.store.apply(.projectList([ProjectInfo(path: "/p", name: "p", kind: "git")]))
        model.store.apply(.worktreeList([
            WorktreeInfo(pane: 1, project: "/p", name: "a", branch: "clowder/a", state: .working),
        ]))
        model.selection = .worktree(1)
        XCTAssertFalse(model.canRestartSelection)
        model.restartSelectedWorktree()
        XCTAssertFalse(fake.sentLines.contains { $0.contains("restartWorktree") },
                       "restart must not be sent for a live agent")

        model.store.apply(.attentionChanged(pane: 1, state: .exited))
        XCTAssertTrue(model.canRestartSelection)
        model.restartSelectedWorktree()
        XCTAssertTrue(fake.sentLines.contains { $0.contains("restartWorktree") }, "\(fake.sentLines)")
    }

    /// Project terminals are deliberately not persisted daemon-side, but the same `AgentStore`
    /// survives an ordinary reconnect (only an explicit backend swap calls `store.reset()`). If a
    /// daemon restart silently drops a live `path -> pane` mapping, the app must not keep
    /// believing it — the pane it names may now be dead or reused by something else, with no way
    /// back once the app attaches to it.
    func testReconnectClearsStaleProjectTerminals() async {
        let controller = SleepController()
        var transports: [FakeControlTransport] = []
        let model = AppModel(
            makeTransport: { let f = FakeControlTransport(); transports.append(f); return f },
            sleep: { await controller.sleep($0) }
        )

        model.connect()
        model.store.apply(.projectTerminalOpened(path: "/code/alpha", pane: 9))
        XCTAssertEqual(model.store.projectTerminals["/code/alpha"], 9)

        transports[0].onClose?()                              // the daemon restarted
        let parkedAtFirstBackoff = await eventually { controller.parkedCount == 1 }
        XCTAssertTrue(parkedAtFirstBackoff)
        controller.advance()                                  // wake -> attemptConnect -> live
        let wentLive = await eventually { model.connectionState == .live }
        XCTAssertTrue(wentLive)

        XCTAssertNil(model.store.projectTerminals["/code/alpha"], "stale mapping must not survive a reconnect")
        model.shutdown()
    }

    /// Removing the currently-selected project must not leave the detail pane wedged on a
    /// permanent "Starting terminal…" spinner — the row is gone, so there is no future "next
    /// select" to respawn the open, unlike the ordinary case this comment used to justify.
    func testSelectionClearsWhenItsProjectIsRemoved() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.store.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        model.selection = .project("/code/alpha")
        XCTAssertNotNil(model.selection)

        model.store.apply(.projectRemoved(path: "/code/alpha"))
        // objectWillChange fires synchronously (Combine's `willChange` semantics), but the
        // resolution runs on the next main-queue turn — pump it like the reconnect tests do.
        let exp = expectation(description: "main queue pump")
        DispatchQueue.main.async { exp.fulfill() }
        wait(for: [exp], timeout: 1.0)
        XCTAssertNil(model.selection, "a selection whose project vanished must clear, not spin forever")
    }

    /// A project terminal that closes (the user typed `exit`) while still selected must not look
    /// identical to "still opening" — the fix distinguishes the two so the detail view can offer
    /// Reopen instead of spinning forever with no way back in.
    func testProjectTerminalIsTrackedAsClosedAfterHavingBeenOpen() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.store.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        model.selection = .project("/code/alpha")
        XCTAssertFalse(model.closedProjectTerminals.contains("/code/alpha"), "never opened yet — not 'closed'")

        model.store.apply(.projectTerminalOpened(path: "/code/alpha", pane: 9))
        pumpMainQueue()
        XCTAssertFalse(model.closedProjectTerminals.contains("/code/alpha"), "open — not closed")

        model.store.apply(.projectTerminalClosed(path: "/code/alpha"))
        pumpMainQueue()
        XCTAssertTrue(model.closedProjectTerminals.contains("/code/alpha"), "was open, now isn't — closed")

        // Reopening clears the closed marker again.
        model.store.apply(.projectTerminalOpened(path: "/code/alpha", pane: 11))
        pumpMainQueue()
        XCTAssertFalse(model.closedProjectTerminals.contains("/code/alpha"))
    }

    private func pumpMainQueue() {
        let exp = expectation(description: "main queue pump")
        DispatchQueue.main.async { exp.fulfill() }
        wait(for: [exp], timeout: 1.0)
    }

    func testSelectingAWorktreeRequestsItsSplitTree() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.selection = .worktree(4)
        XCTAssertTrue(fake.sentLines.contains { $0.contains("getSplitTree") }, "\(fake.sentLines)")
    }
}

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
