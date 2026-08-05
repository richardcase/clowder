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

    func testByProjectGroupsAndSorts() {
        let s = AgentStore()
        s.apply(.worktreeList([
            WorktreeInfo(pane: 3, project: "b", name: "t", branch: "clowder/t", state: .working),
            WorktreeInfo(pane: 1, project: "a", name: "t", branch: "clowder/t", state: .working),
            WorktreeInfo(pane: 2, project: "a", name: "t", branch: "clowder/t", state: .working),
        ]))
        let bp = s.byProject
        XCTAssertEqual(bp.map { $0.project }, ["a", "b"])
        XCTAssertEqual(bp[0].worktrees.map { $0.pane }, [1, 2])
    }

    func testErrorEventSetsLastError() {
        let s = AgentStore()
        XCTAssertNil(s.lastError)
        s.apply(.error(message: "boom"))
        XCTAssertEqual(s.lastError, "boom")
    }
}
