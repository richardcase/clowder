import XCTest
@testable import ClowderCore

final class RemoteHostTests: XCTestCase {
    /// Resolve `docs/protocol/fixtures` from this source file's location, so the test does not
    /// depend on the working directory `swift test` happens to run in. Same shape as ModelsTests.
    private func fixture(_ name: String, file: StaticString = #filePath) throws -> Data {
        let here = URL(fileURLWithPath: "\(file)")
        let repo = here.deletingLastPathComponent()   // ClowderCoreTests
            .deletingLastPathComponent()              // Tests
            .deletingLastPathComponent()              // macos
            .deletingLastPathComponent()              // repo root
        return try Data(contentsOf: repo.appendingPathComponent("docs/protocol/fixtures/\(name)"))
    }

    func testDecodesTheHostListFixture() throws {
        let out = try JSONDecoder().decode(ListOutput.self, from: fixture("remote-host-list.json"))
        XCTAssertEqual(out.hosts.count, 2)

        let studio = out.hosts[0]
        XCTAssertEqual(studio.name, "studio")
        XCTAssertEqual(studio.address, "studio.tailnet:7777")
        XCTAssertTrue(studio.tls)
        XCTAssertTrue(studio.hasToken)
        XCTAssertEqual(studio.fingerprint, "a1b2")
        XCTAssertEqual(studio.source, .registry)
        XCTAssertTrue(studio.isTrusted)
        XCTAssertTrue(studio.isEditable)

        let config = out.hosts[1]
        XCTAssertEqual(config.name, "config")
        XCTAssertFalse(config.hasToken)
        XCTAssertNil(config.fingerprint)
        XCTAssertEqual(config.source, .config)
        XCTAssertFalse(config.isTrusted)
        XCTAssertFalse(config.isEditable, "a config-sourced entry lives in config.toml and is read-only")
    }

    func testDecodesTheProbeFixture() throws {
        let out = try JSONDecoder().decode(ProbeOutput.self, from: fixture("remote-probe.json"))
        XCTAssertEqual(out.probe.name, "studio")
        XCTAssertTrue(out.probe.reachable)
        XCTAssertTrue(out.probe.tls)
        XCTAssertEqual(out.probe.fingerprint, "a1b2")
        XCTAssertNil(out.probe.pinnedFingerprint)
        XCTAssertEqual(out.probe.fingerprintMatch, .new)
        XCTAssertTrue(out.probe.authenticated)
        XCTAssertNil(out.probe.error)
    }

    func testProbeAuthenticationIsNotClaimedForAPlaintextDaemon() {
        // A plaintext daemon passes expected_token: None and accepts anything, so
        // `authenticated == true` there does NOT mean authenticated. Anything rendering this
        // must consult `tls` too, so the model exposes the distinction rather than a bare Bool.
        let plaintext = HostProbe(name: "p", address: "h:1", reachable: true, tls: false,
                                  fingerprint: nil, pinnedFingerprint: nil,
                                  fingerprintMatch: nil, authenticated: true, error: nil)
        XCTAssertEqual(plaintext.authSummary, .nonePlaintext)

        let accepted = HostProbe(name: "p", address: "h:1", reachable: true, tls: true,
                                 fingerprint: "aa", pinnedFingerprint: nil,
                                 fingerprintMatch: .new, authenticated: true, error: nil)
        XCTAssertEqual(accepted.authSummary, .tokenAccepted)

        let rejected = HostProbe(name: "p", address: "h:1", reachable: true, tls: true,
                                 fingerprint: "aa", pinnedFingerprint: nil,
                                 fingerprintMatch: .new, authenticated: false, error: nil)
        XCTAssertEqual(rejected.authSummary, .tokenRejected)
    }

    func testBackendIDIsHashableAndDistinguishesHosts() {
        let a: BackendID = .remote(HostID("studio"))
        let b: BackendID = .remote(HostID("laptop"))
        XCTAssertNotEqual(a, b)
        XCTAssertNotEqual(a, .local)
        XCTAssertEqual(Set([a, b, .local, a]).count, 3)
    }

    func testBackendIDDescriptionIsStableForMenusAndLogs() {
        XCTAssertEqual(BackendID.local.description, "Local")
        XCTAssertEqual(BackendID.remote(HostID("studio")).description, "studio")
    }
}
