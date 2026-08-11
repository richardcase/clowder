import Foundation
import Combine

/// One project row plus its worktrees, prepared for rendering. Built by `AgentStore.sidebar`
/// so the view layer — which no local compiler checks — stays a plain `ForEach` over this.
public struct SidebarProject: Identifiable, Equatable, Sendable {
    public let path: String
    public let name: String
    /// `"git"` or `"jj"`.
    public let kind: String
    public let worktrees: [WorktreeInfo]
    /// How many of THIS project's worktrees want a response. Shown on the row so a collapsed
    /// project can never hide a waiting agent.
    public let attentionCount: Int
    public var id: String { path }
}

/// The client-side agent model. Refresh-driven: events that can't fully hydrate a
/// row (a pane-only `agentSpawned`, or any event for an unknown pane) set `needsRefresh`,
/// which the session/UI answers with a `ControlRequest.listWorktrees`.
public final class AgentStore: ObservableObject {
    @Published public private(set) var worktrees: [UInt64: WorktreeInfo] = [:]
    @Published public private(set) var needsRefresh: Bool = false
    @Published public private(set) var lastError: String?
    @Published public private(set) var trees: [UInt64: PaneTree] = [:]
    @Published public private(set) var adapters: [AdapterInfo] = AgentStore.defaultAdapters
    @Published public private(set) var projects: [ProjectInfo] = []
    /// Every profile, enabled or not — what the Settings Agents pane renders. `adapters` remains
    /// the ENABLED subset the daemon sends for the New Worktree picker.
    @Published public private(set) var agentProfiles: [AgentProfileInfo] = []
    /// Project path → its open terminal's pane. Populated by `projectTerminalOpened`; a missing
    /// entry means "not open yet", which is what makes selecting a project ask the daemon.
    @Published public private(set) var projectTerminals: [String: UInt64] = [:]

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
        case let .projectList(list):
            projects = list
            // Drop terminal mappings for projects that no longer exist.
            let live = Set(list.map(\.path))
            projectTerminals = projectTerminals.filter { live.contains($0.key) }
        case let .projectAdded(project):
            // Arrives twice for the requesting client (direct reply + broadcast) by design.
            if let i = projects.firstIndex(where: { $0.path == project.path }) {
                projects[i] = project
            } else {
                projects.append(project)
            }
        case let .projectRemoved(path):
            projects.removeAll { $0.path == path }
            projectTerminals[path] = nil
        case let .projectTerminalOpened(path, pane):
            projectTerminals[path] = pane
        case let .projectTerminalClosed(path):
            projectTerminals[path] = nil
        case let .agentProfileList(list):
            agentProfiles = list
        }
    }

    public func clearRefresh() { needsRefresh = false }

    /// Drop all project → pane terminal mappings. Project terminals are not persisted daemon-side
    /// (see `projectTerminals`'s doc comment), so a fresh connection — including a reconnect to
    /// the SAME daemon after it restarted — must not keep believing a stale `path → pane`
    /// mapping; the pane it names may belong to a different (or no) process now. Called from
    /// `AppModel.attemptConnect()` on every connect, not just `reset()`, because an ordinary
    /// reconnect reuses the `AgentStore` instance and never calls `reset()`.
    public func clearProjectTerminals() { projectTerminals = [:] }

    /// Dismiss the current error (clears the error banner).
    public func clearLastError() { lastError = nil }

    /// Record an error the APP produced (a backend switch we refused, a host we could not reach)
    /// in the same one-shot slot as daemon-reported `.error` events. Deliberately the same slot:
    /// the UI has exactly one dismissable error banner, and a parallel channel would either render
    /// nowhere or fight it for the same strip of screen.
    public func reportLocalError(_ message: String) { lastError = message }

    /// Drop all per-backend state (used when the app swaps between the local daemon and the remote
    /// forwarder). Adapters fall back to the default until the new connection reports its own list.
    public func reset() {
        worktrees = [:]
        trees = [:]
        lastError = nil
        needsRefresh = false
        adapters = AgentStore.defaultAdapters
        projects = []
        projectTerminals = [:]
        agentProfiles = []
    }

    /// Projects with their worktrees, ready to render. Projects sort by display name, worktrees
    /// by pane. Worktrees whose project is not registered are omitted — the spec's "fresh start"
    /// decision — so they are absent from the order and the attention count too.
    public var sidebar: [SidebarProject] {
        let byPath = Dictionary(grouping: worktrees.values, by: { $0.project })
        return projects
            .sorted { ($0.name, $0.path) < ($1.name, $1.path) }
            .map { p in
                let mine = (byPath[p.path] ?? []).sorted { $0.pane < $1.pane }
                return SidebarProject(
                    path: p.path, name: p.name, kind: p.kind, worktrees: mine,
                    attentionCount: mine.filter { $0.state == .needsInput || $0.state == .completed }.count)
            }
    }

    /// The sidebar order flattened — the stable index order for Cmd-1…9 and the palette.
    public var orderedWorktrees: [WorktreeInfo] { sidebar.flatMap(\.worktrees) }

    /// Worktrees that want a response — NeedsInput or Completed — in sidebar order.
    public var worktreesNeedingAttention: [WorktreeInfo] {
        orderedWorktrees.filter { $0.state == .needsInput || $0.state == .completed }
    }

    /// How many worktrees need attention (the menu-bar count).
    public var attentionCount: Int { worktreesNeedingAttention.count }
}
