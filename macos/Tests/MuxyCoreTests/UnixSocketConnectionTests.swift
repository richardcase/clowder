import XCTest
@testable import MuxyCore

final class UnixSocketConnectionTests: XCTestCase {
    /// Bind a POSIX Unix stream socket at `path` and return the listening fd.
    private func listenSocket(at path: String) -> Int32 {
        unlink(path)
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        precondition(fd >= 0)
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pb = path.utf8CString
        withUnsafeMutablePointer(to: &addr.sun_path) { p in
            p.withMemoryRebound(to: CChar.self, capacity: pb.count) { dst in
                for (i, b) in pb.enumerated() { dst[i] = b }
            }
        }
        let len = socklen_t(MemoryLayout<sockaddr_un>.size)
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(fd, $0, len) }
        }
        precondition(rc == 0, "bind failed: \(errno)")
        precondition(listen(fd, 1) == 0)
        return fd
    }

    func testConnectSendReceiveOnMain() throws {
        let path = NSTemporaryDirectory() + "muxy-ut-\(UUID().uuidString).sock"
        let serverFd = listenSocket(at: path)
        defer { close(serverFd); unlink(path) }

        let serverGotRequest = expectation(description: "server received listAgents")
        DispatchQueue.global().async {
            let conn = accept(serverFd, nil, nil)
            precondition(conn >= 0)
            var buf = [UInt8](repeating: 0, count: 1024)
            let n = read(conn, &buf, buf.count)
            if n > 0, String(decoding: buf[0..<n], as: UTF8.self).contains("listAgents") {
                serverGotRequest.fulfill()
            }
            let reply = #"{"type":"agentList","agents":[{"pane":1,"project":"p","task":"t","state":"Working"}]}"# + "\n"
            _ = reply.withCString { write(conn, $0, strlen($0)) }
            Thread.sleep(forTimeInterval: 0.2)
            close(conn)
        }

        let conn = try UnixSocketConnection(path: path)
        let deliveredOnMain = expectation(description: "agentList delivered on main")
        conn.setReceiver { line in
            XCTAssertTrue(Thread.isMainThread, "receiver must run on the main thread")
            if line.contains("agentList") { deliveredOnMain.fulfill() }
        }
        try conn.send(line: #"{"type":"listAgents"}"#)

        wait(for: [serverGotRequest, deliveredOnMain], timeout: 5.0)
    }

    func testConnectToMissingSocketThrows() {
        let path = NSTemporaryDirectory() + "muxy-nope-\(UUID().uuidString).sock"
        XCTAssertThrowsError(try UnixSocketConnection(path: path))
    }

    func testDisconnectClosesSocket() throws {
        let path = NSTemporaryDirectory() + "muxy-dc-\(UUID().uuidString).sock"
        let serverFd = listenSocket(at: path)
        defer { Darwin.close(serverFd); unlink(path) }
        DispatchQueue.global().async { _ = accept(serverFd, nil, nil) }

        let conn = try UnixSocketConnection(path: path)
        conn.disconnect()
        // fd is closed → a subsequent write must fail.
        XCTAssertThrowsError(try conn.send(line: "x"))
    }

    func testOnCloseFiresOnMainThreadWhenPeerCloses() throws {
        let path = NSTemporaryDirectory() + "muxy-onclose-\(UUID().uuidString).sock"
        let serverFd = listenSocket(at: path)
        defer { Darwin.close(serverFd); unlink(path) }

        let conn = try UnixSocketConnection(path: path)

        let closed = expectation(description: "onClose fired")
        var firedOnMain = false
        conn.setOnClose {
            firedOnMain = Thread.isMainThread
            closed.fulfill()
        }
        conn.setReceiver { _ in }          // starts the read loop
        let clientFd = accept(serverFd, nil, nil)    // ensure the connection is established
        precondition(clientFd >= 0)
        Darwin.close(clientFd)                // peer closes -> read() returns 0 -> loop exits

        wait(for: [closed], timeout: 2.0)
        XCTAssertTrue(firedOnMain, "onClose must be delivered on the main thread")
        conn.disconnect()
    }

    func testOnCloseFiresAtMostOnce() throws {
        let path = NSTemporaryDirectory() + "muxy-once-\(UUID().uuidString).sock"
        let serverFd = listenSocket(at: path)
        defer { Darwin.close(serverFd); unlink(path) }

        let conn = try UnixSocketConnection(path: path)
        var count = 0
        let fired = expectation(description: "onClose fired once")
        fired.assertForOverFulfill = false
        conn.setOnClose { count += 1; fired.fulfill() }
        conn.setReceiver { _ in }
        let clientFd = accept(serverFd, nil, nil)
        precondition(clientFd >= 0)
        Darwin.close(clientFd)

        conn.disconnect()                   // triggers loop exit
        wait(for: [fired], timeout: 2.0)
        conn.disconnect()                   // idempotent, must not fire again
        RunLoop.main.run(until: Date().addingTimeInterval(0.2))
        XCTAssertEqual(count, 1)
    }
}
