import Foundation

/// A host's nickname — its identity in the registry. Wrapped rather than a bare `String` so a host
/// name can never be passed where an address (or any other string) is expected.
public struct HostID: Hashable, Codable, Sendable, CustomStringConvertible {
    public let rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
    public var description: String { rawValue }
}

/// Which backend the app is pointed at. Carried on connection state, selection, and every menu.
///
/// Deliberately separate from `RemoteHost`: this is the *identity*, `RemoteHost` is the value needed
/// to launch. Keeping them apart is what lets a future multi-connect qualify panes by backend without
/// threading whole host records through the UI.
public enum BackendID: Hashable, Codable, Sendable, CustomStringConvertible {
    case local
    case remote(HostID)

    public var description: String {
        switch self {
        case .local: return "Local"
        case let .remote(id): return id.rawValue
        }
    }

    public var hostID: HostID? {
        switch self {
        case .local: return nil
        case let .remote(id): return id
        }
    }
}

/// Where an entry came from. `config` entries live in `config.toml`, which clowder never rewrites.
public enum HostSource: String, Codable, Sendable {
    case registry
    case config
}

/// How an observed certificate relates to the stored pin. Absent when no certificate was seen —
/// a plaintext daemon or a failed handshake is not a *changed* certificate.
public enum FingerprintMatch: String, Codable, Sendable {
    case new
    case match
    case changed
}

/// One remote daemon, as `clowder remote list --json` reports it.
///
/// Note what is absent: the token. The CLI emits only `hasToken`, so the app never holds the secret
/// and a future move to the Keychain touches Rust only.
public struct RemoteHost: Codable, Identifiable, Hashable, Sendable {
    public let name: String
    public let address: String
    public let tls: Bool
    public let hasToken: Bool
    public let fingerprint: String?
    public let trusted: Bool
    public let source: HostSource

    public init(name: String, address: String, tls: Bool, hasToken: Bool,
                fingerprint: String?, trusted: Bool, source: HostSource) {
        self.name = name
        self.address = address
        self.tls = tls
        self.hasToken = hasToken
        self.fingerprint = fingerprint
        self.trusted = trusted
        self.source = source
    }

    public var id: HostID { HostID(name) }
    public var backend: BackendID { .remote(id) }
    /// Whether this host is paired with a certificate pin. This is the CLI's verdict, not a
    /// local re-derivation — the CLI computes it from the presence of a fingerprint, and this
    /// property returns that wire value to ensure changes to the contract surface immediately.
    public var isTrusted: Bool { trusted }
    /// `config`-sourced entries are read-only — they are defined in `config.toml`.
    public var isEditable: Bool { source == .registry }
}

/// The `clowder remote list --json` envelope.
public struct ListOutput: Codable, Sendable {
    public let hosts: [RemoteHost]
}

/// What one `clowder remote probe --json` observed.
public struct HostProbe: Codable, Sendable, Equatable {
    public let name: String
    public let address: String
    public let reachable: Bool
    public let tls: Bool
    public let fingerprint: String?
    public let pinnedFingerprint: String?
    public let fingerprintMatch: FingerprintMatch?
    /// Whether the daemon accepted our token. **Not** meaningful alone — a plaintext daemon accepts
    /// anything. Use `authSummary`, which folds in `tls`.
    public let authenticated: Bool
    public let error: String?

    public init(name: String, address: String, reachable: Bool, tls: Bool,
                fingerprint: String?, pinnedFingerprint: String?,
                fingerprintMatch: FingerprintMatch?, authenticated: Bool, error: String?) {
        self.name = name
        self.address = address
        self.reachable = reachable
        self.tls = tls
        self.fingerprint = fingerprint
        self.pinnedFingerprint = pinnedFingerprint
        self.fingerprintMatch = fingerprintMatch
        self.authenticated = authenticated
        self.error = error
    }

    /// What to actually tell the user about authentication.
    public enum AuthSummary: Equatable, Sendable {
        /// No TLS, so the daemon accepted our token without checking it. Reporting this as success
        /// would be a lie.
        case nonePlaintext
        case tokenAccepted
        case tokenRejected
    }

    public var authSummary: AuthSummary {
        guard tls else { return .nonePlaintext }
        return authenticated ? .tokenAccepted : .tokenRejected
    }
}

/// The `clowder remote probe --json` envelope.
public struct ProbeOutput: Codable, Sendable {
    public let probe: HostProbe
}

/// A hex fingerprint (`RemoteHost.fingerprint` or `HostProbe.fingerprint`) split into 4-character
/// groups — the form people can actually compare, e.g. by eye against a daemon's startup log.
///
/// A free function rather than a method on either type: it is pure string formatting with no
/// dependency on `RemoteHost` or `HostProbe`, and both the editor's stored pin and the pairing
/// sheet's freshly-observed value need it. Lives in `ClowderCore`, not a view, so it is testable —
/// `ClowderApp` has no test target.
public func groupedFingerprint(_ fingerprint: String) -> String {
    stride(from: 0, to: fingerprint.count, by: 4).map {
        let start = fingerprint.index(fingerprint.startIndex, offsetBy: $0)
        let end = fingerprint.index(start, offsetBy: min(4, fingerprint.count - $0))
        return String(fingerprint[start..<end])
    }.joined(separator: " ")
}

/// Why a switch to this host must be refused, or nil if it may proceed.
///
/// Reachability alone is not enough to commit to a switch. A host whose certificate rotated, or
/// whose token the daemon rejects, is perfectly *reachable*: `clowder connect`'s pre-dial lands, so
/// it does not exit 4, it binds its sockets and lives — and the daemon then rejects every forwarded
/// stream, leaving the app reconnecting forever with the real reason visible only in `daemon.log`.
/// Refusing here, before anything is torn down, is the only place that failure mode is legible.
///
/// Uses `authSummary` rather than the raw `authenticated` flag: against a plaintext daemon
/// `authenticated` is true for any token, so reading it directly would refuse every plaintext host.
public func backendSwitchRefusal(_ probe: HostProbe) -> String? {
    if !probe.reachable {
        let detail = probe.error.map { " \($0)" } ?? ""
        return "Cannot reach \(probe.name) at \(probe.address).\(detail)"
    }
    if probe.fingerprintMatch == .changed {
        return """
        \(probe.name) presented a different certificate than the one you pinned. \
        If you expect this (the daemon was reinstalled), re-pin it with \
        `clowder remote trust \(probe.name)`; otherwise do not connect.
        """
    }
    if probe.authSummary == .tokenRejected {
        return "\(probe.name) rejected the access token. Update it with `clowder remote set \(probe.name) --token-stdin`."
    }
    return nil
}
