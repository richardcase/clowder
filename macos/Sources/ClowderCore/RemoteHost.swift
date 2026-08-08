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
