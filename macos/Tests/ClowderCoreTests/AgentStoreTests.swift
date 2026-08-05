import XCTest
@testable import ClowderCore

final class AgentStoreTests: XCTestCase {
    func testAgentListReplacesAndClearsRefresh() {
        let s = AgentStore()
        s.apply(.agentSpawned(pane: 1))
        XCTAssertTrue(s.needsRefresh)
        s.apply(.worktreeList([WorktreeInfo(pane: 1, project: "p", name: "t", branch: "clowder/t", state: .working)]))
        XCTAssertFalse(s.needsRefresh)
        XCTAssertEqual(s.worktrees.count, 1)
        XCTAssertEqual(s.worktrees[1]?.name, "t")
    }

    func testAttentionChangedUpdatesKnownPane() {
        let s = AgentStore()
        s.apply(.worktreeList([WorktreeInfo(pane: 1, project: "p", name: "t", branch: "clowder/t", state: .working)]))
        s.apply(.attentionChanged(pane: 1, state: .needsInput))
        XCTAssertEqual(s.worktrees[1]?.state, .needsInput)
        XCTAssertFalse(s.needsRefresh)
    }

    func testAttentionChangedUnknownPaneTriggersRefresh() {
        let s = AgentStore()
        s.apply(.attentionChanged(pane: 99, state: .working))
        XCTAssertTrue(s.needsRefresh)
        XCTAssertNil(s.worktrees[99])
    }

    func testAgentRemovedIsIdempotent() {
        let s = AgentStore()
        s.apply(.worktreeList([WorktreeInfo(pane: 1, project: "p", name: "t", branch: "clowder/t", state: .working)]))
        s.apply(.agentRemoved(pane: 1))
        XCTAssertNil(s.worktrees[1])
        s.apply(.agentRemoved(pane: 1)) // no crash, still absent
        XCTAssertNil(s.worktrees[1])
    }

    func testSidebarGroupsAndSorts() {
        // Renamed from testByProjectGroupsAndSorts: `byProject` grouped by the worktree's raw
        // `project` string with no registration required. `sidebar` requires the project to be
        // registered first (the "fresh start" invariant), so this now registers "a"/"b" before
        // asserting the same grouping/sort behavior via `sidebar`.
        let s = AgentStore()
        s.apply(.projectList([
            ProjectInfo(path: "a", name: "a", kind: "git"),
            ProjectInfo(path: "b", name: "b", kind: "git"),
        ]))
        s.apply(.worktreeList([
            WorktreeInfo(pane: 3, project: "b", name: "t", branch: "clowder/t", state: .working),
            WorktreeInfo(pane: 1, project: "a", name: "t", branch: "clowder/t", state: .working),
            WorktreeInfo(pane: 2, project: "a", name: "t", branch: "clowder/t", state: .working),
        ]))
        let sb = s.sidebar
        XCTAssertEqual(sb.map { $0.path }, ["a", "b"])
        XCTAssertEqual(sb[0].worktrees.map { $0.pane }, [1, 2])
    }

    func testErrorEventSetsLastError() {
        let s = AgentStore()
        XCTAssertNil(s.lastError)
        s.apply(.error(message: "boom"))
        XCTAssertEqual(s.lastError, "boom")
    }

    // MARK: - Project state (M10c)

    private func wt(_ pane: UInt64, _ project: String, _ name: String,
                    _ state: AttentionState = .working) -> WorktreeInfo {
        WorktreeInfo(pane: pane, project: project, name: name,
                     branch: "clowder/\(name)", state: state)
    }

    func testSidebarGroupsWorktreesUnderTheirProject() {
        let s = AgentStore()
        s.apply(.projectList([
            ProjectInfo(path: "/code/beta", name: "beta", kind: "jj"),
            ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git"),
        ]))
        s.apply(.worktreeList([
            wt(3, "/code/alpha", "c"), wt(1, "/code/alpha", "a"), wt(2, "/code/beta", "b"),
        ]))
        let sidebar = s.sidebar
        XCTAssertEqual(sidebar.map(\.name), ["alpha", "beta"], "projects sort by name")
        XCTAssertEqual(sidebar[0].kind, "git")
        XCTAssertEqual(sidebar[0].worktrees.map(\.pane), [1, 3], "worktrees sort by pane")
        XCTAssertEqual(sidebar[1].worktrees.map(\.pane), [2])
    }

    func testSidebarOmitsWorktreesWithNoRegisteredProject() {
        let s = AgentStore()
        s.apply(.projectList([ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")]))
        s.apply(.worktreeList([wt(1, "/code/alpha", "a"), wt(2, "/code/orphan", "b")]))
        XCTAssertEqual(s.sidebar.count, 1)
        XCTAssertEqual(s.orderedWorktrees.map(\.pane), [1], "orphans are omitted from the order too")
        XCTAssertEqual(s.attentionCount, 0)
    }

    func testProjectAttentionCountRollsUpItsOwnWorktreesOnly() {
        let s = AgentStore()
        s.apply(.projectList([
            ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git"),
            ProjectInfo(path: "/code/beta", name: "beta", kind: "git"),
        ]))
        s.apply(.worktreeList([
            wt(1, "/code/alpha", "a", .needsInput),
            wt(2, "/code/alpha", "b", .completed),
            wt(3, "/code/alpha", "c", .working),
            wt(4, "/code/beta", "d", .needsInput),
        ]))
        XCTAssertEqual(s.sidebar[0].attentionCount, 2, "needsInput + completed, alpha only")
        XCTAssertEqual(s.sidebar[1].attentionCount, 1)
        XCTAssertEqual(s.attentionCount, 3, "global count is the sum")
    }

    func testProjectAddedAndRemovedMutateTheList() {
        let s = AgentStore()
        s.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        XCTAssertEqual(s.sidebar.map(\.path), ["/code/alpha"])
        s.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        XCTAssertEqual(s.sidebar.count, 1, "projectAdded is idempotent — it arrives twice by design")
        s.apply(.projectRemoved(path: "/code/alpha"))
        XCTAssertTrue(s.sidebar.isEmpty)
    }

    func testProjectTerminalMappingIsTrackedAndCleared() {
        let s = AgentStore()
        s.apply(.projectTerminalOpened(path: "/code/alpha", pane: 7))
        XCTAssertEqual(s.projectTerminals["/code/alpha"], 7)
        s.apply(.projectTerminalClosed(path: "/code/alpha"))
        XCTAssertNil(s.projectTerminals["/code/alpha"])
    }

    func testRemovingAProjectDropsItsTerminalMapping() {
        let s = AgentStore()
        s.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        s.apply(.projectTerminalOpened(path: "/code/alpha", pane: 7))
        s.apply(.projectRemoved(path: "/code/alpha"))
        XCTAssertNil(s.projectTerminals["/code/alpha"], "a removed project's terminal is gone")
    }

    func testResetClearsProjectState() {
        let s = AgentStore()
        s.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        s.apply(.projectTerminalOpened(path: "/code/alpha", pane: 7))
        s.reset()
        XCTAssertTrue(s.projects.isEmpty)
        XCTAssertTrue(s.projectTerminals.isEmpty)
    }
}
