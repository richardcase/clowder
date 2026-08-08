import Foundation
import Combine

public enum LifecycleAction: Equatable, Sendable { case land, discard }

/// A Land/Discard awaiting user confirmation (captured at request time so the confirmation
/// UI can show which worktree name is affected, even if selection changes before confirming).
public struct PendingLifecycle: Equatable, Sendable {
    public let action: LifecycleAction
    public let pane: UInt64
    public let name: String
    public init(action: LifecycleAction, pane: UInt64, name: String) {
        self.action = action
        self.pane = pane
        self.name = name
    }
}

/// Who owns backend processes and the host list. `AppDelegate` conforms; the chip, the menu bar,
/// and the command palette all read this one source rather than each holding their own closures.
@MainActor
public protocol BackendSwitching: AnyObject {
    var hosts: [RemoteHost] { get }
    var activeBackend: BackendID { get }
    func switchBackend(to backend: BackendID)
    func refreshHosts()
}

/// Owns the control channel and the app's selection. Libghostty-free so it is unit-testable.
/// Retaining `session` is what keeps ControlSession's `[weak self]` receiver alive.
@MainActor
public final class AppModel: ObservableObject {
    public enum ConnectionState: Equatable {
        case connecting
        case live
        case reconnecting
        case closed(reason: String)
    }

    public let store: AgentStore
    @Published public var selection: SidebarSelection? {
        didSet {
            if selection != nil { pendingRestore = nil }
            focusedPane = selectedPane          // focus the root pane on (re)select
            switch selection {
            case let .worktree(pane):
                try? session?.send(.getSplitTree(agent: pane))
            case let .project(path):
                if let pane = store.projectTerminals[path] {
                    try? session?.send(.getSplitTree(agent: pane))
                } else {
                    // Not open yet — ask. `projectTerminalOpened` will populate the mapping,
                    // and the daemon's open is idempotent, so a duplicate ask is harmless.
                    try? session?.send(.openProjectTerminal(path: path))
                }
            case nil:
                break
            }
        }
    }

    /// The root pane of the current selection. **Derived, never stored** — this is what lets
    /// `currentTree`, `focusedPane`, `splitFocused`, `closeFocused` and `SplitContainer` keep
    /// working unchanged, since they always meant "the selection's root pane".
    public var selectedPane: UInt64? {
        switch selection {
        case let .worktree(pane): return pane
        case let .project(path): return store.projectTerminals[path]
        case nil: return nil
        }
    }
    @Published public var focusedPane: UInt64?
    @Published public private(set) var connectionState: ConnectionState = .connecting
    @Published public var showingPalette: Bool = false
    @Published public var showingAddProject: Bool = false
    @Published public var showingNewWorktree: Bool = false
    /// Which project the New Worktree sheet should prefill. Set by the per-project `+` and the
    /// context menu; `.newWorktree` from the palette or Cmd-N leaves it as-is, so the sheet falls
    /// back to the current selection's project or the first project.
    @Published public var newWorktreeProject: String = ""
    @Published public var pendingLifecycle: PendingLifecycle?

    /// Paths whose project terminal was observed open at least once and is not currently open —
    /// e.g. the user typed `exit`. Lets the detail view distinguish "closed, offer Reopen" from
    /// "still opening" (the same nil-`selectedPane` state a fresh, never-opened selection is in)
    /// so it doesn't render a permanent spinner. Deliberately does NOT auto-reopen — looping
    /// against a shell that exits immediately would hang the UI; the user must ask via Reopen.
    @Published public private(set) var closedProjectTerminals: Set<String> = []
    /// Paths whose terminal has been live at least once (drives `closedProjectTerminals` above).
    private var everOpenedProjectPaths: Set<String> = []

    /// Which backend the control channel is pointed at.
    @Published public private(set) var activeBackend: BackendID = .local
    /// The known remote hosts, as last read from the registry.
    @Published public private(set) var hosts: [RemoteHost] = []

