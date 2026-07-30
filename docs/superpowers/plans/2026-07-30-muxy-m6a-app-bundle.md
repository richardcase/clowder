# muxy M6a — Bundle + Self-Contained Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn muxy into a double-click-runnable `Muxy.app`: a committed script assembles the bundle
(SwiftPM app + the 3 release Rust binaries + Info.plist + placeholder icon), the app resolves its
binaries bundle-relative and **launches + supervises its own daemon**, sockets are per-user, and the
bare-exe activation workaround is retired.

**Architecture:** Four slices. (1) `muxy-config` socket **defaults** move from `/tmp` to a per-user
runtime dir. (2) A testable `MuxyCore/DaemonSupervisor` owns the relaunch policy (injected spawn +
sleep seams, mirroring M5d). (3) `MuxyApp` supplies the real `Process`-backed daemon spawn, computes
per-user sockets, resolves the bundled `muxy` binary, and wires supervision into `bootstrap()`.
(4) `scripts/build-app.sh` assembles `Muxy.app` (Info.plist from a `VERSION` file, a placeholder
`.icns` rendered by a small Swift generator).

**Tech Stack:** Rust (`muxy-config`, `muxy-daemon`), Swift 6 / AppKit (`MuxyCore`, `MuxyApp`), bash +
`sips`/`iconutil`. Spec: `docs/superpowers/specs/2026-07-30-muxy-m6-packaging-design.md` (§M6a).

## Global Constraints

- **Scope: M6a only.** No signing/notarization, no CI, no Homebrew, no libghostty build script (those
  are M6b–f). Files touched: `crates/muxy-config/src/lib.rs`, `crates/muxy-daemon/src/main.rs`,
  `macos/Sources/MuxyCore/DaemonSupervisor.swift` (new) + tests, `macos/Sources/MuxyApp/App.swift`
  (+ a new `DaemonLaunch.swift`), and new repo-root files (`VERSION`, `scripts/build-app.sh`,
  `scripts/gen-icon.swift`).
- **Per-user socket default:** `<runtime_dir>/muxy/{muxy.sock,muxy-control.sock,muxy-hook.sock}` where
  `runtime_dir = $XDG_RUNTIME_DIR › $TMPDIR › /tmp` — identical to M5b's `InstanceLock::default_path`.
  **Env still wins** (`MUXY_SOCK`/`MUXY_CONTROL_SOCK`/`MUXY_HOOK_SOCK`), so dev/CI flows are unchanged.
- **Supervisor policy:** relaunch on unexpected exit with **bounded exponential backoff**
  (`min(10, 0.5·2^attempt)` → 0.5,1,2,4,8,10…), backoff-*first* so it never hot-loops. The daemon exits
  with a **distinct code 3** when it loses M5b's single-instance `flock` → the supervisor **yields**
  (does not relaunch; the app connects to the existing daemon via M5d). Any OTHER non-zero exit (a
  crash, or an `anyhow`-`Err` from `main` such as a bind/accept failure — all of which are code ≠ 3)
  is treated as a crash → relaunch. (Do NOT match on code 1: `fn main() -> Result<()>` returning `Err`
  also exits 1, so 1 is NOT flock-specific.) `stop()` cancels and terminates. Same `@MainActor` +
  injected-`sleep` pattern as M5d's `AppModel`.
