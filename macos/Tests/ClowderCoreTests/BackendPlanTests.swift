// SPDX-License-Identifier: Apache-2.0

import XCTest
@testable import ClowderCore

final class BackendPlanTests: XCTestCase {
    private let sockets = SocketPaths(
        client: "/run/clowder/clowder.sock",
        control: "/run/clowder/clowder-control.sock",
        hook: "/run/clowder/clowder-hook.sock"
    )

    private func host(_ name: String) -> RemoteHost {
        RemoteHost(name: name, address: "\(name).tail:7777", tls: true, hasToken: true,
                   fingerprint: "a1b2", trusted: true, source: .registry)
    }

    func testLocalPlanSpawnsTheDaemonWithExplicitSockets() {
        let plan = backendPlan(target: .local, sockets: sockets)
        XCTAssertEqual(plan.id, .local)
        XCTAssertEqual(plan.executable, .daemon)
        XCTAssertTrue(plan.args.isEmpty, "the local daemon takes its config from the environment")
        XCTAssertEqual(plan.envOverlay["CLOWDER_SOCK"], sockets.client)
        XCTAssertEqual(plan.envOverlay["CLOWDER_CONTROL_SOCK"], sockets.control)
        XCTAssertEqual(plan.envOverlay["CLOWDER_HOOK_SOCK"], sockets.hook)
        XCTAssertEqual(plan.controlPath, sockets.control)
        XCTAssertEqual(plan.renderPath, sockets.client)
    }

    func testRemotePlanSpawnsConnectWithAnExplicitSocketDir() {
        let plan = backendPlan(target: .remote(host("studio")), sockets: sockets)
        XCTAssertEqual(plan.id, .remote(HostID("studio")))
        XCTAssertEqual(plan.executable, .clowder)
        // The app passes --socket-dir so there is ONE authority for this path. Before M11a the
        // forwarder derived it and Swift re-derived the same rule; that duplication is now gone.
        XCTAssertEqual(plan.args, ["connect", "studio", "--socket-dir", "/run/clowder/remote/studio"])
        XCTAssertEqual(plan.controlPath, "/run/clowder/remote/studio/clowder-control.sock")
        XCTAssertEqual(plan.renderPath, "/run/clowder/remote/studio/clowder.sock")
    }

    func testRemotePlanSelectsByNameNotAddress() {
        // `clowder connect` resolves a nickname through the registry, which is what carries the
        // host's token and pin. Passing the address would produce an ad-hoc TOFU target instead.
        let plan = backendPlan(target: .remote(host("studio")), sockets: sockets)
        XCTAssertEqual(plan.args[1], "studio")
        XCTAssertFalse(plan.args.contains("studio.tail:7777"))
    }

    func testRemotePlanDoesNotOverrideTheSocketEnvVars() {
        // Setting CLOWDER_CONTROL_SOCK for the forwarder would change what IT considers the
        // default control socket. The app controls the path via --socket-dir instead.
        let plan = backendPlan(target: .remote(host("studio")), sockets: sockets)
        XCTAssertNil(plan.envOverlay["CLOWDER_CONTROL_SOCK"])
        XCTAssertNil(plan.envOverlay["CLOWDER_SOCK"])
        XCTAssertNil(plan.envOverlay["CLOWDER_HOOK_SOCK"])
    }

    func testForwarderDirIsPerHost() {
        XCTAssertEqual(forwarderSocketDir(controlPath: sockets.control, host: "studio"),
                       "/run/clowder/remote/studio")
        XCTAssertEqual(forwarderSocketDir(controlPath: sockets.control, host: "laptop"),
                       "/run/clowder/remote/laptop")
    }

    func testTwoHostsGetDistinctSocketPaths() {
        let a = backendPlan(target: .remote(host("studio")), sockets: sockets)
        let b = backendPlan(target: .remote(host("laptop")), sockets: sockets)
        XCTAssertNotEqual(a.controlPath, b.controlPath)
        XCTAssertNotEqual(a.renderPath, b.renderPath)
    }
}
