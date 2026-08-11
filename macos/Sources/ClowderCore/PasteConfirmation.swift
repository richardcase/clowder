import Foundation

/// Why libghostty is asking the user before it moves clipboard data.
/// Mirrors `ghostty_clipboard_request_e`.
public enum PasteRequestKind: Sendable {
    /// A paste libghostty judged unsafe — multi-line text arriving where the running program has
    /// not enabled bracketed paste, so it may be executed the moment it lands.
    case paste
    /// A program in the pane asked to read the clipboard (OSC 52).
    case osc52Read
    /// A program in the pane asked to replace the clipboard (OSC 52).
    case osc52Write
}

/// The text of a confirmation prompt. The app target renders this with `NSAlert`.
public struct PasteAlert: Equatable, Sendable {
    public let title: String
    public let message: String
    public let confirmTitle: String

    public init(title: String, message: String, confirmTitle: String) {
        self.title = title
        self.message = message
        self.confirmTitle = confirmTitle
    }
}

/// Builds the wording for clipboard confirmations.
///
/// The three cases are genuinely different warnings — one is "this may run without you meaning
/// it", the others are "a program in this pane is reaching for your clipboard" — so they read
/// differently rather than sharing one generic prompt.
public enum PasteConfirmation {
    /// How much of the text to show. Long enough to recognise a pasted command, short enough that
    /// the alert stays a dialog rather than a document.
    public static let previewLimit = 512

    public static func alert(kind: PasteRequestKind, text: String) -> PasteAlert {
        switch kind {
        case .paste:
            let lines = lineCount(text)
            let detail = lines > 1
                ? "This paste is \(lines) lines long. The program in this pane may run it as soon as it arrives."
                : "The program in this pane may run this as soon as it arrives."
            return PasteAlert(title: "Paste this text?",
                              message: detail + "\n\n" + preview(text),
                              confirmTitle: "Paste")
        case .osc52Read:
            return PasteAlert(
                title: "Let this pane read the clipboard?",
                message: "A program running in this pane is asking for your clipboard contents.\n\n"
                    + preview(text),
                confirmTitle: "Allow")
        case .osc52Write:
            return PasteAlert(
                title: "Let this pane change the clipboard?",
                message: "A program running in this pane wants to replace your clipboard contents.\n\n"
                    + preview(text),
                confirmTitle: "Allow")
        }
    }

    /// The text as shown in the alert, truncated with an ellipsis when it runs long.
    public static func preview(_ text: String) -> String {
        text.count <= previewLimit ? text : String(text.prefix(previewLimit)) + "…"
    }

    /// Lines as a person would count them: a single trailing newline terminates the last line
    /// rather than starting an empty one, so "ls" and "ls\n" are both one line.
    public static func lineCount(_ text: String) -> Int {
        guard !text.isEmpty else { return 0 }
        let newlines = text.reduce(0) { $1 == "\n" ? $0 + 1 : $0 }
        return text.hasSuffix("\n") ? max(newlines, 1) : newlines + 1
    }
}
