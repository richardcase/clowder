import XCTest
@testable import ClowderCore

final class TerminalClipboardTests: XCTestCase {
    // libghostty's default copy_to_clipboard:mixed sends both flavours; plain text is what the
    // pasteboard's string type must get.
    func testPrefersPlainTextOverHTML() {
        let contents = [
            ClipboardContent(mime: "text/html", data: "<b>hi</b>"),
            ClipboardContent(mime: "text/plain", data: "hi"),
        ]
        XCTAssertEqual(TerminalClipboard.plainText(from: contents), "hi")
        XCTAssertEqual(TerminalClipboard.html(from: contents), "<b>hi</b>")
    }

    func testToleratesMimeParameters() {
        let contents = [ClipboardContent(mime: "text/plain;charset=utf-8", data: "hi")]
        XCTAssertEqual(TerminalClipboard.plainText(from: contents), "hi")
    }

    func testMimeMatchIsCaseInsensitiveAndIgnoresSpacing() {
        let contents = [ClipboardContent(mime: "TEXT/Plain ; charset=UTF-8", data: "hi")]
        XCTAssertEqual(TerminalClipboard.plainText(from: contents), "hi")
    }

    // A caller sending one unlabelled flavour means that one.
    func testFallsBackToASoleUnlabelledEntry() {
        let contents = [ClipboardContent(mime: "", data: "hi")]
        XCTAssertEqual(TerminalClipboard.plainText(from: contents), "hi")
    }

    // Markup must never land on the pasteboard as if the user had selected it.
    func testLoneHTMLIsNotUsedAsPlainText() {
        let contents = [ClipboardContent(mime: "text/html", data: "<b>hi</b>")]
        XCTAssertNil(TerminalClipboard.plainText(from: contents))
        XCTAssertEqual(TerminalClipboard.html(from: contents), "<b>hi</b>")
    }

    // Ambiguous: several unlabelled flavours and no text/plain. Refuse rather than guess.
    func testSeveralUnlabelledEntriesAreRefused() {
        let contents = [ClipboardContent(mime: "application/x-thing", data: "a"),
                        ClipboardContent(mime: "application/x-other", data: "b")]
        XCTAssertNil(TerminalClipboard.plainText(from: contents))
    }

    func testEmptyWriteYieldsNothing() {
        XCTAssertNil(TerminalClipboard.plainText(from: []))
        XCTAssertNil(TerminalClipboard.html(from: []))
        XCTAssertNil(TerminalClipboard.plainText(from: [ClipboardContent(mime: "text/plain", data: "")]))
        XCTAssertNil(TerminalClipboard.html(from: [ClipboardContent(mime: "text/html", data: "")]))
    }

    func testNoHTMLFlavourWhenOnlyPlainTextIsSent() {
        XCTAssertNil(TerminalClipboard.html(from: [ClipboardContent(mime: "text/plain", data: "hi")]))
    }

    func testPreservesTrailingNewlineAndWhitespace() {
        // Copying a whole line includes its newline; trimming it here would change what gets pasted.
        let contents = [ClipboardContent(mime: "text/plain", data: "  ls -la\n")]
        XCTAssertEqual(TerminalClipboard.plainText(from: contents), "  ls -la\n")
    }
}
