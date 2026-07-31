# clowder M7c — macOS app remote mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** The macOS app runs in a config-driven **remote mode** — its backend supervisor runs the M7b forwarder (`clowder connect <host>`) instead of a local `clowder-daemon`, and the app talks to the forwarder's local sockets. A menu-bar status shows the mode, and a **live "Use local" / "Use remote" swap** switches backends without an app restart.

**Architecture:** The app resolves the remote host by shelling out to a new `clowder remote-host` (Rust owns config.toml/env parsing; Swift has no TOML parser). In remote mode the supervisor spawns `clowder connect <host>`; the app points `AppModel`'s control transport and each pane's `clowder attach` (`SurfaceHost`) at the forwarder's sockets under `<control-parent>/remote/`. The `DaemonSupervisor` policy (ClowderCore) is **unchanged** — it supervises whatever the injected `spawn` closure launches.

**Tech Stack:** Rust (a CLI subcommand), Swift (ClowderApp wiring + AppKit `StatusBarController`). ClowderCore stays libghostty-free/unit-testable; ClowderApp needs the vendored libghostty to build (`cd macos && swift build`).

## Global Constraints

- **Prefix cargo with `source "$HOME/.cargo/env" && `.** Swift builds run in `macos/` (`swift build`, `swift test`). `swift build` (ClowderApp) **requires the vendored libghostty** (present here); `swift test` (ClowderCore) does not.
- **Config read = query the binary.** New `clowder remote-host` prints `Config.remote_host` (empty line if unset). The app runs `<bundledBin("clowder")> remote-host`, trims stdout; non-empty ⇒ remote mode.
- **Forwarder sockets** (must match M7b's Rust derivation): `dir = <control_sock parent>/remote`; render `dir/clowder.sock`, control `dir/clowder-control.sock`. Carry-forward from M7b review: **one forwarder per dir** (no flock — the supervisor guarantees a single instance) and **derive the dir, don't scrape the forwarder's stdout**.
- **Supervisor policy unchanged:** M7c only changes the injected `spawn` closure (what/how it launches) and the socket paths the app connects to. `DaemonSupervisorTests` must still pass untouched. The exit-3 "flock loser" yield simply never fires for a forwarder (it holds no flock) — no code change needed.
- Reuse: `ProcessDaemon`/`makeDaemonSupervisor` (`DaemonLaunch.swift`), `AppModel(makeTransport:)` + M5d reconnect (`AppModel.swift`), `SurfaceHost(socketPath:)` (`SurfaceHost.swift`), `StatusBarController` (`StatusBarController.swift`), `bootstrap()` (`App.swift`).

---

## Task 1: `clowder remote-host` subcommand (Rust)

**Files:** Modify `crates/clowder-client/src/main.rs`.

**Interfaces:** Produces the CLI verb `clowder remote-host` → prints the resolved remote host (`Config::load().remote_host`) or an empty line; exit 0.

- [ ] **Step 1: Add the arm.** In `main.rs`, before the fallback:
```rust
        Some("remote-host") => {
            println!("{}", clowder_config::Config::load().remote_host.unwrap_or_default());
            Ok(())
        }
```
Add `remote-host` to the top-level usage string.
- [ ] **Step 2: Build + smoke.** `source "$HOME/.cargo/env" && cargo build -p clowder-client && cargo test --workspace --locked` (green). Smoke: `CLOWDER_REMOTE_HOST=h:1 target/debug/clowder remote-host` prints `h:1`; unset prints an empty line.
- [ ] **Step 3: Commit** `feat(client): clowder remote-host — print the resolved [remote] host`.

## Task 2: Backend supervisor for local OR remote (ClowderApp)

**Files:** Modify `crates/../macos/Sources/ClowderApp/DaemonLaunch.swift`.

**Interfaces:**
- `ProcessDaemon.init(execPath:args:env:)` — add `args: [String] = []` (set `process.arguments = args`).
- `func forwarderSocketDir(controlPath: String) -> String` — `(controlPath as NSString).deletingLastPathComponent + "/remote"`.
- `func makeBackendSupervisor(remoteHost: String?) -> (supervisor: DaemonSupervisor, control: String, render: String)?` — remote: spawn `bundledBin("clowder")` with args `["connect", host]`, control/render = the forwarder dir's sockets; local: today's `clowder-daemon` spawn, control/render = `socketPaths().control/.client`. Returns nil unbundled.

- [ ] **Step 1:** Add `args` to `ProcessDaemon` (default `[]`, `process.arguments = args`); confirm `makeDaemonSupervisor` still compiles (passes no args).
- [ ] **Step 2:** Add `forwarderSocketDir` + `makeBackendSupervisor(remoteHost:)` (remote branch spawns `clowder connect <host>` with the per-user socket env pointed at the forwarder dir + `PATH`; local branch = the existing daemon wiring). Keep `makeDaemonSupervisor()` as the local path or fold it into `makeBackendSupervisor(remoteHost: nil)`.
- [ ] **Step 3: Build.** `cd macos && swift build 2>&1 | tail -5` (compiles); `swift test 2>&1 | tail -5` (DaemonSupervisorTests still green — supervisor unchanged).
- [ ] **Step 4: Commit** `feat(app): makeBackendSupervisor — supervise a local daemon or the remote forwarder`.

## Task 3: bootstrap mode decision + socket wiring (ClowderApp)

**Files:** Modify `macos/Sources/ClowderApp/App.swift` (`bootstrap()`).

- [ ] **Step 1:** At bootstrap, resolve the remote host: run `bundledBin("clowder")` with `["remote-host"]`, capture+trim stdout (empty ⇒ local). A small `resolveRemoteHost() -> String?` helper (`Process` + `Pipe`; nil if the binary is missing / unbundled dev).
- [ ] **Step 2:** Replace the `makeDaemonSupervisor()` call with `makeBackendSupervisor(remoteHost: resolveRemoteHost())`; use its returned `control`/`render` for `AppModel(makeTransport: { try UnixSocketConnection(path: control) })` and `SurfaceHost(..., socketPath: render)`. Store the current mode/host on `AppDelegate` for the tray + swap.
- [ ] **Step 3: Build + manual.** `cd macos && swift build`. **Manual (maintainer):** with a remote daemon (`CLOWDER_LISTEN=…`) reachable, set `[remote] host` (or `CLOWDER_REMOTE_HOST`), launch the app → it supervises `clowder connect`, connects control to the forwarder, spawns/attaches agents on the remote daemon.
- [ ] **Step 4: Commit** `feat(app): bootstrap picks local vs remote backend from clowder remote-host`.

## Task 4: menu-bar status line (ClowderApp)

**Files:** Modify `macos/Sources/ClowderApp/StatusBarController.swift` (+ its construction in `App.swift`).

- [ ] **Step 1:** Thread the mode/host into `StatusBarController` (new init param or a `@Published` field on `AppModel`). In `menuNeedsUpdate`, add a disabled header item: `"Remote: <host>"` in remote mode, `"Local"` otherwise (mirror the existing disabled `"No agents…"` item pattern).
- [ ] **Step 2: Build + manual.** `cd macos && swift build`; **manual:** the menu shows the correct mode/host line.
- [ ] **Step 3: Commit** `feat(app): menu-bar shows the current backend (Remote: <host> / Local)`.

## Task 5: live "Use local" / "Use remote" swap (ClowderApp)

**Files:** Modify `App.swift` (a re-entrant backend start/stop), `StatusBarController.swift` (the menu item).

- [ ] **Step 1:** Extract the Task-3 wiring into `startBackend(remoteHost: String?)` and `stopBackend()` on `AppDelegate` (`stopBackend` = `appModel?.shutdown()` + `daemonSupervisor?.stop()` + clear `SurfaceHost` views/panes so stale agents from the other backend are dropped). `switchBackend(to:)` = `stopBackend()` then `startBackend(...)`, then refresh the tray + window.
- [ ] **Step 2:** In `StatusBarController`, add an actionable item — `"Use local"` when remote, `"Connect to remote"` when local — wired (like the existing `showWindow` closure) to a callback that calls `switchBackend`.
- [ ] **Step 3: Build + manual.** `cd macos && swift build`; **manual:** from remote mode, "Use local" tears down the forwarder and starts the local daemon live (remote agents disappear, local list is empty); switching back re-runs the forwarder. Quitting stops whichever backend is active (no orphan process).
- [ ] **Step 4: Commit** `feat(app): live Use local / remote backend swap`.

---

## Decomposition note
Tasks 1–2 are automated-testable (Rust `cargo test`; Swift `swift test` for the unchanged supervisor). Tasks 3–5 are ClowderApp: they `swift build` (libghostty) but their behavior is **verified by a maintainer running the app** (GUI) — no headless unit test exercises the window/menu/swap. Sequence them last; if Task 5 (live swap) proves too large, split it into its own slice.

## Self-Review
- Spec coverage: remote-mode supervisor (T2) ✓, config-driven decision via query (T1,T3) ✓, forwarder socket wiring (T2,T3) ✓, menu-bar status (T4) ✓, live Use-local swap (T5) ✓. Carry-forward: single-forwarder-per-dir (supervisor is one instance) ✓, derive-dir-not-stdout (T2 `forwarderSocketDir`) ✓.
- Types: `makeBackendSupervisor` (T2) consumed by bootstrap (T3); `startBackend/stopBackend/switchBackend` (T5) built from T3's wiring; `remote-host` verb (T1) called in T3.
