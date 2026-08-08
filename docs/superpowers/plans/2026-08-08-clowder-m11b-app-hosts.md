# M11b — the app consumes the host list

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended)
> or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax
> for tracking.

**Goal:** Make the macOS app read the host registry M11a built, show which backend it is connected to, and
switch between Local and any remote host from three surfaces — **without killing running agents**.

**Architecture:** All decisions live in `ClowderCore`, which `swift test` compiles; `ClowderApp` gets
renderers and `Process` plumbing only, because that target has no tests and is never compiled by
`swift test`. The app learns the host list by shelling out to `clowder remote list --json` behind an
injectable `CommandRunner`, so the whole layer is unit-testable with a fake. `DaemonSupervisor` gains
`detach()`/`resume()` so switching away from Local stops terminating the daemon, and `.failed` so an
unreachable host stops relaunching forever. Backend identity is carried as `BackendID`, and the app
passes `--socket-dir` explicitly — retiring the duplicated path derivation M11a deliberately left in
place.

**Tech Stack:** Swift 5.9 / SwiftPM, macOS 14+, XCTest, Combine. Rust changes limited to one hardening
task. No new dependencies.

**Branch:** `feat/m11b-app-hosts`, based on **`feat/m11a-host-registry`** — this is a stacked PR and
targets that branch, not `main`.

