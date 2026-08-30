// SPDX-License-Identifier: Apache-2.0

import Foundation

/// The app's three local socket paths, resolved once at startup.
public struct SocketPaths: Equatable, Sendable {
    public let client: String
    public let control: String
    public let hook: String
    public init(client: String, control: String, hook: String) {
        self.client = client
        self.control = control
        self.hook = hook
    }
}

/// Which backend to launch.
public enum BackendTarget: Equatable, Sendable {
    case local
    case remote(RemoteHost)

    public var id: BackendID {
        switch self {
        case .local: return .local
        case let .remote(h): return h.backend
        }
    }
}

/// Which bundled binary a plan runs.
public enum BackendExecutable: Equatable, Sendable {
    case daemon      // clowder-daemon
    case clowder     // clowder (the `connect` forwarder)
}

/// Everything needed to launch one backend and connect to it. Pure data, so the decision is
/// testable and `ClowderApp` only has to run it.
public struct BackendPlan: Equatable, Sendable {
    public let id: BackendID
    public let executable: BackendExecutable
    public let args: [String]
    /// Environment entries to overlay on the process environment. Deliberately an overlay, not a
    /// replacement — the child needs the inherited PATH, HOME, and the user's own settings.
    public let envOverlay: [String: String]
    public let controlPath: String
    public let renderPath: String
}

/// Where the `clowder connect` forwarder binds a given host's sockets:
/// `<control-sock parent>/remote/<host>`.
///
/// Per-host, so two hosts never collide — and so a future multi-connect needs no path changes.
/// The app passes this to the forwarder via `--socket-dir`; the forwarder's own default is flat
/// (`<control parent>/remote`) for backward compatibility, and is not used here.
public func forwarderSocketDir(controlPath: String, host: String) -> String {
    let parent = (controlPath as NSString).deletingLastPathComponent
    return ((parent as NSString).appendingPathComponent("remote") as NSString)
        .appendingPathComponent(host)
}

/// How to launch `target`.
public func backendPlan(target: BackendTarget, sockets: SocketPaths) -> BackendPlan {
    switch target {
    case .local:
        return BackendPlan(
            id: .local,
            executable: .daemon,
            args: [],
            envOverlay: [
                "CLOWDER_SOCK": sockets.client,
                "CLOWDER_CONTROL_SOCK": sockets.control,
                "CLOWDER_HOOK_SOCK": sockets.hook,
            ],
            controlPath: sockets.control,
            renderPath: sockets.client
        )

    case let .remote(host):
        let dir = forwarderSocketDir(controlPath: sockets.control, host: host.name)
        return BackendPlan(
            id: host.backend,
            executable: .clowder,
            // Select by NICKNAME: `clowder connect` resolves it through the registry, which is what
            // supplies the host's token and pin. An address would become an ad-hoc TOFU target.
            args: ["connect", host.name, "--socket-dir", dir],
            // No CLOWDER_*_SOCK overlay: those would change what the forwarder itself treats as the
            // default control socket. `--socket-dir` is the one authority for where it binds.
            envOverlay: [:],
            controlPath: (dir as NSString).appendingPathComponent("clowder-control.sock"),
            renderPath: (dir as NSString).appendingPathComponent("clowder.sock")
        )
    }
}
