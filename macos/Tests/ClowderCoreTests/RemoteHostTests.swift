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
        XCTAssertEqual(config.address, "10.0.0.5:7777")
        XCTAssertFalse(config.tls)
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

    func testTrustedAndFingerprintInvariant() throws {
        let out = try JSONDecoder().decode(ListOutput.self, from: fixture("remote-host-list.json"))
        for host in out.hosts {
            XCTAssertEqual(host.trusted, host.fingerprint != nil,
                           "Host '\(host.name)': trusted field must equal (fingerprint != nil)")
        }
    }

    func testIsTrustedReturnsTheWireValue() {
        // isTrusted must return the decoded `trusted` value, not recompute from fingerprint.
        // This ensures drift in the Rust implementation surfaces immediately.
        let withFingerprint = RemoteHost(name: "p", address: "h:1", tls: true, hasToken: true,
                                         fingerprint: "abc", trusted: true, source: .registry)
        XCTAssertEqual(withFingerprint.isTrusted, true)

        let withoutFingerprint = RemoteHost(name: "p", address: "h:1", tls: true, hasToken: true,
                                            fingerprint: nil, trusted: false, source: .registry)
        XCTAssertEqual(withoutFingerprint.isTrusted, false)

        // This would only catch drift if isTrusted computes from fingerprint instead of the wire value
        let mismatched = RemoteHost(name: "p", address: "h:1", tls: true, hasToken: true,
                                    fingerprint: nil, trusted: true, source: .registry)
        XCTAssertEqual(mismatched.isTrusted, true, "isTrusted must equal the decoded trusted field")
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

    private func probe(reachable: Bool = true, tls: Bool = true,
                       match: FingerprintMatch? = .match, authenticated: Bool = true,
                       error: String? = nil) -> HostProbe {
        HostProbe(name: "studio", address: "studio.tail:7777", reachable: reachable, tls: tls,
                  fingerprint: "aa", pinnedFingerprint: "aa", fingerprintMatch: match,
                  authenticated: authenticated, error: error)
    }

    func testAHealthyProbeIsNotRefused() {
        XCTAssertNil(backendSwitchRefusal(probe()))
        // A plaintext daemon "authenticates" everyone; that is not a reason to refuse a switch.
        XCTAssertNil(backendSwitchRefusal(probe(tls: false, match: nil)))
    }

    func testAnUnreachableHostIsRefusedWithTheProbesOwnError() throws {
        let refusal = try XCTUnwrap(backendSwitchRefusal(
            probe(reachable: false, error: "connection refused")))
        XCTAssertTrue(refusal.contains("studio.tail:7777"), refusal)
        XCTAssertTrue(refusal.contains("connection refused"), refusal)
    }

    func testAChangedFingerprintIsRefusedAndNamesTheRepinCommand() throws {
        // Reachable, so the old `reachable == false` gate let this through: `clowder connect`'s
        // pre-dial lands, the daemon then rejects every forwarded stream, and the app reconnects
        // forever with "REMOTE DAEMON IDENTIFICATION HAS CHANGED" only in daemon.log.
        let refusal = try XCTUnwrap(backendSwitchRefusal(probe(match: .changed)))
        XCTAssertTrue(refusal.contains("certificate"), refusal)
        XCTAssertTrue(refusal.contains("clowder remote trust"), refusal)
    }

    func testARejectedTokenIsRefusedAndSaysSo() throws {
        let refusal = try XCTUnwrap(backendSwitchRefusal(probe(authenticated: false)))
        XCTAssertTrue(refusal.contains("token"), refusal)
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

    func testGroupedFingerprintSplitsASHA256HexStringInto16GroupsOfFour() {
        // A real SHA-256 hex digest: 64 characters.
        let digest = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"
        // (65 chars above would be wrong — trim to exactly 64.)
        let sha256 = String(digest.prefix(64))
        XCTAssertEqual(sha256.count, 64)

        let grouped = groupedFingerprint(sha256)
        let groups = grouped.split(separator: " ")
        XCTAssertEqual(groups.count, 16, "a 64-character hex string must split into 16 groups of 4")
        for group in groups {
            XCTAssertEqual(group.count, 4)
        }
        XCTAssertEqual(groups.joined(), sha256, "grouping must not drop or reorder characters")
    }

    func testGroupedFingerprintDoesNotCrashOnAShortString() {
        XCTAssertEqual(groupedFingerprint("a1b"), "a1b")
        XCTAssertEqual(groupedFingerprint("a1b2c"), "a1b2 c")
    }

    func testGroupedFingerprintOfAnEmptyStringIsEmpty() {
        XCTAssertEqual(groupedFingerprint(""), "")
    }
}
