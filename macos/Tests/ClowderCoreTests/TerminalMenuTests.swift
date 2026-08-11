import XCTest
@testable import ClowderCore

final class TerminalMenuTests: XCTestCase {
    private func menu(hasSelection: Bool = true,
                      pasteboardHasText: Bool = true,
                      canClosePane: Bool = true) -> [TerminalMenuItem?] {
        TerminalMenu.contextMenu(hasSelection: hasSelection,
                                 pasteboardHasText: pasteboardHasText,
                                 canClosePane: canClosePane)
    }

    private func item(_ items: [TerminalMenuItem?], _ action: TerminalMenuAction) -> TerminalMenuItem? {
        items.compactMap { $0 }.first { $0.action == action }
    }

    func testShapeAndOrder() {
        let items = menu()
        XCTAssertEqual(items.map { $0?.action }, [
            .copy, .paste, .selectAll,
            nil,                                  // separator
            .command(.splitRight), .command(.splitDown), .command(.closePane),
        ])
        XCTAssertEqual(items.compactMap { $0 }.map(\.title),
                       ["Copy", "Paste", "Select All", "Split Right", "Split Down", "Close Pane"])
    }

    // A terminal's scrollback is not editable, so there is deliberately nothing to cut.
    func testHasNoCutItem() {
        XCTAssertFalse(menu().compactMap { $0 }.contains { $0.title.lowercased().contains("cut") })
    }

    func testCopyNeedsASelection() {
        XCTAssertEqual(item(menu(hasSelection: true), .copy)?.isEnabled, true)
        XCTAssertEqual(item(menu(hasSelection: false), .copy)?.isEnabled, false)
    }

    func testPasteNeedsSomethingOnThePasteboard() {
        XCTAssertEqual(item(menu(pasteboardHasText: true), .paste)?.isEnabled, true)
        XCTAssertEqual(item(menu(pasteboardHasText: false), .paste)?.isEnabled, false)
    }

    func testClosePaneFollowsTheModel() {
        XCTAssertEqual(item(menu(canClosePane: true), .command(.closePane))?.isEnabled, true)
        XCTAssertEqual(item(menu(canClosePane: false), .command(.closePane))?.isEnabled, false)
    }

    // Select All and the splits always apply — nothing about the surface can forbid them.
    func testAlwaysAvailableItemsStayEnabled() {
        let items = menu(hasSelection: false, pasteboardHasText: false, canClosePane: false)
        XCTAssertEqual(item(items, .selectAll)?.isEnabled, true)
        XCTAssertEqual(item(items, .command(.splitRight))?.isEnabled, true)
        XCTAssertEqual(item(items, .command(.splitDown))?.isEnabled, true)
    }

    // The split/close titles come from the palette registry; a rename there must not silently
    // drop the item from this menu.
    func testTitlesComeFromTheCommandRegistry() {
        for id in [CommandID.splitRight, .splitDown, .closePane] {
            XCTAssertNotNil(TerminalMenu.registryTitle(for: id), "\(id) is missing from CommandRegistry")
        }
        let registry = CommandRegistry.all(keymap: Keymap())
        XCTAssertEqual(item(menu(), .command(.splitRight))?.title,
                       registry.first { $0.id == .splitRight }?.title)
    }
}
