import XCTest
@testable import MuxyCore

@MainActor
final class SplitNavigationTests: XCTestCase {
    private let treeJSON = #"{"type":"splitTreeChanged","agent":1,"tree":{"kind":"split","id":1,"axis":"horizontal","ratio":0.5,"first":{"kind":"leaf","pane":1},"second":{"kind":"split","id":2,"axis":"vertical","ratio":0.5,"first":{"kind":"leaf","pane":2},"second":{"kind":"leaf","pane":3}}}}"#

    private func liveModel() -> (AppModel, FakeControlTransport) {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        return (model, fake)
    }

    func testSelectingAgentRequestsTreeAndFocusesAgent() {
        let (model, fake) = liveModel()
        model.selectedPane = 1
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"getSplitTree\"") && $0.contains("\"agent\":1") })
        XCTAssertEqual(model.focusedPane, 1)
    }

    func testSplitFocusedSendsSplitPane() {
        let (model, fake) = liveModel()
        model.selectedPane = 1
        model.focusedPane = 1
        model.run(.splitRight)
        XCTAssertTrue(fake.sentLines.contains {
            $0.contains("\"type\":\"splitPane\"") && $0.contains("\"pane\":1") && $0.contains("\"direction\":\"right\"")
        })
    }

    func testFocusNextCyclesTreeLeaves() {
        let (model, fake) = liveModel()
        model.selectedPane = 1
        fake.deliver(treeJSON)              // trees[1] leaves = [1,2,3]
        model.focusedPane = 1
        model.focusNextPane(); XCTAssertEqual(model.focusedPane, 2)
        model.focusNextPane(); XCTAssertEqual(model.focusedPane, 3)
        model.focusNextPane(); XCTAssertEqual(model.focusedPane, 1)
    }

    func testCloseFocusedOnlyClosesCompanions() {
        let (model, fake) = liveModel()
        model.selectedPane = 1
        fake.deliver(treeJSON)
        model.focusedPane = 1               // the agent pane
        model.closeFocused()
        XCTAssertFalse(fake.sentLines.contains { $0.contains("\"type\":\"closePane\"") })
        model.focusedPane = 2               // a companion
        model.closeFocused()
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"closePane\"") && $0.contains("\"pane\":2") })
    }

    func testKeymapAndRegistryCarrySplitCommands() {
        let k = Keymap()
        XCTAssertEqual(k.binding(for: .splitRight), KeyBinding("d", .command))
        XCTAssertEqual(k.binding(for: .splitDown), KeyBinding("d", [.command, .shift]))
        XCTAssertEqual(k.binding(for: .closePane), KeyBinding("w", [.command, .shift]))
        XCTAssertEqual(k.binding(for: .focusNextPane), KeyBinding("]", .command))
        let ids = CommandRegistry.all(keymap: k).map(\.id)
        XCTAssertTrue([.splitRight, .splitDown, .closePane, .focusNextPane].allSatisfy { ids.contains($0) })
    }
}
