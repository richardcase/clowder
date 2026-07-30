import Foundation
import Combine

public enum LifecycleAction: Equatable, Sendable { case land, discard }

/// A Land/Discard awaiting user confirmation (captured at request time so the confirmation
/// UI can show which agent + task is affected, even if selection changes before confirming).
public struct PendingLifecycle: Equatable, Sendable {
    public let action: LifecycleAction
    public let pane: UInt64
    public let task: String
    public init(action: LifecycleAction, pane: UInt64, task: String) {
        self.action = action
        self.pane = pane
        self.task = task
    }
}

/// Owns the control channel and the app's selection. Libghostty-free so it is unit-testable.
/// Retaining `session` is what keeps ControlSession's `[weak self]` receiver alive.
@MainActor
public final class AppModel: ObservableObject {
    public enum ConnectionState: Equatable {
        case connecting
        case live
        case reconnecting
        case closed(reason: String)
    }

    public let store: AgentStore
    @Published public var selectedPane: UInt64? {
        didSet {
            focusedPane = selectedPane            // focus the agent pane on (re)select
            if let agent = selectedPane { try? session?.send(.getSplitTree(agent: agent)) }
        }
    }
    @Published public var focusedPane: UInt64?
    @Published public private(set) var connectionState: ConnectionState = .connecting
    @Published public var showingPalette: Bool = false
    @Published public var showingSpawn: Bool = false
    @Published public var pendingLifecycle: PendingLifecycle?

    private let makeTransport: () throws -> ControlTransport
    private var connection: ControlTransport?
    private var session: ControlSession?
    private var storeSubscription: AnyCancellable?
    private let sleepFn: (TimeInterval) async -> Void
    private var reconnectTask: Task<Void, Never>?
    private var isShuttingDown = false

    public init(store: AgentStore = AgentStore(),
                makeTransport: @escaping () throws -> ControlTransport,
                sleep: @escaping (TimeInterval) async -> Void = { d in
                    try? await Task.sleep(nanoseconds: UInt64(max(0, d) * 1_000_000_000))
                }) {
        self.store = store
        self.makeTransport = makeTransport
        self.sleepFn = sleep
        // Republish nested store changes so SwiftUI views observing AppModel refresh
        // when agents/attention/lastError mutate (nested ObservableObject changes do
        // not cascade to the parent automatically under Combine).
        self.storeSubscription = store.objectWillChange.sink { [weak self] _ in
            self?.objectWillChange.send()
            DispatchQueue.main.async { self?.reconcileFocus() }
        }
    }

    /// Build the transport + session and hydrate. Initial failure lands in `.closed`; a later DROP
    /// of a live connection enters the reconnect loop (see `handleClose`).
    public func connect() {
        isShuttingDown = false
        connectionState = .connecting
        do {
            try attemptConnect()
        } catch {
            connectionState = .closed(reason: "Could not connect: \(error)")
        }
    }

    /// One connection attempt: build the transport, wire close→reconnect, hydrate. Throws on failure.
    private func attemptConnect() throws {
        let transport = try makeTransport()
        transport.setOnClose { [weak self] in self?.handleClose() }
        let session = ControlSession(transport: transport, store: store)
        self.connection = transport
        self.session = session
        connectionState = .live
        try session.send(.listAgents)
        try session.send(.listAdapters)
    }

    /// The transport closed. Unless we're explicitly shutting down, start reconnecting.
    private func handleClose() {
        guard !isShuttingDown else { return }
        scheduleReconnect()
    }

    private func backoffDelay(_ attempt: Int) -> TimeInterval {
        min(10.0, 0.5 * pow(2.0, Double(attempt)))
    }

    /// Start the bounded exponential-backoff reconnect loop (idempotent while one is running).
    private func scheduleReconnect() {
        guard !isShuttingDown, reconnectTask == nil else { return }
        connectionState = .reconnecting
        reconnectTask = Task { [weak self] in await self?.reconnectLoop() }
    }

    private func reconnectLoop() async {
        var attempt = 0
        while !Task.isCancelled && !isShuttingDown {
            await sleepFn(backoffDelay(attempt))
            if Task.isCancelled || isShuttingDown { break }
            do {
                try attemptConnect()          // sets .live + re-hydrates on success
                reconnectTask = nil
                return
            } catch {
                attempt += 1
            }
        }
        reconnectTask = nil
    }

