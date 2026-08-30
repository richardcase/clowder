// SPDX-License-Identifier: Apache-2.0

import Foundation

/// What a terminal menu item does when chosen.
public enum TerminalMenuAction: Equatable, Sendable {
    /// Handled by the surface itself via a libghostty binding action.
    case copy
    case paste
    case selectAll
    /// Handled by the app model, exactly as the clowder menu and the ⌘K palette would.
    case command(CommandID)
}

public struct TerminalMenuItem: Equatable, Sendable {
    public let title: String
    public let action: TerminalMenuAction
    public let isEnabled: Bool

    public init(title: String, action: TerminalMenuAction, isEnabled: Bool) {
        self.title = title
        self.action = action
        self.isEnabled = isEnabled
    }
}

/// The right-click menu for a terminal pane.
///
/// There is no Cut: a terminal's scrollback is not an editable buffer, and libghostty has no cut
/// action to bind one to.
public enum TerminalMenu {
    /// The menu, in order. `nil` is a separator.
    ///
    /// Split and close reuse the existing `CommandID`s — including their registry titles — so the
    /// context menu cannot drift from the clowder menu and the palette.
    public static func contextMenu(hasSelection: Bool,
                                   pasteboardHasText: Bool,
                                   canClosePane: Bool) -> [TerminalMenuItem?] {
        var items: [TerminalMenuItem?] = [
            TerminalMenuItem(title: "Copy", action: .copy, isEnabled: hasSelection),
            TerminalMenuItem(title: "Paste", action: .paste, isEnabled: pasteboardHasText),
            TerminalMenuItem(title: "Select All", action: .selectAll, isEnabled: true),
            nil,
        ]
        for (id, enabled) in [(CommandID.splitRight, true),
                              (CommandID.splitDown, true),
                              (CommandID.closePane, canClosePane)] {
            guard let title = registryTitle(for: id) else { continue }
            items.append(TerminalMenuItem(title: title, action: .command(id), isEnabled: enabled))
        }
        return items
    }

    /// The palette's title for a command, so renames land here too.
    static func registryTitle(for id: CommandID) -> String? {
        CommandRegistry.all(keymap: Keymap()).first { $0.id == id }?.title
    }
}
