// SPDX-License-Identifier: Apache-2.0

import Foundation

/// How urgent the chip looks. Mapped to colours by the view, so the decision stays testable.
public enum ChipTone: Equatable, Sendable {
    case ok
    case pending
    case warning
    case error
}

/// Everything the sidebar's connection chip renders.
public struct ConnectionChip: Equatable, Sendable {
    public let title: String
    public let detail: String?
    public let symbol: String
    public let tone: ChipTone
    /// Whether to offer a Retry. False where a retry loop is already running, or where there is
    /// nothing to retry.
    public let canRetry: Bool
}

/// What to tell the user about the current connection.
///
/// The supervisor's state takes precedence over the control channel's, because a backend that
/// exited with a terminal condition leaves the control channel merely "connecting" forever — a
/// hopeful spinner for a host that will never answer.
public func connectionChip(backend: BackendID,
                           hosts: [RemoteHost],
                           connection: AppModel.ConnectionState,
                           supervisor: DaemonSupervisor.State) -> ConnectionChip {
    let host = backend.hostID.flatMap { id in hosts.first { $0.id == id } }
    let title = backend.description
    let symbol = backend == .local ? "desktopcomputer" : "network"

    // Terminal backend failure wins: retrying the control channel cannot fix a wrong address.
    if case let .failed(reason) = supervisor {
        return ConnectionChip(title: title, detail: reason, symbol: symbol,
                              tone: .error, canRetry: true)
    }

    // A remote backend whose host is no longer in the registry: still connected, but the user
    // should know the entry is gone (they cannot re-select it after switching away).
    if backend != .local, host == nil {
        return ConnectionChip(title: title, detail: "not in your host list", symbol: symbol,
                              tone: .warning, canRetry: false)
    }

    switch connection {
    case .live:
        // Exit 3 = another daemon owns the single-instance lock. That daemon is serving us
        // perfectly well, so this is a healthy state with a note, not an error.
        let detail: String? = {
            if case .yielded = supervisor { return "external daemon" }
            return host?.address
        }()
        return ConnectionChip(title: title, detail: detail, symbol: symbol,
                              tone: .ok, canRetry: false)

    case .connecting:
        // Mirrors AppModel's startup grace period, which deliberately shows no banner.
        return ConnectionChip(title: title, detail: "connecting…", symbol: symbol,
                              tone: .pending, canRetry: false)

    case .reconnecting:
        return ConnectionChip(title: title, detail: "reconnecting…", symbol: symbol,
                              tone: .warning, canRetry: false)

    case let .closed(reason):
        return ConnectionChip(title: title, detail: reason, symbol: symbol,
                              tone: .error, canRetry: true)
    }
}
