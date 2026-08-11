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

    private let send: (ControlRequest) throws -> Void

    public init(send: @escaping (ControlRequest) throws -> Void) {
        self.send = send
    }

    public var selectedProfile: AgentProfileInfo? {
        selected.flatMap { id in profiles.first { $0.id == id } }
    }

    /// Built-ins can be edited and disabled but never removed — the daemon refuses it too.
    public var canRemoveSelection: Bool { selectedProfile.map { !$0.builtin } ?? false }

    /// Whether `draft` differs from what the daemon holds, so Save/Revert only light up when there
    /// is something to save or discard.
    public var isDirty: Bool {
        guard let draft else { return false }
        guard !draft.isNew else { return draft != AgentProfileDraft() }
        guard let p = selectedProfile else { return true }
        return draft.displayName != p.displayName || draft.enabled != p.enabled
            || draft.args != p.args || draft.base != p.base
    }

    /// The editor's live preview of the resolved arguments.
    public var preview: String { draft.map { AgentArgs.preview($0.args) } ?? "" }

    public func dismissError() { lastError = nil }

    /// Adopt a list from the daemon. Keeps the current selection and any in-progress edit, unless
    /// the selected profile has gone away.
    public func apply(profiles: [AgentProfileInfo]) {
        self.profiles = profiles
        guard let selected else { return }
        if !profiles.contains(where: { $0.id == selected }) {
            self.selected = nil
            draft = nil
        }
    }

    public func reload() { dispatch(.listAgentProfiles) }

    public func select(_ id: String?) {
        selected = id
        guard let p = id.flatMap({ i in profiles.first { $0.id == i } }) else {
            draft = nil
            return
        }
        draft = AgentProfileDraft(id: p.id, base: p.base, displayName: p.displayName,
                                  enabled: p.enabled, args: p.args, isNew: false)
    }

    public func beginAdd() {
        selected = nil
        draft = AgentProfileDraft()
    }

    /// A local, unsaved copy of the selection under a fresh id — how a user makes "Claude (Opus)"
    /// from "Claude Code" without inventing a program.
    public func duplicateSelected() {
        guard let p = selectedProfile else { return }
        selected = nil
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
        dispatch(draft.isNew ? .addAgentProfile(wire) : .updateAgentProfile(wire))
    }

    public func remove(_ id: String) {
        if profiles.first(where: { $0.id == id })?.builtin == true {
            lastError = "\(id) is a built-in agent and cannot be removed — disable it instead."
            return
        }
        dispatch(.removeAgentProfile(id: id))
    }

    private func dispatch(_ req: ControlRequest) {
        do { try send(req) } catch { lastError = "Could not reach the daemon: \(error.localizedDescription)" }
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
