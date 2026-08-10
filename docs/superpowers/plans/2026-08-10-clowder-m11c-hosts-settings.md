# M11c — the Settings window and the pairing sheet

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended)
> or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax
> for tracking.

**Goal:** Let a user add, edit, remove and **pair** remote hosts from the app, instead of dropping to the
CLI. This is the last milestone of M11.

**Architecture:** A `Settings` scene (⌘,) — the app has never had one — hosting a Hosts pane. Every
decision lives in `ClowderCore`: `HostDraft` validates input against the same fixture the Rust validator
is checked against, and `HostsViewModel` owns all state and operations, driven end-to-end in `swift test`
by the existing `FakeCommandRunner`. `ClowderApp` gets four SwiftUI views that render and nothing else.
The pairing flow probes **off the main thread** — a change from M11b, where the probe blocked it.

**Tech Stack:** Swift 5.9 / SwiftPM, macOS 14+, XCTest, SwiftUI. No new dependencies. No Rust changes.

**Branch:** `feat/m11c-hosts-settings`, based on **`feat/m11b-app-hosts`** — a stacked PR targeting that
branch, not `main`.

**Spec:** `docs/superpowers/specs/2026-08-07-clowder-m11-remote-host-management-design.md`
**Predecessors:** M11a (PR #74), M11b (PR #75) — both CI-green and manually verified.

## Global Constraints

- **Swift lives in `macos/`.** `cd macos && swift test` runs `ClowderCore`'s XCTest suite **and compiles
  `ClowderApp`** — a compile error there aborts the run before any test executes. It does not need the
  vendored libghostty (it never links the executable); `swift build` does. See `AGENTS.md`, corrected
  2026-08-08.
- **Every task must leave the whole package compiling.** A Core change that breaks an App call site must
  fix that call site in the same commit. This repo merges feature branches, so every commit lands
  individually in `main` and a non-building commit is a `git bisect` hazard.
- **`ClowderApp` has no test target.** Every branch, derivation and decision belongs in `ClowderCore`. The
  acceptance criterion for each App file is "contains no branch a test could meaningfully cover".
- **The token never enters Swift at rest.** `clowder remote list --json` emits `hasToken: Bool`; a token
  is only ever written *out*, via `--token-stdin`. Nothing may display, log, or store one.
- **Tests must be able to fail.** Break the behavior a test targets, run it, **capture the runner's actual
  output**, revert, and paste it. Across M11a and M11b, six tests reached review unable to fail and three
  reports claimed verifications that had not happened. Reasoning about a test's power is not verification.
- **Ignore stale editor/SourceKit diagnostics** ("No such module", "cannot find type"). They appear
  constantly on these branches. Trust `swift test` / `swift build`.
- **Conventional Commits** — the type drives the released version (`feat` → minor, `fix`/`perf` → patch,
  `test`/`docs` → no release). Run `scripts/check-commit-messages.sh feat/m11b-app-hosts HEAD`.
- **Run commands in the foreground.** Do not arm background monitors — two agents stalled that way in
  M11a. **Do not change machine state outside the repository.**

## What M11b handed over

Three carried items, all discharged here:

1. **Swift's host-name validator must be driven by `docs/protocol/fixtures/host-names.json`** — the last
   of M11a's four handoffs. Task 2.
2. **Restore the `SettingsLink`** that M11b removed from the connection chip. It pointed at a `Settings`
   scene that did not exist; the spec described the chip as having it while scheduling the scene a
   milestone later. Task 8.
3. **`HostRegistry.remove`/`untrust` have no argv tests** and no callers until now. `FakeCommandRunner`
   returns success by default, so a wrong argv would be invisible. Task 1.

Also folded in: **the probe moves off the main thread** (Task 1). M11b bounded it to `--timeout 1` but
left it synchronous on `@MainActor`; a pairing sheet that probes on appear makes that unacceptable.

## Verified facts this plan is built on

Checked against source on 2026-08-10 — the recurring defect across M11a/M11b was plan code with wrong API
shapes, so these are stated rather than assumed:

- `HostRegistry` is a `public struct` with `init(runner: CommandRunner)` and methods `list()`,
  `show(name:)`, `add(name:address:token:tls:)`, `update(name:rename:address:token:tls:)`,
  `remove(name:)`, `probe(name:timeoutSeconds:)` (default 3), `probe(address:token:tls:timeoutSeconds:)`,
  `trust(name:fingerprint:)`, `untrust(name:)`. It is **not** declared `Sendable` — Task 1 adds it.
- `CommandRunner` is `AnyObject, Sendable` with `run(_ args: [String], stdin: String?) throws -> CommandResult`.
- `RemoteHost` has `name`, `address`, `tls`, `hasToken`, `fingerprint`, `trusted`, `source`, plus
  `id: HostID`, `backend: BackendID`, `isTrusted` (returns the decoded `trusted`), `isEditable`
  (`source == .registry`).
- `HostProbe` has `fingerprintMatch: FingerprintMatch?` and `authSummary: AuthSummary`
  (`.nonePlaintext` / `.tokenAccepted` / `.tokenRejected`).
- `TokenEdit` is `.unchanged` / `.clear` / `.set(String)`.
- `SheetForms.swift` already holds `AddProjectForm` and `NewWorktreeForm`; `SheetFormsTests.swift` already
  has the fixture-driving pattern to copy (it resolves `docs/protocol/fixtures/` from `#filePath`).
- `ClowderApp.body` is a lone `WindowGroup` plus `.commands`. There is no `Settings` scene.
- `AppDelegate.hostRegistry` is **private**; Task 8 must expose what the Settings scene needs.

**A trap worth naming:** `NewWorktreeForm.nameError` is a *different, stricter* validator than the one host
names need. Rust's `validate_name` (`crates/clowder-config/src/hosts.rs`) allows `...` and `a..b` and has
no `.lock` rule; the worktree validator rejects both. The fixture contains `{"...": true}` and
`{"a..b": true}` precisely to catch a copy-paste. Do not reuse `NewWorktreeForm`'s body.

## File structure

| File | Responsibility |
|---|---|
| `macos/Sources/ClowderCore/HostRegistry.swift` | `Sendable` conformance (Task 1) |
| `macos/Sources/ClowderCore/SheetForms.swift` | `HostDraft` alongside the two existing forms |
| `macos/Sources/ClowderCore/HostsViewModel.swift` (new) | all Settings-pane state and operations |
| `macos/Sources/ClowderApp/SettingsView.swift` (new) | the `TabView` shell |
| `macos/Sources/ClowderApp/HostsSettingsView.swift` (new) | master list + `+`/`−` footer |
| `macos/Sources/ClowderApp/HostEditorView.swift` (new) | the per-host `Form` |
| `macos/Sources/ClowderApp/PairingSheet.swift` (new) | probe → compare → trust |
| `macos/Sources/ClowderApp/App.swift` | the `Settings` scene; expose the view model |
| `macos/Sources/ClowderApp/ConnectionChipView.swift` | restore `SettingsLink` |

---

### Task 1: `HostRegistry` — `Sendable`, argv tests, and an off-main probe

**Files:**
- Modify: `macos/Sources/ClowderCore/HostRegistry.swift`,
  `macos/Tests/ClowderCoreTests/HostRegistryTests.swift`

**Interfaces:**
- Produces: `HostRegistry: Sendable`; `HostRegistry.probeAsync(name:timeoutSeconds:) async throws -> HostProbe`
  and `probeAsync(address:token:tls:timeoutSeconds:) async throws -> HostProbe`.

**Why async:** `HostsViewModel` is `@MainActor` and the pairing sheet probes on appear. `probe` shells out
to `clowder remote probe`, which bounds each of connect/handshake/read-line separately — up to ~3× the
timeout. Blocking the main actor there beachballs the Settings window. Moving it off is the fix M11b
deferred.

- [ ] **Step 1: Write the failing tests**

Append to `macos/Tests/ClowderCoreTests/HostRegistryTests.swift`:

```swift
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cd macos && swift test --filter HostRegistryTests`
Expected: FAIL — `value of type 'HostRegistry' has no member 'probeAsync'`. The two argv tests may
**pass** immediately if the argv happens to be right; that is fine and expected — they are regression
guards for code that has no callers yet, not TDD drivers. Say so in your report rather than pretending
they went red.

- [ ] **Step 3: Implement**

Mark the type `Sendable` and add the async wrappers:

```swift
/// Reads and writes the host registry by driving `clowder remote …`.
///
/// `Sendable` because the pairing flow runs `probeAsync` off the main actor: the CLI bounds each of
/// connect / TLS handshake / read-line by the timeout separately, so a probe can take ~3× it, and
/// blocking `@MainActor` for that long freezes the Settings window.
public struct HostRegistry: Sendable {
```

Then, alongside the existing synchronous `probe` methods:

```swift
    /// `probe(name:timeoutSeconds:)` off the calling actor.
    ///
    /// The work is a blocking subprocess call, so it goes to a detached task rather than merely being
    /// `async` — an `async` function that never suspends would still run on the caller's executor.
    public func probeAsync(name: String, timeoutSeconds: Int = 3) async throws -> HostProbe {
        let registry = self
        return try await Task.detached(priority: .userInitiated) {
            try registry.probe(name: name, timeoutSeconds: timeoutSeconds)
        }.value
    }

    /// `probe(address:token:tls:timeoutSeconds:)` off the calling actor. Used by "Test" before a host
    /// is saved, so the token is still in hand and still goes via stdin.
    public func probeAsync(address: String, token: String?, tls: Bool,
                           timeoutSeconds: Int = 3) async throws -> HostProbe {
        let registry = self
        return try await Task.detached(priority: .userInitiated) {
            try registry.probe(address: address, token: token, tls: tls, timeoutSeconds: timeoutSeconds)
        }.value
    }
```

If the compiler rejects `Sendable` because `CommandRunner` is not sufficiently constrained, **report what
it said** rather than reaching for `@unchecked Sendable` — the protocol already declares `AnyObject, Sendable`,
so a rejection would mean something else is wrong and worth understanding.

- [ ] **Step 4: Run to verify the tests pass**

Run: `cd macos && swift test`
Expected: PASS — 188 existing + 5 new = 193.

- [ ] **Step 5: Demonstrate the argv tests can fail**

Change `remove`'s args to `["remote", "remove", name, "--json"]` (a plausible wrong spelling — the CLI
subcommand is `rm`), run, confirm `testRemoveSendsTheExpectedArguments` fails, **capture the output**,
revert. These tests exist precisely because nothing else would catch that.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/HostRegistry.swift macos/Tests/ClowderCoreTests/HostRegistryTests.swift
git commit -m "feat(app): probe off the main actor and pin remove/untrust arguments"
```

---

### Task 2: `HostDraft` — the last cross-language validator

**Files:**
- Modify: `macos/Sources/ClowderCore/SheetForms.swift`,
  `macos/Tests/ClowderCoreTests/SheetFormsTests.swift`

**Interfaces:**
- Produces: `HostDraft { name, address, tls, token: String?, isNew }` with `nameError`, `addressError`,
  `isValid`.

**This discharges the last of M11a's four handoffs.** `docs/protocol/fixtures/host-names.json` has been
checked against Rust's `validate_name` since M11a; this makes it check the Swift side too, so the two
cannot drift.

**Do not reuse `NewWorktreeForm.nameError`.** It is stricter: it rejects a `..` substring and a `.lock`
suffix, neither of which applies to host names. The fixture contains `"..."` → valid and `"a..b"` → valid
specifically to catch that copy-paste.

- [ ] **Step 1: Write the failing tests**

Append to `macos/Tests/ClowderCoreTests/SheetFormsTests.swift`:

```swift
final class HostDraftTests: XCTestCase {
    private func fixtureCases(_ name: String, file: StaticString = #filePath) throws -> [(String, Bool)] {
        struct Case: Decodable { let name: String; let valid: Bool }
        let here = URL(fileURLWithPath: "\(file)")
        let repo = here.deletingLastPathComponent()   // ClowderCoreTests
            .deletingLastPathComponent()              // Tests
            .deletingLastPathComponent()              // macos
            .deletingLastPathComponent()              // repo root
        let data = try Data(contentsOf: repo.appendingPathComponent("docs/protocol/fixtures/\(name)"))
        return try JSONDecoder().decode([Case].self, from: data).map { ($0.name, $0.valid) }
    }

