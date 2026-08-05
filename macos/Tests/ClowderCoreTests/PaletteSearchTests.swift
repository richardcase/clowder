import XCTest
@testable import ClowderCore

final class PaletteSearchTests: XCTestCase {
    private let cmds = CommandRegistry.all(keymap: Keymap())
    private func worktrees() -> [WorktreeInfo] {
        [WorktreeInfo(pane: 1, project: "/home/api", name: "fix login", branch: "clowder/fix login", state: .working),
         WorktreeInfo(pane: 2, project: "/home/web", name: "spawn worker", branch: "clowder/spawn worker", state: .idle)]
    }
    private func isCommand(_ i: PaletteItem) -> Bool { if case .command = i.kind { return true }; return false }
    private func isAgent(_ i: PaletteItem, _ pane: UInt64) -> Bool {
        if case let .agent(p) = i.kind { return p == pane }; return false
    }

    func testEmptyQueryReturnsAllCommandsThenAgents() {
        let r = paletteResults(query: "", commands: cmds, worktrees: worktrees())
        XCTAssertEqual(r.count, cmds.count + 2)
        XCTAssertTrue(r.prefix(cmds.count).allSatisfy(isCommand))
        XCTAssertTrue(r.suffix(2).allSatisfy { !isCommand($0) })
    }

    func testCommandQueryRanksCommandFirst() {
        let r = paletteResults(query: "new work", commands: cmds, worktrees: worktrees())
        XCTAssertEqual(r.first?.title, "New Worktree")
        XCTAssertTrue(isCommand(r[0]))
    }

    func testAgentQueryMatchesOnlyAgent() {
        let r = paletteResults(query: "login", commands: cmds, worktrees: worktrees())
        XCTAssertTrue(r.contains { isAgent($0, 1) })
        XCTAssertFalse(r.contains { isCommand($0) })
    }

    func testNoMatchIsEmpty() {
        XCTAssertTrue(paletteResults(query: "zzzzz", commands: cmds, worktrees: worktrees()).isEmpty)
    }
}
