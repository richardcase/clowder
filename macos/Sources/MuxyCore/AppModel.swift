import Foundation
import Combine

/// Owns the control channel and the app's selection. Libghostty-free so it is unit-testable.
/// Retaining `session` is what keeps ControlSession's `[weak self]` receiver alive.
@MainActor
public final class AppModel: ObservableObject {
    public enum ConnectionState: Equatable {
        case connecting
        case live
        case closed(reason: String)
    }

    public let store: AgentStore
    @Published public var selectedPane: UInt64?
    @Published public private(set) var connectionState: ConnectionState = .connecting
    @Published public var showingPalette: Bool = false
    @Published public var showingSpawn: Bool = false

    private let makeTransport: () throws -> ControlTransport
    private var connection: ControlTransport?
    private var session: ControlSession?
    private var storeSubscription: AnyCancellable?

    public init(store: AgentStore = AgentStore(),
                makeTransport: @escaping () throws -> ControlTransport) {
        self.store = store
        self.makeTransport = makeTransport
        // Republish nested store changes so SwiftUI views observing AppModel refresh
        // when agents/attention/lastError mutate (nested ObservableObject changes do
        // not cascade to the parent automatically under Combine).
        self.storeSubscription = store.objectWillChange.sink { [weak self] _ in
            self?.objectWillChange.send()
        }
    }

    /// Build the transport + session and hydrate. On any failure, land in `.closed`.
    public func connect() {
        connectionState = .connecting
        do {
            let transport = try makeTransport()
            transport.setOnClose { [weak self] in
                // UnixSocketConnection already delivers this on the main queue.
                self?.connectionState = .closed(reason: "Disconnected from daemon")
            }
            let session = ControlSession(transport: transport, store: store)
            self.connection = transport
            self.session = session
            connectionState = .live
            try session.send(.listAgents)
        } catch {
            connectionState = .closed(reason: "Could not connect: \(error)")
        }
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

    /// Dispatch a command by id.
    public func run(_ id: CommandID) {
        switch id {
        case .openPalette: showingPalette.toggle()
        case .spawnAgent: showingSpawn = true
        case .nextAttention: selectNextAttention()
        case let .switchToAgent(i): selectAgent(atIndex: i)
        }
    }

    /// Dismiss the current error banner.
    public func dismissError() { store.clearLastError() }

    /// Explicit teardown (F1): never rely on deinit — the read loop keeps the
    /// connection alive while parked in read().
    public func shutdown() {
        connection?.disconnect()
        session = nil
        connection = nil
    }
}
