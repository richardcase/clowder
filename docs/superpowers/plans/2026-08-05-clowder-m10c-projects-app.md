# M10c — Projects App Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the macOS app the projects UI — a sidebar of projects containing worktrees, project terminals, restart, and the two creation sheets — and close the one daemon race M10b deferred.

**Architecture:** Almost all logic lands in `ClowderCore`, which `swift test` compiles and runs locally: selection, the prepared sidebar model, attention rollups, command enablement, and sheet validation are pure and unit-tested. `Sources/ClowderApp/` stays a thin renderer over that model, because **`swift test` never compiles it** — CI is the first compiler it meets. The riskiest task is therefore last, built on pieces already proven green.

**Tech Stack:** Swift 5 / SwiftPM (`ClowderCore` library + `ClowderApp` executable), SwiftUI/AppKit; Rust (`clowder-daemon`) for Task 1 only.

## Global Constraints

- **Swift tests: `cd macos && swift test`.** **NEVER run `swift build`** — it links a gitignored 189 MB vendored `libghostty` that is absent here and needs full Xcode. Its failure means nothing about your work.
- **`swift test` builds `ClowderCore` only.** Nothing under `Sources/ClowderApp/` is compiled locally. Re-read every edit there against the current `ClowderCore` API as a compiler would.
- **Ignore SourceKit/IDE diagnostics.** They are stale in this repo and routinely report phantom errors that a passing `swift test` disproves. Trust CLI output only.
- **Prefix every cargo command** with `source "$HOME/.cargo/env" && `. **Run everything in the FOREGROUND.**
- **If a test appears to hang, do not wait on it** — run it alone (`cargo test -p clowder-daemon --lib <name> -- --exact`; `cargo test` accepts only ONE filter substring). The daemon suite hung rather than failed three times during M10b.
- **Branch:** `feat/m10c-projects-app`, already checked out, cut from `feat/m10b-projects-daemon`. Its PR targets **`feat/m10b-projects-daemon`**, not `main`.
- **Every commit message ends with these two trailers**, separated from the body by a blank line:
  ```
  Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC
  ```
- **Add a fixture** to `docs/protocol/fixtures/` for any new wire message, per `docs/protocol/README.md`. This task adds none, but check before assuming.
- `git` is proxied through a filtering wrapper; use `rtk proxy git <args>` for raw output. Do not use `git stash`.
- Test output must be pristine — no new warnings.

### Invariants this plan depends on

M10b's plan shipped six defects, several because a snippet was transcribed rather than checked. Where a snippet below must preserve an invariant, the invariant is stated beside it. **If a snippet and its stated invariant disagree, the invariant wins — fix the snippet and say so in your report.**

