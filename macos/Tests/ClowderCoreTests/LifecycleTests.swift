import XCTest
@testable import ClowderCore

@MainActor
final class LifecycleTests: XCTestCase {
    private func modelWithAgent() -> (AppModel, FakeControlTransport) {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        fake.deliver(#"{"type":"worktreeList","worktrees":[{"pane":1,"project":"/p","name":"fix-bug","branch":"clowder/fix-bug","state":"Completed"}]}"#)
        model.selection = .worktree(1)
        return (model, fake)
    }

    func testLandEncodes() throws {
        let o = try JSONSerialization.jsonObject(with: JSONEncoder().encode(ControlRequest.landAgent(pane: 3))) as! [String: Any]
        XCTAssertEqual(o["type"] as? String, "landAgent")
        XCTAssertEqual(o["pane"] as? Int, 3)
        let d = try JSONSerialization.jsonObject(with: JSONEncoder().encode(ControlRequest.discardAgent(pane: 4))) as! [String: Any]
        XCTAssertEqual(d["type"] as? String, "discardAgent")
        XCTAssertEqual(d["pane"] as? Int, 4)
    }

    func testRunLandSetsPendingConfirmationAndDoesNotSend() {
        let (model, fake) = modelWithAgent()
        model.run(.landAgent)
        XCTAssertEqual(model.pendingLifecycle, PendingLifecycle(action: .land, pane: 1, name: "fix-bug"))
        XCTAssertFalse(fake.sentLines.contains { $0.contains("\"type\":\"landAgent\"") }, "must not send before confirm")
    }

    func testConfirmSendsLandThenClears() {
        let (model, fake) = modelWithAgent()
        model.run(.landAgent)
        model.confirmLifecycle()
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"landAgent\"") && $0.contains("\"pane\":1") })
        XCTAssertNil(model.pendingLifecycle)
    }

    func testDiscardFlow() {
        let (model, fake) = modelWithAgent()
        model.run(.discardAgent)
        XCTAssertEqual(model.pendingLifecycle?.action, .discard)
        model.confirmLifecycle()
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"discardAgent\"") && $0.contains("\"pane\":1") })
    }

    func testCancelClearsWithoutSending() {
        let (model, fake) = modelWithAgent()
        model.run(.discardAgent)
        model.cancelLifecycle()
        XCTAssertNil(model.pendingLifecycle)
        XCTAssertFalse(fake.sentLines.contains { $0.contains("discardAgent") })
    }

    func testNoSelectionNoPending() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()   // selectedPane is nil
        model.run(.landAgent)
        XCTAssertNil(model.pendingLifecycle)
    }

    func testKeymapAndRegistryCarryLifecycle() {
        XCTAssertEqual(Keymap().binding(for: .landAgent), KeyBinding("l", .command))
        let ids = CommandRegistry.all(keymap: Keymap()).map(\.id)
        XCTAssertTrue(ids.contains(.landAgent) && ids.contains(.discardAgent))
    }

    func testConfirmUsesCapturedPaneNotCurrentSelection() {
        let (model, fake) = modelWithAgent()   // agent at pane 1, selectedPane == 1
        model.run(.landAgent)                  // captures pane 1 into pendingLifecycle
        model.selection = .worktree(2)         // selection moves after the request
        model.confirmLifecycle()
        // must send to the CAPTURED pane (1), not the current selection (2)
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"landAgent\"") && $0.contains("\"pane\":1") })
        XCTAssertFalse(fake.sentLines.contains { $0.contains("\"pane\":2") })
    }
}
