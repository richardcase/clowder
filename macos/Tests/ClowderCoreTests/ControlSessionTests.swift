import XCTest
@testable import ClowderCore

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
        let session = ControlSession(transport: t)
        t.feed(#"{"type":"attentionChanged","pane":42,"state":"Working"}"#)
        XCTAssertEqual(t.sent, [#"{"type":"listAgents"}"#])
    }

    func testAgentSpawnedTriggersListAgentsSend() {
        let t = FakeTransport()
        let session = ControlSession(transport: t)
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