    func testNameAgreesWithTheSharedFixture() throws {
        let cases = try fixtureCases("host-names.json")
        XCTAssertFalse(cases.isEmpty, "fixture must not be empty")
        for (name, valid) in cases {
            var draft = HostDraft()
            draft.name = name
            XCTAssertEqual(draft.nameError == nil, valid,
                           "disagreed on \(name.debugDescription) — if you changed a rule, update the "
                           + "shared cases AND clowder_config::hosts::validate_name")
        }
    }

    func testHostNamesAreNotValidatedLikeWorktreeNames() {
        // The two validators are deliberately different. `...` and `a..b` are fine host names but are
        // rejected as worktree names; conflating them would break hosts the CLI accepts.
        for good in ["...", "a..b"] {
            var draft = HostDraft(); draft.name = good
            XCTAssertNil(draft.nameError, "\(good) is a valid HOST name")
            XCTAssertNotNil(NewWorktreeForm(projectPath: "/p", name: good, adapter: "claude").nameError,
                            "\(good) should still be an invalid WORKTREE name")
        }
    }

    func testNameErrorNamesTheProblem() {
        var draft = HostDraft(); draft.name = "has space"
        XCTAssertTrue(draft.nameError?.contains("letters") == true, "unhelpful: \(draft.nameError ?? "nil")")
        draft.name = ".."
        XCTAssertTrue(draft.nameError?.contains("'..'") == true, "unhelpful: \(draft.nameError ?? "nil")")
    }

    func testAddressRequiresAHostAndAPort() {
        for good in ["h:7777", "10.0.0.5:1", "studio.tail1234.ts.net:7777", "[::1]:7777", "[fd7a::1]:22"] {
            var draft = HostDraft(); draft.address = good
            XCTAssertNil(draft.addressError, "\(good) should be valid")
        }
        for bad in ["", "h", "h:", ":7777", "h:0", "h:70000", "h:abc", "::1:7777", "[::1]7777", "a b:7777"] {
            var draft = HostDraft(); draft.address = bad
            XCTAssertNotNil(draft.addressError, "\(bad) should be invalid")
        }
    }

    func testIsValidRequiresBothFields() {
        var draft = HostDraft()
        XCTAssertFalse(draft.isValid, "an empty draft is not valid")
        draft.name = "studio"
        XCTAssertFalse(draft.isValid, "a name alone is not valid")
        draft.address = "s:7777"
        XCTAssertTrue(draft.isValid)
        draft.name = "bad name"
        XCTAssertFalse(draft.isValid)
    }

