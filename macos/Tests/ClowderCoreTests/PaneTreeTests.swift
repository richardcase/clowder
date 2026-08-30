// SPDX-License-Identifier: Apache-2.0

import XCTest
@testable import ClowderCore

final class PaneTreeTests: XCTestCase {
    func testDecodeLeaf() throws {
        let t = try JSONDecoder().decode(PaneTree.self, from: Data(#"{"kind":"leaf","pane":7}"#.utf8))
        XCTAssertEqual(t, .leaf(pane: 7))
        XCTAssertEqual(t.leaves, [7])
    }

    func testDecodeNestedSplitInOrder() throws {
        let json = #"{"kind":"split","id":1,"axis":"horizontal","ratio":0.5,"first":{"kind":"leaf","pane":1},"second":{"kind":"split","id":2,"axis":"vertical","ratio":0.3,"first":{"kind":"leaf","pane":2},"second":{"kind":"leaf","pane":3}}}"#
        let t = try JSONDecoder().decode(PaneTree.self, from: Data(json.utf8))
        XCTAssertEqual(t.leaves, [1, 2, 3])
        guard case let .split(id, axis, ratio, first, second) = t else { return XCTFail("expected split") }
        XCTAssertEqual(id, 1)
        XCTAssertEqual(axis, .horizontal)
        XCTAssertEqual(ratio, 0.5, accuracy: 1e-9)
        XCTAssertEqual(first, .leaf(pane: 1))
        if case .split(_, .vertical, _, _, _) = second {} else { XCTFail("second should be a vertical split") }
    }

    func testUnknownKindThrows() {
        XCTAssertThrowsError(try JSONDecoder().decode(PaneTree.self, from: Data(#"{"kind":"blah"}"#.utf8)))
    }

    func testSplitDirectionEncodesLowercase() throws {
        XCTAssertEqual(String(decoding: try JSONEncoder().encode(SplitDirection.right), as: UTF8.self), "\"right\"")
        XCTAssertEqual(String(decoding: try JSONEncoder().encode(SplitDirection.down), as: UTF8.self), "\"down\"")
    }
}
