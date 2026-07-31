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

    /// A binary bundled next to the app executable at Contents/MacOS/<name>, or nil when running
    /// unbundled (swift run — .build/debug/clowder-app has no Rust siblings, so this returns nil and
    /// callers fall back to CLOWDER_BIN / the dev target path).
    static func bundledBin(_ name: String) -> String? {
        guard let exeDir = Bundle.main.executableURL?.deletingLastPathComponent() else { return nil }
        let path = exeDir.appendingPathComponent(name).path
        return FileManager.default.isExecutableFile(atPath: path) ? path : nil
    }
}

/// A real clowder-daemon child process (Foundation.Process). Fires onExit on the main actor.
final class ProcessDaemon: DaemonProcess {
    private let process = Process()
    private var onExit: ((Int32) -> Void)?
    private var launchFailed = false

    init(execPath: String, args: [String] = [], env: [String: String]) {
        process.executableURL = URL(fileURLWithPath: execPath)
        process.arguments = args
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
            FileHandle.standardError.write(Data("clowder: failed to launch backend at \(execPath): \(error)\n".utf8))
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

/// Build the backend supervisor plus the control/render socket paths the app should connect to.
/// `remoteHost == nil` → supervise a local `clowder-daemon` (today's behavior); non-nil → supervise
/// the `clowder connect <host>` forwarder and connect to its local sockets. Returns nil when unbundled.
@MainActor
func makeBackendSupervisor(remoteHost: String?) -> (supervisor: DaemonSupervisor, control: String, render: String)? {
    let socks = ClowderPaths.socketPaths()
    var env = ProcessInfo.processInfo.environment

    if let host = remoteHost {
        guard let clowderPath = ClowderPaths.bundledBin("clowder") else { return nil }
        let dir = forwarderSocketDir(controlPath: socks.control)
        let control = (dir as NSString).appendingPathComponent("clowder-control.sock")
        let render = (dir as NSString).appendingPathComponent("clowder.sock")
        let binDir = (clowderPath as NSString).deletingLastPathComponent
        env["PATH"] = binDir + ":" + (env["PATH"] ?? "/usr/bin:/bin")
        // Deliberately do NOT set CLOWDER_*_SOCK: the forwarder derives its own dir from the default
        // control sock (a clean env), which must equal `dir` above — overriding it would push the
        // forwarder to `.../remote/remote`.
        let sup = DaemonSupervisor(spawn: { ProcessDaemon(execPath: clowderPath, args: ["connect", host], env: env) })
        return (sup, control, render)
    } else {
        guard let daemonPath = ClowderPaths.bundledBin("clowder-daemon") else { return nil }
        env["CLOWDER_SOCK"] = socks.client
        env["CLOWDER_CONTROL_SOCK"] = socks.control
        env["CLOWDER_HOOK_SOCK"] = socks.hook
        let binDir = (daemonPath as NSString).deletingLastPathComponent
        env["PATH"] = binDir + ":" + (env["PATH"] ?? "/usr/bin:/bin")
        let sup = DaemonSupervisor(spawn: { ProcessDaemon(execPath: daemonPath, env: env) })
        return (sup, socks.control, socks.client)
    }
}
