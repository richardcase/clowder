import AppKit
import SwiftUI
import GhosttyKit
import ClowderCore

// Read by the C wakeup callback (which can't capture Swift context).
var gApp: ghostty_app_t?

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    // Optionals, not implicitly-unwrapped: SwiftUI can evaluate the scene body BEFORE
    // applicationDidFinishLaunching on some macOS versions, so nothing may force-unwrap these.
    private(set) var appModel: AppModel?
    private(set) var surfaceHost: SurfaceHost?
    private var mainWindow: NSWindow?
    private var windowCloseDelegate: HideOnCloseDelegate?
    private var statusBar: StatusBarController?
    /// One supervisor per backend the app has launched. Local's outlives a switch away from it (it
    /// is detached, not stopped) so its agents — PTY children of that daemon — survive; a forwarder
    /// is stopped and dropped, since it holds no state.
    private var supervisors: [BackendID: DaemonSupervisor] = [:]
    private var hostRegistry: HostRegistry?
    private(set) var hosts: [RemoteHost] = []
    private(set) var activeBackend: BackendID = .local
    private var sockets = SocketPaths(client: "", control: "", hook: "")

    /// One-time libghostty + model initialization. Idempotent and main-thread-only; runs on
    /// whichever fires first — the SwiftUI scene body or `applicationDidFinishLaunching` — so
    /// the app never depends on that ordering (the launch-order dependency was crashing at
    /// startup). Creating the ghostty app object here is run-loop-independent; the wakeup tick
    /// is queued via DispatchQueue.main and serviced once the run loop is up.
    @discardableResult
    func bootstrap() -> (appModel: AppModel, surfaceHost: SurfaceHost) {
        if let appModel, let surfaceHost { return (appModel, surfaceHost) }

        // Bundled binary + per-user sockets (dev overrides via env/CLOWDER_BIN still honored).
        let socks = ClowderPaths.socketPaths()
        sockets = SocketPaths(client: socks.client, control: socks.control, hook: socks.hook)
        let clowderBinary = ProcessInfo.processInfo.environment["CLOWDER_BIN"]
            ?? ClowderPaths.bundledBin("clowder")
            ?? FileManager.default.currentDirectoryPath + "/../target/debug/clowder"

        // The CLI owns config.toml + hosts.json parsing, so the app reads the host list through it
        // rather than parsing either itself.
        hostRegistry = HostRegistry(runner: ProcessCommandRunner(executablePath: clowderBinary))
        refreshHosts()

        // Always start Local. Unlike pre-M11b, a configured `[remote] host` no longer changes what
        // the app connects to at launch — the user picks a backend, and the chip says which it is.
        let plan = backendPlan(target: .local, sockets: sockets)
        let controlPath = plan.controlPath
        let socketPath = plan.renderPath
        if let supervisor = makeBackendSupervisor(plan: plan) {
            supervisors[.local] = supervisor
            supervisor.start()
        }
        // Unbundled dev (no supervisor): the local plan's sockets are the default ones, which is
        // exactly where a hand-run `cargo run -p clowder-daemon` binds. Nothing to adjust.

        // --- libghostty init (unchanged sequence, relocated from main.swift) ---
        guard ghostty_init(UInt(CommandLine.argc), CommandLine.unsafeArgv) == GHOSTTY_SUCCESS else {
            fatalError("clowder: ghostty_init failed")
        }
        let config = ghostty_config_new()
        ghostty_config_finalize(config)

        var runtime = ghostty_runtime_config_s()
        runtime.userdata = nil
        runtime.supports_selection_clipboard = false
        runtime.wakeup_cb = { _ in
            DispatchQueue.main.async { if let a = gApp { ghostty_app_tick(a) } }
        }
        runtime.action_cb = { _, _, _ in false }
        runtime.read_clipboard_cb = { _, _, _ in false }
        runtime.confirm_read_clipboard_cb = { _, _, _, _ in }
        runtime.write_clipboard_cb = { _, _, _, _, _ in }
        runtime.close_surface_cb = { _, _ in }

        guard let app = ghostty_app_new(&runtime, config) else {
            fatalError("clowder: ghostty_app_new failed")
        }
        gApp = app
        ghostty_app_set_focus(app, true)

        // --- model + surface registry ---
        let host = SurfaceHost(app: app, clowderBinary: clowderBinary, socketPath: socketPath)
        let model = AppModel(makeTransport: { try UnixSocketConnection(path: controlPath) })
        surfaceHost = host
        appModel = model
        model.backends = self
        model.setHosts(hosts)
        model.connect()
        // Task 11 replaces these three closures with the `BackendSwitching` reference above; until
        // then, adapt the tray's existing host-name shape onto `activeBackend`/`hosts`.
        statusBar = StatusBarController(appModel: model,
                                        showWindow: { [weak self] in self?.showWindow() },
                                        remoteHost: { [weak self] in self?.activeBackend.hostID?.rawValue },
                                        configuredRemoteHost: { [weak self] in self?.hosts.first?.name },
                                        switchBackend: { [weak self] name in
                                            self?.switchBackend(to: name.map { .remote(HostID($0)) } ?? .local)
                                        })
        return (model, host)
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        bootstrap()
        // A real .app bundle is frontmost on launch. Only force activation when running UNBUNDLED
        // (dev `swift run clowder-app`), where a bare executable would otherwise not become active.
        if Bundle.main.bundleIdentifier == nil {
            NSApp.setActivationPolicy(.regular)
            NSApp.activate(ignoringOtherApps: true)
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        appModel?.shutdown()                          // F1: explicit disconnect
        // Quit means quit: every backend we spawned, including a DETACHED local daemon we left
        // running across a switch, gets terminated. `stop()` clears the detached flag first, so the
        // retained handle is actually signalled rather than orphaned.
        for (_, supervisor) in supervisors { supervisor.stop() }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        showWindow()
        return true
    }

    /// Called by WindowAccessor when the window attaches. Runs once; makes the red close
    /// button hide (not destroy) the window so the app stays menu-bar-resident.
    func adoptWindow(_ window: NSWindow) {
        guard mainWindow == nil else { return }
        mainWindow = window
        window.isReleasedWhenClosed = false
        let d = HideOnCloseDelegate()
        windowCloseDelegate = d
        window.delegate = d
    }

    /// Bring the (possibly hidden) window to the front.
    func showWindow() {
        mainWindow?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }
}

