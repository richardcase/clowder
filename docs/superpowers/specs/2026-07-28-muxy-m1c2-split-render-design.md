# muxy M1c-2 — Split-Pane Client Render

## Context

M1c (companion split panes) is a nested, **daemon-owned** split tree: each agent owns a
binary split tree; leaves are the agent pane + companion shells rooted in the same worktree.
**M1c-1 (merged, PR #17)** built the whole server side — the daemon maintains the tree,
`SplitPane`/`ClosePane`/`SetSplitRatio`/`GetSplitTree` requests, and a `SplitTreeChanged`
event. **This slice, M1c-2, is the macOS client:** render the selected agent's split tree,
attach a terminal per leaf, focus a pane, and drive split/close/focus from the keyboard +
palette. **M1c-3** is then purely draggable dividers (resize → `SetSplitRatio`).

> **Scope adjustment from the original decomposition:** the split/close/focus **commands**
> move into M1c-2 (not M1c-3). Rendering alone can't be manually verified — there'd be no way
> to *create* a split — and the commands need the focus model this slice builds anyway. M1c-3
> is left as just drag-to-resize.

### M1c-1 carry-forwards (must honor)

1. **`SplitTreeChanged` arrives twice** per mutation on the initiating connection (direct
   reply + broadcast) → the client must apply tree snapshots **idempotently** (replace the
   stored tree; never append/merge).
2. **`PaneTree` needs a manual Swift `Codable`** — a recursive internally-tagged enum
   (`indirect enum`, switch on `"kind"`); no free synthesis.
3. **No per-companion `AgentRemoved`** — the client learns a companion vanished only from a
   `SplitTreeChanged` whose tree no longer contains that leaf; on the **agent's**
   `AgentRemoved`, drop the whole tree.

### What exists (ground truth)

`MuxyCore`: `AgentStore` (`agents`, `byProject`, `orderedAgents`, `lastError`, `apply`),
`AppModel` (`@MainActor`: `store`, `selectedPane`, `connectionState`, `showingPalette/Spawn`,
`selectAgent`/`selectNextAttention`/`run(_:)`, `spawn`, republishes the store), `ControlSession`
(decodes lines → `store.apply`), `ControlRequest` (custom `Encodable`: `listAgents`,
`spawnAgent`), `ControlEvent` (custom `Decodable` on `"type"`), `Keymap`/`CommandID`/
`CommandRegistry`/`paletteResults` (M1a). `SurfaceHost.view(for: UInt64) -> SurfaceView`
returns one retained `SurfaceView` per pane running `muxy attach <pane>` — **works for any
pane, including companions, unchanged**. `ContentView` detail currently renders
`TerminalContainer(pane: selectedPane).id(pane)` (or an exited placeholder). Wire JSON
contract: `PaneId`/`SplitId` bare numbers, `Axis`/`SplitDirection` lowercase, `PaneTree`
tagged on `"kind"` (`"leaf"`/`"split"`), requests/events tagged on `"type"` camelCase.

## Goals / Non-goals

**Goals:** select an agent and see its split tree rendered in the detail area (agent pane +
companions, each a live terminal); **⌘D** / **⌘⇧D** split the focused pane right/down (a
companion shell spawns in the worktree and appears); **⌘⇧W** closes the focused companion
(the layout collapses); click or **⌘]** moves focus; the split/close/focus commands are also
in the Cmd-K palette. Splits/closes survive detach/reattach (the daemon owns the tree; the
client re-fetches it). All state changes apply idempotently.

**Non-goals (later):** **draggable dividers / resize** (M1c-3 — dividers render at the tree's
ratio, fixed, not draggable here); companion-exit auto-removal (a companion whose shell exits
on its own stays in the tree showing libghostty's "process exited" screen until closed with
⌘⇧W — auto-close needs a daemon-side companion watcher, deferred); per-pane titles/labels.

## Component design

### MuxyCore (pure / model — unit-tested)

**`PaneTree` + `Axis` + `SplitDirection`** (new `PaneTree.swift`):

