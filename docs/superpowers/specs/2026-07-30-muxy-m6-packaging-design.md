# muxy M6 — Build & Packaging (macOS release)

## Context

muxy works end-to-end on macOS (M0–M5, all merged), but it is **not shippable**: there is no `.app`
bundle, no `Info.plist`/icon/bundle-id, no code-signing/notarization, no CI, no versioning, and no
distribution channel. Running it today means `cargo build`, `cargo run -p muxy-daemon`, then
`MUXY_BIN=…/target/debug/muxy swift run muxy-app` from `macos/` — a developer workflow, not a product.
The app is a bare SwiftPM executable that force-sets its activation policy at runtime because it has no
bundle identity; the daemon must be started by hand; sockets live in `/tmp`; all crates are version
`0.0.0`.

M6 — **Build & Packaging** — turns this into a shippable macOS release: a real double-click-runnable
`.app` that launches its own daemon, a reproducible from-source build (including libghostty), CI,
versioning/releases, and — once an Apple Developer ID is available — a signed, notarized, Homebrew-
installable build.

### What exists (ground truth, 2026-07-30)

- **No bundle:** no `Info.plist`, `.app`, `.entitlements`, `.icns`, or `.xcodeproj`. `macos/Package.swift`
  builds a bare `muxy-app` executable; `App.swift:75` force-calls `NSApp.setActivationPolicy(.regular)`
  + `activate` to become frontmost (a bare-exe workaround).
- **Dev-only runtime resolution:** `App.swift:28-29` resolves the `muxy` CLI (for the libghostty
  surface command `muxy attach <pane>`) via `$MUXY_BIN` else `<cwd>/../target/debug/muxy`;
  `App.swift:31` reads `$MUXY_CONTROL_SOCK`. The daemon resolves `muxy-hook` via `$MUXY_HOOK_BIN` →
  **its own exe-sibling** → bare name (`agent.rs:22`).
- **Sockets:** `muxy-config` (M5a) defaults sockets to `/tmp/muxy*.sock`; `InstanceLock` (M5b) already
  uses `<runtime_dir>/muxy/daemon.pid` where `runtime_dir = $XDG_RUNTIME_DIR › $TMPDIR › /tmp`.
- **libghostty:** the 189 MB `macos/vendor/libghostty/ghostty-internal.a` is **gitignored** and built
  by a **manual** zig recipe (in `docs/superpowers/research/2026-07-26-libghostty-build-spike.md`);
  `ghostty.h` is committed. Ghostty pin `2de5e7d38e1354759211722a8687c0815d2cf02c`, zig 0.16, needs
  **full Xcode** (Metal shader compiler).
- **No CI** (no `.github/`), no `Makefile`/`scripts/`/`README`, all crate versions `0.0.0`.

### User decisions (brainstorm, 2026-07-30)

- **Spec the full M6 pipeline now; build M6a first.**
- M6a's `.app` **launches and supervises its own daemon** (relaunch-on-crash with backoff).
- **No Apple Developer ID yet** → the sign/notarize/DMG and Homebrew slices are **designed but deferred**
  (blocked on obtaining a Developer ID). M6a and CI proceed **unsigned**.
- Placeholder app icon for now. Bundle id `com.github.richardcase.muxy`; per-user data under
  `$XDG_RUNTIME_DIR/muxy` (else `$TMPDIR/muxy`).

## Goals / Non-goals

**Goals:** (1) a real `Muxy.app` bundle assembled by a committed script, containing the SwiftPM app +
the three release Rust binaries, with an `Info.plist`, placeholder icon, and version; (2) a
**self-contained runtime** — the app resolves its binaries bundle-relative and **launches +
supervises its own daemon**, so double-clicking the app just works; (3) **per-user sockets** (no
`/tmp` collisions); (4) a **reproducible libghostty build script**; (5) **CI** (GitHub Actions)
building everything and producing an unsigned `.app` artifact; (6) **versioning + tagged releases**;
(7) — deferred until a Developer ID exists — **codesign → notarize → staple → DMG** and a **Homebrew
cask/tap**.

**Non-goals:** Sparkle auto-update; Linux packaging (that is M8); an App Store build; a universal
binary story beyond what `cargo`/`swift` produce on the CI runner's arch; any change to app features
(M6 is packaging only). Signing/notarization/Homebrew are **in-scope to design** but **out-of-scope to
build** this cycle (no Developer ID).

## Component design

### M6a — Bundle + self-contained runtime (BUILD NOW)

#### 1. Bundle assembly — `scripts/build-app.sh`

A committed shell script (no Xcode project) that produces `Muxy.app`. Steps: build the Rust binaries
`cargo build --release` (→ `target/release/{muxy-daemon,muxy,muxy-hook}`), build the app
`swift build -c release` (→ `.build/release/muxy-app`), generate the placeholder icon, then assemble:

