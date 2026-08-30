// SPDX-License-Identifier: Apache-2.0

import XCTest
@testable import ClowderCore

final class AttentionCountTests: XCTestCase {
    func testCountsNeedsInputAndCompletedInOrder() {
        let store = AgentStore()
        // `attentionCount`/`worktreesNeedingAttention` derive from `orderedWorktrees`, which now
        // requires each worktree's project to be registered (M10c) — register /a and /b first.
        store.apply(.projectList([
            ProjectInfo(path: "/a", name: "a", kind: "git"),
            ProjectInfo(path: "/b", name: "b", kind: "git"),
        ]))
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
        // Register /a first — otherwise these worktrees are excluded by the unregistered-project
        // rule before the state filter even runs, and this test would pass even if that filter
        // were inverted.
        store.apply(.projectList([ProjectInfo(path: "/a", name: "a", kind: "git")]))
        store.apply(.worktreeList([
            WorktreeInfo(pane: 1, project: "/a", name: "t", branch: "clowder/t", state: .working),
            WorktreeInfo(pane: 2, project: "/a", name: "t", branch: "clowder/t", state: .exited),
        ]))
        XCTAssertEqual(store.attentionCount, 0)
        XCTAssertTrue(store.worktreesNeedingAttention.isEmpty)
    }
}
