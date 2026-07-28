# muxy M1a — Command Palette + Keymap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Cmd-K command palette that fuzzy-searches commands **and** agents, plus keyboard-driven actions (Cmd-N spawn, Cmd-1…9 switch agent, Cmd-Shift-A next-attention) that work while a terminal is focused.

**Architecture:** A pure, Swift-native command/keymap/search model in `MuxyCore` (unit-tested, no SwiftUI); the macOS app maps it to an app-menu `Commands` block (whose Cmd-shortcuts fire via the responder chain over the focused terminal) and a `CommandPaletteView` overlay. Presentation state (`showingPalette`/`showingSpawn`) lives on `AppModel` so the menu and views share one source of truth.

**Tech Stack:** Swift 6 (language mode v5), SwiftUI + AppKit, Combine; `MuxyCore` (Foundation-only).

## Global Constraints

- **Deployment target = macOS 14.** `macos/Package.swift` `platforms` becomes `.macOS(.v14)` (from `.v13`) so the palette can use `.onKeyPress`. Safe: dev is on macOS 26, no external users, libghostty min is 13.
- **`MuxyCore` stays libghostty-free AND SwiftUI-free** — `Keymap`/`PaletteSearch` import only `Foundation`; `AppModel` may import `Combine`. `swift test` must run without the vendored lib. The app layer maps `KeyBinding` → SwiftUI `KeyboardShortcut`.
- **Cmd-modified shortcuts only** — every default binding includes `.command`, so the menu intercepts it before the terminal `SurfaceView`.
- Commit after each task with a conventional message and the standard trailers.

**Test commands:**
- Core: `cd macos && swift test`
- App build gate (UI tasks): `cd macos && swift build`

---

## Task 1: Keymap model (MuxyCore)

**Files:**
- Create: `macos/Sources/MuxyCore/Keymap.swift`
- Test: `macos/Tests/MuxyCoreTests/KeymapTests.swift`

**Interfaces:**
- Produces: `CommandID`, `KeyModifiers`, `KeyBinding`, `Command`, `Keymap` (+ `Keymap.defaults`, `binding(for:)`), `CommandRegistry.all(keymap:)`.

- [ ] **Step 1: Write the failing test** (`KeymapTests.swift`):

```swift
import XCTest
@testable import MuxyCore

final class KeymapTests: XCTestCase {
    func testDefaultBindings() {
        let k = Keymap()
        XCTAssertEqual(k.binding(for: .openPalette), KeyBinding("k", .command))
        XCTAssertEqual(k.binding(for: .spawnAgent), KeyBinding("n", .command))
        XCTAssertEqual(k.binding(for: .nextAttention), KeyBinding("a", [.command, .shift]))
        XCTAssertEqual(k.binding(for: .switchToAgent(1)), KeyBinding("1", .command))
        XCTAssertEqual(k.binding(for: .switchToAgent(9)), KeyBinding("9", .command))
    }

    func testOverrideWins() {
        let k = Keymap(overrides: [.spawnAgent: KeyBinding("m", .command)])
        XCTAssertEqual(k.binding(for: .spawnAgent), KeyBinding("m", .command))
        XCTAssertEqual(k.binding(for: .openPalette), KeyBinding("k", .command)) // default untouched
    }

    func testRegistryRows() {
        let rows = CommandRegistry.all(keymap: Keymap())
        XCTAssertEqual(rows.map(\.id), [.spawnAgent, .nextAttention])
        XCTAssertEqual(rows[0].title, "Spawn Agent")
        XCTAssertEqual(rows[0].defaultShortcut, KeyBinding("n", .command))
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter KeymapTests`
Expected: FAIL — no `Keymap` type.

- [ ] **Step 3: Implement `Keymap.swift`:**

