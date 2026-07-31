import XCTest
@testable import ClowderCore

final class PaletteSearchTests: XCTestCase {
    private let cmds = CommandRegistry.all(keymap: Keymap())
    private func agents() -> [AgentInfo] {
        [AgentInfo(pane: 1, project: "/home/api", task: "fix login", state: .working),
         AgentInfo(pane: 2, project: "/home/web", task: "spawn worker", state: .idle)]
    }
    private func isCommand(_ i: PaletteItem) -> Bool { if case .command = i.kind { return true }; return false }
    private func isAgent(_ i: PaletteItem, _ pane: UInt64) -> Bool {
        if case let .agent(p) = i.kind { return p == pane }; return false
    }

    func testEmptyQueryReturnsAllCommandsThenAgents() {
        let r = paletteResults(query: "", commands: cmds, agents: agents())
        XCTAssertEqual(r.count, cmds.count + 2)
        XCTAssertTrue(r.prefix(cmds.count).allSatisfy(isCommand))
        XCTAssertTrue(r.suffix(2).allSatisfy { !isCommand($0) })
    }

    func testCommandQueryRanksCommandFirst() {
        let r = paletteResults(query: "spawn ag", commands: cmds, agents: agents())
        XCTAssertEqual(r.first?.title, "Spawn Agent")
        XCTAssertTrue(isCommand(r[0]))
    }

    func testAgentQueryMatchesOnlyAgent() {
        let r = paletteResults(query: "login", commands: cmds, agents: agents())
        XCTAssertTrue(r.contains { isAgent($0, 1) })
        XCTAssertFalse(r.contains { isCommand($0) })
    }

    func testNoMatchIsEmpty() {
        XCTAssertTrue(paletteResults(query: "zzzzz", commands: cmds, agents: agents()).isEmpty)
    }
}
