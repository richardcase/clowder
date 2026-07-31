import XCTest
import ClowderCore

final class RemotePathsTests: XCTestCase {
    func testForwarderSocketDirIsRemoteSubdirOfControlParent() {
        XCTAssertEqual(
            forwarderSocketDir(controlPath: "/x/clowder/clowder-control.sock"),
            "/x/clowder/remote"
        )
        XCTAssertEqual(
            forwarderSocketDir(controlPath: "/run/user/501/clowder/clowder-control.sock"),
            "/run/user/501/clowder/remote"
        )
    }

    func testForwarderSocketDirNeverDoublesToRemoteRemote() {
        // The app must feed the DEFAULT control path (not the forwarder's own socket), so the
        // derived dir must never contain a nested `remote/remote`.
        let dir = forwarderSocketDir(controlPath: "/tmp/clowder/clowder-control.sock")
        XCTAssertFalse(dir.contains("remote/remote"))
    }
}
