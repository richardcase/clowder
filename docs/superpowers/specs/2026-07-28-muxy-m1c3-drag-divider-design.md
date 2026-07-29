# muxy M1c-3 — Draggable Dividers + Focus Polish

## Context

M1c (companion split panes) is a nested, daemon-owned split tree. **M1c-1** built the daemon
core (incl. `SetSplitRatio`), **M1c-2** built the client render (recursive `SplitContainer`,
focus, split/close/focus commands) with **fixed** dividers. **This slice, M1c-3, is the last
M1c piece:** make the dividers **draggable** (drag → `setSplitRatio`), and fold in the two
carry-forwards the M1c-2 final review flagged.

### Carry-forwards from M1c-2 (fold in here)

1. **Focus reconciliation:** when a `SplitTreeChanged`/`agentRemoved` leaves `focusedPane` no
   longer a leaf of `currentTree`, reset it to the agent pane (`selectedPane`). Spec'd in
   M1c-2 but unimplemented (unreachable single-client) — implement it now.
2. **Surface focus follows `isFocused`:** `SurfaceView.createSurface` calls
   `ghostty_surface_set_focus(surface, true)` unconditionally, so with multiple panes **every**
   pane renders a live cursor. Tie libghostty surface focus to the pane's `isFocused` so only
   the focused pane shows a live cursor.

### What exists (ground truth)

`MuxyCore`: `AppModel` (`@MainActor`; `selectedPane` didSet fetches tree + focuses agent,
`focusedPane`, `currentTree`, `splitFocused`/`closeFocused`/`focusNextPane`, `run(_:)`,
`storeSubscription` forwarding `store.objectWillChange`), `ControlRequest.setSplitRatio(split:
UInt64, ratio: Double)` (already defined + encoded), `PaneTree` (`.split(id, axis, ratio,
first, second)`, `leaves`). `MuxyApp`: `SplitContainer` (recursive; `.split` lays first at
`total*ratio - 0.5` with a plain `Divider()`), `TerminalContainer(pane:surfaceHost:isFocused:
onFocus:)` (drives first responder from `isFocused`), `SurfaceView` (`onFocus` via
`becomeFirstResponder`; `ghostty_surface_set_focus(surface, true)` at creation, line ~71).
Daemon clamps ratio to `[0.05, 0.95]` (M1c-1).

## Goals / Non-goals

**Goals:** drag a divider between two panes to resize them — the divider moves smoothly under
the cursor (resize cursor on hover), and on release the new ratio is sent to the daemon
(`setSplitRatio`) and persists across detach/reopen; the ratio is clamped to `[0.05, 0.95]`
client-side (matching the daemon). Only the **focused** pane shows a live libghostty cursor.
`focusedPane` never dangles after a tree change.

**Non-goals (later / separate):** `SurfaceHost` dead-pane eviction; companion-exit
auto-removal (needs a daemon companion watcher); the tray attention count / snapshot-on-attach
(other M1 slices); H/V branch de-duplication and other purely-cosmetic refactors.

## Component design

### MuxyCore (testable)

**`AppModel.setDividerRatio(split: UInt64, ratio: Double)`** — clamps to `[0.05, 0.95]` and
sends `ControlRequest.setSplitRatio(split:ratio:)`:

```swift
public func setDividerRatio(split: UInt64, ratio: Double) {
    guard session != nil else { return }
    let r = min(0.95, max(0.05, ratio))
    try? session?.send(.setSplitRatio(split: split, ratio: r))
}
```

**Focus reconciliation** — a `reconcileFocus()` invoked whenever the store changes, from the
existing `storeSubscription` sink:

```swift
func reconcileFocus() {
    guard let leaves = currentTree?.leaves else { return }   // no tree → leave focus as-is
    if let f = focusedPane, !leaves.contains(f) {
        focusedPane = selectedPane
    }
}
```
Wire it in the `storeSubscription` sink (which already forwards `objectWillChange`):
`sink { [weak self] _ in self?.objectWillChange.send(); self?.reconcileFocus() }`.
(This is main-thread — `store` mutates on main.) Testable: deliver a `splitTreeChanged` whose
tree drops the focused leaf → `focusedPane` resets to `selectedPane`.

### MuxyApp (build + manual)

