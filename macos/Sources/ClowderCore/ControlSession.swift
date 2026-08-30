// SPDX-License-Identifier: Apache-2.0

import Foundation

/// A source/sink of newline-delimited JSON control lines. The real Unix-socket
/// implementation is added in M0c-3b2 (it's only meaningful against a live daemon);
/// M0c-3b1 tests use a fake.
public protocol ControlTransport: AnyObject {
    /// Register a callback invoked once per inbound line (newline stripped).
    func setReceiver(_ receiver: @escaping (String) -> Void)
    /// Send one request line (the implementation appends the newline).
    func send(line: String) throws
    /// Register a handler invoked once, on the main thread, when the channel closes
    /// (peer close, read error, or `disconnect()`).
    func setOnClose(_ handler: @escaping () -> Void)
    /// Proactively close the channel. Idempotent.
    func disconnect()
}

public extension ControlTransport {
    func setOnClose(_ handler: @escaping () -> Void) {}
    func disconnect() {}
}

/// Drives the control channel: inbound lines → decode → AgentStore; auto-refreshes
/// (sends `listWorktrees`) whenever the store can't fully hydrate from a streamed event.
public final class ControlSession {
    private let transport: ControlTransport
    public let store: AgentStore

    public init(transport: ControlTransport, store: AgentStore = AgentStore()) {
        self.transport = transport
        self.store = store
        transport.setReceiver { [weak self] line in self?.handle(line: line) }
    }

    private func handle(line: String) {
        let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              let event = try? JSONDecoder().decode(ControlEvent.self, from: Data(trimmed.utf8))
        else { return }
        store.apply(event)
        if store.needsRefresh {
            store.clearRefresh()
            try? send(.listWorktrees)
        }
    }

    public func send(_ request: ControlRequest) throws {
        let data = try JSONEncoder().encode(request)
        try transport.send(line: String(decoding: data, as: UTF8.self))
    }
}