    public func spawn(project: String, task: String, adapter: String) {
        guard let session else { return }
        do {
            try session.send(.spawnAgent(project: project, task: task, adapter: adapter))
        } catch {
            connectionState = .closed(reason: "Send failed: \(error)")
        }
    }

    /// Select the 1-based Nth agent in the ordered list (Cmd-N). No-op if out of range.
    public func selectAgent(atIndex index: Int) {
        let ordered = store.orderedAgents
        guard index >= 1, index <= ordered.count else { return }
        selectedPane = ordered[index - 1].pane
    }

    /// Select the next agent needing input after the current selection, cycling. If the
    /// current selection isn't needy, select the first needy one; no-op if none need input.
    public func selectNextAttention() {
        let needy = store.orderedAgents.filter { $0.state == .needsInput }
        guard !needy.isEmpty else { return }
        if let cur = selectedPane, let idx = needy.firstIndex(where: { $0.pane == cur }) {
            selectedPane = needy[(idx + 1) % needy.count].pane
        } else {
            selectedPane = needy[0].pane
        }
    }

    /// The selected agent's split tree, or nil (the detail falls back to a lone leaf).
    public var currentTree: PaneTree? { selectedPane.flatMap { store.trees[$0] } }

    public var adapters: [AdapterInfo] { store.adapters }

    public func splitFocused(_ direction: SplitDirection) {
        guard let target = focusedPane ?? selectedPane, session != nil else { return }
        try? session?.send(.splitPane(pane: target, direction: direction))
    }

    public func closeFocused() {
        // Only companions are closable here; closing the agent pane is teardown (out of scope).
        guard let f = focusedPane, f != selectedPane, session != nil else { return }
        try? session?.send(.closePane(pane: f))
        focusedPane = selectedPane            // optimistic: the leaf is going away
    }

    public func focusNextPane() {
        guard let leaves = currentTree?.leaves, !leaves.isEmpty else { return }
        if let f = focusedPane, let i = leaves.firstIndex(of: f) {
            focusedPane = leaves[(i + 1) % leaves.count]
        } else {
            focusedPane = leaves.first
        }
    }

    /// Send a new divider ratio (clamped to [0.05, 0.95], matching the daemon) to the daemon.
    public func setDividerRatio(split: UInt64, ratio: Double) {
        guard session != nil else { return }
        let r = min(0.95, max(0.05, ratio))
        try? session?.send(.setSplitRatio(split: split, ratio: r))
    }

    /// If the focused pane is no longer a leaf of the current tree (a companion closed, or an
    /// external tree change), move focus back to the agent pane.
    func reconcileFocus() {
        guard let leaves = currentTree?.leaves else { return }   // no tree → leave focus as-is
        if let f = focusedPane, !leaves.contains(f) {
            focusedPane = selectedPane
        }
    }

    /// Dispatch a command by id.
    public func run(_ id: CommandID) {
        switch id {
        case .openPalette: showingPalette.toggle()
        case .spawnAgent: showingSpawn = true
        case .nextAttention: selectNextAttention()
        case let .switchToAgent(i): selectAgent(atIndex: i)
        case .splitRight: splitFocused(.right)
        case .splitDown: splitFocused(.down)
        case .closePane: closeFocused()
        case .focusNextPane: focusNextPane()
        case .landAgent: requestLifecycle(.land)
        case .discardAgent: requestLifecycle(.discard)
        }
    }

    /// Begin a Land/Discard: capture the selected agent + task and await confirmation.
    public func requestLifecycle(_ action: LifecycleAction) {
        guard let pane = selectedPane, let agent = store.agents[pane] else { return }
        pendingLifecycle = PendingLifecycle(action: action, pane: pane, task: agent.task)
    }

    /// Confirmed: send the request and clear.
    public func confirmLifecycle() {
        guard let p = pendingLifecycle else { return }
        switch p.action {
        case .land: try? session?.send(.landAgent(pane: p.pane))
        case .discard: try? session?.send(.discardAgent(pane: p.pane))
        }
        pendingLifecycle = nil
    }

    public func cancelLifecycle() { pendingLifecycle = nil }

    /// Dismiss the current error banner.
    public func dismissError() { store.clearLastError() }

    /// Explicit teardown (F1): cancel any reconnect loop, then disconnect. `isShuttingDown` makes the
    /// disconnect's own `onClose` a no-op so we don't re-arm reconnect while quitting.
    public func shutdown() {
        isShuttingDown = true
        reconnectTask?.cancel()
        reconnectTask = nil
        connection?.disconnect()
        session = nil
        connection = nil
    }
}
