import XCTest
@testable import ClowderCore

@MainActor
final class NavigationTests: XCTestCase {
    private func modelWithAgents() -> AppModel {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        // `orderedWorktrees` now requires each worktree's project to be registered (M10c) —
        // register /a and /b before feeding the worktree list.
        fake.deliver(#"{"type":"projectList","projects":[{"path":"/a","name":"a","kind":"git"},{"path":"/b","name":"b","kind":"git"}]}"#)
        fake.deliver(#"{"type":"worktreeList","worktrees":[{"pane":1,"project":"/a","name":"t1","branch":"clowder/t1","state":"Working"},{"pane":2,"project":"/a","name":"t2","branch":"clowder/t2","state":"NeedsInput"},{"pane":3,"project":"/b","name":"t3","branch":"clowder/t3","state":"NeedsInput"}]}"#)
        return model
    }

    func testOrderedAgentsIsByProjectFlatten() {
        let m = modelWithAgents()
        XCTAssertEqual(m.store.orderedWorktrees.map(\.pane), [1, 2, 3]) // /a (pane 1,2) then /b (pane 3)
    }

    func testSelectAgentAtIndexIsOneBasedAndBounded() {
        let m = modelWithAgents()
        m.selectAgent(atIndex: 2)
        XCTAssertEqual(m.selectedPane, 2)
        m.selectAgent(atIndex: 99)      // out of range -> unchanged
        XCTAssertEqual(m.selectedPane, 2)
        m.selectAgent(atIndex: 0)       // 0 invalid (1-based) -> unchanged
        XCTAssertEqual(m.selectedPane, 2)
    }

    func testNextAttentionCyclesNeedyOnly() {
        let m = modelWithAgents()
        m.selectNextAttention()                     // nothing selected -> first needy (2)
        XCTAssertEqual(m.selectedPane, 2)
        m.selectNextAttention()                     // -> next needy (3)
        XCTAssertEqual(m.selectedPane, 3)
        m.selectNextAttention()                     // cycle -> 2
        XCTAssertEqual(m.selectedPane, 2)
    }

    func testNextAttentionNoOpWhenNoneNeedy() {
        let fake = FakeControlTransport()
        let m = AppModel(makeTransport: { fake })
        m.connect()
        fake.deliver(#"{"type":"projectList","projects":[{"path":"/a","name":"a","kind":"git"}]}"#)
        fake.deliver(#"{"type":"worktreeList","worktrees":[{"pane":1,"project":"/a","name":"t","branch":"clowder/t","state":"Working"}]}"#)
        m.selectedPane = 1
        m.selectNextAttention()
        XCTAssertEqual(m.selectedPane, 1)           // unchanged
    }

    func testRunDispatch() {
        let m = modelWithAgents()
        m.run(.openPalette); XCTAssertTrue(m.showingPalette)
        m.run(.openPalette); XCTAssertFalse(m.showingPalette)   // toggles
        m.run(.spawnAgent); XCTAssertTrue(m.showingSpawn)
        m.run(.switchToAgent(1)); XCTAssertEqual(m.selectedPane, 1)
        m.run(.nextAttention); XCTAssertEqual(m.selectedPane, 2) // first needy after pane 1
    }
}