- **`selection` and `selectedPane` agree.** `selectedPane` is derived, never stored. For `.worktree(p)` it is `p`; for `.project(path)` it is whatever pane the daemon reported for that project's terminal, or `nil` if none is open yet.
- **A project row's attention count is exactly the number of its worktrees needing attention.** No worktree may be counted under two projects, and none may be silently dropped.
- **Worktrees whose project is not registered are omitted from the sidebar**, per the spec's "fresh start" decision — and therefore from `orderedWorktrees`, Cmd-1…9 and the attention count.

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/clowder-daemon/src/server.rs` | Serialize spawn against `remove_project` | 1 |
| `macos/Sources/ClowderCore/AgentStore.swift` | Project state, prepared sidebar model, rollups | 2 |
| `macos/Sources/ClowderCore/SidebarSelection.swift` (new) | The selection enum | 3 |
| `macos/Sources/ClowderCore/AppModel.swift` | `selection`, derived `selectedPane`, terminal open, restart | 3 |
| `macos/Sources/ClowderCore/Keymap.swift` | `CommandID` changes, enablement | 4 |
| `macos/Sources/ClowderCore/PaletteSearch.swift` | Palette over the new command set | 4 |
| `macos/Sources/ClowderCore/SheetForms.swift` (new) | `AddProjectForm` / `NewWorktreeForm` validation | 5 |
| `macos/Sources/ClowderApp/{AddProjectSheet,NewWorktreeSheet}.swift` | Thin sheet views | 5 |
| `macos/Sources/ClowderApp/ContentView.swift` | Sidebar + detail renderer | 6 |
| `macos/Sources/ClowderApp/{StatusBarController,CommandPaletteView,App}.swift` | Follow the renames | 6 |

---

### Task 1: Serialize spawn against `remove_project`

M10b's review found this and deferred it here as its own change. `spawn_agent` checks project registration, then does hundreds of milliseconds of `git worktree add`, and only inserts into `agents` at the very end via `finalize_agent`. A concurrent `remove_project` counts `agents`, sees zero, and removes the project — leaving a running agent whose project is unregistered. That is exactly the invariant the guard exists to hold.

**Files:** Modify `crates/clowder-daemon/src/server.rs`.

**Interfaces:**
- Produces: a private `project_mutation: Mutex<()>` on `Daemon`, held across the whole of `spawn_agent` and the whole of `remove_project`.

- [ ] **Step 1: Write the failing test**

Add to `server.rs`'s test module:

```rust
    #[tokio::test]
    async fn remove_project_cannot_race_a_spawn_into_it() {
        use crate::SyntheticAdapter;
        let state = tempfile::tempdir().unwrap();
        let repo = crate::test_support::init_repo();
        let d = test_daemon_in(state.path());
        d.add_project(repo.path()).unwrap();

        let adapter = SyntheticAdapter { command: crate::PaneCommand {
            program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] } };

        // Spawn on a blocking thread while the main task hammers remove_project. Without the
        // serialization, remove_project can observe an empty `agents` map during the provisioning
        // window and drop the project out from under a live agent.
        let d2 = StdArc::clone(&d);
        let path = repo.path().to_path_buf();
        let spawner = std::thread::spawn(move || d2.spawn_agent(&path, &adapter, "racy"));

        let mut removed_while_spawning = false;
        for _ in 0..200 {
            if d.remove_project(repo.path()).is_ok() {
                removed_while_spawning = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let pane = spawner.join().unwrap();

        if let Ok(pane) = pane {
            assert!(!removed_while_spawning,
                    "removed the project while an agent was being spawned into it");
            assert!(d.is_registered_project(repo.path()), "project must still be registered");
            d.teardown_agent(pane).unwrap();
        } else {
            // The spawn lost the race and was rejected — acceptable, but then the project must
            // genuinely be gone, not half-removed.
            assert!(!d.is_registered_project(repo.path()));
        }
    }
```

This test is inherently timing-dependent. **If it proves flaky under parallel load, mark it `#[ignore]` with a comment saying why and run it explicitly — do not weaken its assertions.** Three daemon tests already carry that treatment.

- [ ] **Step 2: Run it to verify it fails**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon --lib remove_project_cannot_race -- --exact --nocapture`
Expected: FAIL — the removal succeeds mid-spawn, tripping the first assertion. If it passes on the first run, the window was simply missed; increase the loop count or confirm by reading that no lock exists yet.

- [ ] **Step 3: Add the mutex**

Add the field to `Daemon` beside `projects`, and initialize it in `new_with_paths`:

```rust
    /// Serializes project-list mutations against agent spawns. `spawn_agent` validates the project
    /// then provisions for hundreds of milliseconds before it appears in `agents`; without this,
    /// a concurrent `remove_project` counts zero worktrees and removes a project out from under a
    /// live agent. Held across the WHOLE of each operation, not just the check.
    project_mutation: Mutex<()>,
```

In `spawn_agent`, take it as the very first statement — before `canonicalize`, so the guard covers the registration check *and* provisioning *and* the registry insert:

```rust
        let _mutation = self.project_mutation.lock();
```

In `remove_project`, take it as the very first statement too.

**Invariant to preserve:** `spawn_agent` must not hold `project_mutation` across anything that could re-enter it. It calls `finalize_agent`, which spawns background tasks — those are `tokio::spawn`ed and do not call back into `spawn_agent`/`remove_project`, so this is safe. **Verify that is still true before you finish**, and say so in your report.

- [ ] **Step 4: Run the test and the suite**

Run: `source "$HOME/.cargo/env" && cargo test -p clowder-daemon --lib remove_project_cannot_race -- --exact` then `cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/clowder-daemon/src/server.rs
git commit -m "fix(daemon): serialize spawn against remove_project

spawn_agent validated the project, then provisioned for hundreds of milliseconds
before the agent appeared in the agents map. A concurrent remove_project counted
zero worktrees and removed the project out from under a live agent — the exact
outcome its guard exists to prevent.

Deferred from M10b, which flagged that it deserved its own change rather than
being folded into UI work.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 2: Project state in `AgentStore`

`AgentStore.apply` currently ignores all five project events (`AgentStore.swift:42-43`). This task makes it hold project state and expose the prepared sidebar model the views render.

**Files:** Modify `macos/Sources/ClowderCore/AgentStore.swift`; add tests to `macos/Tests/ClowderCoreTests/AgentStoreTests.swift`.

**Interfaces:**
- Produces:
  - `SidebarProject { path, name, kind, worktrees: [WorktreeInfo], attentionCount: Int }`, `Identifiable` by `path`
  - `AgentStore.projects: [ProjectInfo]`, `AgentStore.projectTerminals: [String: UInt64]`
  - `AgentStore.sidebar: [SidebarProject]` — projects sorted by name, worktrees by pane
  - `AgentStore.orderedWorktrees` re-derived from `sidebar` (replacing `byProject`)

- [ ] **Step 1: Write the failing tests**

```swift
    private func wt(_ pane: UInt64, _ project: String, _ name: String,
                    _ state: AttentionState = .working) -> WorktreeInfo {
        WorktreeInfo(pane: pane, project: project, name: name,
                     branch: "clowder/\(name)", state: state)
    }

    func testSidebarGroupsWorktreesUnderTheirProject() {
        let s = AgentStore()
        s.apply(.projectList([
            ProjectInfo(path: "/code/beta", name: "beta", kind: "jj"),
            ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git"),
        ]))
        s.apply(.worktreeList([
            wt(3, "/code/alpha", "c"), wt(1, "/code/alpha", "a"), wt(2, "/code/beta", "b"),
        ]))
        let sidebar = s.sidebar
        XCTAssertEqual(sidebar.map(\.name), ["alpha", "beta"], "projects sort by name")
        XCTAssertEqual(sidebar[0].kind, "git")
        XCTAssertEqual(sidebar[0].worktrees.map(\.pane), [1, 3], "worktrees sort by pane")
        XCTAssertEqual(sidebar[1].worktrees.map(\.pane), [2])
    }

    func testSidebarOmitsWorktreesWithNoRegisteredProject() {
        let s = AgentStore()
        s.apply(.projectList([ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")]))
        s.apply(.worktreeList([wt(1, "/code/alpha", "a"), wt(2, "/code/orphan", "b")]))
        XCTAssertEqual(s.sidebar.count, 1)
        XCTAssertEqual(s.orderedWorktrees.map(\.pane), [1], "orphans are omitted from the order too")
        XCTAssertEqual(s.attentionCount, 0)
    }

    func testProjectAttentionCountRollsUpItsOwnWorktreesOnly() {
        let s = AgentStore()
        s.apply(.projectList([
            ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git"),
            ProjectInfo(path: "/code/beta", name: "beta", kind: "git"),
        ]))
        s.apply(.worktreeList([
            wt(1, "/code/alpha", "a", .needsInput),
            wt(2, "/code/alpha", "b", .completed),
            wt(3, "/code/alpha", "c", .working),
            wt(4, "/code/beta", "d", .needsInput),
        ]))
        XCTAssertEqual(s.sidebar[0].attentionCount, 2, "needsInput + completed, alpha only")
        XCTAssertEqual(s.sidebar[1].attentionCount, 1)
        XCTAssertEqual(s.attentionCount, 3, "global count is the sum")
    }

    func testProjectAddedAndRemovedMutateTheList() {
        let s = AgentStore()
        s.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        XCTAssertEqual(s.sidebar.map(\.path), ["/code/alpha"])
        s.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        XCTAssertEqual(s.sidebar.count, 1, "projectAdded is idempotent — it arrives twice by design")
        s.apply(.projectRemoved(path: "/code/alpha"))
        XCTAssertTrue(s.sidebar.isEmpty)
    }

    func testProjectTerminalMappingIsTrackedAndCleared() {
        let s = AgentStore()
        s.apply(.projectTerminalOpened(path: "/code/alpha", pane: 7))
        XCTAssertEqual(s.projectTerminals["/code/alpha"], 7)
        s.apply(.projectTerminalClosed(path: "/code/alpha"))
        XCTAssertNil(s.projectTerminals["/code/alpha"])
    }

    func testRemovingAProjectDropsItsTerminalMapping() {
        let s = AgentStore()
        s.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        s.apply(.projectTerminalOpened(path: "/code/alpha", pane: 7))
        s.apply(.projectRemoved(path: "/code/alpha"))
        XCTAssertNil(s.projectTerminals["/code/alpha"], "a removed project's terminal is gone")
    }

    func testResetClearsProjectState() {
        let s = AgentStore()
        s.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        s.apply(.projectTerminalOpened(path: "/code/alpha", pane: 7))
        s.reset()
        XCTAssertTrue(s.projects.isEmpty)
        XCTAssertTrue(s.projectTerminals.isEmpty)
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd macos && swift test --filter AgentStoreTests`
Expected: FAIL to compile — `AgentStore` has no member `sidebar` / `projects` / `projectTerminals`.

- [ ] **Step 3: Implement**

Add above `AgentStore`:

```swift
/// One project row plus its worktrees, prepared for rendering. Built by `AgentStore.sidebar`
/// so the view layer — which no local compiler checks — stays a plain `ForEach` over this.
public struct SidebarProject: Identifiable, Equatable, Sendable {
    public let path: String
    public let name: String
    /// `"git"` or `"jj"`.
    public let kind: String
    public let worktrees: [WorktreeInfo]
    /// How many of THIS project's worktrees want a response. Shown on the row so a collapsed
    /// project can never hide a waiting agent.
    public let attentionCount: Int
    public var id: String { path }
}
```

Add the stored state beside the existing `@Published` properties:

```swift
    @Published public private(set) var projects: [ProjectInfo] = []
    /// Project path → its open terminal's pane. Populated by `projectTerminalOpened`; a missing
    /// entry means "not open yet", which is what makes selecting a project ask the daemon.
    @Published public private(set) var projectTerminals: [String: UInt64] = [:]
```

Replace the `.projectList, .projectAdded, …` no-op arm in `apply` with:

```swift
        case let .projectList(list):
            projects = list
            // Drop terminal mappings for projects that no longer exist.
            let live = Set(list.map(\.path))
            projectTerminals = projectTerminals.filter { live.contains($0.key) }
        case let .projectAdded(project):
            // Arrives twice for the requesting client (direct reply + broadcast) by design.
            if let i = projects.firstIndex(where: { $0.path == project.path }) {
                projects[i] = project
            } else {
                projects.append(project)
            }
        case let .projectRemoved(path):
            projects.removeAll { $0.path == path }
            projectTerminals[path] = nil
        case let .projectTerminalOpened(path, pane):
            projectTerminals[path] = pane
        case let .projectTerminalClosed(path):
            projectTerminals[path] = nil
```

Extend `reset()` with `projects = []` and `projectTerminals = [:]`.

Replace `byProject` with `sidebar`, and re-derive `orderedWorktrees` from it:

```swift
    /// Projects with their worktrees, ready to render. Projects sort by display name, worktrees
    /// by pane. Worktrees whose project is not registered are omitted — the spec's "fresh start"
    /// decision — so they are absent from the order and the attention count too.
    public var sidebar: [SidebarProject] {
        let byPath = Dictionary(grouping: worktrees.values, by: { $0.project })
        return projects
            .sorted { ($0.name, $0.path) < ($1.name, $1.path) }
            .map { p in
                let mine = (byPath[p.path] ?? []).sorted { $0.pane < $1.pane }
                return SidebarProject(
                    path: p.path, name: p.name, kind: p.kind, worktrees: mine,
                    attentionCount: mine.filter { $0.state == .needsInput || $0.state == .completed }.count)
            }
    }

    /// The sidebar order flattened — the stable index order for Cmd-1…9 and the palette.
    public var orderedWorktrees: [WorktreeInfo] { sidebar.flatMap(\.worktrees) }
```

`worktreesNeedingAttention` and `attentionCount` already derive from `orderedWorktrees` and need no change — but confirm that, since it is what keeps the per-project rollup and the global count consistent.

- [ ] **Step 4: Run the tests**

Run: `cd macos && swift test`
Expected: PASS. Existing tests referencing `byProject` must be updated to `sidebar` — that is a rename of the same concept, not a change of what they assert.

- [ ] **Step 5: Commit**

```bash
git add macos/
git commit -m "feat(app): hold project state in AgentStore

apply() now handles all five project events, and `sidebar` prepares the rendered
model — projects sorted by name, worktrees by pane, each row carrying its own
attention rollup so a collapsed project cannot hide a waiting agent.

Worktrees whose project is not registered are omitted, per the spec's fresh-start
decision, and therefore absent from Cmd-1..9 and the attention count too.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 3: `SidebarSelection` and a derived `selectedPane`

The keystone. `AppModel.selectedPane` is stored today and threaded through `currentTree`, `focusedPane`, `splitFocused`, `closeFocused`, `requestLifecycle` and `SplitContainer`. Making it **computed** from a new `selection` is what keeps every one of those call sites working unchanged.

**Files:** Create `macos/Sources/ClowderCore/SidebarSelection.swift`; modify `macos/Sources/ClowderCore/AppModel.swift`; add tests to `macos/Tests/ClowderCoreTests/AppModelTests.swift`.

**Interfaces:**
- Produces:
  - `enum SidebarSelection: Hashable, Sendable { case project(String), worktree(UInt64) }`
  - `AppModel.selection: SidebarSelection?` (stored, `@Published`)
  - `AppModel.selectedPane: UInt64?` (**computed**, not stored)
  - `AppModel.restartSelectedWorktree()`
  - `AppModel.openTerminal(forProject:)`

- [ ] **Step 1: Write the failing tests**

```swift
    func testSelectedPaneIsDerivedFromSelection() {
        let m = makeModel()               // existing helper in this file
        m.selection = .worktree(5)
        XCTAssertEqual(m.selectedPane, 5)
        m.selection = .project("/code/alpha")
        XCTAssertNil(m.selectedPane, "a project with no open terminal has no pane yet")
        m.store.apply(.projectTerminalOpened(path: "/code/alpha", pane: 9))
        XCTAssertEqual(m.selectedPane, 9, "once the daemon reports the terminal, it resolves")
        m.selection = nil
        XCTAssertNil(m.selectedPane)
    }

    func testSelectingAProjectWithNoTerminalAsksTheDaemon() throws {
        let (m, fake) = makeModelWithTransport()   // existing helper
        m.store.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        fake.sent.removeAll()
        m.selection = .project("/code/alpha")
        XCTAssertTrue(fake.sent.contains { $0.contains("openProjectTerminal") },
                      "must ask the daemon to open the terminal: \(fake.sent)")
    }

    func testSelectingAProjectWithAKnownTerminalDoesNotReask() {
        let (m, fake) = makeModelWithTransport()
        m.store.apply(.projectTerminalOpened(path: "/code/alpha", pane: 9))
        fake.sent.removeAll()
        m.selection = .project("/code/alpha")
        XCTAssertFalse(fake.sent.contains { $0.contains("openProjectTerminal") },
                       "already open — selecting must not respawn")
    }

    func testLifecycleCommandsAreNoOpsUnderAProjectSelection() {
        let m = makeModel()
        m.store.apply(.projectAdded(ProjectInfo(path: "/code/alpha", name: "alpha", kind: "git")))
        m.store.apply(.projectTerminalOpened(path: "/code/alpha", pane: 9))
        m.selection = .project("/code/alpha")
        m.requestLifecycle(.land)
        XCTAssertNil(m.pendingLifecycle, "land must refuse a project terminal")
        m.requestLifecycle(.discard)
        XCTAssertNil(m.pendingLifecycle)
    }

    func testRestartIsOnlyOfferedForAnExitedWorktree() {
        let (m, fake) = makeModelWithTransport()
        m.store.apply(.projectList([ProjectInfo(path: "/p", name: "p", kind: "git")]))
        m.store.apply(.worktreeList([
            WorktreeInfo(pane: 1, project: "/p", name: "a", branch: "clowder/a", state: .working),
        ]))
        m.selection = .worktree(1)
        XCTAssertFalse(m.canRestartSelection)
        fake.sent.removeAll()
        m.restartSelectedWorktree()
        XCTAssertTrue(fake.sent.isEmpty, "restart must not be sent for a live agent")

        m.store.apply(.attentionChanged(pane: 1, state: .exited))
        XCTAssertTrue(m.canRestartSelection)
        m.restartSelectedWorktree()
        XCTAssertTrue(fake.sent.contains { $0.contains("restartWorktree") }, "\(fake.sent)")
    }

    func testSelectingAWorktreeRequestsItsSplitTree() {
        let (m, fake) = makeModelWithTransport()
        fake.sent.removeAll()
        m.selection = .worktree(4)
        XCTAssertTrue(fake.sent.contains { $0.contains("getSplitTree") }, "\(fake.sent)")
    }
```

**`AppModelTests.swift` has no `makeModel` helper** — every test constructs the model inline, and `FakeControlTransport` (defined at the top of that file) records to **`sentLines`**, not `sent`. So write the setup as the file already does, and read `fake.sentLines`:

```swift
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
```

Adapt the test bodies above accordingly (`fake.sentLines` throughout, and `fake.deliver(_:)` if you need to push a line). Do **not** add a `makeModel` helper alongside the existing inline style.

- [ ] **Step 2: Run to verify they fail**

Run: `cd macos && swift test --filter AppModelTests`
Expected: FAIL to compile — no `selection`, no `canRestartSelection`.

- [ ] **Step 3: Create the selection type**

`macos/Sources/ClowderCore/SidebarSelection.swift`:

```swift
import Foundation

/// What the sidebar has selected. A project resolves to its terminal's pane (once open);
/// a worktree resolves to its agent's pane, which is also the worktree's durable identity —
/// the daemon re-spawns an agent under its original pane id.
public enum SidebarSelection: Hashable, Sendable {
    /// Canonical project path.
    case project(String)
    /// The worktree's pane id.
    case worktree(UInt64)
}
```

- [ ] **Step 4: Rework `AppModel`**

Replace the stored `selectedPane` (`AppModel.swift:31-36`) with:

```swift
    @Published public var selection: SidebarSelection? {
        didSet {
            focusedPane = selectedPane          // focus the root pane on (re)select
            switch selection {
            case let .worktree(pane):
                try? session?.send(.getSplitTree(agent: pane))
            case let .project(path):
                if let pane = store.projectTerminals[path] {
                    try? session?.send(.getSplitTree(agent: pane))
                } else {
                    // Not open yet — ask. `projectTerminalOpened` will populate the mapping,
                    // and the daemon's open is idempotent, so a duplicate ask is harmless.
                    try? session?.send(.openProjectTerminal(path: path))
                }
            case nil:
                break
            }
        }
    }

    /// The root pane of the current selection. **Derived, never stored** — this is what lets
    /// `currentTree`, `focusedPane`, `splitFocused`, `closeFocused` and `SplitContainer` keep
    /// working unchanged, since they always meant "the selection's root pane".
    public var selectedPane: UInt64? {
        switch selection {
        case let .worktree(pane): return pane
        case let .project(path): return store.projectTerminals[path]
        case nil: return nil
        }
    }
```

Add, near `requestLifecycle`:

```swift
    /// The selected worktree, if the selection is a worktree that exists.
    public var selectedWorktree: WorktreeInfo? {
        guard case let .worktree(pane) = selection else { return nil }
        return store.worktrees[pane]
    }

    /// Restart is offered only for an exited agent — the daemon refuses it otherwise.
    public var canRestartSelection: Bool { selectedWorktree?.state == .exited }

    public func restartSelectedWorktree() {
        guard canRestartSelection, case let .worktree(pane) = selection else { return }
        try? session?.send(.restartWorktree(pane: pane))
    }

    /// Ask the daemon to open (or re-open) a project's terminal. Idempotent daemon-side.
    public func openTerminal(forProject path: String) {
        try? session?.send(.openProjectTerminal(path: path))
    }

    public func addProject(path: String) { try? session?.send(.addProject(path: path)) }
    public func removeProject(path: String) { try? session?.send(.removeProject(path: path)) }
```

**Invariant:** `requestLifecycle` must refuse a project selection. Change its guard from `selectedPane` to:

```swift
    public func requestLifecycle(_ action: LifecycleAction) {
        guard case let .worktree(pane) = selection, let w = store.worktrees[pane] else { return }
        pendingLifecycle = PendingLifecycle(action: action, pane: pane, name: w.name)
    }
```

Update `selectAgent(atIndex:)` and `selectNextAttention()` to assign `.worktree(…)`, and `reconnect()`'s `selectedPane = nil` to `selection = nil`. `currentTree`, `splitFocused`, `closeFocused`, `focusNextPane` and `reconcileFocus` reference `selectedPane` and need **no change** — verify that and report it.

- [ ] **Step 5: Run the tests**

Run: `cd macos && swift test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add macos/
git commit -m "feat(app): SidebarSelection with a derived selectedPane

selection is stored; selectedPane becomes computed from it. Every existing call
site — currentTree, focusedPane, splitFocused, closeFocused, SplitContainer —
already meant 'the selection's root pane', so they keep working untouched.

Selecting a project asks the daemon to open its terminal when none is mapped yet;
land/discard refuse a project selection, and restart is offered only for an
exited agent, matching the daemon's own guard.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 4: Commands, keymap and palette

`Cmd-N` is `spawnAgent` today and must stay unconditional. There are now two creation actions.

**Files:** Modify `macos/Sources/ClowderCore/Keymap.swift`, `macos/Sources/ClowderCore/PaletteSearch.swift`; tests in `KeymapTests.swift` / `PaletteSearchTests.swift`.

**Interfaces:**
- Produces: `CommandID.{newWorktree, addProject, restartWorktree}` replacing `.spawnAgent`; `Cmd-N` → `.newWorktree`, `Cmd-Shift-N` → `.addProject`; `AppModel.isEnabled(_:) -> Bool`.

- [ ] **Step 1: Write the failing tests**

```swift
    func testNewWorktreeKeepsCmdNAndAddProjectTakesCmdShiftN() {
        let k = Keymap()
        XCTAssertEqual(k.binding(for: .newWorktree), KeyBinding("n", .command))
        XCTAssertEqual(k.binding(for: .addProject), KeyBinding("n", [.command, .shift]))
    }

    func testCommandRegistryListsBothCreationCommandsAndRestart() {
        let titles = CommandRegistry.all(keymap: Keymap()).map(\.title)
        XCTAssertTrue(titles.contains("New Worktree"), "\(titles)")
        XCTAssertTrue(titles.contains("Add Project"), "\(titles)")
        XCTAssertTrue(titles.contains("Restart Agent"), "\(titles)")
        XCTAssertFalse(titles.contains("Spawn Agent"), "renamed: \(titles)")
    }

    func testRestartIsDisabledUnlessAnExitedWorktreeIsSelected() {
        let m = makeModel()
        m.store.apply(.projectList([ProjectInfo(path: "/p", name: "p", kind: "git")]))
        m.store.apply(.worktreeList([
            WorktreeInfo(pane: 1, project: "/p", name: "a", branch: "clowder/a", state: .working),
        ]))
        XCTAssertFalse(m.isEnabled(.restartWorktree), "nothing selected")
        m.selection = .worktree(1)
        XCTAssertFalse(m.isEnabled(.restartWorktree), "agent is alive")
        m.store.apply(.attentionChanged(pane: 1, state: .exited))
        XCTAssertTrue(m.isEnabled(.restartWorktree))
        XCTAssertTrue(m.isEnabled(.newWorktree), "New Worktree is always available")
        XCTAssertTrue(m.isEnabled(.addProject))
    }

    func testLandAndDiscardAreDisabledUnderAProjectSelection() {
        let m = makeModel()
        m.store.apply(.projectAdded(ProjectInfo(path: "/p", name: "p", kind: "git")))
        m.selection = .project("/p")
        XCTAssertFalse(m.isEnabled(.landAgent))
        XCTAssertFalse(m.isEnabled(.discardAgent))
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd macos && swift test --filter KeymapTests`
Expected: FAIL to compile — no `.newWorktree`.

- [ ] **Step 3: Implement**

In `Keymap.swift`: rename `case spawnAgent` to `case newWorktree`, add `case addProject` and `case restartWorktree`. In `Keymap.defaults` replace `.spawnAgent: KeyBinding("n", .command)` with:

```swift
            .newWorktree:   KeyBinding("n", .command),
            .addProject:    KeyBinding("n", [.command, .shift]),
```

In `CommandRegistry.all`, rename the Spawn Agent row and add two more:

```swift
            Command(id: .newWorktree, title: "New Worktree",
                    subtitle: "Create a worktree in a project and start an agent",
                    defaultShortcut: keymap.binding(for: .newWorktree)),
            Command(id: .addProject, title: "Add Project",
                    subtitle: "Register a git or jj repository",
                    defaultShortcut: keymap.binding(for: .addProject)),
            Command(id: .restartWorktree, title: "Restart Agent",
                    subtitle: "Re-run the agent in the selected worktree",
                    defaultShortcut: keymap.binding(for: .restartWorktree)),
```

In `AppModel`, add enablement and extend `run(_:)`:

```swift
    /// Whether a command applies to the current selection. The palette dims disabled rows and
    /// key handling ignores them, so the UI never offers something the daemon would refuse.
    public func isEnabled(_ id: CommandID) -> Bool {
        switch id {
        case .landAgent, .discardAgent: return selectedWorktree != nil
        case .restartWorktree:          return canRestartSelection
        case .closePane:                return focusedPane != nil && focusedPane != selectedPane
        default:                        return true
        }
    }
```

and in `run(_:)`: `case .newWorktree: showingNewWorktree = true`, `case .addProject: showingAddProject = true`, `case .restartWorktree: restartSelectedWorktree()`.

Replace the `showingSpawn` property with three:

```swift
    @Published public var showingAddProject: Bool = false
    @Published public var showingNewWorktree: Bool = false
    /// Which project the New Worktree sheet should prefill. Set by the per-project `+` and the
    /// context menu; `.newWorktree` from the palette or Cmd-N leaves it as-is, so the sheet falls
    /// back to the current selection's project or the first project.
    @Published public var newWorktreeProject: String = ""
```

Have `run(.newWorktree)` prefill from the current selection before showing the sheet, so `Cmd-N` with a worktree selected lands in the right project:

```swift
        case .newWorktree:
            if case let .project(path) = selection {
                newWorktreeProject = path
            } else if let w = selectedWorktree {
                newWorktreeProject = w.project
            }
            showingNewWorktree = true
```

Add a test for that prefill: with `.worktree(1)` selected where worktree 1's project is `/p`, `run(.newWorktree)` sets `newWorktreeProject == "/p"`.

In `PaletteSearch.swift`, keep the fuzzy machinery and `PaletteItemKind` as they are; the command list now simply contains the new rows.

- [ ] **Step 4: Run the tests**

Run: `cd macos && swift test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add macos/
git commit -m "feat(app): New Worktree and Add Project commands

Cmd-N stays unconditional and becomes New Worktree; Cmd-Shift-N adds a project.
Restart Agent joins the palette, and isEnabled() gates land/discard/restart so the
UI never offers an action the daemon would refuse.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 5: Sheet form models and the two sheets

Validation lives in `ClowderCore` so it is unit-tested; the sheets stay thin.

**Files:** Create `macos/Sources/ClowderCore/SheetForms.swift`, `macos/Sources/ClowderApp/AddProjectSheet.swift`, `macos/Sources/ClowderApp/NewWorktreeSheet.swift`; delete `macos/Sources/ClowderApp/SpawnSheet.swift`; tests in a new `macos/Tests/ClowderCoreTests/SheetFormsTests.swift`.

**Interfaces:**
- Produces: `AddProjectForm { path; var isValid: Bool }`, `NewWorktreeForm { projectPath, name, adapter; var isValid: Bool; var nameError: String? }`

The name rules must mirror the daemon's `validate_workspace_name` so the sheet can disable Create rather than round-tripping an error: non-empty, ≤64 chars, `[A-Za-z0-9._-]` only, no leading `.` or `-`, not `.`/`..`, no `..`, no `.lock` suffix, no trailing `.`. **The daemon remains the authority** — this is a convenience mirror, and a name that slips through still gets a clean daemon error.

- [ ] **Step 1: Write the failing tests**

```swift
import XCTest
@testable import ClowderCore

final class SheetFormsTests: XCTestCase {
    func testAddProjectFormRequiresANonEmptyPath() {
        XCTAssertFalse(AddProjectForm(path: "").isValid)
        XCTAssertFalse(AddProjectForm(path: "   ").isValid)
        XCTAssertTrue(AddProjectForm(path: "/code/alpha").isValid)
    }

    func testNewWorktreeFormMirrorsTheDaemonsNameRules() {
        func form(_ name: String) -> NewWorktreeForm {
            NewWorktreeForm(projectPath: "/p", name: name, adapter: "claude")
        }
        for ok in ["a", "add-projects", "fix_bug", "v1.2", "M10a"] {
            XCTAssertTrue(form(ok).isValid, "should accept \(ok)")
            XCTAssertNil(form(ok).nameError)
        }
        for bad in ["", "   ", String(repeating: "a", count: 65), ".", "..", "a..b",
                    "x.lock", "my feature", "feat/x", ".hidden", "-dash", "v1.", "café"] {
            XCTAssertFalse(form(bad).isValid, "should reject \(bad)")
            XCTAssertNotNil(form(bad).nameError, "rejection must explain itself: \(bad)")
        }
    }

    func testNewWorktreeFormRequiresAProject() {
        XCTAssertFalse(NewWorktreeForm(projectPath: "", name: "ok", adapter: "claude").isValid)
    }

    func testNameErrorNamesTheProblem() {
        let e = NewWorktreeForm(projectPath: "/p", name: "my feature", adapter: "claude").nameError
        XCTAssertTrue(e?.contains("letters") == true, "unhelpful: \(e ?? "nil")")
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd macos && swift test --filter SheetFormsTests`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `SheetForms.swift`**

```swift
import Foundation

/// The Add Project sheet's state. The daemon validates for real (it must — in remote mode the
/// path is on another host); this only gates the button.
public struct AddProjectForm: Equatable, Sendable {
    public var path: String
    public init(path: String = "") { self.path = path }
    public var isValid: Bool { !path.trimmingCharacters(in: .whitespaces).isEmpty }
}

/// The New Worktree sheet's state. `nameError` mirrors the daemon's `validate_workspace_name`
/// so the sheet can explain a bad name immediately. The daemon remains the authority — a name
/// that slips through here still gets a clean error back.
public struct NewWorktreeForm: Equatable, Sendable {
    public var projectPath: String
    public var name: String
    public var adapter: String

    public init(projectPath: String = "", name: String = "", adapter: String = "claude") {
        self.projectPath = projectPath
        self.name = name
        self.adapter = adapter
    }

    public var isValid: Bool { !projectPath.isEmpty && nameError == nil }

    /// Nil when the name is acceptable; otherwise a user-facing reason.
    public var nameError: String? {
        let n = name.trimmingCharacters(in: .whitespaces)
        if n.isEmpty { return "Name must not be empty" }
        if n.count > 64 { return "Name must be 64 characters or fewer" }
        if n == "." || n == ".." { return "Name must not be \(n)" }
        if n.contains("..") { return "Name must not contain '..'" }
        if n.hasSuffix(".lock") { return "Name must not end with '.lock' (git reserves it)" }
        if n.hasSuffix(".") { return "Name must not end with '.' (git rejects it as a ref)" }
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "._-"))
        if n.unicodeScalars.contains(where: { !allowed.contains($0) || !$0.isASCII }) {
            return "Name may contain only letters, digits, '.', '_' or '-'"
        }
        if n.hasPrefix(".") || n.hasPrefix("-") { return "Name must not start with \(n.prefix(1))" }
        return nil
    }
}
```

Note `!$0.isASCII` alongside the `allowed` check: `CharacterSet.alphanumerics` accepts non-ASCII letters, and the daemon's rule is ASCII-only.

- [ ] **Step 4: Write the two sheets**

`AddProjectSheet.swift` — a path field plus a `Browse…` button that opens `NSOpenPanel`, hidden in remote mode where a local picker would return a path the daemon cannot see:

```swift
import SwiftUI
import AppKit
import ClowderCore

struct AddProjectSheet: View {
    /// False when attached to a remote daemon — a local directory picker is meaningless there.
    let canBrowse: Bool
    let onAdd: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var form = AddProjectForm()

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add Project").font(.headline)
            HStack {
                TextField("Path to a git or jj repository", text: $form.path)
                    .textFieldStyle(.roundedBorder)
                if canBrowse {
                    Button("Browse…") { browse() }
                }
            }
            Text("Must be a git or jj repository on the daemon's host.")
                .font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button("Add") {
                    onAdd(form.path.trimmingCharacters(in: .whitespaces))
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!form.isValid)
            }
        }
        .padding(20)
        .frame(width: 460)
    }

    private func browse() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url {
            form.path = url.path
        }
    }
}
```

`NewWorktreeSheet.swift` — project picker prefilled and editable, so `Cmd-N` stays unconditional:

```swift
import SwiftUI
import ClowderCore

