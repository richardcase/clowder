import XCTest
@testable import MuxyCore

final class AgentStoreTests: XCTestCase {
    func testAgentListReplacesAndClearsRefresh() {
        let s = AgentStore()
        s.apply(.agentSpawned(pane: 1))
        XCTAssertTrue(s.needsRefresh)
        s.apply(.agentList([AgentInfo(pane: 1, project: "p", task: "t", state: .working)]))
        XCTAssertFalse(s.needsRefresh)
        XCTAssertEqual(s.agents.count, 1)
        XCTAssertEqual(s.agents[1]?.task, "t")
    }

    func testAttentionChangedUpdatesKnownPane() {
        let s = AgentStore()
        s.apply(.agentList([AgentInfo(pane: 1, project: "p", task: "t", state: .working)]))
        s.apply(.attentionChanged(pane: 1, state: .needsInput))
        XCTAssertEqual(s.agents[1]?.state, .needsInput)
        XCTAssertFalse(s.needsRefresh)
    }

    func testAttentionChangedUnknownPaneTriggersRefresh() {
        let s = AgentStore()
        s.apply(.attentionChanged(pane: 99, state: .working))
        XCTAssertTrue(s.needsRefresh)
        XCTAssertNil(s.agents[99])
    }

    func testAgentRemovedIsIdempotent() {
        let s = AgentStore()
        s.apply(.agentList([AgentInfo(pane: 1, project: "p", task: "t", state: .working)]))
        s.apply(.agentRemoved(pane: 1))
        XCTAssertNil(s.agents[1])
        s.apply(.agentRemoved(pane: 1)) // no crash, still absent
        XCTAssertNil(s.agents[1])
    }

    func testByProjectGroupsAndSorts() {
        let s = AgentStore()
        s.apply(.agentList([
            AgentInfo(pane: 3, project: "b", task: "t", state: .working),
            AgentInfo(pane: 1, project: "a", task: "t", state: .working),
            AgentInfo(pane: 2, project: "a", task: "t", state: .working),
        ]))
        let bp = s.byProject
        XCTAssertEqual(bp.map { $0.project }, ["a", "b"])
        XCTAssertEqual(bp[0].agents.map { $0.pane }, [1, 2])
    }

    func testErrorEventSetsLastError() {
        let s = AgentStore()
        XCTAssertNil(s.lastError)
        s.apply(.error(message: "boom"))
        XCTAssertEqual(s.lastError, "boom")
    }
}
