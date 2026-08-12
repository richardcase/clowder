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

        guard case let .splitTreeChanged(agent, tree) =
                try d.decode(ControlEvent.self, from: fixture("split-tree-changed.json")) else {
            return XCTFail("split-tree-changed.json did not decode to .splitTreeChanged")
        }
        XCTAssertEqual(agent, 2)
        guard case let .split(id, axis, ratio, first, second) = tree else {
            return XCTFail("expected a .split root, got \(tree)")
        }
        XCTAssertEqual(id, 1)
        XCTAssertEqual(axis, .horizontal)
        XCTAssertEqual(ratio, 0.5, accuracy: 0.0001)
        XCTAssertEqual(first, .leaf(pane: 2))
        XCTAssertEqual(second, .leaf(pane: 3))

        guard case let .agentProfileList(profiles) =
                try d.decode(ControlEvent.self, from: fixture("agent-profile-list.json")) else {
            return XCTFail("agent-profile-list.json did not decode to .agentProfileList")
        }
        XCTAssertEqual(profiles.count, 2)
        XCTAssertTrue(profiles[0].builtin, "first profile must be builtin: \(profiles[0])")
        XCTAssertFalse(profiles[1].builtin, "second profile must be a user profile: \(profiles[1])")
        XCTAssertEqual(profiles[1].displayName, "Claude (Opus)")
        XCTAssertEqual(profiles[1].args, "--model opus")
    }

    func testSpawnAgentEncodesNameNotTask() throws {
        let data = try JSONEncoder().encode(
            ControlRequest.spawnAgent(project: "/p", name: "add-projects", adapter: "claude"))
        let s = String(decoding: data, as: UTF8.self)
        XCTAssertTrue(s.contains("\"name\":\"add-projects\""), s)
        XCTAssertFalse(s.contains("\"task\""), s)
    }

    /// Requests are the mirror image of events: Swift *encodes* a `ControlRequest` and Rust
    /// decodes it (`request_fixtures_decode_and_roundtrip` in `crates/clowder-proto`). So the
    /// Swift-side guarantee for a request fixture is that the encoder PRODUCES the fixture —
    /// the opposite check from the event fixtures above, which Swift only decodes.
    ///
    /// This compares decoded JSON objects rather than raw bytes: confirmed empirically,
    /// `JSONEncoder` on this toolchain does NOT preserve the key order `encode(to:)` calls
    /// `container.encode(_:forKey:)` in, so a literal byte/string comparison against a fixture
    /// whose key order comes from Rust's field-declaration order is not stable. Comparing the
    /// parsed objects still fails exactly like a byte comparison would if a field were renamed,
    /// dropped, added, or given the wrong value — it just isn't sensitive to key order.
    func testEncodesEveryGoldenRequestFixtureExactly() throws {
        let e = JSONEncoder()

        let spawnData = try e.encode(
            ControlRequest.spawnAgent(project: "/Users/x/code/clowder", name: "add-projects", adapter: "claude"))
        try assertJSONObjectsEqual(spawnData, fixture("spawn-agent.json"))

        let openData = try e.encode(ControlRequest.openProjectTerminal(path: "/Users/x/code/clowder"))
        try assertJSONObjectsEqual(openData, fixture("open-project-terminal.json"))

        let listData = try e.encode(ControlRequest.listAgentProfiles)
        try assertJSONObjectsEqual(listData, fixture("list-agent-profiles.json"))

        let addData = try e.encode(ControlRequest.addAgentProfile(AgentProfileInfo(
            id: "opus", base: "claude", displayName: "Claude (Opus)",
            enabled: true, args: "--model opus", builtin: false)))
        try assertJSONObjectsEqual(addData, fixture("add-agent-profile.json"))

        let updateData = try e.encode(ControlRequest.updateAgentProfile(AgentProfileInfo(
            id: "opus", base: "claude", displayName: "Claude (Opus, updated)",
            enabled: false, args: "--model opus --verbose", builtin: false)))
        try assertJSONObjectsEqual(updateData, fixture("update-agent-profile.json"))

        let removeData = try e.encode(ControlRequest.removeAgentProfile(id: "opus"))
        try assertJSONObjectsEqual(removeData, fixture("remove-agent-profile.json"))
    }

    private func assertJSONObjectsEqual(
        _ encoded: Data, _ fixtureData: Data, file: StaticString = #filePath, line: UInt = #line
    ) throws {
        let a = try JSONSerialization.jsonObject(with: encoded) as? NSDictionary
        let b = try JSONSerialization.jsonObject(with: fixtureData) as? NSDictionary
        XCTAssertNotNil(a, "encoded request did not parse as a JSON object", file: file, line: line)
        XCTAssertNotNil(b, "fixture did not parse as a JSON object", file: file, line: line)
        XCTAssertEqual(a, b, file: file, line: line)
    }

    func testAgentProfileRequestsEncodeLikeTheRustEnum() throws {
        let p = AgentProfileInfo(id: "opus", base: "claude", displayName: "Claude (Opus)",
                                 enabled: true, args: "--model opus", builtin: false)
        let add = try JSONEncoder().encode(ControlRequest.addAgentProfile(p))
        let obj = try XCTUnwrap(JSONSerialization.jsonObject(with: add) as? [String: Any])
        XCTAssertEqual(obj["type"] as? String, "addAgentProfile")
        let profile = try XCTUnwrap(obj["profile"] as? [String: Any])
        XCTAssertEqual(profile["displayName"] as? String, "Claude (Opus)")
        XCTAssertEqual(profile["builtin"] as? Bool, false)

        let list = try JSONEncoder().encode(ControlRequest.listAgentProfiles)
        XCTAssertEqual(String(decoding: list, as: UTF8.self), #"{"type":"listAgentProfiles"}"#)

        let rm = try JSONEncoder().encode(ControlRequest.removeAgentProfile(id: "opus"))
        let rmObj = try XCTUnwrap(JSONSerialization.jsonObject(with: rm) as? [String: Any])
        XCTAssertEqual(rmObj["type"] as? String, "removeAgentProfile")
        XCTAssertEqual(rmObj["id"] as? String, "opus")
    }

    func testAgentProfileListEventDecodes() throws {
        let json = #"""
        {"type":"agentProfileList","profiles":[
          {"id":"claude","base":"claude","displayName":"Claude Code","enabled":true,"args":"","builtin":true},
          {"id":"opus","base":"claude","displayName":"Claude (Opus)","enabled":false,"args":"--model opus","builtin":false}
        ]}
        """#
        let ev = try JSONDecoder().decode(ControlEvent.self, from: Data(json.utf8))
        guard case let .agentProfileList(profiles) = ev else { return XCTFail("wrong case: \(ev)") }
        XCTAssertEqual(profiles.count, 2)
        XCTAssertTrue(profiles[0].builtin)
        XCTAssertFalse(profiles[1].enabled)
        XCTAssertEqual(profiles[1].args, "--model opus")
    }
}