    func testATokenImpliesTLSIsRequired() {
        // The CLI refuses a token without TLS at add/set time; say so before the user submits.
        var draft = HostDraft()
        draft.name = "studio"; draft.address = "s:7777"
        draft.token = "s3cr3t"; draft.tls = false
        XCTAssertFalse(draft.isValid)
        XCTAssertNotNil(draft.tlsError)
        draft.tls = true
        XCTAssertTrue(draft.isValid)
        XCTAssertNil(draft.tlsError)
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd macos && swift test --filter HostDraftTests`
Expected: FAIL — `cannot find 'HostDraft' in scope`.

- [ ] **Step 3: Implement**

Append to `macos/Sources/ClowderCore/SheetForms.swift`:

```swift
/// The Hosts pane's editor state.
///
/// `nameError` mirrors `clowder_config::hosts::validate_name` and is checked against the same
/// `docs/protocol/fixtures/host-names.json` the Rust validator is, so the two cannot drift. The CLI
/// remains the authority — a value that slips through here still gets a clean error back.
///
/// NOTE: host names are validated **differently** from worktree names. `validate_name` allows `...`
/// and `a..b` and has no `.lock` rule; `NewWorktreeForm.nameError` rejects all three. Do not merge them.
public struct HostDraft: Equatable, Sendable {
    public var name: String
    public var address: String
    public var tls: Bool
    /// A token the user has typed. `nil` means "unchanged" for an existing host — the app never reads
    /// a stored token back, only writes one.
    public var token: String?
    /// True when this draft creates a host rather than editing one.
    public var isNew: Bool

    public init(name: String = "", address: String = "", tls: Bool = false,
                token: String? = nil, isNew: Bool = true) {
        self.name = name
        self.address = address
        self.tls = tls
        self.token = token
        self.isNew = isNew
    }

    private static let maxName = 64

    /// Nil when acceptable; otherwise a user-facing reason. Validates the value AS TYPED — no trimming
    /// — so it agrees with the Rust validator, which also does not trim.
    public var nameError: String? {
        if name.isEmpty { return "Name must not be empty" }
        // Count Unicode scalars, matching Rust's `chars().count()`. (For any name that passes the
        // charset check below the two counts are identical, since it is ASCII by then — but matching
        // the Rust rule exactly is cheaper than reasoning about when it matters.)
        if name.unicodeScalars.count > Self.maxName {
            return "Name must be \(Self.maxName) characters or fewer"
        }
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "._-"))
        if name.unicodeScalars.contains(where: { !allowed.contains($0) || !$0.isASCII }) {
            return "Name may contain only letters, digits, '.', '_' or '-'"
        }
        // '.' is allowed above so "a.b" works, which lets these two through the charset check. The
        // name becomes a socket directory (`<runtime>/clowder/remote/<name>/`), so '..' escapes it.
        if name == "." || name == ".." { return "Name must not be '.' or '..'" }
        return nil
    }

    /// Nil when acceptable. Requires an explicit port — there is no default to fall back on, since the
    /// daemon's listen address is operator-chosen.
    public var addressError: String? {
        if address.isEmpty { return "Address must not be empty" }
        if address.unicodeScalars.contains(where: { CharacterSet.whitespacesAndNewlines.contains($0) }) {
            return "Address must not contain spaces"
        }
        guard let (host, port) = Self.splitHostPort(address), !host.isEmpty else {
            return "Address must be host:port (or [ipv6]:port)"
        }
        guard let n = UInt16(port), n != 0 else { return "Address must end in a valid port" }
        return nil
    }

    /// Nil when acceptable. A token is only ever sent over TLS — the CLI refuses the combination, so
    /// say so before the user submits rather than after.
    public var tlsError: String? {
        let hasToken = !(token ?? "").isEmpty
        return hasToken && !tls ? "A token requires TLS — turn on Use TLS, or clear the token" : nil
    }

    public var isValid: Bool { nameError == nil && addressError == nil && tlsError == nil }

    /// Split `host:port` / `[v6]:port`. Nil when there is no port, or when a bare (unbracketed) IPv6
    /// literal makes the split ambiguous.
    private static func splitHostPort(_ s: String) -> (String, String)? {
        if s.hasPrefix("[") {
            guard let close = s.firstIndex(of: "]") else { return nil }
            let host = String(s[s.index(after: s.startIndex)..<close])
            let rest = s[s.index(after: close)...]
            guard rest.hasPrefix(":") else { return nil }
            return (host, String(rest.dropFirst()))
        }
        guard let colon = s.lastIndex(of: ":") else { return nil }
        let host = String(s[s.startIndex..<colon])
        if host.contains(":") { return nil }   // bare v6 literal — require brackets
        return (host, String(s[s.index(after: colon)...]))
    }
}
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `cd macos && swift test`
Expected: PASS — 193 + 6 = 199.

- [ ] **Step 5: Demonstrate the fixture test can fail**

Change `nameError` to reject a `..` substring (the worktree rule), run, and confirm
`testNameAgreesWithTheSharedFixture` fails on `"a..b"` and `"..."`. **Capture the output**, revert. This
is the exact copy-paste the fixture exists to catch, so proving it catches it is the point.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/SheetForms.swift macos/Tests/ClowderCoreTests/SheetFormsTests.swift
git commit -m "feat(app): validate host drafts against the shared name fixture"
```

---

### Task 3: `HostsViewModel` — list, select, edit, remove

**Files:**
- Create: `macos/Sources/ClowderCore/HostsViewModel.swift`
- Test: `macos/Tests/ClowderCoreTests/HostsViewModelTests.swift`

**Interfaces:**
- Consumes: `HostRegistry` (Task 1), `HostDraft` (Task 2), `RemoteHost`, `HostID`, `TokenEdit`.
- Produces: `HostsViewModel` with `hosts`, `selected`, `draft`, `lastError`, `isBusy`; `reload()`,
  `select(_:)`, `beginAdd()`, `save()`, `remove(_:)`, `dismissError()`; and an `onHostsChanged` callback.

**Why `onHostsChanged`:** adding a host in Settings must make it appear in the sidebar chip, the menu bar
and the palette. Those read `AppModel.hosts`. The view model cannot import the app, so it publishes
through an injected callback that `AppDelegate` wires to `refreshHosts()`.

**Removing the active host is refused.** `BackendID` *is* the host name, so removing the host you are
connected to leaves the app pointed at an id that no longer resolves — the chip degrades to "not in your
host list" and there is no way back to it. Refusing with a clear message is better than a broken state.

- [ ] **Step 1: Write the failing tests**

Create `macos/Tests/ClowderCoreTests/HostsViewModelTests.swift`:

```swift
import XCTest
@testable import ClowderCore

@MainActor
final class HostsViewModelTests: XCTestCase {
    private let twoHostsJSON = """
    {"hosts":[
      {"name":"studio","address":"s:7777","tls":true,"hasToken":true,"fingerprint":"a1b2","trusted":true,"source":"registry"},
      {"name":"config","address":"c:7777","tls":false,"hasToken":false,"fingerprint":null,"trusted":false,"source":"config"}
    ]}
    """
    private let oneHostJSON = #"{"name":"studio","address":"s:7777","tls":true,"hasToken":true,"fingerprint":null,"trusted":false,"source":"registry"}"#

