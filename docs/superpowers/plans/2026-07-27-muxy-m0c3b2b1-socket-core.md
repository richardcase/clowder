# muxy M0c-3b2-b-1 — Socket / Threading Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `MuxyCore` a real `ControlTransport` — a POSIX Unix-socket connection to the daemon's JSON control socket — plus a testable `LineBuffer` and the two fixes the M0c-3b1 review flagged (main-thread line delivery; `AgentStore.lastError`). All `swift test`; the SwiftUI shell is b2-b-2.

**Architecture:** In the `macos/MuxyCore` library. `LineBuffer` (pure) splits a byte stream into newline lines. `UnixSocketConnection` implements the existing `ControlTransport`: a background read loop feeds `LineBuffer` and delivers each line on `DispatchQueue.main` (so downstream `AgentStore` `@Published` mutations are main-safe — no change to `ControlSession`/`AgentStore`'s logic beyond adding `lastError`). Verified end-to-end against an in-process Swift Unix-socket server, no Rust daemon needed.

**Tech Stack:** Swift 6.3 (CLT), SwiftPM, Foundation + Darwin POSIX (`socket`/`connect`/`bind`), XCTest.

## Global Constraints

- **Swift on PATH.** Tests run `cd /Users/richard/code/muxy/macos && swift test`. (Rust `cargo test` is a separate build, unaffected.)
- **Package is `.v13` / `.v5` language mode** (already set). Additions go in `macos/Sources/MuxyCore/`.
- **Do not break M0c-3b1.** All 16 existing `MuxyCore` tests stay green; the `ControlTransport` protocol and `ControlSession` are unchanged (only `AgentStore` gains `lastError`). Run `swift test` after each task.
- **`ControlTransport` contract note:** implementations invoke the receiver **on the main thread**. `UnixSocketConnection` guarantees this; the b1 `FakeTransport` (synchronous, test-only) is unaffected.
- **Explicit `git add`** of changed files; never `git add .`; do not commit `macos/.build/` or `macos/vendor/` (already gitignored).
- **Deferred to b2-b-2:** the SwiftUI app (window, sidebar, `SurfaceView` embedding, spawn button) and the pump `SIGWINCH`→`Resize` / mouse / IME gaps.

---

### Task 1: `LineBuffer` — byte-stream → newline lines

**Files:**
- Create: `macos/Sources/MuxyCore/LineBuffer.swift`
- Create: `macos/Tests/MuxyCoreTests/LineBufferTests.swift`

**Interfaces:**
- Produces: `struct LineBuffer { init(); mutating func append(_ bytes: Data) -> [String] }` — returns complete newline-terminated lines (newline stripped), holding any trailing partial across calls.

- [ ] **Step 1: Create `macos/Sources/MuxyCore/LineBuffer.swift`**

```swift
import Foundation

/// Accumulates bytes and yields complete newline-terminated lines (newline stripped),
/// holding any trailing partial line until its newline arrives.
public struct LineBuffer {
    private var pending = Data()

    public init() {}

    public mutating func append(_ bytes: Data) -> [String] {
        pending.append(bytes)
        var lines: [String] = []
        while let nl = pending.firstIndex(of: 0x0A) {
            let lineData = pending.subdata(in: pending.startIndex..<nl)
            pending.removeSubrange(pending.startIndex...nl)
            lines.append(String(decoding: lineData, as: UTF8.self))
        }
        return lines
    }
}
```

- [ ] **Step 2: Create `macos/Tests/MuxyCoreTests/LineBufferTests.swift`**

```swift
import XCTest
@testable import MuxyCore

final class LineBufferTests: XCTestCase {
    func testMultipleLinesInOneChunk() {
        var b = LineBuffer()
        XCTAssertEqual(b.append(Data("a\nbb\nccc\n".utf8)), ["a", "bb", "ccc"])
    }

    func testLineSplitAcrossAppends() {
        var b = LineBuffer()
        XCTAssertEqual(b.append(Data("hel".utf8)), [])
        XCTAssertEqual(b.append(Data("lo\n".utf8)), ["hello"])
    }

    func testTrailingPartialHeldUntilNewline() {
        var b = LineBuffer()
        XCTAssertEqual(b.append(Data("done\npart".utf8)), ["done"])
        XCTAssertEqual(b.append(Data("ial\n".utf8)), ["partial"])
    }

    func testBlankLines() {
        var b = LineBuffer()
        XCTAssertEqual(b.append(Data("\n\nx\n".utf8)), ["", "", "x"])
    }

    func testNoNewlineYieldsNothing() {
        var b = LineBuffer()
        XCTAssertEqual(b.append(Data("nolf".utf8)), [])
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cd /Users/richard/code/muxy/macos && swift test --filter LineBufferTests`, then `cd /Users/richard/code/muxy/macos && swift test` (all green).
Expected: 5 new pass; 21 total (16 prior + 5).

- [ ] **Step 4: Commit**

```bash
git add macos/Sources/MuxyCore/LineBuffer.swift macos/Tests/MuxyCoreTests/LineBufferTests.swift
git commit -m "feat(macos): LineBuffer — byte-stream to newline lines"
```

---

### Task 2: `AgentStore.lastError`

**Files:**
- Modify: `macos/Sources/MuxyCore/AgentStore.swift` (add `lastError`; set it on `.error`)
- Modify: `macos/Tests/MuxyCoreTests/AgentStoreTests.swift` (add one test)

**Interfaces:**
- Adds `@Published public private(set) var lastError: String?` on `AgentStore`; `apply(.error(message))` sets it.

- [ ] **Step 1: Add `lastError` in `macos/Sources/MuxyCore/AgentStore.swift`**

Add the published property alongside `agents`/`needsRefresh`:
```swift
    @Published public private(set) var lastError: String?
```
Change the `apply` `error` case from ignoring to recording it (last error wins):
```swift
        case let .error(message):
            lastError = message
```

- [ ] **Step 2: Add the test to `macos/Tests/MuxyCoreTests/AgentStoreTests.swift`**

```swift
    func testErrorEventSetsLastError() {
        let s = AgentStore()
        XCTAssertNil(s.lastError)
        s.apply(.error(message: "boom"))
        XCTAssertEqual(s.lastError, "boom")
    }
```

- [ ] **Step 3: Run tests**

Run: `cd /Users/richard/code/muxy/macos && swift test`
Expected: PASS — 22 total (21 + 1). Existing AgentStore tests unchanged.

- [ ] **Step 4: Commit**

```bash
git add macos/Sources/MuxyCore/AgentStore.swift macos/Tests/MuxyCoreTests/AgentStoreTests.swift
git commit -m "feat(macos): AgentStore.lastError surfaces daemon error events"
```

---

### Task 3: `UnixSocketConnection` — the real `ControlTransport`

**Files:**
- Create: `macos/Sources/MuxyCore/UnixSocketConnection.swift`
- Create: `macos/Tests/MuxyCoreTests/UnixSocketConnectionTests.swift`

**Interfaces:**
- Produces: `final class UnixSocketConnection: ControlTransport` — `init(path:) throws`, `setReceiver(_:)` (starts a background read loop delivering lines on `DispatchQueue.main`), `send(line:) throws`.

- [ ] **Step 1: Create `macos/Sources/MuxyCore/UnixSocketConnection.swift`**

```swift
import Foundation

/// A ControlTransport over a POSIX Unix-domain stream socket. The read loop runs on a
/// background queue and delivers each complete line ON THE MAIN QUEUE, so downstream
/// AgentStore @Published mutations are main-thread-safe.
public final class UnixSocketConnection: ControlTransport {
    private let fd: Int32
    private var receiver: ((String) -> Void)?
    private let readQueue = DispatchQueue(label: "muxy.control.read")
    private var isRunning = true

    public init(path: String) throws {
        fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let maxLen = MemoryLayout.size(ofValue: addr.sun_path) // 104 on macOS
        let pathBytes = path.utf8CString                       // includes NUL
        guard pathBytes.count <= maxLen else {
            close(fd)
            throw POSIXError(.ENAMETOOLONG)
        }
        withUnsafeMutablePointer(to: &addr.sun_path) { p in
            p.withMemoryRebound(to: CChar.self, capacity: maxLen) { dst in
                for (i, b) in pathBytes.enumerated() where i < maxLen { dst[i] = b }
            }
        }
        let len = socklen_t(MemoryLayout<sockaddr_un>.size)
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, len) }
        }
        guard rc == 0 else {
            close(fd)
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .ECONNREFUSED)
        }
    }

    public func setReceiver(_ receiver: @escaping (String) -> Void) {
        self.receiver = receiver
        readQueue.async { [weak self] in self?.readLoop() }
    }

    private func readLoop() {
        var buf = [UInt8](repeating: 0, count: 4096)
        var lineBuffer = LineBuffer()
        while isRunning {
            let n = read(fd, &buf, buf.count)
            if n <= 0 { break }
            let lines = lineBuffer.append(Data(buf[0..<n]))
            for line in lines {
                DispatchQueue.main.async { [weak self] in self?.receiver?(line) }
            }
        }
    }

    public func send(line: String) throws {
        let bytes = Array((line + "\n").utf8)
        try bytes.withUnsafeBytes { raw in
            var off = 0
            while off < raw.count {
                let n = write(fd, raw.baseAddress!.advanced(by: off), raw.count - off)
                if n <= 0 { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
                off += n
            }
        }
    }

    deinit {
        isRunning = false
        close(fd)
    }
}
```

- [ ] **Step 2: Create `macos/Tests/MuxyCoreTests/UnixSocketConnectionTests.swift`**

An in-process POSIX Unix-socket server (no Rust daemon): accept one connection, read the request, reply with a JSON event line; assert the client sends and receives (on main).

```swift
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
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, len) }
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
}
```

- [ ] **Step 3: Run tests**

Run: `cd /Users/richard/code/muxy/macos && swift test --filter UnixSocketConnectionTests`, then `cd /Users/richard/code/muxy/macos && swift test` (all green).
Expected: 2 new pass; 24 total. If the socket test flakes on timing, raise the `wait` timeout — do NOT weaken the main-thread assertion.

- [ ] **Step 4: Commit**

```bash
git add macos/Sources/MuxyCore/UnixSocketConnection.swift macos/Tests/MuxyCoreTests/UnixSocketConnectionTests.swift
git commit -m "feat(macos): UnixSocketConnection ControlTransport (main-thread delivery)"
```

---

## What M0c-3b2-b-1 excludes (b2-b-2)

The SwiftUI `@main App` + `NavigationSplitView` sidebar (from `AgentStore.byProject` + badges + `lastError`), the `SurfaceView` (`NSViewRepresentable`) per selected agent, the spawn button/sheet, and the pump `SIGWINCH`→`Resize` / mouse / IME gaps.

## Self-Review

- **Spec coverage:** `LineBuffer` byte→lines (Task 1) ✓; `AgentStore.lastError` from `.error` — b1 review #2 (Task 2) ✓; `UnixSocketConnection` POSIX `ControlTransport` delivering **on main** — b1 review #1 (Task 3) ✓; verified against an in-process socket server, no daemon ✓; `ControlSession`/`ControlTransport` protocol unchanged ✓.
- **Placeholder scan:** every step has complete Swift code; no TBD.
- **Type consistency:** `UnixSocketConnection` conforms to the existing `ControlTransport` (`setReceiver`/`send(line:)`); it consumes `LineBuffer` from Task 1; `AgentStore.lastError` (Task 2) is used only by the shell later.
- **Threading:** the main-thread delivery lives entirely in `UnixSocketConnection.readLoop` (`DispatchQueue.main.async`); `ControlSession`/`AgentStore` are untouched w.r.t. threading; the socket test asserts `Thread.isMainThread` via `waitForExpectations` (which pumps the main run loop).
- **M0c-3b1 preservation:** the 16 existing tests stay green; only additive changes (new files + a `lastError` property + the `.error` case body).
