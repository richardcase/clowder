import XCTest
@testable import MuxyCore

final class AgentStoreTreesTests: XCTestCase {
    private func tree() throws -> PaneTree {
        try JSONDecoder().decode(PaneTree.self, from: Data(
            #"{"kind":"split","id":1,"axis":"horizontal","ratio":0.5,"first":{"kind":"leaf","pane":1},"second":{"kind":"leaf","pane":2}}"#.utf8))
    }

    func testSplitTreeChangedStoresTreeIdempotently() throws {
        let store = AgentStore()
        let t = try tree()
        store.apply(.splitTreeChanged(agent: 1, tree: t))
        XCTAssertEqual(store.trees[1], t)
        store.apply(.splitTreeChanged(agent: 1, tree: t))   // same event twice → one tree
        XCTAssertEqual(store.trees.count, 1)
        XCTAssertEqual(store.trees[1], t)
    }

    func testAgentRemovedClearsTree() throws {
        let store = AgentStore()
        store.apply(.splitTreeChanged(agent: 1, tree: try tree()))
        store.apply(.agentRemoved(pane: 1))
        XCTAssertNil(store.trees[1])
    }
}
