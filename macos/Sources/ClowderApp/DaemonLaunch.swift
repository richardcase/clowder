import Foundation
import ClowderCore

/// Where clowder's per-user sockets and bundled binaries live.
enum ClowderPaths {
    /// $XDG_RUNTIME_DIR › $TMPDIR › /tmp, then `/clowder` (matches clowder-config + the daemon PID lock).
    static func runtimeDir() -> String {
        let env = ProcessInfo.processInfo.environment
        let base = env["XDG_RUNTIME_DIR"].flatMap { $0.isEmpty ? nil : $0 }
            ?? env["TMPDIR"].flatMap { $0.isEmpty ? nil : $0 }
            ?? "/tmp"
        return (base as NSString).appendingPathComponent("clowder")
    }

    /// Per-user socket paths, honoring the same env overrides as clowder-config.
    static func socketPaths() -> (client: String, control: String, hook: String) {
        let env = ProcessInfo.processInfo.environment
        let dir = runtimeDir()
        func p(_ envKey: String, _ name: String) -> String {
            env[envKey].flatMap { $0.isEmpty ? nil : $0 } ?? (dir as NSString).appendingPathComponent(name)
        }
        return (p("CLOWDER_SOCK", "clowder.sock"),
                p("CLOWDER_CONTROL_SOCK", "clowder-control.sock"),
                p("CLOWDER_HOOK_SOCK", "clowder-hook.sock"))
    }

    /// A binary bundled at Contents/Resources/bin/<name>, or nil when running unbundled (swift run).
    static func bundledBin(_ name: String) -> String? {
        guard let res = Bundle.main.resourcePath else { return nil }
        let path = (res as NSString).appendingPathComponent("bin/\(name)")
        return FileManager.default.isExecutableFile(atPath: path) ? path : nil
    }
}

/// A real clowder-daemon child process (Foundation.Process). Fires onExit on the main actor.
final class ProcessDaemon: DaemonProcess {
    private let process = Process()
    private var onExit: ((Int32) -> Void)?
    private var launchFailed = false

    init(execPath: String, env: [String: String]) {
        process.executableURL = URL(fileURLWithPath: execPath)
        process.environment = env
        process.terminationHandler = { [weak self] p in
            let code = p.terminationStatus
            Task { @MainActor in self?.onExit?(code) }
        }
        do {
            try process.run()
        } catch {
            // Launch failed → the terminationHandler will never fire. Record it and deliver a
            // synthetic crash exit once the supervisor registers onExit, so it relaunches (backoff)
            // instead of being stuck in a false ".running".
            FileHandle.standardError.write(Data("clowder: failed to launch daemon at \(execPath): \(error)\n".utf8))
            launchFailed = true
        }
    }

    func terminate() { if process.isRunning { process.terminate() } }   // SIGTERM → M5b graceful shutdown

    func setOnExit(_ handler: @escaping (Int32) -> Void) {
        onExit = handler
        if launchFailed {
            // -1 = crash-style (NOT 1, which means "lost the single-instance flock") → backoff relaunch.
            Task { @MainActor in handler(-1) }
        }
    }
}

/// Build a supervisor that spawns the bundled daemon with per-user sockets + bundled bin/ on PATH.
/// Returns nil when unbundled (dev `swift run`) so the developer keeps starting the daemon by hand.
@MainActor
func makeDaemonSupervisor() -> DaemonSupervisor? {
    guard let daemonPath = ClowderPaths.bundledBin("clowder-daemon"),
          let res = Bundle.main.resourcePath else { return nil }
    let socks = ClowderPaths.socketPaths()
    var env = ProcessInfo.processInfo.environment
    env["CLOWDER_SOCK"] = socks.client
    env["CLOWDER_CONTROL_SOCK"] = socks.control
    env["CLOWDER_HOOK_SOCK"] = socks.hook
    let binDir = (res as NSString).appendingPathComponent("bin")
    env["PATH"] = binDir + ":" + (env["PATH"] ?? "/usr/bin:/bin")
    return DaemonSupervisor(spawn: { ProcessDaemon(execPath: daemonPath, env: env) })
}