struct NewWorktreeSheet: View {
    let projects: [SidebarProject]
    let adapters: [AdapterInfo]
    /// Prefill from the current selection, or the last-used project.
    let initialProjectPath: String
    let onCreate: (String, String, String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var form = NewWorktreeForm()

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("New Worktree").font(.headline)
            Form {
                Picker("Project", selection: $form.projectPath) {
                    ForEach(projects) { p in Text(p.name).tag(p.path) }
                }
                TextField("Name", text: $form.name).textFieldStyle(.roundedBorder)
                Picker("Agent", selection: $form.adapter) {
                    ForEach(adapters) { a in Text(a.displayName).tag(a.id) }
                }
            }
            if let err = form.nameError, !form.name.isEmpty {
                Text(err).font(.caption).foregroundStyle(.red)
            }
            HStack {
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button("Create") {
                    onCreate(form.projectPath,
                             form.name.trimmingCharacters(in: .whitespaces),
                             form.adapter)
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!form.isValid)
            }
        }
        .padding(20)
        .frame(width: 460)
        .onAppear {
            form.projectPath = initialProjectPath.isEmpty ? (projects.first?.path ?? "") : initialProjectPath
            form.adapter = adapters.first?.id ?? "claude"
        }
    }
}
```

Delete `SpawnSheet.swift`.

- [ ] **Step 5: Run the tests**

Run: `cd macos && swift test`
Expected: PASS. (The sheets themselves are not compiled — Task 6 wires them and CI checks them.)

- [ ] **Step 6: Commit**

```bash
git add macos/
git commit -m "feat(app): Add Project and New Worktree sheets