    /// Owns the backend processes. `weak` because the delegate owns this model.
    public weak var backends: BackendSwitching?

    /// Where the user was in each backend, so switching feels like tabs rather than a restart.
    /// `reconnect` necessarily clears `selection` — the new backend's panes are different — and
    /// this is what puts it back on return.
    private var lastSelection: [BackendID: SidebarSelection] = [:]

    /// A selection to re-apply once the incoming backend's worktrees arrive. Cleared after one
    /// successful restore (or when the user selects something themselves).
    private var pendingRestore: SidebarSelection?

    public func setHosts(_ hosts: [RemoteHost]) { self.hosts = hosts }

    public func requestSwitch(to backend: BackendID) { backends?.switchBackend(to: backend) }
    public func requestHostRefresh() { backends?.refreshHosts() }

    private var makeTransport: () throws -> ControlTransport
    private var connection: ControlTransport?
    private var session: ControlSession?
    private var storeSubscription: AnyCancellable?
    private let sleepFn: (TimeInterval) async -> Void
    private var reconnectTask: Task<Void, Never>?
    private var isShuttingDown = false

    public init(store: AgentStore = AgentStore(),
                makeTransport: @escaping () throws -> ControlTransport,
                sleep: @escaping (TimeInterval) async -> Void = { d in
                    try? await Task.sleep(nanoseconds: UInt64(max(0, d) * 1_000_000_000))
                }) {
        self.store = store
        self.makeTransport = makeTransport
        self.sleepFn = sleep
        // Republish nested store changes so SwiftUI views observing AppModel refresh
        // when agents/attention/lastError mutate (nested ObservableObject changes do
        // not cascade to the parent automatically under Combine).
        self.storeSubscription = store.objectWillChange.sink { [weak self] _ in
            self?.objectWillChange.send()
            DispatchQueue.main.async {
                self?.restorePendingSelectionIfPossible()
                self?.reconcileFocus()
                self?.reconcileProjectSelection()
            }
        }
    }

    /// Build the transport + session and hydrate.
    ///
    /// On first launch the app spawns the daemon and connects immediately, so the socket routinely
    /// does not exist yet (`connect(2)` → ENOENT) — `Process.run()` returns at fork/exec, long before
    /// the daemon has bound anything. Giving up here left a terminal red banner with no Retry, cured
    /// only by relaunching the app (which then found the *first* launch's daemon already listening).
    ///
    /// So retry, exactly as `reconnect()` does. The first `graceAttempts` are held in `.connecting` —
    /// which renders no banner — so an ordinary cold start is invisible; only a genuinely slow or
    /// broken daemon escalates to the orange `.reconnecting` state.
    public func connect() {
        isShuttingDown = false
        connectionState = .connecting
        do {
            try attemptConnect()
        } catch {
            scheduleReconnect(graceAttempts: Self.startupGraceAttempts)
        }
    }

    /// Point the control channel at a different backend (a live local↔remote swap): tear down the
    /// current connection + reconnect loop, drop the previous backend's agents, then connect to the
    /// new transport. Keeps the same `AppModel` instance so SwiftUI views stay bound.
    ///
    /// Remembers the outgoing backend's selection and restores the incoming one's once the
    /// connection is live and its worktrees have arrived.
    public func reconnect(to backend: BackendID,
                          makeTransport newMakeTransport: @escaping () throws -> ControlTransport) {
        if let current = selection { lastSelection[activeBackend] = current }
        shutdown()                       // cancel reconnect, disconnect, clear session/connection
        store.reset()                    // drop the previous backend's agents/trees
        selection = nil
        activeBackend = backend
        pendingRestore = lastSelection[backend]
        self.makeTransport = newMakeTransport
        isShuttingDown = false
        connectionState = .connecting
        do {
            try attemptConnect()
        } catch {
            // The freshly-started backend may still be binding its socket — retry with backoff
            // (the same bounded loop as a live drop) rather than giving up in `.closed`.
            scheduleReconnect()
        }
    }

