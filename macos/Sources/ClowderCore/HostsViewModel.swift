// SPDX-License-Identifier: Apache-2.0

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

    /// Whether `draft` differs from what's actually stored, so Save/Revert only light up when there
    /// is something to save or discard.
    ///
    /// `draft.token` is special: `select` always fills it with `nil` (the app never reads a stored
    /// token back), so `nil`/empty does not mean "the token was cleared" — it means "leave the stored
    /// one alone". Only a non-empty typed token counts as a change.
    public var isDirty: Bool {
        guard let draft else { return false }
        guard !draft.isNew else {
            // Nothing is stored yet, so "dirty" can't mean "differs from storage". Instead it means
            // "the user has put something into this draft" — an untouched new draft is already
            // `!isValid`, so this only matters for Revert, where it lets an in-progress new host be
            // discarded via `select(nil)` rather than being permanently un-revertable.
            return draft != HostDraft()
        }
        guard let selectedHost else {
            // Shouldn't happen — a non-new draft only ever comes from `select`, which always sets
            // `selected` alongside it — but if it did, there is nothing to diff against. Treat the
            // draft as dirty so Save/Revert don't lock up.
            return true
        }
        if draft.name != selectedHost.name { return true }
        if draft.address != selectedHost.address { return true }
        if draft.tls != selectedHost.tls { return true }
        if let token = draft.token, !token.isEmpty { return true }
        return false
    }

    public func dismissError() { lastError = nil }

    public func reload() async {
        // Assign only on success: a failed reload must not blank a list the user is looking at.
        guard let hosts = await run({ try $0.list() }) else { return }
        self.hosts = hosts
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
        // `isNew` is only ever set by `select`/`beginAdd`, and `select` always sets `selected`
        // alongside a non-new draft — so an edit always has an original name. The guard below is
        // defensive, not a real path; past it, "no original name" means exactly "a new host".
        let originalName: String? = isNew ? nil : selected?.rawValue
        guard isNew || originalName != nil else { return }

        // BackendID *is* the host name, so renaming the connected host would leave the app pointed
        // at an id that no longer resolves: the chip goes permanently orange with no Retry, and
        // retrying reports a host that "is not configured". Same reasoning as `remove`.
        if let originalName, draft.name != originalName,
           activeBackend() == .remote(HostID(originalName)) {
            lastError = "You are connected to \(originalName). "
                + "Switch to another backend before renaming it."
            return
        }

        // Only value types cross to the detached task — nothing `@MainActor` is touched there.
        let name = draft.name, address = draft.address, tls = draft.tls
        let hosts = await run { registry -> [RemoteHost] in
            if let originalName {
                _ = try registry.update(
                    name: originalName,
                    rename: name == originalName ? nil : name,
                    address: address,
                    // Only `.set` when the user actually typed one. `.unchanged` is what keeps an
                    // existing token intact through an unrelated edit.
                    token: typedToken.map { .set($0) } ?? .unchanged,
                    tls: tls
                )
            } else {
                _ = try registry.add(name: name, address: address, token: typedToken, tls: tls)
            }
            return try registry.list()
        }
        // A failed save must not disturb what the user typed: `self.draft` was never touched above,
        // so their name/address/TLS/token are still exactly as entered and they can fix the problem
        // (e.g. a bad address) and retry without re-typing anything — including a token, which is
        // never re-displayed once written. Advancing `selected` on failure would be worse than just
        // losing the form: for a rename it would point at a name that was never persisted, so a
        // retry's `originalName` would target a host that does not exist.
        guard let hosts else { return }
        self.hosts = hosts
        onHostsChanged()
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
        let name = id.rawValue
        let hosts = await run { registry -> [RemoteHost] in
            try registry.remove(name: name)
            return try registry.list()
        }
        // Only deselect once the host is actually gone — a failed remove leaves it in `hosts`, and
        // clearing the selection anyway would strand the user looking at a blank editor for a host
        // that is still right there in the list.
        guard let hosts else { return }
        self.hosts = hosts
        onHostsChanged()
        if selected == id { select(nil) }
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
        let name = host.name
        let hosts = await run { registry -> [RemoteHost] in
            // Verbatim what was displayed. If a cert is swapped between probe and trust, the pin
            // fails loudly on the very next connect — an accepted, documented TOCTOU.
            try registry.trust(name: name, fingerprint: fingerprint)
            return try registry.list()
        }
        // Bind the result rather than re-deriving success from `lastError`, exactly as `save` and
        // `remove` do — one definition of "it worked" for all four operations.
        guard let hosts else { return }
        self.hosts = hosts
        onHostsChanged()
        cancelPairing()
    }

    public func cancelPairing() {
        pairing = .idle
        expectedFingerprint = ""
        // The sheet renders `lastError` itself (it sits above the settings window's alert, which
        // AppKit will not present over a sheet), so a failed Trust must not survive into the next
        // pairing attempt.
        lastError = nil
    }

    /// Run a registry operation off the main actor, with busy state and uniform error surfacing.
    ///
    /// The closure touches only the (`Sendable`) registry; results are applied by the caller back on
    /// the main actor, so no `@MainActor` state is mutated off-actor. Going off-actor is what makes
    /// `isBusy` mean anything: with the blocking `Process` call inline there was no suspension point
    /// between setting it and clearing it, so SwiftUI never saw it true — and the UI froze for the
    /// ~200-300 ms each operation's CLI spawns take. Returns nil when `body` threw, so callers can
    /// gate state advancement on success.
    private func run<T: Sendable>(_ body: @escaping @Sendable (HostRegistry) throws -> T) async -> T? {
        isBusy = true
        lastError = nil
        defer { isBusy = false }
        let registry = self.registry
        do {
            return try await Task.detached(priority: .userInitiated) { try body(registry) }.value
        } catch {
            lastError = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
            return nil
        }
    }
}
