import Foundation
import Combine

/// The client-side agent model. Refresh-driven: events that can't fully hydrate a
/// row (a pane-only `agentSpawned`, or any event for an unknown pane) set `needsRefresh`,
/// which the session/UI answers with a `ControlRequest.listAgents`.
public final class AgentStore: ObservableObject {
    @Published public private(set) var agents: [UInt64: AgentInfo] = [:]
    @Published public private(set) var needsRefresh: Bool = false
    @Published public private(set) var lastError: String?
    @Published public private(set) var trees: [UInt64: PaneTree] = [:]

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
            agents[pane] = nil
            trees[pane] = nil          // no per-companion AgentRemoved; drop the agent's tree
        case let .error(message):
            lastError = message
        case let .splitTreeChanged(agent, tree):
            trees[agent] = tree        // idempotent replace (carry-forward #1)
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

    /// Agents that want a response — NeedsInput or Completed — in sidebar order.
    public var agentsNeedingAttention: [AgentInfo] {
        orderedAgents.filter { $0.state == .needsInput || $0.state == .completed }
    }

    /// How many agents need attention (the menu-bar count).
    public var attentionCount: Int { agentsNeedingAttention.count }
}
