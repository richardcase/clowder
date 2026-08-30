// SPDX-License-Identifier: Apache-2.0

import Foundation
import XCTest
@testable import ClowderCore

/// Records what the registry asked for and replays canned CLI output.
final class FakeCommandRunner: CommandRunner, @unchecked Sendable {
    struct Invocation: Equatable {
        let args: [String]
        let stdin: String?
    }
    private(set) var invocations: [Invocation] = []
    var results: [CommandResult] = []
    var thrownError: Error?
    /// Whether each call happened on the main thread — how a test pins that the blocking CLI call is
    /// made off the main actor.
    private(set) var ranOnMainThread: [Bool] = []
    /// Called inside `run`, on whatever thread the caller used, after the call is recorded. Lets a
    /// test park a call in flight and inspect the model while it is still running.
    var onRun: (@Sendable () -> Void)?

    func run(_ args: [String], stdin: String?) throws -> CommandResult {
        ranOnMainThread.append(Thread.isMainThread)
        invocations.append(Invocation(args: args, stdin: stdin))
        onRun?()
        if let thrownError { throw thrownError }
        guard !results.isEmpty else {
            return CommandResult(status: 0, stdout: Data("{}".utf8), stderr: Data())
        }
        return results.removeFirst()
    }
}

// Declared on `CommandResult` itself (not nested in `FakeCommandRunner`) so the `.ok(...)` /
// `.failed(...)` implicit-member shorthand below resolves — Swift only looks at static members
// of the contextually expected type.
extension CommandResult {
    static func ok(_ json: String) -> CommandResult {
        CommandResult(status: 0, stdout: Data(json.utf8), stderr: Data())
    }
    static func failed(_ json: String, status: Int32 = 1) -> CommandResult {
        CommandResult(status: status, stdout: Data(json.utf8), stderr: Data())
    }
}

final class HostRegistryTests: XCTestCase {
    private let listJSON = """
    {"hosts":[{"name":"studio","address":"s:7777","tls":true,"hasToken":true,
    "fingerprint":"a1b2","trusted":true,"source":"registry"}]}
    """

    func testListSendsJSONFlagAndDecodes() throws {
        let fake = FakeCommandRunner()
        fake.results = [.ok(listJSON)]
        let hosts = try HostRegistry(runner: fake).list()
        XCTAssertEqual(fake.invocations, [.init(args: ["remote", "list", "--json"], stdin: nil)])
        XCTAssertEqual(hosts.map(\.name), ["studio"])
    }

