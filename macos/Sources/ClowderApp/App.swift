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
    private var daemonSupervisor: DaemonSupervisor?
    /// The remote host the app is currently pointed at (nil = local daemon). Drives the tray label.
    private(set) var currentRemoteHost: String?

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
        let clowderBinary = ProcessInfo.processInfo.environment["CLOWDER_BIN"]
            ?? ClowderPaths.bundledBin("clowder")
            ?? FileManager.default.currentDirectoryPath + "/../target/debug/clowder"

        // Config-driven backend: ask the clowder binary for the resolved [remote] host (it owns
        // config.toml/env parsing). Non-nil → remote mode (supervise `clowder connect <host>` and
        // connect to the forwarder's sockets); nil → local daemon. Default to the local socket paths;
        // makeBackendSupervisor returns the mode's actual control/render sockets when bundled.
        let remoteHost = resolveRemoteHost(clowderBinary: clowderBinary)
        currentRemoteHost = remoteHost
        var socketPath = socks.client
        var controlPath = socks.control
        if let backend = makeBackendSupervisor(remoteHost: remoteHost) {
            daemonSupervisor = backend.supervisor
            controlPath = backend.control
            socketPath = backend.render
            backend.supervisor.start()
        }

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
        model.connect()
        statusBar = StatusBarController(appModel: model,
                                        showWindow: { [weak self] in self?.showWindow() },
                                        remoteHost: { [weak self] in self?.currentRemoteHost })
        return (model, host)
    }

    /// Ask the clowder binary for the resolved `[remote] host` (it owns config.toml/env parsing, which
    /// the app can't do itself). Returns the trimmed host, or nil when empty / the query fails.
    private func resolveRemoteHost(clowderBinary: String) -> String? {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: clowderBinary)
        proc.arguments = ["remote-host"]
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = FileHandle.nullDevice
        do {
            try proc.run()
            proc.waitUntilExit()
        } catch {
            return nil
        }
        guard proc.terminationStatus == 0 else { return nil }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let host = (String(data: data, encoding: .utf8) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        return host.isEmpty ? nil : host
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
        appModel?.shutdown()          // F1: explicit disconnect
        daemonSupervisor?.stop()      // terminate the child daemon we spawned
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
            ContentView(surfaceHost: boot.surfaceHost)
                .environmentObject(boot.appModel)
                .frame(minWidth: 900, minHeight: 560)
                .background(WindowAccessor { [weak d = delegate] window in d?.adoptWindow(window) })
        }
        .commands {
            // clowder is a single-window app; remove the default File > New Window (frees ⌘N
            // for Spawn Agent instead of opening a second window).
            CommandGroup(replacing: .newItem) { }

            CommandMenu("clowder") {
                menuItem("Command Palette", .openPalette)
                menuItem("Spawn Agent", .spawnAgent)
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
    @ViewBuilder
    private func menuItem(_ title: String, _ id: CommandID) -> some View {
        Button(title) { delegate.appModel?.run(id) }
            .keyboardShortcut(shortcut(id))
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
