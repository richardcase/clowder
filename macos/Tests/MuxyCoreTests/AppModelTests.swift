import XCTest
import Combine
@testable import MuxyCore

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
    func disconnect() { disconnected = true; onClose?() }
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
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"listAgents\"") })
    }

    func testConnectFailureBecomesClosed() {
        struct BoomError: Error {}
        let model = AppModel(makeTransport: { throw BoomError() })
        model.connect()
        guard case .closed = model.connectionState else {
            return XCTFail("expected .closed, got \(model.connectionState)")
        }
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
        model.spawn(project: "/tmp/repo", task: "demo", adapter: "claude")
        let spawnLine = try XCTUnwrap(fake.sentLines.last)
        let obj = try JSONSerialization.jsonObject(with: Data(spawnLine.utf8)) as? [String: Any]
        XCTAssertEqual(obj?["type"] as? String, "spawnAgent")
        XCTAssertEqual(obj?["project"] as? String, "/tmp/repo")
        XCTAssertEqual(obj?["task"] as? String, "demo")
        XCTAssertEqual(obj?["adapter"] as? String, "claude")
    }

    func testShutdownDisconnectsTransport() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.shutdown()
        XCTAssertTrue(fake.disconnected)
    }

    func testAppliedEventsFlowToStore() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        fake.deliver(#"{"type":"agentList","agents":[{"pane":1,"project":"/p","task":"t","state":"Working"}]}"#)
        XCTAssertEqual(model.store.agents[1]?.task, "t")
    }

    func testStoreMutationRepublishesThroughModel() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        let exp = expectation(description: "model republished on store mutation")
        exp.assertForOverFulfill = false
        let c = model.objectWillChange.sink { _ in exp.fulfill() }
        fake.deliver(#"{"type":"agentList","agents":[{"pane":1,"project":"/p","task":"t","state":"Working"}]}"#)
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
