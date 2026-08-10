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

        await run {
            if isNew {
                _ = try self.registry.add(name: draft.name, address: draft.address,
                                          token: typedToken, tls: draft.tls)
            } else {
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
        await run {
            try self.registry.remove(name: id.rawValue)
            self.hosts = try self.registry.list()
            self.onHostsChanged()
        }
        if selected == id { select(nil) }
    }

    /// Run a CLI-touching operation with busy state and uniform error surfacing. Task 4's pairing
    /// operations use this too, so it stays file-private rather than becoming API.
    private func run(_ body: @escaping () throws -> Void) async {
        isBusy = true
        lastError = nil
        defer { isBusy = false }
        do {
            try body()
        } catch {
            lastError = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
        }
    }
}