- **Bundle identity:** `com.github.richardcase.muxy`, name `Muxy`, version from the top-level `VERSION`
  file (`0.1.0`), min macOS 14, regular dock app (**not** `LSUIElement` — M1d's tray relies on it).
- **Dev flows must keep working:** `swift run muxy-app` (unbundled) still resolves `muxy` via
  `$MUXY_BIN`/`../target/debug` and still calls `setActivationPolicy` — guarded by "am I unbundled?"
  (`Bundle.main.bundleIdentifier == nil`). Env-set sockets still honored.
- Rust: `anyhow`/`cargo test`. Swift: `@MainActor`, `cd macos && swift test` (MuxyCore) /
  `swift build` (MuxyApp, links vendored libghostty — present locally). Prefix cargo with
  `source "$HOME/.cargo/env" && `.

---

## Task 1: Per-user socket defaults (`muxy-config` + daemon dir creation)

**Files:**
- Modify: `crates/muxy-config/src/lib.rs` (default socket dir → per-user; update/extend tests)
- Modify: `crates/muxy-daemon/src/main.rs` (create each socket's parent dir before binding)

**Interfaces:**
- Consumes: nothing new.
- Produces: `Config` socket defaults now resolve to `<runtime_dir>/muxy/<name>` (env still wins). No
  signature change (`Config::load`/`resolve` unchanged shapes).

- [ ] **Step 1: Update the failing tests.** In `crates/muxy-config/src/lib.rs`, REPLACE the
`defaults_when_empty` test's socket assertion and ADD two tests. Change the existing test:

```rust
    #[test]
    fn defaults_when_empty() {
        let c = Config::resolve(FileConfig::default(), &no_env);
        // Per-user default: no XDG_RUNTIME_DIR/TMPDIR in `no_env` → runtime_dir is /tmp.
        assert_eq!(c.client_sock, PathBuf::from("/tmp/muxy/muxy.sock"));
        assert_eq!(c.backlog_cap, 262144);
        assert_eq!(c.shell, "/bin/sh");
        assert_eq!((c.default_cols, c.default_rows), (80, 24));
    }
```

Add:

```rust
    #[test]
    fn default_socket_dir_honors_xdg_runtime_dir_then_tmpdir() {
        let xdg = |k: &str| if k == "XDG_RUNTIME_DIR" { Some("/run/user/501".into()) } else { None };
        let c = Config::resolve(FileConfig::default(), &xdg);
        assert_eq!(c.client_sock, PathBuf::from("/run/user/501/muxy/muxy.sock"));
        assert_eq!(c.control_sock, PathBuf::from("/run/user/501/muxy/muxy-control.sock"));
        assert_eq!(c.hook_sock, PathBuf::from("/run/user/501/muxy/muxy-hook.sock"));

        let tmp = |k: &str| if k == "TMPDIR" { Some("/var/folders/xy".into()) } else { None };
        let c2 = Config::resolve(FileConfig::default(), &tmp);
        assert_eq!(c2.client_sock, PathBuf::from("/var/folders/xy/muxy/muxy.sock"));
    }

    #[test]
    fn env_socket_overrides_per_user_default() {
        let env = |k: &str| match k {
            "XDG_RUNTIME_DIR" => Some("/run/user/501".into()),
            "MUXY_SOCK" => Some("/env/explicit.sock".into()),
            _ => None,
        };
        let c = Config::resolve(FileConfig::default(), &env);
        assert_eq!(c.client_sock, PathBuf::from("/env/explicit.sock")); // env wins over the per-user default
        assert_eq!(c.control_sock, PathBuf::from("/run/user/501/muxy/muxy-control.sock")); // others still per-user
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-config`
Expected: FAIL — `defaults_when_empty` (still `/tmp/muxy.sock`) and the two new tests (defaults still
`/tmp/*`), because the per-user default isn't implemented.

- [ ] **Step 3: Implement per-user socket defaults.** In `crates/muxy-config/src/lib.rs`:

Remove the three socket string consts (keep the non-socket ones):

```rust
const DEFAULT_CLIENT_SOCK: &str = "/tmp/muxy.sock";
const DEFAULT_CONTROL_SOCK: &str = "/tmp/muxy-control.sock";
const DEFAULT_HOOK_SOCK: &str = "/tmp/muxy-hook.sock";
```
→ (deleted).

Then rewrite `resolve` to compute a per-user socket dir from the env-getter and use `PathBuf` defaults:

```rust
    /// Pure resolver (testable): env > file > default. `get_env(key)` yields the env value.
    fn resolve(f: FileConfig, get_env: &dyn Fn(&str) -> Option<String>) -> Config {
        let s = f.sockets.unwrap_or_default();
        let p = f.pane.unwrap_or_default();

        // Per-user runtime dir for sockets: $XDG_RUNTIME_DIR › $TMPDIR › /tmp (mirrors the daemon's
        // single-instance PID lock dir). Env socket vars still override below.
        let nonempty = |k: &str| get_env(k).filter(|v| !v.is_empty());
        let runtime_dir = nonempty("XDG_RUNTIME_DIR")
            .or_else(|| nonempty("TMPDIR"))
            .unwrap_or_else(|| "/tmp".to_string());
        let default_sock = |name: &str| PathBuf::from(&runtime_dir).join("muxy").join(name);

        let path = |env: &str, file: Option<PathBuf>, def: PathBuf| {
            get_env(env).map(PathBuf::from).or(file).unwrap_or(def)
        };
        Config {
            client_sock: path("MUXY_SOCK", s.client, default_sock("muxy.sock")),
            control_sock: path("MUXY_CONTROL_SOCK", s.control, default_sock("muxy-control.sock")),
            hook_sock: path("MUXY_HOOK_SOCK", s.hook, default_sock("muxy-hook.sock")),
            backlog_cap: get_env("MUXY_BACKLOG_CAP").and_then(|v| v.parse().ok())
                .or(p.backlog_cap).unwrap_or(DEFAULT_BACKLOG_CAP),
            shell: get_env("SHELL").or(p.shell).unwrap_or_else(|| "/bin/sh".into()),
            default_cols: p.cols.unwrap_or(DEFAULT_COLS),
            default_rows: p.rows.unwrap_or(DEFAULT_ROWS),
        }
    }
```

- [ ] **Step 4: Run the config tests to verify they pass**

Run: `source "$HOME/.cargo/env" && cargo test -p muxy-config`
Expected: PASS — all four socket tests (`defaults_when_empty`, the two new, `env_overrides_file`,
`env_overrides_default`-style) green. (`env_overrides_file` uses `MUXY_SOCK` so still passes.)

- [ ] **Step 5: Ensure the daemon creates each socket's parent dir before binding.** In
`crates/muxy-daemon/src/main.rs`, the sockets now live under `<runtime_dir>/muxy/` which may not exist.
The single-instance lock already `create_dir_all`s that dir, but make socket binding robust for any
(env-overridden) path: right after resolving `sock_path`/`control_path`/`hook_path` and BEFORE
`remove_files(...)`/binding, add:

```rust
    // Sockets may live in a per-user dir that doesn't exist yet; create each parent.
    for p in [&sock_path, &hook_path, &control_path] {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
```

(Place it after `let hook_path = daemon.hook_sock().to_path_buf();` and before the `InstanceLock`
acquire / `remove_files` block — order relative to the lock doesn't matter since both target the same
default dir; this just guarantees the socket parent exists.)

- [ ] **Step 6: Build + run the whole workspace suite (no regressions)**

Run: `source "$HOME/.cargo/env" && cargo build -p muxy-daemon && cargo test 2>&1 | grep -E 'test result|error' | tail -30`
Expected: builds clean; all suites `0 failed`. Note: daemon tests pass explicit socket paths to
`Daemon::new_with` (not `Config::load`), so the default change doesn't affect them.

- [ ] **Step 7: Commit**

```bash
git add crates/muxy-config/src/lib.rs crates/muxy-daemon/src/main.rs
git commit -m "feat(config): per-user socket defaults (<runtime_dir>/muxy); daemon creates socket dir"
```

---

## Task 2: `DaemonSupervisor` (MuxyCore)

**Files:**
- Create: `macos/Sources/MuxyCore/DaemonSupervisor.swift`
- Create: `macos/Tests/MuxyCoreTests/DaemonSupervisorTests.swift`

**Interfaces:**
- Consumes: the `SleepController` + `eventually` async test helpers already defined at module scope in
  `AppModelTests.swift` (reuse — do NOT redeclare them).
- Produces:
  - `protocol DaemonProcess: AnyObject { func terminate(); func setOnExit(_ handler: @escaping (Int32) -> Void) }`
  - `@MainActor final class DaemonSupervisor` with `State { stopped, running, relaunching, yielded }`,
    `init(spawn: @escaping () -> DaemonProcess, sleep: (TimeInterval) async -> Void = <Task.sleep>)`,
    `start()`, `stop()`, and `@Published private(set) var state`.

- [ ] **Step 1: Write the failing tests.** Create
`macos/Tests/MuxyCoreTests/DaemonSupervisorTests.swift`:

```swift
import XCTest
@testable import MuxyCore

/// A fake daemon process the test drives: records terminate(), and fires its exit handler on demand.
@MainActor
final class FakeDaemonProcess: DaemonProcess {
    private(set) var terminated = false
    private var onExit: ((Int32) -> Void)?
    func terminate() { terminated = true }
    func setOnExit(_ handler: @escaping (Int32) -> Void) { onExit = handler }
    /// Test helper: simulate the process exiting with `code`.
    func exit(_ code: Int32) { onExit?(code) }
}

@MainActor
final class DaemonSupervisorTests: XCTestCase {
    func testStartSpawnsAndRuns() {
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        XCTAssertEqual(spawned.count, 1)
        XCTAssertEqual(sup.state, .running)
        sup.stop()
    }

    func testCrashRelaunchesWithBackoff() async {
        let controller = SleepController()
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(
            spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p },
            sleep: { await controller.sleep($0) }
        )
        sup.start()
        XCTAssertEqual(sup.state, .running)

        spawned[0].exit(139)                          // crash (SIGSEGV-style), not exit 1
        XCTAssertEqual(sup.state, .relaunching)

        let parked = await eventually { controller.parkedCount == 1 }
        XCTAssertTrue(parked)
        controller.advance()                          // wake → relaunch
        let live = await eventually { sup.state == .running }
        XCTAssertTrue(live)
        XCTAssertEqual(spawned.count, 2)              // a fresh process was spawned
        sup.stop()
    }

    func testBackoffIsBoundedAndNonDecreasing() async {
        let controller = SleepController()
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(
            spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p },
            sleep: { await controller.sleep($0) }
        )
        sup.start()
        spawned[0].exit(2)                            // schedule backoff #1 (0.5)
        // 7 backoffs total: crash after each of the first 6 relaunches; let the 7th survive (so no
        // trailing parked sleep is left dangling).
        for i in 0..<7 {
            let parked = await eventually { controller.parkedCount == 1 }
            XCTAssertTrue(parked)
            controller.advance()                      // consume the backoff → relaunch
            let running = await eventually { sup.state == .running }
            XCTAssertTrue(running)
            if i < 6 { spawned.last?.exit(2) }        // crash again → schedule the next backoff
        }
        // 7 recorded backoffs, bounded at 10, non-decreasing.
        XCTAssertEqual(controller.delays, [0.5, 1, 2, 4, 8, 10, 10])
        sup.stop()
    }

    func testStopTerminatesAndDoesNotRelaunch() {
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        let first = spawned[0]
        sup.stop()
        XCTAssertTrue(first.terminated)
        XCTAssertEqual(sup.state, .stopped)
        first.exit(139)                               // a late exit callback must not relaunch
        XCTAssertEqual(spawned.count, 1)
        XCTAssertEqual(sup.state, .stopped)
    }

    func testExitCode3YieldsWithoutRelaunch() async {
        let controller = SleepController()
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(
            spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p },
            sleep: { await controller.sleep($0) }
        )
        sup.start()
        spawned[0].exit(3)                            // distinct single-instance-loser code (lost flock)
        XCTAssertEqual(sup.state, .yielded)
        // No backoff scheduled, no relaunch — the app connects to the existing daemon via M5d.
        for _ in 0..<20 { await Task.yield() }
        XCTAssertEqual(controller.parkedCount, 0)
        XCTAssertEqual(spawned.count, 1)
        sup.stop()
    }

    func testGenericErrorExit1Relaunches() async {
        let controller = SleepController()
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(
            spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p },
            sleep: { await controller.sleep($0) }
        )
        sup.start()
        spawned[0].exit(1)                            // generic main() Err (e.g. bind failure) → relaunch, NOT yield
        XCTAssertEqual(sup.state, .relaunching)
        let parked = await eventually { controller.parkedCount == 1 }
        XCTAssertTrue(parked)
        controller.advance()
        let live = await eventually { sup.state == .running }
        XCTAssertTrue(live)
        XCTAssertEqual(spawned.count, 2)
        sup.stop()
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd /Users/richard/code/muxy/macos && swift test --filter DaemonSupervisorTests 2>&1 | tail -20`
Expected: FAIL to compile — `DaemonSupervisor`/`DaemonProcess` don't exist.

- [ ] **Step 3: Implement `DaemonSupervisor`.** Create
`macos/Sources/MuxyCore/DaemonSupervisor.swift`:

```swift
import Foundation

/// A running daemon process the supervisor controls. The real implementation (MuxyApp) wraps a
/// Foundation.Process; tests use a fake.
public protocol DaemonProcess: AnyObject {
    /// Ask the process to terminate (SIGTERM).
    func terminate()
    /// Register a handler invoked once, on the main actor, when the process exits (with its code).
    func setOnExit(_ handler: @escaping (Int32) -> Void)
}

/// Launches and supervises the muxy-daemon child process: relaunches it (bounded backoff) if it exits
/// unexpectedly, yields if it lost the single-instance lock (exit 1), and stops cleanly on quit.
/// Libghostty-free and unit-testable via injected spawn + sleep seams (mirrors AppModel's reconnect).
@MainActor
public final class DaemonSupervisor {
    public enum State: Equatable { case stopped, running, relaunching, yielded }
    @Published public private(set) var state: State = .stopped

    private let spawn: () -> DaemonProcess
    private let sleepFn: (TimeInterval) async -> Void
    private var process: DaemonProcess?
    private var relaunchTask: Task<Void, Never>?
    private var relaunchAttempt = 0     // persists across crashes so backoff escalates (crashes arrive
                                        // as separate async callbacks, not one continuous loop)
    private var isStopping = false

    public init(spawn: @escaping () -> DaemonProcess,
                sleep: @escaping (TimeInterval) async -> Void = { d in
                    try? await Task.sleep(nanoseconds: UInt64(max(0, d) * 1_000_000_000))
                }) {
        self.spawn = spawn
        self.sleepFn = sleep
    }

    /// Spawn the daemon and supervise it. Idempotent while already running/relaunching.
    public func start() {
        guard process == nil, relaunchTask == nil else { return }
        isStopping = false
        relaunchAttempt = 0
        launch()
    }

    private func launch() {
        let p = spawn()
        process = p
        state = .running
        p.setOnExit { [weak self] code in self?.handleExit(code) }
    }

    private func handleExit(_ code: Int32) {
        process = nil
        guard !isStopping else { return }
        if code == 3 {
            // Daemon's DISTINCT single-instance-loser code (lost M5b's flock): another daemon owns
            // it. Don't relaunch — the app connects to the existing daemon via M5d. NOT code 1:
            // `main() -> Result<()>` returning Err (e.g. a bind failure) also exits 1 and must relaunch.
            state = .yielded
            return
        }
        scheduleRelaunch()
    }

    private func backoffDelay(_ attempt: Int) -> TimeInterval { min(10.0, 0.5 * pow(2.0, Double(attempt))) }

    /// Schedule one delayed relaunch. `relaunchAttempt` is an INSTANCE counter (not loop-local): each
    /// crash arrives as its own async `onExit` callback, so the counter must persist across callbacks
    /// for the backoff to escalate. Reset only in `start()`.
    private func scheduleRelaunch() {
        guard relaunchTask == nil, !isStopping else { return }
        state = .relaunching
        let delay = backoffDelay(relaunchAttempt)
        relaunchAttempt += 1
        relaunchTask = Task { [weak self] in
            guard let self else { return }
            await self.sleepFn(delay)
            self.relaunchTask = nil
            guard !Task.isCancelled, !self.isStopping else { return }
            self.launch()            // spawn again; sets .running
        }
    }

    /// Explicit teardown (app quit): cancel relaunches and terminate the child.
    public func stop() {
        isStopping = true
        relaunchTask?.cancel()
        relaunchTask = nil
        process?.terminate()
        process = nil
        state = .stopped
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd /Users/richard/code/muxy/macos && swift test --filter DaemonSupervisorTests 2>&1 | tail -20`
Expected: PASS — all 5 tests.

- [ ] **Step 5: Run the whole MuxyCore suite (no regressions)**

Run: `cd /Users/richard/code/muxy/macos && swift test 2>&1 | tail -6`
Expected: all MuxyCore tests PASS (the reused `SleepController`/`eventually` still serve `AppModelTests`).

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/MuxyCore/DaemonSupervisor.swift macos/Tests/MuxyCoreTests/DaemonSupervisorTests.swift
git commit -m "feat(client): DaemonSupervisor — launch/relaunch the daemon with bounded backoff"
```

---

## Task 3: MuxyApp real wiring (spawn/supervise daemon + bundle-relative paths)

**Files:**
- Create: `macos/Sources/MuxyApp/DaemonLaunch.swift` (real `DaemonProcess` + path/socket helpers)
- Modify: `macos/Sources/MuxyApp/App.swift` (`bootstrap()` wires the supervisor + per-user sockets +
  bundled `muxy`; guard the activation hack)

**Interfaces:**
- Consumes: `DaemonSupervisor`/`DaemonProcess` (Task 2); per-user socket layout (Task 1).
- Produces: `ProcessDaemon` (real `DaemonProcess` over `Foundation.Process`); `MuxyPaths` helpers
  (`bundledBin(_:)`, `runtimeDir()`, `socketPaths()`); a supervisor started in `bootstrap()`.

- [ ] **Step 1: Create the real process + path helpers.** Create
`macos/Sources/MuxyApp/DaemonLaunch.swift`:

```swift
import Foundation
import MuxyCore

/// Where muxy's per-user sockets and bundled binaries live.
enum MuxyPaths {
    /// $XDG_RUNTIME_DIR › $TMPDIR › /tmp, then `/muxy` (matches muxy-config + the daemon PID lock).
    static func runtimeDir() -> String {
        let env = ProcessInfo.processInfo.environment
        let base = env["XDG_RUNTIME_DIR"].flatMap { $0.isEmpty ? nil : $0 }
            ?? env["TMPDIR"].flatMap { $0.isEmpty ? nil : $0 }
            ?? "/tmp"
        return (base as NSString).appendingPathComponent("muxy")
    }

    /// Per-user socket paths, honoring the same env overrides as muxy-config.
    static func socketPaths() -> (client: String, control: String, hook: String) {
        let env = ProcessInfo.processInfo.environment
        let dir = runtimeDir()
        func p(_ envKey: String, _ name: String) -> String {
            env[envKey].flatMap { $0.isEmpty ? nil : $0 } ?? (dir as NSString).appendingPathComponent(name)
        }
        return (p("MUXY_SOCK", "muxy.sock"),
                p("MUXY_CONTROL_SOCK", "muxy-control.sock"),
                p("MUXY_HOOK_SOCK", "muxy-hook.sock"))
    }

    /// A binary bundled at Contents/Resources/bin/<name>, or nil when running unbundled (swift run).
    static func bundledBin(_ name: String) -> String? {
        guard let res = Bundle.main.resourcePath else { return nil }
        let path = (res as NSString).appendingPathComponent("bin/\(name)")
        return FileManager.default.isExecutableFile(atPath: path) ? path : nil
    }
}

/// A real muxy-daemon child process (Foundation.Process). Fires onExit on the main actor.
final class ProcessDaemon: DaemonProcess {
    private let process = Process()
    private var onExit: ((Int32) -> Void)?

    init(execPath: String, env: [String: String]) {
        process.executableURL = URL(fileURLWithPath: execPath)
        process.environment = env
        process.terminationHandler = { [weak self] p in
            let code = p.terminationStatus
            Task { @MainActor in self?.onExit?(code) }
        }
        try? process.run()
    }

    func terminate() { if process.isRunning { process.terminate() } }   // SIGTERM → M5b graceful shutdown
    func setOnExit(_ handler: @escaping (Int32) -> Void) { onExit = handler }
}

/// Build a supervisor that spawns the bundled daemon with per-user sockets + bundled bin/ on PATH.
/// Returns nil when unbundled (dev `swift run`) so the developer keeps starting the daemon by hand.
@MainActor
func makeDaemonSupervisor() -> DaemonSupervisor? {
    guard let daemonPath = MuxyPaths.bundledBin("muxy-daemon"),
          let res = Bundle.main.resourcePath else { return nil }
    let socks = MuxyPaths.socketPaths()
    var env = ProcessInfo.processInfo.environment
    env["MUXY_SOCK"] = socks.client
    env["MUXY_CONTROL_SOCK"] = socks.control
    env["MUXY_HOOK_SOCK"] = socks.hook
    let binDir = (res as NSString).appendingPathComponent("bin")
    env["PATH"] = binDir + ":" + (env["PATH"] ?? "/usr/bin:/bin")
    return DaemonSupervisor(spawn: { ProcessDaemon(execPath: daemonPath, env: env) })
}
```

- [ ] **Step 2: Wire `bootstrap()`.** In `macos/Sources/MuxyApp/App.swift`:

Add a stored supervisor property on `AppDelegate` (next to the others):

```swift
    private var daemonSupervisor: DaemonSupervisor?
```

In `bootstrap()`, replace the dev-only binary/socket resolution:

```swift
        let muxyBinary = ProcessInfo.processInfo.environment["MUXY_BIN"]
            ?? FileManager.default.currentDirectoryPath + "/../target/debug/muxy"
        let socketPath = ProcessInfo.processInfo.environment["MUXY_SOCK"] ?? "/tmp/muxy.sock"
        let controlPath = ProcessInfo.processInfo.environment["MUXY_CONTROL_SOCK"]
            ?? "/tmp/muxy-control.sock"
```
with:
```swift
        // Bundled binary + per-user sockets (dev overrides via env/MUXY_BIN still honored).
        let socks = MuxyPaths.socketPaths()
        let socketPath = socks.client
        let controlPath = socks.control
        let muxyBinary = ProcessInfo.processInfo.environment["MUXY_BIN"]
            ?? MuxyPaths.bundledBin("muxy")
            ?? FileManager.default.currentDirectoryPath + "/../target/debug/muxy"

        // Launch + supervise our own daemon when bundled (no-op / nil under `swift run`, where the
        // developer starts the daemon by hand).
        if let supervisor = makeDaemonSupervisor() {
            daemonSupervisor = supervisor
            supervisor.start()
        }
```

Keep the rest of `bootstrap()` (libghostty init, `SurfaceHost(app:muxyBinary:socketPath:)`,
`AppModel(makeTransport: { try UnixSocketConnection(path: controlPath) })`, `model.connect()`,
`StatusBarController`) unchanged.

- [ ] **Step 3: Guard the activation hack + stop the supervisor on quit.** In `App.swift`, change
`applicationDidFinishLaunching`:

```swift
    func applicationDidFinishLaunching(_ notification: Notification) {
        bootstrap()
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
    }
```
to:
```swift
    func applicationDidFinishLaunching(_ notification: Notification) {
        bootstrap()
        // A real .app bundle is frontmost on launch. Only force activation when running UNBUNDLED
        // (dev `swift run muxy-app`), where a bare executable would otherwise not become active.
        if Bundle.main.bundleIdentifier == nil {
            NSApp.setActivationPolicy(.regular)
            NSApp.activate(ignoringOtherApps: true)
        }
    }
```

And change `applicationWillTerminate`:

```swift
    func applicationWillTerminate(_ notification: Notification) {
        appModel?.shutdown()   // F1: explicit disconnect
    }
```
to:
```swift
    func applicationWillTerminate(_ notification: Notification) {
        appModel?.shutdown()          // F1: explicit disconnect
        daemonSupervisor?.stop()      // terminate the child daemon we spawned
    }
```

- [ ] **Step 4: Build the app (compiles MuxyApp against the vendored libghostty)**

Run: `cd /Users/richard/code/muxy/macos && swift build 2>&1 | tail -20`
Expected: builds with no errors. (If SourceKit editor diagnostics claim missing symbols, ignore them —
trust `swift build`; this repo has hit stale-index diagnostics before.)

- [ ] **Step 5: Run the whole MuxyCore suite (unchanged; wiring is MuxyApp-only)**

Run: `cd /Users/richard/code/muxy/macos && swift test 2>&1 | tail -6`
Expected: all MuxyCore tests still PASS (Task 3 touches MuxyApp only).

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/MuxyApp/DaemonLaunch.swift macos/Sources/MuxyApp/App.swift
git commit -m "feat(client): app launches + supervises its bundled daemon; bundle-relative paths"
```

---

## Task 4: `build-app.sh` — assemble `Muxy.app`

**Files:**
- Create: `VERSION` (repo root)
- Create: `scripts/gen-icon.swift` (renders the placeholder icon PNG)
- Create: `scripts/build-app.sh` (assembles the bundle; executable)

**Interfaces:**
- Consumes: the release binaries (`cargo build --release`), the app exe (`swift build -c release`),
  Task 3's bundle-relative resolution (so the assembled app actually works).
- Produces: `Muxy.app` (default at repo-root `dist/Muxy.app`).

- [ ] **Step 1: Create the `VERSION` file.**

```bash
printf '0.1.0\n' > /Users/richard/code/muxy/VERSION
```

- [ ] **Step 2: Create the placeholder-icon generator.** Create `scripts/gen-icon.swift`:

```swift
// Renders a simple placeholder app icon (a teal rounded square with a white "M") to a PNG.
// Usage: swift scripts/gen-icon.swift <out.png> [size]
import AppKit

let args = CommandLine.arguments
guard args.count >= 2 else { fputs("usage: gen-icon.swift <out.png> [size]\n", stderr); exit(2) }
let outPath = args[1]
let size = args.count >= 3 ? (Int(args[2]) ?? 1024) : 1024
let s = CGFloat(size)

let image = NSImage(size: NSSize(width: s, height: s))
image.lockFocus()
let rect = NSRect(x: s * 0.08, y: s * 0.08, width: s * 0.84, height: s * 0.84)
let path = NSBezierPath(roundedRect: rect, xRadius: s * 0.18, yRadius: s * 0.18)
NSColor(calibratedRed: 0.05, green: 0.55, blue: 0.55, alpha: 1).setFill()
path.fill()
let para = NSMutableParagraphStyle(); para.alignment = .center
let attrs: [NSAttributedString.Key: Any] = [
    .font: NSFont.systemFont(ofSize: s * 0.55, weight: .bold),
    .foregroundColor: NSColor.white,
    .paragraphStyle: para,
]
let m = "M" as NSString
let textSize = m.size(withAttributes: attrs)
m.draw(at: NSPoint(x: (s - textSize.width) / 2, y: (s - textSize.height) / 2), withAttributes: attrs)
image.unlockFocus()

guard let tiff = image.tiffRepresentation,
      let rep = NSBitmapImageRep(data: tiff),
      let png = rep.representation(using: .png, properties: [:]) else {
    fputs("gen-icon: failed to render PNG\n", stderr); exit(1)
}
try! png.write(to: URL(fileURLWithPath: outPath))
```

- [ ] **Step 3: Create the bundle-assembly script.** Create `scripts/build-app.sh`:

```bash
#!/usr/bin/env bash
# Assemble Muxy.app: the SwiftPM app exe + the three release Rust binaries + Info.plist + icon.
# Usage: scripts/build-app.sh [output-dir]   (default: dist/)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="${1:-$ROOT/dist}"
APP="$OUT_DIR/Muxy.app"
VERSION="$(tr -d '[:space:]' < "$ROOT/VERSION")"

echo "==> Building Rust binaries (release)"
( cd "$ROOT" && cargo build --release -p muxy-daemon -p muxy-client -p muxy-hook )

echo "==> Building macOS app (release)"
( cd "$ROOT/macos" && swift build -c release )
APP_EXE="$ROOT/macos/.build/release/muxy-app"
[ -x "$APP_EXE" ] || { echo "missing app exe: $APP_EXE" >&2; exit 1; }

echo "==> Assembling $APP (version $VERSION)"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/bin"

cp "$APP_EXE" "$APP/Contents/MacOS/muxy-app"
for bin in muxy-daemon muxy muxy-hook; do
  cp "$ROOT/target/release/$bin" "$APP/Contents/Resources/bin/$bin"
done

echo "==> Generating placeholder icon"
ICONSET="$(mktemp -d)/Muxy.iconset"; mkdir -p "$ICONSET"
BASE_PNG="$(mktemp -d)/icon.png"
swift "$ROOT/scripts/gen-icon.swift" "$BASE_PNG" 1024
for sz in 16 32 128 256 512; do
  sips -z "$sz" "$sz"       "$BASE_PNG" --out "$ICONSET/icon_${sz}x${sz}.png" >/dev/null
  sips -z $((sz*2)) $((sz*2)) "$BASE_PNG" --out "$ICONSET/icon_${sz}x${sz}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Muxy.icns"

echo "==> Writing Info.plist"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>Muxy</string>
    <key>CFBundleDisplayName</key>     <string>Muxy</string>
    <key>CFBundleIdentifier</key>      <string>com.github.richardcase.muxy</string>
    <key>CFBundleExecutable</key>      <string>muxy-app</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleShortVersionString</key> <string>$VERSION</string>
    <key>CFBundleVersion</key>         <string>$VERSION</string>
    <key>CFBundleIconFile</key>        <string>Muxy</string>
    <key>LSMinimumSystemVersion</key>  <string>14.0</string>
    <key>NSHighResolutionCapable</key> <true/>
</dict>
</plist>
PLIST

echo "==> Done: $APP"
```

Make it executable:
```bash
chmod +x /Users/richard/code/muxy/scripts/build-app.sh
```

- [ ] **Step 4: Run the script and verify the bundle layout**

Run:
```bash
source "$HOME/.cargo/env" && /Users/richard/code/muxy/scripts/build-app.sh
```
Then assert the layout:
```bash
APP=/Users/richard/code/muxy/dist/Muxy.app
test -x "$APP/Contents/MacOS/muxy-app" && \
test -x "$APP/Contents/Resources/bin/muxy-daemon" && \
test -x "$APP/Contents/Resources/bin/muxy" && \
test -x "$APP/Contents/Resources/bin/muxy-hook" && \
test -f "$APP/Contents/Resources/Muxy.icns" && \
/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist" | grep -q 'com.github.richardcase.muxy' && \
/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist" | grep -q '0.1.0' && \
echo "BUNDLE LAYOUT OK"
```
Expected: prints `BUNDLE LAYOUT OK` (all binaries present + Info.plist keys correct).

- [ ] **Step 5: gitignore the build output.** Append to `/Users/richard/code/muxy/.gitignore`:

```
/dist/
```

- [ ] **Step 6: Manual verification** (record the outcome; the user runs the GUI):
  1. `open dist/Muxy.app` (or double-click in Finder) — the app launches **without** any manual
     `cargo run -p muxy-daemon` (it spawns its own bundled daemon on the per-user socket).
  2. Spawn an agent from the GUI; it runs (proves the daemon + bundled `muxy attach` + hook work
     bundle-relative).
  3. `pkill -f 'Muxy.app/Contents/Resources/bin/muxy-daemon'` — the app **relaunches** the daemon
     (supervisor backoff) and the client **reconnects** (M5d): the "Reconnecting…" banner appears then
     clears.
  4. Quit the app (⌘Q) — the child daemon process is gone (`pgrep -f Muxy.app/.../muxy-daemon` empty).

- [ ] **Step 7: Commit**

```bash
git add VERSION scripts/gen-icon.swift scripts/build-app.sh .gitignore
git commit -m "feat(build): scripts/build-app.sh assembles Muxy.app (bundle + placeholder icon)"
```

---

## Self-Review Notes (author)

- **Spec §M6a coverage:** bundle assembly script → Task 4; the 3 bundled binaries + exe-sibling hook
  resolution → Task 4 layout (Resources/bin); bundle-relative `muxy` + retire-activation-hack → Task 3;
  app launches + supervises its own daemon → Task 2 (policy) + Task 3 (real Process); per-user sockets →
  Task 1 (config default) + Task 3 (app env); Info.plist/icon/version → Task 4. Spec §Testing bullets
  map: DaemonSupervisor tests → Task 2; muxy-config default + env-wins → Task 1; bundle layout + manual
  double-click smoke → Task 4.
- **Reuses, not redefines,** `SleepController`/`eventually` (module-scoped in `AppModelTests.swift`) —
  Task 2 must not redeclare them (duplicate-symbol compile error).
- **Composition with M5:** the daemon's flock-refusal path exits with a DISTINCT code 3 (changed from
  1 so a generic `main` `Err`/bind-failure exit-1 relaunches rather than yielding); the supervisor's
  `exit == 3` → `.yielded` branch defers to M5b's flock
  owner; a crash relaunch composes with M5d's client reconnect (Task 3 manual step 3 exercises both).
- **Dev flows preserved:** `swift run muxy-app` is unbundled → `bundledBin` returns nil →
  `makeDaemonSupervisor()` returns nil (no auto-daemon; dev starts it by hand), `muxy` resolves via
  `$MUXY_BIN`/`../target/debug`, and the activation hack still fires (`bundleIdentifier == nil`).
- **Type consistency:** `DaemonProcess`/`DaemonSupervisor`/`State`/`MuxyPaths`/`ProcessDaemon`/
  `makeDaemonSupervisor` names are used identically across Tasks 2–3; socket layout matches Task 1's
  `<runtime_dir>/muxy/<name>` on both sides (Rust default + Swift `MuxyPaths`).
- **Deferred (carry-forward):** real icon; git-tag→version (M6e); signing (M6c); the supervisor could
  add a "give up after N immediate non-1 crashes" cap (currently bounded-backoff-forever) — non-blocking.
