import XCTest
@testable import MuxyCore

/// A fake daemon process the test drives: records terminate(), and fires its exit handler on demand.
@MainActor
final class FakeDaemonProcess: DaemonProcess {
    private(set) var terminated = false
    private var onExit: ((Int32) -> Void)?
    func terminate() { terminated = true }
    func setOnExit(_ handler: @escaping (Int32) -> Void) { onExit = handler }
    /// Test helper: simulate the process exiting with `code`.
    func exit(_ code: Int32) { onExit?(code) }
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
}