```
Muxy.app/Contents/
  Info.plist
  MacOS/muxy-app                                   # the SwiftPM release executable
  Resources/
    Muxy.icns                                      # generated placeholder
    bin/muxy-daemon
    bin/muxy                                        # the `muxy attach <pane>` CLI (surface command)
    bin/muxy-hook                                   # daemon finds it as an exe-sibling of muxy-daemon
```

Placing all three Rust binaries in `Contents/Resources/bin/` means the daemon's existing exe-sibling
resolution (`agent.rs:22`) finds `muxy-hook` with **no env var needed**. The script is idempotent
(clean-rebuilds `Muxy.app`), takes an optional output dir, and reads the version from a top-level
`VERSION` file. **Rationale:** the repo already builds via SwiftPM + cargo; a script adds no xcodeproj
toolchain and is CI-friendly. (Alternatives: an Xcode project duplicates the build and adds machinery;
a SwiftPM plugin cannot emit a `.app` natively.)

#### 2. Bundle-relative binary resolution (`MuxyApp`)

Resolve the `muxy` CLI (for the surface `muxy attach` command) and the per-user socket paths from the
running bundle instead of dev paths. New resolution order for the `muxy` binary:
`$MUXY_BIN` (dev override) → **`Bundle.main.resourcePath/bin/muxy`** (bundled) → `<cwd>/../target/debug/muxy`
(dev fallback when run unbundled via `swift run`). The daemon's own `muxy-hook` resolution is unchanged
(exe-sibling already works inside the bundle). Retire the unconditional
`NSApp.setActivationPolicy(.regular)` + `activate` at `App.swift:75`: a real bundle with an `Info.plist`
is a proper GUI app and is frontmost on launch. (Keep it **only** behind an "am I unbundled?" check so
`swift run muxy-app` during dev still activates — i.e. call it only when `Bundle.main.bundleIdentifier`
is nil.)

#### 3. App launches + supervises its own daemon

A **testable supervisor**, split like M5d's reconnect (pure policy in `MuxyCore`, real process I/O in
`MuxyApp`):

- **`MuxyCore/DaemonSupervisor`** — owns the relaunch policy. Injected seams: `spawn: () -> DaemonProcess`
  (starts the daemon, returns a handle) and the handle's termination signal; plus an injected async
  `sleep` (same pattern as `AppModel`). Behavior: `start()` spawns the daemon; when the process exits
  **unexpectedly** (not via `stop()`), relaunch with **bounded exponential backoff** (`0.5,1,2,4,8,10…`
  capped 10s); `stop()` sets a shutting-down flag, cancels the loop, and terminates the child.
  Unit-tested via a fake `DaemonProcess` that exits on demand (mirrors M5d's `SleepController` +
  fail-then-succeed transport).
- **`MuxyApp` real wiring** — supplies `spawn` as a `Foundation.Process` launching
  `Bundle.main/Resources/bin/muxy-daemon` with the per-user socket env (below) and the bundle's
  `bin/` on `PATH` (so any `muxy`/`muxy-hook` lookups resolve to the bundled copies). The app calls
  `supervisor.start()` in `bootstrap()` before `AppModel.connect()`. M5b's single-instance `flock`
  makes a redundant spawn (e.g. a dev daemon already running) exit with a **distinct code 3**
  ("single-instance loser") — the supervisor treats code 3 as "someone else owns the daemon" and
  **yields** (does not relaunch; the app connects to the existing daemon via M5d). Any other non-zero
  exit — a crash, or an `anyhow`-`Err` from `main` (e.g. a bind failure, which exits 1) — is a crash →
  relaunch with backoff. (Code 1 is deliberately NOT the flock signal, since `main() -> Result<()>`
  returning `Err` also yields 1.) On daemon crash, the supervisor relaunches **and** `AppModel`'s M5d
  reconnect re-attaches — the two compose.

#### 4. Per-user sockets

