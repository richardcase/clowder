import XCTest
@testable import ClowderCore

@MainActor
final class KeymapTests: XCTestCase {
    func testDefaultBindings() {
        let k = Keymap()
        XCTAssertEqual(k.binding(for: .openPalette), KeyBinding("k", .command))
        XCTAssertEqual(k.binding(for: .newWorktree), KeyBinding("n", .command))
        XCTAssertEqual(k.binding(for: .nextAttention), KeyBinding("a", [.command, .shift]))
        XCTAssertEqual(k.binding(for: .switchToAgent(1)), KeyBinding("1", .command))
        XCTAssertEqual(k.binding(for: .switchToAgent(9)), KeyBinding("9", .command))
    }

    func testOverrideWins() {
        let k = Keymap(overrides: [.newWorktree: KeyBinding("m", .command)])
        XCTAssertEqual(k.binding(for: .newWorktree), KeyBinding("m", .command))
        XCTAssertEqual(k.binding(for: .openPalette), KeyBinding("k", .command)) // default untouched
    }

    func testRegistryRows() {
        let rows = CommandRegistry.all(keymap: Keymap())
        XCTAssertEqual(rows.map(\.id), [
            .newWorktree, .addProject, .restartWorktree, .nextAttention,
            .splitRight, .splitDown, .closePane, .focusNextPane, .landAgent, .discardAgent,
        ])
        XCTAssertEqual(rows[0].title, "New Worktree")
        XCTAssertEqual(rows[0].defaultShortcut, KeyBinding("n", .command))
    }

    func testNewWorktreeKeepsCmdNAndAddProjectTakesCmdShiftN() {
        let k = Keymap()
        XCTAssertEqual(k.binding(for: .newWorktree), KeyBinding("n", .command))
        XCTAssertEqual(k.binding(for: .addProject), KeyBinding("n", [.command, .shift]))
    }

    func testCommandRegistryListsBothCreationCommandsAndRestart() {
        let titles = CommandRegistry.all(keymap: Keymap()).map(\.title)
        XCTAssertTrue(titles.contains("New Worktree"), "\(titles)")
        XCTAssertTrue(titles.contains("Add Project"), "\(titles)")
        XCTAssertTrue(titles.contains("Restart Agent"), "\(titles)")
        XCTAssertFalse(titles.contains("Spawn Agent"), "renamed: \(titles)")
    }

    func testRestartIsDisabledUnlessAnExitedWorktreeIsSelected() {
        let m = makeModel()
        m.store.apply(.projectList([ProjectInfo(path: "/p", name: "p", kind: "git")]))
        m.store.apply(.worktreeList([
            WorktreeInfo(pane: 1, project: "/p", name: "a", branch: "clowder/a", state: .working),
        ]))
        XCTAssertFalse(m.isEnabled(.restartWorktree), "nothing selected")
        m.selection = .worktree(1)
        XCTAssertFalse(m.isEnabled(.restartWorktree), "agent is alive")
        m.store.apply(.attentionChanged(pane: 1, state: .exited))
        XCTAssertTrue(m.isEnabled(.restartWorktree))
        XCTAssertTrue(m.isEnabled(.newWorktree), "New Worktree is always available")
        XCTAssertTrue(m.isEnabled(.addProject))
    }

    func testLandAndDiscardAreDisabledUnderAProjectSelection() {
        let m = makeModel()
        m.store.apply(.projectAdded(ProjectInfo(path: "/p", name: "p", kind: "git")))
        m.selection = .project("/p")
        XCTAssertFalse(m.isEnabled(.landAgent))
        XCTAssertFalse(m.isEnabled(.discardAgent))
    }

    func testRunNewWorktreePrefillsProjectFromSelectedWorktree() {
        let m = makeModel()
        m.store.apply(.projectList([ProjectInfo(path: "/p", name: "p", kind: "git")]))
        m.store.apply(.worktreeList([
            WorktreeInfo(pane: 1, project: "/p", name: "a", branch: "clowder/a", state: .working),
        ]))
        m.selection = .worktree(1)
        m.run(.newWorktree)
        XCTAssertEqual(m.newWorktreeProject, "/p")
        XCTAssertTrue(m.showingNewWorktree)
    }

    func testRunNewWorktreePrefillsProjectFromProjectSelection() {
        let m = makeModel()
        m.store.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        m.selection = .project("/code/alpha")
        m.run(.newWorktree)
        XCTAssertEqual(m.newWorktreeProject, "/code/alpha")
    }

    func testClosePaneIsEnabledOnlyForAFocusedCompanion() {
        let m = makeModel()
        m.store.apply(.projectList([ProjectInfo(path: "/p", name: "p", kind: "git")]))
        m.store.apply(.worktreeList([
            WorktreeInfo(pane: 1, project: "/p", name: "a", branch: "clowder/a", state: .working),
        ]))
        m.selection = .worktree(1)

        m.focusedPane = nil
        XCTAssertFalse(m.isEnabled(.closePane), "nothing focused")

        m.focusedPane = 1
        XCTAssertFalse(m.isEnabled(.closePane), "focused pane IS the selection root — not a companion")

        m.focusedPane = 2
        XCTAssertTrue(m.isEnabled(.closePane), "a focused companion is closable")
    }

    private func makeModel() -> AppModel {
        AppModel(makeTransport: { FakeControlTransport() })
    }
}