    private func model(_ fake: FakeCommandRunner,
                      activeBackend: BackendID = .local,
                      onChanged: (() -> Void)? = nil) -> HostsViewModel {
        HostsViewModel(registry: HostRegistry(runner: fake),
                       activeBackend: { activeBackend },
                       onHostsChanged: { onChanged?() })
    }

    func testReloadPopulatesHosts() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        XCTAssertEqual(m.hosts.map(\.name), ["studio", "config"])
        XCTAssertNil(m.lastError)
    }

    func testReloadSurfacesAnErrorAndLeavesHostsAlone() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        fake.results = [.failed(#"{"error":"registry unreadable"}"#)]
        await m.reload()
        XCTAssertEqual(m.hosts.map(\.name), ["studio", "config"], "a failed reload must not blank the list")
        XCTAssertTrue(m.lastError?.contains("unreadable") == true, m.lastError ?? "nil")
    }

    func testSelectingAHostFillsTheDraftWithoutTheToken() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        let draft = try! XCTUnwrap(m.draft)
        XCTAssertEqual(draft.name, "studio")
        XCTAssertEqual(draft.address, "s:7777")
        XCTAssertTrue(draft.tls)
        XCTAssertFalse(draft.isNew)
        // The app never reads a stored token back — only ever writes one.
        XCTAssertNil(draft.token)
    }

    func testBeginAddStartsAnEmptyNewDraft() {
        let m = model(FakeCommandRunner())
        m.beginAdd()
        let draft = try! XCTUnwrap(m.draft)
        XCTAssertTrue(draft.isNew)
        XCTAssertEqual(draft.name, "")
        XCTAssertNil(m.selected)
    }

    func testSavingANewHostCallsAddThenReloadsAndNotifies() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(oneHostJSON), .ok(twoHostsJSON)]
        var notified = 0
        let m = model(fake, onChanged: { notified += 1 })
        m.beginAdd()
        m.draft?.name = "studio"
        m.draft?.address = "s:7777"
        m.draft?.tls = true
        m.draft?.token = "s3cr3t"
        await m.save()

        let add = fake.invocations[0]
        XCTAssertEqual(add.args.prefix(4).map { $0 }, ["remote", "add", "studio", "s:7777"])
        XCTAssertEqual(add.stdin, "s3cr3t", "the token goes on stdin")
        XCTAssertFalse(add.args.contains("s3cr3t"), "never in argv: \(add.args)")
        XCTAssertEqual(fake.invocations[1].args, ["remote", "list", "--json"], "save must reload")
        XCTAssertEqual(notified, 1, "the chip/tray/palette must be told")
        XCTAssertNil(m.lastError)
    }

    func testSavingAnExistingHostCallsSetAndLeavesAnUntypedTokenUnchanged() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(oneHostJSON), .ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        m.draft?.address = "moved:9999"
        await m.save()

        let set = fake.invocations[1]
        XCTAssertEqual(set.args.prefix(3).map { $0 }, ["remote", "set", "studio"])
        XCTAssertTrue(set.args.contains("--address"))
        XCTAssertTrue(set.args.contains("moved:9999"))
        XCTAssertFalse(set.args.contains("--token-stdin"), "an untouched token must not be rewritten")
        XCTAssertFalse(set.args.contains("--no-token"), "an untouched token must not be cleared")
        XCTAssertNil(set.stdin)
    }

    func testSavingAnInvalidDraftDoesNothing() async {
        let fake = FakeCommandRunner()
        let m = model(fake)
        m.beginAdd()
        m.draft?.name = "bad name"
        m.draft?.address = "s:7777"
        await m.save()
        XCTAssertTrue(fake.invocations.isEmpty, "an invalid draft must not reach the CLI")
        XCTAssertNotNil(m.lastError)
    }

    func testRemovingTheActiveHostIsRefused() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake, activeBackend: .remote(HostID("studio")))
        await m.reload()
        await m.remove(HostID("studio"))
        XCTAssertEqual(fake.invocations.count, 1, "only the reload — no rm should have run")
        XCTAssertTrue(m.lastError?.lowercased().contains("connected") == true, m.lastError ?? "nil")
    }

    func testRemovingAnInactiveHostCallsRmAndReloads() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok("{}"), .ok(twoHostsJSON)]
        var notified = 0
        let m = model(fake, onChanged: { notified += 1 })
        await m.reload()
        await m.remove(HostID("studio"))
        XCTAssertEqual(fake.invocations[1].args, ["remote", "rm", "studio", "--json"])
        XCTAssertEqual(fake.invocations[2].args, ["remote", "list", "--json"])
        XCTAssertEqual(notified, 1)
    }

    func testAConfigSourcedHostIsNotEditable() async {
        let fake = FakeCommandRunner(); fake.results = [.ok(twoHostsJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("config"))
        XCTAssertFalse(m.canEditSelection, "[remote] host lives in config.toml and is read-only")
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd macos && swift test --filter HostsViewModelTests`
Expected: FAIL — `cannot find 'HostsViewModel' in scope`.

- [ ] **Step 3: Implement**

Create `macos/Sources/ClowderCore/HostsViewModel.swift`:

```swift
import Foundation
import Combine

/// All state and operations behind the Settings window's Hosts pane.
///
/// Lives in `ClowderCore` because `ClowderApp` has no test target — every decision here is driven in
/// `swift test` by a fake `CommandRunner`. The views render this and nothing else.
@MainActor
public final class HostsViewModel: ObservableObject {
    @Published public private(set) var hosts: [RemoteHost] = []
    @Published public private(set) var selected: HostID?
    /// The editor's live state. Nil when nothing is selected.
    @Published public var draft: HostDraft?
    @Published public private(set) var lastError: String?
    /// True while a CLI call is in flight, so the view can disable its controls.
    @Published public private(set) var isBusy = false

    private let registry: HostRegistry
    /// Which backend the app is connected to, so removing it can be refused.
    private let activeBackend: () -> BackendID
    /// Told after any change that alters the host list, so the chip, tray and palette refresh.
    private let onHostsChanged: () -> Void

    public init(registry: HostRegistry,
                activeBackend: @escaping () -> BackendID,
                onHostsChanged: @escaping () -> Void) {
        self.registry = registry
        self.activeBackend = activeBackend
        self.onHostsChanged = onHostsChanged
    }

    /// The selected host, if it still exists.
    public var selectedHost: RemoteHost? {
        selected.flatMap { id in hosts.first { $0.id == id } }
    }

    /// `[remote] host` entries live in `config.toml`, which clowder never rewrites.
    public var canEditSelection: Bool { selectedHost?.isEditable ?? false }

    public func dismissError() { lastError = nil }

    public func reload() async {
        await run {
            // Assign only on success: a failed reload must not blank a list the user is looking at.
            self.hosts = try self.registry.list()
        }
    }

    public func select(_ id: HostID?) {
        selected = id
        guard let host = id.flatMap({ i in hosts.first { $0.id == i } }) else {
            draft = nil
            return
        }
        // `token` stays nil: the app never reads a stored token back, only writes one.
        draft = HostDraft(name: host.name, address: host.address, tls: host.tls,
                          token: nil, isNew: false)
    }

    public func beginAdd() {
        selected = nil
        draft = HostDraft()
    }

    public func save() async {
        guard var draft else { return }
        guard draft.isValid else {
            lastError = draft.nameError ?? draft.addressError ?? draft.tlsError
            return
        }
        let typedToken = (draft.token?.isEmpty == false) ? draft.token : nil
        let isNew = draft.isNew
        let originalName = selected?.rawValue

        await run {
            if isNew {
                _ = try self.registry.add(name: draft.name, address: draft.address,
                                          token: typedToken, tls: draft.tls)
            } else {
                guard let originalName else { return }
                _ = try self.registry.update(
                    name: originalName,
                    rename: draft.name == originalName ? nil : draft.name,
                    address: draft.address,
                    // Only `.set` when the user actually typed one. `.unchanged` is what keeps an
                    // existing token intact through an unrelated edit.
                    token: typedToken.map { .set($0) } ?? .unchanged,
                    tls: draft.tls
                )
            }
            self.hosts = try self.registry.list()
            self.onHostsChanged()
        }
        // Clear the typed token so it does not linger in memory or get re-sent on the next save.
        draft.token = nil
        self.draft = draft
        if !isNew { selected = HostID(draft.name) } else { select(HostID(draft.name)) }
    }

    public func remove(_ id: HostID) async {
        // BackendID *is* the host name, so removing the connected host would leave the app pointed at
        // an id that no longer resolves, with no way back to it.
        if activeBackend() == .remote(id) {
            lastError = "You are connected to \(id.rawValue). Switch to another backend before removing it."
            return
        }
        await run {
            try self.registry.remove(name: id.rawValue)
            self.hosts = try self.registry.list()
            self.onHostsChanged()
        }
        if selected == id { select(nil) }
    }

    /// Run a CLI-touching operation with busy state and uniform error surfacing. Task 4's pairing
    /// operations use this too, so it stays file-private rather than becoming API.
    private func run(_ body: @escaping () throws -> Void) async {
        isBusy = true
        lastError = nil
        defer { isBusy = false }
        do {
            try body()
        } catch {
            lastError = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
        }
    }
}
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `cd macos && swift test`
Expected: PASS — 199 + 10 = 209.

- [ ] **Step 5: Demonstrate two tests can fail**

Break `remove` to drop the active-backend guard and confirm `testRemovingTheActiveHostIsRefused` fails.
Then change `save`'s `.unchanged` to `.clear` and confirm
`testSavingAnExistingHostCallsSetAndLeavesAnUntypedTokenUnchanged` fails. **Capture both outputs**, revert.
The second is the one that matters — silently clearing a token on an unrelated edit would break a working
host and be very hard to diagnose.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/HostsViewModel.swift macos/Tests/ClowderCoreTests/HostsViewModelTests.swift
git commit -m "feat(app): add the Hosts pane's view model"
```

---

### Task 4: `HostsViewModel` — the pairing flow

**Files:**
- Modify: `macos/Sources/ClowderCore/HostsViewModel.swift`,
  `macos/Tests/ClowderCoreTests/HostsViewModelTests.swift`

**Interfaces:**
- Produces: `HostsViewModel.PairingState`, `pairing`, `beginPairing()`, `confirmTrust()`,
  `cancelPairing()`, `expectedFingerprint`.

**Why the sheet must name an out-of-band source.** Probing and trusting are separate acts with a human in
between; if the user does not compare the fingerprint against something the daemon told them directly,
pairing is trust-on-first-use with extra clicks. The model exposes an `expectedFingerprint` the user can
paste, and refuses to trust on mismatch — so the comparison is done by software, not by eye.

- [ ] **Step 1: Write the failing tests**

Append to `HostsViewModelTests`:

```swift
    private let probeNewJSON = #"{"probe":{"name":"studio","address":"s:7777","reachable":true,"tls":true,"fingerprint":"a1b2","pinnedFingerprint":null,"fingerprintMatch":"new","authenticated":true,"error":null}}"#
    private let probeUnreachableJSON = #"{"probe":{"name":"studio","address":"s:7777","reachable":false,"tls":true,"fingerprint":null,"pinnedFingerprint":null,"fingerprintMatch":null,"authenticated":false,"error":"connection refused"}}"#
    private let probePlaintextJSON = #"{"probe":{"name":"studio","address":"s:7777","reachable":true,"tls":false,"fingerprint":null,"pinnedFingerprint":null,"fingerprintMatch":null,"authenticated":true,"error":null}}"#

    func testBeginPairingProbesAndOffersTheFingerprint() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probeNewJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        guard case let .observed(probe) = m.pairing else {
            return XCTFail("expected .observed, got \(m.pairing)")
        }
        XCTAssertEqual(probe.fingerprint, "a1b2")
        XCTAssertEqual(probe.authSummary, .tokenAccepted)
        XCTAssertTrue(m.canTrust, "a new fingerprint with no expectation typed is trustable")
    }

    func testAnUnreachableHostCannotBeTrusted() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probeUnreachableJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        XCTAssertFalse(m.canTrust)
    }

    func testAPlaintextDaemonPresentsNoFingerprintAndCannotBeTrusted() async {
        // No TLS means no certificate to pin, and "authenticated" is meaningless there.
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probePlaintextJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        guard case let .observed(probe) = m.pairing else { return XCTFail("expected .observed") }
        XCTAssertEqual(probe.authSummary, .nonePlaintext)
        XCTAssertFalse(m.canTrust, "there is no certificate to trust")
    }

    func testAMismatchedExpectedFingerprintBlocksTrust() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probeNewJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        m.expectedFingerprint = "deadbeef"
        XCTAssertFalse(m.canTrust, "a typed expectation that disagrees must block trust")
        XCTAssertNotNil(m.fingerprintComparison)
        m.expectedFingerprint = "A1B2"       // case- and whitespace-insensitive
        XCTAssertTrue(m.canTrust)
    }

    func testConfirmTrustSendsTheObservedFingerprintVerbatim() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probeNewJSON), .ok("{}"), .ok(twoHostsJSON)]
        var notified = 0
        let m = model(fake, onChanged: { notified += 1 })
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        await m.confirmTrust()
        XCTAssertEqual(fake.invocations[2].args,
                       ["remote", "trust", "studio", "--fingerprint", "a1b2", "--json"])
        XCTAssertEqual(notified, 1)
        XCTAssertEqual(m.pairing, .idle, "a successful trust closes the sheet")
    }

    func testCancelPairingClearsEverything() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .ok(probeNewJSON)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        m.expectedFingerprint = "aa"
        m.cancelPairing()
        XCTAssertEqual(m.pairing, .idle)
        XCTAssertEqual(m.expectedFingerprint, "")
    }

    func testAFailedProbeBecomesAPairingFailureNotASilentIdle() async {
        let fake = FakeCommandRunner()
        fake.results = [.ok(twoHostsJSON), .failed(#"{"error":"unknown host"}"#)]
        let m = model(fake)
        await m.reload()
        m.select(HostID("studio"))
        await m.beginPairing()
        guard case let .failed(message) = m.pairing else {
            return XCTFail("expected .failed, got \(m.pairing)")
        }
        XCTAssertTrue(message.contains("unknown host"), message)
        XCTAssertFalse(m.canTrust)
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cd macos && swift test --filter HostsViewModelTests`
Expected: FAIL — `value of type 'HostsViewModel' has no member 'pairing'`.

- [ ] **Step 3: Implement**

Add to `HostsViewModel`:

```swift
    /// Where the pairing flow is. `observed` carries what the probe saw — nothing is written until the
    /// user confirms, which is the whole point of splitting observe from trust.
    public enum PairingState: Equatable, Sendable {
        case idle
        case probing
        case observed(HostProbe)
        case failed(String)
    }

    @Published public private(set) var pairing: PairingState = .idle
    /// A fingerprint the user pasted from an out-of-band source, to be compared by software rather
    /// than by eye. Empty means "not comparing".
    @Published public var expectedFingerprint: String = ""

    private var observedProbe: HostProbe? {
        if case let .observed(p) = pairing { return p }
        return nil
    }

    /// Nil when there is nothing to compare; otherwise whether the typed expectation matches.
    public var fingerprintComparison: Bool? {
        guard let observed = observedProbe?.fingerprint else { return nil }
        let typed = expectedFingerprint.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !typed.isEmpty else { return nil }
        return typed == observed.lowercased()
    }

    /// Trust is offered only when a certificate was actually observed, and only when any typed
    /// expectation agrees with it.
    public var canTrust: Bool {
        guard let probe = observedProbe, probe.reachable, probe.fingerprint != nil else { return false }
        return fingerprintComparison != false
    }

    public func beginPairing() async {
        guard let host = selectedHost else { return }
        pairing = .probing
        expectedFingerprint = ""
        do {
            // Off the main actor: the CLI bounds each phase separately, so a probe can take ~3× its
            // timeout, and this runs while a sheet is on screen.
            pairing = .observed(try await registry.probeAsync(name: host.name))
        } catch {
            pairing = .failed((error as? LocalizedError)?.errorDescription ?? error.localizedDescription)
        }
    }

    public func confirmTrust() async {
        guard let host = selectedHost, canTrust,
              let fingerprint = observedProbe?.fingerprint else { return }
        await run {
            // Verbatim what was displayed. If a cert is swapped between probe and trust, the pin
            // fails loudly on the very next connect — an accepted, documented TOCTOU.
            try self.registry.trust(name: host.name, fingerprint: fingerprint)
            self.hosts = try self.registry.list()
            self.onHostsChanged()
        }
        if lastError == nil { cancelPairing() }
    }

    public func cancelPairing() {
        pairing = .idle
        expectedFingerprint = ""
    }
```

- [ ] **Step 4: Run to verify the tests pass**

Run: `cd macos && swift test`
Expected: PASS — 209 + 7 = 216.

- [ ] **Step 5: Demonstrate the mismatch guard can fail**

Change `canTrust` to `fingerprintComparison != nil` (a plausible slip), run, and confirm
`testAMismatchedExpectedFingerprintBlocksTrust` fails. **Capture the output**, revert. That guard is the
only thing standing between "compared out of band" and "trust-on-first-use with extra clicks".

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderCore/HostsViewModel.swift macos/Tests/ClowderCoreTests/HostsViewModelTests.swift
git commit -m "feat(app): add the probe-then-confirm pairing flow"
```

---

### Task 5: The Settings scene and the Hosts list

**Files:**
- Create: `macos/Sources/ClowderApp/SettingsView.swift`, `macos/Sources/ClowderApp/HostsSettingsView.swift`
- Modify: `macos/Sources/ClowderApp/App.swift`

**No tests** — `ClowderApp` has no test target. Every decision is in `HostsViewModel`; these views render it.

- [ ] **Step 1: Expose a view model from the delegate**

`AppDelegate.hostRegistry` is currently `private`. In `bootstrap()`, build the view model once (bootstrap
is idempotent, so this runs once) and expose it:

```swift
    private(set) var hostsModel: HostsViewModel?
```

```swift
        // In bootstrap(), after hostRegistry is created:
        let registry = HostRegistry(runner: ProcessCommandRunner(executablePath: clowderBinary))
        hostRegistry = registry
        hostsModel = HostsViewModel(
            registry: registry,
            activeBackend: { [weak self] in self?.activeBackend ?? .local },
            onHostsChanged: { [weak self] in self?.refreshHosts() }
        )
```

`onHostsChanged` wiring to `refreshHosts()` is what makes a host added in Settings appear in the sidebar
chip, the menu bar and the palette.

- [ ] **Step 2: Add the Settings scene**

In `ClowderApp.body`, after the `WindowGroup` and alongside `.commands`:

```swift
        Settings {
            // bootstrap() is idempotent; call it so a Settings-first launch is safe, then read the
            // model off the delegate. Its return tuple is (appModel:, surfaceHost:) and does NOT
            // carry hostsModel — verified against App.swift, do not write `bootstrap().hostsModel`.
            SettingsView(hosts: { _ = delegate.bootstrap(); return delegate.hostsModel }())
                .frame(width: 680, height: 460)
        }
```

Note the Settings body **cannot** see the `WindowGroup`'s `@EnvironmentObject` — pass the model
explicitly.

If that immediately-invoked closure reads badly, widening `bootstrap()`'s return tuple to include
`hostsModel` is the tidier alternative; either is fine, but say which you did and make sure a
Settings-first launch (⌘, before the main window has ever rendered) still bootstraps.

- [ ] **Step 3: Write `SettingsView`**

```swift
import SwiftUI
import ClowderCore

/// The Settings window (⌘,). One tab today; `TabView` so General/Keys can be added later without
/// restructuring the scene.
struct SettingsView: View {
    let hosts: HostsViewModel?

    var body: some View {
        TabView {
            Group {
                if let hosts {
                    HostsSettingsView(model: hosts)
                } else {
                    // Unbundled dev builds may bootstrap without a registry.
                    Text("Host management is unavailable in this build.")
                        .foregroundStyle(.secondary)
                }
            }
            .tabItem { Label("Hosts", systemImage: "network") }
        }
    }
}
```

- [ ] **Step 4: Write `HostsSettingsView`**

```swift
import SwiftUI
import ClowderCore

/// Master/detail over the host registry: the list on the left, the editor on the right.
struct HostsSettingsView: View {
    @ObservedObject var model: HostsViewModel

    var body: some View {
        HSplitView {
            VStack(spacing: 0) {
                List(selection: Binding(
                    get: { model.selected },
                    set: { model.select($0) }
                )) {
                    ForEach(model.hosts) { host in
                        HStack(spacing: 6) {
                            Image(systemName: host.isTrusted ? "lock.fill" : "lock.open")
                                .foregroundStyle(host.isTrusted ? .green : .secondary)
                                .help(host.isTrusted ? "Paired" : "Not paired")
                            VStack(alignment: .leading, spacing: 1) {
                                Text(host.name)
                                Text(host.address).font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            if !host.isEditable {
                                Text("config.toml").font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                        .tag(host.id)
                    }
                }

                Divider()
                HStack(spacing: 4) {
                    Button { model.beginAdd() } label: { Image(systemName: "plus") }
                        .help("Add a host")
                    Button {
                        if let id = model.selected { Task { await model.remove(id) } }
                    } label: { Image(systemName: "minus") }
                        .disabled(!model.canEditSelection)
                        .help("Remove the selected host")
                    Spacer()
                }
                .buttonStyle(.borderless)
                .padding(6)
            }
            .frame(minWidth: 220)

            Group {
                if model.draft != nil {
                    HostEditorView(model: model)
                } else {
                    Text("Select a host, or press + to add one.")
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .frame(minWidth: 380)
        }
        .task { await model.reload() }
        .alert("Host registry", isPresented: Binding(
            get: { model.lastError != nil },
            set: { if !$0 { model.dismissError() } }
        )) {
            Button("OK") { model.dismissError() }
        } message: {
            Text(model.lastError ?? "")
        }
    }
}
```

- [ ] **Step 5: Build**

Run: `cd macos && swift build`
Expected: success. `HostEditorView` does not exist yet — either write Task 6 first and commit both, or
add a placeholder and note it. **Do not commit a non-building tree**; if you stub it, the stub must
compile and Task 6 replaces it in the next commit.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/ClowderApp/SettingsView.swift macos/Sources/ClowderApp/HostsSettingsView.swift \
        macos/Sources/ClowderApp/App.swift
git commit -m "feat(app): add a Settings window with a hosts list"
```

---

### Task 6: `HostEditorView`

**Files:**
- Create: `macos/Sources/ClowderApp/HostEditorView.swift`

- [ ] **Step 1: Write the editor**

```swift
import SwiftUI
import ClowderCore

/// The per-host form. Purely a renderer: every rule (`nameError`, `addressError`, `tlsError`,
/// `isValid`) comes from `HostDraft`, and every operation from `HostsViewModel`.
struct HostEditorView: View {
    @ObservedObject var model: HostsViewModel
    @State private var showingPairing = false

    private var isReadOnly: Bool {
        // A `[remote] host` entry is defined in config.toml, which clowder never rewrites.
        model.draft?.isNew == false && !model.canEditSelection
    }

    var body: some View {
        Form {
            if isReadOnly {
                Text("Defined by [remote] host in config.toml — edit that file, or add a separate entry.")
                    .font(.caption).foregroundStyle(.secondary)
            }

            Section {
                TextField("Nickname", text: binding(\.name))
                if let e = model.draft?.nameError, !(model.draft?.name.isEmpty ?? true) {
                    Text(e).font(.caption).foregroundStyle(.red)
                }
                TextField("Address (host:port)", text: binding(\.address))
                if let e = model.draft?.addressError, !(model.draft?.address.isEmpty ?? true) {
                    Text(e).font(.caption).foregroundStyle(.red)
                }
            }

            Section {
                Toggle("Use TLS", isOn: binding(\.tls))
                SecureField(model.selectedHost?.hasToken == true ? "•••••••• (stored)" : "Token",
                            text: Binding(
                                get: { model.draft?.token ?? "" },
                                set: { model.draft?.token = $0.isEmpty ? nil : $0 }
                            ))
                Text("Typing a token replaces the stored one. Leave it blank to keep the current token.")
                    .font(.caption).foregroundStyle(.secondary)
                if let e = model.draft?.tlsError {
                    Text(e).font(.caption).foregroundStyle(.red)
                }
            }

            if model.draft?.isNew == false {
                Section("Trust") {
                    if let fp = model.selectedHost?.fingerprint {
                        Text(Self.grouped(fp))
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                    } else {
                        Text("Not paired — this host is trusted on first use.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    Button(model.selectedHost?.isTrusted == true ? "Re-pair…" : "Pair…") {
                        showingPairing = true
                    }
                    .disabled(isReadOnly)
                }
            }

            Section {
                HStack {
                    Spacer()
                    Button("Revert") { model.select(model.selected) }
                        .disabled(model.draft?.isNew == true)
                    Button("Save") { Task { await model.save() } }
                        .keyboardShortcut(.defaultAction)
                        .disabled(isReadOnly || !(model.draft?.isValid ?? false) || model.isBusy)
                }
            }
        }
        .formStyle(.grouped)
        .disabled(model.isBusy)
        .sheet(isPresented: $showingPairing) {
            PairingSheet(model: model, isPresented: $showingPairing)
        }
    }

    private func binding<T>(_ keyPath: WritableKeyPath<HostDraft, T>) -> Binding<T> where T: Equatable {
        Binding(
            get: { model.draft?[keyPath: keyPath] ?? HostDraft()[keyPath: keyPath] },
            set: { model.draft?[keyPath: keyPath] = $0 }
        )
    }

    /// A SHA-256 hex fingerprint in 4-character groups — the form people can actually compare.
    static func grouped(_ fingerprint: String) -> String {
        stride(from: 0, to: fingerprint.count, by: 4).map {
            let start = fingerprint.index(fingerprint.startIndex, offsetBy: $0)
            let end = fingerprint.index(start, offsetBy: min(4, fingerprint.count - $0))
            return String(fingerprint[start..<end])
        }.joined(separator: " ")
    }
}
```

- [ ] **Step 2: Build**

Run: `cd macos && swift build`
Expected: success once `PairingSheet` exists (Task 7). Same rule as Task 5 — do not commit a
non-building tree; stub or reorder, and say which.

- [ ] **Step 3: Commit**

```bash
git add macos/Sources/ClowderApp/HostEditorView.swift
git commit -m "feat(app): add the host editor form"
```

---

### Task 7: `PairingSheet`

**Files:**
- Create: `macos/Sources/ClowderApp/PairingSheet.swift`

**The sheet's job is to make an out-of-band comparison happen.** If the user does not check the
fingerprint against something the daemon told them directly, this is trust-on-first-use with extra
clicks — so the sheet must name where to get the real value, and offer to do the comparison in software.

- [ ] **Step 1: Write the sheet**

```swift
import SwiftUI
import ClowderCore

/// Probe a host, show what it presented, and record the user's decision. Nothing is written until
/// they confirm — observing and trusting are deliberately separate acts.
struct PairingSheet: View {
    @ObservedObject var model: HostsViewModel
    @Binding var isPresented: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Pair \(model.selectedHost?.name ?? "host")").font(.headline)

            switch model.pairing {
            case .idle, .probing:
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Contacting \(model.selectedHost?.address ?? "…")")
                }
                .frame(maxWidth: .infinity, alignment: .leading)

            case let .failed(message):
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)

            case let .observed(probe):
                observed(probe)
            }

            HStack {
                Button("Cancel") { model.cancelPairing(); isPresented = false }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Trust") {
                    Task {
                        await model.confirmTrust()
                        if model.lastError == nil { isPresented = false }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!model.canTrust || model.isBusy)
            }
        }
        .padding(20)
        .frame(width: 520)
        .task { await model.beginPairing() }
    }

    @ViewBuilder
    private func observed(_ probe: HostProbe) -> some View {
        if let fingerprint = probe.fingerprint {
            Text("This daemon presented:").font(.subheadline)
            Text(HostEditorView.grouped(fingerprint))
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)

            // The load-bearing sentence: without an out-of-band comparison this is TOFU with extra
            // clicks, so name exactly where the real value comes from.
            Text("Compare this with the fingerprint printed by `clowder remote-token` **on the daemon's "
                 + "own machine**, or in that daemon's startup log. Anything else can be forged.")
                .font(.caption).foregroundStyle(.secondary)

            TextField("Paste the expected fingerprint to compare (optional)",
                      text: $model.expectedFingerprint)
                .textFieldStyle(.roundedBorder)
                .font(.system(.caption, design: .monospaced))

            if let matches = model.fingerprintComparison {
                Label(matches ? "Matches" : "Does NOT match — do not trust this host",
                      systemImage: matches ? "checkmark.circle.fill" : "xmark.octagon.fill")
                    .foregroundStyle(matches ? .green : .red)
                    .font(.callout)
            }
        } else if !probe.tls {
            Label("This daemon is not using TLS, so it presents no certificate to pin.",
                  systemImage: "lock.open")
                .foregroundStyle(.orange)
        } else if !probe.reachable {
            Label(probe.error ?? "Could not reach the daemon.", systemImage: "network.slash")
                .foregroundStyle(.orange)
        }

        switch probe.authSummary {
        case .tokenAccepted: Label("Token accepted", systemImage: "checkmark.seal").font(.caption)
        case .tokenRejected: Label("Token rejected", systemImage: "xmark.seal")
                .font(.caption).foregroundStyle(.red)
        case .nonePlaintext: Label("No authentication (plaintext daemon)", systemImage: "exclamationmark.triangle")
                .font(.caption).foregroundStyle(.orange)
        }
    }
}
```

- [ ] **Step 2: Build and run the suite**

Run: `cd macos && swift build` then `cd macos && swift test`
Expected: both succeed; test count unchanged from Task 4 (216).

- [ ] **Step 3: Commit**

```bash
git add macos/Sources/ClowderApp/PairingSheet.swift
git commit -m "feat(app): add the fingerprint-confirmation pairing sheet"
```

---

### Task 8: Restore the `SettingsLink`

**Files:**
- Modify: `macos/Sources/ClowderApp/ConnectionChipView.swift`

M11b removed `SettingsLink { Text("Manage Hosts…") }` because no `Settings` scene existed, replacing both
the empty-registry hint and the removed item with text naming `clowder remote add`. The scene exists now.

- [ ] **Step 1: Restore it**

At the two places carrying the interim hints (around lines 51-53 and 67-69):

- Empty-registry branch: keep a short line saying there are no hosts, and follow it with the
  `SettingsLink` so the user can act — that is what turns a dead end into a path.
- Non-empty branch: replace the `clowder remote add` hint with `SettingsLink { Text("Manage Hosts…") }`.

Delete the "the next milestone replaces this" comments — this is that milestone.

`SettingsLink` requires macOS 14, which `Package.swift` targets.

- [ ] **Step 2: Build**

Run: `cd macos && swift build` and `cd macos && swift test`
Expected: both succeed.

- [ ] **Step 3: Commit**

```bash
git add macos/Sources/ClowderApp/ConnectionChipView.swift
git commit -m "feat(app): point the connection chip at the new Settings window"
```

---

### Task 9: Documentation and verification

**Files:**
- Modify: `AGENTS.md`, `README.md`, `docs/remote-tls.md`

- [ ] **Step 1: Document**

- `README.md` — hosts can be managed from the app's Settings window (⌘,), not only the CLI.
- `docs/remote-tls.md` — the pairing section currently describes the CLI flow (`probe` → compare →
  `trust`). Add that the app offers the same flow in Settings ▸ Hosts ▸ Pair, **with the same
  out-of-band requirement** — the comparison is what makes it meaningful, in either surface.
- `AGENTS.md` — add `SettingsView`/`HostsSettingsView`/`HostEditorView`/`PairingSheet` and
  `HostsViewModel` to the repo-layout description of `macos/`.

**Read the source for each claim** rather than paraphrasing this plan; several M11a/M11b doc statements
went stale between plan and implementation.

- [ ] **Step 2: Verify what you can**

```sh
source "$HOME/.cargo/env" && cargo test --workspace --locked
cd macos && swift test
cd macos && swift build
scripts/build-app.sh
./scripts/check-commit-messages.sh feat/m11b-app-hosts HEAD
```

Paste the real output of each. Three `clowder-daemon` tests flake under parallel load and pass on re-run;
say so if you hit them.

- [ ] **Step 3: Write the manual checklist as a handover**

You have **no GUI session** — do not report any of these as done. Write them into your report for the
maintainer:

1. **⌘, opens Settings** with a Hosts tab.
2. **Add a host** in Settings; it appears in the sidebar chip, the menu bar and the palette **without
   restarting** (this is `onHostsChanged` → `refreshHosts()`).
3. **Edit a host's address**, save, and confirm the change survives a reopen.
4. **Edit an unrelated field on a host that has a token**, save, then confirm the host still connects —
   proving the stored token was not silently cleared.
5. **Pair a host**: the sheet probes, shows a fingerprint in 4-character groups, and Trust is disabled
   until a pasted expectation matches. Paste a **wrong** value and confirm Trust stays disabled and the
   mismatch is called out in red.
6. **Trust it**, then confirm the list's lock icon turns green and `clowder remote show <name>` reports
   the same fingerprint.
7. **Try to remove the host you are currently connected to** — expect a refusal naming the host.
8. **Select the `[remote] host` entry** (if you have one configured): the form is read-only and says so.
9. **Probe an unreachable host** from the sheet and confirm the window stays responsive — this is what
   the off-main-thread probe buys.
10. Check `~/.local/state/clowder/daemon.log` for anything alarming.

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md README.md docs/remote-tls.md
git commit -m "docs(m11c): document the Settings window and in-app pairing"
```

