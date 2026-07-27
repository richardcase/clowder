# M0c-3b2-a — libghostty terminal-view spike: RESULT ✅

**Proven on screen:** a native SwiftPM app embedding one libghostty surface that runs
`muxy attach <pane>` renders a daemon-owned agent's terminal, takes real keyboard input, and
survives the window closing. This retires the last two open risks:

- **#4 compositing** — a libghostty surface renders correctly in a plain layer-backed `NSView` we
  provide (no manual Metal-layer setup; libghostty installs its own once given `wantsLayer` + the
  `nsview`).
- **#5 pump-as-PTY-child** — libghostty running `muxy attach <pane>` as its command works: the
  agent's shell renders, and closing + reopening the window re-attaches to the same live session
  (the daemon owns the PTY). The tmux client/server architecture is validated end to end.

## How it's built (`macos/`)

- `MuxyApp` executable target imports `ghostty.h` via the `GhosttyKit` module map and links the
  vendored `ghostty-internal.a` + frameworks (`Package.swift`).
- `main.swift`: `ghostty_init` → `ghostty_config_new`/`finalize` → a `ghostty_runtime_config_s`
  vtable (6 callbacks; only `wakeup_cb` ticks the app on main) → `ghostty_app_new` → NSApplication +
  window + `SurfaceView`.
- `SurfaceView` (`NSView`): `wantsLayer = true`, then `ghostty_surface_config_new` with
  `platform.macos.nsview`, `scale_factor`, `command = "muxy attach <pane>"`, and `MUXY_SOCK` in
  `env_vars` → `ghostty_surface_new`. Pushes `set_size` (backing pixels) + `set_content_scale`;
  forwards keys via `ghostty_surface_key` (raw macOS keycode + mapped mods; `text` only for
  printable chars so libghostty encodes Enter/Backspace/Ctrl).

## Reproduce

Prereq — build libghostty once (the vendored `.a` is gitignored) via the M0c-2 recipe and place it:
```
# in the pinned Ghostty checkout (see 2026-07-26-libghostty-build-spike.md):
SDKROOT="$(xcrun --show-sdk-path)" zig build -Dapp-runtime=none -Demit-xcframework=false
cp zig-out/lib/ghostty-internal.a  <muxy>/macos/vendor/libghostty/ghostty-internal.a
# ghostty.h is committed at macos/Sources/GhosttyKit/include/ghostty.h
```
Run:
```
./target/debug/muxy-daemon &                                  # binds /tmp/muxy{,-control,-hook}.sock
PANE=$(./target/debug/muxy spawn "$(pwd)" demo shell)         # prints a pane id
cd macos && MUXY_BIN="$(cd .. && pwd)/target/debug/muxy" swift run muxy-app "$PANE"
```

## Spike limitations (M0c-3b2-b / polish)

- Single terminal, pane id via argv — no sidebar/UI yet (that's M0c-3b2-b, driven by `MuxyCore`).
- Input: no IME/composed text, no modifier-only (`flagsChanged`) events, no mouse forwarding.
- Window resize resizes libghostty's terminal but the pump doesn't yet SIGWINCH→`Resize` the daemon's
  agent PTY (deferred: pump adaptation).
- `Package.swift` links the gitignored vendored `.a` by absolute path — reproducible only after the
  build-libghostty step above (fine for a spike; a real build wires libghostty into CI later).
- Spike-quality shell: programmatic `NSApplication`, no app bundle/Info.plist, macOS 13 target.
