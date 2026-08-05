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
        let data = try JSONEncoder().encode(ControlRequest.spawnAgent(project: "/p", name: "t", adapter: "shell"))
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

    /// Resolve `docs/protocol/fixtures` from this source file's location, so the test does not
    /// depend on the working directory `swift test` happens to run in.
    private func fixture(_ name: String, file: StaticString = #filePath) throws -> Data {
        let here = URL(fileURLWithPath: "\(file)")
        let repo = here.deletingLastPathComponent()   // ClowderCoreTests
            .deletingLastPathComponent()              // Tests
            .deletingLastPathComponent()              // macos
            .deletingLastPathComponent()              // repo root
        return try Data(contentsOf: repo.appendingPathComponent("docs/protocol/fixtures/\(name)"))
    }

    func testDecodesEveryGoldenFixture() throws {
        let d = JSONDecoder()

        guard case let .projectList(ps) = try d.decode(ControlEvent.self, from: fixture("project-list.json")) else {
            return XCTFail("project-list.json did not decode to .projectList")
        }
        XCTAssertEqual(ps, [ProjectInfo(path: "/Users/x/code/clowder", name: "clowder", kind: "git")])

        guard case let .projectAdded(p) = try d.decode(ControlEvent.self, from: fixture("project-added.json")) else {
            return XCTFail("project-added.json did not decode to .projectAdded")
        }
        XCTAssertEqual(p.kind, "git")

        guard case let .projectRemoved(path) = try d.decode(ControlEvent.self, from: fixture("project-removed.json")) else {
            return XCTFail("project-removed.json did not decode to .projectRemoved")
        }
        XCTAssertEqual(path, "/Users/x/code/clowder")

        guard case let .projectTerminalOpened(tPath, pane) =
                try d.decode(ControlEvent.self, from: fixture("project-terminal-opened.json")) else {
            return XCTFail("project-terminal-opened.json did not decode")
        }
        XCTAssertEqual(tPath, "/Users/x/code/clowder")
        XCTAssertEqual(pane, 9)

        guard case let .projectTerminalClosed(cPath) =
                try d.decode(ControlEvent.self, from: fixture("project-terminal-closed.json")) else {
            return XCTFail("project-terminal-closed.json did not decode")
        }
        XCTAssertEqual(cPath, "/Users/x/code/clowder")

        guard case let .worktreeList(ws) = try d.decode(ControlEvent.self, from: fixture("worktree-list.json")) else {
            return XCTFail("worktree-list.json did not decode to .worktreeList")
        }
        XCTAssertEqual(ws.count, 1)
        XCTAssertEqual(ws[0].branch, "clowder/task-a")
        XCTAssertEqual(ws[0].state, .needsInput)
    }

    func testSpawnAgentEncodesNameNotTask() throws {
        let data = try JSONEncoder().encode(
            ControlRequest.spawnAgent(project: "/p", name: "add-projects", adapter: "claude"))
        let s = String(decoding: data, as: UTF8.self)
        XCTAssertTrue(s.contains("\"name\":\"add-projects\""), s)
        XCTAssertFalse(s.contains("\"task\""), s)
    }
}