    /// One connection attempt: build the transport, wire close→reconnect, hydrate. Throws on failure.
    private func attemptConnect() throws {
        let transport = try makeTransport()
        // Guard on connection identity: the real transport fires onClose ASYNCHRONOUSLY, so a
        // transport we've already replaced (e.g. during a live backend swap) can deliver a late close
        // — ignore it, or it would flip the healthy new connection back into reconnecting.
        transport.setOnClose { [weak self, weak transport] in
            guard let self, self.connection === transport else { return }
            self.handleClose()
        }
        let session = ControlSession(transport: transport, store: store)
        self.connection = transport
        self.session = session
        connectionState = .live
        try session.send(.listWorktrees)
        try session.send(.listAdapters)
        try session.send(.listProjects)
        store.clearProjectTerminals()
    }

    /// The transport closed. Unless we're explicitly shutting down, start reconnecting.
    private func handleClose() {
        guard !isShuttingDown else { return }
        scheduleReconnect()
    }

    /// Retries spent in `.connecting` before showing the user anything. 5 attempts on the fast ramp
    /// below is ~1.5s — comfortably longer than a daemon takes to bind, short enough that a real
    /// failure still surfaces promptly.
    private static let startupGraceAttempts = 5

    private func backoffDelay(_ attempt: Int) -> TimeInterval {
        min(10.0, 0.5 * pow(2.0, Double(attempt)))
    }

    /// Delay for an attempt inside the startup grace period: 50/100/200/400/800ms. Deliberately
    /// separate from `backoffDelay` — the live-drop cadence is established behaviour, and a dropped
    /// connection has no reason to be probed this eagerly.
    private func graceDelay(_ attempt: Int) -> TimeInterval {
        0.05 * pow(2.0, Double(attempt))
    }

    /// Start the exponential-backoff reconnect loop (idempotent while one is running).
    ///
    /// `graceAttempts` > 0 keeps the first N attempts in `.connecting` on a fast ramp, so a
    /// cold start that resolves quickly never shows a banner. Past that, or when 0 (a live
    /// connection dropping), it behaves exactly as before: `.reconnecting` + `backoffDelay`.
    private func scheduleReconnect(graceAttempts: Int = 0) {
        guard !isShuttingDown, reconnectTask == nil else { return }
        connectionState = graceAttempts > 0 ? .connecting : .reconnecting
        reconnectTask = Task { [weak self] in await self?.reconnectLoop(graceAttempts: graceAttempts) }
    }

    private func reconnectLoop(graceAttempts: Int = 0) async {
        var attempt = 0
        while !Task.isCancelled && !isShuttingDown {
            let inGrace = attempt < graceAttempts
            await sleepFn(inGrace ? graceDelay(attempt) : backoffDelay(attempt - graceAttempts))
            if Task.isCancelled || isShuttingDown { break }
            do {
                try attemptConnect()          // sets .live + re-hydrates on success
                reconnectTask = nil
                return
            } catch {
                // A mid-hydration failure may have flipped us to .live; put the state back. Stay
                // silent (`.connecting`) while still inside the grace period.
                connectionState = attempt + 1 < graceAttempts ? .connecting : .reconnecting
                attempt += 1
            }
        }
        reconnectTask = nil
    }

    public func spawn(project: String, name: String, adapter: String) {
        guard let session else { return }
        do {
            try session.send(.spawnAgent(project: project, name: name, adapter: adapter))
        } catch {
            connectionState = .closed(reason: "Send failed: \(error)")
        }
    }

    /// Select the 1-based Nth agent in the ordered list (Cmd-N). No-op if out of range.
    public func selectAgent(atIndex index: Int) {
        let ordered = store.orderedWorktrees
        guard index >= 1, index <= ordered.count else { return }
        selection = .worktree(ordered[index - 1].pane)
    }

