import Foundation

public enum CommandID: Hashable, Sendable {
    case openPalette
    case spawnAgent
    case nextAttention
    case switchToAgent(Int)   // 1-based position in the ordered agent list
    case splitRight
    case splitDown
    case closePane
    case focusNextPane
    case landAgent
    case discardAgent
}

public struct KeyModifiers: OptionSet, Hashable, Sendable {
    public let rawValue: Int
    public init(rawValue: Int) { self.rawValue = rawValue }
    public static let command = KeyModifiers(rawValue: 1 << 0)
    public static let shift   = KeyModifiers(rawValue: 1 << 1)
    public static let option  = KeyModifiers(rawValue: 1 << 2)
    public static let control = KeyModifiers(rawValue: 1 << 3)
}

public struct KeyBinding: Hashable, Sendable {
    public let key: Character
    public let modifiers: KeyModifiers
    public init(_ key: Character, _ modifiers: KeyModifiers) {
        self.key = key
        self.modifiers = modifiers
    }
}

public struct Command: Identifiable, Sendable {
    public let id: CommandID
    public let title: String
    public let subtitle: String?
    public var defaultShortcut: KeyBinding?
    public init(id: CommandID, title: String, subtitle: String? = nil, defaultShortcut: KeyBinding? = nil) {
        self.id = id
        self.title = title
        self.subtitle = subtitle
        self.defaultShortcut = defaultShortcut
    }
}

public struct Keymap: Sendable {
    private let overrides: [CommandID: KeyBinding]
    public init(overrides: [CommandID: KeyBinding] = [:]) { self.overrides = overrides }

    public static let defaults: [CommandID: KeyBinding] = {
        var m: [CommandID: KeyBinding] = [
            .openPalette:   KeyBinding("k", .command),
            .spawnAgent:    KeyBinding("n", .command),
            .nextAttention: KeyBinding("a", [.command, .shift]),
            .splitRight:    KeyBinding("d", .command),
            .splitDown:     KeyBinding("d", [.command, .shift]),
            .closePane:     KeyBinding("w", [.command, .shift]),
            .focusNextPane: KeyBinding("]", .command),
            .landAgent:     KeyBinding("l", .command),
        ]
        for i in 1...9 { m[.switchToAgent(i)] = KeyBinding(Character("\(i)"), .command) }
        return m
    }()

    public func binding(for id: CommandID) -> KeyBinding? {
        overrides[id] ?? Keymap.defaults[id]
    }
}

public enum CommandRegistry {
    /// Palette-visible commands (rows). `switchToAgent`/`openPalette` are bindings, not
    /// rows — agent-switching lives in the palette's agent section.
    public static func all(keymap: Keymap) -> [Command] {
        [
            Command(id: .spawnAgent, title: "Spawn Agent",
                    subtitle: "Start a new agent",
                    defaultShortcut: keymap.binding(for: .spawnAgent)),
            Command(id: .nextAttention, title: "Next Attention",
                    subtitle: "Jump to the next agent needing input",
                    defaultShortcut: keymap.binding(for: .nextAttention)),
            Command(id: .splitRight, title: "Split Right", subtitle: "Split the focused pane rightward",
                    defaultShortcut: keymap.binding(for: .splitRight)),
            Command(id: .splitDown, title: "Split Down", subtitle: "Split the focused pane downward",
                    defaultShortcut: keymap.binding(for: .splitDown)),
            Command(id: .closePane, title: "Close Pane", subtitle: "Close the focused companion pane",
                    defaultShortcut: keymap.binding(for: .closePane)),
            Command(id: .focusNextPane, title: "Focus Next Pane", subtitle: "Move focus to the next pane",
                    defaultShortcut: keymap.binding(for: .focusNextPane)),
            Command(id: .landAgent, title: "Land Agent",
                    subtitle: "Finalize the selected agent's work onto its branch",
                    defaultShortcut: keymap.binding(for: .landAgent)),
            Command(id: .discardAgent, title: "Discard Agent",
                    subtitle: "Throw away the selected agent's work + delete its branch",
                    defaultShortcut: keymap.binding(for: .discardAgent)),
        ]
    }
}
