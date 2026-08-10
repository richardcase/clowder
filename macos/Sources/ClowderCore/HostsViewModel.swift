import Foundation
import Combine

/// All state and operations behind the Settings window's Hosts pane.
///
/// Lives in `ClowderCore` because `ClowderApp` has no test target — every decision here is driven in
/// `swift test` by a fake `CommandRunner`. The views render this and nothing else.
@MainActor
public final class HostsViewModel: ObservableObject {
    @Published public private(set) var hosts: [RemoteHost] = []
    @Published public private(set) var selected: HostID?
    /// The editor's live state. Nil when nothing is selected.
    @Published public var draft: HostDraft?
    @Published public private(set) var lastError: String?
    /// True while a CLI call is in flight, so the view can disable its controls.
    @Published public private(set) var isBusy = false

    private let registry: HostRegistry
    /// Which backend the app is connected to, so removing it can be refused.
    private let activeBackend: () -> BackendID
    /// Told after any change that alters the host list, so the chip, tray and palette refresh.
    private let onHostsChanged: () -> Void

    public init(registry: HostRegistry,
                activeBackend: @escaping () -> BackendID,
                onHostsChanged: @escaping () -> Void) {
        self.registry = registry
        self.activeBackend = activeBackend
        self.onHostsChanged = onHostsChanged
    }

    /// The selected host, if it still exists.
    public var selectedHost: RemoteHost? {
        selected.flatMap { id in hosts.first { $0.id == id } }
    }

    /// `[remote] host` entries live in `config.toml`, which clowder never rewrites.
    public var canEditSelection: Bool { selectedHost?.isEditable ?? false }

    public func dismissError() { lastError = nil }

    public func reload() async {
        await run {
            // Assign only on success: a failed reload must not blank a list the user is looking at.
            self.hosts = try self.registry.list()
        }
    }

    public func select(_ id: HostID?) {
        selected = id
        guard let host = id.flatMap({ i in hosts.first { $0.id == i } }) else {
            draft = nil
            return
        }
        // `token` stays nil: the app never reads a stored token back, only writes one.
        draft = HostDraft(name: host.name, address: host.address, tls: host.tls,
                          token: nil, isNew: false)
    }

    public func beginAdd() {
        selected = nil
        draft = HostDraft()
    }

    public func save() async {
        guard var draft else { return }
        guard draft.isValid else {
            lastError = draft.nameError ?? draft.addressError ?? draft.tlsError
            return
        }
        let typedToken = (draft.token?.isEmpty == false) ? draft.token : nil
        let isNew = draft.isNew
        let originalName = selected?.rawValue

        let succeeded = await run {
            if isNew {
                _ = try self.registry.add(name: draft.name, address: draft.address,
                                          token: typedToken, tls: draft.tls)
            } else {
                // `isNew` is only ever set by `select`/`beginAdd`, and `select` always sets
                // `selected` alongside a non-new draft — so `originalName` is always present on
                // this branch. The guard is defensive, not a real path.
                guard let originalName else { return }
                _ = try self.registry.update(
                    name: originalName,
                    rename: draft.name == originalName ? nil : draft.name,
                    address: draft.address,
                    // Only `.set` when the user actually typed one. `.unchanged` is what keeps an
                    // existing token intact through an unrelated edit.
                    token: typedToken.map { .set($0) } ?? .unchanged,
                    tls: draft.tls
                )
            }
            self.hosts = try self.registry.list()
            self.onHostsChanged()
        }
        // A failed save must not disturb what the user typed: `self.draft` was never touched above,
        // so their name/address/TLS/token are still exactly as entered and they can fix the problem
        // (e.g. a bad address) and retry without re-typing anything — including a token, which is
        // never re-displayed once written. Advancing `selected` on failure would be worse than just
        // losing the form: for a rename it would point at a name that was never persisted, so a
        // retry's `originalName` would target a host that does not exist.
        guard succeeded else { return }
        // Clear the typed token so it does not linger in memory or get re-sent on the next save.
        draft.token = nil
        self.draft = draft
        if !isNew { selected = HostID(draft.name) } else { select(HostID(draft.name)) }
    }

    public func remove(_ id: HostID) async {
        // BackendID *is* the host name, so removing the connected host would leave the app pointed at
        // an id that no longer resolves, with no way back to it.
        if activeBackend() == .remote(id) {
            lastError = "You are connected to \(id.rawValue). Switch to another backend before removing it."
            return
        }
        let succeeded = await run {
            try self.registry.remove(name: id.rawValue)
            self.hosts = try self.registry.list()
            self.onHostsChanged()
        }
        // Only deselect once the host is actually gone — a failed remove leaves it in `hosts`, and
        // clearing the selection anyway would strand the user looking at a blank editor for a host
        // that is still right there in the list.
        if succeeded, selected == id { select(nil) }
    }

    /// Where the pairing flow is. `observed` carries what the probe saw — nothing is written until the
    /// user confirms, which is the whole point of splitting observe from trust.
    public enum PairingState: Equatable, Sendable {
        case idle
        case probing
        case observed(HostProbe)
        case failed(String)
    }

    @Published public private(set) var pairing: PairingState = .idle
    /// A fingerprint the user pasted from an out-of-band source, to be compared by software rather
    /// than by eye. Empty means "not comparing".
    @Published public var expectedFingerprint: String = ""

    private var observedProbe: HostProbe? {
        if case let .observed(p) = pairing { return p }
        return nil
    }

    /// Nil when there is nothing to compare; otherwise whether the typed expectation matches.
    public var fingerprintComparison: Bool? {
        guard let observed = observedProbe?.fingerprint else { return nil }
        let typed = expectedFingerprint.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !typed.isEmpty else { return nil }
        return typed == observed.lowercased()
    }

    /// Trust is offered only when a certificate was actually observed, and only when any typed
    /// expectation agrees with it.
    public var canTrust: Bool {
        guard let probe = observedProbe, probe.reachable, probe.fingerprint != nil else { return false }
        return fingerprintComparison != false
    }

    public func beginPairing() async {
        guard let host = selectedHost else { return }
        pairing = .probing
        expectedFingerprint = ""
        do {
            // Off the main actor: the CLI bounds each phase separately, so a probe can take ~3× its
            // timeout, and this runs while a sheet is on screen.
            pairing = .observed(try await registry.probeAsync(name: host.name))
        } catch {
            pairing = .failed((error as? LocalizedError)?.errorDescription ?? error.localizedDescription)
        }
    }

    public func confirmTrust() async {
        guard let host = selectedHost, canTrust,
              let fingerprint = observedProbe?.fingerprint else { return }
        await run {
            // Verbatim what was displayed. If a cert is swapped between probe and trust, the pin
            // fails loudly on the very next connect — an accepted, documented TOCTOU.
            try self.registry.trust(name: host.name, fingerprint: fingerprint)
            self.hosts = try self.registry.list()
            self.onHostsChanged()
        }
        if lastError == nil { cancelPairing() }
    }

    public func cancelPairing() {
        pairing = .idle
        expectedFingerprint = ""
    }

    /// Run a CLI-touching operation with busy state and uniform error surfacing. Task 4's pairing
    /// operations use this too, so it stays file-private rather than becoming API. Returns whether
    /// `body` completed without throwing, so callers can gate state advancement on success.
    @discardableResult
    private func run(_ body: @escaping () throws -> Void) async -> Bool {
        isBusy = true
        lastError = nil
        defer { isBusy = false }
        do {
            try body()
            return true
        } catch {
            lastError = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
            return false
        }
    }
}