Change `muxy-config`'s socket **defaults** from `/tmp/muxy*.sock` to a **per-user runtime dir**:
`<runtime_dir>/muxy/{muxy.sock,muxy-control.sock,muxy-hook.sock}` where
`runtime_dir = $XDG_RUNTIME_DIR › $TMPDIR › /tmp` (identical to M5b's `InstanceLock::default_path`).
Env vars still win (dev/CI flows unchanged). The app computes the same per-user dir and sets
`MUXY_SOCK`/`MUXY_CONTROL_SOCK`/`MUXY_HOOK_SOCK` on the spawned daemon **and** reads `MUXY_CONTROL_SOCK`
for its own control connection (the app keeps env-based socket resolution — the M5 Swift boundary is
intact). Result: daemon, CLI, and app agree on per-user paths with no `/tmp` collision between users.

#### 5. Bundle metadata

`Info.plist`: `CFBundleIdentifier=com.github.richardcase.muxy`, `CFBundleName=Muxy`,
`CFBundleExecutable=muxy-app`, `CFBundleShortVersionString`/`CFBundleVersion` from the `VERSION` file
(`0.1.0` for M6a), `LSMinimumSystemVersion=14.0`, `CFBundleIconFile=Muxy`, `NSHighResolutionCapable=true`,
`CFBundlePackageType=APPL`. **Not** `LSUIElement` — it stays a regular dock app so M1d's menu-bar
status item + dock reopen keep working. A generated placeholder `Muxy.icns` (a simple solid-color
"M" rendered via `sips`/`iconutil` in the build script, so the bundle looks intentional and is
swappable later).

### M6b — Reproducible libghostty build script (spec now, build later)

Commit `scripts/build-libghostty.sh` capturing the manual recipe: check out ghostty at the pin
`2de5e7d38e1354759211722a8687c0815d2cf02c`, apply the `build.zig` Darwin-install patch, run
`SDKROOT=$(xcrun --show-sdk-path) zig build -Dapp-runtime=none -Demit-xcframework=false` (zig 0.16),
and copy the resulting `ghostty-internal.a` + `ghostty.h` to `macos/vendor/libghostty/`. Documents the
toolchain (full Xcode for Metal, zig 0.16, clang 17). This makes the vendored `.a` reproducible and is
the build step M6d's CI invokes. The `.a` stays gitignored (built by the script / cached in CI).

### M6c — Codesign → notarize → staple → DMG (DEFERRED — blocked on Developer ID)

Design (not built until a Developer ID exists): `codesign --deep --options runtime` with a hardened-
runtime entitlements plist and the "Developer ID Application" identity over `Muxy.app` (bundled
binaries signed inner-first); submit to Apple `notarytool` with an app-specific password / API key;
`stapler staple`; package a DMG (`create-dmg` or `hdiutil`). CI secrets: the signing identity (imported
into a temporary keychain) and the notarization credentials. This slice replaces M6d's "unsigned
artifact" step with a signed+notarized+stapled DMG.

### M6d — CI (GitHub Actions) (spec now, buildable unsigned)

`.github/workflows/ci.yml` on a `macos-14` runner: install zig 0.16 + select full Xcode; build
libghostty via `scripts/build-libghostty.sh` (**cached** by the ghostty pin so it's built once, not
every run); `cargo test` (whole workspace) + `cd macos && swift test` (MuxyCore); assemble the
**unsigned** `Muxy.app` via `scripts/build-app.sh` and upload it as a build artifact. This closes the
north-star "libghostty build in CI" risk gate. Signing/notarization are added to this workflow when
M6c lands (gated on the presence of the signing secret, so forks/PRs still build unsigned).

### M6e — Versioning + releases (spec now)

A single source of truth: a top-level `VERSION` file, kept in sync with a `vX.Y.Z` git tag. A
`scripts/set-version.sh` stamps `VERSION` → `Info.plist` (via `build-app.sh`) and the crate/package
versions (retiring `0.0.0`). A `release.yml` workflow on a `v*` tag builds the artifact (via M6d) and
publishes a GitHub Release with it attached. (Signed DMG + Homebrew bump join this once M6c/M6f land.)

### M6f — Homebrew cask + tap (DEFERRED — blocked on Developer ID / notarized artifact)

Design (not built until M6c produces a notarized DMG): a separate `richardcase/homebrew-muxy` tap repo
with a **cask** (`muxy.rb`) installing the notarized `.app`/DMG from the GitHub Release; the release CI
auto-bumps the cask's version + sha256. `brew install --cask richardcase/muxy/muxy`. (A CLI-only
formula for `muxy` is a possible later add-on; the cask is primary since the `.app` bundles the CLI.)

## Data flow

```
build:    scripts/build-libghostty.sh (M6b) ─► vendor/libghostty/ghostty-internal.a
          scripts/build-app.sh (M6a): cargo --release + swift -c release ─► Muxy.app
                                        (Info.plist, icon, Resources/bin/{daemon,muxy,muxy-hook})
runtime:  double-click Muxy.app ─► MuxyApp.bootstrap()
              DaemonSupervisor.start() ─► spawn Resources/bin/muxy-daemon
                  (env: per-user MUXY_*_SOCK, PATH=Resources/bin) ; flock guards single instance
              AppModel.connect() ─► MUXY_CONTROL_SOCK (per-user) ─► live
          daemon crash ─► supervisor relaunch (backoff) + AppModel M5d reconnect ─► live
          quit ─► supervisor.stop() (terminate child) + AppModel.shutdown()
CI (M6d): macos-14 ─► zig+Xcode ─► build-libghostty (cached) ─► cargo test + swift test
                                    ─► build-app.sh ─► upload unsigned Muxy.app
release (M6e): tag v* ─► build artifact ─► GitHub Release
deferred:  M6c sign+notarize+staple+DMG ; M6f Homebrew cask (need Developer ID)
```