extension AppDelegate: BackendSwitching {
    /// Read the registry. Cheap enough to do on demand (menu/palette/settings open); there is
    /// deliberately no file watcher.
    ///
    /// Never throws into the UI: no registry at all is the normal state before the user adds a
    /// host, so a failure logs and leaves the list as it was.
    func refreshHosts() {
        guard let hostRegistry else { return }
        do {
            hosts = try hostRegistry.list()
            appModel?.setHosts(hosts)
        } catch {
            DaemonLog.note("could not read the host registry: \(error.localizedDescription)")
        }
    }

    /// Live backend swap. Reconfigures the SAME AppModel + SurfaceHost in place, so SwiftUI keeps
    /// its bindings.
    ///
    /// Local is DETACHED rather than stopped: its agents are PTY children that do not survive a
    /// restart, and switching back re-adopts the same daemon. A forwarder is STOPPED — it holds no
    /// state, and leaving one bound would collide when we reconnect to that host.
    func switchBackend(to backend: BackendID) {
        guard backend != activeBackend else { return }
        let target: BackendTarget
        switch backend {
        case .local:
            target = .local
        case let .remote(id):
            guard let host = hosts.first(where: { $0.id == id }) else {
                appModel?.reportBackendError("No host named \(id.rawValue) is configured.")
                return
            }
            // Probe first: refusing costs ~3s, whereas switching to a dead host tears down a
            // healthy session and leaves the user with a red chip and nothing running.
            if let probe = try? hostRegistry?.probe(name: host.name), probe.reachable == false {
                appModel?.reportBackendError(
                    "Cannot reach \(host.name) at \(host.address). \(probe.error ?? "")")
                return
            }
            target = .remote(host)
        }

        let plan = backendPlan(target: target, sockets: sockets)
        // Build (or recover) the new supervisor BEFORE touching the current one: if we can't
        // (unbundled dev has no bundled binaries), the running backend must be left alone.
        guard let supervisor = supervisors[backend] ?? makeBackendSupervisor(plan: plan) else {
            DaemonLog.note("no bundled binary to run backend \(backend); staying on \(activeBackend)")
            return
        }

        if activeBackend == .local {
            supervisors[.local]?.detach()
        } else {
            supervisors[activeBackend]?.stop()
            supervisors[activeBackend] = nil
        }

        supervisors[backend] = supervisor
        if supervisor.state == .detached { supervisor.resume() } else { supervisor.start() }
        activeBackend = backend
        appModel?.reconnect(to: backend, makeTransport: {
            try UnixSocketConnection(path: plan.controlPath)
        })
        surfaceHost?.retarget(socketPath: plan.renderPath)
    }
}

