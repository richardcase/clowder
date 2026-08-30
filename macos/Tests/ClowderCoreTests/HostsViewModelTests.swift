// SPDX-License-Identifier: Apache-2.0

import Foundation
import XCTest
@testable import ClowderCore

@MainActor
final class HostsViewModelTests: XCTestCase {
    private let twoHostsJSON = """
    {"hosts":[
      {"name":"studio","address":"s:7777","tls":true,"hasToken":true,"fingerprint":"a1b2","trusted":true,"source":"registry"},
      {"name":"config","address":"c:7777","tls":false,"hasToken":false,"fingerprint":null,"trusted":false,"source":"config"}
    ]}
    """
    private let oneHostJSON = #"{"name":"studio","address":"s:7777","tls":true,"hasToken":true,"fingerprint":null,"trusted":false,"source":"registry"}"#

    private func model(_ fake: FakeCommandRunner,
                      activeBackend: BackendID = .local,
                      onChanged: (() -> Void)? = nil) -> HostsViewModel {
        HostsViewModel(registry: HostRegistry(runner: fake),
                       activeBackend: { activeBackend },
                       onHostsChanged: { onChanged?() })
    }

    func testReloadPopulatesHosts() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        XCTAssertEqual(m.hosts.map(\.name), ["studio", "config"])
        XCTAssertNil(m.lastError)
    }

    func testReloadSurfacesAnErrorAndLeavesHostsAlone() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        fake.results = [.failed(#"{"error":"registry unreadable"}"#)]
        await m.reload()
        XCTAssertEqual(m.hosts.map(\.name), ["studio", "config"], "a failed reload must not blank the list")
        XCTAssertTrue(m.lastError?.contains("unreadable") == true, m.lastError ?? "nil")
    }

    func testSelectingAHostFillsTheDraftWithoutTheToken() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        let draft = try! XCTUnwrap(m.draft)
        XCTAssertEqual(draft.name, "studio")
        XCTAssertEqual(draft.address, "s:7777")
        XCTAssertTrue(draft.tls)
        XCTAssertFalse(draft.isNew)
        // The app never reads a stored token back — only ever writes one.
        XCTAssertNil(draft.token)
    }

    func testBeginAddStartsAnEmptyNewDraft() {
        let m = model(FakeCommandRunner())
        m.beginAdd()
        let draft = try! XCTUnwrap(m.draft)
        XCTAssertTrue(draft.isNew)
        XCTAssertEqual(draft.name, "")
        XCTAssertNil(m.selected)
    }

    func testSavingANewHostCallsAddThenReloadsAndNotifies() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(oneHostJSON), .ok(twoHostsJSON)]
        var notified = 0
        let m = model(fake, onChanged: { notified += 1 })
        m.beginAdd()
        m.draft?.name = "studio"
        m.draft?.address = "s:7777"
        m.draft?.tls = true
        m.draft?.token = "s3cr3t"
        await m.save()

        let add = fake.invocations[0]
        XCTAssertEqual(add.args.prefix(4).map { $0 }, ["remote", "add", "studio", "s:7777"])
        XCTAssertEqual(add.stdin, "s3cr3t", "the token goes on stdin")
        XCTAssertFalse(add.args.contains("s3cr3t"), "never in argv: \(add.args)")
        XCTAssertEqual(fake.invocations[1].args, ["remote", "list", "--json"], "save must reload")
        XCTAssertEqual(notified, 1, "the chip/tray/palette must be told")
        XCTAssertNil(m.lastError)
    }

    func testSavingAnExistingHostCallsSetAndLeavesAnUntypedTokenUnchanged() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(oneHostJSON), .ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        m.draft?.address = "moved:9999"
        await m.save()

        let set = fake.invocations[1]
        XCTAssertEqual(set.args.prefix(3).map { $0 }, ["remote", "set", "studio"])
        XCTAssertTrue(set.args.contains("--address"))
        XCTAssertTrue(set.args.contains("moved:9999"))
        XCTAssertFalse(set.args.contains("--token-stdin"), "an untouched token must not be rewritten")
        XCTAssertFalse(set.args.contains("--no-token"), "an untouched token must not be cleared")
        XCTAssertNil(set.stdin)
    }

    func testSavingAnInvalidDraftDoesNothing() async {
        let fake = FakeCommandRunner()
        let m = model(fake)
        m.beginAdd()
        m.draft?.name = "bad name"
        m.draft?.address = "s:7777"
        await m.save()
        XCTAssertTrue(fake.invocations.isEmpty, "an invalid draft must not reach the CLI")
        XCTAssertNotNil(m.lastError)
    }

    func testAFailedSaveForANewHostLeavesTheDraftIntact() async {
        let fake = FakeCommandRunner()
        fake.results = [.failed(#"{"error":"address already in use"}"#)]
        let m = model(fake)
        m.beginAdd()
        m.draft?.name = "studio"
        m.draft?.address = "s:7777"
        m.draft?.tls = true
        m.draft?.token = "s3cr3t"
        await m.save()

        XCTAssertEqual(m.draft?.name, "studio")
        XCTAssertEqual(m.draft?.address, "s:7777")
        XCTAssertEqual(m.draft?.tls, true)
        XCTAssertEqual(m.draft?.token, "s3cr3t",
                       "a failed save must not wipe what the user typed, including the token")
        XCTAssertNotNil(m.lastError)
    }

    func testAFailedRenameLeavesSelectedPointingAtTheOriginalName() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .failed(#"{"error":"address already in use"}"#)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        m.draft?.name = "studio-2"
        await m.save()

        XCTAssertEqual(m.selected, HostID("studio"),
                       "a failed rename must not point selection at a name that was never persisted")
        XCTAssertNotNil(m.lastError)
    }

    func testRenamingTheActiveHostIsRefused() async {
        // BackendID *is* the host name: a rename would leave `activeBackend` pointing at a name that
        // is no longer in `hosts`, which costs the chip its Retry while still connected.
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake, activeBackend: .remote(HostID("studio")))
        await m.reload()
        m.select(HostID("studio"))
        m.draft?.name = "studio-2"
        await m.save()

        XCTAssertEqual(fake.invocations.count, 1, "only the reload — no set should have run")
        XCTAssertTrue(m.lastError?.lowercased().contains("connected") == true, m.lastError ?? "nil")
        XCTAssertEqual(m.draft?.name, "studio-2", "the draft must stay exactly as typed")
        XCTAssertEqual(m.selected, HostID("studio"))
    }

    func testEditingTheActiveHostWithoutRenamingIsAllowed() async {
        // Only the *rename* strands the app; re-pointing the connected host at a new address is a
        // normal thing to do from Settings.
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(oneHostJSON), .ok(twoHostsJSON)]
        let m = model(fake, activeBackend: .remote(HostID("studio")))
        await m.reload()
        m.select(HostID("studio"))
        m.draft?.address = "moved:9999"
        await m.save()

        guard fake.invocations.count == 3 else {
            return XCTFail("expected list, set, list — got \(fake.invocations.map(\.args))")
        }
        XCTAssertEqual(fake.invocations[1].args.prefix(3).map { $0 }, ["remote", "set", "studio"])
        XCTAssertNil(m.lastError)
    }

    func testRemovingTheActiveHostIsRefused() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake, activeBackend: .remote(HostID("studio")))
        await m.reload()
        await m.remove(HostID("studio"))
        XCTAssertEqual(fake.invocations.count, 1, "only the reload — no rm should have run")
        XCTAssertTrue(m.lastError?.lowercased().contains("connected") == true, m.lastError ?? "nil")
    }

    func testRemovingAnInactiveHostCallsRmAndReloads() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok("{}"), .ok(twoHostsJSON)]
        var notified = 0
        let m = model(fake, onChanged: { notified += 1 })
        await m.reload()
        await m.remove(HostID("studio"))
        XCTAssertEqual(fake.invocations[1].args, ["remote", "rm", "studio", "--json"])
        XCTAssertEqual(fake.invocations[2].args, ["remote", "list", "--json"])
        XCTAssertEqual(notified, 1)
    }

    func testAFailedRemoveLeavesSelectedUnchanged() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .failed(#"{"error":"boom"}"#)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.remove(HostID("studio"))

        XCTAssertEqual(m.selected, HostID("studio"),
                       "a failed remove must not deselect a host that is still there")
        XCTAssertNotNil(m.lastError)
    }

    func testAnUntouchedSelectionIsNotDirty() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        XCTAssertFalse(m.isDirty, "selecting a host and touching nothing must not read as a change")
    }

    func testChangingTheNameIsDirty() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        m.draft?.name = "studio-2"
        XCTAssertTrue(m.isDirty)
    }

    func testChangingTheAddressIsDirty() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        m.draft?.address = "moved:9999"
        XCTAssertTrue(m.isDirty)
    }

    func testTogglingTLSIsDirty() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        m.draft?.tls.toggle()
        XCTAssertTrue(m.isDirty)
    }

    func testTypingATokenIsDirtyButClearingItBackIsNot() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        XCTAssertFalse(m.isDirty)
        m.draft?.token = "s3cr3t"
        XCTAssertTrue(m.isDirty, "typing a token is a real change to send")
        m.draft?.token = ""
        XCTAssertFalse(m.isDirty,
                       "an empty token means \"leave the stored one alone\", not \"clear it\"")
    }

    func testANewDraftIsNotDirtyUntouchedButIsOnceTyped() {
        let m = model(FakeCommandRunner())
        m.beginAdd()
        XCTAssertFalse(m.isDirty, "an untouched new draft has nothing to revert")
        m.draft?.name = "studio"
        XCTAssertTrue(m.isDirty, "a typed-into new draft is revertable (discardable) via Revert")
    }

    func testAConfigSourcedHostIsNotEditable() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("config"))
        XCTAssertFalse(m.canEditSelection, "[remote] host lives in config.toml and is read-only")
    }

    private let probeNewJSON = #"{"probe":{"name":"studio","address":"s:7777","reachable":true,"tls":true,"fingerprint":"a1b2","pinnedFingerprint":null,"fingerprintMatch":"new","authenticated":true,"error":null}}"#
    private let probeUnreachableJSON = #"{"probe":{"name":"studio","address":"s:7777","reachable":false,"tls":true,"fingerprint":null,"pinnedFingerprint":null,"fingerprintMatch":null,"authenticated":false,"error":"connection refused"}}"#
    private let probePlaintextJSON = #"{"probe":{"name":"studio","address":"s:7777","reachable":true,"tls":false,"fingerprint":null,"pinnedFingerprint":null,"fingerprintMatch":null,"authenticated":true,"error":null}}"#

    func testBeginPairingProbesAndOffersTheFingerprint() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probeNewJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        guard case let .observed(probe) = m.pairing else {
            return XCTFail("expected .observed, got \(m.pairing)")
        }
        XCTAssertEqual(probe.fingerprint, "a1b2")
        XCTAssertEqual(probe.authSummary, .tokenAccepted)
        XCTAssertTrue(m.canTrust, "a new fingerprint with no expectation typed is trustable")
    }

    func testAnUnreachableHostCannotBeTrusted() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probeUnreachableJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        XCTAssertFalse(m.canTrust)
    }

    func testAPlaintextDaemonPresentsNoFingerprintAndCannotBeTrusted() async {
        // No TLS means no certificate to pin, and "authenticated" is meaningless there.
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probePlaintextJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        guard case let .observed(probe) = m.pairing else { return XCTFail("expected .observed") }
        XCTAssertEqual(probe.authSummary, .nonePlaintext)
        XCTAssertFalse(m.canTrust, "there is no certificate to trust")
    }

    func testAMismatchedExpectedFingerprintBlocksTrust() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probeNewJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        m.expectedFingerprint = "deadbeef"
        XCTAssertFalse(m.canTrust, "a typed expectation that disagrees must block trust")
        XCTAssertNotNil(m.fingerprintComparison)
        m.expectedFingerprint = "A1B2"       // case- and whitespace-insensitive
        XCTAssertTrue(m.canTrust)
    }

    func testConfirmTrustSendsTheObservedFingerprintVerbatim() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probeNewJSON), .ok("{}"), .ok(twoHostsJSON)]
        var notified = 0
        let m = model(fake, onChanged: { notified += 1 })
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        await m.confirmTrust()
        XCTAssertEqual(fake.invocations[2].args,
                       ["remote", "trust", "studio", "--fingerprint", "a1b2", "--json"])
        XCTAssertEqual(notified, 1)
        XCTAssertEqual(m.pairing, .idle, "a successful trust closes the sheet")
    }

    func testCancelPairingClearsEverything() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probeNewJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        m.expectedFingerprint = "aa"
        m.cancelPairing()
        XCTAssertEqual(m.pairing, .idle)
        XCTAssertEqual(m.expectedFingerprint, "")
    }

    func testCancelPairingClearsAFailedTrustError() async {
        // The sheet renders `lastError` itself, so a failure from one attempt must not still be on
        // screen when the next pairing attempt opens.
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probeNewJSON), .failed(#"{"error":"trust failed"}"#)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        await m.confirmTrust()
        XCTAssertNotNil(m.lastError, "a failed trust must say so")
        XCTAssertNotEqual(m.pairing, .idle, "a failed trust must not close the sheet")

        m.cancelPairing()
        XCTAssertNil(m.lastError)
    }

    func testIsBusyIsObservableAndTheCLICallRunsOffTheMainThread() async {
        // `isBusy` gates three controls in the UI. It can only ever be seen true if `run` actually
        // suspends — a blocking call made inline on the main actor never yields SwiftUI a run-loop
        // turn between setting it and clearing it.
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let started = expectation(description: "the CLI call started")
        let release = DispatchSemaphore(value: 0)
        // Park the call in flight. The wait is bounded so that a regression which runs the body on
        // the main actor fails this test instead of deadlocking the suite.
        fake.onRun = { started.fulfill(); _ = release.wait(timeout: .now() + 2) }
        let m = model(fake)

        let reload = Task { await m.reload() }
        await fulfillment(of: [started], timeout: 5)
        XCTAssertTrue(m.isBusy, "the main actor must be free, with isBusy true, while the CLI runs")
        release.signal()
        await reload.value

        XCTAssertFalse(m.isBusy)
        XCTAssertEqual(fake.ranOnMainThread, [false],
                       "the blocking CLI call must not run on the main thread")
        XCTAssertEqual(m.hosts.map(\.name), ["studio", "config"], "and the result still lands")
    }

    func testAFailedProbeBecomesAPairingFailureNotASilentIdle() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .failed(#"{"error":"unknown host"}"#)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        guard case let .failed(message) = m.pairing else {
            return XCTFail("expected .failed, got \(m.pairing)")
        }
        XCTAssertTrue(message.contains("unknown host"), message)
        XCTAssertFalse(m.canTrust)
    }
}
