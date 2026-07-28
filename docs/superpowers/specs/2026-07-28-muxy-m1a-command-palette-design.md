# muxy M1a — Command Palette + Keymap

## Context

M0 shipped a working native macOS app: a sidebar of agents grouped by project with
attention badges, GUI spawn, and a driven terminal per selected agent. M1 (multi-agent
polish) is decomposed into independent slices — companion split panes, command palette +
keymap, tray attention count, snapshot-on-attach. This is the **first slice, M1a: the
command palette + keymap + hotkeys**, chosen first because the command/keymap layer is
foundational — later slices (split-pane commands, etc.) register their commands into it.

Decisions taken during brainstorming:
- **Swift-native** command registry + keymap (in `MuxyCore`), not the north-star's shared
  Rust `muxy-keymap` crate. The macOS client is the only client today; commands are
  inherently client-side actions; a Rust↔Swift FFI seam buys nothing for a macOS-only
  milestone. The Rust crate can mirror this model when the Linux client lands (M5).
- The **Cmd-K palette searches both commands and agents** (unified "do or go" palette).
- **Cmd-modified shortcuts only** — non-Cmd keys belong to the terminal.
- **Default bindings shipped; the keymap is a rebindable data model, but no in-app
  rebinding UI** in M1a (deferred).

### What exists (ground truth)

`MuxyCore` (`macos/Sources/MuxyCore/`): `AgentStore` (`@Published agents: [UInt64:AgentInfo]`,
`byProject: [(project,[AgentInfo])]` sorted, `lastError`, `apply`), `AppModel`
(`@MainActor ObservableObject`: `store`, `@Published selectedPane: UInt64?`,
`connectionState`, `connect/spawn/shutdown/dismissError`, republishes the store),
`AgentInfo {pane, project, task, state}`, `AttentionState {.idle/.working/.needsInput/.completed/.exited}`.

`MuxyApp`: `App.swift` (`@main` + `AppDelegate` bootstrap, `WindowGroup { ContentView }`),
`ContentView` (`NavigationSplitView`, sidebar `List(selection: $model.selectedPane)`,
toolbar `+` → local `@State showingSpawn` → `.sheet(SpawnSheet)`), `SpawnSheet(onSpawn:)`,
`SurfaceView`/`SurfaceHost`/`TerminalContainer`.

`macos/Package.swift`: `platforms: [.macOS(.v13)]`.

## Goals / Non-goals

**Goals:** open a palette with Cmd-K, fuzzy-search commands **and** agents, run/jump with
Enter; drive the app from the keyboard while a terminal is focused — Cmd-N spawn, Cmd-1…9
switch to agent N, Cmd-Shift-A jump to the next agent needing input.

**Non-goals (later):** an in-app keybinding-editor UI; persisting custom bindings to disk;
split-pane / teardown / companion commands (their slices register into this registry
later); a Linux keymap crate.

## Global constraints

- **Deployment target → macOS 14.** Bump `Package.swift` `platforms` from `.macOS(.v13)`
  to `.macOS(.v14)` so the palette can use SwiftUI `.onKeyPress` for ↑/↓/Enter/Esc. (The
  developer runs macOS 26; there is no external user base; libghostty targets 13, so 14 is
  safe.)
- **`MuxyCore` stays libghostty-free and SwiftUI-free** — the command/keymap/search model
  is pure Foundation, so `swift test` runs without the vendored lib. The app layer maps
  `KeyBinding` → SwiftUI `KeyboardShortcut`.
- **Cmd-modified shortcuts only.** Every binding includes `.command`, so the menu/responder
  chain intercepts it before the terminal `SurfaceView`.

## Component design

### MuxyCore (pure, unit-tested)

**`CommandID`** — the set of app commands:

```swift
public enum CommandID: Hashable, Sendable {
    case openPalette
    case spawnAgent
    case nextAttention
    case switchToAgent(Int)   // 1-based position in the ordered agent list
}
```

**`KeyModifiers`** — a pure OptionSet mirroring the modifier keys (no SwiftUI import):

```swift
public struct KeyModifiers: OptionSet, Hashable, Sendable {
    public let rawValue: Int
    public init(rawValue: Int) { self.rawValue = rawValue }
    public static let command = KeyModifiers(rawValue: 1 << 0)
    public static let shift   = KeyModifiers(rawValue: 1 << 1)
    public static let option  = KeyModifiers(rawValue: 1 << 2)
    public static let control = KeyModifiers(rawValue: 1 << 3)
}
```

