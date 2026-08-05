import Foundation

/// Mirrors the Rust `AttentionState` (serialized as its PascalCase variant name).
public enum AttentionState: String, Codable, Equatable, Sendable {
    case idle = "Idle"
    case working = "Working"
    case needsInput = "NeedsInput"
    case completed = "Completed"
    case exited = "Exited"
}

/// Mirrors the Rust `WorktreeInfo` (`pane` is a bare number). One worktree under a project;
/// the agent is a process inside it, so `state` is that process's attention.
public struct WorktreeInfo: Codable, Identifiable, Equatable, Sendable {
    public let pane: UInt64
    /// Full path to the project root (NOT a basename).
    public let project: String
    /// The worktree's name — also the suffix of its branch.
    public let name: String
    /// `clowder/<name>`.
    public let branch: String
    public var state: AttentionState
    public var id: UInt64 { pane }

    public init(pane: UInt64, project: String, name: String, branch: String, state: AttentionState) {
        self.pane = pane
        self.project = project
        self.name = name
        self.branch = branch
        self.state = state
    }
}

/// Mirrors the Rust `AdapterInfo`.
public struct AdapterInfo: Codable, Identifiable, Equatable, Sendable {
    public let id: String
    public let displayName: String
    public init(id: String, displayName: String) {
        self.id = id
        self.displayName = displayName
    }
}

/// GUI/CLI → daemon. Custom `Encodable` for the internally-tagged JSON shape.
public enum ControlRequest: Encodable, Equatable, Sendable {
    case listWorktrees
    case listAdapters
    case spawnAgent(project: String, task: String, adapter: String)
    case splitPane(pane: UInt64, direction: SplitDirection)
    case closePane(pane: UInt64)
    case setSplitRatio(split: UInt64, ratio: Double)
    case getSplitTree(agent: UInt64)
    case landAgent(pane: UInt64)
    case discardAgent(pane: UInt64)

    private enum CodingKeys: String, CodingKey { case type, project, task, adapter, pane, direction, split, ratio, agent }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .listWorktrees:
            try c.encode("listWorktrees", forKey: .type)
        case .listAdapters:
            try c.encode("listAdapters", forKey: .type)
        case let .spawnAgent(project, task, adapter):
            try c.encode("spawnAgent", forKey: .type)
            try c.encode(project, forKey: .project)
            try c.encode(task, forKey: .task)
            try c.encode(adapter, forKey: .adapter)
        case let .splitPane(pane, direction):
            try c.encode("splitPane", forKey: .type)
            try c.encode(pane, forKey: .pane)
            try c.encode(direction, forKey: .direction)
        case let .closePane(pane):
            try c.encode("closePane", forKey: .type)
            try c.encode(pane, forKey: .pane)
        case let .setSplitRatio(split, ratio):
            try c.encode("setSplitRatio", forKey: .type)
            try c.encode(split, forKey: .split)
            try c.encode(ratio, forKey: .ratio)
        case let .getSplitTree(agent):
            try c.encode("getSplitTree", forKey: .type)
            try c.encode(agent, forKey: .agent)
        case let .landAgent(pane):
            try c.encode("landAgent", forKey: .type)
            try c.encode(pane, forKey: .pane)
        case let .discardAgent(pane):
            try c.encode("discardAgent", forKey: .type)
            try c.encode(pane, forKey: .pane)
        }
    }
}

/// daemon → GUI/CLI. Custom `Decodable` discriminating on `type`.
public enum ControlEvent: Decodable, Equatable, Sendable {
    case worktreeList([WorktreeInfo])
    case adapterList([AdapterInfo])
    case attentionChanged(pane: UInt64, state: AttentionState)
    case agentRemoved(pane: UInt64)
    case agentSpawned(pane: UInt64)
    case error(message: String)
    case splitTreeChanged(agent: UInt64, tree: PaneTree)

    private enum CodingKeys: String, CodingKey { case type, worktrees, adapters, pane, state, message, agent, tree }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)
        switch type {
        case "worktreeList":
            self = .worktreeList(try c.decode([WorktreeInfo].self, forKey: .worktrees))
        case "adapterList":
            self = .adapterList(try c.decode([AdapterInfo].self, forKey: .adapters))
        case "attentionChanged":
            self = .attentionChanged(
                pane: try c.decode(UInt64.self, forKey: .pane),
                state: try c.decode(AttentionState.self, forKey: .state))
        case "agentRemoved":
            self = .agentRemoved(pane: try c.decode(UInt64.self, forKey: .pane))
        case "agentSpawned":
            self = .agentSpawned(pane: try c.decode(UInt64.self, forKey: .pane))
        case "error":
            self = .error(message: try c.decode(String.self, forKey: .message))
        case "splitTreeChanged":
            self = .splitTreeChanged(
                agent: try c.decode(UInt64.self, forKey: .agent),
                tree: try c.decode(PaneTree.self, forKey: .tree))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .type, in: c, debugDescription: "unknown control event type: \(type)")
        }
    }
}