Validation lives in ClowderCore as AddProjectForm/NewWorktreeForm so it is unit
tested; the sheets are thin views over it. NewWorktreeForm mirrors the daemon's
validate_workspace_name — including the trailing-dot rule git rejects — so a bad
name is explained immediately, with the daemon still the authority.

Browse… is hidden in remote mode, where a local picker would return a path the
daemon cannot see.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 6: The sidebar, detail and menu bar

The only task whose code no local compiler checks. Everything it consumes is already proven by Tasks 2–5.

**Files:** Modify `macos/Sources/ClowderApp/ContentView.swift`, `StatusBarController.swift`, `CommandPaletteView.swift`, `App.swift`.

**Interfaces:** Consumes `AgentStore.sidebar`, `SidebarProject`, `SidebarSelection`, `AppModel.{selection, selectedPane, isEnabled, canRestartSelection, restartSelectedWorktree, addProject, removeProject, showingAddProject, showingNewWorktree}`, and the two sheets.

- [ ] **Step 1: Rewrite the sidebar**

Replace `ContentView`'s `sidebar` with a `List` over `SidebarProject`, using `DisclosureGroup` for nesting. Expansion state persists per project path.

```swift
    @State private var expanded: Set<String> = ContentView.loadExpanded()

    private var sidebar: some View {
        List(selection: $model.selection) {
            ForEach(model.store.sidebar) { project in
                DisclosureGroup(isExpanded: binding(for: project.path)) {
                    ForEach(project.worktrees) { worktree in
                        HStack(spacing: 8) {
                            Circle().fill(color(for: worktree.state)).frame(width: 8, height: 8)
                            Text(worktree.name).lineLimit(1)
                            Spacer()
                        }
                        .tag(SidebarSelection.worktree(worktree.pane))
                        .contextMenu { worktreeMenu(worktree) }
                    }
                } label: {
                    projectRow(project)
                        .tag(SidebarSelection.project(project.path))
                        .contextMenu { projectMenu(project) }
                }
            }
        }
        .overlay {
            if model.store.sidebar.isEmpty && model.connectionState == .live {
                Text("No projects yet — add one with +").foregroundStyle(.secondary)
            }
        }
    }

    private func binding(for path: String) -> Binding<Bool> {
        Binding(
            get: { expanded.contains(path) },
            set: { isOpen in
                if isOpen { expanded.insert(path) } else { expanded.remove(path) }
                ContentView.saveExpanded(expanded)
            }
        )
    }

    private static let expandedKey = "clowder.sidebar.expandedProjects"
    private static func loadExpanded() -> Set<String> {
        Set(UserDefaults.standard.stringArray(forKey: expandedKey) ?? [])
    }
    private static func saveExpanded(_ s: Set<String>) {
        UserDefaults.standard.set(Array(s), forKey: expandedKey)
    }
```

