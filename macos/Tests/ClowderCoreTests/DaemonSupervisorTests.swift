import XCTest
@testable import ClowderCore

/// A fake daemon process the test drives: records terminate(), and fires its exit handler on demand.
@MainActor
final class FakeDaemonProcess: DaemonProcess {
    private(set) var terminated = false
    /// Simulates a live child. `exit(_:)` clears it, mirroring a real process.
    var isRunning = true
    private var onExit: ((Int32) -> Void)?
    func terminate() { terminated = true; isRunning = false }
    func setOnExit(_ handler: @escaping (Int32) -> Void) { onExit = handler }
    func exit(_ code: Int32) { isRunning = false; onExit?(code) }
}

@MainActor
final class DaemonSupervisorTests: XCTestCase {
    func testStartSpawnsAndRuns() {
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        XCTAssertEqual(spawned.count, 1)
        XCTAssertEqual(sup.state, .running)
        sup.stop()
    }

    func testCrashRelaunchesWithBackoff() async {
        let controller = SleepController()
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(
            spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p },
            sleep: { await controller.sleep($0) }
        )
        sup.start()
        XCTAssertEqual(sup.state, .running)

        spawned[0].exit(139)                          // crash (SIGSEGV-style), not exit 1
        XCTAssertEqual(sup.state, .relaunching)

        let parked = await eventually { controller.parkedCount == 1 }
        XCTAssertTrue(parked)
        controller.advance()                          // wake → relaunch
        let live = await eventually { sup.state == .running }
        XCTAssertTrue(live)
        XCTAssertEqual(spawned.count, 2)              // a fresh process was spawned
        sup.stop()
    }

    func testBackoffIsBoundedAndNonDecreasing() async {
        let controller = SleepController()
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(
            spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p },
            sleep: { await controller.sleep($0) }
        )
        sup.start()
        spawned[0].exit(2)                            // schedule backoff #1 (0.5)
        // 7 backoffs total: crash after each of the first 6 relaunches; let the 7th survive.
        for i in 0..<7 {
            let parked = await eventually { controller.parkedCount == 1 }
            XCTAssertTrue(parked)
            controller.advance()                      // consume the backoff → relaunch
            let running = await eventually { sup.state == .running }
            XCTAssertTrue(running)
            if i < 6 { spawned.last?.exit(2) }        // crash again → schedule the next backoff
        }
        XCTAssertEqual(controller.delays, [0.5, 1, 2, 4, 8, 10, 10])
        sup.stop()
    }

    func testStopTerminatesAndDoesNotRelaunch() {
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        let first = spawned[0]
        sup.stop()
        XCTAssertTrue(first.terminated)
        XCTAssertEqual(sup.state, .stopped)
        first.exit(139)                               // a late exit callback must not relaunch
        XCTAssertEqual(spawned.count, 1)
        XCTAssertEqual(sup.state, .stopped)
    }

    func testExitCode3YieldsWithoutRelaunch() async {
        let controller = SleepController()
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(
            spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p },
            sleep: { await controller.sleep($0) }
        )
        sup.start()
        spawned[0].exit(3)                            // lost M5b's single-instance flock
        XCTAssertEqual(sup.state, .yielded)
        // No backoff scheduled, no relaunch — the app connects to the existing daemon via M5d.
        for _ in 0..<20 { await Task.yield() }
        XCTAssertEqual(controller.parkedCount, 0)
        XCTAssertEqual(spawned.count, 1)
        sup.stop()
    }

    func testGenericErrorExit1Relaunches() async {
        let controller = SleepController()
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(
            spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p },
            sleep: { await controller.sleep($0) }
        )
        sup.start()
        spawned[0].exit(1)                            // generic main() Err (e.g. bind failure) → relaunch, NOT yield
        XCTAssertEqual(sup.state, .relaunching)
        let parked = await eventually { controller.parkedCount == 1 }
        XCTAssertTrue(parked)
        controller.advance()
        let live = await eventually { sup.state == .running }
        XCTAssertTrue(live)
        XCTAssertEqual(spawned.count, 2)
        sup.stop()
    }

    func testDetachDoesNotTerminateTheProcess() {
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        sup.detach()
        XCTAssertEqual(sup.state, .detached)
        // The whole point: local agents are PTY children of this process and do not survive a
        // restart, so switching away must not kill it.
        XCTAssertFalse(spawned[0].terminated, "detach must not SIGTERM the daemon")
    }

    func testResumeReadoptsAStillLiveProcessWithoutRespawning() {
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        sup.detach()
        sup.resume()
        XCTAssertEqual(sup.state, .running)
        XCTAssertEqual(spawned.count, 1, "a live daemon must be re-adopted, not respawned")
    }

    func testResumeRelaunchesWhenTheProcessDiedWhileDetached() {
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        sup.detach()
        spawned[0].isRunning = false        // died on its own while nobody was supervising
        sup.resume()
        XCTAssertEqual(sup.state, .running)
        XCTAssertEqual(spawned.count, 2, "a dead daemon must be relaunched on resume")
    }

    func testAnExitWhileDetachedDoesNotRelaunch() async {
        let controller = SleepController()
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(
            spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p },
            sleep: { await controller.sleep($0) }
        )
        sup.start()
        sup.detach()
        spawned[0].exit(139)                // crashed while detached
        XCTAssertEqual(sup.state, .detached, "a detached supervisor must not resurrect the process")
        XCTAssertEqual(controller.parkedCount, 0, "no relaunch may be scheduled while detached")
        XCTAssertEqual(spawned.count, 1)
    }

    func testExitCode4EntersFailedAndDoesNotRelaunch() async {
        let controller = SleepController()
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(
            spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p },
            sleep: { await controller.sleep($0) }
        )
        sup.start()
        spawned[0].exit(4)                  // `clowder connect`: the first dial never landed
        guard case let .failed(reason) = sup.state else {
            return XCTFail("expected .failed, got \(sup.state)")
        }
        XCTAssertFalse(reason.isEmpty, "the chip shows this reason to the user")
        XCTAssertEqual(controller.parkedCount, 0, "an unreachable host must not relaunch forever")
        XCTAssertEqual(spawned.count, 1)
    }

    func testExitCode3StillYields() {
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        spawned[0].exit(3)
        XCTAssertEqual(sup.state, .yielded, "exit 3 (lost the single-instance flock) must not change")
    }

    func testStartAfterFailedRetries() {
        // The chip offers a Retry; it must actually spawn again.
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        spawned[0].exit(4)
        sup.start()
        XCTAssertEqual(sup.state, .running)
        XCTAssertEqual(spawned.count, 2)
    }
}
