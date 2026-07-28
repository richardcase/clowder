# muxy M0c-3b2-b-2 — SwiftUI Shell (+resize, mouse/IME) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the proven single-surface `MuxyApp` into a real macOS app — a sidebar of agents grouped by project with attention badges, a terminal pane per selected agent, a GUI spawn action, a correct connection lifecycle — and close the terminal input gaps (window resize propagation, mouse, IME).

**Architecture:** All testable, libghostty-free logic (the connection/selection state machine `AppModel`, and the transport close/disconnect lifecycle) lives in `MuxyCore` and is unit-tested with `swift test`. The view layer (`@main` App, sidebar, spawn sheet, surface hosting) and the `SurfaceView` mouse/IME additions live in `MuxyApp` and are verified by `swift build` + a manual run. The resize fix is a self-contained Rust change in `muxy-client` verified with `cargo test`.

**Tech Stack:** Swift 6 (language mode v5, macOS 13), SwiftUI + AppKit, libghostty (via `GhosttyKit`), Combine; Rust (tokio "full", crossterm 0.28) in `muxy-client`.

## Global Constraints

- **`MuxyCore` stays libghostty-free.** `AppModel` and the transport changes must not import `GhosttyKit`/AppKit, so `swift test` runs without the vendored static lib. Only `MuxyApp` links libghostty.
- **The only `MuxyCore` protocol change is additive.** `ControlTransport` gains `setOnClose` and `disconnect`, both with no-op default implementations, so the b1 `FakeTransport` and all 25 existing tests keep compiling and passing unchanged.
- **F1 (teardown):** app/scene teardown MUST call `connection.disconnect()` explicitly. Never rely on `deinit` — the read loop is parked in `read()` and keeps the object alive.
- **F2 (closed signal):** the on-close handler is invoked exactly once, on the main thread, when the read loop exits (peer close, error, or `disconnect()`).
- **JSON control contract is frozen** (Rust↔Swift, already verified): `ControlRequest`/`ControlEvent` shapes are unchanged. No new proto variants — `ClientToDaemon::Resize { pane, cols, rows }` already exists and the daemon already handles it.
- **IME must not regress plain typing.** Enter, Ctrl-C, arrows, and printable ASCII must behave exactly as in the M0c-3b2-a proof after mouse/IME is added.
- **Mouse coordinates** are passed to libghostty as points with a **top-left origin** (flip AppKit's bottom-left Y).
- **Control socket:** env `MUXY_CONTROL_SOCK`, default `/tmp/muxy-control.sock` (matches the daemon). `MUXY_SOCK` default `/tmp/muxy.sock`; `MUXY_BIN` default as in current `main.swift`.
- Commit after each task with a conventional message; end messages with the standard trailers.

**Test commands:**
- Swift core: `cd macos && swift test`
- Swift app build gate (UI tasks): `cd macos && swift build`
- Rust: `cargo test -p muxy-client` (and `cargo test` for the full suite)

---

## Task 1: `ControlTransport` close/disconnect lifecycle (MuxyCore)

Add the F2 close signal and make `disconnect` part of the protocol, so `AppModel` (Task 2) can hang up and be notified — with zero change to existing transports/tests.

**Files:**
- Modify: `macos/Sources/MuxyCore/ControlSession.swift` (the `ControlTransport` protocol)
- Modify: `macos/Sources/MuxyCore/UnixSocketConnection.swift`
- Test: `macos/Tests/MuxyCoreTests/UnixSocketConnectionTests.swift` (extend)

**Interfaces:**
- Produces: `ControlTransport.setOnClose(_:)`, `ControlTransport.disconnect()` (both with default no-op implementations); `UnixSocketConnection` fires the close handler once, on main, at read-loop exit.
- Consumes: nothing new.

- [ ] **Step 1: Write the failing test** — append to `UnixSocketConnectionTests.swift`. It stands up the same in-process POSIX server the existing tests use (server `socket`/`bind`/`listen`/`accept` on a temp path), connects a `UnixSocketConnection`, registers `setOnClose`, then closes the server side and asserts the handler fires **on the main thread**:

```swift
func testOnCloseFiresOnMainThreadWhenPeerCloses() throws {
    let path = Self.tempSocketPath()
    let server = try InProcessSocketServer(path: path)   // existing test helper
    let conn = try UnixSocketConnection(path: path)

    let closed = expectation(description: "onClose fired")
    var firedOnMain = false
    conn.setOnClose {
        firedOnMain = Thread.isMainThread
        closed.fulfill()
    }
    conn.setReceiver { _ in }          // starts the read loop
    let client = try server.accept()    // ensure the connection is established
    server.close(client)                // peer closes -> read() returns 0 -> loop exits

    wait(for: [closed], timeout: 2.0)
    XCTAssertTrue(firedOnMain, "onClose must be delivered on the main thread")
    conn.disconnect()
}

func testOnCloseFiresAtMostOnce() throws {
    let path = Self.tempSocketPath()
    let server = try InProcessSocketServer(path: path)
    let conn = try UnixSocketConnection(path: path)
    var count = 0
    let fired = expectation(description: "onClose fired once")
    fired.assertForOverFulfill = false
    conn.setOnClose { count += 1; fired.fulfill() }
    conn.setReceiver { _ in }
    _ = try server.accept()
    conn.disconnect()                   // triggers loop exit
    wait(for: [fired], timeout: 2.0)
    conn.disconnect()                   // idempotent, must not fire again
    RunLoop.main.run(until: Date().addingTimeInterval(0.2))
    XCTAssertEqual(count, 1)
}
```

> If the existing test file names its helpers differently (e.g. the server type or `tempSocketPath()`), reuse the existing names — do not introduce a second server helper. The two behaviors under test are: (a) close handler fires on main on peer close, (b) it fires at most once.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd macos && swift test --filter UnixSocketConnectionTests`
Expected: FAIL — `setOnClose` does not exist on `UnixSocketConnection`.

- [ ] **Step 3: Extend the `ControlTransport` protocol** in `ControlSession.swift`:

```swift
public protocol ControlTransport: AnyObject {
    /// Register a callback invoked once per inbound line (newline stripped).
    func setReceiver(_ receiver: @escaping (String) -> Void)
    /// Send one request line (the implementation appends the newline).
    func send(line: String) throws
    /// Register a handler invoked once, on the main thread, when the channel closes
    /// (peer close, read error, or `disconnect()`).
    func setOnClose(_ handler: @escaping () -> Void)
    /// Proactively close the channel. Idempotent.
    func disconnect()
}

public extension ControlTransport {
    func setOnClose(_ handler: @escaping () -> Void) {}
    func disconnect() {}
}
```

- [ ] **Step 4: Wire the close handler in `UnixSocketConnection.swift`.** Add storage, a `setOnClose`, a once-guard, and fire it when `readLoop` exits:

```swift
private var onClose: (() -> Void)?
private var didFireClose = false   // read/written only on the main queue

public func setOnClose(_ handler: @escaping () -> Void) {
    self.onClose = handler
}

private func fireClose() {          // always invoked on the main queue
    guard !didFireClose else { return }
    didFireClose = true
    onClose?()
}
```

At the end of `readLoop()`, after the `while` loop breaks, dispatch the fire onto main:

```swift
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
    DispatchQueue.main.async { [weak self] in self?.fireClose() }
}
```

`disconnect()` is unchanged (it already `shutdown`s the fd, which unblocks `read()` so the loop exits and `fireClose` runs). It is already a `public func` and now satisfies the protocol requirement.

> Note in a comment: if `disconnect()` is called before `setReceiver` ever starts the loop, no close fires — the handler is about an established channel dying, which is the F2 case.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd macos && swift test`
Expected: PASS — all prior tests (25) plus the two new ones. Confirm the count did not drop.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/MuxyCore/ControlSession.swift macos/Sources/MuxyCore/UnixSocketConnection.swift macos/Tests/MuxyCoreTests/UnixSocketConnectionTests.swift
git commit -m "feat(core): ControlTransport close signal + disconnect on the protocol (F2)"
```

---

## Task 2: `AppModel` connection/selection state machine (MuxyCore)

The libghostty-free heart of the app: owns and retains the connection + session, exposes the store, tracks selection and connection state, and does F1 teardown. Fully unit-tested with a fake transport.

**Files:**
- Create: `macos/Sources/MuxyCore/AppModel.swift`
- Test: `macos/Tests/MuxyCoreTests/AppModelTests.swift`

**Interfaces:**
- Consumes: `ControlTransport` (incl. Task 1's `setOnClose`/`disconnect`), `ControlSession`, `AgentStore`, `ControlRequest`.
- Produces:
  - `AppModel(store:makeTransport:)` where `makeTransport: () throws -> ControlTransport`
  - `let store: AgentStore`
  - `@Published var selectedPane: UInt64?`
  - `@Published private(set) var connectionState: AppModel.ConnectionState` (`connecting`/`live`/`closed(reason:)`, `Equatable`)
  - `func connect()`, `func spawn(project:task:adapter:)`, `func shutdown()`

- [ ] **Step 1: Write the failing test** — `AppModelTests.swift`. Uses an in-target fake transport so no real socket is needed. All test methods are `@MainActor` (AppModel is `@MainActor`):

```swift
import XCTest
@testable import MuxyCore