```swift
import Foundation

public enum CommandID: Hashable, Sendable {
    case openPalette
    case spawnAgent
    case nextAttention
    case switchToAgent(Int)   // 1-based position in the ordered agent list
}

public struct KeyModifiers: OptionSet, Hashable, Sendable {
    public let rawValue: Int
    public init(rawValue: Int) { self.rawValue = rawValue }
    public static let command = KeyModifiers(rawValue: 1 << 0)
    public static let shift   = KeyModifiers(rawValue: 1 << 1)
    public static let option  = KeyModifiers(rawValue: 1 << 2)
    public static let control = KeyModifiers(rawValue: 1 << 3)
}

public struct KeyBinding: Hashable, Sendable {
    public let key: Character
    public let modifiers: KeyModifiers
    public init(_ key: Character, _ modifiers: KeyModifiers) {
        self.key = key
        self.modifiers = modifiers
    }
}

public struct Command: Identifiable, Sendable {
    public let id: CommandID
    public let title: String
    public let subtitle: String?
    public var defaultShortcut: KeyBinding?
    public init(id: CommandID, title: String, subtitle: String? = nil, defaultShortcut: KeyBinding? = nil) {
        self.id = id
        self.title = title
        self.subtitle = subtitle
        self.defaultShortcut = defaultShortcut
    }
}

public struct Keymap: Sendable {
    private let overrides: [CommandID: KeyBinding]
    public init(overrides: [CommandID: KeyBinding] = [:]) { self.overrides = overrides }

    public static let defaults: [CommandID: KeyBinding] = {
        var m: [CommandID: KeyBinding] = [
            .openPalette:   KeyBinding("k", .command),
            .spawnAgent:    KeyBinding("n", .command),
            .nextAttention: KeyBinding("a", [.command, .shift]),
        ]
        for i in 1...9 { m[.switchToAgent(i)] = KeyBinding(Character("\(i)"), .command) }
        return m
    }()

    public func binding(for id: CommandID) -> KeyBinding? {
        overrides[id] ?? Keymap.defaults[id]
    }
}

public enum CommandRegistry {
    /// Palette-visible commands (rows). `switchToAgent`/`openPalette` are bindings, not
    /// rows — agent-switching lives in the palette's agent section.
    public static func all(keymap: Keymap) -> [Command] {
        [
            Command(id: .spawnAgent, title: "Spawn Agent",
                    subtitle: "Start a new agent",
                    defaultShortcut: keymap.binding(for: .spawnAgent)),
            Command(id: .nextAttention, title: "Next Attention",
                    subtitle: "Jump to the next agent needing input",
                    defaultShortcut: keymap.binding(for: .nextAttention)),
        ]
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd macos && swift test`
Expected: PASS — existing 35 + the 3 new `KeymapTests`.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/MuxyCore/Keymap.swift macos/Tests/MuxyCoreTests/KeymapTests.swift
git commit -m "feat(core): command/keymap model + default bindings"
```

---

## Task 2: Ordered agents, navigation, UI-intent state, `run(_:)` (MuxyCore)

**Files:**
- Modify: `macos/Sources/MuxyCore/AgentStore.swift` (add `orderedAgents`)
- Modify: `macos/Sources/MuxyCore/AppModel.swift` (add `showingPalette`/`showingSpawn`, `selectAgent(atIndex:)`, `selectNextAttention()`, `run(_:)`)
- Test: `macos/Tests/MuxyCoreTests/NavigationTests.swift`

**Interfaces:**
- Consumes: `AgentStore.byProject`, `AttentionState`, `CommandID` (Task 1).
- Produces: `AgentStore.orderedAgents: [AgentInfo]`; `AppModel.showingPalette`/`showingSpawn` (`@Published`), `selectAgent(atIndex:)`, `selectNextAttention()`, `run(_ id: CommandID)`.

- [ ] **Step 1: Write the failing test** (`NavigationTests.swift`) — reuses `FakeControlTransport` from `AppModelTests.swift` (same test target):

```swift
import XCTest
@testable import MuxyCore

