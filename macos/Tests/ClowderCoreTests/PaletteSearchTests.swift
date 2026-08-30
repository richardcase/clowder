// SPDX-License-Identifier: Apache-2.0

import XCTest
@testable import ClowderCore

final class PaletteSearchTests: XCTestCase {
    private let cmds = CommandRegistry.all(keymap: Keymap())
    private func worktrees() -> [WorktreeInfo] {
        [WorktreeInfo(pane: 1, project: "/home/api", name: "fix login", branch: "clowder/fix login", state: .working),
         WorktreeInfo(pane: 2, project: "/home/web", name: "spawn worker", branch: "clowder/spawn worker", state: .idle)]
    }
    private func isCommand(_ i: PaletteItem) -> Bool { if case .command = i.kind { return true }; return false }
    private func isAgent(_ i: PaletteItem, _ pane: UInt64) -> Bool {
        if case let .agent(p) = i.kind { return p == pane }; return false
    }

    func testEmptyQueryReturnsAllCommandsThenAgents() {
        let r = paletteResults(query: "", commands: cmds, worktrees: worktrees())
        XCTAssertEqual(r.count, cmds.count + 2)
        XCTAssertTrue(r.prefix(cmds.count).allSatisfy(isCommand))
        XCTAssertTrue(r.suffix(2).allSatisfy { !isCommand($0) })
    }

    func testCommandQueryRanksCommandFirst() {
        let r = paletteResults(query: "new work", commands: cmds, worktrees: worktrees())
        XCTAssertEqual(r.first?.title, "New Worktree")
        XCTAssertTrue(isCommand(r[0]))
    }

    func testAgentQueryMatchesOnlyAgent() {
        let r = paletteResults(query: "login", commands: cmds, worktrees: worktrees())
        XCTAssertTrue(r.contains { isAgent($0, 1) })
        XCTAssertFalse(r.contains { isCommand($0) })
    }

    func testNoMatchIsEmpty() {
        XCTAssertTrue(paletteResults(query: "zzzzz", commands: cmds, worktrees: worktrees()).isEmpty)
    }

    private func twoHosts() -> [RemoteHost] {
        [RemoteHost(name: "studio", address: "studio.tail:7777", tls: true, hasToken: true,
                    fingerprint: "a1b2", trusted: true, source: .registry),
         RemoteHost(name: "laptop", address: "laptop.tail:7777", tls: false, hasToken: false,
                    fingerprint: nil, trusted: false, source: .registry)]
    }

    func testBackendEntriesAppearAndExcludeTheActiveOne() {
        let items = paletteResults(query: "", commands: [], worktrees: [],
                                   hosts: twoHosts(), activeBackend: .local)
        let backends = items.compactMap { kind -> BackendID? in
            if case let .backend(id) = kind.kind { return id }
            return nil
        }
        // Local is active, so it is not offered; both remotes are.
        XCTAssertEqual(backends, [.remote(HostID("studio")), .remote(HostID("laptop"))])
    }

    func testTheActiveRemoteIsExcludedAndLocalIsOffered() {
        let items = paletteResults(query: "", commands: [], worktrees: [],
                                   hosts: twoHosts(), activeBackend: .remote(HostID("studio")))
        let backends = items.compactMap { kind -> BackendID? in
            if case let .backend(id) = kind.kind { return id }
            return nil
        }
        XCTAssertEqual(backends, [.local, .remote(HostID("laptop"))])
    }

    func testBackendEntriesAreFuzzyMatchedAndTitled() {
        let items = paletteResults(query: "studio", commands: [], worktrees: [],
                                   hosts: twoHosts(), activeBackend: .local)
        let match = items.first { if case .backend = $0.kind { return true } else { return false } }
        let item = try! XCTUnwrap(match)
        XCTAssertEqual(item.title, "Connect to studio")
        XCTAssertEqual(item.subtitle, "studio.tail:7777")
        XCTAssertFalse(items.contains { $0.title == "Connect to laptop" })
    }

    func testAQueryMatchingOnlyTheAddressFindsTheHost() {
        // Hosts are routinely remembered by where they live, not what they were named.
        let items = paletteResults(query: "laptop.tail", commands: [], worktrees: [],
                                   hosts: twoHosts(), activeBackend: .local)
        let titles = items.compactMap { item -> String? in
            if case .backend = item.kind { return item.title }
            return nil
        }
        XCTAssertEqual(titles, ["Connect to laptop"])
    }

    func testANoiseQueryDoesNotSurfaceEveryBackend() {
        // "not" is a subsequence of "Connect to", which used to be part of every backend's
        // haystack — so every host matched every such query.
        for noise in ["not", "con", "cot", "net"] {
            let items = paletteResults(query: noise, commands: [], worktrees: [],
                                       hosts: twoHosts(), activeBackend: .remote(HostID("studio")))
            let backends = items.filter { if case .backend = $0.kind { return true }; return false }
            XCTAssertTrue(backends.isEmpty,
                          "\"\(noise)\" matches nothing in these hosts, got \(backends.map(\.title))")
        }
    }

    func testBackendsSortAfterCommandsAndBeforeAgents() {
        // NOTE: `CommandRegistry.all` is a FUNCTION taking a keymap, not a static property.
        let cmds = CommandRegistry.all(keymap: Keymap())
        let worktrees = [WorktreeInfo(pane: 1, project: "/p", name: "agent",
                                      branch: "clowder/agent", state: .idle)]
        let items = paletteResults(query: "", commands: cmds, worktrees: worktrees,
                                   hosts: twoHosts(), activeBackend: .local)
        func firstIndex(where pred: (PaletteItemKind) -> Bool) -> Int? {
            items.firstIndex { pred($0.kind) }
        }
        let cmdIdx = try! XCTUnwrap(firstIndex { if case .command = $0 { return true }; return false })
        let backIdx = try! XCTUnwrap(firstIndex { if case .backend = $0 { return true }; return false })
        let agentIdx = try! XCTUnwrap(firstIndex { if case .agent = $0 { return true }; return false })
        XCTAssertLessThan(cmdIdx, backIdx)
        XCTAssertLessThan(backIdx, agentIdx)
    }

    func testExistingCallersSeeNoBackendEntries() {
        // The `hosts:`/`activeBackend:` parameters are defaulted so every pre-M11b call site keeps
        // compiling and behaving identically.
        let items = paletteResults(query: "", commands: CommandRegistry.all(keymap: Keymap()),
                                   worktrees: [])
        XCTAssertFalse(items.contains { if case .backend = $0.kind { return true }; return false })
    }
}
