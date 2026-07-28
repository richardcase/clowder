import XCTest
import Combine
@testable import MuxyCore

final class FakeControlTransport: ControlTransport {
    private(set) var sentLines: [String] = []
    private(set) var disconnected = false
    var receiver: ((String) -> Void)?
    var onClose: (() -> Void)?
    func setReceiver(_ receiver: @escaping (String) -> Void) { self.receiver = receiver }
    func send(line: String) throws { sentLines.append(line) }
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

    func testOnCloseTransitionsToClosed() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        fake.onClose?()                       // simulate daemon death
        guard case .closed = model.connectionState else {
            return XCTFail("expected .closed after onClose")
        }
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
}