---

## Verification gate for M11c

- [ ] `cargo test --workspace --locked` and `cd macos && swift test` green; `swift build` and
      `scripts/build-app.sh` succeed.
- [ ] `scripts/check-commit-messages.sh feat/m11b-app-hosts HEAD` passes.
- [ ] `HostDraft.nameError` agrees with `docs/protocol/fixtures/host-names.json` on every case, and the
      test proves it fails when the worktree rules are substituted.
- [ ] A host can be added, edited, paired and removed entirely from the app.
- [ ] A typed fingerprint that disagrees with the observed one **blocks** Trust.
- [ ] An edit that does not touch the token leaves the stored token intact.
- [ ] Removing the connected host is refused.
- [ ] The probe does not block the main thread.

## Deferred beyond M11c

Recorded from M11a/M11b reviews; none blocking:

- `AppDelegate` duplicates `activeBackend`/`hosts` that `AppModel` also holds — it could forward instead.
- The command palette does not call `refreshHosts()` on open (the chip and menu bar do).
- `detach()` is a no-op when Local is `.yielded` (an externally-started daemon), so switching back
  spuriously respawns.
- If `clowder remote rm <host>` runs in a shell while the app is connected to that host, no menu item is
  marked active — worth a disabled "Connected to `<host>` (no longer configured)" row.
