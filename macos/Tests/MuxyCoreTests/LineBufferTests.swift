import XCTest
@testable import MuxyCore

final class LineBufferTests: XCTestCase {
    func testMultipleLinesInOneChunk() {
        var b = LineBuffer()
        XCTAssertEqual(b.append(Data("a\nbb\nccc\n".utf8)), ["a", "bb", "ccc"])
    }

    func testLineSplitAcrossAppends() {
        var b = LineBuffer()
        XCTAssertEqual(b.append(Data("hel".utf8)), [])
        XCTAssertEqual(b.append(Data("lo\n".utf8)), ["hello"])
    }

    func testTrailingPartialHeldUntilNewline() {
        var b = LineBuffer()
        XCTAssertEqual(b.append(Data("done\npart".utf8)), ["done"])
        XCTAssertEqual(b.append(Data("ial\n".utf8)), ["partial"])
    }

    func testBlankLines() {
        var b = LineBuffer()
        XCTAssertEqual(b.append(Data("\n\nx\n".utf8)), ["", "", "x"])
    }

    func testNoNewlineYieldsNothing() {
        var b = LineBuffer()
        XCTAssertEqual(b.append(Data("nolf".utf8)), [])
    }
}