    func testAddPassesTheTokenOnStdinNeverInArgv() throws {
        let fake = FakeCommandRunner()
        fake.results = [.ok(#"{"name":"studio","address":"s:7777","tls":true,"hasToken":true,"fingerprint":null,"trusted":false,"source":"registry"}"#)]
        _ = try HostRegistry(runner: fake).add(name: "studio", address: "s:7777", token: "s3cr3t", tls: true)

        let inv = try XCTUnwrap(fake.invocations.first)
        XCTAssertEqual(inv.stdin, "s3cr3t")
        XCTAssertTrue(inv.args.contains("--token-stdin"))
        // argv is world-readable via `ps`. This assertion is the point of the whole design.
        XCTAssertFalse(inv.args.contains("s3cr3t"), "token must never appear in argv: \(inv.args)")
    }

    func testAddWithoutATokenOmitsTheStdinFlag() throws {
        let fake = FakeCommandRunner()
        fake.results = [.ok(#"{"name":"a","address":"h:1","tls":false,"hasToken":false,"fingerprint":null,"trusted":false,"source":"registry"}"#)]
        _ = try HostRegistry(runner: fake).add(name: "a", address: "h:1", token: nil, tls: false)
        let inv = try XCTUnwrap(fake.invocations.first)
        XCTAssertNil(inv.stdin)
        XCTAssertFalse(inv.args.contains("--token-stdin"))
        XCTAssertTrue(inv.args.contains("--no-tls"))
    }

    func testUpdateDistinguishesUnchangedClearAndSet() throws {
        let ok = CommandResult(status: 0, stdout: Data(#"{"name":"a","address":"h:1","tls":true,"hasToken":false,"fingerprint":null,"trusted":false,"source":"registry"}"#.utf8), stderr: Data())

        let unchanged = FakeCommandRunner(); unchanged.results = [ok]
        _ = try HostRegistry(runner: unchanged).update(name: "a", rename: nil, address: nil, token: .unchanged, tls: nil)
        let a = try XCTUnwrap(unchanged.invocations.first)
        XCTAssertFalse(a.args.contains("--token-stdin"))
        XCTAssertFalse(a.args.contains("--no-token"))

        let cleared = FakeCommandRunner(); cleared.results = [ok]
        _ = try HostRegistry(runner: cleared).update(name: "a", rename: nil, address: nil, token: .clear, tls: nil)
        XCTAssertTrue(try XCTUnwrap(cleared.invocations.first).args.contains("--no-token"))

        let set = FakeCommandRunner(); set.results = [ok]
        _ = try HostRegistry(runner: set).update(name: "a", rename: nil, address: nil, token: .set("t"), tls: nil)
        let c = try XCTUnwrap(set.invocations.first)
        XCTAssertTrue(c.args.contains("--token-stdin"))
        XCTAssertEqual(c.stdin, "t")
    }

    func testProbeByNameAndByAddressUseDifferentArguments() throws {
        let probeJSON = #"{"probe":{"name":"studio","address":"s:7777","reachable":true,"tls":true,"fingerprint":"a1b2","pinnedFingerprint":null,"fingerprintMatch":"new","authenticated":true,"error":null}}"#

        let byName = FakeCommandRunner(); byName.results = [.ok(probeJSON)]
        _ = try HostRegistry(runner: byName).probe(name: "studio")
        XCTAssertEqual(try XCTUnwrap(byName.invocations.first).args,
                       ["remote", "probe", "studio", "--timeout", "3", "--json"])

        let byAddr = FakeCommandRunner(); byAddr.results = [.ok(probeJSON)]
        _ = try HostRegistry(runner: byAddr).probe(address: "s:7777", token: "t", tls: true)
        let inv = try XCTUnwrap(byAddr.invocations.first)
        XCTAssertTrue(inv.args.contains("--address"))
        XCTAssertTrue(inv.args.contains("s:7777"))
        XCTAssertEqual(inv.stdin, "t", "an unsaved host's token still goes via stdin")
        XCTAssertFalse(inv.args.contains("t"))
    }

    func testProbeAlwaysSendsAnExplicitTimeout() throws {
        // The caller's timeout MUST reach argv. `probe` runs synchronously on the app's main
        // thread and the CLI bounds the connect, the handshake and the read-line each by this
        // value — so silently falling back to the CLI's 3s default is ~9s of frozen UI.
        let probeJSON = #"{"probe":{"name":"studio","address":"s:7777","reachable":true,"tls":true,"fingerprint":"a1b2","pinnedFingerprint":null,"fingerprintMatch":"new","authenticated":true,"error":null}}"#

        let byName = FakeCommandRunner(); byName.results = [.ok(probeJSON)]
        _ = try HostRegistry(runner: byName).probe(name: "studio", timeoutSeconds: 1)
        XCTAssertEqual(try XCTUnwrap(byName.invocations.first).args,
                       ["remote", "probe", "studio", "--timeout", "1", "--json"])

        let byAddr = FakeCommandRunner(); byAddr.results = [.ok(probeJSON)]
        _ = try HostRegistry(runner: byAddr).probe(address: "s:7777", token: nil, tls: false,
                                                   timeoutSeconds: 2)
        let args = try XCTUnwrap(byAddr.invocations.first).args
        let flag = try XCTUnwrap(args.firstIndex(of: "--timeout"), "argv: \(args)")
        XCTAssertEqual(args[flag + 1], "2", "argv: \(args)")
    }

    func testTrustPassesTheFingerprintVerbatim() throws {
        let fake = FakeCommandRunner()
        fake.results = [.ok(#"{"name":"studio","address":"s:7777","tls":true,"hasToken":true,"fingerprint":"a1b2","trusted":true,"source":"registry"}"#)]
        try HostRegistry(runner: fake).trust(name: "studio", fingerprint: "a1b2")
        XCTAssertEqual(try XCTUnwrap(fake.invocations.first).args,
                       ["remote", "trust", "studio", "--fingerprint", "a1b2", "--json"])
    }

    func testANonZeroExitSurfacesTheCLIsErrorMessage() {
        let fake = FakeCommandRunner()
        fake.results = [.failed(#"{"error":"unknown host \"studi\"; try `clowder remote list`"}"#)]
        do {
            _ = try HostRegistry(runner: fake).list()
            XCTFail("a non-zero exit must throw")
        } catch let HostRegistryError.cli(message) {
            // The CLI's message is the useful one — a generic "command failed" would strand the user.
            XCTAssertTrue(message.contains("studi"), message)
        } catch {
            XCTFail("expected .cli, got \(error)")
        }
    }

    func testAFailureWithUndecodableStdoutStillReportsSomething() {
        let fake = FakeCommandRunner()
        fake.results = [CommandResult(status: 1, stdout: Data(), stderr: Data("boom\n".utf8))]
        do {
            _ = try HostRegistry(runner: fake).list()
            XCTFail("must throw")
        } catch let HostRegistryError.cli(message) {
            XCTAssertTrue(message.contains("boom"), "fall back to stderr when stdout has no JSON: \(message)")
        } catch {
            XCTFail("expected .cli, got \(error)")
        }
    }

    func testASuccessfulExitWithGarbageStdoutThrowsDecode() {
        let fake = FakeCommandRunner()
        fake.results = [.ok("not json")]
        XCTAssertThrowsError(try HostRegistry(runner: fake).list()) { error in
            guard case HostRegistryError.decode = error else {
                return XCTFail("expected .decode, got \(error)")
            }
        }
    }

    func testRemoveSendsTheExpectedArguments() throws {
        let fake = FakeCommandRunner()
        fake.results = [.ok("{}")]
        try HostRegistry(runner: fake).remove(name: "studio")
        XCTAssertEqual(fake.invocations.map(\.args), [["remote", "rm", "studio", "--json"]])
    }

    func testUntrustSendsTheExpectedArguments() throws {
        let fake = FakeCommandRunner()
        fake.results = [.ok(#"{"name":"studio","address":"s:7777","tls":true,"hasToken":true,"fingerprint":null,"trusted":false,"source":"registry"}"#)]
        try HostRegistry(runner: fake).untrust(name: "studio")
        XCTAssertEqual(fake.invocations.map(\.args), [["remote", "untrust", "studio", "--json"]])
    }

    func testRemoveSurfacesTheCLIsError() {
        let fake = FakeCommandRunner()
        fake.results = [.failed(#"{"error":"\"config\" is defined by [remote] host in config.toml"}"#)]
        XCTAssertThrowsError(try HostRegistry(runner: fake).remove(name: "config")) { error in
            guard case let HostRegistryError.cli(m) = error else { return XCTFail("expected .cli") }
            XCTAssertTrue(m.contains("config.toml"), m)
        }
    }

    func testProbeAsyncReturnsTheSameResultAsTheSyncCall() async throws {
        let probeJSON = #"{"probe":{"name":"studio","address":"s:7777","reachable":true,"tls":true,"fingerprint":"a1b2","pinnedFingerprint":null,"fingerprintMatch":"new","authenticated":true,"error":null}}"#
        let fake = FakeCommandRunner()
        fake.results = [.ok(probeJSON)]
        let probe = try await HostRegistry(runner: fake).probeAsync(name: "studio", timeoutSeconds: 2)
        XCTAssertEqual(probe.fingerprint, "a1b2")
        XCTAssertEqual(fake.invocations.map(\.args),
                       [["remote", "probe", "studio", "--timeout", "2", "--json"]])
    }

    func testProbeAsyncPropagatesFailures() async {
        let fake = FakeCommandRunner()
        fake.results = [.failed(#"{"error":"unknown host \"nope\""}"#)]
        do {
            _ = try await HostRegistry(runner: fake).probeAsync(name: "nope")
            XCTFail("a failing probe must throw")
        } catch let HostRegistryError.cli(m) {
            XCTAssertTrue(m.contains("nope"), m)
        } catch {
            XCTFail("expected .cli, got \(error)")
        }
    }
}
