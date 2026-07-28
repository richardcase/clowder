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

    private let makeTransport: () throws -> ControlTransport
    private var connection: ControlTransport?
    private var session: ControlSession?

    public init(store: AgentStore = AgentStore(),
                makeTransport: @escaping () throws -> ControlTransport) {
        self.store = store
        self.makeTransport = makeTransport
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

    /// Explicit teardown (F1): never rely on deinit — the read loop keeps the
    /// connection alive while parked in read().
    public func shutdown() {
        connection?.disconnect()
        session = nil
        connection = nil
    }
}
