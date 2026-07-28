import XCTest
@testable import MuxyCore

@MainActor
final class DividerFocusTests: XCTestCase {
    private func liveModel() -> (AppModel, FakeControlTransport) {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        return (model, fake)
    }
    // agent 1 tree with leaves [1,2,3]
    private let tree123 = #"{"type":"splitTreeChanged","agent":1,"tree":{"kind":"split","id":1,"axis":"horizontal","ratio":0.5,"first":{"kind":"leaf","pane":1},"second":{"kind":"split","id":2,"axis":"vertical","ratio":0.5,"first":{"kind":"leaf","pane":2},"second":{"kind":"leaf","pane":3}}}}"#
    // agent 1 tree with leaves [1,3] (pane 2 gone)
    private let tree13 = #"{"type":"splitTreeChanged","agent":1,"tree":{"kind":"split","id":1,"axis":"horizontal","ratio":0.5,"first":{"kind":"leaf","pane":1},"second":{"kind":"leaf","pane":3}}}"#

    func testSetDividerRatioClampsAndSends() {
        let (model, fake) = liveModel()
        model.setDividerRatio(split: 5, ratio: 2.0)          // clamps to 0.95
        XCTAssertTrue(fake.sentLines.contains {
            $0.contains("\"type\":\"setSplitRatio\"") && $0.contains("\"split\":5") && $0.contains("0.95")
        })
        model.setDividerRatio(split: 5, ratio: -1.0)         // clamps to 0.05
        XCTAssertTrue(fake.sentLines.contains { $0.contains("0.05") })
    }

    func testReconcileFocusResetsWhenLeafGone() {
        let (model, fake) = liveModel()
        model.selectedPane = 1
        fake.deliver(tree123)
        model.focusedPane = 2
        fake.deliver(tree13)            // pane 2 no longer a leaf
        model.reconcileFocus()
        XCTAssertEqual(model.focusedPane, 1)   // reset to the agent (selectedPane)
    }

    func testReconcileFocusKeepsValidFocus() {
        let (model, fake) = liveModel()
        model.selectedPane = 1
        fake.deliver(tree123)
        model.focusedPane = 2
        model.reconcileFocus()
        XCTAssertEqual(model.focusedPane, 2)   // still a leaf → unchanged
    }
}