## Decomposition (each its own plan → SDD → PR)

- **M6a — Bundle + self-contained runtime.** `scripts/build-app.sh`; bundle the 3 release binaries;
  `MuxyCore/DaemonSupervisor` (testable) + `MuxyApp` process wiring; bundle-relative `muxy` resolution;
  per-user socket defaults in `muxy-config` + app env; `Info.plist` + placeholder `.icns` + `VERSION`;
  retire the `setActivationPolicy` hack (dev-only now). **BUILD NOW.**
- **M6b — libghostty build script.** `scripts/build-libghostty.sh`. Spec now, build later (needs Xcode).
- **M6d — CI.** `.github/workflows/ci.yml` (unsigned). Spec now.
- **M6e — Versioning + releases.** `VERSION` + `set-version.sh` + `release.yml`. Spec now.
- **M6c — Sign/notarize/DMG.** DEFERRED (Developer ID).
- **M6f — Homebrew cask/tap.** DEFERRED (Developer ID / notarized artifact).

Order: **M6a → M6b → M6d → M6e**, then **M6c → M6f** once a Developer ID is available.

## Testing

- **M6a — `DaemonSupervisor` (`swift test`, MuxyCore):** a fake daemon process that exits unexpectedly
  drives a relaunch; backoff is bounded and non-decreasing; `stop()` terminates the child and cancels
  the loop (no relaunch after stop); an exit code 3 (single-instance loser) yields without relaunch,
  while a code-1 (generic `main` error) exit still relaunches.
  Mirrors M5d's deterministic seams (injected spawn + sleep).
- **M6a — `muxy-config` (`cargo test`):** the default socket dir resolves to `<runtime_dir>/muxy/…`
  (XDG › TMPDIR › /tmp); an env override still wins (no dev regression).
- **M6a — bundle (script + manual):** `scripts/build-app.sh` produces a `Muxy.app` whose layout
  matches the spec (assert the three `Resources/bin` binaries + `Info.plist` keys exist); **manual
  (user):** double-click `Muxy.app` → it launches, spawns its own daemon (no manual `cargo run`),
  agents spawn and run; `kill` the daemon PID → the app relaunches it and reconnects; quit → the child
  daemon is gone.
- **M6b/M6d (build-level):** `build-libghostty.sh` reproduces the vendored `.a` (checked in CI by a
  successful downstream link); the CI workflow goes green (libghostty cached, cargo+swift tests pass,
  unsigned `.app` artifact uploaded).
- **M6e:** a `v*` tag yields a GitHub Release with the artifact; `VERSION` flows into `Info.plist`.

## Risks

1. **libghostty in CI is heavy** — full Xcode + zig 0.16 + a 189 MB build. Mitigation: cache the built
   `.a` keyed by the ghostty pin so it builds once; the pin rarely changes.
2. **Daemon supervision vs. M5b single-instance + M5d reconnect** must compose, not fight — a redundant
   spawn must exit cleanly (flock) and the supervisor must not hot-loop relaunch a single-instance loser.
   Covered by the supervisor tests (immediate-exit-1 case) and the bounded backoff.
3. **Per-user socket default change** must not break dev/CI — env always wins and the default mirrors
   M5b's existing `runtime_dir`. Covered by the env-overrides-default test.
4. **Bundle-relative resolution regressing dev flows** — keep the `swift run muxy-app` (unbundled) dev
   path working via the `$MUXY_BIN`/`../target/debug` fallback and the "activate only if unbundled"
   guard.
5. **No Developer ID** blocks M6c/M6f. Scoped out this cycle; M6a/M6d/M6e deliver an unsigned but
   real, CI-built, versioned `.app`. Revisit signing when the account exists.

## Verification gate

Per slice: its tests green + existing suites green. **M6a end state:** `scripts/build-app.sh` produces
a `Muxy.app` that a user double-clicks to get a working muxy — it launches and supervises its own
bundled daemon (no manual daemon start), uses per-user sockets, survives a daemon crash (supervised
relaunch + reconnect), and no longer relies on the bare-exe activation hack. **Full-M6 end state
(across slices):** a reproducible from-source build (incl. libghostty), CI producing an unsigned
artifact, and tagged releases — with signing/notarization/Homebrew ready to switch on once a Developer
ID is available. Deferred (own efforts): M6c signing, M6f Homebrew, and (out of M6 entirely) Sparkle
auto-update and Linux packaging (M8).
