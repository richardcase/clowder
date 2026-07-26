import Foundation

/// Mirrors the Rust `AttentionState` (serialized as its PascalCase variant name).
public enum AttentionState: String, Codable, Equatable, Sendable {
    case idle = "Idle"
    case working = "Working"
    case needsInput = "NeedsInput"
    case completed = "Completed"
    case exited = "Exited"
}

/// Mirrors the Rust `AgentInfo` (`pane` is a bare number).
public struct AgentInfo: Codable, Identifiable, Equatable, Sendable {
    public let pane: UInt64
    public let project: String
    public let task: String
    public var state: AttentionState
    public var id: UInt64 { pane }

    public init(pane: UInt64, project: String, task: String, state: AttentionState) {
        self.pane = pane
        self.project = project
        self.task = task
        self.state = state
    }
}

/// GUI/CLI → daemon. Custom `Encodable` for the internally-tagged JSON shape.
public enum ControlRequest: Encodable, Equatable, Sendable {
    case listAgents
    case spawnAgent(project: String, task: String, adapter: String)

    private enum CodingKeys: String, CodingKey { case type, project, task, adapter }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .listAgents:
            try c.encode("listAgents", forKey: .type)
        case let .spawnAgent(project, task, adapter):
            try c.encode("spawnAgent", forKey: .type)
            try c.encode(project, forKey: .project)
            try c.encode(task, forKey: .task)
            try c.encode(adapter, forKey: .adapter)
        }
    }
}

/// daemon → GUI/CLI. Custom `Decodable` discriminating on `type`.
public enum ControlEvent: Decodable, Equatable, Sendable {
    case agentList([AgentInfo])
    case attentionChanged(pane: UInt64, state: AttentionState)
    case agentRemoved(pane: UInt64)
    case agentSpawned(pane: UInt64)
    case error(message: String)

    private enum CodingKeys: String, CodingKey { case type, agents, pane, state, message }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)
        switch type {
        case "agentList":
            self = .agentList(try c.decode([AgentInfo].self, forKey: .agents))
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
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .type, in: c, debugDescription: "unknown control event type: \(type)")
        }
    }
}