```swift
public enum Axis: String, Decodable, Equatable, Sendable { case horizontal, vertical }
public enum SplitDirection: String, Encodable, Equatable, Sendable { case right, down }

public indirect enum PaneTree: Decodable, Equatable, Sendable {
    case leaf(pane: UInt64)
    case split(id: UInt64, axis: Axis, ratio: Double, first: PaneTree, second: PaneTree)

    private enum CodingKeys: String, CodingKey { case kind, pane, id, axis, ratio, first, second }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "leaf":  self = .leaf(pane: try c.decode(UInt64.self, forKey: .pane))
        case "split": self = .split(
            id: try c.decode(UInt64.self, forKey: .id),
            axis: try c.decode(Axis.self, forKey: .axis),
            ratio: try c.decode(Double.self, forKey: .ratio),
            first: try c.decode(PaneTree.self, forKey: .first),
            second: try c.decode(PaneTree.self, forKey: .second))
        default: throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "unknown kind")
        }
    }
    public var leaves: [UInt64] {   // in render order
        switch self {
        case let .leaf(pane): return [pane]
        case let .split(_, _, _, first, second): return first.leaves + second.leaves
        }
    }
}
```

**`ControlRequest`** gains the split cases (custom `Encodable`, mirroring the daemon):
`splitPane(pane: UInt64, direction: SplitDirection)`, `closePane(pane: UInt64)`,
`setSplitRatio(split: UInt64, ratio: Double)`, `getSplitTree(agent: UInt64)` — encoded
`{"type":"splitPane","pane":N,"direction":"right"}` etc. (M1c-2 uses splitPane/closePane/
getSplitTree; setSplitRatio is added for the mirror, exercised in M1c-3.)

**`ControlEvent`** gains `splitTreeChanged(agent: UInt64, tree: PaneTree)` (decode when
`type=="splitTreeChanged"`, reading `agent` + `tree`).

**`AgentStore`** gains `@Published public private(set) var trees: [UInt64: PaneTree]` and an
`apply` case: `.splitTreeChanged(agent, tree)` → `trees[agent] = tree` (idempotent replace,
carry-forward #1); `.agentRemoved(pane)` also clears `trees[pane]` (carry-forward #3).

**`AppModel`:**
- `@Published var focusedPane: UInt64?` — the focused leaf within the current agent's tree.
- `var currentTree: PaneTree?` — `selectedPane.flatMap { store.trees[$0] }`; the detail falls
  back to a lone `.leaf(pane: selectedPane)` when no tree is stored yet.
- On selecting an agent, request its tree: `selectAgent`/sidebar selection triggers
  `try? session.send(.getSplitTree(agent:))` (and reset `focusedPane` to the agent pane).
  (An `onSelect(agent:)` hook or a `didSet` on `selectedPane`.)
- Actions (send `ControlRequest`s; the resulting `SplitTreeChanged` updates the store):
  `splitFocused(_ direction: SplitDirection)` → `splitPane(pane: focusedPane ?? selectedPane,
  direction)`; `closeFocused()` → `closePane(pane: focusedPane)` (only when the focused pane
  is a companion, i.e. not the agent pane — closing the agent is teardown, out of scope here);
  `focusNextPane()` → set `focusedPane` to the next leaf in `currentTree.leaves` (cycle).
- `run(_:)` handles the new `CommandID`s (below).
- When a `SplitTreeChanged`/`AgentRemoved` makes `focusedPane` no longer a leaf of
  `currentTree`, reset it to the agent pane.

**`CommandID` / `Keymap` / `CommandRegistry`** gain: `.splitRight` (⌘D), `.splitDown` (⌘⇧D),
`.closePane` (⌘⇧W), `.focusNextPane` (⌘]). Added to `CommandRegistry.all` (palette rows) and
`Keymap.defaults`. `run(_:)` maps them to the `AppModel` actions.

### MuxyApp (UI)

**`SplitContainer`** (new, recursive) — renders a `PaneTree`:
- `.leaf(pane)` → `TerminalContainer(pane:surfaceHost:)` (reusing `SurfaceHost`), wrapped so
  the focused leaf shows a subtle focus ring; the view reports focus back (below).
- `.split(_, axis, ratio, first, second)` → a `GeometryReader` splitting into two recursive
  `SplitContainer`s along `axis` (horizontal = side-by-side, vertical = stacked) at `ratio`
  with a thin **non-draggable** divider (drag comes in M1c-3). Keyed by node identity so
  SwiftUI diffs the tree cleanly.

**Focus** via first responder (native): `SurfaceView` gains an `onFocus: (() -> Void)?` that
fires from `becomeFirstResponder()` (AppKit already makes a clicked terminal the first
responder — so clicking a pane focuses it with no gesture overlay to steal terminal clicks).
`TerminalContainer(pane:)` sets `view.onFocus = { model.focusedPane = pane }`. Keystrokes go
to whichever leaf is first responder — native, no routing needed; `focusedPane` only tracks
the split/close target.

