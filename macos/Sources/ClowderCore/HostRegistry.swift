import Foundation

/// One finished subprocess run.
public struct CommandResult: Sendable {
    public let status: Int32
    public let stdout: Data
    public let stderr: Data
    public init(status: Int32, stdout: Data, stderr: Data) {
        self.status = status
        self.stdout = stdout
        self.stderr = stderr
    }
}

/// Runs the `clowder` binary. Injected so the whole registry layer is unit-testable —
/// `ClowderApp`, where the `Process` implementation lives, has no tests at all.
public protocol CommandRunner: AnyObject, Sendable {
    func run(_ args: [String], stdin: String?) throws -> CommandResult
}

public enum HostRegistryError: Error, LocalizedError, Equatable {
    /// The CLI reported a failure. Carries the CLI's own message — it is far more useful than
    /// anything this layer could invent.
    case cli(String)
    case decode(String)

    public var errorDescription: String? {
        switch self {
        case let .cli(m): return m
        case let .decode(m): return "Could not read the clowder CLI's response: \(m)"
        }
    }
}

/// How an edit treats the host's token.
public enum TokenEdit: Sendable, Equatable {
    case unchanged
    case clear
    case set(String)
}

/// Reads and writes the host registry by driving `clowder remote …`.
///
/// The app never parses `config.toml` or `hosts.json` itself — the CLI owns both, including the
/// merge that surfaces `[remote] host` as a read-only entry. This mirrors how the app already
/// asked the CLI for the resolved remote host before M11.
///
/// `Sendable` because the pairing flow runs `probeAsync` off the main actor: the CLI bounds each of
/// connect / TLS handshake / read-line by the timeout separately, so a probe can take ~3× it, and
/// blocking `@MainActor` for that long freezes the Settings window.
public struct HostRegistry: Sendable {
    private let runner: CommandRunner
    public init(runner: CommandRunner) { self.runner = runner }

    public func list() throws -> [RemoteHost] {
        try decode(ListOutput.self, from: run(["remote", "list", "--json"])).hosts
    }

    public func show(name: String) throws -> RemoteHost {
        try decode(RemoteHost.self, from: run(["remote", "show", name, "--json"]))
    }

    @discardableResult
    public func add(name: String, address: String, token: String?, tls: Bool) throws -> RemoteHost {
        var args = ["remote", "add", name, address]
        args.append(tls ? "--tls" : "--no-tls")
        if token != nil { args.append("--token-stdin") }
        args.append("--json")
        return try decode(RemoteHost.self, from: run(args, stdin: token))
    }

    @discardableResult
    public func update(name: String, rename: String?, address: String?,
                       token: TokenEdit, tls: Bool?) throws -> RemoteHost {
        var args = ["remote", "set", name]
        if let rename { args += ["--rename", rename] }
        if let address { args += ["--address", address] }
        if let tls { args.append(tls ? "--tls" : "--no-tls") }
        var stdin: String?
        switch token {
        case .unchanged: break
        case .clear: args.append("--no-token")
        case let .set(t):
            args.append("--token-stdin")
            stdin = t
        }
        args.append("--json")
        return try decode(RemoteHost.self, from: run(args, stdin: stdin))
    }

    public func remove(name: String) throws {
        _ = try run(["remote", "rm", name, "--json"])
    }

    /// Probe a registered host.
    ///
    /// `timeoutSeconds` is passed through as `--timeout`. The CLI applies it SEPARATELY to the TCP
    /// connect, the TLS handshake and the read-line, so a call can block for up to ~3x this value —
    /// which matters because every caller today runs it synchronously on the main thread.
    public func probe(name: String, timeoutSeconds: Int = 3) throws -> HostProbe {
        let args = ["remote", "probe", name, "--timeout", String(timeoutSeconds), "--json"]
        return try decode(ProbeOutput.self, from: run(args)).probe
    }

    /// Probe a host that is not (yet) in the registry — what a "Test" button needs before saving.
    /// See `probe(name:timeoutSeconds:)` for what the timeout actually bounds.
    public func probe(address: String, token: String?, tls: Bool,
                      timeoutSeconds: Int = 3) throws -> HostProbe {
        var args = ["remote", "probe", "--address", address]
        args.append(tls ? "--tls" : "--no-tls")
        args += ["--timeout", String(timeoutSeconds)]
        if token != nil { args.append("--token-stdin") }
        args.append("--json")
        return try decode(ProbeOutput.self, from: run(args, stdin: token)).probe
    }

    /// `probe(name:timeoutSeconds:)` off the calling actor.
    ///
    /// The work is a blocking subprocess call, so it goes to a detached task rather than merely being
    /// `async` — an `async` function that never suspends would still run on the caller's executor.
    public func probeAsync(name: String, timeoutSeconds: Int = 3) async throws -> HostProbe {
        let registry = self
        return try await Task.detached(priority: .userInitiated) {
            try registry.probe(name: name, timeoutSeconds: timeoutSeconds)
        }.value
    }

    /// `probe(address:token:tls:timeoutSeconds:)` off the calling actor. Used by "Test" before a host
    /// is saved, so the token is still in hand and still goes via stdin.
    public func probeAsync(address: String, token: String?, tls: Bool,
                           timeoutSeconds: Int = 3) async throws -> HostProbe {
        let registry = self
        return try await Task.detached(priority: .userInitiated) {
            try registry.probe(address: address, token: token, tls: tls, timeoutSeconds: timeoutSeconds)
        }.value
    }

    public func trust(name: String, fingerprint: String) throws {
        _ = try run(["remote", "trust", name, "--fingerprint", fingerprint, "--json"])
    }

    public func untrust(name: String) throws {
        _ = try run(["remote", "untrust", name, "--json"])
    }

    // MARK: - plumbing

    private func run(_ args: [String], stdin: String? = nil) throws -> Data {
        let result: CommandResult
        do {
            result = try runner.run(args, stdin: stdin)
        } catch {
            throw HostRegistryError.cli("Could not run the clowder CLI: \(error.localizedDescription)")
        }
        guard result.status == 0 else {
            // The CLI emits `{"error": …}` on stdout even for failures, precisely so this layer
            // has a message to show. Decode stdout FIRST and fall back to the exit code — a stray
            // library warning on stderr must not become the user-facing error.
            throw HostRegistryError.cli(errorMessage(from: result))
        }
        return result.stdout
    }

    private func errorMessage(from result: CommandResult) -> String {
        struct ErrorEnvelope: Decodable { let error: String }
        if let env = try? JSONDecoder().decode(ErrorEnvelope.self, from: result.stdout) {
            return env.error
        }
        let err = String(decoding: result.stderr, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        if !err.isEmpty { return err }
        return "clowder exited with status \(result.status)"
    }

    private func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        do {
            return try JSONDecoder().decode(type, from: data)
        } catch {
            throw HostRegistryError.decode(String(describing: error))
        }
    }
}
