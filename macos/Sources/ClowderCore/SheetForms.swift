import Foundation

/// The Add Project sheet's state. The daemon validates for real (it must — in remote mode the
/// path is on another host); this only gates the button.
public struct AddProjectForm: Equatable, Sendable {
    public var path: String
    public init(path: String = "") { self.path = path }
    public var isValid: Bool { !path.trimmingCharacters(in: .whitespaces).isEmpty }
}

/// The New Worktree sheet's state. `nameError` mirrors the daemon's `validate_workspace_name`
/// so the sheet can explain a bad name immediately. The daemon remains the authority — a name
/// that slips through here still gets a clean error back.
public struct NewWorktreeForm: Equatable, Sendable {
    public var projectPath: String
    public var name: String
    public var adapter: String

    public init(projectPath: String = "", name: String = "", adapter: String = "claude") {
        self.projectPath = projectPath
        self.name = name
        self.adapter = adapter
    }

    public var isValid: Bool { !projectPath.isEmpty && nameError == nil }

    /// Fill in whatever the user has not chosen, and nothing else.
    ///
    /// Safe to call repeatedly, which is the point: the sheet applies it when it appears AND again
    /// whenever the enabled-agent list changes, because that list can arrive after the sheet is
    /// already on screen (the control connection delivers `agentProfileList` asynchronously) or
    /// change under it (a profile toggled in Settings, a backend switch, the `clowder agent` CLI).
    ///
    /// So it must not overwrite a choice the user has already made:
    /// - `projectPath` is only ever filled when empty. Re-applying it would yank the user back to
    ///   the initial/first project mid-edit.
    /// - `adapter` is re-pointed only when it is empty or names an agent that is no longer offered
    ///   — otherwise the picker would jump off the user's selection.
    public mutating func applyDefaults(projects: [SidebarProject],
                                       adapters: [AdapterInfo],
                                       initialProjectPath: String) {
        if projectPath.isEmpty {
            projectPath = initialProjectPath.isEmpty ? (projects.first?.path ?? "") : initialProjectPath
        }
        if adapter.isEmpty || !adapters.contains(where: { $0.id == adapter }) {
            adapter = adapters.first?.id ?? ""
        }
    }

    /// Nil when the name is acceptable; otherwise a user-facing reason. Validates `name` AS SENT
    /// — no trimming — so this agrees with the daemon's `validate_workspace_name`, which also
    /// does not trim (whitespace, including a leading/trailing space, is rejected by the charset
    /// check below, not silently accepted).
    public var nameError: String? {
        let n = name
        if n.isEmpty { return "Name must not be empty" }
        if n.count > 64 { return "Name must be 64 characters or fewer" }
        if n == "." || n == ".." { return "Name must not be \(n)" }
        if n.contains("..") { return "Name must not contain '..'" }
        if n.hasSuffix(".lock") { return "Name must not end with '.lock' (git reserves it)" }
        if n.hasSuffix(".") { return "Name must not end with '.' (git rejects it as a ref)" }
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "._-"))
        if n.unicodeScalars.contains(where: { !allowed.contains($0) || !$0.isASCII }) {
            return "Name may contain only letters, digits, '.', '_' or '-'"
        }
        if n.hasPrefix(".") || n.hasPrefix("-") { return "Name must not start with \(n.prefix(1))" }
        return nil
    }
}

/// The Hosts pane's editor state.
///
/// `nameError` mirrors `clowder_config::hosts::validate_name` and is checked against the same
/// `docs/protocol/fixtures/host-names.json` the Rust validator is, so the two cannot drift. The CLI
/// remains the authority — a value that slips through here still gets a clean error back.
///
/// NOTE: host names are validated **differently** from worktree names. `validate_name` allows `...`
/// and `a..b` and has no `.lock` rule; `NewWorktreeForm.nameError` rejects all three. Do not merge them.
public struct HostDraft: Equatable, Sendable {
    public var name: String
    public var address: String
    public var tls: Bool
    /// A token the user has typed. `nil` means "unchanged" for an existing host — the app never reads
    /// a stored token back, only writes one.
    public var token: String?
    /// True when this draft creates a host rather than editing one.
    public var isNew: Bool

    public init(name: String = "", address: String = "", tls: Bool = false,
                token: String? = nil, isNew: Bool = true) {
        self.name = name
        self.address = address
        self.tls = tls
        self.token = token
        self.isNew = isNew
    }

    private static let maxName = 64