**`KeyBinding`** — a pure key + modifiers value; the app maps it to `KeyboardShortcut`:

```swift
public struct KeyBinding: Hashable, Sendable {
    public let key: Character
    public let modifiers: KeyModifiers
    public init(_ key: Character, _ modifiers: KeyModifiers) { self.key = key; self.modifiers = modifiers }
}
```

**`Command`** — a palette-visible command's metadata:

```swift
public struct Command: Identifiable, Sendable {
    public let id: CommandID
    public let title: String
    public let subtitle: String?
    public var defaultShortcut: KeyBinding?
}
```

**`Keymap`** — bindings for every bound command, defaults + overrides:

```swift
public struct Keymap: Sendable {
    private var overrides: [CommandID: KeyBinding]
    public init(overrides: [CommandID: KeyBinding] = [:]) { self.overrides = overrides }
    public func binding(for id: CommandID) -> KeyBinding?   // override ?? default
    public static let defaults: [CommandID: KeyBinding]      // the shipped bindings
}
```

Default bindings (`Keymap.defaults`):
- `.openPalette` → Cmd-K
- `.spawnAgent` → Cmd-N
- `.nextAttention` → Cmd-Shift-A
- `.switchToAgent(1…9)` → Cmd-1 … Cmd-9

**`CommandRegistry`** — the palette-visible commands (rows). `switchToAgent`/`openPalette`
are bindings, not rows (agent-switching lives in the palette's agent section; you don't
list "open palette" while in it):

```swift
public enum CommandRegistry {
    public static func all(keymap: Keymap) -> [Command]   // [.spawnAgent, .nextAttention] with titles + shortcuts
}
```

**Palette search** — fuzzy-filter commands + agents into one ranked list:

```swift
public enum PaletteItemKind: Hashable, Sendable {
    case command(CommandID)
    case agent(pane: UInt64)
}
public struct PaletteItem: Identifiable, Sendable {
    public let id: PaletteItemKind      // stable identity
    public let title: String
    public let subtitle: String?
    public let kind: PaletteItemKind
}
/// Case-insensitive subsequence match. Commands match on title; agents match on
/// "project task". Ranking: exact/prefix > earlier-first-match subsequence. Empty query
/// returns all commands then all agents. Commands section first, then agents.
public func paletteResults(query: String, commands: [Command], agents: [AgentInfo]) -> [PaletteItem]
```

**Ordered agents + navigation** (the stable order = the sidebar order):

```swift
extension AgentStore {
    public var orderedAgents: [AgentInfo] { byProject.flatMap { $0.agents } }
}
extension AppModel {
    /// Select the 1-based Nth agent in orderedAgents (Cmd-N). No-op if out of range.
    public func selectAgent(atIndex index: Int)
    /// Select the next agent whose state == .needsInput after the current selection,
    /// cycling; if the current selection isn't needy, select the first needy one; no-op
    /// if none need input.
    public func selectNextAttention()
    /// Run a command by id (spawn/nextAttention/switchToAgent/openPalette toggle).
    public func run(_ id: CommandID)
}
```

**App-level UI intents on `AppModel`** — the menu and the views share one observable, so
presentation state moves onto `AppModel`:

```swift
@Published public var showingPalette: Bool = false
@Published public var showingSpawn: Bool = false
```

`run(_:)` maps: `.openPalette` → toggle `showingPalette`; `.spawnAgent` → `showingSpawn = true`;
`.nextAttention` → `selectNextAttention()`; `.switchToAgent(i)` → `selectAgent(atIndex: i)`.

### MuxyApp (UI / wiring)