final class FakeControlTransport: ControlTransport {
    private(set) var sentLines: [String] = []
    private(set) var disconnected = false
    var receiver: ((String) -> Void)?
    var onClose: (() -> Void)?
    func setReceiver(_ receiver: @escaping (String) -> Void) { self.receiver = receiver }
    func send(line: String) throws { sentLines.append(line) }
    func setOnClose(_ handler: @escaping () -> Void) { self.onClose = handler }
    func disconnect() { disconnected = true; onClose?() }
    /// Test helper: simulate the daemon pushing a JSON line.
    func deliver(_ line: String) { receiver?(line) }
}

@MainActor
final class AppModelTests: XCTestCase {
    func testConnectGoesLiveAndRequestsAgentList() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        XCTAssertEqual(model.connectionState, .live)
        XCTAssertTrue(fake.sentLines.contains { $0.contains("\"type\":\"listAgents\"") })
    }

    func testConnectFailureBecomesClosed() {
        struct BoomError: Error {}
        let model = AppModel(makeTransport: { throw BoomError() })
        model.connect()
        guard case .closed = model.connectionState else {
            return XCTFail("expected .closed, got \(model.connectionState)")
        }
    }

    func testOnCloseTransitionsToClosed() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        fake.onClose?()                       // simulate daemon death
        guard case .closed = model.connectionState else {
            return XCTFail("expected .closed after onClose")
        }
    }

    func testSpawnSendsSpawnAgent() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.spawn(project: "/tmp/repo", task: "demo", adapter: "claude")
        XCTAssertTrue(fake.sentLines.contains {
            $0.contains("\"type\":\"spawnAgent\"") &&
            $0.contains("\"project\":\"/tmp/repo\"") &&
            $0.contains("\"task\":\"demo\"") &&
            $0.contains("\"adapter\":\"claude\"")
        })
    }

    func testShutdownDisconnectsTransport() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        model.shutdown()
        XCTAssertTrue(fake.disconnected)
    }

    func testAppliedEventsFlowToStore() {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        fake.deliver(#"{"type":"agentList","agents":[{"pane":1,"project":"/p","task":"t","state":"Working"}]}"#)
        XCTAssertEqual(model.store.agents[1]?.task, "t")
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter AppModelTests`
Expected: FAIL — no `AppModel` type.

- [ ] **Step 3: Implement `AppModel.swift`:**

```swift
import Foundation
import Combine

/// Owns the control channel and the app's selection. Libghostty-free so it is unit-testable.
/// Retaining `session` is what keeps ControlSession's `[weak self]` receiver alive.
@MainActor
public final class AppModel: ObservableObject {
    public enum ConnectionState: Equatable {
        case connecting
        case live
        case closed(reason: String)
    }

    public let store: AgentStore
    @Published public var selectedPane: UInt64?
    @Published public private(set) var connectionState: ConnectionState = .connecting

    private let makeTransport: () throws -> ControlTransport
    private var connection: ControlTransport?
    private var session: ControlSession?

    public init(store: AgentStore = AgentStore(),
                makeTransport: @escaping () throws -> ControlTransport) {
        self.store = store
        self.makeTransport = makeTransport
    }

    /// Build the transport + session and hydrate. On any failure, land in `.closed`.
    public func connect() {
        connectionState = .connecting
        do {
            let transport = try makeTransport()
            transport.setOnClose { [weak self] in
                // UnixSocketConnection already delivers this on the main queue.
                self?.connectionState = .closed(reason: "Disconnected from daemon")
            }
            let session = ControlSession(transport: transport, store: store)
            self.connection = transport
            self.session = session
            connectionState = .live
            try session.send(.listAgents)
        } catch {
            connectionState = .closed(reason: "Could not connect: \(error)")
        }
    }

    public func spawn(project: String, task: String, adapter: String) {
        guard let session else { return }
        do {
            try session.send(.spawnAgent(project: project, task: task, adapter: adapter))
        } catch {
            connectionState = .closed(reason: "Send failed: \(error)")
        }
    }

    /// Explicit teardown (F1): never rely on deinit — the read loop keeps the
    /// connection alive while parked in read().
    public func shutdown() {
        connection?.disconnect()
        session = nil
        connection = nil
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd macos && swift test`
Expected: PASS — all prior tests plus the 6 new `AppModelTests`.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/MuxyCore/AppModel.swift macos/Tests/MuxyCoreTests/AppModelTests.swift
git commit -m "feat(core): AppModel connection/selection state machine (F1)"
```

---

## Task 3: `muxy attach` resize propagation (muxy-client, Rust)

Make the attach client send the terminal size to the daemon on attach and on every SIGWINCH, so a resized window reflows the agent's PTY. Daemon side already handles `Resize`.

**Files:**
- Modify: `crates/muxy-client/src/lib.rs` (`pump`, `attach`, add `resize_msg`)
- Test: `crates/muxy-client/src/lib.rs` `#[cfg(test)]` module (extend)

**Interfaces:**
- Produces: `pub fn resize_msg(pane: PaneId, cols: u16, rows: u16) -> ClientToDaemon`; `pump` gains a resize-source parameter `resizes: tokio::sync::mpsc::Receiver<(u16, u16)>`.
- Consumes: `ClientToDaemon::Resize` (exists), `crossterm::terminal::size`, `tokio::signal::unix`.

- [ ] **Step 1: Write the failing unit test for `resize_msg`** — in the test module:

```rust
#[test]
fn resize_msg_builds_resize_variant() {
    let m = resize_msg(PaneId(7), 120, 40);
    assert_eq!(m, ClientToDaemon::Resize { pane: PaneId(7), cols: 120, rows: 40 });
}
```

- [ ] **Step 2: Write the failing integration test for the resize arm** — drives a resize through the injected channel and asserts the daemon side receives `Attach` then `Resize`, using `MsgStream` directly (no daemon needed):

```rust
#[tokio::test]
async fn pump_forwards_resize_from_channel() {
    use muxy_proto::MsgStream;
    let pane = PaneId(3);
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (tx, rx) = tokio::sync::mpsc::channel::<(u16, u16)>(8);

    // Empty stdin (never yields) and a sink stdout.
    let (input_reader, _input_writer) = tokio::io::duplex(64);
    let (_out_reader, out_writer) = tokio::io::duplex(64);

    let pump_task = tokio::spawn(async move {
        pump(client_io, pane, input_reader, out_writer, rx).await
    });

    tx.send((100, 40)).await.unwrap();

    let mut server = MsgStream::new(server_io);
    // First frame is Attach, then our Resize.
    let first: ClientToDaemon = server.recv().await.unwrap().unwrap();
    assert_eq!(first, ClientToDaemon::Attach { pane });
    let second: ClientToDaemon = server.recv().await.unwrap().unwrap();
    assert_eq!(second, ClientToDaemon::Resize { pane, cols: 100, rows: 40 });

    drop(tx);
    pump_task.abort();
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p muxy-client`
Expected: FAIL — `resize_msg` undefined and `pump` takes 4 args, not 5.

- [ ] **Step 4: Add `resize_msg` and the resize arm to `pump`.** Add the pure helper near the top of `lib.rs`:

```rust
/// Build a Resize message for the pane (pure; unit-tested).
pub fn resize_msg(pane: PaneId, cols: u16, rows: u16) -> ClientToDaemon {
    ClientToDaemon::Resize { pane, cols, rows }
}
```

Change `pump`'s signature to accept a resize receiver and add the third `select!` arm:

```rust
pub async fn pump<S, R, W>(
    io: S,
    pane: PaneId,
    mut input: R,
    mut output: W,
    mut resizes: tokio::sync::mpsc::Receiver<(u16, u16)>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let mut msgs = MsgStream::new(io);
    msgs.send(&ClientToDaemon::Attach { pane }).await?;

    let mut buf = [0u8; 4096];
    loop {
        tokio::select! {
            n = input.read(&mut buf) => {
                match n {
                    Ok(0) => { let _ = msgs.send(&ClientToDaemon::Detach).await; break; }
                    Ok(n) => msgs.send(&ClientToDaemon::Input { pane, bytes: buf[..n].to_vec() }).await?,
                    Err(_) => break,
                }
            }
            Some((cols, rows)) = resizes.recv() => {
                msgs.send(&resize_msg(pane, cols, rows)).await?;
            }
            msg = msgs.recv::<DaemonToClient>() => {
                match msg? {
                    Some(DaemonToClient::Output { bytes, .. }) => {
                        output.write_all(&bytes).await?;
                        output.flush().await?;
                    }
                    Some(DaemonToClient::PaneExited { .. }) | None => break,
                    Some(DaemonToClient::Attached { .. }) => {}
                    Some(DaemonToClient::AttentionChanged { .. }) => {}
                    Some(DaemonToClient::AgentList { .. }) => {}
                    Some(DaemonToClient::AgentRemoved { .. }) => {}
                }
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Wire the real winsize + SIGWINCH source in `attach`.** Replace the `pump(...)` call in `attach` with a channel fed by the initial size and a SIGWINCH task:

```rust
pub async fn attach(pane_id: u64) -> Result<()> {
    let sock = std::env::var("MUXY_SOCK").unwrap_or_else(|_| "/tmp/muxy.sock".into());
    let pane = PaneId(pane_id);

    let stream = UnixStream::connect(&sock).await?;

    let _guard = RawModeGuard::enable()?;
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    // Resize source: send the current size immediately, then on each SIGWINCH.
    let (tx, rx) = tokio::sync::mpsc::channel::<(u16, u16)>(8);
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        let _ = tx.send((cols, rows)).await;
    }
    let winch_tx = tx.clone();
    tokio::spawn(async move {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        {
            while sig.recv().await.is_some() {
                if let Ok((cols, rows)) = crossterm::terminal::size() {
                    if winch_tx.send((cols, rows)).await.is_err() {
                        break; // pump gone
                    }
                }
            }
        }
    });

    pump(stream, pane, stdin, stdout, rx).await
    // _guard drops here, restoring raw mode; pump's Result is returned unmasked.
}
```

- [ ] **Step 6: Update the existing `pump` test call.** In `pump_forwards_input_renders_output_and_shuts_down_on_eof`, create an unused resize channel and pass its receiver:

```rust
let (_resize_tx, resize_rx) = tokio::sync::mpsc::channel::<(u16, u16)>(8);
let pump_task = tokio::spawn(async move {
    pump(client_io, pane, input_reader, out_writer, resize_rx).await
});
```

- [ ] **Step 7: Run to verify all pass**

Run: `cargo test -p muxy-client` then `cargo test`
Expected: PASS — the two new tests, the updated existing `pump` test, and the whole workspace (39 + new).

- [ ] **Step 8: Commit**

```bash
git add crates/muxy-client/src/lib.rs
git commit -m "feat(client): propagate terminal size to daemon on attach + SIGWINCH"
```

---

## Task 4: Mouse + IME in `SurfaceView` (MuxyApp)

Extend the existing `SurfaceView` (its initializer and key handling stay compatible) with mouse forwarding and `NSTextInputClient` composition. No unit tests exist for this layer; the gate is `swift build` + a described manual check. Do this **before** the app restructure so the change lands while the current app still compiles.

**Files:**
- Modify: `macos/Sources/MuxyApp/SurfaceView.swift`

**Interfaces:**
- Consumes: `ghostty_surface_mouse_button`, `ghostty_surface_mouse_pos`, `ghostty_surface_mouse_scroll`, `ghostty_surface_text`, `ghostty_surface_preedit`, `ghostty_surface_ime_point`, `ghostty_input_key_s.composing`, and the existing `ghosttyMods(_:)`.
- Produces: an updated `SurfaceView` usable unchanged by `SurfaceHost` (Task 5).

- [ ] **Step 1: Add mouse handling.** Add these overrides and helpers to `SurfaceView` (keep everything already there):

```swift
// MARK: - Mouse

private func mousePoint(_ event: NSEvent) -> (Double, Double) {
    let p = convert(event.locationInWindow, from: nil)
    // libghostty wants a top-left origin; AppKit's is bottom-left.
    return (Double(p.x), Double(bounds.height - p.y))
}

private func sendMousePos(_ event: NSEvent) {
    guard let surface else { return }
    let (x, y) = mousePoint(event)
    ghostty_surface_mouse_pos(surface, x, y, ghosttyMods(event.modifierFlags))
}

private func sendMouseButton(_ event: NSEvent,
                             _ state: ghostty_input_mouse_state_e,
                             _ button: ghostty_input_mouse_button_e) {
    guard let surface else { return }
    sendMousePos(event)
    _ = ghostty_surface_mouse_button(surface, state, button, ghosttyMods(event.modifierFlags))
}

override func mouseDown(with e: NSEvent)  { sendMouseButton(e, GHOSTTY_MOUSE_PRESS,   GHOSTTY_MOUSE_LEFT) }
override func mouseUp(with e: NSEvent)    { sendMouseButton(e, GHOSTTY_MOUSE_RELEASE, GHOSTTY_MOUSE_LEFT) }
override func mouseDragged(with e: NSEvent) { sendMousePos(e) }
override func rightMouseDown(with e: NSEvent)  { sendMouseButton(e, GHOSTTY_MOUSE_PRESS,   GHOSTTY_MOUSE_RIGHT) }
override func rightMouseUp(with e: NSEvent)    { sendMouseButton(e, GHOSTTY_MOUSE_RELEASE, GHOSTTY_MOUSE_RIGHT) }
override func rightMouseDragged(with e: NSEvent) { sendMousePos(e) }
override func otherMouseDown(with e: NSEvent)  { sendMouseButton(e, GHOSTTY_MOUSE_PRESS,   GHOSTTY_MOUSE_MIDDLE) }
override func otherMouseUp(with e: NSEvent)    { sendMouseButton(e, GHOSTTY_MOUSE_RELEASE, GHOSTTY_MOUSE_MIDDLE) }
override func otherMouseDragged(with e: NSEvent) { sendMousePos(e) }

override func scrollWheel(with e: NSEvent) {
    guard let surface else { return }
    ghostty_surface_mouse_scroll(surface, Double(e.scrollingDeltaX), Double(e.scrollingDeltaY), 0)
}
```

- [ ] **Step 2: Add IME via `NSTextInputClient` with a key-text accumulator.** This follows Ghostty's own AppKit surface: `keyDown` runs the event through the input context; committed text is accumulated and sent as text, marked text becomes preedit, and anything the IME does not consume (Enter, Ctrl-C, arrows, plain keys) falls back to the existing `ghostty_surface_key` path so nothing regresses.

Replace the existing `keyDown` and add the accumulator + extension:

```swift
// Accumulates text the IME commits during interpretKeyEvents/handleEvent.
private var keyTextAccumulator: [String]?
private var markedText = ""

override func keyDown(with event: NSEvent) {
    keyTextAccumulator = []
    let handledByIME = inputContext?.handleEvent(event) ?? false
    let commits = keyTextAccumulator ?? []
    keyTextAccumulator = nil

    // IME committed text -> send it as text and stop.
    if !commits.isEmpty {
        for t in commits { sendText(t) }
        return
    }
    // Still composing: setMarkedText already pushed preedit. Stop.
    if handledByIME && hasMarkedText() { return }
    // Not consumed by IME: encode normally (Enter, Ctrl-*, arrows, plain char).
    sendKey(event, GHOSTTY_ACTION_PRESS)
}

private func sendText(_ text: String) {
    guard let surface, !text.isEmpty else { return }
    text.withCString { ghostty_surface_text(surface, $0, UInt(strlen($0))) }
}

private func asString(_ any: Any) -> String? {
    if let s = any as? String { return s }
    if let a = any as? NSAttributedString { return a.string }
    return nil
}
```

Add the `NSTextInputClient` conformance:

```swift
extension SurfaceView: NSTextInputClient {
    func insertText(_ string: Any, replacementRange: NSRange) {
        guard let s = asString(string) else { return }
        if keyTextAccumulator != nil {
            keyTextAccumulator?.append(s)      // committed during keyDown
        } else {
            sendText(s)
        }
        markedText = ""
        if let surface { ghostty_surface_preedit(surface, nil, 0) }  // clear preedit
    }

    func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        markedText = asString(string) ?? ""
        guard let surface else { return }
        if markedText.isEmpty {
            ghostty_surface_preedit(surface, nil, 0)
        } else {
            markedText.withCString { ghostty_surface_preedit(surface, $0, UInt(strlen($0))) }
        }
    }

    func unmarkText() {
        markedText = ""
        if let surface { ghostty_surface_preedit(surface, nil, 0) }
    }

    func hasMarkedText() -> Bool { !markedText.isEmpty }

    func markedRange() -> NSRange {
        markedText.isEmpty ? NSRange(location: NSNotFound, length: 0)
                           : NSRange(location: 0, length: markedText.utf16.count)
    }

    func selectedRange() -> NSRange { NSRange(location: NSNotFound, length: 0) }

    func attributedSubstring(forProposedRange range: NSRange,
                             actualRange: NSRangePointer?) -> NSAttributedString? { nil }

    func validAttributesForMarkedText() -> [NSAttributedString.Key] { [] }

    func characterIndex(for point: NSPoint) -> Int { 0 }

    func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        guard let surface, let window else { return .zero }
        var x = 0.0, y = 0.0, w = 0.0, h = 0.0
        ghostty_surface_ime_point(surface, &x, &y, &w, &h)   // top-left origin, points
        let local = NSRect(x: x, y: bounds.height - y, width: max(w, 1), height: max(h, 1))
        let inWindow = convert(local, to: nil)
        return window.convertToScreen(inWindow)
    }

    func doCommandBySelector(_ selector: Selector) {
        // Intentionally empty: keyDown's fallback path encodes command keys
        // (Enter, Backspace, arrows) via ghostty_surface_key.
    }
}
```

- [ ] **Step 3: Build the app**

Run: `cd macos && swift build`
Expected: builds and links libghostty with no errors.

- [ ] **Step 4: Manual smoke (record in the report; the controller/user runs it).** With the *current* single-surface app (still present until Task 5), launch against a running daemon+pane and confirm: printable typing, Enter, and Ctrl-C behave exactly as before (no regression); clicking positions the cursor; scrolling moves the viewport. IME (CJK) composition can be confirmed later in the full app. If plain typing regresses, the fault is the `keyDown` routing — fix before proceeding.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/MuxyApp/SurfaceView.swift
git commit -m "feat(app): mouse + IME (NSTextInputClient) input in SurfaceView"
```

---

## Task 5: `@main` app + libghostty bootstrap + `SurfaceHost` (MuxyApp)

Replace the single-surface `main.swift` with a SwiftUI `App`. Relocate the libghostty init into an app delegate (preserving the single-init + wakeup-tick invariants), construct `AppModel` + `SurfaceHost`, and render a minimal placeholder `ContentView` (fleshed out in Task 6). Gate: `swift build`.

**Files:**
- Delete: `macos/Sources/MuxyApp/main.swift`
- Create: `macos/Sources/MuxyApp/App.swift` (the `@main` app + `AppDelegate`)
- Create: `macos/Sources/MuxyApp/SurfaceHost.swift`
- Create: `macos/Sources/MuxyApp/ContentView.swift` (placeholder here; expanded in Task 6)

**Interfaces:**
- Consumes: `AppModel` (Task 2), `SurfaceView` (Task 4), the libghostty C API.
- Produces: `SurfaceHost.view(for:) -> SurfaceView`; `AppModel` instance in the SwiftUI environment; global `gApp` for `wakeup_cb`.

- [ ] **Step 1: Create `SurfaceHost.swift`** — one retained `SurfaceView` per pane:

```swift
import AppKit
import GhosttyKit

/// Owns one SurfaceView per pane so switching agents never restarts `muxy attach`.
@MainActor
final class SurfaceHost {
    private let app: ghostty_app_t
    private let muxyBinary: String
    private let socketPath: String
    private var views: [UInt64: SurfaceView] = [:]

    init(app: ghostty_app_t, muxyBinary: String, socketPath: String) {
        self.app = app
        self.muxyBinary = muxyBinary
        self.socketPath = socketPath
    }

    func view(for pane: UInt64) -> SurfaceView {
        if let v = views[pane] { return v }
        let v = SurfaceView(app: app, paneId: pane, muxyBinary: muxyBinary, socketPath: socketPath)
        views[pane] = v
        return v
    }
}
```

- [ ] **Step 2: Create `App.swift`** — the delegate runs the exact init sequence from the old `main.swift`, then builds the model/host and connects:

```swift
import AppKit
import SwiftUI
import GhosttyKit
import MuxyCore

// Read by the C wakeup callback (which can't capture Swift context).
var gApp: ghostty_app_t?

final class AppDelegate: NSObject, NSApplicationDelegate {
    var appModel: AppModel!
    var surfaceHost: SurfaceHost!

    func applicationDidFinishLaunching(_ notification: Notification) {
        let muxyBinary = ProcessInfo.processInfo.environment["MUXY_BIN"]
            ?? FileManager.default.currentDirectoryPath + "/../target/debug/muxy"
        let socketPath = ProcessInfo.processInfo.environment["MUXY_SOCK"] ?? "/tmp/muxy.sock"
        let controlPath = ProcessInfo.processInfo.environment["MUXY_CONTROL_SOCK"]
            ?? "/tmp/muxy-control.sock"

        // --- libghostty init (unchanged sequence, relocated from main.swift) ---
        guard ghostty_init(UInt(CommandLine.argc), CommandLine.unsafeArgv) == GHOSTTY_SUCCESS else {
            fatalError("muxy: ghostty_init failed")
        }
        let config = ghostty_config_new()
        ghostty_config_finalize(config)

        var runtime = ghostty_runtime_config_s()
        runtime.userdata = nil
        runtime.supports_selection_clipboard = false
        runtime.wakeup_cb = { _ in
            DispatchQueue.main.async { if let a = gApp { ghostty_app_tick(a) } }
        }
        runtime.action_cb = { _, _, _ in false }
        runtime.read_clipboard_cb = { _, _, _ in false }
        runtime.confirm_read_clipboard_cb = { _, _, _, _ in }
        runtime.write_clipboard_cb = { _, _, _, _, _ in }
        runtime.close_surface_cb = { _, _ in }

        guard let app = ghostty_app_new(&runtime, config) else {
            fatalError("muxy: ghostty_app_new failed")
        }
        gApp = app
        ghostty_app_set_focus(app, true)

        // --- model + surface registry ---
        surfaceHost = SurfaceHost(app: app, muxyBinary: muxyBinary, socketPath: socketPath)
        appModel = AppModel(makeTransport: { try UnixSocketConnection(path: controlPath) })
        appModel.connect()
    }

    func applicationWillTerminate(_ notification: Notification) {
        appModel?.shutdown()   // F1: explicit disconnect
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

@main
struct MuxyApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        WindowGroup {
            ContentView(surfaceHost: delegate.surfaceHost)
                .environmentObject(delegate.appModel)
                .frame(minWidth: 900, minHeight: 560)
        }
    }
}
```

> `delegate.surfaceHost`/`appModel` are created in `applicationDidFinishLaunching`, which runs before the scene body is evaluated. If the SwiftUI lifecycle evaluates `body` before the delegate finishes (observed on some macOS versions), guard by making `ContentView` tolerate a nil host until `appModel` publishes — but first try this straightforward form; the delegate adaptor initializes the delegate eagerly.

- [ ] **Step 3: Create a placeholder `ContentView.swift`** (Task 6 expands it):

```swift
import SwiftUI
import MuxyCore

struct ContentView: View {
    @EnvironmentObject var model: AppModel
    let surfaceHost: SurfaceHost

    var body: some View {
        VStack {
            Text("muxy").font(.largeTitle)
            switch model.connectionState {
            case .connecting: Text("Connecting…")
            case .live: Text("\(model.store.agents.count) agent(s)")
            case .closed(let reason): Text(reason).foregroundStyle(.red)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
```

- [ ] **Step 4: Build**

Run: `cd macos && swift build`
Expected: builds; `main.swift` is gone and the app has a single `@main`.

- [ ] **Step 5: Manual smoke (recorded).** With a daemon running, launch the app: a window opens and shows the agent count (or "Connecting…"/disconnected). No terminal yet — that's Task 6.

- [ ] **Step 6: Commit**

```bash
git rm macos/Sources/MuxyApp/main.swift
git add macos/Sources/MuxyApp/App.swift macos/Sources/MuxyApp/SurfaceHost.swift macos/Sources/MuxyApp/ContentView.swift
git commit -m "feat(app): @main SwiftUI app, libghostty bootstrap in AppDelegate, SurfaceHost"
```

---

## Task 6: Sidebar + terminal pane (`ContentView`, `TerminalContainer`)

Replace the placeholder with the real `NavigationSplitView`: sidebar of agents grouped by project with attention badges, the selected agent's terminal, and the `lastError`/disconnected surfaces. Gate: `swift build`.

**Files:**
- Modify: `macos/Sources/MuxyApp/ContentView.swift`
- Create: `macos/Sources/MuxyApp/TerminalContainer.swift`
- Create: `macos/Sources/MuxyApp/SpawnSheet.swift` (minimal here; polished in Task 7)

**Interfaces:**
- Consumes: `AppModel` (`store`, `selectedPane`, `connectionState`), `AgentStore.byProject`, `AttentionState`, `SurfaceHost`, `SurfaceView`.
- Produces: `TerminalContainer` (`NSViewRepresentable`), `SpawnSheet(onSpawn:)`.

- [ ] **Step 1: Create `TerminalContainer.swift`:**

```swift
import SwiftUI
import AppKit

/// Bridges a retained per-pane SurfaceView into SwiftUI. Keyed by pane at the call
/// site with `.id(pane)`, so selecting a different agent makes a different view.
struct TerminalContainer: NSViewRepresentable {
    let pane: UInt64
    let surfaceHost: SurfaceHost

    func makeNSView(context: Context) -> SurfaceView {
        let view = surfaceHost.view(for: pane)
        DispatchQueue.main.async { view.window?.makeFirstResponder(view) }
        return view
    }

    func updateNSView(_ nsView: SurfaceView, context: Context) {
        DispatchQueue.main.async { nsView.window?.makeFirstResponder(nsView) }
    }
}
```

- [ ] **Step 2: Implement the real `ContentView.swift`:**

```swift
import SwiftUI
import MuxyCore

struct ContentView: View {
    @EnvironmentObject var model: AppModel
    let surfaceHost: SurfaceHost
    @State private var showingSpawn = false

    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 220, ideal: 260)
        } detail: {
            detail
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { showingSpawn = true } label: { Image(systemName: "plus") }
                    .disabled(model.connectionState != .live)
                    .help("Spawn a new agent")
            }
        }
        .sheet(isPresented: $showingSpawn) {
            SpawnSheet { project, task, adapter in
                model.spawn(project: project, task: task, adapter: adapter)
            }
        }
        .safeAreaInset(edge: .bottom) { statusBar }
    }

    private var sidebar: some View {
        List(selection: $model.selectedPane) {
            ForEach(model.store.byProject, id: \.project) { group in
                Section(header: Text(projectLabel(group.project))) {
                    ForEach(group.agents) { agent in
                        HStack(spacing: 8) {
                            Circle()
                                .fill(color(for: agent.state))
                                .frame(width: 8, height: 8)
                            Text(agent.task).lineLimit(1)
                            Spacer()
                        }
                        .tag(agent.pane)
                    }
                }
            }
        }
        .overlay {
            if model.store.agents.isEmpty && model.connectionState == .live {
                Text("No agents yet — spawn one with +").foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private var detail: some View {
        if let pane = model.selectedPane, model.store.agents[pane] != nil {
            TerminalContainer(pane: pane, surfaceHost: surfaceHost)
                .id(pane)
        } else {
            Text("Select an agent").foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    @ViewBuilder private var statusBar: some View {
        VStack(spacing: 0) {
            if case let .closed(reason) = model.connectionState {
                banner(reason, color: .red)
            } else if let err = model.store.lastError {
                banner(err, color: .orange)
            }
        }
    }

    private func banner(_ text: String, color: Color) -> some View {
        HStack {
            Image(systemName: "exclamationmark.triangle.fill")
            Text(text).lineLimit(2)
            Spacer()
        }
        .font(.callout)
        .padding(8)
        .frame(maxWidth: .infinity)
        .background(color.opacity(0.15))
        .foregroundStyle(color)
    }

    private func projectLabel(_ path: String) -> String {
        (path as NSString).lastPathComponent.isEmpty ? path : (path as NSString).lastPathComponent
    }

    private func color(for state: AttentionState) -> Color {
        switch state {
        case .needsInput: return .red        // the whole point — must be loud
        case .working:    return .green
        case .completed:  return .blue
        case .exited:     return .gray
        case .idle:       return .secondary
        }
    }
}
```

> `SpawnSheet` is referenced here but created in Task 7. To keep this task's build green on its own, add a minimal stub `SpawnSheet` in this task (a placeholder view that calls the closure with empty fields is enough to compile), and let Task 7 replace it — OR fold Tasks 6 and 7 if executing strictly in order. Prefer: include a minimal real `SpawnSheet` file in this task so `swift build` passes, then Task 7 only enriches it. (See Task 7 for the full version.)

- [ ] **Step 3: Add a minimal `SpawnSheet.swift` so this task builds** (Task 7 replaces its body with validation/polish):

```swift
import SwiftUI

struct SpawnSheet: View {
    let onSpawn: (String, String, String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var project = ""
    @State private var task = ""
    @State private var adapter = "claude"

    var body: some View {
        Form {
            TextField("Project path", text: $project)
            TextField("Task", text: $task)
            TextField("Adapter", text: $adapter)
            HStack {
                Button("Cancel") { dismiss() }
                Spacer()
                Button("Spawn") {
                    onSpawn(project, task, adapter.isEmpty ? "claude" : adapter)
                    dismiss()
                }
            }
        }
        .padding()
        .frame(width: 420)
    }
}
```

- [ ] **Step 4: Build**

Run: `cd macos && swift build`
Expected: builds.

- [ ] **Step 5: Manual smoke (recorded).** With a daemon + an existing agent: the sidebar lists it under its project with a badge; selecting it renders its terminal; typing/clicking/resizing work; a disconnected daemon shows the red banner.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/MuxyApp/ContentView.swift macos/Sources/MuxyApp/TerminalContainer.swift macos/Sources/MuxyApp/SpawnSheet.swift
git commit -m "feat(app): NavigationSplitView sidebar + terminal pane + status banners"
```

---

## Task 7: Spawn sheet polish (`SpawnSheet`)

Finish the spawn UX: validation (non-empty project path and task; adapter defaults to `claude`), disabled Spawn until valid, and clear labels. The daemon's `agentSpawned` → `needsRefresh` → auto `listAgents` populates the new row (no client optimism). Gate: `swift build`.

**Files:**
- Modify: `macos/Sources/MuxyApp/SpawnSheet.swift`

**Interfaces:**
- Consumes: the `onSpawn(project:task:adapter:)` closure from `ContentView`.

- [ ] **Step 1: Replace `SpawnSheet` body with the validated version:**

```swift
import SwiftUI

struct SpawnSheet: View {
    let onSpawn: (String, String, String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var project = ""
    @State private var task = ""
    @State private var adapter = "claude"

    private var isValid: Bool {
        !project.trimmingCharacters(in: .whitespaces).isEmpty &&
        !task.trimmingCharacters(in: .whitespaces).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Spawn Agent").font(.headline)
            Form {
                TextField("Project path", text: $project)
                    .textFieldStyle(.roundedBorder)
                TextField("Task", text: $task)
                    .textFieldStyle(.roundedBorder)
                TextField("Adapter", text: $adapter)
                    .textFieldStyle(.roundedBorder)
            }
            HStack {
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Spawn") {
                    let a = adapter.trimmingCharacters(in: .whitespaces)
                    onSpawn(project.trimmingCharacters(in: .whitespaces),
                            task.trimmingCharacters(in: .whitespaces),
                            a.isEmpty ? "claude" : a)
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!isValid)
            }
        }
        .padding(20)
        .frame(width: 440)
    }
}
```

- [ ] **Step 2: Build**

Run: `cd macos && swift build`
Expected: builds.

- [ ] **Step 3: Full manual verification (recorded; the user runs the end-to-end pass).** Start the daemon; launch the app; click "+", enter a scratch git repo path + task, adapter `claude` → Spawn; confirm a worktree/branch is created and a new sidebar row appears under that project; select it, type/click/compose/resize; trigger a Claude `Notification`/`Stop` and confirm the badge flips to the `.needsInput` (red) color; close and reopen the window (agent survives, terminal restored); kill the daemon (red disconnected banner) and quit cleanly (no hang — F1).

- [ ] **Step 4: Commit**

```bash
git add macos/Sources/MuxyApp/SpawnSheet.swift
git commit -m "feat(app): spawn sheet validation and polish"
```

---

## Final verification

- `cd macos && swift test` → the b1 core suite (25) + Task 1 close tests + Task 2 `AppModelTests`, all green.
- `cd macos && swift build` → the full app builds and links libghostty.
- `cargo test` → whole workspace green including Task 3's resize tests.
- Manual (user): the Task 7 Step 3 end-to-end pass — spawn from GUI, drive the terminal (keys/mouse/IME), resize reflows the agent, badge flips on attention, agent survives window close, daemon death shows the disconnected banner, quit stays clean.
