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

    /// `$XDG_STATE_HOME/clowder` › `~/.local/state/clowder` — the dir the daemon already keeps
    /// agents.json/projects.json in (mirrors Registry::default_path in the Rust side).
    static func stateDir() -> String {
        let env = ProcessInfo.processInfo.environment
        if let xdg = env["XDG_STATE_HOME"], !xdg.isEmpty {
            return (xdg as NSString).appendingPathComponent("clowder")
        }
        let home = env["HOME"] ?? NSHomeDirectory()
        return ((home as NSString).appendingPathComponent(".local/state") as NSString)
            .appendingPathComponent("clowder")
    }

    static var daemonLogPath: String { (stateDir() as NSString).appendingPathComponent("daemon.log") }
}

/// A file handle for the backend's stdout/stderr.
///
/// The daemon logs to stderr, but a child of a Finder/Dock/launchd-launched `.app` inherits the
/// app's fds — /dev/null. So every `tracing::error!`, the exit reason and any bind failure were
/// discarded, which is why a first-launch failure surfaced as an opaque banner with nothing to
/// inspect. Send it somewhere a user (or a bug report) can actually read.
///
/// Appends across relaunches so a crash loop is visible, and truncates once past `maxBytes` so an
/// unattended app cannot fill the disk. Returns nil if the log can't be opened — logging must never
/// be the reason the daemon fails to start.
enum DaemonLog {
    static let maxBytes: UInt64 = 4 * 1024 * 1024

    static func handle() -> FileHandle? {
        let path = ClowderPaths.daemonLogPath
        let fm = FileManager.default
        try? fm.createDirectory(atPath: ClowderPaths.stateDir(), withIntermediateDirectories: true)
        if let attrs = try? fm.attributesOfItem(atPath: path),
           let size = attrs[.size] as? UInt64, size > maxBytes {
            try? fm.removeItem(atPath: path)
        }
        if !fm.fileExists(atPath: path) {
            fm.createFile(atPath: path, contents: nil)
        }
        guard let h = FileHandle(forWritingAtPath: path) else { return nil }
        h.seekToEndOfFile()
        return h
    }

    /// Write an app-side line into the same log, so "the app could not launch the daemon" and "the
    /// daemon died on startup" are read in one place, in order.
    static func note(_ message: String) {
        let line = "[clowder-app] \(ISO8601DateFormatter().string(from: Date())) \(message)\n"
        guard let h = handle() else {
            FileHandle.standardError.write(Data(line.utf8))
            return
        }
        h.write(Data(line.utf8))
        try? h.close()
    }
}

/// A real clowder-daemon child process (Foundation.Process). Fires onExit on the main actor.
final class ProcessDaemon: DaemonProcess {
    private let process = Process()
    private var onExit: ((Int32) -> Void)?
    private var launchFailed = false

    private var logHandle: FileHandle?

    init(execPath: String, args: [String] = [], env: [String: String]) {
        process.executableURL = URL(fileURLWithPath: execPath)
        process.arguments = args
        process.environment = env
        // Without this the child inherits the GUI app's fds (/dev/null under Finder), and every
        // daemon diagnostic is lost. See DaemonLog.
        if let log = DaemonLog.handle() {
            logHandle = log
            process.standardOutput = log
            process.standardError = log
        }
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
            DaemonLog.note("failed to launch backend at \(execPath): \(error)")
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
        let dir = forwarderSocketDir(controlPath: socks.control, host: host)
        let control = (dir as NSString).appendingPathComponent("clowder-control.sock")
        let render = (dir as NSString).appendingPathComponent("clowder.sock")
        let binDir = (clowderPath as NSString).deletingLastPathComponent
        env["PATH"] = binDir + ":" + (env["PATH"] ?? "/usr/bin:/bin")
        // Deliberately do NOT set CLOWDER_*_SOCK: overriding them would fight --socket-dir below.
        // The app passes --socket-dir explicitly, so there is one authority for this path instead
        // of two derivations (app + forwarder) that would have to agree independently.
        // Task 9: this whole function is rewritten around backendPlan(target:sockets:).
        let sup = DaemonSupervisor(spawn: {
            ProcessDaemon(execPath: clowderPath, args: ["connect", host, "--socket-dir", dir], env: env)
        })
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