The project row carries name, kind badge, and the attention rollup — **the rollup is what stops a collapsed project hiding a waiting agent, so it must render regardless of expansion state**:

```swift
    private func projectRow(_ project: SidebarProject) -> some View {
        HStack(spacing: 6) {
            Image(systemName: project.kind == "jj"
                  ? "point.3.connected.trianglepath.dotted" : "arrow.triangle.branch")
                .foregroundStyle(.secondary)
                .help(project.kind == "jj" ? "jj workspace" : "git worktree")
            Text(project.name).lineLimit(1)
            Spacer()
            if project.attentionCount > 0 {
                Text("\(project.attentionCount)")
                    .font(.caption2).monospacedDigit()
                    .padding(.horizontal, 5).padding(.vertical, 1)
                    .background(Capsule().fill(Color.red.opacity(0.85)))
                    .foregroundStyle(.white)
                    .help("\(project.attentionCount) waiting for input")
            }
            Button {
                model.newWorktreeProject = project.path
                model.showingNewWorktree = true
            } label: { Image(systemName: "plus") }
            .buttonStyle(.plain)
            .help("New worktree in \(project.name)")
        }
    }
```

`model.newWorktreeProject` was added in Task 4 — the per-project `+` sets it so the sheet opens on that project.

Context menus:

