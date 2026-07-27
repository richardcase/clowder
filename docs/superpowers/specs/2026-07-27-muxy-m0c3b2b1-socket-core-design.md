# muxy M0c-3b2-b-1 — Socket / Threading Core

## Context

M0c-3b2-a proved the libghostty terminal renders a daemon-owned agent on screen. M0c-3b2-b builds
the app around it, split into this **testable core** (b2-b-1) and the **SwiftUI shell** (b2-b-2).
b2-b-1 is the real transport that connects `MuxyCore` to the daemon's JSON control socket, plus the
two fixes the M0c-3b1 final review flagged — all `swift test`-verifiable, no UI, subagent-executable.

Consumes M0c-3b1's `MuxyCore` (`ControlTransport`, `ControlSession`, `AgentStore`, the Codable
control models) + M0c-3a's JSON control socket (`MUXY_CONTROL_SOCK`, newline-delimited JSON).

## Components (all in `macos/Sources/MuxyCore/`)

### `LineBuffer` — byte-stream → newline lines

A pure value type that accumulates bytes and yields complete newline-terminated lines (newline
stripped), holding any trailing partial line across calls. This is the testable heart of the socket
read loop.

```swift
public struct LineBuffer {
    public init()
    public mutating func append(_ bytes: Data) -> [String]   // complete lines only
}
```

### `UnixSocketConnection` — the real `ControlTransport`

A POSIX Unix-domain socket implementation of `ControlTransport`:
- `init(path:)` — `socket(AF_UNIX, SOCK_STREAM)` + `connect` to `sockaddr_un(path)`.
- `setReceiver(_:)` — start a background read loop (`DispatchQueue`) that reads the fd, feeds bytes
  to a `LineBuffer`, and delivers each complete line **on `DispatchQueue.main`**.
- `send(line:)` — write `line + "\n"` to the fd.
- `deinit` — stop + `close`.

**Main-thread delivery is the key design point** (closes M0c-3b1 review finding #1): the real
transport reads on a background thread, so it hops each line to main *before* invoking the receiver.
Downstream (`ControlSession.handle` → `AgentStore.apply` mutating `@Published`) therefore runs on
main — no "publishing from a background thread" crash — with **zero change to the b1
`ControlSession`/`AgentStore`**. The b1 `FakeTransport` stays synchronous (fine for its tests). The
`ControlTransport` contract gains a documented note: *the receiver is invoked on the main thread.*

### `AgentStore.lastError`

Add `@Published public private(set) var lastError: String?`, set from `ControlEvent.error(message)`
in `apply` (closes M0c-3b1 review finding #2 — errors were dropped). The SwiftUI shell (b2-b-2)
surfaces it.

## Testability (`swift test`)

- **`LineBuffer`:** multiple lines in one chunk; a line split across two `append`s; a trailing
  partial held until its newline arrives; empty/blank lines.
- **`AgentStore.lastError`:** `apply(.error("boom"))` sets `lastError`; a later non-error event
  leaves it (or the shell clears it — keep it simple: last error wins).
- **`UnixSocketConnection` (end-to-end, no Rust daemon):** the test stands up an **in-process Swift
  POSIX Unix-socket server** (`socket`/`bind`/`listen`/`accept` on a temp path). `UnixSocketConnection`
  connects; the test asserts (a) `send(line:)` reaches the server, and (b) a JSON `ControlEvent` line
  the server writes is delivered to the receiver **on the main thread** (assert `Thread.isMainThread`),
  driven via an `XCTestExpectation` + `waitForExpectations` so the main run loop pumps the
  `DispatchQueue.main.async` delivery. This verifies the real socket transport without the daemon.

## Deferred (M0c-3b2-b-2)

The SwiftUI app: `@main App` + `NavigationSplitView` (sidebar from `AgentStore.byProject` + attention
badges + `lastError` surfacing), the `SurfaceView` (wrapped in `NSViewRepresentable`) for the selected
agent, a spawn button/sheet (sends `ControlRequest.spawnAgent`), and closing the spike's input/resize
gaps (pump `SIGWINCH`→`Resize`, mouse/IME). Verified by running it.

## Verification

`cd macos && swift test` — all core tests green (existing 16 + the new `LineBuffer`/`lastError`/
`UnixSocketConnection` tests). Manual (later, in b2-b-2): run the daemon, spawn an agent, and confirm
a small tool using `UnixSocketConnection` + `ControlSession` receives the live `AgentList` + events.
