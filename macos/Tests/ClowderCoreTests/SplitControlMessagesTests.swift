// SPDX-License-Identifier: Apache-2.0

import XCTest
@testable import ClowderCore

final class SplitControlMessagesTests: XCTestCase {
    private func obj(_ req: ControlRequest) throws -> [String: Any] {
        try JSONSerialization.jsonObject(with: JSONEncoder().encode(req)) as! [String: Any]
    }

    func testSplitPaneEncodes() throws {
        let o = try obj(.splitPane(pane: 3, direction: .right))
        XCTAssertEqual(o["type"] as? String, "splitPane")
        XCTAssertEqual(o["pane"] as? Int, 3)
        XCTAssertEqual(o["direction"] as? String, "right")
    }

    func testCloseGetRatioEncode() throws {
        XCTAssertEqual(try obj(.closePane(pane: 5))["type"] as? String, "closePane")
        XCTAssertEqual(try obj(.closePane(pane: 5))["pane"] as? Int, 5)
        XCTAssertEqual(try obj(.getSplitTree(agent: 1))["type"] as? String, "getSplitTree")
        XCTAssertEqual(try obj(.getSplitTree(agent: 1))["agent"] as? Int, 1)
        XCTAssertEqual(try obj(.setSplitRatio(split: 2, ratio: 0.4))["type"] as? String, "setSplitRatio")
    }

    func testSplitTreeChangedDecodes() throws {
        let json = #"{"type":"splitTreeChanged","agent":1,"tree":{"kind":"split","id":1,"axis":"horizontal","ratio":0.5,"first":{"kind":"leaf","pane":1},"second":{"kind":"leaf","pane":2}}}"#
        let e = try JSONDecoder().decode(ControlEvent.self, from: Data(json.utf8))
        guard case let .splitTreeChanged(agent, tree) = e else { return XCTFail("expected splitTreeChanged") }
        XCTAssertEqual(agent, 1)
        XCTAssertEqual(tree.leaves, [1, 2])
    }
}
