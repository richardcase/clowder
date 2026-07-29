# muxy M1 — Polish Pass

## Context

M1 (multi-agent polish) is functionally complete: M1a (command palette + keymap), M1c-1/2/3
(companion split panes), M1d (menu-bar attention count), and snapshot-on-attach (pre-existing).
Each slice's final whole-branch review parked a few small, non-blocking carry-forwards. This
is a **polish pass** that clears the five safest, highest-value ones. It's a curated grab-bag
of independent fixes, not a new feature — no new surface area.

Deliberately **out of scope** (features, not polish, tracked for later): companion-exit
auto-removal (needs a daemon companion watcher) and `SurfaceHost` dead-pane eviction (must
avoid evicting a still-displayed surface).

## The five items

### 1. Lazy status-bar menu (M1d)

**Problem:** `StatusBarController.refresh()` reassigns `statusItem.menu = menu` on *every*
`AppModel` change (`StatusBarController.swift:63`). If a state flips while the user has the
status menu open, AppKit gets a fresh menu under the open one — flicker/dismiss.

**Fix:** conform `StatusBarController` to `NSMenuDelegate`; set the `NSMenu` (with
`delegate = self`) on the status item **once**, and build its items in
`menuNeedsUpdate(_:)` (fired on open) instead of in `refresh()`. `refresh()` then updates
**only the button** (count/icon), which still tracks live. Verify: the count keeps updating;
opening the menu shows the current agents; a state change while the menu is open doesn't
disrupt it.

### 2. `[weak delegate]` in the `WindowAccessor` capture (M1d)

**Problem:** `.background(WindowAccessor { window in delegate.adoptWindow(window) })`
(`App.swift:129`) captures the `AppDelegate` strongly, forming a benign
AppDelegate↔window cycle (both app-lifetime singletons — no leak, but untidy).

**Fix:** capture `[weak delegate]` and guard: `{ [weak delegate] window in
delegate?.adoptWindow(window) }`. Build-verified; no behavior change.

### 3. `shortcut()` dead fallback → assertion (M1a)

**Problem:** `shortcut(_:)` returns `KeyboardShortcut("?", modifiers: [])` when
`keymap.binding(for: id)` is nil (`App.swift:161`). Unreachable today (every menu command has
a default binding), but a future binding-less command would silently bind a bogus `?`.

**Fix:** `assertionFailure("no key binding for \(id)")` before the release-only fallback
return, so the invariant fails loudly in debug. Build-verified; no behavior change for current
commands.

### 4. `.id(id)` on `SplitNode` (M1c-3)

**Problem:** the recursive split view keys leaves with `.id(pane)` but the `.split` case's
`SplitNode(...)` (`SplitContainer.swift:25`) has no explicit identity. If a split slot is
positionally reused for a *different* split, `@State` (`localRatio`) could momentarily show a
stale ratio (it self-heals via `.onChange(of: ratio)`, but a coincidental-equal ratio wouldn't
trigger it).

**Fix:** add `.id(id)` to the `SplitNode(...)` call so SwiftUI keys it by split id — symmetry
with the leaf treatment, defense-in-depth. Build-verified.

### 5. Daemon split-tree hardening (M1c-1)

**Problem A:** `split_pane`/`close_pane` ignore the `bool` returned by
`split_tree::split_leaf`/`remove_leaf` (`server.rs:260,286`). Under the current invariant they
always succeed, but a silent `false` (structural drift) would go unnoticed.

**Fix A:** `debug_assert!(ok, …)` on both returns — loud in debug, no-op in release.

**Problem B:** the teardown-cascade no-leak test only ever has ONE live companion at teardown
(the M1c-1 review flagged the multi-companion loop as untested).

**Fix B:** add a test that splits an agent **twice** (two live companions), tears down the
agent, and asserts BOTH companions are gone from `panes` and the tree is dropped.

## Testing

- **Item 5** is `cargo test`-verified: the `debug_assert!`s compile and don't trip existing
  tests; the new `teardown_kills_multiple_live_companions` test passes (and would fail if the
  cascade missed a companion).
- **Items 1–4** are `swift build`-gated (MuxyCore/app tests stay green). Items 2–4 are
  behavior-preserving for current usage (build is the real gate). Item 1 has a manual check
  (menu doesn't flicker on a state change while open; count still updates live).

## Verification gate

`cargo test` green (existing + the new multi-companion teardown test); `cd macos && swift
test` green + `swift build` clean; and the user confirms the status menu no longer flickers
while open and the count still tracks live. No new features; each item closes a parked
carry-forward.