```swift
    @ViewBuilder private func projectMenu(_ project: SidebarProject) -> some View {
        Button("New Worktree…") {
            model.newWorktreeProject = project.path
            model.showingNewWorktree = true
        }
        Button("Reveal in Finder") {
            NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: project.path)
        }
        Divider()
        Button("Remove Project", role: .destructive) { model.removeProject(path: project.path) }
    }

    @ViewBuilder private func worktreeMenu(_ worktree: WorktreeInfo) -> some View {
        if worktree.state == .exited {
            Button("Restart Agent") {
                model.selection = .worktree(worktree.pane)
                model.restartSelectedWorktree()
            }
            Divider()
        }
        Button("Land") { model.selection = .worktree(worktree.pane); model.requestLifecycle(.land) }
        Button("Discard", role: .destructive) {
            model.selection = .worktree(worktree.pane)
            model.requestLifecycle(.discard)
        }
    }
```

`Remove Project` is not gated in the UI — the daemon refuses while worktrees exist and its message names the count, which surfaces in the existing error banner. That keeps one authority for the rule.

- [ ] **Step 2: Update toolbar, sheets and detail**

Toolbar `+` becomes Add Project:

```swift
            ToolbarItem(placement: .primaryAction) {
                Button { model.showingAddProject = true } label: { Image(systemName: "plus") }
                    .disabled(model.connectionState != .live)
                    .help("Add a project")
            }
```

