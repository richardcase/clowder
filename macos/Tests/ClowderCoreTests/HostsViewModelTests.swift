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

    func testAConfigSourcedHostIsNotEditable() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("config"))
        XCTAssertFalse(m.canEditSelection, "[remote] host lives in config.toml and is read-only")
    }
}