@MainActor
final class NavigationTests: XCTestCase {
    private func modelWithAgents() -> AppModel {
        let fake = FakeControlTransport()
        let model = AppModel(makeTransport: { fake })
        model.connect()
        fake.deliver(#"{"type":"agentList","agents":[{"pane":1,"project":"/a","task":"t1","state":"Working"},{"pane":2,"project":"/a","task":"t2","state":"NeedsInput"},{"pane":3,"project":"/b","task":"t3","state":"NeedsInput"}]}"#)
        return model
    }

    func testOrderedAgentsIsByProjectFlatten() {
        let m = modelWithAgents()
        XCTAssertEqual(m.store.orderedAgents.map(\.pane), [1, 2, 3]) // /a (pane 1,2) then /b (pane 3)
    }

    func testSelectAgentAtIndexIsOneBasedAndBounded() {
        let m = modelWithAgents()
        m.selectAgent(atIndex: 2)
        XCTAssertEqual(m.selectedPane, 2)
        m.selectAgent(atIndex: 99)      // out of range -> unchanged
        XCTAssertEqual(m.selectedPane, 2)
        m.selectAgent(atIndex: 0)       // 0 invalid (1-based) -> unchanged
        XCTAssertEqual(m.selectedPane, 2)
    }

    func testNextAttentionCyclesNeedyOnly() {
        let m = modelWithAgents()
        m.selectNextAttention()                     // nothing selected -> first needy (2)
        XCTAssertEqual(m.selectedPane, 2)
        m.selectNextAttention()                     // -> next needy (3)
        XCTAssertEqual(m.selectedPane, 3)
        m.selectNextAttention()                     // cycle -> 2
        XCTAssertEqual(m.selectedPane, 2)
    }

    func testNextAttentionNoOpWhenNoneNeedy() {
        let fake = FakeControlTransport()
        let m = AppModel(makeTransport: { fake })
        m.connect()
        fake.deliver(#"{"type":"agentList","agents":[{"pane":1,"project":"/a","task":"t","state":"Working"}]}"#)
        m.selectedPane = 1
        m.selectNextAttention()
        XCTAssertEqual(m.selectedPane, 1)           // unchanged
    }

    func testRunDispatch() {
        let m = modelWithAgents()
        m.run(.openPalette); XCTAssertTrue(m.showingPalette)
        m.run(.openPalette); XCTAssertFalse(m.showingPalette)   // toggles
        m.run(.spawnAgent); XCTAssertTrue(m.showingSpawn)
        m.run(.switchToAgent(1)); XCTAssertEqual(m.selectedPane, 1)
        m.run(.nextAttention); XCTAssertEqual(m.selectedPane, 2) // first needy after pane 1
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter NavigationTests`
Expected: FAIL — `orderedAgents`/`selectAgent`/`run` don't exist.

- [ ] **Step 3: Add `orderedAgents` to `AgentStore.swift`** (near `byProject`):

```swift
    /// The sidebar order flattened: agents grouped by project, projects sorted, agents by pane.
    /// The stable index order used by Cmd-1…9 and the palette.
    public var orderedAgents: [AgentInfo] { byProject.flatMap { $0.agents } }
```

- [ ] **Step 4: Add the state + methods to `AppModel.swift`.** Add the published intents next to the existing `@Published` properties:

```swift
    @Published public var showingPalette: Bool = false
    @Published public var showingSpawn: Bool = false
```

Add the methods (e.g. after `spawn`):

```swift
    /// Select the 1-based Nth agent in the ordered list (Cmd-N). No-op if out of range.
    public func selectAgent(atIndex index: Int) {
        let ordered = store.orderedAgents
        guard index >= 1, index <= ordered.count else { return }
        selectedPane = ordered[index - 1].pane
    }

    /// Select the next agent needing input after the current selection, cycling. If the
    /// current selection isn't needy, select the first needy one; no-op if none need input.
    public func selectNextAttention() {
        let needy = store.orderedAgents.filter { $0.state == .needsInput }
        guard !needy.isEmpty else { return }
        if let cur = selectedPane, let idx = needy.firstIndex(where: { $0.pane == cur }) {
            selectedPane = needy[(idx + 1) % needy.count].pane
        } else {
            selectedPane = needy[0].pane
        }
    }

    /// Dispatch a command by id.
    public func run(_ id: CommandID) {
        switch id {
        case .openPalette: showingPalette.toggle()
        case .spawnAgent: showingSpawn = true
        case .nextAttention: selectNextAttention()
        case let .switchToAgent(i): selectAgent(atIndex: i)
        }
    }
```

- [ ] **Step 5: Run to verify it passes**

Run: `cd macos && swift test`
Expected: PASS — existing + `NavigationTests`.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/MuxyCore/AgentStore.swift macos/Sources/MuxyCore/AppModel.swift macos/Tests/MuxyCoreTests/NavigationTests.swift
git commit -m "feat(core): ordered agents, keyboard navigation, command dispatch"
```

---

## Task 3: Palette fuzzy search (MuxyCore)

**Files:**
- Create: `macos/Sources/MuxyCore/PaletteSearch.swift`
- Test: `macos/Tests/MuxyCoreTests/PaletteSearchTests.swift`

**Interfaces:**
- Consumes: `Command`, `CommandID` (Task 1), `AgentInfo`.
- Produces: `PaletteItemKind`, `PaletteItem`, `paletteResults(query:commands:agents:)`.

- [ ] **Step 1: Write the failing test** (`PaletteSearchTests.swift`):

```swift
import XCTest
@testable import MuxyCore

final class PaletteSearchTests: XCTestCase {
    private let cmds = CommandRegistry.all(keymap: Keymap())
    private func agents() -> [AgentInfo] {
        [AgentInfo(pane: 1, project: "/home/api", task: "fix login", state: .working),
         AgentInfo(pane: 2, project: "/home/web", task: "spawn worker", state: .idle)]
    }
    private func isCommand(_ i: PaletteItem) -> Bool { if case .command = i.kind { return true }; return false }
    private func isAgent(_ i: PaletteItem, _ pane: UInt64) -> Bool {
        if case let .agent(p) = i.kind { return p == pane }; return false
    }

    func testEmptyQueryReturnsAllCommandsThenAgents() {
        let r = paletteResults(query: "", commands: cmds, agents: agents())
        XCTAssertEqual(r.count, 4)
        XCTAssertTrue(isCommand(r[0]) && isCommand(r[1]))
        XCTAssertTrue(!isCommand(r[2]) && !isCommand(r[3]))
    }

    func testCommandQueryRanksCommandFirst() {
        let r = paletteResults(query: "spawn ag", commands: cmds, agents: agents())
        XCTAssertEqual(r.first?.title, "Spawn Agent")
        XCTAssertTrue(isCommand(r[0]))
    }

    func testAgentQueryMatchesOnlyAgent() {
        let r = paletteResults(query: "login", commands: cmds, agents: agents())
        XCTAssertTrue(r.contains { isAgent($0, 1) })
        XCTAssertFalse(r.contains { isCommand($0) })
    }

    func testNoMatchIsEmpty() {
        XCTAssertTrue(paletteResults(query: "zzzzz", commands: cmds, agents: agents()).isEmpty)
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd macos && swift test --filter PaletteSearchTests`
Expected: FAIL — no `paletteResults`.

- [ ] **Step 3: Implement `PaletteSearch.swift`:**

```swift
import Foundation

public enum PaletteItemKind: Hashable, Sendable {
    case command(CommandID)
    case agent(pane: UInt64)
}

public struct PaletteItem: Identifiable, Sendable {
    public let id: PaletteItemKind
    public let title: String
    public let subtitle: String?
    public let kind: PaletteItemKind
    public init(id: PaletteItemKind, title: String, subtitle: String?, kind: PaletteItemKind) {
        self.id = id
        self.title = title
        self.subtitle = subtitle
        self.kind = kind
    }
}

/// Case-insensitive subsequence match. Returns a rank (lower is better: the index of the
/// first matched character, so a prefix match ranks best) or nil if `text` doesn't contain
/// `query` as a subsequence. Empty query matches everything at rank 0.
func fuzzyRank(_ query: String, _ text: String) -> Int? {
    if query.isEmpty { return 0 }
    let q = Array(query.lowercased())
    let t = Array(text.lowercased())
    var qi = 0
    var firstMatch: Int?
    for (ti, ch) in t.enumerated() where qi < q.count && ch == q[qi] {
        if firstMatch == nil { firstMatch = ti }
        qi += 1
    }
    return qi == q.count ? (firstMatch ?? 0) : nil
}

/// Fuzzy-filter commands (matched on title) and agents (matched on "project task") into one
/// ranked list — commands section first, then agents. Ties keep input order.
public func paletteResults(query: String, commands: [Command], agents: [AgentInfo]) -> [PaletteItem] {
    let trimmed = query.trimmingCharacters(in: .whitespaces)

    let cmdItems = commands.enumerated().compactMap { (i, c) -> (Int, Int, PaletteItem)? in
        guard let r = fuzzyRank(trimmed, c.title) else { return nil }
        return (r, i, PaletteItem(id: .command(c.id), title: c.title, subtitle: c.subtitle, kind: .command(c.id)))
    }
    let agentItems = agents.enumerated().compactMap { (i, a) -> (Int, Int, PaletteItem)? in
        guard let r = fuzzyRank(trimmed, "\(a.project) \(a.task)") else { return nil }
        let proj = (a.project as NSString).lastPathComponent
        let sub = proj.isEmpty ? a.project : proj
        return (r, i, PaletteItem(id: .agent(pane: a.pane), title: a.task, subtitle: sub, kind: .agent(pane: a.pane)))
    }

    let sortedCmds = cmdItems.sorted { ($0.0, $0.1) < ($1.0, $1.1) }.map(\.2)
    let sortedAgents = agentItems.sorted { ($0.0, $0.1) < ($1.0, $1.1) }.map(\.2)
    return sortedCmds + sortedAgents
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd macos && swift test`
Expected: PASS — all prior + `PaletteSearchTests`.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/MuxyCore/PaletteSearch.swift macos/Tests/MuxyCoreTests/PaletteSearchTests.swift
git commit -m "feat(core): unified fuzzy palette search over commands + agents"
```

---

## Task 4: Deployment bump + app-menu Commands + hoist spawn state (MuxyApp)

Wire the keymap to real menu shortcuts (which fire over the focused terminal) and move the spawn-sheet presentation onto `AppModel`. Gate: `swift build`.

**Files:**
- Modify: `macos/Package.swift` (`.macOS(.v13)` → `.macOS(.v14)`)
- Modify: `macos/Sources/MuxyApp/App.swift` (add `.commands { … }` + `KeyBinding`→`KeyboardShortcut` mapping)
- Modify: `macos/Sources/MuxyApp/ContentView.swift` (use `model.showingSpawn` instead of a local `@State`)

**Interfaces:**
- Consumes: `AppModel.run(_:)`, `Keymap`, `CommandID`, `KeyBinding`/`KeyModifiers` (Tasks 1–2).

- [ ] **Step 1: Bump the deployment target** in `macos/Package.swift`:

```swift
    platforms: [
        .macOS(.v14),
    ],
```

- [ ] **Step 2: Hoist the spawn-sheet state in `ContentView.swift`.** Remove the local state and use the model's:
  - Delete `@State private var showingSpawn = false`.
  - Change the toolbar button action from `showingSpawn = true` to `model.showingSpawn = true` (keep the `.disabled(model.connectionState != .live)`).
  - Change `.sheet(isPresented: $showingSpawn)` to `.sheet(isPresented: $model.showingSpawn)`.

- [ ] **Step 3: Add the app menu + shortcut mapping in `App.swift`.** Add a `.commands` modifier on the `WindowGroup` scene and the mapping helpers on the `MuxyApp` struct:

```swift
@main
struct MuxyApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    private let keymap = Keymap()

    var body: some Scene {
        WindowGroup {
            let boot = delegate.bootstrap()
            ContentView(surfaceHost: boot.surfaceHost)
                .environmentObject(boot.appModel)
                .frame(minWidth: 900, minHeight: 560)
        }
        .commands {
            CommandMenu("muxy") {
                menuItem("Command Palette", .openPalette)
                menuItem("Spawn Agent", .spawnAgent)
                menuItem("Next Attention", .nextAttention)
                Divider()
                ForEach(1...9, id: \.self) { i in
                    menuItem("Switch to Agent \(i)", .switchToAgent(i))
                }
            }
        }
    }

    // A menu button that runs a command via the shared AppModel and carries its shortcut.
    @ViewBuilder
    private func menuItem(_ title: String, _ id: CommandID) -> some View {
        Button(title) { delegate.appModel?.run(id) }
            .keyboardShortcut(shortcut(id))
    }

    private func shortcut(_ id: CommandID) -> KeyboardShortcut {
        guard let b = keymap.binding(for: id) else { return KeyboardShortcut("?", modifiers: []) }
        return KeyboardShortcut(KeyEquivalent(b.key), modifiers: eventModifiers(b.modifiers))
    }

    private func eventModifiers(_ m: KeyModifiers) -> EventModifiers {
        var e: EventModifiers = []
        if m.contains(.command) { e.insert(.command) }
        if m.contains(.shift)   { e.insert(.shift) }
        if m.contains(.option)  { e.insert(.option) }
        if m.contains(.control) { e.insert(.control) }
        return e
    }
}
```

> `delegate.appModel` is the same instance the scene injects as an `environmentObject`, so the menu and the views share state. It is populated by `bootstrap()` before any menu action can fire.

- [ ] **Step 4: Build**

Run: `cd macos && swift build`
Expected: builds on the macOS 14 target.

- [ ] **Step 5: Manual smoke (recorded).** With a daemon + a couple of agents: Cmd-N opens the spawn sheet (and while a terminal is focused); Cmd-1…9 switch the selected agent; Cmd-Shift-A jumps to the next needs-input agent; the toolbar `+` still spawns. Cmd-K currently just toggles `showingPalette` with no visible UI yet (Task 5 adds the overlay).

- [ ] **Step 6: Commit**

```bash
git add macos/Package.swift macos/Sources/MuxyApp/App.swift macos/Sources/MuxyApp/ContentView.swift
git commit -m "feat(app): macOS 14 target, command menu + keyboard shortcuts, shared spawn state"
```

---

## Task 5: `CommandPaletteView` overlay (MuxyApp)

The Cmd-K palette itself: search field, unified command+agent results, keyboard navigation. Gate: `swift build`.

**Files:**
- Create: `macos/Sources/MuxyApp/CommandPaletteView.swift`
- Modify: `macos/Sources/MuxyApp/ContentView.swift` (present the overlay when `model.showingPalette`)

**Interfaces:**
- Consumes: `AppModel` (`store.orderedAgents`, `showingPalette`, `run(_:)`, `selectedPane`), `paletteResults`, `CommandRegistry`, `Keymap`, `PaletteItem`.

- [ ] **Step 1: Create `CommandPaletteView.swift`:**

```swift
import SwiftUI
import MuxyCore

struct CommandPaletteView: View {
    @EnvironmentObject var model: AppModel
    @State private var query = ""
    @State private var selectedIndex = 0
    @FocusState private var fieldFocused: Bool
    private let keymap = Keymap()

    private var results: [PaletteItem] {
        paletteResults(query: query,
                       commands: CommandRegistry.all(keymap: keymap),
                       agents: model.store.orderedAgents)
    }

    var body: some View {
        VStack(spacing: 0) {
            TextField("Search commands and agents…", text: $query)
                .textFieldStyle(.plain)
                .font(.title3)
                .padding(12)
                .focused($fieldFocused)
                .onSubmit { runSelected() }
            Divider()
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(results.enumerated()), id: \.element.id) { idx, item in
                            row(item, selected: idx == selectedIndex)
                                .id(idx)
                                .contentShape(Rectangle())
                                .onTapGesture { selectedIndex = idx; runSelected() }
                        }
                    }
                }
                .frame(maxHeight: 320)
                .onChange(of: selectedIndex) { proxy.scrollTo(selectedIndex, anchor: .center) }
            }
        }
        .frame(width: 560)
        .background(.regularMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .shadow(radius: 20)
        .onAppear { fieldFocused = true; selectedIndex = 0 }
        .onChange(of: query) { selectedIndex = 0 }
        .onKeyPress(.downArrow) { move(1); return .handled }
        .onKeyPress(.upArrow) { move(-1); return .handled }
        .onExitCommand { close() }   // Esc
    }