Replace the `showingSpawn` sheet with two:

```swift
        .sheet(isPresented: $model.showingAddProject) {
            AddProjectSheet(canBrowse: !model.isRemote) { path in model.addProject(path: path) }
        }
        .sheet(isPresented: $model.showingNewWorktree) {
            NewWorktreeSheet(projects: model.store.sidebar,
                             adapters: model.adapters,
                             initialProjectPath: model.newWorktreeProject) { project, name, adapter in
                model.spawn(project: project, name: name, adapter: adapter)
            }
        }
```

**`AppModel` has no remote flag, and should not gain one** — local-vs-remote is an app-launch concern, not a control-channel one. `App.swift:44-45` already resolves it (`let remoteHost = resolveRemoteHost(clowderBinary:)` → `configuredRemoteHost`), and `ContentView` is constructed at `App.swift:195`. So thread it in as an init parameter:

```swift
// ContentView
let surfaceHost: SurfaceHost
/// False in remote mode: a local NSOpenPanel would return a path the daemon cannot see.
let isRemote: Bool
```

```swift
// App.swift:195
ContentView(surfaceHost: boot.surfaceHost, isRemote: configuredRemoteHost != nil)
```

and use `AddProjectSheet(canBrowse: !isRemote)`. Confirm `configuredRemoteHost` is in scope at that construction site; if it is not, report where it lives rather than defaulting the flag — a wrong default silently hides `Browse…` from local users.