**Draggable divider** — extract the `.split` case into a `SplitNode` view that owns the drag:

- Renders the two child `SplitContainer`s along the axis, with a **draggable divider strip**
  between them (a thin interactive `Rectangle`/overlay ~6pt wide with `.onHover` →
  `NSCursor.resizeLeftRight`/`resizeUpDown`, centered on the 1px visual line).
- Local optimistic ratio: `@State private var localRatio: Double?` — the rendered ratio is
  `localRatio ?? ratio`. A `DragGesture` on the strip updates `localRatio = clamp(base +
  translation.(x|y) / total, 0.05, 0.95)` on `.onChanged`; on `.onEnded` it calls
  `appModel.setDividerRatio(split: id, ratio: localRatio)`. A `.onChange(of: ratio)` syncs
  `localRatio` to the incoming tree ratio (the daemon echo, or an external change) so there is
  **no snap-back** after release and external changes are honored.
- `total` (the split's length along its axis) comes from the `GeometryReader`, as today.

**Surface focus follows `isFocused`** — `SurfaceView` gains `func setFocused(_ focused: Bool)`
that calls `ghostty_surface_set_focus(surface, focused)`; `createSurface` sets the **initial**
focus from a stored `isFocused` flag (default false) instead of unconditional `true`;
`TerminalContainer.updateNSView` calls `nsView.setFocused(isFocused)` alongside the existing
first-responder logic. Result: the focused pane shows a live cursor; others don't.

**Leaf identity** (cheap flicker fix, optional-but-included): give the `.leaf` case
`.id(pane)` so topology changes reuse the right container.

## Data flow

```
drag divider ─► SplitNode.localRatio updates (smooth, clamped) ─► children reflow live
release ─► appModel.setDividerRatio(split:id, ratio) ─► send SetSplitRatio
   ─► daemon set_ratio + broadcast SplitTreeChanged ─► store.trees replace (new ratio)
   ─► SplitNode.onChange(of: ratio) syncs localRatio (no snap) ; persists across reopen

click pane ─► becomeFirstResponder ─► onFocus ─► focusedPane=pane
   ─► TerminalContainer.updateNSView ─► setFocused(true) on it, setFocused(false) elsewhere
store change drops focusedPane's leaf ─► reconcileFocus ─► focusedPane = agent pane
```

## Testing

Automated (`swift test`, MuxyCore):
- **`setDividerRatio`** sends `setSplitRatio` with the clamped ratio (e.g. `2.0 → 0.95`,
  `-1 → 0.05`), via the fake transport.
- **`reconcileFocus`**: with a tree whose leaves are `[1,2,3]` and `focusedPane == 2`,
  delivering a `splitTreeChanged` whose new tree lacks leaf 2 resets `focusedPane` to
  `selectedPane`; a tree that still contains it leaves it unchanged; no tree → unchanged.

Manual (**user runs it**; UI): with an agent split into ≥2 panes, drag a divider — it moves
smoothly under the cursor (resize cursor on hover), the panes reflow, and on release the ratio
holds (no snap-back); close+reopen the window → the dragged ratio is restored. Only the focused
pane shows a blinking cursor; clicking another pane moves the live cursor. Dragging can't
collapse a pane past the 5%/95% clamp.

## Risks

1. **Divider drag vs. terminal mouse.** The drag strip sits between panes; it must capture
   drags without stealing clicks meant for the terminals. Mitigated by a narrow (~6pt) strip
   with its own gesture; the panes' `SurfaceView`s handle their own mouse. Verify terminal
   clicks near a divider still register.
2. **Snap-back after release.** Mitigated by `localRatio` + `.onChange(of: ratio)` sync (never
   clear to nil on end).
3. **Surface focus flag timing.** `setFocused` must run after the surface exists;
   `TerminalContainer.updateNSView` runs post-creation, and `createSurface` seeds the initial
   value — verify a freshly-created focused pane gets a cursor and a blurred one doesn't.

## Verification gate

`swift test` green (existing 61 + `setDividerRatio`/`reconcileFocus` tests); `swift build`
clean; and the user confirms: dividers drag smoothly and the ratio persists across reopen,
only the focused pane shows a live cursor, and `focusedPane` never gets stuck on a gone pane.
This completes M1c (companion split panes).
