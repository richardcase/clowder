import Foundation

/// A running daemon process the supervisor controls. The real implementation (MuxyApp) wraps a
/// Foundation.Process; tests use a fake.
public protocol DaemonProcess: AnyObject {
    /// Ask the process to terminate (SIGTERM).
    func terminate()
    /// Register a handler invoked once, on the main actor, when the process exits (with its code).
    func setOnExit(_ handler: @escaping (Int32) -> Void)
}

/// Launches and supervises the muxy-daemon child process: relaunches it (bounded backoff) if it exits
/// unexpectedly, yields if it lost the single-instance lock (exit 1), and stops cleanly on quit.
/// Libghostty-free and unit-testable via injected spawn + sleep seams (mirrors AppModel's reconnect).
@MainActor
public final class DaemonSupervisor {
    public enum State: Equatable { case stopped, running, relaunching, yielded }
    @Published public private(set) var state: State = .stopped

    private let spawn: () -> DaemonProcess
    private let sleepFn: (TimeInterval) async -> Void
    private var process: DaemonProcess?
    private var relaunchTask: Task<Void, Never>?
    private var isStopping = false
    private var relaunchAttempt = 0     // persists across crashes so backoff escalates

    public init(spawn: @escaping () -> DaemonProcess,
                sleep: @escaping (TimeInterval) async -> Void = { d in
                    try? await Task.sleep(nanoseconds: UInt64(max(0, d) * 1_000_000_000))
                }) {
        self.spawn = spawn
        self.sleepFn = sleep
    }

    /// Spawn the daemon and supervise it. Idempotent while already running/relaunching.
    public func start() {
        guard process == nil, relaunchTask == nil else { return }
        isStopping = false
        relaunchAttempt = 0
        launch()
    }

    private func launch() {
        let p = spawn()
        process = p
        state = .running
        p.setOnExit { [weak self] code in self?.handleExit(code) }
    }

    private func handleExit(_ code: Int32) {
        process = nil
        guard !isStopping else { return }
        if code == 1 {
            // Lost M5b's single-instance flock: another daemon owns it. Don't relaunch — the app
            // connects to the existing daemon via M5d.
            state = .yielded
            return
        }
        scheduleRelaunch()
    }

    private func backoffDelay(_ attempt: Int) -> TimeInterval { min(10.0, 0.5 * pow(2.0, Double(attempt))) }

    /// Schedule one delayed relaunch. `relaunchAttempt` is an INSTANCE counter (not loop-local): each
    /// crash arrives as its own async `onExit` callback, so the counter must persist across callbacks
    /// for the backoff to escalate. Reset only in `start()`.
    private func scheduleRelaunch() {
        guard relaunchTask == nil, !isStopping else { return }
        state = .relaunching
        let delay = backoffDelay(relaunchAttempt)
        relaunchAttempt += 1
        relaunchTask = Task { [weak self] in
            guard let self else { return }
            await self.sleepFn(delay)
            self.relaunchTask = nil
            guard !Task.isCancelled, !self.isStopping else { return }
            self.launch()            // spawn again; sets .running
        }
    }

    /// Explicit teardown (app quit): cancel relaunches and terminate the child.
    public func stop() {
        isStopping = true
        relaunchTask?.cancel()
        relaunchTask = nil
        process?.terminate()
        process = nil
        state = .stopped
    }
}