- `connectionChip`'s host-not-in-list branch is evaluated before the connection `switch`, so a `.closed`
  reason is dropped for a host missing from the list.
- `ProcessCommandRunner` drains stdout fully before stderr; a chattier child could deadlock.
- The `<path>.lock` derivation exists in three Rust places that agree today but are not shared.

## Self-review notes

Checked against the spec's §7 (Settings scene) and M11b's three handoffs:

- Spec §7's pane, editor and pairing sheet — Tasks 5, 6, 7. `HostDraft` — Task 2. `HostsViewModel` —
  Tasks 3, 4.
- M11b handoff 1 (shared name fixture) — Task 2. Handoff 2 (`SettingsLink`) — Task 8. Handoff 3
  (`remove`/`untrust` argv tests) — Task 1.
- The deferred off-main-thread probe is folded into Task 1, because a sheet that probes on appear makes
  it user-visible rather than theoretical.
- Names are consistent across tasks: `HostDraft` (`name`/`address`/`tls`/`token`/`isNew`, `nameError`/
  `addressError`/`tlsError`/`isValid`); `HostsViewModel` (`hosts`/`selected`/`draft`/`lastError`/
  `isBusy`/`pairing`/`expectedFingerprint`, `reload`/`select`/`beginAdd`/`save`/`remove`/`beginPairing`/
  `confirmTrust`/`cancelPairing`, `selectedHost`/`canEditSelection`/`canTrust`/`fingerprintComparison`);
  `probeAsync` (Task 1).
- Three places deliberately tell the implementer to read the code and report rather than trust this plan:
  `bootstrap()`'s return shape (Task 5), whether `HostRegistry` accepts `Sendable` cleanly (Task 1), and
  the build-ordering between Tasks 5-7. Every M11a/M11b defect that reached review came from a plan
  detail asserted rather than checked.