**Spec:** `docs/superpowers/specs/2026-08-07-clowder-m11-remote-host-management-design.md`
**Predecessor:** `docs/superpowers/plans/2026-08-07-clowder-m11a-host-registry.md` (PR #74)

## Global Constraints

- **Swift lives in `macos/`**: `cd macos && swift test` runs `ClowderCore`'s XCTest suite.
  `cd macos && swift build` also builds `clowder-app`, which needs the vendored libghostty (189 MB
  gitignored `macos/vendor/libghostty/ghostty-internal.a`, built by `scripts/build-libghostty.sh`,
  needs zig 0.16 + full Xcode).
- **CORRECTION (verified 2026-08-08, after this plan was first written): `swift test` DOES compile the
  `ClowderApp` target.** SwiftPM builds the whole package graph, so a compile error anywhere in
  `ClowderApp` aborts `swift test` before any test binary is linked — the suite does not merely stay
  green, it does not run at all. **Consequence: every task must leave the whole package compiling.**
  A Core change that breaks an App call site must include the minimal call-site update in the same
  commit, even when the real rewiring belongs to a later task. Do not defer it and do not work around
  it with `swift build --product ClowderPackageTests`; a commit that does not build is a `git bisect`
  hazard, and this repo merges feature branches so every commit lands individually in `main`.
- **Prefix every cargo command** with `source "$HOME/.cargo/env" && ` (Task 1 only).
- **Trust the toolchain over the editor**: stale SourceKit "No such module" / "cannot find type"
  diagnostics are common here. Believe `swift build` / `swift test` output.
- **`ClowderApp` has no tests and `swift test` never compiles it.** Every branch, decision, and derivation
  must live in `ClowderCore`. The acceptance criterion for each App file is "contains no branch a test
  could meaningfully cover".
- **All JSON from the CLI is `camelCase`** and decodes with Swift's default key coding — do not add a
  `keyDecodingStrategy`.
- **The token never enters Swift.** `clowder remote list --json` emits `hasToken: Bool`, never the token.
  Secrets reach the CLI via `--token-stdin` only, never argv.
- **Tests must be able to fail.** Before claiming a test verifies something, break the behavior it
  targets, run it, **capture the runner's actual output**, revert, and paste that output. M11a's review
  found four tests that read as rigorous but could not fail regardless of implementation, and two reports
  whose verification claims did not survive review. Reasoning about a test's power is not verification.
- **Conventional Commits** — `type(scope): subject`. The type drives the released version (`feat` → minor,
  `fix`/`perf` → patch, `test`/`docs` → no release), so a wrong type mis-versions the release. Run
  `scripts/check-commit-messages.sh` before pushing.
- **Stage every file you touched.** Verify the *committed* tree builds, not just your working directory —
  an M11a task produced a commit that did not compile because a call site was left unstaged.

## What M11a left for this milestone

Four items, recorded in PR #74. Tasks 1, 4, and 5 discharge the first three; the fourth belongs to M11c.

1. `DaemonLaunch.swift` must pass `--socket-dir`; `RemotePaths.swift`'s mirrored derivation then retires.
2. `DaemonSupervisor` must learn exit code 4 ("the first dial never landed").
3. `remote_known_hosts` is written unlocked and non-atomically — the race becomes real in this milestone,
   because the app starts driving `trust` and TOFU-recording connects concurrently.
4. Swift's `HostDraft.nameError` must be driven by `docs/protocol/fixtures/host-names.json` — **M11c**,
   which is where `HostDraft` lands.

## Two defects in existing code this milestone must fix

- **`switchBackend` kills every running local agent.** `AppDelegate.switchBackend(to:)` calls
  `daemonSupervisor?.stop()`, and agents are PTY children of the daemon that do not survive a restart.
  Task 5 + Task 9.
- **`ContentView(isRemote:)` is already stale.** It is computed once in the scene body as
  `delegate.configuredRemoteHost != nil` and never updated by a swap, so `AddProjectSheet(canBrowse:)` is
  wrong after any switch. Task 10.

## File structure

| File | Responsibility |
|---|---|
| `crates/clowder-config/src/hosts.rs`, `crates/clowder-client/src/{tofu,remote_cli}.rs` | Task 1 only: atomic + locked known-hosts writes |
| `macos/Sources/ClowderCore/RemoteHost.swift` (new) | `HostID`, `BackendID`, `HostSource`, `RemoteHost`, `HostProbe`, `FingerprintMatch` |
| `macos/Sources/ClowderCore/HostRegistry.swift` (new) | `CommandRunner`, `CommandResult`, `HostRegistry`, `HostRegistryError`, `TokenEdit` |
| `macos/Sources/ClowderCore/BackendPlan.swift` (new; absorbs `RemotePaths.swift`) | `BackendTarget`, `BackendPlan`, `SocketPaths`, `backendPlan(...)`, host-scoped forwarder dir |
| `macos/Sources/ClowderCore/DaemonSupervisor.swift` | `detach()`, `resume()`, `.detached`, `.failed`, `DaemonProcess.isRunning` |
| `macos/Sources/ClowderCore/AppModel.swift` | `activeBackend`, `reconnect(to:makeTransport:)`, `BackendSwitching`, `lastSelection` |
| `macos/Sources/ClowderCore/ConnectionChip.swift` (new) | pure `connectionChip(...)` presentation |
| `macos/Sources/ClowderCore/PaletteSearch.swift` | `.backend(BackendID)` item kind |
| `macos/Sources/ClowderApp/ProcessCommandRunner.swift` (new) | `CommandRunner` over `Foundation.Process` |
| `macos/Sources/ClowderApp/App.swift`, `DaemonLaunch.swift` | multi-supervisor `switchBackend`, plan-driven spawn |
| `macos/Sources/ClowderApp/ConnectionChipView.swift` (new), `ContentView.swift` | the sidebar chip; derived `isRemote` |
| `macos/Sources/ClowderApp/StatusBarController.swift` | full host list with checkmarks |

---

### Task 1: Make `remote_known_hosts` writes atomic and locked (Rust)

**Files:**
- Modify: `crates/clowder-config/src/hosts.rs` (export the two helpers), `crates/clowder-client/src/tofu.rs`,
  `crates/clowder-client/src/remote_cli.rs`

**Interfaces:**
- Produces: `pub fn clowder_config::hosts::write_atomic_0600(path: &Path, bytes: &[u8]) -> anyhow::Result<()>`
  and `pub struct clowder_config::hosts::FileLock` with `pub fn acquire(path: &Path) -> anyhow::Result<Self>`
  — both currently private in that module.

**Why now:** three call sites (`tofu::check`, `remote_cli::record_known_host`,
`remote_cli::prune_known_host`) do read-all → filter → `std::fs::write` with no lock and no atomic
replace. M11a's whole-branch review rated this Important and deferred it here, because this is the
milestone where the app starts driving `trust` and TOFU-recording connects concurrently. A truncated
`remote_known_hosts` loses *other* hosts' fingerprints, not just the one being written.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `crates/clowder-client/src/tofu.rs`:

```rust
    #[test]
    fn concurrent_first_sight_records_do_not_lose_lines() {
        // Each thread records a DIFFERENT host into the same file. Without a lock, two
        // read-all/filter/write cycles interleave and one host's line is lost.
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        let handles: Vec<_> = (0..16u32)
            .map(|i| {
                let kh = kh.clone();
                std::thread::spawn(move || {
                    check(&kh, &format!("host{i}:7777"), "aa11").unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let text = std::fs::read_to_string(&kh).unwrap();
        for i in 0..16u32 {
            assert!(
                text.lines().any(|l| l.split_whitespace().next() == Some(&format!("host{i}:7777"))),
                "host{i} lost from known_hosts:\n{text}"
            );
        }
    }

    #[test]
    fn known_hosts_is_written_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let kh = dir.path().join("known_hosts");
        check(&kh, "h:7777", "aa11").unwrap();
        let mode = std::fs::metadata(&kh).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "known_hosts records who you trust; it should not be world-readable");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client tofu`
Expected: `concurrent_first_sight_records_do_not_lose_lines` FAILS with at least one lost host (it may
pass intermittently — run it several times; a race that passes once is still a race). The 0600 test
FAILS with mode `0644`.

- [ ] **Step 3: Export the helpers from `clowder-config`**

In `crates/clowder-config/src/hosts.rs`, change `fn write_atomic_0600` to `pub fn write_atomic_0600`,
and `struct FileLock` / `impl FileLock { fn acquire` to `pub struct FileLock` / `pub fn acquire`. Add a
doc comment on each saying it is exported for `clowder-client`'s `remote_known_hosts` writes, which need
the identical guarantees for the identical reason.

Leave `create_private` private — callers only need the two public entry points.

- [ ] **Step 4: Use them in all three call sites**

In `crates/clowder-client/src/tofu.rs`, `check`'s first-sight branch currently ends with
`std::fs::write(path, content)`. Take the lock **before** the read and write atomically:

```rust
pub fn check(path: &Path, host: &str, fp: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Hold the lock across read-modify-write: two clients recording different hosts on first
    // sight would otherwise interleave and drop one another's line, losing trust for a host
    // neither of them was touching.
    let _guard = clowder_config::hosts::FileLock::acquire(&lock_path(path))
        .map_err(|e| format!("lock known_hosts: {e}"))?;
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read known_hosts {}: {e}", path.display())),
    };
    for line in existing.lines() {
        let mut it = line.split_whitespace();
        if let (Some(h), Some(f)) = (it.next(), it.next()) {
            if h == host {
                return if f == fp {
                    Ok(())
                } else {
                    Err(format!(
                        "REMOTE DAEMON IDENTIFICATION HAS CHANGED for {host}: known {f}, got {fp}. \
                         If you rotated the daemon cert, remove the line from {}; otherwise this may be a MITM.",
                        path.display()
                    ))
                };
            }
        }
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!("{host} {fp}\n"));
    clowder_config::hosts::write_atomic_0600(path, content.as_bytes())
        .map_err(|e| format!("write known_hosts: {e}"))
}

/// The lock file guarding `remote_known_hosts`. Separate from the data file because the data file
/// is replaced by `rename`, so a lock on its inode would not be seen by the next writer.
fn lock_path(path: &Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".lock");
    std::path::PathBuf::from(s)
}
```

In `crates/clowder-client/src/remote_cli.rs`, apply the same treatment to `record_known_host` and
`prune_known_host`: acquire `FileLock` on the `.lock` sibling before reading, and replace their
`std::fs::write` with `write_atomic_0600`. Both are currently best-effort (`let _ = …`) — **keep them
best-effort**, since the registry pin is authoritative and a failed mirror write must not fail the
command. Log to stderr on failure rather than swallowing silently.

- [ ] **Step 5: Run to verify the tests pass**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-client tofu` — repeat 5 times; the concurrency
test must be green every time. Then `source "$HOME/.cargo/env" && cargo test --workspace --locked`.

- [ ] **Step 6: Commit**

```bash
git add crates/clowder-config/src/hosts.rs crates/clowder-client/src/tofu.rs \
        crates/clowder-client/src/remote_cli.rs
git commit -m "fix(client): write remote_known_hosts atomically under a lock"
```

---

### Task 2: `RemoteHost.swift` — identity and decodable views

**Files:**
- Create: `macos/Sources/ClowderCore/RemoteHost.swift`
- Test: `macos/Tests/ClowderCoreTests/RemoteHostTests.swift`

**Interfaces:**
- Produces: `HostID`, `BackendID`, `HostSource`, `RemoteHost`, `FingerprintMatch`, `HostProbe`,
  and the `ListOutput` / `ProbeOutput` wrappers matching the CLI's JSON envelopes.

**Why `BackendID` and `RemoteHost` are separate types:** `BackendID` is the identity carried on connection
state, selection, and menus; `RemoteHost` is the value needed to *launch* a backend. Keeping them apart is
what makes a future multi-connect additive rather than a rewrite — panes get qualified by `BackendID`
without every menu having to hold a whole `RemoteHost`.

- [ ] **Step 1: Write the failing test**

Create `macos/Tests/ClowderCoreTests/RemoteHostTests.swift`:

```swift
import XCTest
@testable import ClowderCore

final class RemoteHostTests: XCTestCase {
    /// Resolve `docs/protocol/fixtures` from this source file's location, so the test does not
    /// depend on the working directory `swift test` happens to run in. Same shape as ModelsTests.
    private func fixture(_ name: String, file: StaticString = #filePath) throws -> Data {
        let here = URL(fileURLWithPath: "\(file)")
        let repo = here.deletingLastPathComponent()   // ClowderCoreTests
            .deletingLastPathComponent()              // Tests
            .deletingLastPathComponent()              // macos
            .deletingLastPathComponent()              // repo root
        return try Data(contentsOf: repo.appendingPathComponent("docs/protocol/fixtures/\(name)"))
    }

    func testDecodesTheHostListFixture() throws {
        let out = try JSONDecoder().decode(ListOutput.self, from: fixture("remote-host-list.json"))
        XCTAssertEqual(out.hosts.count, 2)

        let studio = out.hosts[0]
        XCTAssertEqual(studio.name, "studio")
        XCTAssertEqual(studio.address, "studio.tailnet:7777")
        XCTAssertTrue(studio.tls)
        XCTAssertTrue(studio.hasToken)
        XCTAssertEqual(studio.fingerprint, "a1b2")
        XCTAssertEqual(studio.source, .registry)
        XCTAssertTrue(studio.isTrusted)
        XCTAssertTrue(studio.isEditable)

        let config = out.hosts[1]
        XCTAssertEqual(config.name, "config")
        XCTAssertFalse(config.hasToken)
        XCTAssertNil(config.fingerprint)
        XCTAssertEqual(config.source, .config)
        XCTAssertFalse(config.isTrusted)
        XCTAssertFalse(config.isEditable, "a config-sourced entry lives in config.toml and is read-only")
    }

    func testDecodesTheProbeFixture() throws {
        let out = try JSONDecoder().decode(ProbeOutput.self, from: fixture("remote-probe.json"))
        XCTAssertEqual(out.probe.name, "studio")
        XCTAssertTrue(out.probe.reachable)
        XCTAssertTrue(out.probe.tls)
        XCTAssertEqual(out.probe.fingerprint, "a1b2")
        XCTAssertNil(out.probe.pinnedFingerprint)
        XCTAssertEqual(out.probe.fingerprintMatch, .new)
        XCTAssertTrue(out.probe.authenticated)
        XCTAssertNil(out.probe.error)
    }

    func testProbeAuthenticationIsNotClaimedForAPlaintextDaemon() {
        // A plaintext daemon passes expected_token: None and accepts anything, so
        // `authenticated == true` there does NOT mean authenticated. Anything rendering this
        // must consult `tls` too, so the model exposes the distinction rather than a bare Bool.
        let plaintext = HostProbe(name: "p", address: "h:1", reachable: true, tls: false,
                                  fingerprint: nil, pinnedFingerprint: nil,
                                  fingerprintMatch: nil, authenticated: true, error: nil)
        XCTAssertEqual(plaintext.authSummary, .nonePlaintext)

        let accepted = HostProbe(name: "p", address: "h:1", reachable: true, tls: true,
                                 fingerprint: "aa", pinnedFingerprint: nil,
                                 fingerprintMatch: .new, authenticated: true, error: nil)
        XCTAssertEqual(accepted.authSummary, .tokenAccepted)

        let rejected = HostProbe(name: "p", address: "h:1", reachable: true, tls: true,
                                 fingerprint: "aa", pinnedFingerprint: nil,
                                 fingerprintMatch: .new, authenticated: false, error: nil)
        XCTAssertEqual(rejected.authSummary, .tokenRejected)
    }

    func testBackendIDIsHashableAndDistinguishesHosts() {
        let a: BackendID = .remote(HostID("studio"))
        let b: BackendID = .remote(HostID("laptop"))
        XCTAssertNotEqual(a, b)
        XCTAssertNotEqual(a, .local)
        XCTAssertEqual(Set([a, b, .local, a]).count, 3)
    }

    func testBackendIDDescriptionIsStableForMenusAndLogs() {
        XCTAssertEqual(BackendID.local.description, "Local")
        XCTAssertEqual(BackendID.remote(HostID("studio")).description, "studio")
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd macos && swift test --filter RemoteHostTests`
Expected: FAIL — `cannot find 'ListOutput' in scope`, `cannot find type 'HostProbe'`.

- [ ] **Step 3: Implement**

Create `macos/Sources/ClowderCore/RemoteHost.swift`:

```swift
import Foundation

/// A host's nickname — its identity in the registry. Wrapped rather than a bare `String` so a host
/// name can never be passed where an address (or any other string) is expected.
public struct HostID: Hashable, Codable, Sendable, CustomStringConvertible {
    public let rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
    public var description: String { rawValue }
}

/// Which backend the app is pointed at. Carried on connection state, selection, and every menu.
///
/// Deliberately separate from `RemoteHost`: this is the *identity*, `RemoteHost` is the value needed
/// to launch. Keeping them apart is what lets a future multi-connect qualify panes by backend without
/// threading whole host records through the UI.
public enum BackendID: Hashable, Codable, Sendable, CustomStringConvertible {
    case local
    case remote(HostID)

    public var description: String {
        switch self {
        case .local: return "Local"
        case let .remote(id): return id.rawValue
        }
    }

    public var hostID: HostID? {
        switch self {
        case .local: return nil
        case let .remote(id): return id
        }
    }
}

/// Where an entry came from. `config` entries live in `config.toml`, which clowder never rewrites.
public enum HostSource: String, Codable, Sendable {
    case registry
    case config
}

/// How an observed certificate relates to the stored pin. Absent when no certificate was seen —
/// a plaintext daemon or a failed handshake is not a *changed* certificate.
public enum FingerprintMatch: String, Codable, Sendable {
    case new
    case match
    case changed
}

/// One remote daemon, as `clowder remote list --json` reports it.
///
/// Note what is absent: the token. The CLI emits only `hasToken`, so the app never holds the secret
/// and a future move to the Keychain touches Rust only.
public struct RemoteHost: Codable, Identifiable, Hashable, Sendable {
    public let name: String
    public let address: String
    public let tls: Bool
    public let hasToken: Bool
    public let fingerprint: String?
    public let trusted: Bool
    public let source: HostSource

    public init(name: String, address: String, tls: Bool, hasToken: Bool,
                fingerprint: String?, trusted: Bool, source: HostSource) {
        self.name = name
        self.address = address
        self.tls = tls
        self.hasToken = hasToken
        self.fingerprint = fingerprint
        self.trusted = trusted
        self.source = source
    }

    public var id: HostID { HostID(name) }
    public var backend: BackendID { .remote(id) }
    /// Paired: a pin is recorded, so the certificate is checked strictly on every connect.
    public var isTrusted: Bool { fingerprint != nil }
    /// `config`-sourced entries are read-only — they are defined in `config.toml`.
    public var isEditable: Bool { source == .registry }
}

/// The `clowder remote list --json` envelope.
public struct ListOutput: Codable, Sendable {
    public let hosts: [RemoteHost]
}

/// What one `clowder remote probe --json` observed.
public struct HostProbe: Codable, Sendable, Equatable {
    public let name: String
    public let address: String
    public let reachable: Bool
    public let tls: Bool
    public let fingerprint: String?
    public let pinnedFingerprint: String?
    public let fingerprintMatch: FingerprintMatch?
    /// Whether the daemon accepted our token. **Not** meaningful alone — a plaintext daemon accepts
    /// anything. Use `authSummary`, which folds in `tls`.
    public let authenticated: Bool
    public let error: String?

    public init(name: String, address: String, reachable: Bool, tls: Bool,
                fingerprint: String?, pinnedFingerprint: String?,
                fingerprintMatch: FingerprintMatch?, authenticated: Bool, error: String?) {
        self.name = name
        self.address = address
        self.reachable = reachable
        self.tls = tls
        self.fingerprint = fingerprint
        self.pinnedFingerprint = pinnedFingerprint
        self.fingerprintMatch = fingerprintMatch
        self.authenticated = authenticated
        self.error = error
    }

    /// What to actually tell the user about authentication.
    public enum AuthSummary: Equatable, Sendable {
        /// No TLS, so the daemon accepted our token without checking it. Reporting this as success
        /// would be a lie.
        case nonePlaintext
        case tokenAccepted
        case tokenRejected
    }

    public var authSummary: AuthSummary {
        guard tls else { return .nonePlaintext }
        return authenticated ? .tokenAccepted : .tokenRejected
    }
}

/// The `clowder remote probe --json` envelope.
public struct ProbeOutput: Codable, Sendable {
    public let probe: HostProbe
}
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `cd macos && swift test --filter RemoteHostTests`
Expected: PASS — 5 tests.

- [ ] **Step 5: Demonstrate the fixture tests can fail**

Temporarily change `RemoteHost`'s `source` decoding by renaming the property to something the fixture
does not contain, run the test, and confirm it fails with a decoding error. Revert. Capture the output.

This matters because a fixture test that silently tolerates a missing field is exactly the class of
test M11a's review kept finding: `Codable` with optionals is easy to make un-failable by accident.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/RemoteHost.swift macos/Tests/ClowderCoreTests/RemoteHostTests.swift
git commit -m "feat(app): add remote host identity and decodable CLI views"
```

---

### Task 3: `HostRegistry.swift` — the shell-out layer

**Files:**
- Create: `macos/Sources/ClowderCore/HostRegistry.swift`
- Test: `macos/Tests/ClowderCoreTests/HostRegistryTests.swift`

**Interfaces:**
- Consumes: `RemoteHost`, `ListOutput`, `HostProbe`, `ProbeOutput` (Task 2).
- Produces: `CommandResult`, `CommandRunner`, `HostRegistry`, `HostRegistryError`, `TokenEdit`.

**Why an injected runner:** `ClowderApp` is untested, so a `Process`-based registry would be untestable.
Behind `CommandRunner` the entire argv construction, JSON decoding, and error handling is driven from
`swift test` by a fake that asserts the exact arguments.

- [ ] **Step 1: Write the failing test**

Create `macos/Tests/ClowderCoreTests/HostRegistryTests.swift`:

```swift
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

    func run(_ args: [String], stdin: String?) throws -> CommandResult {
        invocations.append(Invocation(args: args, stdin: stdin))
        if let thrownError { throw thrownError }
        guard !results.isEmpty else {
            return CommandResult(status: 0, stdout: Data("{}".utf8), stderr: Data())
        }
        return results.removeFirst()
    }

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
                       ["remote", "probe", "studio", "--json"])

        let byAddr = FakeCommandRunner(); byAddr.results = [.ok(probeJSON)]
        _ = try HostRegistry(runner: byAddr).probe(address: "s:7777", token: "t", tls: true)
        let inv = try XCTUnwrap(byAddr.invocations.first)
        XCTAssertTrue(inv.args.contains("--address"))
        XCTAssertTrue(inv.args.contains("s:7777"))
        XCTAssertEqual(inv.stdin, "t", "an unsaved host's token still goes via stdin")
        XCTAssertFalse(inv.args.contains("t"))
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
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd macos && swift test --filter HostRegistryTests`
Expected: FAIL — `cannot find type 'CommandRunner'`.

- [ ] **Step 3: Implement**

Create `macos/Sources/ClowderCore/HostRegistry.swift`:

```swift
import Foundation

/// One finished subprocess run.
public struct CommandResult: Sendable {
    public let status: Int32
    public let stdout: Data
    public let stderr: Data
    public init(status: Int32, stdout: Data, stderr: Data) {
        self.status = status
        self.stdout = stdout
        self.stderr = stderr
    }
}

/// Runs the `clowder` binary. Injected so the whole registry layer is unit-testable —
/// `ClowderApp`, where the `Process` implementation lives, has no tests at all.
public protocol CommandRunner: AnyObject, Sendable {
    func run(_ args: [String], stdin: String?) throws -> CommandResult
}

public enum HostRegistryError: Error, LocalizedError, Equatable {
    /// The CLI reported a failure. Carries the CLI's own message — it is far more useful than
    /// anything this layer could invent.
    case cli(String)
    case decode(String)

    public var errorDescription: String? {
        switch self {
        case let .cli(m): return m
        case let .decode(m): return "Could not read the clowder CLI's response: \(m)"
            }
    }
}

/// How an edit treats the host's token.
public enum TokenEdit: Sendable, Equatable {
    case unchanged
    case clear
    case set(String)
}

/// Reads and writes the host registry by driving `clowder remote …`.
///
/// The app never parses `config.toml` or `hosts.json` itself — the CLI owns both, including the
/// merge that surfaces `[remote] host` as a read-only entry. This mirrors how the app already
/// asked the CLI for the resolved remote host before M11.
public struct HostRegistry {
    private let runner: CommandRunner
    public init(runner: CommandRunner) { self.runner = runner }

    public func list() throws -> [RemoteHost] {
        try decode(ListOutput.self, from: run(["remote", "list", "--json"])).hosts
    }

    public func show(name: String) throws -> RemoteHost {
        try decode(RemoteHost.self, from: run(["remote", "show", name, "--json"]))
    }

    @discardableResult
    public func add(name: String, address: String, token: String?, tls: Bool) throws -> RemoteHost {
        var args = ["remote", "add", name, address]
        args.append(tls ? "--tls" : "--no-tls")
        if token != nil { args.append("--token-stdin") }
        args.append("--json")
        return try decode(RemoteHost.self, from: run(args, stdin: token))
    }

    @discardableResult
    public func update(name: String, rename: String?, address: String?,
                       token: TokenEdit, tls: Bool?) throws -> RemoteHost {
        var args = ["remote", "set", name]
        if let rename { args += ["--rename", rename] }
        if let address { args += ["--address", address] }
        if let tls { args.append(tls ? "--tls" : "--no-tls") }
        var stdin: String?
        switch token {
        case .unchanged: break
        case .clear: args.append("--no-token")
        case let .set(t):
            args.append("--token-stdin")
            stdin = t
        }
        args.append("--json")
        return try decode(RemoteHost.self, from: run(args, stdin: stdin))
    }

    public func remove(name: String) throws {
        _ = try run(["remote", "rm", name, "--json"])
    }

    public func probe(name: String) throws -> HostProbe {
        try decode(ProbeOutput.self, from: run(["remote", "probe", name, "--json"])).probe
    }

    /// Probe a host that is not (yet) in the registry — what a "Test" button needs before saving.
    public func probe(address: String, token: String?, tls: Bool) throws -> HostProbe {
        var args = ["remote", "probe", "--address", address]
        args.append(tls ? "--tls" : "--no-tls")
        if token != nil { args.append("--token-stdin") }
        args.append("--json")
        return try decode(ProbeOutput.self, from: run(args, stdin: token)).probe
    }

    public func trust(name: String, fingerprint: String) throws {
        _ = try run(["remote", "trust", name, "--fingerprint", fingerprint, "--json"])
    }

    public func untrust(name: String) throws {
        _ = try run(["remote", "untrust", name, "--json"])
    }

    // MARK: - plumbing

    private func run(_ args: [String], stdin: String? = nil) throws -> Data {
        let result: CommandResult
        do {
            result = try runner.run(args, stdin: stdin)
        } catch {
            throw HostRegistryError.cli("Could not run the clowder CLI: \(error.localizedDescription)")
        }
        guard result.status == 0 else {
            // The CLI emits `{"error": …}` on stdout even for failures, precisely so this layer
            // has a message to show. Decode stdout FIRST and fall back to the exit code — a stray
            // library warning on stderr must not become the user-facing error.
            throw HostRegistryError.cli(errorMessage(from: result))
        }
        return result.stdout
    }

    private func errorMessage(from result: CommandResult) -> String {
        struct ErrorEnvelope: Decodable { let error: String }
        if let env = try? JSONDecoder().decode(ErrorEnvelope.self, from: result.stdout) {
            return env.error
        }
        let err = String(decoding: result.stderr, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        if !err.isEmpty { return err }
        return "clowder exited with status \(result.status)"
    }

    private func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        do {
            return try JSONDecoder().decode(type, from: data)
        } catch {
            throw HostRegistryError.decode(String(describing: error))
        }
    }
}
```

Note `String(decoding:as:)` requires no import beyond Foundation on macOS 14.

- [ ] **Step 4: Run to verify the tests pass**

Run: `cd macos && swift test --filter HostRegistryTests`
Expected: PASS — 9 tests.

- [ ] **Step 5: Demonstrate the token-in-argv test can fail**

Temporarily change `add` to append the token to `args` instead of passing it as `stdin`, run
`testAddPassesTheTokenOnStdinNeverInArgv`, and confirm it fails. Revert. **Capture the output.** This is
the assertion that protects a bearer token from `ps`; it must be proven to have teeth, not assumed.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/HostRegistry.swift macos/Tests/ClowderCoreTests/HostRegistryTests.swift
git commit -m "feat(app): read the host registry through an injectable command runner"
```

---

### Task 4: `BackendPlan.swift` — one authority for how a backend is launched

**Files:**
- Create: `macos/Sources/ClowderCore/BackendPlan.swift`
- Delete: `macos/Sources/ClowderCore/RemotePaths.swift`
- Modify: `macos/Tests/ClowderCoreTests/RemotePathsTests.swift` → rewrite as `BackendPlanTests.swift`

**Interfaces:**
- Consumes: `RemoteHost`, `BackendID` (Task 2).
- Produces: `SocketPaths`, `BackendTarget`, `BackendExecutable`, `BackendPlan`, `backendPlan(target:sockets:)`,
  `forwarderSocketDir(controlPath:host:)`.

**Why `RemotePaths.swift` goes away:** it exists only because the Rust forwarder derived its own socket
directory and Swift had to compute the identical rule — a duplication its own doc comment calls "the
load-bearing seam". M11a added `--socket-dir` so the caller owns the path. Now that the app passes it,
the mirrored derivation has one authority and Swift's copy is dead. The **only** consumer of the old
function is `makeBackendSupervisor`, updated in Task 9.

Note the Rust default is deliberately flat (`<control parent>/remote`) so M11a could merge without
breaking the app; the app now opts into a per-host directory by passing the flag. That per-host directory
is the concrete step that keeps a future multi-connect from being a rewrite.

- [ ] **Step 1: Write the failing test**

Create `macos/Tests/ClowderCoreTests/BackendPlanTests.swift` (and delete
`RemotePathsTests.swift` — its single test is superseded by `testForwarderDirIsPerHost` below):

```swift
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cd macos && swift test --filter BackendPlanTests`
Expected: FAIL — `cannot find 'backendPlan' in scope`.

- [ ] **Step 3: Implement**

Create `macos/Sources/ClowderCore/BackendPlan.swift`:

```swift
import Foundation

/// The app's three local socket paths, resolved once at startup.
public struct SocketPaths: Equatable, Sendable {
    public let client: String
    public let control: String
    public let hook: String
    public init(client: String, control: String, hook: String) {
        self.client = client
        self.control = control
        self.hook = hook
    }
}

/// Which backend to launch.
public enum BackendTarget: Equatable, Sendable {
    case local
    case remote(RemoteHost)

    public var id: BackendID {
        switch self {
        case .local: return .local
        case let .remote(h): return h.backend
        }
    }
}

/// Which bundled binary a plan runs.
public enum BackendExecutable: Equatable, Sendable {
    case daemon      // clowder-daemon
    case clowder     // clowder (the `connect` forwarder)
}

/// Everything needed to launch one backend and connect to it. Pure data, so the decision is
/// testable and `ClowderApp` only has to run it.
public struct BackendPlan: Equatable, Sendable {
    public let id: BackendID
    public let executable: BackendExecutable
    public let args: [String]
    /// Environment entries to overlay on the process environment. Deliberately an overlay, not a
    /// replacement — the child needs the inherited PATH, HOME, and the user's own settings.
    public let envOverlay: [String: String]
    public let controlPath: String
    public let renderPath: String
}

/// Where the `clowder connect` forwarder binds a given host's sockets:
/// `<control-sock parent>/remote/<host>`.
///
/// Per-host, so two hosts never collide — and so a future multi-connect needs no path changes.
/// The app passes this to the forwarder via `--socket-dir`; the forwarder's own default is flat
/// (`<control parent>/remote`) for backward compatibility, and is not used here.
public func forwarderSocketDir(controlPath: String, host: String) -> String {
    let parent = (controlPath as NSString).deletingLastPathComponent
    return ((parent as NSString).appendingPathComponent("remote") as NSString)
        .appendingPathComponent(host)
}

/// How to launch `target`.
public func backendPlan(target: BackendTarget, sockets: SocketPaths) -> BackendPlan {
    switch target {
    case .local:
        return BackendPlan(
            id: .local,
            executable: .daemon,
            args: [],
            envOverlay: [
                "CLOWDER_SOCK": sockets.client,
                "CLOWDER_CONTROL_SOCK": sockets.control,
                "CLOWDER_HOOK_SOCK": sockets.hook,
            ],
            controlPath: sockets.control,
            renderPath: sockets.client
        )

    case let .remote(host):
        let dir = forwarderSocketDir(controlPath: sockets.control, host: host.name)
        return BackendPlan(
            id: host.backend,
            executable: .clowder,
            // Select by NICKNAME: `clowder connect` resolves it through the registry, which is what
            // supplies the host's token and pin. An address would become an ad-hoc TOFU target.
            args: ["connect", host.name, "--socket-dir", dir],
            // No CLOWDER_*_SOCK overlay: those would change what the forwarder itself treats as the
            // default control socket. `--socket-dir` is the one authority for where it binds.
            envOverlay: [:],
            controlPath: (dir as NSString).appendingPathComponent("clowder-control.sock"),
            renderPath: (dir as NSString).appendingPathComponent("clowder.sock")
        )
    }
}
```

Then delete `macos/Sources/ClowderCore/RemotePaths.swift` and
`macos/Tests/ClowderCoreTests/RemotePathsTests.swift`.

- [ ] **Step 4: Keep the package compiling with a minimal call-site update**

Deleting `forwarderSocketDir(controlPath:)` breaks its only caller,
`makeBackendSupervisor` in `macos/Sources/ClowderApp/DaemonLaunch.swift:148`. Since `swift test`
compiles `ClowderApp` too (see the corrected Global Constraint), leaving it broken makes the suite
un-runnable and puts a non-building commit in history.

Make the **minimal** change — one line, same behavior as before apart from the now per-host directory:

```swift
        let dir = forwarderSocketDir(controlPath: socks.control, host: host)
```

Change nothing else in `ClowderApp`. Task 9 rewrites this whole function around `backendPlan`; this is
only the stopgap that keeps every commit buildable, exactly as M11a's Task 4 did for `forward.rs`.

- [ ] **Step 5: Run to verify the tests pass**

Run: `cd macos && swift test`
Expected: PASS — 151 tests (147 baseline − 2 from the deleted `RemotePathsTests` + 6 new).

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/BackendPlan.swift macos/Tests/ClowderCoreTests/BackendPlanTests.swift \
        macos/Sources/ClowderApp/DaemonLaunch.swift
git rm macos/Sources/ClowderCore/RemotePaths.swift macos/Tests/ClowderCoreTests/RemotePathsTests.swift
git commit -m "feat(app): derive backend launch plans, one authority for socket paths"
```

---

### Task 5: `DaemonSupervisor` — detach, resume, and exit 4

**Files:**
- Modify: `macos/Sources/ClowderCore/DaemonSupervisor.swift`,
  `macos/Tests/ClowderCoreTests/DaemonSupervisorTests.swift`

**Interfaces:**
- Produces: `DaemonProcess.isRunning`; `State.detached`, `State.failed(String)`; `detach()`, `resume()`.
- Consumed by: Task 9's multi-supervisor `switchBackend`.

**Why detach:** agents are PTY children of the daemon and do not survive a restart. Today's
`switchBackend` calls `stop()`, which SIGTERMs the daemon and kills every running local agent. Detaching
keeps the process **and its handle**, so switching back re-adopts the same daemon rather than respawning a
doomed one — and there is no single-instance flock contention, because nothing was restarted.

**Why `.failed`:** `clowder connect` exits **4** when its first dial never lands. Without a distinct
state, `handleExit` treats it as a crash and relaunches forever — a permanent "Reconnecting…" with no way
to tell a typo from a daemon that is down. This mirrors the existing exit-3 → `.yielded` rule.

- [ ] **Step 1: Write the failing tests**

Add `isRunning` to the existing `FakeDaemonProcess` in `DaemonSupervisorTests.swift`:

```swift
@MainActor
final class FakeDaemonProcess: DaemonProcess {
    private(set) var terminated = false
    /// Simulates a live child. `exit(_:)` clears it, mirroring a real process.
    var isRunning = true
    private var onExit: ((Int32) -> Void)?
    func terminate() { terminated = true; isRunning = false }
    func setOnExit(_ handler: @escaping (Int32) -> Void) { onExit = handler }
    func exit(_ code: Int32) { isRunning = false; onExit?(code) }
}
```

Then append these tests:

```swift
    func testDetachDoesNotTerminateTheProcess() {
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        sup.detach()
        XCTAssertEqual(sup.state, .detached)
        // The whole point: local agents are PTY children of this process and do not survive a
        // restart, so switching away must not kill it.
        XCTAssertFalse(spawned[0].terminated, "detach must not SIGTERM the daemon")
    }

    func testResumeReadoptsAStillLiveProcessWithoutRespawning() {
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        sup.detach()
        sup.resume()
        XCTAssertEqual(sup.state, .running)
        XCTAssertEqual(spawned.count, 1, "a live daemon must be re-adopted, not respawned")
    }

    func testResumeRelaunchesWhenTheProcessDiedWhileDetached() {
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        sup.detach()
        spawned[0].isRunning = false        // died on its own while nobody was supervising
        sup.resume()
        XCTAssertEqual(sup.state, .running)
        XCTAssertEqual(spawned.count, 2, "a dead daemon must be relaunched on resume")
    }

    func testAnExitWhileDetachedDoesNotRelaunch() async {
        let controller = SleepController()
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(
            spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p },
            sleep: { await controller.sleep($0) }
        )
        sup.start()
        sup.detach()
        spawned[0].exit(139)                // crashed while detached
        XCTAssertEqual(sup.state, .detached, "a detached supervisor must not resurrect the process")
        XCTAssertEqual(controller.parkedCount, 0, "no relaunch may be scheduled while detached")
        XCTAssertEqual(spawned.count, 1)
    }

    func testExitCode4EntersFailedAndDoesNotRelaunch() async {
        let controller = SleepController()
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(
            spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p },
            sleep: { await controller.sleep($0) }
        )
        sup.start()
        spawned[0].exit(4)                  // `clowder connect`: the first dial never landed
        guard case let .failed(reason) = sup.state else {
            return XCTFail("expected .failed, got \(sup.state)")
        }
        XCTAssertFalse(reason.isEmpty, "the chip shows this reason to the user")
        XCTAssertEqual(controller.parkedCount, 0, "an unreachable host must not relaunch forever")
        XCTAssertEqual(spawned.count, 1)
    }

    func testExitCode3StillYields() {
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        spawned[0].exit(3)
        XCTAssertEqual(sup.state, .yielded, "exit 3 (lost the single-instance flock) must not change")
    }

    func testStartAfterFailedRetries() {
        // The chip offers a Retry; it must actually spawn again.
        var spawned: [FakeDaemonProcess] = []
        let sup = DaemonSupervisor(spawn: { let p = FakeDaemonProcess(); spawned.append(p); return p })
        sup.start()
        spawned[0].exit(4)
        sup.start()
        XCTAssertEqual(sup.state, .running)
        XCTAssertEqual(spawned.count, 2)
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cd macos && swift test --filter DaemonSupervisorTests`
Expected: FAIL — `value of type 'FakeDaemonProcess' has no member 'isRunning'` is satisfied by the fake,
but `DaemonProcess` has no such requirement, `.detached` / `.failed` do not exist, and
`detach` / `resume` are not found.

- [ ] **Step 3: Implement**

In `macos/Sources/ClowderCore/DaemonSupervisor.swift`:

Add to the protocol:

```swift
public protocol DaemonProcess: AnyObject {
    /// Ask the process to terminate (SIGTERM).
    func terminate()
    /// Register a handler invoked once, on the main actor, when the process exits (with its code).
    func setOnExit(_ handler: @escaping (Int32) -> Void)
    /// Whether the child is still alive. Read on `resume()` to decide between re-adopting a
    /// still-running daemon and relaunching a dead one.
    var isRunning: Bool { get }
}
```

Extend the state and add the two operations:

```swift
    public enum State: Equatable {
        case stopped
        case running
        case relaunching
        /// Lost the single-instance lock (exit 3) — another daemon owns it, so yield for good.
        case yielded
        /// Deliberately not supervising a process we left running (a backend switch). Local agents
        /// are PTY children of it, so terminating would destroy the user's work.
        case detached
        /// The backend reported a condition retrying cannot fix (exit 4: the first dial never
        /// landed). Surfaced to the user with a Retry rather than looped over.
        case failed(String)
    }
```

Add an `isDetached` flag beside `isStopping`, and:

```swift
    /// Stop supervising without killing the child.
    ///
    /// Keeps the `process` handle rather than orphaning it, so `resume()` can re-adopt the *same*
    /// daemon. Orphaning would force a respawn on every switch-back, which both kills the agents
    /// this exists to protect and races the daemon's single-instance flock.
    public func detach() {
        guard process != nil || relaunchTask != nil else { return }
        isDetached = true
        relaunchTask?.cancel()
        relaunchTask = nil
        state = .detached
    }

    /// Resume supervision: re-adopt a still-running child, or relaunch if it died while detached.
    public func resume() {
        guard isDetached else { return }
        isDetached = false
        if let p = process, p.isRunning {
            // The onExit handler registered at launch is still installed, so supervision simply
            // takes effect again.
            state = .running
        } else {
            process = nil
            relaunchAttempt = 0
            launch()
        }
    }
```

Update `handleExit` and `start()`:

```swift
    private func handleExit(_ code: Int32) {
        process = nil
        guard !isStopping else { return }
        // A process we deliberately stopped supervising must not be resurrected — the user switched
        // away from this backend on purpose.
        guard !isDetached else { return }
        if code == 3 {
            // Daemon's DISTINCT single-instance-loser code (lost M5b's flock) → defer to the owner.
            // NOT code 1: `main() -> Result<()>` returning Err (e.g. a bind failure) also exits 1 and
            // must relaunch, not yield.
            state = .yielded
            return
        }
        if code == 4 {
            // `clowder connect`'s DISTINCT "the first dial never landed" code. Relaunching cannot
            // fix a wrong address or a daemon that is down, and doing so forever would leave the
            // user staring at "Reconnecting…" with no way to tell those apart.
            state = .failed("could not reach the remote daemon")
            return
        }
        scheduleRelaunch()
    }

    public func start() {
        guard process == nil, relaunchTask == nil else { return }
        isStopping = false
        isDetached = false
        relaunchAttempt = 0
        launch()
    }
```

Also set `isDetached = false` in `stop()`, so a stopped supervisor is unambiguously stopped.

- [ ] **Step 4: Run to verify the tests pass**

Run: `cd macos && swift test --filter DaemonSupervisorTests`
Expected: PASS — 7 new tests plus the existing ones.

- [ ] **Step 5: Demonstrate two tests can fail**

Break `detach()` to call `process?.terminate()` and confirm `testDetachDoesNotTerminateTheProcess`
fails. Then remove the `code == 4` branch and confirm `testExitCode4EntersFailedAndDoesNotRelaunch`
fails. **Capture both outputs.** Revert.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/DaemonSupervisor.swift \
        macos/Tests/ClowderCoreTests/DaemonSupervisorTests.swift
git commit -m "fix(app): detach instead of killing the daemon, and stop relaunching on exit 4"
```

---

### Task 6: `AppModel` — active backend, switching protocol, selection memory

**Files:**
- Modify: `macos/Sources/ClowderCore/AppModel.swift`,
  `macos/Tests/ClowderCoreTests/AppModelTests.swift`

**Interfaces:**
- Consumes: `BackendID`, `RemoteHost` (Task 2).
- Produces: `AppModel.activeBackend`, `AppModel.reconnect(to:makeTransport:)`, `BackendSwitching`,
  `AppModel.hosts`, `AppModel.backends`.

**Why `lastSelection`:** `reconnect` clears `selection` (correctly — the new backend's panes are
different). Without remembering it per backend, switching away and back drops the user where they
started rather than where they left, which makes switching feel like a restart instead of like tabs.

- [ ] **Step 1: Write the failing tests**

Append to `macos/Tests/ClowderCoreTests/AppModelTests.swift`:

```swift
/// A fake `BackendSwitching` so views and menus can be driven without an AppDelegate.
@MainActor
final class FakeBackendSwitching: BackendSwitching {
    var hosts: [RemoteHost] = []
    var activeBackend: BackendID = .local
    private(set) var switched: [BackendID] = []
    private(set) var refreshCount = 0
    func switchBackend(to backend: BackendID) { switched.append(backend) }
    func refreshHosts() { refreshCount += 1 }
}

@MainActor
final class AppModelBackendTests: XCTestCase {
    private func hosts() -> [RemoteHost] {
        [RemoteHost(name: "studio", address: "s:7777", tls: true, hasToken: true,
                    fingerprint: "a1b2", trusted: true, source: .registry)]
    }

    func testActiveBackendStartsLocal() {
        let model = AppModel(makeTransport: { FakeControlTransport() })
        XCTAssertEqual(model.activeBackend, .local)
    }

    func testReconnectToRecordsTheNewBackend() {
        let model = AppModel(makeTransport: { FakeControlTransport() })
        model.connect()
        model.reconnect(to: .remote(HostID("studio")), makeTransport: { FakeControlTransport() })
        XCTAssertEqual(model.activeBackend, .remote(HostID("studio")))
    }

    func testSwitchingAwayAndBackRestoresTheSelection() async {
        let model = AppModel(makeTransport: { FakeControlTransport() })
        model.connect()
        model.selection = .worktree(42)

        model.reconnect(to: .remote(HostID("studio")), makeTransport: { FakeControlTransport() })
        // The remote backend's panes are different, so the selection must be cleared on arrival.
        XCTAssertNil(model.selection)

        model.reconnect(to: .local, makeTransport: { FakeControlTransport() })
        let restored = await eventually { model.selection == .worktree(42) }
        XCTAssertTrue(restored, "returning to a backend must restore where the user left off")
    }

    func testSelectionMemoryIsPerBackend() async {
        let model = AppModel(makeTransport: { FakeControlTransport() })
        model.connect()
        model.selection = .worktree(1)
        model.reconnect(to: .remote(HostID("studio")), makeTransport: { FakeControlTransport() })
        model.selection = .worktree(2)
        model.reconnect(to: .local, makeTransport: { FakeControlTransport() })
        _ = await eventually { model.selection != nil }
        XCTAssertEqual(model.selection, .worktree(1), "each backend remembers its own selection")
    }

    func testReconnectToTheSameBackendStillReconnects() {
        // A Retry after `.failed` targets the backend already active; it must not be a no-op.
        let model = AppModel(makeTransport: { FakeControlTransport() })
        model.connect()
        var built = 0
        model.reconnect(to: .local, makeTransport: { built += 1; return FakeControlTransport() })
        XCTAssertEqual(built, 1)
    }

    func testHostsArePublishedForTheUISurfaces() {
        let model = AppModel(makeTransport: { FakeControlTransport() })
        model.setHosts(hosts())
        XCTAssertEqual(model.hosts.map(\.name), ["studio"])
    }

    func testBackendSwitchingIsForwardedToTheDelegate() {
        let model = AppModel(makeTransport: { FakeControlTransport() })
        let fake = FakeBackendSwitching()
        model.backends = fake
        model.requestSwitch(to: .remote(HostID("studio")))
        XCTAssertEqual(fake.switched, [.remote(HostID("studio"))])
        model.requestHostRefresh()
        XCTAssertEqual(fake.refreshCount, 1)
    }

    func testRequestSwitchWithNoDelegateIsHarmless() {
        let model = AppModel(makeTransport: { FakeControlTransport() })
        model.requestSwitch(to: .local)     // must not crash or trap
        XCTAssertEqual(model.activeBackend, .local)
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd macos && swift test --filter AppModelBackendTests`
Expected: FAIL — `cannot find type 'BackendSwitching'`, `AppModel has no member 'activeBackend'`.

- [ ] **Step 3: Implement**

In `macos/Sources/ClowderCore/AppModel.swift`, add above the class:

```swift
/// Who owns backend processes and the host list. `AppDelegate` conforms; the chip, the menu bar,
/// and the command palette all read this one source rather than each holding their own closures.
@MainActor
public protocol BackendSwitching: AnyObject {
    var hosts: [RemoteHost] { get }
    var activeBackend: BackendID { get }
    func switchBackend(to backend: BackendID)
    func refreshHosts()
}
```

Add to `AppModel`:

```swift
    /// Which backend the control channel is pointed at.
    @Published public private(set) var activeBackend: BackendID = .local
    /// The known remote hosts, as last read from the registry.
    @Published public private(set) var hosts: [RemoteHost] = []

    /// Owns the backend processes. `weak` because the delegate owns this model.
    public weak var backends: BackendSwitching?

    /// Where the user was in each backend, so switching feels like tabs rather than a restart.
    /// `reconnect` necessarily clears `selection` — the new backend's panes are different — and
    /// this is what puts it back on return.
    private var lastSelection: [BackendID: SidebarSelection] = [:]

    public func setHosts(_ hosts: [RemoteHost]) { self.hosts = hosts }

    public func requestSwitch(to backend: BackendID) { backends?.switchBackend(to: backend) }
    public func requestHostRefresh() { backends?.refreshHosts() }
```

Replace `reconnect(makeTransport:)` with a backend-aware version, keeping the old signature as a
deprecated shim is **not** needed — `AppDelegate` is its only caller and Task 9 updates it:

```swift
    /// Point the control channel at a different backend (a live local↔remote swap): tear down the
    /// current connection + reconnect loop, drop the previous backend's agents, then connect to the
    /// new transport. Keeps the same `AppModel` instance so SwiftUI views stay bound.
    ///
    /// Remembers the outgoing backend's selection and restores the incoming one's once the
    /// connection is live and its worktrees have arrived.
    public func reconnect(to backend: BackendID,
                          makeTransport newMakeTransport: @escaping () throws -> ControlTransport) {
        if let current = selection { lastSelection[activeBackend] = current }
        shutdown()                       // cancel reconnect, disconnect, clear session/connection
        store.reset()                    // drop the previous backend's agents/trees
        selection = nil
        activeBackend = backend
        pendingRestore = lastSelection[backend]
        self.makeTransport = newMakeTransport
        isShuttingDown = false
        connectionState = .connecting
        do {
            try attemptConnect()
        } catch {
            // The freshly-started backend may still be binding its socket — retry with backoff
            // (the same bounded loop as a live drop) rather than giving up in `.closed`.
            scheduleReconnect()
        }
    }
```

Add the restore machinery. The selection can only be restored once the backend's worktrees exist, which
arrives asynchronously, so hook the store subscription that already runs on every store change:

```swift
    /// A selection to re-apply once the incoming backend's worktrees arrive. Cleared after one
    /// successful restore (or when the user selects something themselves).
    private var pendingRestore: SidebarSelection?

    /// Re-apply a remembered selection once its target exists in the new backend's store.
    /// Silently gives up if the pane or project is gone — a worktree may have been landed on the
    /// other machine since we were last here.
    private func restorePendingSelectionIfPossible() {
        guard let want = pendingRestore, selection == nil else { return }
        switch want {
        case let .worktree(pane):
            guard store.worktrees[pane] != nil else { return }
        case let .project(path):
            guard store.projects.contains(where: { $0.path == path }) else { return }
        }
        pendingRestore = nil
        selection = want
    }
```

Call it from the existing store subscription in `init`, alongside the two reconcile calls:

```swift
        self.storeSubscription = store.objectWillChange.sink { [weak self] _ in
            self?.objectWillChange.send()
            DispatchQueue.main.async {
                self?.restorePendingSelectionIfPossible()
                self?.reconcileFocus()
                self?.reconcileProjectSelection()
            }
        }
```

And clear `pendingRestore` in `selection`'s `didSet` when the user picks something, so a late store
update cannot yank them elsewhere. Add as the first line of the `didSet` body:

```swift
            if selection != nil { pendingRestore = nil }
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `cd macos && swift test`
Expected: PASS — the whole Core suite, including the existing `AppModelTests`.

`FakeControlTransport` (AppModelTests.swift:5) delivers nothing by default, so
`testSwitchingAwayAndBackRestoresTheSelection` needs the store to contain pane 42. If the test needs a
worktree present, use the fake's `deliver(_:)` to send a `worktreeList` event containing pane 42 before
switching — mirror how the existing tests hydrate the store, and say in your report which approach you
took.

- [ ] **Step 5: Demonstrate the restore test can fail**

Remove the `restorePendingSelectionIfPossible()` call from the subscription and confirm
`testSwitchingAwayAndBackRestoresTheSelection` fails. **Capture the output.** Revert.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/AppModel.swift macos/Tests/ClowderCoreTests/AppModelTests.swift
git commit -m "feat(app): track the active backend and restore per-backend selection"
```

---

### Task 7: `ConnectionChip.swift` — pure presentation

**Files:**
- Create: `macos/Sources/ClowderCore/ConnectionChip.swift`
- Test: `macos/Tests/ClowderCoreTests/ConnectionChipTests.swift`

**Interfaces:**
- Consumes: `BackendID`, `RemoteHost` (Task 2), `AppModel.ConnectionState`, `DaemonSupervisor.State` (Task 5).
- Produces: `ChipTone`, `ConnectionChip`, `connectionChip(backend:hosts:connection:supervisor:)`.

**Why in Core:** this is the only place that decides what the user is told about their connection, and it
is entirely a function of four inputs. Putting it in a SwiftUI view would make it untestable.

- [ ] **Step 1: Write the failing test**

Create `macos/Tests/ClowderCoreTests/ConnectionChipTests.swift`:

```swift
import XCTest
@testable import ClowderCore

final class ConnectionChipTests: XCTestCase {
    private let studio = RemoteHost(name: "studio", address: "studio.tail:7777", tls: true,
                                    hasToken: true, fingerprint: "a1b2", trusted: true,
                                    source: .registry)

    private func chip(_ backend: BackendID,
                      _ connection: AppModel.ConnectionState,
                      supervisor: DaemonSupervisor.State = .running,
                      hosts: [RemoteHost]? = nil) -> ConnectionChip {
        connectionChip(backend: backend, hosts: hosts ?? [studio],
                       connection: connection, supervisor: supervisor)
    }

    func testLocalAndLiveReadsLocal() {
        let c = chip(.local, .live)
        XCTAssertEqual(c.title, "Local")
        XCTAssertEqual(c.tone, .ok)
        XCTAssertNil(c.detail)
    }

    func testRemoteAndLiveNamesTheHostAndShowsItsAddress() {
        let c = chip(.remote(HostID("studio")), .live)
        XCTAssertEqual(c.title, "studio")
        XCTAssertEqual(c.detail, "studio.tail:7777")
        XCTAssertEqual(c.tone, .ok)
    }

    func testConnectingIsPendingAndNotAnError() {
        // The startup grace period deliberately shows no banner; the chip must match that and
        // not flash red on an ordinary cold start.
        XCTAssertEqual(chip(.local, .connecting).tone, .pending)
    }

    func testReconnectingIsAWarningNotAnError() {
        XCTAssertEqual(chip(.local, .reconnecting).tone, .warning)
    }

    func testClosedIsAnErrorAndCarriesTheReason() {
        let c = chip(.local, .closed(reason: "socket gone"))
        XCTAssertEqual(c.tone, .error)
        XCTAssertEqual(c.detail, "socket gone")
    }

    func testAFailedSupervisorWinsOverTheConnectionState() {
        // An unreachable host leaves the control channel merely "connecting" forever. The
        // supervisor knows the real story (exit 4), so it must take precedence — otherwise the
        // user sees a hopeful spinner for a host that will never answer.
        let c = chip(.remote(HostID("studio")), .connecting,
                     supervisor: .failed("could not reach the remote daemon"))
        XCTAssertEqual(c.tone, .error)
        XCTAssertTrue(c.detail?.contains("could not reach") ?? false, "\(String(describing: c.detail))")
        XCTAssertTrue(c.canRetry, "a failed backend must offer a Retry")
    }

    func testAYieldedLocalSupervisorIsHealthyNotAnError() {
        // Exit 3 means another daemon owns the lock — an externally started daemon is a perfectly
        // good backend, and the app connects to its sockets. Saying "error" would be wrong.
        let c = chip(.local, .live, supervisor: .yielded)
        XCTAssertEqual(c.tone, .ok)
        XCTAssertEqual(c.detail, "external daemon")
    }

    func testAnUnknownHostIDDegradesGracefully() {
        // The host was removed from the registry while connected to it.
        let c = chip(.remote(HostID("ghost")), .live, hosts: [])
        XCTAssertEqual(c.title, "ghost")
        XCTAssertEqual(c.detail, "not in your host list")
        XCTAssertEqual(c.tone, .warning)
    }

    func testRetryIsOfferedOnlyWhereItHelps() {
        XCTAssertFalse(chip(.local, .live).canRetry)
        XCTAssertFalse(chip(.local, .reconnecting).canRetry, "the reconnect loop is already retrying")
        XCTAssertTrue(chip(.local, .closed(reason: "x")).canRetry)
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd macos && swift test --filter ConnectionChipTests`
Expected: FAIL — `cannot find 'connectionChip' in scope`.

- [ ] **Step 3: Implement**

Create `macos/Sources/ClowderCore/ConnectionChip.swift`:

```swift
import Foundation

/// How urgent the chip looks. Mapped to colours by the view, so the decision stays testable.
public enum ChipTone: Equatable, Sendable {
    case ok
    case pending
    case warning
    case error
}

/// Everything the sidebar's connection chip renders.
public struct ConnectionChip: Equatable, Sendable {
    public let title: String
    public let detail: String?
    public let symbol: String
    public let tone: ChipTone
    /// Whether to offer a Retry. False where a retry loop is already running, or where there is
    /// nothing to retry.
    public let canRetry: Bool
}

/// What to tell the user about the current connection.
///
/// The supervisor's state takes precedence over the control channel's, because a backend that
/// exited with a terminal condition leaves the control channel merely "connecting" forever — a
/// hopeful spinner for a host that will never answer.
public func connectionChip(backend: BackendID,
                           hosts: [RemoteHost],
                           connection: AppModel.ConnectionState,
                           supervisor: DaemonSupervisor.State) -> ConnectionChip {
    let host = backend.hostID.flatMap { id in hosts.first { $0.id == id } }
    let title = backend.description
    let symbol = backend == .local ? "desktopcomputer" : "network"

    // Terminal backend failure wins: retrying the control channel cannot fix a wrong address.
    if case let .failed(reason) = supervisor {
        return ConnectionChip(title: title, detail: reason, symbol: symbol,
                              tone: .error, canRetry: true)
    }

    // A remote backend whose host is no longer in the registry: still connected, but the user
    // should know the entry is gone (they cannot re-select it after switching away).
    if backend != .local, host == nil {
        return ConnectionChip(title: title, detail: "not in your host list", symbol: symbol,
                              tone: .warning, canRetry: false)
    }

    switch connection {
    case .live:
        // Exit 3 = another daemon owns the single-instance lock. That daemon is serving us
        // perfectly well, so this is a healthy state with a note, not an error.
        let detail: String? = {
            if case .yielded = supervisor { return "external daemon" }
            return host?.address
        }()
        return ConnectionChip(title: title, detail: detail, symbol: symbol,
                              tone: .ok, canRetry: false)

    case .connecting:
        // Mirrors AppModel's startup grace period, which deliberately shows no banner.
        return ConnectionChip(title: title, detail: "connecting…", symbol: symbol,
                              tone: .pending, canRetry: false)

    case .reconnecting:
        return ConnectionChip(title: title, detail: "reconnecting…", symbol: symbol,
                              tone: .warning, canRetry: false)

    case let .closed(reason):
        return ConnectionChip(title: title, detail: reason, symbol: symbol,
                              tone: .error, canRetry: true)
    }
}
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `cd macos && swift test --filter ConnectionChipTests`
Expected: PASS — 9 tests.

- [ ] **Step 5: Demonstrate the precedence test can fail**

Move the `.failed` check below the `switch connection` block and confirm
`testAFailedSupervisorWinsOverTheConnectionState` fails. **Capture the output.** Revert.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/ConnectionChip.swift \
        macos/Tests/ClowderCoreTests/ConnectionChipTests.swift
git commit -m "feat(app): derive the connection chip's contents in ClowderCore"
```

---

### Task 8: Command-palette backend entries

**Files:**
- Modify: `macos/Sources/ClowderCore/PaletteSearch.swift`,
  `macos/Tests/ClowderCoreTests/PaletteSearchTests.swift`

**Interfaces:**
- Consumes: `BackendID`, `RemoteHost` (Task 2).
- Produces: `PaletteItemKind.backend(BackendID)` and a `hosts:`/`activeBackend:` parameter pair on
  `paletteResults`, both defaulted so existing callers keep compiling.

**Why not a new `CommandID`:** `CommandRegistry.all` is static and `AppModel.run`/`isEnabled` switch
exhaustively over it. A backend entry is data, not a fixed command, so adding a `CommandID` case would
mean a synthetic command per host and touching both switches.

- [ ] **Step 1: Write the failing test**

Append to `macos/Tests/ClowderCoreTests/PaletteSearchTests.swift`:

```swift
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
```

Two signatures verified against source, since getting them wrong is the most common way a plan's test
code fails to compile:

- `WorktreeInfo.init(pane:project:name:branch:state:)` — `branch` is **required**
  (`macos/Sources/ClowderCore/Models.swift:25`), conventionally `clowder/<name>`.
- `CommandRegistry.all(keymap:)` is a **static function**, not a property
  (`macos/Sources/ClowderCore/Keymap.swift:76`). `AttentionState` cases are
  `idle`/`working`/`needsInput`/`completed`/`exited` (`Models.swift:4-10`).

- [ ] **Step 2: Run to verify failure**

Run: `cd macos && swift test --filter PaletteSearchTests`
Expected: FAIL — `type 'PaletteItemKind' has no member 'backend'`.

- [ ] **Step 3: Implement**

In `macos/Sources/ClowderCore/PaletteSearch.swift`:

```swift
public enum PaletteItemKind: Hashable, Sendable {
    case command(CommandID)
    case agent(pane: UInt64)
    /// Switch to a backend. Data rather than a `CommandID` — there is one per host, and
    /// `CommandRegistry.all` is a fixed list that `AppModel.run` switches over exhaustively.
    case backend(BackendID)
}
```

Extend `paletteResults`, keeping the new parameters defaulted:

```swift
/// Fuzzy-filter commands (matched on title), backends (matched on name and address), and worktrees
/// (matched on "project name") into one ranked list — commands first, then backends, then
/// worktrees. Ties keep input order.
///
/// `hosts`/`activeBackend` default to "no backend entries", so every pre-M11b call site is
/// unaffected.
public func paletteResults(query: String,
                           commands: [Command],
                           worktrees: [WorktreeInfo],
                           hosts: [RemoteHost] = [],
                           activeBackend: BackendID = .local) -> [PaletteItem] {
    let trimmed = query.trimmingCharacters(in: .whitespaces)

    let cmdItems = commands.enumerated().compactMap { (i, c) -> (Int, Int, PaletteItem)? in
        guard let r = fuzzyRank(trimmed, c.title) else { return nil }
        return (r, i, PaletteItem(id: .command(c.id), title: c.title, subtitle: c.subtitle, kind: .command(c.id)))
    }

    // Every backend except the one already active — "Connect to <the thing you are connected to>"
    // is noise, and selecting it would tear down a healthy session for no gain.
    var candidates: [(BackendID, String, String?)] = []
    if activeBackend != .local {
        candidates.append((.local, "Local", nil))
    }
    for h in hosts where h.backend != activeBackend {
        candidates.append((h.backend, h.name, h.address))
    }
    let backendItems = candidates.enumerated().compactMap { (i, c) -> (Int, Int, PaletteItem)? in
        let (id, name, address) = c
        let haystack = [name, address].compactMap { $0 }.joined(separator: " ")
        guard let r = fuzzyRank(trimmed, "Connect to \(haystack)") else { return nil }
        return (r, i, PaletteItem(id: .backend(id), title: "Connect to \(name)",
                                  subtitle: address, kind: .backend(id)))
    }

    let agentItems = worktrees.enumerated().compactMap { (i, a) -> (Int, Int, PaletteItem)? in
        guard let r = fuzzyRank(trimmed, "\(a.project) \(a.name)") else { return nil }
        let proj = (a.project as NSString).lastPathComponent
        let sub = proj.isEmpty ? a.project : proj
        return (r, i, PaletteItem(id: .agent(pane: a.pane), title: a.name, subtitle: sub, kind: .agent(pane: a.pane)))
    }

    let sortedCmds = cmdItems.sorted { ($0.0, $0.1) < ($1.0, $1.1) }.map(\.2)
    let sortedBackends = backendItems.sorted { ($0.0, $0.1) < ($1.0, $1.1) }.map(\.2)
    let sortedAgents = agentItems.sorted { ($0.0, $0.1) < ($1.0, $1.1) }.map(\.2)
    return sortedCmds + sortedBackends + sortedAgents
}
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `cd macos && swift test`
Expected: PASS — the whole Core suite.

- [ ] **Step 5: Demonstrate the exclusion test can fail**

Remove the `where h.backend != activeBackend` filter and confirm
`testTheActiveRemoteIsExcludedAndLocalIsOffered` fails. **Capture the output.** Revert.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/PaletteSearch.swift \
        macos/Tests/ClowderCoreTests/PaletteSearchTests.swift
git commit -m "feat(app): offer backend switching from the command palette"
```

---

### Task 9: `ProcessCommandRunner` + the multi-supervisor `AppDelegate`

**Files:**
- Create: `macos/Sources/ClowderApp/ProcessCommandRunner.swift`
- Modify: `macos/Sources/ClowderApp/DaemonLaunch.swift`, `macos/Sources/ClowderApp/App.swift`

**Interfaces:**
- Consumes: `HostRegistry`, `CommandRunner` (Task 3), `backendPlan` (Task 4), supervisor
  `detach`/`resume`/`.failed` (Task 5), `AppModel.reconnect(to:)` + `BackendSwitching` (Task 6).

This is the first task that needs `swift build`, and therefore the vendored libghostty. If it is not
built, run `scripts/build-libghostty.sh` (zig 0.16 + full Xcode) — and if that is unavailable, report
BLOCKED rather than guessing at whether the code compiles.

**No tests here by construction.** `ClowderApp` has none and `swift test` never compiles it. Every
decision this task needs already lives in Core and is tested there; what remains is `Process` plumbing
and wiring.

- [ ] **Step 1: Write `ProcessCommandRunner`**

Create `macos/Sources/ClowderApp/ProcessCommandRunner.swift`:

```swift
import Foundation
import ClowderCore

/// Runs the bundled `clowder` binary. The only `CommandRunner` in the app; everything that decides
/// *what* to run lives in ClowderCore's `HostRegistry`, which is unit-tested against a fake.
final class ProcessCommandRunner: CommandRunner, @unchecked Sendable {
    private let executablePath: String

    init(executablePath: String) {
        self.executablePath = executablePath
    }

    func run(_ args: [String], stdin: String?) throws -> CommandResult {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: executablePath)
        proc.arguments = args

        let out = Pipe()
        let err = Pipe()
        proc.standardOutput = out
        proc.standardError = err

        let input = Pipe()
        proc.standardInput = input

        try proc.run()

        // Write and close stdin BEFORE draining: the child reads to EOF on --token-stdin, so
        // leaving the pipe open would deadlock both sides.
        if let stdin {
            input.fileHandleForWriting.write(Data(stdin.utf8))
        }
        try? input.fileHandleForWriting.close()

        // Read BEFORE waiting: draining after waitUntilExit() can deadlock if the child fills the
        // pipe buffer. Output is small today, but read-before-wait is the safe order — the same
        // discipline the removed `resolveRemoteHost` documented.
        let stdoutData = out.fileHandleForReading.readDataToEndOfFile()
        let stderrData = err.fileHandleForReading.readDataToEndOfFile()
        proc.waitUntilExit()

        return CommandResult(status: proc.terminationStatus, stdout: stdoutData, stderr: stderrData)
    }
}
```

- [ ] **Step 2: Rewrite `makeBackendSupervisor` to run a plan**

In `macos/Sources/ClowderApp/DaemonLaunch.swift`, replace `makeBackendSupervisor(remoteHost:)` with:

```swift
/// Build a supervisor for `plan`, plus the sockets the app should connect to. Returns nil when
/// running unbundled (`swift run clowder-app` has no Rust siblings), where the dev workflow is to
/// run the daemon by hand.
@MainActor
func makeBackendSupervisor(plan: BackendPlan) -> DaemonSupervisor? {
    let name = plan.executable == .daemon ? "clowder-daemon" : "clowder"
    guard let execPath = ClowderPaths.bundledBin(name) else { return nil }

    var env = ProcessInfo.processInfo.environment
    for (k, v) in plan.envOverlay { env[k] = v }
    // The forwarder shells out to nothing, but the daemon spawns agent adapters — keep the bundled
    // binaries first on PATH so `clowder-hook` resolves.
    let binDir = (execPath as NSString).deletingLastPathComponent
    env["PATH"] = binDir + ":" + (env["PATH"] ?? "/usr/bin:/bin")

    return DaemonSupervisor(spawn: {
        ProcessDaemon(execPath: execPath, args: plan.args, env: env)
    })
}
```

Add `isRunning` to `ProcessDaemon` to satisfy the protocol change from Task 5:

```swift
    var isRunning: Bool { process.isRunning }
```

- [ ] **Step 3: Rewire `AppDelegate`**

In `macos/Sources/ClowderApp/App.swift`:

Replace `currentRemoteHost` / `configuredRemoteHost` with plan-driven state, delete
`resolveRemoteHost(clowderBinary:)` entirely, and hold one supervisor per backend:

```swift
    private var supervisors: [BackendID: DaemonSupervisor] = [:]
    private var hostRegistry: HostRegistry?
    private(set) var hosts: [RemoteHost] = []
    private(set) var activeBackend: BackendID = .local
    private var sockets = SocketPaths(client: "", control: "", hook: "")
```

In `bootstrap()`, read the registry instead of the single `remote-host` line, and start Local:

```swift
        let socks = ClowderPaths.socketPaths()
        sockets = SocketPaths(client: socks.client, control: socks.control, hook: socks.hook)
        let clowderBinary = ProcessInfo.processInfo.environment["CLOWDER_BIN"]
            ?? ClowderPaths.bundledBin("clowder")
            ?? FileManager.default.currentDirectoryPath + "/../target/debug/clowder"
        hostRegistry = HostRegistry(runner: ProcessCommandRunner(executablePath: clowderBinary))
        refreshHosts()

        // Always start Local. Unlike pre-M11b, a configured remote host no longer changes what the
        // app connects to at launch — the user picks a backend, and the chip says which it is.
        let plan = backendPlan(target: .local, sockets: sockets)
        var controlPath = plan.controlPath
        var socketPath = plan.renderPath
        if let sup = makeBackendSupervisor(plan: plan) {
            supervisors[.local] = sup
            sup.start()
        } else {
            // Unbundled dev: no supervisor, but the default sockets are still correct.
            controlPath = socks.control
            socketPath = socks.client
        }
```

Then wire the model and conform to `BackendSwitching`:

```swift
        let model = AppModel(makeTransport: { try UnixSocketConnection(path: controlPath) })
        model.backends = self
        model.setHosts(hosts)
```

```swift
extension AppDelegate: BackendSwitching {
    /// Read the registry. Cheap enough to do on demand (menu/palette/settings open); there is
    /// deliberately no file watcher.
    func refreshHosts() {
        guard let hostRegistry else { return }
        do {
            hosts = try hostRegistry.list()
            appModel?.setHosts(hosts)
        } catch {
            DaemonLog.note("could not read the host registry: \(error.localizedDescription)")
        }
    }

    /// Live backend swap. Reconfigures the SAME AppModel + SurfaceHost in place, so SwiftUI keeps
    /// its bindings.
    ///
    /// Local is DETACHED rather than stopped: its agents are PTY children that do not survive a
    /// restart, and switching back re-adopts the same daemon. A forwarder is STOPPED — it holds no
    /// state, and leaving one bound would collide when we reconnect to that host.
    func switchBackend(to backend: BackendID) {
        guard backend != activeBackend else { return }
        let target: BackendTarget
        switch backend {
        case .local:
            target = .local
        case let .remote(id):
            guard let host = hosts.first(where: { $0.id == id }) else {
                appModel?.reportBackendError("No host named \(id.rawValue) is configured.")
                return
            }
            // Probe first: refusing costs 3s, whereas switching to a dead host tears down a healthy
            // session and leaves the user with a red chip and nothing running.
            if let probe = try? hostRegistry?.probe(name: host.name), probe.reachable == false {
                appModel?.reportBackendError(
                    "Cannot reach \(host.name) at \(host.address). \(probe.error ?? "")")
                return
            }
            target = .remote(host)
        }

        let plan = backendPlan(target: target, sockets: sockets)
        guard let supervisor = supervisors[backend] ?? makeBackendSupervisor(plan: plan) else { return }

        if activeBackend == .local {
            supervisors[.local]?.detach()
        } else {
            supervisors[activeBackend]?.stop()
            supervisors[activeBackend] = nil
        }

        supervisors[backend] = supervisor
        if supervisor.state == .detached { supervisor.resume() } else { supervisor.start() }
        activeBackend = backend
        appModel?.reconnect(to: backend, makeTransport: {
            try UnixSocketConnection(path: plan.controlPath)
        })
        surfaceHost?.retarget(socketPath: plan.renderPath)
    }
}
```

Add `reportBackendError(_:)` to `AppModel` in Core (a one-line setter onto `store.lastError`, or a new
`@Published var backendError: String?` — pick whichever matches how `ContentView`'s existing banner reads
errors, and say which in your report).

Keep `applicationWillTerminate` terminating **every** supervisor, including a detached Local one — quit
means quit, per the design decision:

```swift
    func applicationWillTerminate(_ notification: Notification) {
        appModel?.shutdown()
        for (_, sup) in supervisors { sup.stop() }
    }
```

- [ ] **Step 4: Build**

Run: `cd macos && swift build`
Expected: success. Fix any call site the Core changes broke — in particular `ContentView(isRemote:)` still
takes a parameter Task 10 removes; leave `isRemote: activeBackend != .local` as a temporary argument here
if that is what makes it compile, and note it.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/ClowderApp/ProcessCommandRunner.swift \
        macos/Sources/ClowderApp/DaemonLaunch.swift macos/Sources/ClowderApp/App.swift
git commit -m "feat(app): supervise one backend per host and switch without killing agents"
```

---

### Task 10: The sidebar connection chip

**Files:**
- Create: `macos/Sources/ClowderApp/ConnectionChipView.swift`
- Modify: `macos/Sources/ClowderApp/ContentView.swift`, `macos/Sources/ClowderApp/App.swift`

- [ ] **Step 1: Write the chip view**

Create `macos/Sources/ClowderApp/ConnectionChipView.swift`:

```swift
import SwiftUI
import ClowderCore

/// The always-visible backend indicator at the bottom of the sidebar. A menu whose label says
/// which backend is live and how healthy it is, and whose contents switch to another.
struct ConnectionChipView: View {
    @EnvironmentObject private var model: AppModel
    /// The active backend's supervisor state, lifted from the delegate (Core has no access to it).
    let supervisorState: DaemonSupervisor.State
    let onRetry: () -> Void

    private var chip: ConnectionChip {
        connectionChip(backend: model.activeBackend, hosts: model.hosts,
                       connection: model.connectionState, supervisor: supervisorState)
    }

    var body: some View {
        Menu {
            Button {
                model.requestSwitch(to: .local)
            } label: {
                Label("Local", systemImage: model.activeBackend == .local ? "checkmark" : "")
            }
            .disabled(model.activeBackend == .local)

            if model.hosts.isEmpty {
                Divider()
                Text("No remote hosts configured")
            } else {
                Divider()
                ForEach(model.hosts) { host in
                    Button {
                        model.requestSwitch(to: host.backend)
                    } label: {
                        // An unpaired host still connects (trust-on-first-use) — say so rather
                        // than hiding it, so the user knows which hosts they have verified.
                        Text(host.isTrusted ? host.name : "\(host.name) — not paired")
                    }
                    .disabled(host.backend == model.activeBackend)
                }
            }

            if chip.canRetry {
                Divider()
                Button("Retry", action: onRetry)
            }

            Divider()
            SettingsLink { Text("Manage Hosts…") }
        } label: {
            HStack(spacing: 6) {
                Circle().fill(color(chip.tone)).frame(width: 7, height: 7)
                Image(systemName: chip.symbol).imageScale(.small)
                VStack(alignment: .leading, spacing: 0) {
                    Text(chip.title).font(.caption).lineLimit(1)
                    if let detail = chip.detail {
                        Text(detail).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                Spacer()
            }
            .contentShape(Rectangle())
        }
        .menuStyle(.borderlessButton)
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .onTapGesture { model.requestHostRefresh() }   // re-read the registry as the menu opens
    }

    private func color(_ tone: ChipTone) -> Color {
        switch tone {
        case .ok: return .green
        case .pending: return .secondary
        case .warning: return .orange
        case .error: return .red
        }
    }
}
```

`SettingsLink` requires macOS 14, which `Package.swift` already targets (`.macOS(.v14)`). The Settings
scene it opens does not exist until M11c — verify what happens when it is absent and say so in your
report; if it is a no-op that is acceptable for this milestone, but if it crashes, gate it behind a
`#available` check plus a `nil` scene guard and note that M11c removes the guard.

- [ ] **Step 2: Attach it to the sidebar and fix the stale `isRemote` flag**

In `macos/Sources/ClowderApp/ContentView.swift`:

- Delete the `let isRemote: Bool` property and its initializer argument.
- Replace the `AddProjectSheet(canBrowse: !isRemote)` usage with
  `AddProjectSheet(canBrowse: model.activeBackend == .local)`. This is the defect fix: the old flag was
  computed once in the scene body and never updated after a swap, so the file browser was offered for a
  remote backend (and hidden for a local one) depending on how the app started.
- Attach the chip to the **sidebar's `List`** (around line 87), not the `NavigationSplitView`:

```swift
        List(selection: $model.selection) {
            // … existing content …
        }
        .safeAreaInset(edge: .bottom) {
            Divider()
            ConnectionChipView(supervisorState: supervisorState, onRetry: onRetry)
        }
```

The existing `.safeAreaInset(edge: .bottom) { statusBar }` at line 39 is the **window-wide** error
banner and must stay exactly where it is — attaching the chip there would put it under the terminal
instead of under the sidebar, and would fight the banner for the same space.

Thread `supervisorState` and `onRetry` in from `App.swift`'s scene body, reading them from the delegate.

- [ ] **Step 3: Wire the palette's backend entries**

In `CommandPaletteView.swift`, pass the new arguments to `paletteResults` and handle the new item kind:

```swift
        let items = paletteResults(query: query, commands: CommandRegistry.all(keymap: keymap),
                                  worktrees: model.store.orderedWorktrees,
                                  hosts: model.hosts, activeBackend: model.activeBackend)
```

`CommandRegistry.all` takes a keymap — use whichever keymap the view already holds rather than
constructing a fresh one, so the palette's shortcut hints stay correct.

and in whatever `onSelect`/`activate` handler exists:

```swift
        case let .backend(id):
            model.requestSwitch(to: id)
            model.showingPalette = false
```

Read the file first — match its existing selection-handling shape rather than inventing one.

- [ ] **Step 4: Build and check the exhaustive switch**

Run: `cd macos && swift build`
Expected: success. Adding `.backend` to `PaletteItemKind` will break any exhaustive `switch` over it —
that is the compiler doing its job; handle the case rather than adding a `default`.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/ClowderApp/ConnectionChipView.swift macos/Sources/ClowderApp/ContentView.swift \
        macos/Sources/ClowderApp/CommandPaletteView.swift macos/Sources/ClowderApp/App.swift
git commit -m "feat(app): show the active backend in the sidebar and switch from the palette

Also fixes AddProjectSheet's browse affordance, which was decided once at
launch and never updated after a backend swap."
```

---

### Task 11: The menu-bar host list

**Files:**
- Modify: `macos/Sources/ClowderApp/StatusBarController.swift`, `macos/Sources/ClowderApp/App.swift`

**Why this surface matters most:** the app is menu-bar-resident — closing the window hides it rather than
quitting — so the tray is where a user actually lives. Today it shows a disabled `Local` / `Remote: <host>`
header plus one toggle.

- [ ] **Step 1: Collapse the three closures into one reference**

`StatusBarController` currently takes `remoteHost`, `configuredRemoteHost`, and `switchBackend` closures.
Replace all three with a single `BackendSwitching` reference, so the tray reads the same source as the
chip and the palette:

```swift
    private let backends: BackendSwitching
    private let appModel: AppModel

    init(appModel: AppModel,
         backends: BackendSwitching,
         showWindow: @escaping () -> Void) {
        self.appModel = appModel
        self.backends = backends
        self.showWindow = showWindow
        // … unchanged statusItem setup …
    }
```

- [ ] **Step 2: Build the host list in `menuNeedsUpdate`**

Replace the disabled header and single toggle (currently lines 79–93) with a checkmarked list. `NSMenuItem`
has native checkmark support via `state`, which carries the "which one is active" information the disabled
header used to spell out:

```swift
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        backends.refreshHosts()          // the registry may have changed in a shell

        let active = backends.activeBackend

        let local = addItem(to: menu, "Local", #selector(selectBackend(_:)))
        local.representedObject = BackendID.local
        local.state = active == .local ? .on : .off

        for host in backends.hosts {
            let title = host.isTrusted ? host.name : "\(host.name) — not paired"
            let item = addItem(to: menu, title, #selector(selectBackend(_:)))
            item.representedObject = host.backend
            item.state = host.backend == active ? .on : .off
        }
        if backends.hosts.isEmpty {
            let none = NSMenuItem(title: "No remote hosts configured", action: nil, keyEquivalent: "")
            none.isEnabled = false
            menu.addItem(none)
        }

        menu.addItem(.separator())

        // … the existing attention section, unchanged …
    }

    @objc private func selectBackend(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? BackendID else { return }
        backends.switchBackend(to: id)
    }
```

Delete `useLocalAction` and `useRemoteAction`. `BackendID` is an `enum` with an associated value, so it
cannot go into `representedObject` (an `Any?`) as-is on older toolchains — verify it boxes correctly; if
not, store the `String` description and map it back through `backends.hosts`, and say which you did.

- [ ] **Step 3: Update the construction site**

In `App.swift`'s `bootstrap()`:

```swift
        statusBar = StatusBarController(appModel: model,
                                        backends: self,
                                        showWindow: { [weak self] in self?.showWindow() })
```

- [ ] **Step 4: Build**

Run: `cd macos && swift build`
Expected: success.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/ClowderApp/StatusBarController.swift macos/Sources/ClowderApp/App.swift
git commit -m "feat(app): list every host in the menu bar with the active one checked"
```

---

### Task 12: Documentation and end-to-end verification

**Files:**
- Modify: `AGENTS.md`, `docs/remote-tls.md`, `README.md`

- [ ] **Step 1: Document the app's side of the feature**

In `AGENTS.md`'s Runtime model section, record that the app supervises **one backend per host**, that
switching Local→remote **detaches** rather than terminating so local agents survive, that quit still
terminates everything, and that the app passes `--socket-dir` so the forwarder's sockets live at
`<runtime>/clowder/remote/<host>/`. Note that `RemotePaths.swift` is gone and why.

In `docs/remote-tls.md`, replace the interim exit-4 paragraph M11a added: the supervisor now
understands code 4 and stops with a Retry instead of respawning every ~10s.

In `README.md`, mention that the app can switch between Local and configured remote hosts from the
sidebar chip, the menu bar, or the command palette.

- [ ] **Step 2: Run both suites**

```sh
source "$HOME/.cargo/env" && cargo test --workspace --locked
cd macos && swift test
```

Both must be green. Three `clowder-daemon` tests are known to flake under parallel load and pass on
re-run — re-run before investigating, and say so if you hit them.

- [ ] **Step 3: Build the app bundle**

```sh
scripts/build-app.sh
```

- [ ] **Step 4: Manual verification (the point of the milestone)**

Work through these against `dist/Clowder.app`, with a real second machine or a second daemon on a
loopback port. Paste the results into your report — this is the only place several of these behaviors are
checked at all.

1. **Empty registry.** With no hosts, the chip reads `Local`, the tray lists only `Local`, and neither
   offers a dead end.
2. **Add a host on the CLI**, then open the tray — it appears without restarting the app.
3. **Switch Local → remote with a local agent running.** Confirm the agent is *still running* after
   switching back (this is the defect this milestone fixes — before it, the agent was killed).
4. **Switch remote → Local with a remote agent running.** Confirm the remote agent survives.
5. **Selection restore.** Select a specific agent, switch away, switch back — the same agent is selected.
6. **Unreachable host.** Point a host at a dead address and switch to it: expect a refusal *before* the
   local session is torn down. Then make it unreachable *after* connecting and confirm the chip goes red
   with a Retry rather than spinning.
7. **Unpaired host** shows "— not paired" in both the tray and the chip menu.
8. **Palette.** `⌘K` (or the configured binding) offers "Connect to <host>" and switching works.
9. **Quit with a detached local daemon.** Quit the app, then confirm no `clowder-daemon` survives
   (`pgrep -fl clowder-daemon`) — quit means quit.
10. **Check the daemon log** at `~/.local/state/clowder/daemon.log` for anything alarming after all of
    the above; a GUI-launched app has no terminal, so this is the only place startup failures surface.

- [ ] **Step 5: Commit**

```bash
git add AGENTS.md docs/remote-tls.md README.md
git commit -m "docs(m11b): document backend switching and per-host forwarder sockets"
```

---

## Verification gate for M11b

- [ ] `cargo test --workspace --locked` and `cd macos && swift test` both green.
- [ ] `scripts/check-commit-messages.sh` passes for every commit.
- [ ] `scripts/build-app.sh` produces a bundle that launches.
- [ ] The chip names the active backend and its health; the tray and palette list every host with the
      active one marked.
- [ ] **Switching Local↔remote does not kill agents on either side**, and the previous selection is
      restored on return.
- [ ] An unreachable host is refused before the current session is torn down, and a host that dies while
      connected produces a red chip with a Retry rather than an endless reconnect.
- [ ] Quitting terminates every backend, including a detached local daemon.
- [ ] `remote_known_hosts` survives 16 concurrent first-sight records with no lost lines.
- [ ] `RemotePaths.swift` is gone and the forwarder's socket path has exactly one authority.

## Deferred to M11c

`HostDraft` and its shared-fixture validation, the `Settings` scene and Hosts pane, the add/edit/remove
editor, and the fingerprint-confirm pairing sheet. Until M11c, hosts are managed on the CLI and the chip's
"Manage Hosts…" item has nothing to open — Task 10 Step 1 asks you to confirm how `SettingsLink` behaves
with no Settings scene and to guard it if needed.

## Also deferred (recorded from M11a's whole-branch review)

- `trust` on a `tls: false` entry reports `trusted: true` though the pin can never be checked.
- `set --address` leaves a stale `remote_known_hosts` line for the old address.
- `cmd_probe` silently ignores `--address` when a positional name is also given.
- The `--json` raw-arg scan is an exact-token match, so `--json=true` renders as plain stderr text.
- The env-var set/remove-at-tail test pattern leaks `XDG_STATE_HOME` to sibling tests on a mid-test
  panic; an RAII guard would close it for all six affected tests.

## Self-review notes

Checked against the spec's §6 (the app) and M11a's four handoff items:

- Spec §6 Core types — Tasks 2, 3, 4, 5, 6, 7, 8. Spec §6 App wiring — Tasks 9, 10, 11.
- M11a handoff 1 (`--socket-dir`) — Task 4 + Task 9. Handoff 2 (exit 4) — Task 5. Handoff 3
  (known-hosts atomicity) — Task 1. Handoff 4 (shared name fixture) — explicitly M11c.
- Both named existing defects have owners: agent-killing switch (Tasks 5 + 9), stale `isRemote`
  (Task 10).
- Names are consistent across tasks: `HostID`/`BackendID`/`HostSource`/`RemoteHost`/`HostProbe`/
  `ListOutput`/`ProbeOutput` (Task 2), `CommandRunner`/`CommandResult`/`HostRegistry`/`TokenEdit`/
  `HostRegistryError` (Task 3), `SocketPaths`/`BackendTarget`/`BackendExecutable`/`BackendPlan`/
  `backendPlan`/`forwarderSocketDir` (Task 4), `detach`/`resume`/`.detached`/`.failed`/`isRunning`
  (Task 5), `activeBackend`/`reconnect(to:makeTransport:)`/`BackendSwitching`/`setHosts`/
  `requestSwitch`/`requestHostRefresh` (Task 6), `ChipTone`/`ConnectionChip`/`connectionChip` (Task 7),
  `.backend(BackendID)` (Task 8).
- Three places deliberately ask the implementer to read the existing code and report what they found
  rather than trusting this plan: `WorktreeInfo`'s initializer (Task 8), how `ContentView`'s banner
  reads errors (Task 9), and `SettingsLink`'s behavior with no Settings scene (Task 10). M11a's
  execution showed the plan's confident tone outran its accuracy in exactly this kind of detail.
