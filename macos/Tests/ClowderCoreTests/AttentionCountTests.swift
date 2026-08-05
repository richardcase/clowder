import XCTest
@testable import ClowderCore

final class AttentionCountTests: XCTestCase {
    func testCountsNeedsInputAndCompletedInOrder() {
        let store = AgentStore()
        store.apply(.worktreeList([
            WorktreeInfo(pane: 1, project: "/a", name: "t1", branch: "clowder/t1", state: .idle),
            WorktreeInfo(pane: 2, project: "/a", name: "t2", branch: "clowder/t2", state: .working),
            WorktreeInfo(pane: 3, project: "/a", name: "t3", branch: "clowder/t3", state: .needsInput),
            WorktreeInfo(pane: 4, project: "/b", name: "t4", branch: "clowder/t4", state: .completed),
            WorktreeInfo(pane: 5, project: "/b", name: "t5", branch: "clowder/t5", state: .exited),
        ]))
        XCTAssertEqual(store.attentionCount, 2)                                   // needsInput + completed
        XCTAssertEqual(store.worktreesNeedingAttention.map(\.pane), [3, 4])       // orderedWorktrees order, needy only
    }

    func testZeroWhenNoneNeedy() {
        let store = AgentStore()
        store.apply(.worktreeList([
            WorktreeInfo(pane: 1, project: "/a", name: "t", branch: "clowder/t", state: .working),
            WorktreeInfo(pane: 2, project: "/a", name: "t", branch: "clowder/t", state: .exited),
        ]))
        XCTAssertEqual(store.attentionCount, 0)
        XCTAssertTrue(store.worktreesNeedingAttention.isEmpty)
    }
}