**`ContentView` detail:** replace `TerminalContainer(pane:).id(pane)` with
`SplitContainer(tree: model.currentTree ?? .leaf(pane: selectedPane), …)` for a live
(non-exited) selected agent; keep the exited placeholder for an exited agent.

**Commands wiring:** the new menu items + palette entries + keymap shortcuts (⌘D/⌘⇧D/⌘⇧W/⌘])
call `appModel.run(.splitRight/.splitDown/.closePane/.focusNextPane)`, mirroring how M1a wired
Spawn/Next-Attention. These fire over the focused terminal (Cmd-modified, menu-intercepted).

## Data flow

```
select agent ─► send GetSplitTree{agent} ─► daemon replies SplitTreeChanged{agent,tree}
   ─► store.trees[agent]=tree ─► ContentView detail renders SplitContainer(tree)
   each leaf ─► SurfaceHost.view(for: leaf) ─► `muxy attach <leaf>` in a SurfaceView

⌘D (focused pane P) ─► run(.splitRight) ─► send SplitPane{pane:P, direction:right}
   ─► daemon spawns companion shell in worktree, updates tree ─► SplitTreeChanged (x2, idempotent)
   ─► store.trees replace ─► a new leaf renders ─► its SurfaceView attaches the companion
⌘⇧W (focused companion) ─► send ClosePane{pane} ─► collapse ─► SplitTreeChanged ─► leaf removed
click a pane ─► becomeFirstResponder ─► focusedPane = that pane   (keys already route natively)
agent AgentRemoved ─► store.trees[agent]=nil ─► detail returns to placeholder/sidebar
```

## Testing

Automated (`swift test`, no libghostty — MuxyCore only):
- **`PaneTree` decode:** a leaf, a nested split (JSON from M1c-1's shape) round-trips into the
  right `indirect enum`; `leaves` returns render order; unknown `kind` throws.
- **`ControlEvent.splitTreeChanged`** decodes `agent` + a nested `tree`.
- **`ControlRequest`** encodes `splitPane`/`closePane`/`getSplitTree` to the right JSON
  (`type`, bare `pane`, `direction`).
- **`AgentStore.apply`:** `.splitTreeChanged` stores/replaces the tree idempotently (applying
  the same event twice yields one tree — carry-forward #1); `.agentRemoved` clears it.
- **`AppModel`:** `splitFocused`/`closeFocused`/`focusNextPane` send the right requests
  (via the fake transport) and `focusNextPane` cycles `currentTree.leaves`; selecting an
  agent sends `getSplitTree`; a tree that no longer contains `focusedPane` resets focus.

Manual (**user runs it**; UI): select an agent → its terminal renders; **⌘D** splits right —
a shell appears beside it in the worktree (run `pwd`); **⌘⇧D** splits down; click between
panes and type — input goes to the clicked pane; **⌘⇧W** closes a companion and the layout
collapses; the palette lists Split/Close/Focus; close+reopen the window → the split layout is
restored (daemon-owned); the commands are no-ops with no agent selected.

## Risks

1. **Focus vs. first responder with multiple surfaces.** Several `SurfaceView`s in one window;
   clicking one must focus it (native FR) and update `focusedPane`. Mitigated by the
   `becomeFirstResponder → onFocus` hook (no gesture overlay competing for terminal clicks).
   Verify keys reach the clicked pane and split/close target the right one.
2. **Idempotent double `SplitTreeChanged`** (carry-forward #1). Mitigated by replace-not-merge
   + a test applying the same event twice.
3. **Manual `PaneTree` Codable** (carry-forward #2) — recursion via `indirect enum`; unit-test
   the nested decode against M1c-1's exact JSON.
4. **SurfaceHost lifetime.** Companion `SurfaceView`s are retained per pane like agents; a
   closed/removed leaf's view simply leaves the hierarchy. Eviction of dead-pane views stays
   the existing deferred `SurfaceHost`-eviction item.

## Verification gate

`swift test` green (existing 47 + the new PaneTree/event/request/store/AppModel tests);
`swift build` clean; and the user confirms the manual pass — split right/down spawns companion
shells in the worktree, focus + close work, the palette lists the commands, and the layout
survives window close/reopen.