    /// Select the next agent needing input after the current selection, cycling. If the
    /// current selection isn't needy, select the first needy one; no-op if none need input.
    public func selectNextAttention() {
        let needy = store.orderedWorktrees.filter { $0.state == .needsInput }
        guard !needy.isEmpty else { return }
        if let cur = selectedPane, let idx = needy.firstIndex(where: { $0.pane == cur }) {
            selection = .worktree(needy[(idx + 1) % needy.count].pane)
        } else {
            selection = .worktree(needy[0].pane)
        }
    }

    /// The selected agent's split tree, or nil (the detail falls back to a lone leaf).
    public var currentTree: PaneTree? { selectedPane.flatMap { store.trees[$0] } }

    public var adapters: [AdapterInfo] { store.adapters }

    public func splitFocused(_ direction: SplitDirection) {
        guard let target = focusedPane ?? selectedPane, session != nil else { return }
        try? session?.send(.splitPane(pane: target, direction: direction))
    }

    public func closeFocused() {
        // Only companions are closable here; closing the agent pane is teardown (out of scope).
        guard let f = focusedPane, f != selectedPane, session != nil else { return }
        try? session?.send(.closePane(pane: f))
        focusedPane = selectedPane            // optimistic: the leaf is going away
    }

    public func focusNextPane() {
        guard let leaves = currentTree?.leaves, !leaves.isEmpty else { return }
        if let f = focusedPane, let i = leaves.firstIndex(of: f) {
            focusedPane = leaves[(i + 1) % leaves.count]
        } else {
            focusedPane = leaves.first
        }
    }

    /// Send a new divider ratio (clamped to [0.05, 0.95], matching the daemon) to the daemon.
    public func setDividerRatio(split: UInt64, ratio: Double) {
        guard session != nil else { return }
        let r = min(0.95, max(0.05, ratio))
        try? session?.send(.setSplitRatio(split: split, ratio: r))
    }

    /// Re-apply a remembered selection once its target exists in the new backend's store.
    /// Silently gives up if the pane or project is gone — a worktree may have been landed on the
    /// other machine since we were last here.
    private func restorePendingSelectionIfPossible() {
        guard let want = pendingRestore, selection == nil else { return }
        switch want {
        case let .worktree(pane):
            guard store.worktrees[pane] != nil else { return }
        case let .project(path):
            guard store.projects.contains(where: { $0.path == path }) else { return }
        }
        pendingRestore = nil
        selection = want
    }

    /// If the focused pane is no longer a leaf of the current tree (a companion closed, or an
    /// external tree change), move focus back to the agent pane.
    func reconcileFocus() {
        // A project selection made before its terminal opened set `focusedPane = selectedPane`
        // while `selectedPane` was still nil (see `selection`'s `didSet`). Once
        // `projectTerminalOpened` resolves the pane, nothing else sets focus — without this, the
        // guard below bails on a nil `currentTree` and the user can select the project, watch its
        // terminal appear, and type into it with no effect until they click it.
        if focusedPane == nil, let p = selectedPane { focusedPane = p; return }
        guard let leaves = currentTree?.leaves else { return }   // no tree → leave focus as-is
        if let f = focusedPane, !leaves.contains(f) {
            focusedPane = selectedPane
        }
    }

    /// Resolve `.project` selection state against the store: clear a selection whose project just
    /// left `store.projects` (removing the currently-selected row must not leave the detail pane
    /// stuck on a spinner forever — there is no "next select" to respawn it, since the row is
    /// gone), and maintain `closedProjectTerminals` from `store.projectTerminals`'s membership so
    /// the detail view can tell "closed" from "still opening".
    private func reconcileProjectSelection() {
        if case let .project(path) = selection, !store.projects.contains(where: { $0.path == path }) {
            selection = nil
        }
        // Paths no longer registered can't be selected again — stop tracking them so the sets
        // don't grow unbounded across an app session of add/remove churn.
        let registered = Set(store.projects.map(\.path))
        everOpenedProjectPaths.formIntersection(registered)
        closedProjectTerminals.formIntersection(registered)

        let live = Set(store.projectTerminals.keys)
        closedProjectTerminals.formUnion(everOpenedProjectPaths.subtracting(live))
        closedProjectTerminals.subtract(live)          // reopened (or never-closed) — not "closed"
        everOpenedProjectPaths.formUnion(live)
    }