The detail view: `selectedPane` is now `nil` while a project's terminal is opening, so distinguish the three states.

```swift
    @ViewBuilder private var detail: some View {
        if let pane = model.selectedPane {
            if let worktree = model.store.worktrees[pane], worktree.state == .exited {
                exitedPlaceholder(worktree)
            } else {
                SplitContainer(node: model.currentTree ?? .leaf(pane: pane),
                               surfaceHost: surfaceHost,
                               focusedPane: $model.focusedPane)
                    .id(pane)
            }
        } else if case .project = model.selection {
            ProgressView("Starting terminal…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            Text("Select a project or worktree").foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
```

Give `exitedPlaceholder` a Restart button:

```swift
    private func exitedPlaceholder(_ worktree: WorktreeInfo) -> some View {
        VStack(spacing: 10) {
            Image(systemName: "moon.zzz.fill").font(.largeTitle).foregroundStyle(.secondary)
            Text("Agent exited").font(.title3)
            Text(worktree.name).foregroundStyle(.secondary)
            Button("Restart Agent") { model.restartSelectedWorktree() }
                .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
```

- [ ] **Step 3: Follow the renames elsewhere**

- `StatusBarController.swift`: its needy loop already uses `worktreesNeedingAttention`; its `selectAgent(_:)` sets a pane id — change it to set `model.selection = .worktree(pane)`.
- `CommandPaletteView.swift`: dim disabled rows using `model.isEnabled(id)` for `.command` items, and set `.worktree(pane)` when an agent row is chosen.
- `App.swift`: `.spawnAgent` in the keyboard/menu wiring becomes `.newWorktree`; add `.addProject`. Ignore a key event whose command is disabled (`model.isEnabled`).

Run `grep -rn "showingSpawn\|spawnAgent\|selectedPane =\|byProject" macos/Sources` and resolve every hit.

- [ ] **Step 4: Verify what can be verified**

Run: `cd macos && swift test`
Expected: PASS (ClowderCore only — this proves you did not break the tested layer).

Then **re-read every file you touched under `Sources/ClowderApp/`** against the current `ClowderCore` API: every symbol resolves, every argument label matches, no local shadows a property. List what you checked in your report. This is the only verification available before CI.

- [ ] **Step 5: Commit**

```bash
git add macos/
git commit -m "feat(app): projects sidebar, terminals and restart

The sidebar becomes projects containing worktrees: a kind badge, an attention
rollup that renders regardless of expansion so a collapsed project cannot hide a
waiting agent, a hover + and a context menu. Selecting a project opens a terminal
at the repo root; selecting a worktree gives its agent, as before. An exited
worktree offers Restart instead of being a dead end.

Remove Project is not gated in the UI — the daemon refuses while worktrees exist
and its message names the count, keeping one authority for the rule.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01QrYWovZ9oDuyDhbhRkksEC"
```

---

### Task 7: End-to-end verification against a real daemon

Nothing in Tasks 2–6 proves the app and daemon actually agree. This task runs them together.

**Files:** none necessarily — this is a verification task. Fix whatever it finds, in the file that owns the defect.

- [ ] **Step 1: Build the binaries**

Run: `source "$HOME/.cargo/env" && cargo build -p clowder-daemon -p clowder-client`

- [ ] **Step 2: Drive the daemon over the control socket**

**Socket paths must be short** — a long path exceeds `SUN_LEN` and the daemon fails to bind. Use `/tmp/clw-m10c`, not the scratchpad.

```bash
SC=/tmp/clw-m10c; rm -rf "$SC"; mkdir -p "$SC/run" "$SC/repo"
(cd "$SC/repo" && git init -q . && git config user.email t@t.test && git config user.name t \
   && echo hi > README.md && git add . && git commit -qm init)
export XDG_RUNTIME_DIR="$SC/run" CLOWDER_STATE_FILE="$SC/agents.json" CLOWDER_PROJECTS_FILE="$SC/projects.json"
./target/debug/clowder-daemon > "$SC/daemon.log" 2>&1 &
# wait for "$SC/run/clowder/clowder-control.sock" to exist, then:
./target/debug/clowder project add "$SC/repo"
./target/debug/clowder project list
./target/debug/clowder spawn "$SC/repo" feat shell
```

Then, with a raw socket client (`nc -U "$SC/run/clowder/clowder-control.sock"` or a short Rust/Python snippet), send each request the **app** sends and record the reply verbatim:

```json
{"type":"listProjects"}
{"type":"openProjectTerminal","path":"<canonical repo path>"}
{"type":"listWorktrees"}
{"type":"restartWorktree","pane":<pane of an exited agent>}
```

**For each reply, confirm Swift's `ControlEvent` decoder accepts it.** The fixtures cover the shapes, but this confirms the daemon emits exactly those shapes at runtime with real data — canonical paths, real pane ids. Any mismatch is a defect in whichever side deviates from `docs/protocol/fixtures/`.

- [ ] **Step 3: Record and clean up**

Put every request and its reply in your report. Then kill the daemon and `rm -rf "$SC"`.

- [ ] **Step 4: Commit only if you changed something**

If Step 2 found a mismatch, fix it and commit with a message naming the mismatch. If everything matched, say so in your report and make no commit.

---

## Final verification

- [ ] `source "$HOME/.cargo/env" && cargo test --workspace --locked` — PASS (this is what CI runs)
- [ ] `cd macos && swift test` — PASS
- [ ] `grep -rn "showingSpawn\|SpawnSheet\|byProject\|\.spawnAgent" macos/Sources macos/Tests` — no hits
- [ ] **`Sources/ClowderApp/` is unverified locally.** State plainly in the PR body which files CI will compile for the first time.
- [ ] Open the stacked PR — **base is `feat/m10b-projects-daemon`, NOT `main`**:
  ```bash
  git push -u origin feat/m10c-projects-app
  gh pr create --base feat/m10b-projects-daemon --title "M10c: projects app layer" --body "..."
  ```

## Notes for after the stack

- `AgentStore` is still named for agents though it now holds projects, worktrees, trees and adapters. Renaming it touches every test file; worth doing once the stack has landed.
- `PaletteItemKind.agent(pane:)` names a palette row, not the domain type — left alone deliberately.
- M10b's deferred items not addressed here: `ProjectTerminalOpened` is still not broadcast to other clients, and the four `*_via_control` CLI helpers still have no timeout. Both are recorded in M10b's plan.
