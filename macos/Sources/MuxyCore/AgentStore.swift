import Foundation
import Combine

/// The client-side agent model. Refresh-driven: events that can't fully hydrate a
/// row (a pane-only `agentSpawned`, or any event for an unknown pane) set `needsRefresh`,
/// which the session/UI answers with a `ControlRequest.listAgents`.
public final class AgentStore: ObservableObject {
    @Published public private(set) var agents: [UInt64: AgentInfo] = [:]
    @Published public private(set) var needsRefresh: Bool = false
    @Published public private(set) var lastError: String?

    public init() {}

    public func apply(_ event: ControlEvent) {
        switch event {
        case let .agentList(list):
            agents = Dictionary(uniqueKeysWithValues: list.map { ($0.pane, $0) })
            needsRefresh = false
        case let .attentionChanged(pane, state):
            if var a = agents[pane] {
                a.state = state
                agents[pane] = a
            } else {
                needsRefresh = true // unknown pane — re-list to learn about it
            }
        case .agentSpawned:
            needsRefresh = true // pane-only — re-list to hydrate project/task/state
        case let .agentRemoved(pane):
            agents[pane] = nil // idempotent
        case let .error(message):
            lastError = message
        }
    }

    public func clearRefresh() { needsRefresh = false }

    /// Dismiss the current error (clears the error banner).
    public func clearLastError() { lastError = nil }

    /// Agents grouped by project (projects sorted; agents within a project sorted by pane).
    public var byProject: [(project: String, agents: [AgentInfo])] {
        Dictionary(grouping: agents.values, by: { $0.project })
            .map { (project: $0.key, agents: $0.value.sorted { $0.pane < $1.pane }) }
            .sorted { $0.project < $1.project }
    }

    /// The sidebar order flattened: agents grouped by project, projects sorted, agents by pane.
    /// The stable index order used by Cmd-1…9 and the palette.
    public var orderedAgents: [AgentInfo] { byProject.flatMap { $0.agents } }
}
