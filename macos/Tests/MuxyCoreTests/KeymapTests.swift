import XCTest
@testable import MuxyCore

final class KeymapTests: XCTestCase {
    func testDefaultBindings() {
        let k = Keymap()
        XCTAssertEqual(k.binding(for: .openPalette), KeyBinding("k", .command))
        XCTAssertEqual(k.binding(for: .spawnAgent), KeyBinding("n", .command))
        XCTAssertEqual(k.binding(for: .nextAttention), KeyBinding("a", [.command, .shift]))
        XCTAssertEqual(k.binding(for: .switchToAgent(1)), KeyBinding("1", .command))
        XCTAssertEqual(k.binding(for: .switchToAgent(9)), KeyBinding("9", .command))
    }

    func testOverrideWins() {
        let k = Keymap(overrides: [.spawnAgent: KeyBinding("m", .command)])
        XCTAssertEqual(k.binding(for: .spawnAgent), KeyBinding("m", .command))
        XCTAssertEqual(k.binding(for: .openPalette), KeyBinding("k", .command)) // default untouched
    }

    func testRegistryRows() {
        let rows = CommandRegistry.all(keymap: Keymap())
        XCTAssertEqual(rows.map(\.id), [.spawnAgent, .nextAttention, .splitRight, .splitDown, .closePane, .focusNextPane])
        XCTAssertEqual(rows[0].title, "Spawn Agent")
        XCTAssertEqual(rows[0].defaultShortcut, KeyBinding("n", .command))
    }
}
