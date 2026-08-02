# clowder M7c2 — live backend swap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans. Steps use checkbox syntax.

**Goal:** A live in-app "Use local" / "Connect to remote" swap — switch the app between a local daemon and the remote forwarder **without a restart**, from the menu bar.

**Architecture:** SwiftUI captures the `AppModel`/`SurfaceHost` **instances** in the window body, so the swap **reconfigures them in place** (keeps the same instances): `AppModel.reconnect(to:)` (tear down → clear store → connect to the new control socket) and `SurfaceHost.retarget(socketPath:)` (drop every pane's `clowder attach` surface → re-point at the new render socket). `AppDelegate.switchBackend(to:)` stops the current backend supervisor, starts the other via `makeBackendSupervisor` (M7c1), and reconfigures both. A `StatusBarController` menu item triggers it.

**Tech Stack:** Swift — `ClowderCore` (AppModel/AgentStore, unit-testable) + `ClowderApp` (SurfaceHost/AppDelegate/StatusBarController, `swift build` + manual).

## Global Constraints
- Swift builds in `macos/`; `swift build` (ClowderApp) needs libghostty; `swift test` (ClowderCore) does not.
- Reuse M7c1: `makeBackendSupervisor(remoteHost:) -> (supervisor, control, render)`; `ClowderCore.forwarderSocketDir`; `currentRemoteHost` (mutable, read via the tray's live closure). The swap keeps the **same** `AppModel`/`SurfaceHost` instances.
- Freeing a `SurfaceView`'s `ghostty_surface_t` (`SurfaceView.deinit`) terminates its `clowder attach` child, so clearing `SurfaceHost.views` tears down the old backend's panes.

## Task 1: AppModel.reconnect + AgentStore.reset (ClowderCore, testable)
**Files:** `macos/Sources/ClowderCore/AgentStore.swift`, `AppModel.swift`; test in `macos/Tests/ClowderCoreTests/AppModelTests.swift`.
- [ ] `AgentStore.reset()` — `agents = [:]; trees = [:]; lastError = nil` (adapters re-hydrate on the new connection).
- [ ] `AppModel`: change `makeTransport` from `let` to `var`; add `public func reconnect(makeTransport:)` = `shutdown()` → `store.reset()` → `selectedPane = nil` → `self.makeTransport = new` → `connect()`.
- [ ] Test `testReconnectSwapsTransportClearsStoreAndReconnects`: connect fake A, deliver an agentList, assert non-empty; `reconnect(makeTransport: { B })`; assert `A.disconnected`, `store.agents.isEmpty`, `.live`, and B got a `listAgents`. `swift test` green.
- [ ] Commit.

## Task 2: SurfaceHost.retarget (ClowderApp)
**Files:** `macos/Sources/ClowderApp/SurfaceHost.swift`.
- [ ] `socketPath` `let` → `private(set) var`; add `func retarget(socketPath: String)` = `views.removeAll()` (drops surfaces → kills old `clowder attach`) + `self.socketPath = socketPath`. New panes' `view(for:)` build `SurfaceView` with the new path.
- [ ] `swift build`. Commit.

## Task 3: AppDelegate switchBackend + menu item (ClowderApp)
**Files:** `macos/Sources/ClowderApp/App.swift`, `StatusBarController.swift`.
- [ ] Store `configuredRemoteHost` (from `resolveRemoteHost` at bootstrap) so a local session still knows the remote target. Add `switchBackend(to remoteHost: String?)`: `daemonSupervisor?.stop()`; `guard let backend = makeBackendSupervisor(remoteHost:)`; set `daemonSupervisor`, `currentRemoteHost`; `backend.supervisor.start()`; `appModel?.reconnect(makeTransport: { try UnixSocketConnection(path: backend.control) })`; `surfaceHost?.retarget(socketPath: backend.render)`.
- [ ] `StatusBarController`: a `swapAction` closure + an item — when remote: "Use local" → `switchBackend(to: nil)`; when local and a remote host is configured: "Connect to <host>" → `switchBackend(to: host)`; when local + none configured: omit. Wire the configured host + swap callback from `App.swift` (like `showWindow`).
- [ ] `swift build`. **Manual:** from remote, "Use local" drops remote agents + starts the local daemon live; switching back re-runs the forwarder; quit stops the active backend (no orphan). Commit.

## Verification
- Agent: `swift test` (AppModel.reconnect test + existing 91 green); `swift build` (ClowderApp); `cargo test --workspace --locked` unaffected (no Rust change).
- Maintainer (GUI): the live swap round-trip above.
