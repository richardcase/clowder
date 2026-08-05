import XCTest
@testable import ClowderCore

final class ModelsTests: XCTestCase {
    func testDecodeWorktreeList() throws {
        let json = #"{"type":"worktreeList","worktrees":[{"pane":2,"project":"clowder","name":"t","branch":"clowder/t","state":"NeedsInput"}]}"#
        let ev = try JSONDecoder().decode(ControlEvent.self, from: Data(json.utf8))
        XCTAssertEqual(ev, .worktreeList([WorktreeInfo(pane: 2, project: "clowder", name: "t", branch: "clowder/t", state: .needsInput)]))
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

    func testWorktreeListDecodesNameAndBranch() throws {
        let json = #"{"type":"worktreeList","worktrees":[{"pane":2,"project":"/Users/x/code/clowder","name":"task-a","branch":"clowder/task-a","state":"NeedsInput"}]}"#
        let ev = try JSONDecoder().decode(ControlEvent.self, from: Data(json.utf8))
        guard case let .worktreeList(list) = ev else {
            return XCTFail("expected worktreeList, got \(ev)")
        }
        XCTAssertEqual(list.count, 1)
        XCTAssertEqual(list[0].pane, 2)
        XCTAssertEqual(list[0].project, "/Users/x/code/clowder")
        XCTAssertEqual(list[0].name, "task-a")
        XCTAssertEqual(list[0].branch, "clowder/task-a")
        XCTAssertEqual(list[0].state, .needsInput)
    }

    func testListWorktreesRequestEncodesTypeOnly() throws {
        let data = try JSONEncoder().encode(ControlRequest.listWorktrees)
        XCTAssertEqual(String(decoding: data, as: UTF8.self), #"{"type":"listWorktrees"}"#)
    }
}
