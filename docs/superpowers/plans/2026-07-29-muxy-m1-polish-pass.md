# muxy M1 — Polish Pass Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clear five small, non-blocking carry-forwards parked by earlier M1 final reviews — daemon split-tree hardening + test, a lazy status-bar menu, and three one-line cleanups.

**Architecture:** Item 5 is Rust (`muxy-daemon`, `cargo test`). Items 1–4 are `MuxyApp` (Swift, `swift build`). No new features; each item closes a parked carry-forward.

**Tech Stack:** Rust (tokio) + Swift 6 (macOS 14, SwiftUI/AppKit).

## Global Constraints

- Behavior-preserving for current usage (items 2–4 change nothing observable today; item 1 changes *when* the menu is built, not its contents; item 5 adds debug-only asserts + a test).
- `debug_assert!` (not `assert!`) so release builds are unaffected.
- Commit after each task; conventional messages + standard trailers.

**Test commands:** Rust: `cargo test -p muxy-daemon` (+ `cargo test`). Swift: `cd macos && swift build` / `swift test`.

---

## Task 1: Daemon split-tree hardening + multi-companion teardown test (muxy-daemon)

**Files:**
- Modify: `crates/muxy-daemon/src/server.rs` (debug_assert on the two ignored bools + a new test)

**Interfaces:**
- Consumes: `split_tree::{split_leaf, remove_leaf, leaves}`, `Daemon::{split_pane, teardown_agent, split_tree_of, get}`.

- [ ] **Step 1: Write the failing test** — add to `server.rs`'s test module, mirroring the setup of the existing `split_close_and_teardown_manage_the_tree` test (temp git repo + `Arc<Daemon>` via `Daemon::new_with(GitWorktreeDriver, FakeNotifier, …)` + a `SyntheticAdapter` running `/bin/sh -c "sleep 30"`):

```rust
    #[tokio::test]
    async fn teardown_kills_multiple_live_companions() {
        // Same daemon + temp-repo + SyntheticAdapter(sleep 30) setup as
        // split_close_and_teardown_manage_the_tree.
        let agent = daemon.spawn_agent(repo.path(), &crate::agent::SyntheticAdapter {
            command: PaneCommand { program: "/bin/sh".into(), args: vec!["-c".into(), "sleep 30".into()], cwd: None, env: vec![] },
        }, "task").unwrap();

        // Two companions, BOTH live at teardown (neither closed first).
        let c1 = daemon.split_pane(agent, SplitDirection::Right).unwrap();
        let c2 = daemon.split_pane(agent, SplitDirection::Down).unwrap();
        assert!(daemon.get(c1).is_some());
        assert!(daemon.get(c2).is_some());
        assert_eq!(split_tree::leaves(&daemon.split_tree_of(agent).unwrap()).len(), 3);

        daemon.teardown_agent(agent).unwrap();

        assert!(daemon.get(c1).is_none(), "companion 1 must be killed on teardown");
        assert!(daemon.get(c2).is_none(), "companion 2 must be killed on teardown");
        assert!(daemon.split_tree_of(agent).is_none());
    }
```

> Reuse the exact repo/daemon setup lines from `split_close_and_teardown_manage_the_tree` in the same file — do not invent a new helper. `split_tree::leaves` is `pub(crate)` and already used by that test's assertions.

- [ ] **Step 2: Run to verify it passes now** (the cascade already works — this test documents/guards it)

Run: `cargo test -p muxy-daemon teardown_kills_multiple_live_companions -- --nocapture`
Expected: PASS (the teardown cascade already kills all companions). If it FAILS, that's a real leak — stop and report. (This is a guard test; it should be green immediately.)

- [ ] **Step 3: Add the `debug_assert!`s.** In `split_pane` (the `trees` block), capture and assert the `split_leaf` bool:

```rust
            let ok = crate::split_tree::split_leaf(tree, target, companion, direction, sid);
            debug_assert!(ok, "split_leaf: {target:?} is not a leaf in the tree for {agent:?}");
```
In `close_pane` (the companion path), capture and assert the `remove_leaf` bool:

```rust
        if let Some(tree) = self.trees.lock().unwrap().get_mut(&agent) {
            let removed = crate::split_tree::remove_leaf(tree, pane);
            debug_assert!(removed, "remove_leaf: {pane:?} is not in the tree for {agent:?}");
        }
```

- [ ] **Step 4: Run the full daemon suite**

Run: `cargo test -p muxy-daemon` then `cargo test`
Expected: PASS — every existing test (the `debug_assert!`s hold under the invariant) + the new one.

- [ ] **Step 5: Commit**

```bash
git add crates/muxy-daemon/src/server.rs
git commit -m "test(daemon): guard multi-companion teardown no-leak; debug_assert tree mutations"
```

---

## Task 2: Lazy status-bar menu (MuxyApp)

Build the status menu on open (`NSMenuDelegate`) so a state change while it's open can't disrupt it; `refresh()` updates only the button. Gate: `swift build`.

**Files:**
- Modify: `macos/Sources/MuxyApp/StatusBarController.swift`

**Interfaces:**
- Consumes: `AppModel.store.attentionCount`/`agentsNeedingAttention`.

- [ ] **Step 1: Set the menu once (with a delegate) in `init`.** Replace the `refresh()` call region so the menu is created once:

```swift
    init(appModel: AppModel, showWindow: @escaping () -> Void) {
        self.appModel = appModel
        self.showWindow = showWindow
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        super.init()
        let menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu
        // objectWillChange fires before the @Published update, so refresh on the next tick.
        cancellable = appModel.objectWillChange.sink { [weak self] _ in
            DispatchQueue.main.async { self?.refresh() }
        }
        refresh()
    }
```

- [ ] **Step 2: Shrink `refresh()` to the button only** (drop all menu-building from it):

```swift
    /// Updates only the status button (count/icon); the menu is built lazily on open.
    private func refresh() {
        let n = appModel.store.attentionCount
        if let button = statusItem.button {
            if n > 0 {
                button.image = NSImage(systemSymbolName: "bell.badge.fill", accessibilityDescription: "agents need attention")
                button.imagePosition = .imageLeading
                button.title = " \(n)"
            } else {
                button.image = NSImage(systemSymbolName: "bell", accessibilityDescription: "muxy")
                button.imagePosition = .imageOnly
                button.title = ""
            }
        }
    }
```

- [ ] **Step 3: Move the menu-building into an `NSMenuDelegate` extension** (`menuNeedsUpdate`, fired when the menu opens). Keep the `addItem`/`selectAgent`/`showWindowAction`/`quitAction` helpers as they are:

```swift
extension StatusBarController: NSMenuDelegate {
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let needy = appModel.store.agentsNeedingAttention
        if needy.isEmpty {
            let item = NSMenuItem(title: "No agents need attention", action: nil, keyEquivalent: "")
            item.isEnabled = false
            menu.addItem(item)
        } else {
            for agent in needy {
                let proj = (agent.project as NSString).lastPathComponent
                let name = proj.isEmpty ? agent.project : proj
                let marker = agent.state == .needsInput ? "🔴" : "🔵"   // NeedsInput vs Completed
                let item = NSMenuItem(title: "\(marker) \(name) — \(agent.task)",
                                      action: #selector(selectAgent(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = agent.pane
                menu.addItem(item)
            }
        }
        menu.addItem(.separator())
        addItem(to: menu, "Show muxy Window", #selector(showWindowAction))
        let quit = addItem(to: menu, "Quit muxy", #selector(quitAction))
        quit.keyEquivalent = "q"
    }
}
```

> Net effect: `refresh()` no longer touches `statusItem.menu`; the menu's contents are rebuilt each time it opens. The `addItem` helper and `@objc` action methods are unchanged.

- [ ] **Step 4: Build + test**

Run: `cd macos && swift build` then `cd macos && swift test`
Expected: builds clean; MuxyCore suite still green.

- [ ] **Step 5: Manual smoke (recorded).** With agents flipping states: the menu-bar number still tracks NeedsInput+Completed live; opening the item shows the current needy agents; a state change **while the menu is open** no longer flickers/dismisses it; clicking an agent still selects it + shows the window; Show Window / Quit still work.

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/MuxyApp/StatusBarController.swift
git commit -m "perf(app): build the status-bar menu lazily (no rebuild while open)"
```

---

## Task 3: Three one-line cleanups (MuxyApp)

Gate: `swift build`.

**Files:**
- Modify: `macos/Sources/MuxyApp/App.swift` (weak capture + shortcut assert)
- Modify: `macos/Sources/MuxyApp/SplitContainer.swift` (`.id(id)`)

- [ ] **Step 1: Weak-capture the delegate in the `WindowAccessor` closure.** In `App.swift`, change the background modifier:

```swift
                .background(WindowAccessor { [weak d = delegate] window in d?.adoptWindow(window) })
```

- [ ] **Step 2: Assert on a missing shortcut binding.** In `App.swift`'s `shortcut(_:)`:

```swift
    private func shortcut(_ id: CommandID) -> KeyboardShortcut {
        guard let b = keymap.binding(for: id) else {
            assertionFailure("no key binding for \(id)")   // loud in debug; release falls through
            return KeyboardShortcut("?", modifiers: [])
        }
        return KeyboardShortcut(KeyEquivalent(b.key), modifiers: eventModifiers(b.modifiers))
    }
```

- [ ] **Step 3: Key the split node by its id.** In `SplitContainer.swift`'s `.split` case:

```swift
        case let .split(id, axis, ratio, first, second):
            SplitNode(id: id, axis: axis, ratio: ratio, first: first, second: second,
                      surfaceHost: surfaceHost, focusedPane: $focusedPane)
                .id(id)
```

- [ ] **Step 4: Build + test**

Run: `cd macos && swift build` then `cd macos && swift test`
Expected: builds clean; MuxyCore suite still green.

- [ ] **Step 5: Manual smoke (recorded; light).** The app still launches; menu shortcuts (⌘K/⌘N/⌘D/…) still fire; splitting/closing still works and dividers still drag. (No behavior change is expected from these three — the build is the real gate.)

- [ ] **Step 6: Commit**

```bash
git add macos/Sources/MuxyApp/App.swift macos/Sources/MuxyApp/SplitContainer.swift
git commit -m "chore(app): weak delegate capture, shortcut assert, key split node by id"
```

---

## Final verification

- `cargo test` → whole workspace green, including the new `teardown_kills_multiple_live_companions`.
- `cd macos && swift test` green + `cd macos && swift build` clean on macOS 14.
- Manual (user): the status menu no longer flickers on a state change while open and the count still tracks live; the app otherwise behaves exactly as before. Five parked carry-forwards closed.
