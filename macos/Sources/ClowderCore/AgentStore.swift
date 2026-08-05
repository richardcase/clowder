import Foundation
import Combine

/// The client-side agent model. Refresh-driven: events that can't fully hydrate a
/// row (a pane-only `agentSpawned`, or any event for an unknown pane) set `needsRefresh`,
/// which the session/UI answers with a `ControlRequest.listWorktrees`.
public final class AgentStore: ObservableObject {
    @Published public private(set) var worktrees: [UInt64: WorktreeInfo] = [:]
    @Published public private(set) var needsRefresh: Bool = false
    @Published public private(set) var lastError: String?
    @Published public private(set) var trees: [UInt64: PaneTree] = [:]
    @Published public private(set) var adapters: [AdapterInfo] = AgentStore.defaultAdapters

    /// The adapter list shown before a connection reports its own via `adapterList`.
    static let defaultAdapters = [AdapterInfo(id: "claude", displayName: "Claude Code")]

    public init() {}

    public func apply(_ event: ControlEvent) {
        switch event {
        case let .worktreeList(list):
            worktrees = Dictionary(uniqueKeysWithValues: list.map { ($0.pane, $0) })
            needsRefresh = false
        case let .adapterList(list):
            adapters = list
        case let .attentionChanged(pane, state):
            if var a = worktrees[pane] {
                a.state = state
                worktrees[pane] = a
            } else {
                needsRefresh = true // unknown pane — re-list to learn about it
            }
        case .agentSpawned:
            needsRefresh = true // pane-only — re-list to hydrate project/name/state
        case let .agentRemoved(pane):
            worktrees[pane] = nil
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

    /// Drop all per-backend state (used when the app swaps between the local daemon and the remote
    /// forwarder). Adapters fall back to the default until the new connection reports its own list.
    public func reset() {
        worktrees = [:]
        trees = [:]
        lastError = nil
        needsRefresh = false
        adapters = AgentStore.defaultAdapters
    }

    /// Worktrees grouped by project (projects sorted; worktrees within a project sorted by pane).
    public var byProject: [(project: String, worktrees: [WorktreeInfo])] {
        Dictionary(grouping: worktrees.values, by: { $0.project })
            .map { (project: $0.key, worktrees: $0.value.sorted { $0.pane < $1.pane }) }
            .sorted { $0.project < $1.project }
    }

    /// The sidebar order flattened: worktrees grouped by project, projects sorted, worktrees by pane.
    /// The stable index order used by Cmd-1…9 and the palette.
    public var orderedWorktrees: [WorktreeInfo] { byProject.flatMap { $0.worktrees } }

    /// Worktrees that want a response — NeedsInput or Completed — in sidebar order.
    public var worktreesNeedingAttention: [WorktreeInfo] {
        orderedWorktrees.filter { $0.state == .needsInput || $0.state == .completed }
    }

    /// How many worktrees need attention (the menu-bar count).
    public var attentionCount: Int { worktreesNeedingAttention.count }
}