    private func row(_ item: PaletteItem, selected: Bool) -> some View {
        HStack(spacing: 8) {
            Image(systemName: icon(item.kind)).frame(width: 18)
            VStack(alignment: .leading, spacing: 1) {
                Text(item.title)
                if let s = item.subtitle {
                    Text(s).font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer()
        }
        .padding(.horizontal, 12).padding(.vertical, 7)
        .background(selected ? Color.accentColor.opacity(0.25) : Color.clear)
    }

    private func icon(_ kind: PaletteItemKind) -> String {
        switch kind {
        case .command: return "command"
        case .agent: return "terminal"
        }
    }

    private func move(_ delta: Int) {
        guard !results.isEmpty else { return }
        selectedIndex = max(0, min(results.count - 1, selectedIndex + delta))
    }

    private func runSelected() {
        guard results.indices.contains(selectedIndex) else { return }
        switch results[selectedIndex].kind {
        case let .command(id): model.run(id)
        case let .agent(pane): model.selectedPane = pane
        }
        close()
    }

    private func close() {
        query = ""
        model.showingPalette = false
    }
}
```

- [ ] **Step 2: Present the overlay in `ContentView.swift`.** Add an `.overlay` on the `NavigationSplitView` (after the existing `.sheet`/`.safeAreaInset` modifiers):

```swift
        .overlay {
            if model.showingPalette {
                ZStack(alignment: .top) {
                    Color.black.opacity(0.2).ignoresSafeArea()
                        .onTapGesture { model.showingPalette = false }
                    CommandPaletteView()
                        .padding(.top, 80)
                }
            }
        }
```

- [ ] **Step 3: Build**

Run: `cd macos && swift build`
Expected: builds.

- [ ] **Step 4: Manual smoke (recorded; user runs the full pass).** Cmd-K opens the palette with the field focused; typing filters commands + agents; ↑/↓ moves the highlight and Enter runs the command / jumps to the agent; clicking a row runs it; Esc or clicking the dimmed background closes. Verify Cmd-K works while a terminal is focused, and that running "Spawn Agent" from the palette opens the spawn sheet.

> If `.onKeyPress(.upArrow/.downArrow)` on the container does not receive arrow keys while the `TextField` is focused (a known SwiftUI focus wrinkle), move those two `.onKeyPress` modifiers onto the `TextField` itself; keep Enter on `.onSubmit` and Esc on `.onExitCommand`. Confirm arrow navigation works before committing.

- [ ] **Step 5: Commit**

```bash
git add macos/Sources/MuxyApp/CommandPaletteView.swift macos/Sources/MuxyApp/ContentView.swift
git commit -m "feat(app): Cmd-K command palette overlay (commands + agents)"
```

---

## Final verification

- `cd macos && swift test` → existing 35 + `KeymapTests` + `NavigationTests` + `PaletteSearchTests`, all green.
- `cd macos && swift build` → builds clean on the macOS 14 target.
- Manual (user): Cmd-K palette filters commands + agents and runs/jumps with the keyboard; Cmd-N / Cmd-1–9 / Cmd-Shift-A work while a terminal is focused; the toolbar `+` still spawns.
