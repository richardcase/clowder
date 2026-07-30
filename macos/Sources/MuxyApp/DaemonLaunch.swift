import Foundation
import MuxyCore

/// Where muxy's per-user sockets and bundled binaries live.
enum MuxyPaths {
    /// $XDG_RUNTIME_DIR › $TMPDIR › /tmp, then `/muxy` (matches muxy-config + the daemon PID lock).
    static func runtimeDir() -> String {
        let env = ProcessInfo.processInfo.environment
        let base = env["XDG_RUNTIME_DIR"].flatMap { $0.isEmpty ? nil : $0 }
            ?? env["TMPDIR"].flatMap { $0.isEmpty ? nil : $0 }
            ?? "/tmp"
        return (base as NSString).appendingPathComponent("muxy")
    }

    /// Per-user socket paths, honoring the same env overrides as muxy-config.
    static func socketPaths() -> (client: String, control: String, hook: String) {
        let env = ProcessInfo.processInfo.environment
        let dir = runtimeDir()
        func p(_ envKey: String, _ name: String) -> String {
            env[envKey].flatMap { $0.isEmpty ? nil : $0 } ?? (dir as NSString).appendingPathComponent(name)
        }
        return (p("MUXY_SOCK", "muxy.sock"),
                p("MUXY_CONTROL_SOCK", "muxy-control.sock"),
                p("MUXY_HOOK_SOCK", "muxy-hook.sock"))
    }

    /// A binary bundled at Contents/Resources/bin/<name>, or nil when running unbundled (swift run).
    static func bundledBin(_ name: String) -> String? {
        guard let res = Bundle.main.resourcePath else { return nil }
        let path = (res as NSString).appendingPathComponent("bin/\(name)")
        return FileManager.default.isExecutableFile(atPath: path) ? path : nil
    }
}

/// A real muxy-daemon child process (Foundation.Process). Fires onExit on the main actor.
final class ProcessDaemon: DaemonProcess {
    private let process = Process()
    private var onExit: ((Int32) -> Void)?

    init(execPath: String, env: [String: String]) {
        process.executableURL = URL(fileURLWithPath: execPath)
        process.environment = env
        process.terminationHandler = { [weak self] p in
            let code = p.terminationStatus
            Task { @MainActor in self?.onExit?(code) }
        }
        try? process.run()
    }

    func terminate() { if process.isRunning { process.terminate() } }   // SIGTERM → M5b graceful shutdown
    func setOnExit(_ handler: @escaping (Int32) -> Void) { onExit = handler }
}

/// Build a supervisor that spawns the bundled daemon with per-user sockets + bundled bin/ on PATH.
/// Returns nil when unbundled (dev `swift run`) so the developer keeps starting the daemon by hand.
@MainActor
func makeDaemonSupervisor() -> DaemonSupervisor? {
    guard let daemonPath = MuxyPaths.bundledBin("muxy-daemon"),
          let res = Bundle.main.resourcePath else { return nil }
    let socks = MuxyPaths.socketPaths()
    var env = ProcessInfo.processInfo.environment
    env["MUXY_SOCK"] = socks.client
    env["MUXY_CONTROL_SOCK"] = socks.control
    env["MUXY_HOOK_SOCK"] = socks.hook
    let binDir = (res as NSString).appendingPathComponent("bin")
    env["PATH"] = binDir + ":" + (env["PATH"] ?? "/usr/bin:/bin")
    return DaemonSupervisor(spawn: { ProcessDaemon(execPath: daemonPath, env: env) })
}