**App menu `Commands`** (in `App.swift`'s `Scene`, `.commands { … }`): a `CommandMenu`
(e.g. "muxy") whose items carry `.keyboardShortcut(mapping(keymap.binding(for: id)))`.
Because these are menu items, the shortcuts fire through the responder chain **even while
the terminal `SurfaceView` is first responder**. Items: Command Palette (Cmd-K), Spawn
Agent (Cmd-N), Next Attention (Cmd-Shift-A), and a "Switch to Agent" group with 1…9
(Cmd-1…Cmd-9). Each item's action calls `appModel.run(id)`. A small helper maps
`KeyBinding` → SwiftUI `KeyboardShortcut` (`KeyEquivalent(key)`, `EventModifiers` from
`KeyModifiers`). The menu reads `appModel` via the same instance the scene owns.

**`CommandPaletteView`** (new `macos/Sources/MuxyApp/CommandPaletteView.swift`) — presented
as an overlay when `appModel.showingPalette`:
- a search `TextField` (focused on appear via `@FocusState`), showing `paletteResults`
  for the live query over `CommandRegistry.all(keymap:)` + `store.orderedAgents`;
- results in a scrolling list with a highlighted `selectedIndex`;
- `.onKeyPress(.upArrow/.downArrow)` moves `selectedIndex`; `.onKeyPress(.return)` runs the
  selected item; `.onKeyPress(.escape)` (or `.onExitCommand`) closes;
- running a `.command(id)` item calls `appModel.run(id)`; running an `.agent(pane)` item
  sets `appModel.selectedPane = pane`; either way the palette closes.
- a dimmed background; clicking outside closes.

**`ContentView` changes:** replace the local `@State showingSpawn` with
`appModel.showingSpawn` (so Cmd-N and the toolbar `+` share one source of truth); present
`CommandPaletteView` as an `.overlay`/`ZStack` gated on `appModel.showingPalette`.

## Data flow

```
Cmd-K (menu) ─► appModel.run(.openPalette) ─► showingPalette=true ─► CommandPaletteView overlay
   type query ─► paletteResults(query, CommandRegistry.all, store.orderedAgents) ─► ranked list
   ↑/↓ move selectedIndex; Enter:
       .command(id) ─► appModel.run(id)         (spawn sheet / next-attention / …)
       .agent(pane) ─► appModel.selectedPane=pane
   Esc / click-out ─► showingPalette=false

Cmd-N  ─► run(.spawnAgent)    ─► showingSpawn=true ─► existing SpawnSheet
Cmd-1..9 ─► run(.switchToAgent(i)) ─► selectAgent(atIndex:i) ─► selectedPane
Cmd-Shift-A ─► run(.nextAttention) ─► selectNextAttention() ─► selectedPane
```

## Testing

Automated (`swift test`, no libghostty):
- **Keymap:** `Keymap.defaults` has the expected bindings; `binding(for:)` returns an
  override when present, else the default; `CommandRegistry.all` yields the palette rows
  with their titles + shortcuts.
- **`paletteResults`:** empty query → all commands then all agents; a command-title query
  (`"spa"` → Spawn Agent) ranks the command; an agent query (project/task substring)
  surfaces that agent as an `.agent` item; ranking prefers prefix over mid-string; the
  command section precedes the agent section.
- **Navigation:** `orderedAgents` equals the `byProject` flatten; `selectAgent(atIndex:)`
  selects the right pane and no-ops out of range; `selectNextAttention()` cycles through
  only `.needsInput` agents, starts at the first when the current selection isn't needy,
  and no-ops when none need input. `run(_:)` sets the right published state
  (`.openPalette` toggles `showingPalette`, `.spawnAgent` sets `showingSpawn`, etc.).

Manual (**user runs it**; UI layer): Cmd-K opens the palette and focuses the field; typing
filters commands + agents; ↑/↓ + Enter runs/jumps; Esc closes. Cmd-N opens the spawn sheet
**while a terminal is focused**; Cmd-1…9 switch agents; Cmd-Shift-A jumps to the next
needs-input agent. Toolbar `+` still spawns.

## Risks

1. **Shortcuts vs. the focused terminal.** Cmd-modified shortcuts declared as menu
   `Commands` fire via the menu before the `SurfaceView` sees them — this is the whole
   reason for the Cmd-only rule. Verify Cmd-N/1-9 work while typing in an agent. If a raw
   Cmd combo ever leaks to libghostty, the menu declaration is the fix.
2. **Palette keyboard handling on the min OS.** `.onKeyPress` is macOS 14+, hence the
   deployment bump; the field must be `@FocusState`-focused on appear so keys route to it.
3. **Shared UI state.** Moving `showingSpawn` onto `AppModel` and adding `showingPalette`
   means the menu and `ContentView` observe one instance — the scene must inject the same
   `appModel` the menu reads.

## Verification gate

`swift test` green (existing 35 + the new keymap/search/navigation tests); `swift build`
clean on the bumped target; and the user confirms the manual pass — Cmd-K palette filters
commands+agents and runs/jumps, and Cmd-N / Cmd-1-9 / Cmd-Shift-A work while a terminal is
focused.
