import XCTest
@testable import ClowderCore

final class ModelsTests: XCTestCase {
    func testDecodeAgentList() throws {
        let json = #"{"type":"agentList","agents":[{"pane":2,"project":"clowder","task":"t","state":"NeedsInput"}]}"#
        let ev = try JSONDecoder().decode(ControlEvent.self, from: Data(json.utf8))
        XCTAssertEqual(ev, .agentList([AgentInfo(pane: 2, project: "clowder", task: "t", state: .needsInput)]))
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
