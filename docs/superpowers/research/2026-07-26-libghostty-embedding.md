# libghostty Embedding C API — Findings for muxy's macOS SwiftUI Client

**Research date:** 2026-07-26
**Subject repo:** [`ghostty-org/ghostty`](https://github.com/ghostty-org/ghostty)
**Pinned commit for all citations:** `2de5e7d38e1354759211722a8687c0815d2cf02c` (branch `main`, committed 2026-07-26T04:05:01Z)
**Ghostty version at this commit:** `1.3.2-dev` (`build.zig.zon` `.version`)

> Permalink form used below: `https://github.com/ghostty-org/ghostty/blob/2de5e7d38e1354759211722a8687c0815d2cf02c/<path>#L<line>`
> All line numbers refer to the files as of this commit. `main` moves; re-verify before writing FFI code.

> **Sourcing note.** Every `ghostty_*` signature, struct layout, and Zig behavior below was read from the actual repository files at the pinned commit (`include/ghostty.h`, `src/apprt/embedded.zig`, `src/Surface.zig`, `src/renderer/Metal.zig`, `src/build/*`, `macos/Sources/Ghostty/**`). Where I state a fact I could not directly verify from source, it is explicitly marked **uncertain**. The header itself warns this is "not meant to be a general purpose embedding API (yet)" and that "the only consumer of this API is the macOS app" (`include/ghostty.h` lines 1-7).

---

## CRUX (make-or-break) — direct answer first

**No. As it exists today, the libghostty embedding C API does NOT let you drive a surface with externally-supplied bytes while the surface stays out of the PTY business. A `ghostty_surface_t` always creates and owns its own PTY and spawns a child process.** There is no `ghostty_*` function that feeds raw bytes into a surface's VT parser, and there is no "no command / no pty" surface mode. This is stated in the source itself:

- `src/Surface.zig` line 3: *"Each surface also creates and owns its pty..."*
  ([Surface.zig#L3](https://github.com/ghostty-org/ghostty/blob/2de5e7d38e1354759211722a8687c0815d2cf02c/src/Surface.zig#L3))
- The surface struct owns the terminal IO directly: `io: termio.Termio` and `io_thread: termio.Thread` (`src/Surface.zig` lines 127-128), and `Surface.init` unconditionally constructs a `termio.Exec` backend — i.e. it spawns a subprocess — via `termio.Exec.init(...)` and `termio.Termio.init(&self.io, ...)` (`src/Surface.zig` lines ~652-671).
  ([Surface.zig#L127](https://github.com/ghostty-org/ghostty/blob/2de5e7d38e1354759211722a8687c0815d2cf02c/src/Surface.zig#L127))
- The embedded apprt's `Surface.init` always calls `self.core_surface.init(...)`, and the code comment at the surface-creation site notes *"This will also initialize all the terminal IO."* (`macos/Sources/Ghostty/Surface View/SurfaceView_AppKit.swift` line 368).
  ([SurfaceView_AppKit.swift#L368](https://github.com/ghostty-org/ghostty/blob/2de5e7d38e1354759211722a8687c0815d2cf02c/macos/Sources/Ghostty/Surface%20View/SurfaceView_AppKit.swift#L368))

### What the config *does* let you control (and why it's not enough)

`ghostty_surface_config_s` (`include/ghostty.h` lines 467-480; Zig side `Surface.Options`, `src/apprt/embedded.zig` lines 426-466) exposes only these process knobs:

- `command` — *"The command to run in the new surface... This command always run in a shell (e.g. via `/bin/sh -c`)"* and setting it forces `wait-after-command = true` (`src/apprt/embedded.zig` lines 444-452, 531-538). If `command` is null, the surface falls back to the configured/default shell — **there is always a child**.
- `initial_input` — a one-shot string. Note carefully: it does **not** write bytes to the terminal's display. It is appended to `config.input` as an escaped `raw` entry (`src/apprt/embedded.zig` lines 554-572), i.e. it is fed to the **child process as input at startup**, not rendered as VT output. Not a per-frame data channel.
- `working_directory`, `env_vars`, `wait_after_command`, `font_size`, `scale_factor`, `context`.

There is **no** `ghostty_surface_write`, no `ghostty_surface_feed`, no VT-parser byte-sink, and no way to pass in a PTY master fd you already own. I enumerated the entire published API surface (`include/ghostty.h` lines 1061-1204) — the only ways data enters a surface are:

- `ghostty_surface_key(...)`, `ghostty_surface_text(...)`, `ghostty_surface_preedit(...)`, `ghostty_surface_mouse_*(...)` — these encode **user input** and write it **to the child via the PTY**; they are not a mechanism to inject display bytes.
- `initial_input` (startup only, also to the child).

The surface even advertises that it owns a real tty: `ghostty_surface_tty_name(...)` and `ghostty_surface_foreground_pid(...)` (`include/ghostty.h` lines 1119-1120). That only makes sense because the PTY is internal.

### Escape hatches, honestly ranked

1. **Relay/passthrough child process (pragmatic, keeps Ghostty's GPU renderer).** Set `command` to a tiny helper binary whose job is to connect to muxy's daemon socket and bridge stdin↔stdout to the daemon. Data flow: daemon owns the *real* PTY → streams bytes to the relay's stdout → relay writes to the surface's PTY → libghostty's VT parser + Metal renderer display it. Keystrokes go the other way: Ghostty encodes input → surface PTY → relay stdin → daemon. **Cost:** every surface owns a *second* PTY and a relay subprocess, and you get a double hop. It works with zero changes to libghostty, but it is architecturally a workaround, and features like `ghostty_surface_process_exited`, `foreground_pid`, child-exit actions, shell integration, and `wait-after-command` will all reflect the *relay*, not the daemon's real child. **Uncertain:** whether Ghostty's shell-integration/OSC features degrade acceptably through the relay — not verified.

2. **Use `libghostty-vt` + write your own renderer (decouples from PTY entirely, loses Ghostty's renderer).** Ghostty is being refactored into a family of libs; `libghostty-vt` is *"a zero-dependency library that provides an API for parsing terminal sequences and maintaining terminal state"* (Mitchell Hashimoto, "Libghostty Is Coming", mitchellh.com/writing/libghostty-is-coming). The build system already produces it: `build.zig` builds `libghostty-vt` shared + static libs and an Apple xcframework (`build.zig` lines 117-171), and `src/build/GhosttyLibVt.*` exists. Its headers live under `include/ghostty/` (referenced in `src/build/GhosttyXCFramework.zig` lines 56-60). **This is the clean way to feed externally-supplied bytes and maintain terminal state** — but it gives you a parser/screen model, **not** a Metal renderer or input encoder. muxy would render cells itself. **Uncertain / important:** I did **not** audit the `libghostty-vt` C headers in this pass, so its exact byte-feed API, its stability, and whether it exposes a render-ready cell grid are **unverified**. Mitchell's own post calls the whole C API a "public alpha (not promising API stability)."

3. **Don't use libghostty's surface renderer at all** — treat Ghostty purely as reference and build rendering on your own stack. (Fallback, not a libghostty embedding path.)

**Bottom line for muxy's architecture:** if the daemon must own the PTY, the surface embedding API (`ghostty_surface_t`) is a poor fit unless you accept the relay-subprocess workaround (#1). The conceptually correct fit for muxy's "daemon owns PTY, client renders bytes" model is `libghostty-vt` (#2) **plus your own renderer** — but that trades away exactly the thing (Ghostty's GPU renderer) you might have wanted libghostty for, and its API is unverified and alpha.

---

## 1. Surface / PTY ownership (detail behind the CRUX)

Covered above. Key citations restated for completeness:

| Claim | Source |
|---|---|
| Surface owns its pty | `src/Surface.zig` L3, L127-128 |
| Surface always spawns a subprocess (`termio.Exec`) | `src/Surface.zig` ~L652-671 |
| `command` runs via `/bin/sh -c`, forces `wait-after-command` | `src/apprt/embedded.zig` L444-452, L531-538 |
| `initial_input` → child input, not display bytes | `src/apprt/embedded.zig` L554-572 |
| No byte-feed / no PTY-fd-passing function in the API | full enumeration of `include/ghostty.h` L1061-1204 |
| Surface exposes its own tty name & fg pid | `include/ghostty.h` L1119-1120 |

---

## 2. Surface / render model — the actual functions

All from `include/ghostty.h` at the pinned commit.

### (a) App / runtime init + config
```c
int              ghostty_init(uintptr_t, char**);                         // L1064
ghostty_info_s   ghostty_info(void);                                      // L1066
ghostty_config_t ghostty_config_new();                                    // L1070
void             ghostty_config_load_default_files(ghostty_config_t);     // L1075
void             ghostty_config_finalize(ghostty_config_t);               // L1077
ghostty_app_t    ghostty_app_new(const ghostty_runtime_config_s*, ghostty_config_t); // L1087
void             ghostty_app_tick(ghostty_app_t);                         // L1090
void             ghostty_app_set_focus(ghostty_app_t, bool);              // L1092
void             ghostty_app_set_color_scheme(ghostty_app_t, ghostty_color_scheme_e); // L1099
```
`ghostty_config_finalize` must be called before use; the app is created from a runtime-config vtable + a config handle. (`ghostty_init` takes `argc`/`argv`-style args.)

### (b)+(c) Create a surface bound to a caller-provided native view
```c
ghostty_surface_config_s ghostty_surface_config_new();                    // L1101
ghostty_surface_t        ghostty_surface_new(ghostty_app_t, const ghostty_surface_config_s*); // L1103
void                     ghostty_surface_free(ghostty_surface_t);         // L1105
```
You hand it the NSView as an opaque pointer through the tagged platform union:
```c
typedef struct { void* nsview; } ghostty_platform_macos_s;                // L448-450
typedef struct { void* uiview; } ghostty_platform_ios_s;                  // L452-454
typedef union  { ghostty_platform_macos_s macos; ghostty_platform_ios_s ios; } ghostty_platform_u; // L456-459
// in ghostty_surface_config_s: ghostty_platform_e platform_tag; ghostty_platform_u platform;  // L467-480
```
`platform_tag` must be `GHOSTTY_PLATFORM_MACOS` (=1). On the Zig side, a null `nsview` is a hard error: `error.NSViewMustBeSet` (`src/apprt/embedded.zig` L378-383). **You pass a bare NSView; libghostty takes over its layer (see Q3).** There is no separate "attach layer" call and no CAMetalLayer parameter in the C API.

### (d) Draw / refresh
```c
void ghostty_surface_refresh(ghostty_surface_t);   // L1112  (request redraw / mark dirty)
void ghostty_surface_draw(ghostty_surface_t);      // L1113  (perform a draw)
```
**Important nuance:** on macOS the app does **not** call `ghostty_surface_draw` at all — libghostty runs its own render thread driven by a Core Animation display callback (see Q3). `ghostty_surface_draw` exists for embedders that must drive drawing manually. Whether you *must* call it on macOS is **uncertain**; the shipping macOS app does not.

### (e) Resize + content-scale / HiDPI
```c
void                   ghostty_surface_set_size(ghostty_surface_t, uint32_t, uint32_t);          // L1117  (pixels)
void                   ghostty_surface_set_content_scale(ghostty_surface_t, double, double);     // L1114
ghostty_surface_size_s ghostty_surface_size(ghostty_surface_t);                                  // L1118
void                   ghostty_surface_set_display_id(ghostty_surface_t, uint32_t);              // L1167 (__APPLE__)
void                   ghostty_surface_set_occlusion(ghostty_surface_t, bool);                   // L1116
```
`ghostty_surface_size_s` returns `{columns, rows, width_px, height_px, cell_width_px, cell_height_px}` (`include/ghostty.h` L482-489). Set size in **pixels**; set content scale from `window.backingScaleFactor`.

### (f) Key / mouse / text input
```c
bool ghostty_surface_key(ghostty_surface_t, ghostty_input_key_s);                                // L1125
void ghostty_surface_text(ghostty_surface_t, const char*, uintptr_t);                            // L1129
void ghostty_surface_preedit(ghostty_surface_t, const char*, uintptr_t);                         // L1130  (IME)
bool ghostty_surface_mouse_button(ghostty_surface_t, ghostty_input_mouse_state_e,
                                  ghostty_input_mouse_button_e, ghostty_input_mods_e);            // L1132
void ghostty_surface_mouse_pos(ghostty_surface_t, double, double, ghostty_input_mods_e);         // L1136
void ghostty_surface_mouse_scroll(ghostty_surface_t, double, double, ghostty_input_scroll_mods_t);// L1140
void ghostty_surface_mouse_pressure(ghostty_surface_t, uint32_t, double);                        // L1144
void ghostty_surface_ime_point(ghostty_surface_t, double*, double*, double*, double*);           // L1145
ghostty_input_mods_e ghostty_surface_key_translation_mods(ghostty_surface_t, ghostty_input_mods_e);// L1123
bool ghostty_app_key(ghostty_app_t, ghostty_input_key_s);                                        // L1093 (app-level keybinds)
```
`ghostty_input_key_s` = `{action, mods, consumed_mods, keycode, const char* text, unshifted_codepoint, composing}` (`include/ghostty.h` L350-358). Keycodes are a W3C-`code`-based enum `ghostty_input_key_e` (L155-348). Both `ghostty_surface_key` and `ghostty_app_key` return `bool` = "consumed by a binding" (see `src/Surface.zig` L110 note).

### (g) The embedder-supplied callback vtable
There is **one** struct with a small fixed set of callbacks (`include/ghostty.h` L1019-1028):
```c
typedef struct {
  void* userdata;
  bool  supports_selection_clipboard;
  ghostty_runtime_wakeup_cb                 wakeup_cb;                 // void(*)(void*)                     L1000
  ghostty_runtime_action_cb                 action_cb;                // bool(*)(app, target, action)       L1015-1017
  ghostty_runtime_read_clipboard_cb         read_clipboard_cb;        // bool(*)(void*, clipboard_e, void*) L1001
  ghostty_runtime_confirm_read_clipboard_cb confirm_read_clipboard_cb;//                                    L1004
  ghostty_runtime_write_clipboard_cb        write_clipboard_cb;       //                                    L1009
  ghostty_runtime_close_surface_cb          close_surface_cb;         // void(*)(void*, bool)               L1014
} ghostty_runtime_config_s;
```
**Critical design point:** title, bell, desktop notification, cursor/mouse shape, pwd, clipboard-via-OSC52, renderer health, open-URL, child-exited, progress reports, "ring bell", set-title, etc. are **NOT individual callbacks**. They are all delivered through the single `action_cb`, which receives a tagged union `ghostty_action_s = {ghostty_action_tag_e tag; ghostty_action_u action;}` (`include/ghostty.h` L995-998). The tag enum `ghostty_action_tag_e` (L885-952) includes e.g. `GHOSTTY_ACTION_SET_TITLE`, `GHOSTTY_ACTION_RING_BELL`, `GHOSTTY_ACTION_DESKTOP_NOTIFICATION`, `GHOSTTY_ACTION_MOUSE_SHAPE`, `GHOSTTY_ACTION_PWD`, `GHOSTTY_ACTION_CLIPBOARD_*`-adjacent, `GHOSTTY_ACTION_PROGRESS_REPORT`, `GHOSTTY_ACTION_SHOW_CHILD_EXITED`, `GHOSTTY_ACTION_RENDERER_HEALTH`, `GHOSTTY_ACTION_OPEN_URL`, and ~65 others. The payload structs are defined at L661-993 (e.g. `ghostty_action_desktop_notification_s {title, body}` L661-664; `ghostty_action_set_title_s {title}` L667-669). `action_cb` returns `bool` = whether the embedder handled it. So: **implement one big `action_cb` switch**, not a dozen callbacks.

The wakeup callback (`wakeup_cb`) is how libghostty asks the main thread to run `ghostty_app_tick` — it's an event-loop integration hook.

Clipboard has dedicated callbacks (read/confirm-read/write) plus a completion function `ghostty_surface_complete_clipboard_request(...)` (L1155) because clipboard is async and needs a round-trip.

---

## 3. How Ghostty's own macOS app embeds it

Swift wrapper types (all under `macos/Sources/Ghostty/`):

- **`Ghostty.App`** (`Ghostty.App.swift`) — builds the `ghostty_runtime_config_s`, calls `ghostty_app_new`. The callbacks are Swift closures bridged to C function pointers (`Ghostty.App.swift` L60-74):
  ```swift
  var runtime_cfg = ghostty_runtime_config_s(
      userdata: ...,
      supports_selection_clipboard: true,
      wakeup_cb: { userdata in App.wakeup(userdata) },
      action_cb: { app, target, action in App.action(app!, target: target, action: action) },
      read_clipboard_cb: ...,
      confirm_read_clipboard_cb: ...,
      write_clipboard_cb: ...,
      close_surface_cb: ...)
  guard let app = ghostty_app_new(&runtime_cfg, config.config) else { ... }   // L73
  ```
  ([Ghostty.App.swift#L60](https://github.com/ghostty-org/ghostty/blob/2de5e7d38e1354759211722a8687c0815d2cf02c/macos/Sources/Ghostty/Ghostty.App.swift#L60))
  The giant `action_cb` switch lives in `Ghostty.Action.swift`.

- **`Ghostty.Surface`** (`Ghostty.Surface.swift`) — thin value wrapper around the opaque `ghostty_surface_t` (`Ghostty.Surface(cSurface: surface)`).

- **`Ghostty.SurfaceView`** — the NSView. Class chain: `SurfaceView: OSSurfaceView` (`SurfaceView_AppKit.swift` L10), and `OSSurfaceView: OSView, ObservableObject` (`OSSurfaceView.swift` L6), where `OSView` is a cross-platform typealias for `NSView`/`UIView`. Creation (`SurfaceView_AppKit.swift` L368-377):
  ```swift
  // "Setup our surface. This will also initialize all the terminal IO."
  let surface = surface_cfg.withCValue(view: self) { surface_cfg_c in
      ghostty_surface_new(app, &surface_cfg_c)   // passes `self` (the NSView) as nsview
  }
  self.surfaceModel = Ghostty.Surface(cSurface: surface)
  ```

### Metal layer setup — done INSIDE libghostty, not in Swift
This is the key surprise for an embedder: **the Swift side never creates a `CAMetalLayer` or an `MTKView`.** It hands over a plain NSView, and libghostty's renderer takes over that view's layer. From `src/renderer/Metal.zig`:

- It reads `nsview` from the apprt surface (`.macos => |v| v.nsview`, L100).
- It creates its own layer (`IOSurfaceLayer` — an IOSurface-backed `CALayer`, **not** a vanilla `CAMetalLayer`; `layer: IOSurfaceLayer`, L41; `var layer = try IOSurfaceLayer.init();`, L111).
- On macOS it makes the view **layer-hosting** by assigning the layer to the view's `layer` property *before* it's added to a window: `info.view.setProperty("layer", layer.layer.value);` (L124). On iOS it instead `addSublayer:` (L129-130).
- It sets `contentsScale` (L143), `needsDisplayOnBoundsChange` (L146), and installs a display callback that self-drives rendering: `self.layer.setDisplayCallback(...)` (L166).
  ([Metal.zig#L100](https://github.com/ghostty-org/ghostty/blob/2de5e7d38e1354759211722a8687c0815d2cf02c/src/renderer/Metal.zig#L100))

Consequences for muxy:
- Provide a **layer-hosting-capable NSView** and do **not** set your own `wantsLayer`/backing layer expectations — libghostty will overwrite `view.layer`.
- You do **not** run a Metal draw loop; libghostty's render thread does. You just forward resize (`ghostty_surface_set_size`), scale (`ghostty_surface_set_content_scale`, from `backingScaleFactor`; see `SurfaceView_AppKit.swift` L846-873), focus, occlusion, and input.

### Input routing (macOS)
- `mouseDown`/`mouseUp`/`otherMouseDown` → `ghostty_surface_mouse_button(...)` (`SurfaceView_AppKit.swift` L883-942), pressure → `ghostty_surface_mouse_pressure` (L1064).
- `keyDown`/`keyUp` build a `ghostty_input_key_s` and call `ghostty_surface_key(...)` (L1471-1509); mods translated via `ghostty_surface_key_translation_mods` (L1089).
- Committed text (IME/insertText) → `ghostty_surface_text(surface, ptr, len)` (L2188).
- A local `NSEvent` monitor handles `.keyUp` and `.leftMouseDown` edge cases (L354-366).

---

## 4. Build & linking

### Zig version
`build.zig.zon` pins `.minimum_zig_version = "0.16.0"` (`build.zig.zon` L6). **You have 0.16.0 → this matches the current pin.** Caveat: it's a *minimum*, and Ghostty tracks Zig closely; a future `main` bump could break your build. Pin Ghostty to a known-good commit (see Q5) and keep a matching Zig toolchain.

### What `zig build` produces
Two distinct libraries — do not confuse them:

1. **libghostty (the full embedding lib with Surface + Metal renderer)** — internally the artifact is named `libghostty-internal.a` (`src/build/GhosttyLib.zig` L69, L299), built as a **static** lib (`GhosttyLib.initStatic`, L21; `link_libc = true`, L34). It is produced when the build's app-runtime is `none` (`build.zig` L177-208: *"Runtime 'none' is libghostty"*). In practice: `zig build -Dapp-runtime=none` (confirm the exact flag against `src/build/Config.zig` before relying on it — **flag name uncertain, not verified verbatim**).

2. **libghostty-vt (the zero-dep VT parser/state lib)** — separate; emits shared `.dylib`/`.so`, static `libghostty-vt.a`, and an Apple xcframework (`build.zig` L117-171). Headers under `include/ghostty/`.

### Headers produced
For the macOS xcframework, the build copies exactly two files into the headers dir (`src/build/GhosttyXCFramework.zig` L61-64):
- `include/ghostty.h` (the umbrella header)
- `include/module.modulemap`

The module map (`include/module.modulemap`) is:
```
module GhosttyKit {
    umbrella header "ghostty.h"
    export *
}
```
So the Swift import is `import GhosttyKit`.

### How the macOS app links it — and the Xcode constraint (relevant to you)
Ghostty's own app links a **`GhosttyKit.xcframework`** built by `src/build/GhosttyXCFramework.zig` (name `"GhosttyKit"`, out path `macos/GhosttyKit.xcframework`, L68-70), which wraps the static libs + the 2 headers + dSYMs for macOS-universal, iOS, and iOS-sim slices.

**⚠️ The xcframework packaging step shells out to `xcodebuild`:** `src/build/XCFrameworkStep.zig` L52 runs `{ "xcodebuild", "-create-xcframework" }`. **`xcodebuild` requires full Xcode, which you do NOT have (CLT + Swift 6.2 only).** Therefore you **cannot** build `GhosttyKit.xcframework` on your machine as-is.

**Recommended path for muxy given CLT-only:** skip the xcframework. For a single native macOS target you can:
1. Build just the static lib: `zig build` with app-runtime `none` to get `libghostty-internal.a` (this step does **not** need `xcodebuild` — verify by building only the lib target, not the xcframework install step).
2. Add `include/ghostty.h` + `include/module.modulemap` to your include path.
3. From SwiftPM, use a `.systemLibrary` / C target module map (or `-Xcc -fmodule-map-file=…`) to `import GhosttyKit`, and link the static `.a`.
4. **Link the frameworks the static lib defers to its consumer**: at minimum `Metal`, `QuartzCore`, `Foundation`, `AppKit`, `CoreText`, `CoreGraphics`, plus `libc`/system libs. (`GhosttyLib.initStatic` is a static archive; framework linking is the app's responsibility. The **exact** framework list is **uncertain — not verified from source**; derive it from link errors or from Ghostty's Xcode project settings.)

**Uncertain:** whether every part of the standard `zig build` graph avoids `xcodebuild` when you target only the static lib. The xcframework step definitely needs Xcode; a bare static-lib build *should* not, but I did not execute the build to confirm. Validate empirically.

---

## 5. API stability & pinning

- The header states up front it is **not** a general-purpose embedding API "(yet)" and that the sole consumer is the macOS app (`include/ghostty.h` L1-7). It also contains a "APIs I'd like to get rid of eventually" section (L1198-1200, `ghostty_set_window_background_blur`).
- Mitchell Hashimoto ("Libghostty Is Coming", mitchellh.com): the C API is a **"public alpha (not promising API stability)"**; "the 'alpha' quality is with respect to the API (functions and types) itself" (logic is production-proven). The clean-slate public C API he describes is being built around `libghostty-vt` first.
- The types carry an explicit sync warning: the C structs/enums **must be kept in sync with their Zig counterparts** by hand (`include/ghostty.h` L62-64) — meaning silent ABI drift between commits is possible if you mix a header and a lib from different commits.
- There is **no** semantic-version guarantee or ABI-version symbol for the embedding API. Version is only the app version (`1.3.2-dev`, `build.zig.zon` L3; also queryable at runtime via `ghostty_info()` → `ghostty_info_s{build_mode, version, version_len}`, `include/ghostty.h` L392-396, L1066).

**Recommended pinning strategy:**
1. Pin Ghostty to an **exact commit SHA** (not `main`, not a tag range) and vendor it as a submodule/lockfile. Use commit `2de5e7d38e1354759211722a8687c0815d2cf02c` or newer-known-good.
2. Pin the Zig toolchain to the version that commit expects (≥ `0.16.0`; you have `0.16.0`).
3. Build `ghostty.h` and the `.a` **from the same commit** — never mix a checked-in header with a differently-versioned binary (hand-synced structs, see above).
4. Assert `ghostty_info().version` at startup against your expected build.
5. Budget for churn on **every** bump: re-diff `include/ghostty.h` and the `ghostty_action_tag_e` enum, since new action tags appear frequently and your `action_cb` switch must stay exhaustive.

---

## Top risks for a from-scratch embedder

1. **PTY ownership is the wrong shape for muxy (the CRUX).** `ghostty_surface_t` always spawns and owns a child + PTY; there is no byte-feed and no PTY-fd handoff. Either accept a per-surface relay subprocess (double PTY, degraded shell-integration/child-exit semantics) or move to `libghostty-vt` + your own renderer (losing Ghostty's GPU renderer, against an unaudited/alpha API).
2. **Alpha, hand-synced, unversioned ABI.** No stability promise, C structs manually mirrored from Zig, `action_cb` tag enum grows commit-to-commit. Header/lib mismatch → silent memory corruption. Mitigate only via exact-commit pinning + startup version assert.
3. **Xcode dependency in the packaging path.** `xcframework` creation needs `xcodebuild`; you have CLT only. You must instead link the raw static lib via a SwiftPM module map and manually link Metal/QuartzCore/Foundation/AppKit — a path Ghostty itself does not use, so it's under-documented and you'll debug link errors.
4. **Renderer seizes your NSView's layer.** libghostty makes the view layer-hosting and installs its own IOSurface layer + render thread (`Metal.zig` L111-166). Your SwiftUI/AppKit layer assumptions (backing layer, overlays, `wantsLayer`) must accommodate that; compositing SwiftUI over it is your problem.
5. **"One giant callback" surprise.** Nearly all embedder integration (title, bell, notifications, pwd, mouse shape, open-URL, child-exit, progress, renderer health…) funnels through a single `action_cb` tagged union with ~65 variants that must stay exhaustive across upgrades.
6. **Zig-version coupling.** Ghostty tracks Zig tightly; a Ghostty bump may demand a newer Zig than 0.16.0. Toolchain and source must move together.

## What remains uncertain (explicitly)

- **`libghostty-vt`'s actual C API** — its byte-feed function(s), whether it exposes a render-ready cell grid, its header layout under `include/ghostty/`, and its stability. **Not audited in this pass.** This is the single most important follow-up for muxy, since it's the architecturally-correct path. Fetch and read `include/ghostty/*.h` and `src/build/GhosttyLibVt.zig` next.
- Whether **`ghostty_surface_draw` must be called by a macOS embedder**, or the internal render thread fully self-drives (the shipping app never calls it, suggesting self-drive, but not proven).
- The **exact `zig build` flag** to emit only the embedding static lib (`-Dapp-runtime=none` inferred from `build.zig` L177-208; not verified against `src/build/Config.zig` option parsing).
- Whether a **bare static-lib build truly avoids `xcodebuild`** end-to-end (only the xcframework step is confirmed to need it).
- The **complete framework/link list** the static `libghostty-internal.a` expects its consumer to provide (Metal/QuartzCore/Foundation/AppKit/CoreText are near-certain, but the full set is unverified).
- Behavior/feature-degradation of the **relay-subprocess workaround** (escape hatch #1) w.r.t. shell integration, OSC handling, and child-exit reporting.
- Line numbers cited are from `main` at the pinned commit; **`main` moves** — re-verify against your chosen pinned commit before writing FFI.

---

### Appendix — primary sources consulted (all at commit `2de5e7d…`)
- `include/ghostty.h` (full read, 1209 lines) — API enumeration
- `include/module.modulemap` — Swift module name `GhosttyKit`
- `src/apprt/embedded.zig` — embedded apprt: `Surface.Options`, `Surface.init`, `ghostty_surface_new` (L1536+), platform union, command/initial_input handling
- `src/Surface.zig` — core surface owns pty + termio (L3, L127-128, ~L652-671)
- `src/renderer/Metal.zig` — layer-hosting NSView takeover, IOSurfaceLayer, display callback
- `src/build/GhosttyXCFramework.zig`, `src/build/GhosttyLib.zig`, `src/build/XCFrameworkStep.zig`, `build.zig`, `build.zig.zon` — build/link/xcodebuild/zig-version facts
- `macos/Sources/Ghostty/Ghostty.App.swift`, `.../Surface View/SurfaceView_AppKit.swift`, `.../OSSurfaceView.swift` — macOS embedding, callback wiring, input routing
- Mitchell Hashimoto, "Libghostty Is Coming" (mitchellh.com/writing/libghostty-is-coming) — alpha status, libghostty-vt intent. *(Secondary/author blog, used only for stability posture and the libghostty-vt roadmap, both corroborated by the build system.)*