/// Hides the window on close instead of destroying it, so the app stays alive in the menu bar.
final class HideOnCloseDelegate: NSObject, NSWindowDelegate {
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        sender.orderOut(nil)
        return false
    }
}

@main
struct ClowderApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    private let keymap = Keymap()

    var body: some Scene {
        WindowGroup {
            // Bootstrap on first body evaluation if the launch callback hasn't run yet;
            // idempotent, so a later applicationDidFinishLaunching is a no-op.
            let boot = delegate.bootstrap()
            // Task 10 removes `isRemote:` in favour of deriving it from `model.activeBackend`
            // inside the view; until then this reads the delegate at body-evaluation time.
            ContentView(surfaceHost: boot.surfaceHost, isRemote: delegate.activeBackend != .local)
                .environmentObject(boot.appModel)
                .frame(minWidth: 900, minHeight: 560)
                .background(WindowAccessor { [weak d = delegate] window in d?.adoptWindow(window) })
        }
        .commands {
            // clowder is a single-window app; remove the default File > New Window (frees ⌘N
            // for New Worktree instead of opening a second window).
            CommandGroup(replacing: .newItem) { }

            CommandMenu("clowder") {
                menuItem("Command Palette", .openPalette)
                menuItem("New Worktree", .newWorktree)
                menuItem("Add Project", .addProject)
                menuItem("Next Attention", .nextAttention)
                Divider()
                ForEach(1...9, id: \.self) { i in
                    menuItem("Switch to Agent \(i)", .switchToAgent(i))
                }
                Divider()
                menuItem("Split Right", .splitRight)
                menuItem("Split Down", .splitDown)
                menuItem("Close Pane", .closePane)
                menuItem("Focus Next Pane", .focusNextPane)
                Divider()
                menuItem("Land Agent", .landAgent)
                Button("Discard Agent…") { delegate.appModel?.run(.discardAgent) }
            }
        }
    }

    // A menu button that runs a command via the shared AppModel and carries its shortcut.
    // Disabled (and so ignoring its keyboard shortcut too — SwiftUI Commands are NSMenu-backed)
    // whenever the model says the command doesn't apply to the current selection.
    @ViewBuilder
    private func menuItem(_ title: String, _ id: CommandID) -> some View {
        Button(title) { delegate.appModel?.run(id) }
            .keyboardShortcut(shortcut(id))
            .disabled(!(delegate.appModel?.isEnabled(id) ?? true))
    }

    private func shortcut(_ id: CommandID) -> KeyboardShortcut {
        guard let b = keymap.binding(for: id) else {
            assertionFailure("no key binding for \(id)")
            return KeyboardShortcut("?", modifiers: [])
        }
        return KeyboardShortcut(KeyEquivalent(b.key), modifiers: eventModifiers(b.modifiers))
    }

    private func eventModifiers(_ m: KeyModifiers) -> EventModifiers {
        var e: EventModifiers = []
        if m.contains(.command) { e.insert(.command) }
        if m.contains(.shift)   { e.insert(.shift) }
        if m.contains(.option)  { e.insert(.option) }
        if m.contains(.control) { e.insert(.control) }
        return e
    }
}
