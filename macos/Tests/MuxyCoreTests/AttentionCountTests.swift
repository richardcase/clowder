import XCTest
@testable import MuxyCore

final class AttentionCountTests: XCTestCase {
    func testCountsNeedsInputAndCompletedInOrder() {
        let store = AgentStore()
        store.apply(.agentList([
            AgentInfo(pane: 1, project: "/a", task: "t1", state: .idle),
            AgentInfo(pane: 2, project: "/a", task: "t2", state: .working),
            AgentInfo(pane: 3, project: "/a", task: "t3", state: .needsInput),
            AgentInfo(pane: 4, project: "/b", task: "t4", state: .completed),
            AgentInfo(pane: 5, project: "/b", task: "t5", state: .exited),
        ]))
        XCTAssertEqual(store.attentionCount, 2)                              // needsInput + completed
        XCTAssertEqual(store.agentsNeedingAttention.map(\.pane), [3, 4])     // orderedAgents order, needy only
    }

    func testZeroWhenNoneNeedy() {
        let store = AgentStore()
        store.apply(.agentList([
            AgentInfo(pane: 1, project: "/a", task: "t", state: .working),
            AgentInfo(pane: 2, project: "/a", task: "t", state: .exited),
        ]))
        XCTAssertEqual(store.attentionCount, 0)
        XCTAssertTrue(store.agentsNeedingAttention.isEmpty)
    }
}
