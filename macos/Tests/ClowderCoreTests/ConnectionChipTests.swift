import XCTest
@testable import ClowderCore

final class ConnectionChipTests: XCTestCase {
    private let studio = RemoteHost(name: "studio", address: "studio.tail:7777", tls: true,
                                    hasToken: true, fingerprint: "a1b2", trusted: true,
                                    source: .registry)

    private func chip(_ backend: BackendID,
                      _ connection: AppModel.ConnectionState,
                      supervisor: DaemonSupervisor.State = .running,
                      hosts: [RemoteHost]? = nil) -> ConnectionChip {
        connectionChip(backend: backend, hosts: hosts ?? [studio],
                       connection: connection, supervisor: supervisor)
    }

    func testLocalAndLiveReadsLocal() {
        let c = chip(.local, .live)
        XCTAssertEqual(c.title, "Local")
        XCTAssertEqual(c.tone, .ok)
        XCTAssertNil(c.detail)
    }

    func testRemoteAndLiveNamesTheHostAndShowsItsAddress() {
        let c = chip(.remote(HostID("studio")), .live)
        XCTAssertEqual(c.title, "studio")
        XCTAssertEqual(c.detail, "studio.tail:7777")
        XCTAssertEqual(c.tone, .ok)
    }

    func testConnectingIsPendingAndNotAnError() {
        // The startup grace period deliberately shows no banner; the chip must match that and
        // not flash red on an ordinary cold start.
        XCTAssertEqual(chip(.local, .connecting).tone, .pending)
    }

    func testReconnectingIsAWarningNotAnError() {
        XCTAssertEqual(chip(.local, .reconnecting).tone, .warning)
    }

    func testClosedIsAnErrorAndCarriesTheReason() {
        let c = chip(.local, .closed(reason: "socket gone"))
        XCTAssertEqual(c.tone, .error)
        XCTAssertEqual(c.detail, "socket gone")
    }

    func testAFailedSupervisorWinsOverTheConnectionState() {
        // An unreachable host leaves the control channel merely "connecting" forever. The
        // supervisor knows the real story (exit 4), so it must take precedence — otherwise the
        // user sees a hopeful spinner for a host that will never answer.
        let c = chip(.remote(HostID("studio")), .connecting,
                     supervisor: .failed("could not reach the remote daemon"))
        XCTAssertEqual(c.tone, .error)
        XCTAssertTrue(c.detail?.contains("could not reach") ?? false, "\(String(describing: c.detail))")
        XCTAssertTrue(c.canRetry, "a failed backend must offer a Retry")
    }

    func testAYieldedLocalSupervisorIsHealthyNotAnError() {
        // Exit 3 means another daemon owns the lock — an externally started daemon is a perfectly
        // good backend, and the app connects to its sockets. Saying "error" would be wrong.
        let c = chip(.local, .live, supervisor: .yielded)
        XCTAssertEqual(c.tone, .ok)
        XCTAssertEqual(c.detail, "external daemon")
    }

    func testAnUnknownHostIDDegradesGracefully() {
        // The host was removed from the registry while connected to it.
        let c = chip(.remote(HostID("ghost")), .live, hosts: [])
        XCTAssertEqual(c.title, "ghost")
        XCTAssertEqual(c.detail, "not in your host list")
        XCTAssertEqual(c.tone, .warning)
    }

    func testRetryIsOfferedOnlyWhereItHelps() {
        XCTAssertFalse(chip(.local, .live).canRetry)
        XCTAssertFalse(chip(.local, .reconnecting).canRetry, "the reconnect loop is already retrying")
        XCTAssertTrue(chip(.local, .closed(reason: "x")).canRetry)
    }
}
