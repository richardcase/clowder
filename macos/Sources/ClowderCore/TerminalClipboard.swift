// SPDX-License-Identifier: Apache-2.0

import Foundation

/// Somewhere text can be copied to and pasted from.
///
/// ClowderCore stays AppKit-free so this logic is unit-testable; the app target supplies an
/// `NSPasteboard`-backed conformance.
public protocol Pasteboard {
    /// The current plain-text contents, or nil when the pasteboard holds nothing pasteable.
    func string() -> String?
    /// Replace the contents. `html` is the optional rich flavour of the same text.
    func write(plain: String, html: String?)
}

/// One flavour of a clipboard write — the Swift mirror of `ghostty_clipboard_content_s`.
public struct ClipboardContent: Equatable, Sendable {
    public let mime: String
    public let data: String

    public init(mime: String, data: String) {
        self.mime = mime
        self.data = data
    }
}

/// Picks what actually goes on the pasteboard when libghostty copies.
///
/// libghostty hands `write_clipboard_cb` an *array* of mime/data pairs rather than one string —
/// its default `copy_to_clipboard:mixed` mode emits `text/plain` and `text/html` together — so
/// something has to choose between them.
public enum TerminalClipboard {
    /// The plain-text flavour, or nil if the write carries no usable text.
    ///
    /// Prefers an explicit `text/plain`. A lone unlabelled entry is taken at face value, since a
    /// caller sending a single flavour means that one; a lone `text/html` is not, because putting
    /// markup on the pasteboard as plain text is never what the user selected.
    public static func plainText(from contents: [ClipboardContent]) -> String? {
        if let plain = contents.first(where: { isPlainText($0.mime) }) {
            return plain.data.isEmpty ? nil : plain.data
        }
        guard contents.count == 1, let only = contents.first,
              !isHTML(only.mime), !only.data.isEmpty else { return nil }
        return only.data
    }

    /// The rich flavour of the same copy, when the write includes one.
    public static func html(from contents: [ClipboardContent]) -> String? {
        guard let html = contents.first(where: { isHTML($0.mime) }), !html.data.isEmpty else {
            return nil
        }
        return html.data
    }

    private static func isPlainText(_ mime: String) -> Bool { baseType(mime) == "text/plain" }

    private static func isHTML(_ mime: String) -> Bool { baseType(mime) == "text/html" }

    /// `text/plain;charset=utf-8` -> `text/plain`. Mime parameters are not part of the choice.
    private static func baseType(_ mime: String) -> String {
        let base = mime.split(separator: ";", maxSplits: 1).first ?? ""
        return base.trimmingCharacters(in: .whitespaces).lowercased()
    }
}
