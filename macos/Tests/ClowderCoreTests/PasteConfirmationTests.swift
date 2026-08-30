// SPDX-License-Identifier: Apache-2.0

import XCTest
@testable import ClowderCore

final class PasteConfirmationTests: XCTestCase {
    func testTheThreeKindsReadDifferently() {
        let paste = PasteConfirmation.alert(kind: .paste, text: "ls\nrm -rf /\n")
        let read = PasteConfirmation.alert(kind: .osc52Read, text: "secret")
        let write = PasteConfirmation.alert(kind: .osc52Write, text: "hello")

        XCTAssertEqual(paste.confirmTitle, "Paste")
        XCTAssertEqual(read.confirmTitle, "Allow")
        XCTAssertEqual(write.confirmTitle, "Allow")

        let titles = Set([paste.title, read.title, write.title])
        XCTAssertEqual(titles.count, 3, "each kind needs its own wording: \(titles)")

        // A read discloses the clipboard; a write replaces it. Saying so is the whole point.
        XCTAssertTrue(read.message.contains("asking for your clipboard"), read.message)
        XCTAssertTrue(write.message.contains("replace your clipboard"), write.message)
    }

    func testUnsafePasteReportsItsLineCount() {
        let alert = PasteConfirmation.alert(kind: .paste, text: "one\ntwo\nthree")
        XCTAssertTrue(alert.message.contains("3 lines"), alert.message)
    }

    // A single-line paste can still be unsafe, and "1 lines" would be sloppy.
    func testSingleLinePasteOmitsTheCount() {
        let alert = PasteConfirmation.alert(kind: .paste, text: "rm -rf /")
        XCTAssertFalse(alert.message.contains("lines"), alert.message)
        XCTAssertTrue(alert.message.contains("rm -rf /"), alert.message)
    }

    func testEveryAlertShowsTheText() {
        for kind in [PasteRequestKind.paste, .osc52Read, .osc52Write] {
            let alert = PasteConfirmation.alert(kind: kind, text: "needle")
            XCTAssertTrue(alert.message.contains("needle"), "\(kind): \(alert.message)")
        }
    }

    func testLongTextIsTruncatedForThePreview() {
        let long = String(repeating: "x", count: PasteConfirmation.previewLimit + 100)
        let preview = PasteConfirmation.preview(long)
        XCTAssertEqual(preview.count, PasteConfirmation.previewLimit + 1)  // + the ellipsis
        XCTAssertTrue(preview.hasSuffix("…"))
    }

    func testTextAtTheLimitIsNotTruncated() {
        let exact = String(repeating: "x", count: PasteConfirmation.previewLimit)
        XCTAssertEqual(PasteConfirmation.preview(exact), exact)
    }

    // Truncating the preview must not distort the count the warning is based on.
    func testLineCountReflectsTheWholeTextNotThePreview() {
        let many = String(repeating: "line\n", count: 400)   // far longer than previewLimit
        XCTAssertEqual(PasteConfirmation.lineCount(many), 400)
        XCTAssertTrue(PasteConfirmation.alert(kind: .paste, text: many).message.contains("400 lines"))
    }

    func testLineCountTreatsATrailingNewlineAsATerminator() {
        XCTAssertEqual(PasteConfirmation.lineCount(""), 0)
        XCTAssertEqual(PasteConfirmation.lineCount("ls"), 1)
        XCTAssertEqual(PasteConfirmation.lineCount("ls\n"), 1)
        XCTAssertEqual(PasteConfirmation.lineCount("a\nb"), 2)
        XCTAssertEqual(PasteConfirmation.lineCount("a\nb\n"), 2)
        XCTAssertEqual(PasteConfirmation.lineCount("\n"), 1)
    }
}