    /// Nil when acceptable; otherwise a user-facing reason. Validates the value AS TYPED — no trimming
    /// — so it agrees with the Rust validator, which also does not trim.
    public var nameError: String? {
        if name.isEmpty { return "Name must not be empty" }
        // Count Unicode scalars, matching Rust's `chars().count()`. (For any name that passes the
        // charset check below the two counts are identical, since it is ASCII by then — but matching
        // the Rust rule exactly is cheaper than reasoning about when it matters.)
        if name.unicodeScalars.count > Self.maxName {
            return "Name must be \(Self.maxName) characters or fewer"
        }
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "._-"))
        if name.unicodeScalars.contains(where: { !allowed.contains($0) || !$0.isASCII }) {
            return "Name may contain only letters, digits, '.', '_' or '-'"
        }
        // '.' is allowed above so "a.b" works, which lets these two through the charset check. The
        // name becomes a socket directory (`<runtime>/clowder/remote/<name>/`), so '..' escapes it.
        if name == "." || name == ".." { return "Name must not be '.' or '..'" }
        return nil
    }

    /// Nil when acceptable. Requires an explicit port — there is no default to fall back on, since the
    /// daemon's listen address is operator-chosen.
    public var addressError: String? {
        if address.isEmpty { return "Address must not be empty" }
        if address.unicodeScalars.contains(where: { CharacterSet.whitespacesAndNewlines.contains($0) }) {
            return "Address must not contain spaces"
        }
        guard let (host, port) = Self.splitHostPort(address), !host.isEmpty else {
            return "Address must be host:port (or [ipv6]:port)"
        }
        guard let n = UInt16(port), n != 0 else { return "Address must end in a valid port" }
        return nil
    }

    /// Nil when acceptable. A token is only ever sent over TLS — the CLI refuses the combination, so
    /// say so before the user submits rather than after.
    public var tlsError: String? {
        let hasToken = !(token ?? "").isEmpty
        return hasToken && !tls ? "A token requires TLS — turn on Use TLS, or clear the token" : nil
    }

    public var isValid: Bool { nameError == nil && addressError == nil && tlsError == nil }

    /// Split `host:port` / `[v6]:port`. Nil when there is no port, or when a bare (unbracketed) IPv6
    /// literal makes the split ambiguous.
    private static func splitHostPort(_ s: String) -> (String, String)? {
        if s.hasPrefix("[") {
            guard let close = s.firstIndex(of: "]") else { return nil }
            let host = String(s[s.index(after: s.startIndex)..<close])
            let rest = s[s.index(after: close)...]
            guard rest.hasPrefix(":") else { return nil }
            return (host, String(rest.dropFirst()))
        }
        guard let colon = s.lastIndex(of: ":") else { return nil }
        let host = String(s[s.startIndex..<colon])
        if host.contains(":") { return nil }   // bare v6 literal — require brackets
        return (host, String(s[s.index(after: colon)...]))
    }
}

/// The Agents pane's editor state.
///
/// `idError` mirrors `clowder_config::agents::validate_id`, which delegates to the host-name rule —
/// so it is checked against the same `docs/protocol/fixtures/host-names.json`. `argsError` mirrors
/// `split_args` + `validate_template` via `AgentArgs`, pinned to `agent-args.json`. The daemon
/// remains the authority.
public struct AgentProfileDraft: Equatable, Sendable {
    /// Immutable once created: the id is recorded on every agent spawned from this profile.
    public var id: String
    public var base: String
    public var displayName: String
    public var enabled: Bool
    public var args: String
    /// True when this draft creates a profile rather than editing one.
    public var isNew: Bool

    public init(id: String = "", base: String = "claude", displayName: String = "",
                enabled: Bool = true, args: String = "", isNew: Bool = true) {
        self.id = id
        self.base = base
        self.displayName = displayName
        self.enabled = enabled
        self.args = args
        self.isNew = isNew
    }

    /// The base adapters a new profile can be built on. Must track the daemon's
    /// `adapter_descriptors()` (`crates/clowder-daemon/src/agent.rs`) — the daemon is the actual
    /// authority and only validates `base` at spawn time, so drift here would let the editor create
    /// a profile that fails later rather than being caught up front. Not fetched from the daemon at
    /// runtime (out of scope); this is the same "mirrors the daemon's rule" tradeoff as `idError`.
    public static let bases = ["claude", "codex", "shell"]

    private static let maxDisplayName = 64
    /// Mirrors `clowder_config::agents::MAX_ARGS` — see `argsError`.
    private static let maxArgs = 4096

    /// Nil when acceptable. Same rule as a host name — see `HostDraft.nameError`.
    public var idError: String? {
        var host = HostDraft()
        host.name = id
        return host.nameError
    }

    public var displayNameError: String? {
        if displayName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return "Name must not be empty" }
        if displayName.unicodeScalars.count > Self.maxDisplayName {
            return "Name must be \(Self.maxDisplayName) characters or fewer"
        }
        return nil
    }

    /// Nil when acceptable. The length bound mirrors `validate_profile`'s `MAX_ARGS` check — same
    /// threshold, counted the same way (Unicode scalars, matching Rust's `chars().count()`) — so the
    /// editor refuses an over-long template before the daemon does.
    public var argsError: String? {
        if args.unicodeScalars.count > Self.maxArgs {
            return "Args must be \(Self.maxArgs) characters or fewer"
        }
        return AgentArgs.templateError(args)
    }

    public var isValid: Bool { idError == nil && displayNameError == nil && argsError == nil }
}
