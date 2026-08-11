import Foundation
import Combine

/// All state and operations behind the Settings window's Agents pane.
///
/// Lives in `ClowderCore` because `ClowderApp` has no test target — every decision here is driven in
/// `swift test` by a recording `send`. The views render this and nothing else.
///
/// The daemon is the source of truth: `save`/`remove` send a request and the resulting
/// `agentProfileList` broadcast arrives via `apply(profiles:)`. Nothing is written optimistically,
/// so the list can never show a profile the daemon refused.
@MainActor
public final class AgentsViewModel: ObservableObject {
    @Published public private(set) var profiles: [AgentProfileInfo] = []
    @Published public private(set) var selected: String?
    /// The editor's live state. Nil when nothing is selected.
    @Published public var draft: AgentProfileDraft?
    @Published public private(set) var lastError: String?

    /// The stored values `draft` was created from. `isDirty` diffs against this, not against the live
    /// `selectedProfile` — so a remote edit landing via `apply(profiles:)` while the user has typed
    /// nothing does not masquerade as a local change, and an actual local change is never silently
    /// overwritten by adopting a concurrent remote edit underneath it.
    private var baseline: AgentProfileInfo?

    /// The id just sent in an `.addAgentProfile` that hasn't yet been confirmed by a broadcast
    /// containing it. Without this, a successful add leaves `selected` nil and `draft.isNew` true
    /// forever — `isDirty` never settles, and a second Save re-sends the same id as a duplicate add.
    /// `apply(profiles:)` adopts the pane onto the new profile once this id actually appears.
    private var pendingAddID: String?

    private let send: (ControlRequest) throws -> Void

    public init(send: @escaping (ControlRequest) throws -> Void) {
        self.send = send
    }

    public var selectedProfile: AgentProfileInfo? {
        selected.flatMap { id in profiles.first { $0.id == id } }
    }

    /// Built-ins can be edited and disabled but never removed — the daemon refuses it too.
    public var canRemoveSelection: Bool { selectedProfile.map { !$0.builtin } ?? false }

    /// Whether `draft` differs from `baseline` — the values it was created from — so Save/Revert
    /// only light up when there is something to save or discard. Deliberately not compared against
    /// `selectedProfile`: see `baseline`.
    public var isDirty: Bool {
        guard let draft else { return false }
        guard !draft.isNew else { return draft != AgentProfileDraft() }
        guard let baseline else { return true }
        return draft.displayName != baseline.displayName || draft.enabled != baseline.enabled
            || draft.args != baseline.args || draft.base != baseline.base
    }

    /// The editor's live preview of the resolved arguments.
    public var preview: String { draft.map { AgentArgs.preview($0.args) } ?? "" }

    public func dismissError() { lastError = nil }

    /// Push an error into this pane's error slot from outside.
    ///
    /// The daemon reports a refused mutation as a bare, uncorrelated `ControlEvent.error` — it
    /// carries no request id, so it cannot be routed back here automatically and lands instead on
    /// `AgentStore.lastError`. Whoever owns both this view model and the store is responsible for
    /// forwarding it here, so a refusal can actually be explained inside the Agents pane instead of
    /// leaving it silently stuck (e.g. mid-save). Mirrors `AgentStore.reportLocalError`.
    public func reportError(_ message: String) { lastError = message }

    /// Adopt a list from the daemon.
    ///
    /// - A pending add whose id has now appeared is adopted: the pane selects it and the draft
    ///   becomes a normal (non-new) draft of it, so `isDirty` settles and a second Save is possible.
    /// - Otherwise, while the selection is still present: an undirtied draft adopts the (possibly
    ///   remotely changed) stored values; a dirtied draft is left completely untouched, so a
    ///   concurrent edit from elsewhere is never silently clobbered by this pane's own Save.
    /// - A selection that has gone away is cleared, along with its draft.
    public func apply(profiles: [AgentProfileInfo]) {
        self.profiles = profiles

        if let pendingAddID, let added = profiles.first(where: { $0.id == pendingAddID }) {
            self.pendingAddID = nil
            adopt(added)
            return
        }

        guard let selected else { return }
        guard let p = profiles.first(where: { $0.id == selected }) else {
            self.selected = nil
            draft = nil
            baseline = nil
            return
        }
        if !isDirty { adopt(p) }
    }

    public func reload() { dispatch(.listAgentProfiles) }

    public func select(_ id: String?) {
        pendingAddID = nil
        guard let p = id.flatMap({ i in profiles.first { $0.id == i } }) else {
            selected = id
            draft = nil
            baseline = nil
            return
        }
        adopt(p)
    }

    public func beginAdd() {
        selected = nil
        baseline = nil
        pendingAddID = nil
        draft = AgentProfileDraft()
    }

    /// A local, unsaved copy of the selection under a fresh id — how a user makes "Claude (Opus)"
    /// from "Claude Code" without inventing a program.
    public func duplicateSelected() {
        guard let p = selectedProfile else { return }
        selected = nil
        baseline = nil
        pendingAddID = nil
        draft = AgentProfileDraft(id: freshID(basedOn: p.id), base: p.base,
                                  displayName: "\(p.displayName) copy", enabled: p.enabled,
                                  args: p.args, isNew: true)
    }

    /// Restore the editor to what the daemon holds.
    public func revert() { select(selected) }

    public func save() {
        guard let draft else { return }
        guard draft.isValid else {
            lastError = draft.idError ?? draft.displayNameError ?? draft.argsError
            return
        }
        let wire = AgentProfileInfo(id: draft.id, base: draft.base, displayName: draft.displayName,
                                    enabled: draft.enabled, args: draft.args, builtin: false)
        if draft.isNew {
            // Remembered so `apply(profiles:)` can adopt the pane once the broadcast confirms it —
            // see `pendingAddID`. Only set once the request was actually dispatched, so a local send
            // failure can't leave a stale id waiting to be (mis)matched by some later, unrelated add.
            if dispatch(.addAgentProfile(wire)) { pendingAddID = draft.id }
        } else {
            dispatch(.updateAgentProfile(wire))
        }
    }

    public func remove(_ id: String) {
        if profiles.first(where: { $0.id == id })?.builtin == true {
            lastError = "\(id) is a built-in agent and cannot be removed — disable it instead."
            return
        }
        dispatch(.removeAgentProfile(id: id))
    }

    /// Point the pane at a stored profile: selection, baseline and a fresh non-new draft, together,
    /// so the three can never drift apart.
    private func adopt(_ p: AgentProfileInfo) {
        selected = p.id
        baseline = p
        draft = AgentProfileDraft(id: p.id, base: p.base, displayName: p.displayName,
                                  enabled: p.enabled, args: p.args, isNew: false)
    }

    @discardableResult
    private func dispatch(_ req: ControlRequest) -> Bool {
        do {
            try send(req)
            return true
        } catch {
            lastError = "Could not reach the daemon: \(error.localizedDescription)"
            return false
        }
    }

    /// `<id>-copy`, then `-copy2`, `-copy3`… — always a valid id, never a collision.
    private func freshID(basedOn id: String) -> String {
        let taken = Set(profiles.map(\.id))
        var candidate = "\(id)-copy"
        var n = 2
        while taken.contains(candidate) {
            candidate = "\(id)-copy\(n)"
            n += 1
        }
        return candidate
    }
}
