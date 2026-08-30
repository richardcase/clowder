// SPDX-License-Identifier: Apache-2.0

import Foundation

/// A running daemon process the supervisor controls. The real implementation (ClowderApp) wraps a
/// Foundation.Process; tests use a fake.
public protocol DaemonProcess: AnyObject {
    /// Ask the process to terminate (SIGTERM).
    func terminate()
    /// Register a handler invoked once, on the main actor, when the process exits (with its code).
    func setOnExit(_ handler: @escaping (Int32) -> Void)
    /// Whether the child is still alive. Read on `resume()` to decide between re-adopting a
    /// still-running daemon and relaunching a dead one.
    var isRunning: Bool { get }
}

/// Launches and supervises the clowder-daemon child process: relaunches it (bounded backoff) if it exits
/// unexpectedly, yields if it lost the single-instance lock (exit 3), and stops cleanly on quit.
/// Libghostty-free and unit-testable via injected spawn + sleep seams (mirrors AppModel's reconnect).
@MainActor
public final class DaemonSupervisor {
    public enum State: Equatable {
        case stopped
        case running
        case relaunching
        /// Lost the single-instance lock (exit 3) — another daemon owns it, so yield for good.
        case yielded
        /// Deliberately not supervising a process we left running (a backend switch). Local agents
        /// are PTY children of it, so terminating would destroy the user's work.
        case detached
        /// The backend reported a condition retrying cannot fix (exit 4: the first dial never
        /// landed). Surfaced to the user with a Retry rather than looped over.
        case failed(String)
    }
    @Published public private(set) var state: State = .stopped

    private let spawn: () -> DaemonProcess
    private let sleepFn: (TimeInterval) async -> Void
    private var process: DaemonProcess?
    private var relaunchTask: Task<Void, Never>?
    private var isStopping = false
    private var isDetached = false
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
        isDetached = false
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
        // A process we deliberately stopped supervising must not be resurrected — the user switched
        // away from this backend on purpose.
        guard !isDetached else { return }
        if code == 3 {
            // Daemon's DISTINCT single-instance-loser code (lost M5b's flock) → defer to the owner.
            // NOT code 1: `main() -> Result<()>` returning Err (e.g. a bind failure) also exits 1 and
            // must relaunch, not yield.
            state = .yielded
            return
        }
        if code == 4 {
            // `clowder connect`'s DISTINCT "the first dial never landed" code. Relaunching cannot
            // fix a wrong address or a daemon that is down, and doing so forever would leave the
            // user staring at "Reconnecting…" with no way to tell those apart.
            state = .failed("could not reach the remote daemon")
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

    /// Stop supervising without killing the child.
    ///
    /// Keeps the `process` handle rather than orphaning it, so `resume()` can re-adopt the *same*
    /// daemon. Orphaning would force a respawn on every switch-back, which both kills the agents
    /// this exists to protect and races the daemon's single-instance flock.
    public func detach() {
        guard process != nil || relaunchTask != nil else { return }
        isDetached = true
        relaunchTask?.cancel()
        relaunchTask = nil
        state = .detached
    }

    /// Resume supervision: re-adopt a still-running child, or relaunch if it died while detached.
    public func resume() {
        guard isDetached else { return }
        isDetached = false
        if let p = process, p.isRunning {
            // The onExit handler registered at launch is still installed, so supervision simply
            // takes effect again.
            state = .running
        } else {
            process = nil
            relaunchAttempt = 0
            launch()
        }
    }

    /// Explicit teardown (app quit): cancel relaunches and terminate the child.
    ///
    /// Quit means quit even for a detached backend: the retained handle from `detach()` still gets
    /// terminated here, so switching to a remote host and then quitting doesn't leave an orphaned
    /// local daemon behind.
    public func stop() {
        isStopping = true
        isDetached = false
        relaunchTask?.cancel()
        relaunchTask = nil
        process?.terminate()
        process = nil
        state = .stopped
    }
}