    /// Whether a command applies to the current selection. The palette dims disabled rows and
    /// key handling ignores them, so the UI never offers something the daemon would refuse.
    public func isEnabled(_ id: CommandID) -> Bool {
        switch id {
        case .landAgent, .discardAgent: return selectedWorktree != nil
        case .restartWorktree:          return canRestartSelection
        case .closePane:                return focusedPane != nil && focusedPane != selectedPane
        default:                        return true
        }
    }

    /// Dispatch a command by id.
    public func run(_ id: CommandID) {
        switch id {
        case .openPalette: showingPalette.toggle()
        case .newWorktree:
            if case let .project(path) = selection {
                newWorktreeProject = path
            } else if let w = selectedWorktree {
                newWorktreeProject = w.project
            }
            showingNewWorktree = true
        case .addProject: showingAddProject = true
        case .nextAttention: selectNextAttention()
        case let .switchToAgent(i): selectAgent(atIndex: i)
        case .splitRight: splitFocused(.right)
        case .splitDown: splitFocused(.down)
        case .closePane: closeFocused()
        case .focusNextPane: focusNextPane()
        case .landAgent: requestLifecycle(.land)
        case .discardAgent: requestLifecycle(.discard)
        case .restartWorktree: restartSelectedWorktree()
        }
    }

    /// Begin a Land/Discard: capture the selected pane + worktree name and await confirmation.
    public func requestLifecycle(_ action: LifecycleAction) {
        guard case let .worktree(pane) = selection, let w = store.worktrees[pane] else { return }
        pendingLifecycle = PendingLifecycle(action: action, pane: pane, name: w.name)
    }

    /// The selected worktree, if the selection is a worktree that exists.
    public var selectedWorktree: WorktreeInfo? {
        guard case let .worktree(pane) = selection else { return nil }
        return store.worktrees[pane]
    }

    /// Restart is offered only for an exited agent — the daemon refuses it otherwise.
    public var canRestartSelection: Bool { selectedWorktree?.state == .exited }

    public func restartSelectedWorktree() {
        guard canRestartSelection, case let .worktree(pane) = selection else { return }
        try? session?.send(.restartWorktree(pane: pane))
    }

    /// Ask the daemon to open (or re-open) a project's terminal. Idempotent daemon-side.
    public func openTerminal(forProject path: String) {
        try? session?.send(.openProjectTerminal(path: path))
    }

    public func addProject(path: String) { try? session?.send(.addProject(path: path)) }
    public func removeProject(path: String) { try? session?.send(.removeProject(path: path)) }

    /// Confirmed: send the request and clear.
    public func confirmLifecycle() {
        guard let p = pendingLifecycle else { return }
        switch p.action {
        case .land: try? session?.send(.landAgent(pane: p.pane))
        case .discard: try? session?.send(.discardAgent(pane: p.pane))
        }
        pendingLifecycle = nil
    }

    public func cancelLifecycle() { pendingLifecycle = nil }

    /// Dismiss the current error banner.
    public func dismissError() { store.clearLastError() }

    /// Explicit teardown (F1): cancel any reconnect loop, then disconnect. `isShuttingDown` makes the
    /// disconnect's own `onClose` a no-op so we don't re-arm reconnect while quitting.
    public func shutdown() {
        isShuttingDown = true
        reconnectTask?.cancel()
        reconnectTask = nil
        connection?.disconnect()
        session = nil
        connection = nil
    }
}
