import AppKit
import Foundation
import GhosttyKit

// The created app, read by the C wakeup callback (which can't capture context).
var gApp: ghostty_app_t?

// --- args: muxy-app <pane-id> ; env MUXY_SOCK (default /tmp/muxy.sock), MUXY_BIN ---
let args = CommandLine.arguments
guard args.count >= 2, let paneId = UInt64(args[1]) else {
    FileHandle.standardError.write(Data("usage: muxy-app <pane-id>\n".utf8))
    exit(2)
}
let socketPath = ProcessInfo.processInfo.environment["MUXY_SOCK"] ?? "/tmp/muxy.sock"
// The `muxy` client binary libghostty will run as the surface command.
let muxyBinary = ProcessInfo.processInfo.environment["MUXY_BIN"]
    ?? FileManager.default.currentDirectoryPath + "/../target/debug/muxy"

// --- libghostty init ---
guard ghostty_init(UInt(CommandLine.argc), CommandLine.unsafeArgv) == GHOSTTY_SUCCESS else {
    FileHandle.standardError.write(Data("muxy: ghostty_init failed\n".utf8))
    exit(1)
}
let config = ghostty_config_new()
ghostty_config_finalize(config)

// --- runtime config: all 6 callbacks non-null; only wakeup does real work ---
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
    FileHandle.standardError.write(Data("muxy: ghostty_app_new failed\n".utf8))
    exit(1)
}
gApp = app
ghostty_app_set_focus(app, true)

// --- NSApplication + window hosting one surface ---
let nsApp = NSApplication.shared
nsApp.setActivationPolicy(.regular)

let surfaceView = SurfaceView(app: app, paneId: paneId, muxyBinary: muxyBinary, socketPath: socketPath)

let window = NSWindow(
    contentRect: NSRect(x: 0, y: 0, width: 800, height: 500),
    styleMask: [.titled, .closable, .resizable, .miniaturizable],
    backing: .buffered,
    defer: false)
window.title = "muxy · agent \(paneId)"
window.contentView = surfaceView
window.center()
window.makeKeyAndOrderFront(nil)
window.makeFirstResponder(surfaceView)

nsApp.activate(ignoringOtherApps: true)
nsApp.run()
