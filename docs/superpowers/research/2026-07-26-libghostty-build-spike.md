# M0c-2 Spike — Building libghostty on macOS (findings so far)

Status: **build/link de-risk in progress; blocked on full Xcode.** This records what the spike
established so the work is reproducible when we resume.

## Goal

Produce a macOS static `libghostty` (`ghostty-internal.a` + `ghostty.h`) that a SwiftPM app can
link, so a libghostty surface (command = `muxy attach <pane>`) can render a daemon-owned agent.

## Environment

- Ghostty pinned at commit `2de5e7d38e1354759211722a8687c0815d2cf02c`.
- Zig `0.16.0` — **matches** Ghostty's `minimum_zig_version`. ✅
- Swift 6.2, clang 17, via **Command Line Tools only — no full Xcode**.

## What worked (CLT-only)

`zig build` almost entirely succeeds under CLT: **119/127 steps**. `libghostty-vt.a` (the
standalone VT parser) and `ghostty.h` (the C embedding header, ~312 `ghostty_*` symbols) build
fine — they have no Metal/AppKit deps.

Two workarounds were needed to even reach the lib build:

1. **`error: DarwinSdkNotFound`** on the default build — the default graph builds an `xcframework`
   that includes an **iOS** static lib, and CLT has no iOS SDK. Fix: `-Demit-xcframework=false`
   (also makes `emit_macos_app=false`) and `-Dapp-runtime=none`. Set `SDKROOT=$(xcrun --show-sdk-path)`.
2. On macOS the plain static-lib install is deliberately guarded off (`build.zig`: *"we don't
   currently build on macOS this way"*), so the `.a`/header never install. **Patch** (`build.zig`,
   the `else if (!config.emit_lib_vt)` block) — add a Darwin branch:
   ```zig
   if (config.target.result.os.tag.isDarwin()) {
       lib_static.installHeader();
       lib_static.install("ghostty-internal.a");
   } else if (!config.target.result.os.tag.isDarwin()) {
       // ... original non-Darwin branch unchanged ...
   }
   ```

Build command:
```
SDKROOT="$(xcrun --show-sdk-path)" zig build -Dapp-runtime=none -Demit-xcframework=false
```

## The blocker: Metal shader compiler needs full Xcode

The build fails at **1 step** — compiling the renderer's Metal shaders:
```
xcrun: error: unable to find utility "metal", not a developer tool or in PATH
   failed command: xcrun -sdk macosx metal -o Ghostty.ir -c src/renderer/shaders/shaders.metal
```
The `metal`/`metallib` shader compilers ship with **full Xcode**, not CLT.

**General consequence:** *any* native GPU terminal renderer on macOS — libghostty's, a hand-rolled
Metal one, or GPUI/Zed's — needs custom Metal shaders, so all of them require full Xcode to build
here. Only a webview (xterm.js/Tauri, WebKit) or legacy OpenGL avoids it.

**Decision (user):** install full Xcode and continue the native libghostty path.

## Resume steps (once Xcode is installed)

1. Point the toolchain at Xcode + accept the license:
   ```
   sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
   sudo xcodebuild -license accept
   ```
2. Verify the previously-missing pieces exist:
   ```
   xcrun metal --version            # Metal shader compiler present
   xcrun --sdk iphoneos --show-sdk-path   # iOS SDK present (proves full Xcode)
   ```
3. Re-run the same lean build (the workaround is still the leanest way to get just the macOS lib):
   ```
   SDKROOT="$(xcrun --show-sdk-path)" zig build -Dapp-runtime=none -Demit-xcframework=false
   ```
   Expect `zig-out/lib/ghostty-internal.a` + `zig-out/include/ghostty.h`.
4. Continue the spike: minimal SwiftPM executable → `GhosttyKit` module map over `ghostty.h` →
   link `ghostty-internal.a` + hand-link Metal/QuartzCore/Foundation/AppKit → call a trivial
   `ghostty_*` function (CLI-verifiable) → then the NSView + surface + `muxy attach` render (visual).

## Note (from the research doc)

`libghostty-vt.a` built cleanly and is the parser-only path the research flagged. If the native
libghostty renderer ever proves too heavy, that + a self-render remains the escape hatch — but it
would *also* need Xcode for its own Metal shaders, so it does not avoid the Xcode requirement.
