import AppKit
import SwiftUI
import GhosttyKit
import MuxyCore

// Read by the C wakeup callback (which can't capture Swift context).
var gApp: ghostty_app_t?

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    // Optionals, not implicitly-unwrapped: SwiftUI can evaluate the scene body BEFORE
    // applicationDidFinishLaunching on some macOS versions, so nothing may force-unwrap these.
    private(set) var appModel: AppModel?
    private(set) var surfaceHost: SurfaceHost?

    /// One-time libghostty + model initialization. Idempotent and main-thread-only; runs on
    /// whichever fires first — the SwiftUI scene body or `applicationDidFinishLaunching` — so
    /// the app never depends on that ordering (the launch-order dependency was crashing at
    /// startup). Creating the ghostty app object here is run-loop-independent; the wakeup tick
    /// is queued via DispatchQueue.main and serviced once the run loop is up.
    @discardableResult
    func bootstrap() -> (appModel: AppModel, surfaceHost: SurfaceHost) {
        if let appModel, let surfaceHost { return (appModel, surfaceHost) }

        let muxyBinary = ProcessInfo.processInfo.environment["MUXY_BIN"]
            ?? FileManager.default.currentDirectoryPath + "/../target/debug/muxy"
        let socketPath = ProcessInfo.processInfo.environment["MUXY_SOCK"] ?? "/tmp/muxy.sock"
        let controlPath = ProcessInfo.processInfo.environment["MUXY_CONTROL_SOCK"]
            ?? "/tmp/muxy-control.sock"

        // --- libghostty init (unchanged sequence, relocated from main.swift) ---
        guard ghostty_init(UInt(CommandLine.argc), CommandLine.unsafeArgv) == GHOSTTY_SUCCESS else {
            fatalError("muxy: ghostty_init failed")
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
            fatalError("muxy: ghostty_app_new failed")
        }
        gApp = app
        ghostty_app_set_focus(app, true)

        // --- model + surface registry ---
        let host = SurfaceHost(app: app, muxyBinary: muxyBinary, socketPath: socketPath)
        let model = AppModel(makeTransport: { try UnixSocketConnection(path: controlPath) })
        surfaceHost = host
        appModel = model
        model.connect()
        return (model, host)
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        bootstrap()
    }

    func applicationWillTerminate(_ notification: Notification) {
        appModel?.shutdown()   // F1: explicit disconnect
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

@main
struct MuxyApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        WindowGroup {
            // Bootstrap on first body evaluation if the launch callback hasn't run yet;
            // idempotent, so a later applicationDidFinishLaunching is a no-op.
            let boot = delegate.bootstrap()
            ContentView(surfaceHost: boot.surfaceHost)
                .environmentObject(boot.appModel)
                .frame(minWidth: 900, minHeight: 560)
        }
    }
}
