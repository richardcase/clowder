# libghostty macOS embedding — findings for muxy M0c-3b2-a

Source: pinned commit `2de5e7d38`, read on disk (not the web).

- C header: `ghostty/zig-out/include/ghostty.h`
- Swift embedder: `ghostty/macos/Sources/Ghostty/**` and app entry `ghostty/macos/Sources/App/macOS/main.swift`

All paths below are relative to the on-disk checkout at
`/private/tmp/claude-501/-Users-richard-code-muxy/9aa331f1-b990-489a-80ce-9931a96fea99/scratchpad/`.
`ghostty.h:NNNN` refers to a line in the header; `ghostty.h:symbol` to a declaration.

Everything here is traced from Ghostty's OWN macOS app. Where I am inferring
(e.g. "libghostty must be creating the layer because no Swift code does"),
I say so explicitly. See the two honesty sections at the end.

---

## TL;DR — the minimal init → surface → draw → input recipe

Ghostty's macOS app does exactly this (call sites cited inline below):

1. **Once per process, before anything else:**
   `ghostty_init(argc, argv)` → must return `GHOSTTY_SUCCESS` (0).
   (`main.swift:8`; `ghostty.h:1064`)
   Ghostty then calls `ghostty_cli_try_action()` (`main.swift:31`) — **muxy can skip this**;
   it only handles `ghostty +action` CLI subcommands.

2. **Build a config object:**
   `ghostty_config_new()` → `ghostty_config_finalize(cfg)`.
   File/CLI loading in between is **optional** (see Q1). (`Ghostty.Config.swift:62,92`)

3. **Create the app with a runtime callback vtable:**
   Fill a `ghostty_runtime_config_s` (userdata + 6 callbacks), then
   `ghostty_app_new(&runtime_cfg, cfg)`. (`Ghostty.App.swift:60-73`; `ghostty.h:1087`)
   Then `ghostty_app_set_focus(app, true)` (`Ghostty.App.swift:82`).

4. **Create an NSView, then a surface bound to it:**
   Start from `ghostty_surface_config_new()`, set
   `platform_tag = GHOSTTY_PLATFORM_MACOS`, `platform.macos.nsview = <NSView*>`,
   `userdata = <NSView*>`, `scale_factor`, and (for muxy) `command`.
   Then `ghostty_surface_new(app, &surface_cfg)`.
   (`Surface View/SurfaceView.swift:682-744`; `Surface View/SurfaceView_AppKit.swift:370-372`; `ghostty.h:1101,1103`)

5. **libghostty renders itself. The embedder does NOT drive draw.**
   `ghostty_surface_draw` exists (`ghostty.h:1113`) but is **never called anywhere in
   Ghostty's macOS code** (grep of `macos/` returns zero hits). libghostty attaches a
   `CAMetalLayer` to the `nsview` and runs its own render thread / CVDisplayLink.
   The embedder only pushes state changes:
   - resize → `ghostty_surface_set_size(surface, wpx, hpx)` (`SurfaceView_AppKit.swift:474`)
   - HiDPI → `ghostty_surface_set_content_scale(surface, xScale, yScale)` (`SurfaceView_AppKit.swift:873`)
   - focus → `ghostty_surface_set_focus(surface, bool)` (`SurfaceView_AppKit.swift:435`)
   - keyboard → `ghostty_surface_key(surface, key_s)` (`SurfaceView_AppKit.swift:1471`)
   - text/IME → `ghostty_surface_text(surface, ptr, len)` (`Ghostty.Surface.swift:47`)
   - mouse → `ghostty_surface_mouse_button/pos/scroll` (`Ghostty.Surface.swift:119,135,151`)

6. **Run loop integration:** the `wakeup_cb` fires from any thread; the embedder
   hops to the main thread and calls `ghostty_app_tick(app)` (`Ghostty.App.swift:434-441,116-118`).
   Everything else is the normal `NSApplicationMain` AppKit run loop (`main.swift:33`).

That is the whole spike. Sizes are in **backing pixels** (post-scale), not points.

---

## Q1 — Minimal init → surface sequence (real signatures + what Ghostty passes)

### `ghostty_init`
```c
GHOSTTY_API int ghostty_init(uintptr_t, char**);   // ghostty.h:1064
```
Ghostty: `ghostty_init(UInt(CommandLine.argc), CommandLine.unsafeArgv)`, checked
`!= GHOSTTY_SUCCESS` (`main.swift:8`). Called once, first thing in `main`.
`GHOSTTY_SUCCESS == 0` (`ghostty.h:29`).

### Config
```c
GHOSTTY_API ghostty_config_t ghostty_config_new();                       // ghostty.h:1070
GHOSTTY_API void ghostty_config_load_file(ghostty_config_t, const char*); // ghostty.h:1074
GHOSTTY_API void ghostty_config_load_default_files(ghostty_config_t);     // ghostty.h:1075
GHOSTTY_API void ghostty_config_load_cli_args(ghostty_config_t);          // ghostty.h:1073
GHOSTTY_API void ghostty_config_load_recursive_files(ghostty_config_t);   // ghostty.h:1076
GHOSTTY_API void ghostty_config_finalize(ghostty_config_t);              // ghostty.h:1077
GHOSTTY_API uint32_t ghostty_config_diagnostics_count(ghostty_config_t);  // ghostty.h:1083
```
Ghostty's real order (`Ghostty.Config.swift:60-93`):
`ghostty_config_new()` → (`load_file` **or** `load_default_files`) → `load_cli_args`
→ `load_recursive_files` → `ghostty_config_finalize`.

**For the spike, the minimum is `ghostty_config_new()` then `ghostty_config_finalize()`.**
`finalize` "will make our defaults available" (comment at `Ghostty.Config.swift:90-92`).
All the `load_*` calls are optional — skip them and you get a valid default config
(default shell, default font, etc.). Do **not** skip `finalize`.
`ghostty_config_new()` can return NULL — check it (`Ghostty.Config.swift:62`).

### App
```c
GHOSTTY_API ghostty_app_t ghostty_app_new(const ghostty_runtime_config_s*,
                                          ghostty_config_t);   // ghostty.h:1087
GHOSTTY_API void ghostty_app_set_focus(ghostty_app_t, bool);   // ghostty.h:1092
```
Ghostty: `ghostty_app_new(&runtime_cfg, config.config)`, NULL-checked
(`Ghostty.App.swift:73-77`), then `ghostty_app_set_focus(app, NSApp.isActive)`
(`Ghostty.App.swift:82`). Note the config passed to `app_new` is a **clone** in
Ghostty's wrapper (`Ghostty.Config.swift:45`), but that's an app-architecture detail;
passing the finalized config directly is fine for a spike.

### Surface
```c
GHOSTTY_API ghostty_surface_config_s ghostty_surface_config_new();               // ghostty.h:1101
GHOSTTY_API ghostty_surface_t ghostty_surface_new(ghostty_app_t,
                                     const ghostty_surface_config_s*);            // ghostty.h:1103
```
Ghostty: `var cfg = ghostty_surface_config_new()`, fill it (Q3), then
`ghostty_surface_new(app, &cfg)`, NULL-checked (`SurfaceView.swift:683`,
`SurfaceView_AppKit.swift:370-376`). **`ghostty_surface_new` also spawns the child
process / PTY** ("This will also initialize all the terminal IO" —
`SurfaceView_AppKit.swift:368`).

---

## Q2 — `ghostty_runtime_config_s` (the app-level callback vtable)

Definition (`ghostty.h:1019-1028`):
```c
typedef struct {
  void* userdata;
  bool  supports_selection_clipboard;
  ghostty_runtime_wakeup_cb                 wakeup_cb;                // (void*)
  ghostty_runtime_action_cb                 action_cb;                // (app, target, action) -> bool
  ghostty_runtime_read_clipboard_cb         read_clipboard_cb;        // (void*, clipboard_e, void* req) -> bool
  ghostty_runtime_confirm_read_clipboard_cb confirm_read_clipboard_cb;// (void*, const char*, void* req, request_e)
  ghostty_runtime_write_clipboard_cb        write_clipboard_cb;       // (void*, clipboard_e, content*, size_t, bool)
  ghostty_runtime_close_surface_cb          close_surface_cb;         // (void*, bool processAlive)
} ghostty_runtime_config_s;
```
Callback typedefs are `ghostty.h:1000-1017`. There are exactly **6 function-pointer
fields** plus `userdata` and `supports_selection_clipboard`.

What Ghostty passes (`Ghostty.App.swift:60-70`):
- `userdata` = `Unmanaged.passUnretained(self /*App*/).toOpaque()` — the app pointer,
  recoverable later via `ghostty_app_userdata(app)` (`ghostty.h:1091`, used at
  `Ghostty.App.swift:122-123`).
- `supports_selection_clipboard = true`.
- All 6 callbacks non-null.

Per-callback purpose and whether a minimal embedder must implement it:

| Field | Purpose | Minimum-embedder verdict |
|---|---|---|
| `wakeup_cb(userdata)` (`:1000`) | libghostty signals "I need `ghostty_app_tick` called on the main thread." Fires from **any** thread. | **MUST implement** for anything live (IO, timers). Body = dispatch `ghostty_app_tick(app)` to main thread. Ghostty: `DispatchQueue.main.async { appTick() }` (`Ghostty.App.swift:434-441`). See Q5. |
| `action_cb(app,target,action) -> bool` (`:1015`) | Big dispatch for ~70 app/surface actions (set title, new window, render, bell, mouse shape, child-exited, …) — enum `ghostty_action_tag_e` (`ghostty.h:885-952`). Return `true` = handled. | **Provide a stub returning `false`.** Ghostty's own default base stub literally `return false` (`Ghostty.App.swift:273`). Notable: `GHOSTTY_ACTION_RENDER` is **not** handled by Ghostty's macOS app — it falls through `default:` and returns false (`Ghostty.App.swift:677-679`), confirming the embedder does not drive drawing. For the spike, ignoring all actions still yields a rendering, input-taking surface. You only lose window title, bell, resize-to-cell, child-exit UI, etc. |
| `read_clipboard_cb(userdata,loc,req)->bool` (`:1001`) | Terminal/app requests clipboard contents; you later call `ghostty_surface_complete_clipboard_request` (`ghostty.h:1155`). | **Stub OK** (`return false`). Base stub returns false (`Ghostty.App.swift:274-280`). Paste won't work; rendering/typing unaffected. |
| `confirm_read_clipboard_cb(...)` (`:1004`) | Ask user to confirm an OSC-52 read. | **Stub OK** (no-op). Base stub empty (`Ghostty.App.swift:282-288`). |
| `write_clipboard_cb(...)` (`:1009`) | App wrote to clipboard (OSC-52 / copy). | **Stub OK** (no-op). Base stub empty (`Ghostty.App.swift:289-296`). Copy won't populate NSPasteboard. |
| `close_surface_cb(userdata,processAlive)` (`:1014`) | libghostty asks host to close/destroy the surface's view. | **Stub OK** for a spike (no-op). Base stub empty (`Ghostty.App.swift:297`). Without it, "exit" in the shell won't tear down your window — fine for a spike; you own the window. |

**Absolute minimum vtable to render + take input:**
`userdata` = your app pointer, `supports_selection_clipboard` = false is fine,
`wakeup_cb` = dispatch-to-main → `ghostty_app_tick`, and the other 5 as
no-op / `return false` stubs. **All six pointers must be non-null** — libghostty
calls them unconditionally; a NULL pointer will crash. Use trivial stubs, not NULL.

---

## Q3 — `ghostty_surface_config_s` and how the surface binds to the view

Definition (`ghostty.h:467-480`):
```c
typedef struct {
  ghostty_platform_e        platform_tag;      // GHOSTTY_PLATFORM_MACOS
  ghostty_platform_u        platform;          // { .macos = { void* nsview } }
  void*                     userdata;          // surface-level userdata (Ghostty passes the NSView*)
  double                    scale_factor;       // backing scale (e.g. 2.0)
  float                     font_size;          // 0 == inherit/default
  const char*               working_directory;  // cwd for the child; may be NULL
  const char*               command;            // command line to launch; NULL == default shell
  ghostty_env_var_s*        env_vars;           // extra env; may be NULL
  size_t                    env_var_count;
  const char*               initial_input;      // text piped to stdin after launch; may be NULL
  bool                      wait_after_command;
  ghostty_surface_context_e context;           // WINDOW / TAB / SPLIT
} ghostty_surface_config_s;
```
`ghostty_platform_macos_s = { void* nsview }` (`ghostty.h:448-450`);
`ghostty_env_var_s = { const char* key; const char* value }` (`ghostty.h:443-446`);
`ghostty_surface_context_e` values (`ghostty.h:461-465`), default
`GHOSTTY_SURFACE_CONTEXT_WINDOW`.

### Binding to the native view/layer (the important part)
Ghostty passes the **`NSView*` itself**, not a layer, not a device
(`SurfaceView.swift:686-690`):
```swift
config.platform_tag = GHOSTTY_PLATFORM_MACOS
config.platform = ghostty_platform_u(macos: ghostty_platform_macos_s(
    nsview: Unmanaged.passUnretained(view).toOpaque()))
config.scale_factor = NSScreen.main!.backingScaleFactor
config.userdata = Unmanaged.passUnretained(view).toOpaque()   // :684
```
The `nsview` field is a plain `void*` that is the `NSView*`. There is **no `layer`
field and no `CAMetalLayer` field** in the config. Ghostty's `SurfaceView`
(`Surface View/SurfaceView_AppKit.swift:10`, subclass of `OSSurfaceView` :
`Surface View/OSSurfaceView.swift:6`, subclass of `NSView`) **never sets
`wantsLayer`, never overrides `makeBackingLayer`, and never creates a
`CAMetalLayer`** — grep across `macos/` finds none of those. It only ever touches
`layer?.contentsScale` (`SurfaceView_AppKit.swift:863`), reading a layer libghostty
already installed.

**Conclusion (inference, clearly flagged):** libghostty, given the `NSView*`,
installs its own Metal layer on that view and drives rendering itself. The embedder
supplies a bare `NSView`; it does **not** create the Metal device, layer, or
drawable. This is the single most load-bearing inference in this doc — see the
uncertainties section. The `userdata` you set here is what `ghostty_surface_userdata`
returns (`ghostty.h:1106`; used at `Ghostty.App.swift:475-476`); Ghostty sets it to the
same NSView so surface callbacks can find the view.

The view is created with a **non-zero initial frame** on purpose so the layer bounds
are non-zero and the renderer "can do SOMETHING" (`SurfaceView_AppKit.swift:238-241`,
frame `800x600`).

### Setting the launched command (muxy needs `muxy attach <pane>`)
`command` is a single `const char*` (`ghostty.h:474`). Ghostty stores it as a Swift
`String?` and passes it through `withCString` so the pointer is valid only for the
duration of `ghostty_surface_new` (`SurfaceView.swift:642,663-664,718-719`):
```swift
return try command.withCString { cCommand in
    config.command = cCommand            // SurfaceView.swift:718-719
    ...
    return try body(&config)             // ghostty_surface_new called inside here
}
```
So: set `config.command = "muxy attach <pane>"` (a shell command string, parsed by
libghostty like a shell would) as a NUL-terminated C string that stays alive across
the `ghostty_surface_new` call. `command == NULL` → default login shell.

`working_directory` (`:473`): same `withCString` lifetime pattern (`SurfaceView.swift:715-716`).
`env_vars` / `env_var_count` (`:475-476`): array of `{key,value}` C-string pairs,
also only valid across the call (`SurfaceView.swift:729-744`). All three strings/arrays
must outlive the `ghostty_surface_new` call but may be freed after it returns.

**I did NOT find the exact parsing/quoting rules for `command`** (shell-split vs
argv-split, quoting, whether it execs a shell). See uncertainties. For a spike a
simple `muxy attach <pane>` with no shell metacharacters is the safe path.

---

## Q4 — Draw / resize / input / content-scale

### Draw
libghostty owns the render loop. Evidence:
- `ghostty_surface_draw` (`ghostty.h:1113`) is declared but **never called** anywhere
  in `macos/` (grep: 0 hits).
- No `drawRect`, no app-owned `CVDisplayLink`, no `MTKView`/`MTLCreateSystemDefaultDevice`
  in the Ghostty package. The only CVDisplayLink reference is a comment saying vsync
  uses "the CVDisplayLink" internally, tied to the display id
  (`SurfaceView_AppKit.swift:791`).
- `GHOSTTY_ACTION_RENDER` is not handled by the app (`Ghostty.App.swift:677-679`).

So the embedder never calls a draw function per frame. Once the `NSView*` is handed to
`ghostty_surface_new`, frames appear on their own. (There is
`ghostty_surface_refresh` — `ghostty.h:1112` — to request a redraw, but Ghostty's
macOS app doesn't call it in the hot path either.)

Optional but present: `ghostty_surface_set_display_id(surface, displayID)` on screen
change, for correct vsync refresh rate (`SurfaceView_AppKit.swift:793`,
`__APPLE__`-only `ghostty.h:1167`). **Skippable for a single-window spike.**

### Resize
```c
GHOSTTY_API void ghostty_surface_set_size(ghostty_surface_t, uint32_t, uint32_t);   // ghostty.h:1117
GHOSTTY_API ghostty_surface_size_s ghostty_surface_size(ghostty_surface_t);         // ghostty.h:1118
```
Ghostty converts the view size to **backing pixels** before calling — this is
emphasized as critical (`SurfaceView_AppKit.swift:459-474`):
```swift
let scaledSize = self.convertToBacking(size)   // points -> pixels
ghostty_surface_set_size(surface, UInt32(scaledSize.width), UInt32(scaledSize.height))
```
Width/height are **pixels, not points**. In Ghostty this is triggered via a
`sizeDidChange` hook driven by a frame-change observer in the SwiftUI/scroll wrappers
(`SurfaceView.swift:622`, `SurfaceScrollView.swift:215`). For a pure-AppKit spike you
call `ghostty_surface_set_size` from an `NSView.setFrameSize`/`frameDidChange` override.
`ghostty_surface_size` returns cols/rows/px/cell-size (`ghostty.h:482-489`) if you want
grid metrics.

### Content scale (HiDPI)
```c
GHOSTTY_API void ghostty_surface_set_content_scale(ghostty_surface_t, double, double); // ghostty.h:1114
```
Called from `viewDidChangeBackingProperties` (`SurfaceView_AppKit.swift:843-877`):
```swift
let fbFrame = self.convertToBacking(self.frame)
let xScale = fbFrame.size.width  / self.frame.size.width
let yScale = fbFrame.size.height / self.frame.size.height
ghostty_surface_set_content_scale(surface, xScale, yScale)   // :873
```
It also sets `layer?.contentsScale = window.backingScaleFactor` inside a
`CATransaction` with actions disabled (`:857-864`) — reading the layer libghostty owns.
On backing-property change it re-pushes size too (`:876-877`). For the spike: push
content scale once after the view has a window, and again on
`viewDidChangeBackingProperties`.

### Keyboard
```c
GHOSTTY_API bool ghostty_surface_key(ghostty_surface_t, ghostty_input_key_s);            // ghostty.h:1125
GHOSTTY_API void ghostty_surface_text(ghostty_surface_t, const char*, uintptr_t);        // ghostty.h:1129
GHOSTTY_API ghostty_input_mods_e ghostty_surface_key_translation_mods(
                                     ghostty_surface_t, ghostty_input_mods_e);           // ghostty.h:1123
GHOSTTY_API void ghostty_surface_preedit(ghostty_surface_t, const char*, uintptr_t);     // ghostty.h:1130
```
`ghostty_input_key_s` (`ghostty.h:350-358`):
```c
typedef struct {
  ghostty_input_action_e action;      // PRESS / RELEASE / REPEAT (ghostty.h:148-152)
  ghostty_input_mods_e   mods;        // bitfield (ghostty.h:127-139)
  ghostty_input_mods_e   consumed_mods;
  uint32_t               keycode;     // physical keycode
  const char*            text;        // resulting text, may be NULL/empty
  uint32_t               unshifted_codepoint;
  bool                   composing;   // true while in IME preedit
} ghostty_input_key_s;
```
Ghostty's `keyDown`/`keyUp` build this struct and call `ghostty_surface_key`
(`SurfaceView_AppKit.swift:1078,1249,1471,1474,1509`). It routes NSEvents through
`interpretKeyEvents` for IME (`:1080,1156`), and commits IME/typed text via
`ghostty_surface_text` (`Ghostty.Surface.swift:41-49`, `SurfaceView_AppKit.swift:2188`).
Mapping NSEvent → keycode/mods is substantial (`Ghostty.Input.swift`, 44KB, plus
`NSEvent+Extension.swift`); the full keymap is the messiest part to port.

**Spike shortcut:** for a first light-up you can get typed characters to the shell with
just `ghostty_surface_text(surface, utf8, len)` on committed text (that's exactly what
`sendText` does — `Ghostty.Surface.swift:41-49`). Full key handling (arrows, ctrl-C,
shortcuts, key bindings) requires building `ghostty_input_key_s` and calling
`ghostty_surface_key`. Note there is also an **app-level** `ghostty_app_key`
(`ghostty.h:1093`) used for global keybinds — skippable for the spike.

### Mouse
```c
GHOSTTY_API bool ghostty_surface_mouse_button(ghostty_surface_t, state_e, button_e, mods_e); // ghostty.h:1132
GHOSTTY_API void ghostty_surface_mouse_pos(ghostty_surface_t, double, double, mods_e);        // ghostty.h:1136
GHOSTTY_API void ghostty_surface_mouse_scroll(ghostty_surface_t, double, double, scroll_mods);// ghostty.h:1140
```
Thin wrappers in `Ghostty.Surface.swift:118-156`, driven from NSView mouse overrides
(`SurfaceView_AppKit.swift:880,974,998,1035,1568`). Mouse position is in **points**
(view coords), not backing pixels (`sendMousePos` passes raw event coords). **Fully
skippable for a "renders + keyboard" spike.**

### Focus
`ghostty_surface_set_focus(surface, bool)` from `becomeFirstResponder`/`resignFirstResponder`
(`SurfaceView_AppKit.swift:435,805-819`). Also `ghostty_surface_set_occlusion`
(`ghostty.h:1116`) for visibility — skippable.

---

## Q5 — App tick / run loop integration

```c
GHOSTTY_API void ghostty_app_tick(ghostty_app_t);        // ghostty.h:1090
GHOSTTY_API void* ghostty_app_userdata(ghostty_app_t);   // ghostty.h:1091
```
Pattern (`Ghostty.App.swift:116-118, 434-441`):
- `wakeup_cb(userdata)` is invoked by libghostty from **any thread** when it needs
  servicing.
- The embedder recovers the app object from `userdata`
  (`Unmanaged<App>.fromOpaque(userdata!)`) and does
  `DispatchQueue.main.async { ghostty_app_tick(app) }`.
- `appTick()` = `ghostty_app_tick(app)` guarded by a nil check.

So there is **no polling loop and no manual per-frame tick**. You do **not** call
`ghostty_app_tick` on a timer; you call it from the main thread **only in response to
`wakeup_cb`**. The comment at `Ghostty.App.swift:437-440` notes wakeups could be
coalesced but performance is fine as-is. Rendering is independent of the tick (Q4).

Integration with NSApplication: Ghostty just uses `NSApplicationMain` (`main.swift:33`)
— the standard AppKit run loop. `DispatchQueue.main.async` posts the tick onto that
run loop. Nothing custom. It also forwards app-active state:
`ghostty_app_set_focus(app, NSApp.isActive)` on init and via
`NSApplication.didBecomeActive/ResignActive` observers (`Ghostty.App.swift:82,90-99`).

---

## Q6 — Absolute-minimum embedding for the spike

Goal: **one surface renders a command in an NSView window and takes keyboard input.**

**Do:**
1. `ghostty_init(argc, argv)`, assert `== GHOSTTY_SUCCESS`. (once)
2. `cfg = ghostty_config_new()`; `ghostty_config_finalize(cfg)`.
3. Build `ghostty_runtime_config_s`:
   - `userdata` = pointer to your app state,
   - `supports_selection_clipboard` = false,
   - `wakeup_cb` = `{ ud in DispatchQueue.main.async { ghostty_app_tick(app) } }`,
   - `action_cb` = `{ _,_,_ in false }`,
   - `read_clipboard_cb` = `{ _,_,_ in false }`,
   - `confirm_read_clipboard_cb`, `write_clipboard_cb`, `close_surface_cb` = no-op.
   (All 6 non-null.)
4. `app = ghostty_app_new(&runtime_cfg, cfg)`; assert non-null; `ghostty_app_set_focus(app, true)`.
5. Create a plain `NSWindow` + `NSView` subclass with a **non-zero** starting frame
   (e.g. 800×600). Make it first responder / `acceptsFirstResponder = true`.
6. `var scfg = ghostty_surface_config_new()`; set
   `platform_tag = GHOSTTY_PLATFORM_MACOS`,
   `platform.macos.nsview = <NSView*>`, `userdata = <NSView*>`,
   `scale_factor = window.backingScaleFactor`,
   `command = "muxy attach <pane>"` (C string alive across the call).
   `surface = ghostty_surface_new(app, &scfg)`; assert non-null.
7. After the view is in a window: push
   `ghostty_surface_set_content_scale(surface, sx, sy)` and
   `ghostty_surface_set_size(surface, wpx, hpx)` (backing **pixels**).
   Re-push both from `viewDidChangeBackingProperties`, size from a frame-change hook.
8. Forward focus (`ghostty_surface_set_focus`) and input:
   - Easiest first light-up: `ghostty_surface_text(surface, utf8, len)` for typed text.
   - Real keys: build `ghostty_input_key_s` in `keyDown`/`keyUp` and call
     `ghostty_surface_key`.
9. `NSApplicationMain(...)`. Let libghostty render on its own.

**Skip for the spike:** config file/CLI loading, `ghostty_cli_try_action`, tabs, splits
(`ghostty_surface_split*`), fullscreen, inspector (all `ghostty_inspector_*`),
clipboard, `set_display_id`, occlusion, mouse, IME/preedit polish, the entire
`action_cb` action set (title/bell/child-exited/etc.), `ghostty_app_key` global keybinds,
and secure input.

**Do NOT skip:** `ghostty_config_finalize`; non-null values for all 6 vtable pointers;
`wakeup_cb`→main→`ghostty_app_tick`; a non-zero view frame; pushing size in **backing
pixels**; keeping the `command`/cwd/env C strings alive across `ghostty_surface_new`.

---

## Top risks / uncertainties for a from-scratch minimal embedder

1. **Who creates the Metal layer is inferred, not seen.** No Ghostty Swift code creates
   a `CAMetalLayer`, sets `wantsLayer`, or overrides `makeBackingLayer`, yet a layer
   exists (`layer?.contentsScale` at `SurfaceView_AppKit.swift:863`). I conclude
   libghostty installs the layer on the passed `NSView*`. If instead it requires a
   pre-existing layer-backed view (`wantsLayer = true`) the surface may render nothing
   until you set that. **First thing to try if the window stays black: set
   `view.wantsLayer = true` before `ghostty_surface_new`.** This is the highest-risk
   item.

2. **`command` parsing/quoting semantics are undocumented in the sources I read.**
   Ghostty passes it as one `const char*` (`SurfaceView.swift:718-719`); I did not find
   whether libghostty shell-splits, argv-splits, or execs via `/bin/sh -c`. `muxy attach
   <pane>` with no metacharacters should be safe; anything with quotes/spaces-in-args is
   a risk. Verify empirically.

3. **Struct ABI must match exactly.** The header warns these structs "must be kept in
   sync with their Zig counterparts" (`ghostty.h:62-64`). Field order, `bool` size,
   enum width, and the `ghostty_platform_u` union layout must match the linked
   `libghostty` build (pinned commit `2de5e7d38`). Hand-written FFI structs that drift
   will corrupt silently. Prefer generating from this exact header.

4. **All 6 callbacks are called unconditionally** — a NULL (vs. a stub) will crash on
   first clipboard/close/wakeup. Ensure real function pointers even for no-ops.

5. **Size units.** libghostty wants backing **pixels** for `set_size`
   (`convertToBacking`, `SurfaceView_AppKit.swift:464-465`) but mouse pos in points.
   Mixing these up gives a mis-scaled or clipped grid.

6. **Threading.** `wakeup_cb` fires on arbitrary threads; all `ghostty_*` calls in
   Ghostty are funneled to the main actor (`Ghostty.Surface.swift:26-35` frees on main;
   wakeup dispatches to main). Calling libghostty off the main thread is likely unsafe.

7. **Lifetime of surface vs app.** `ghostty_surface_free` must run (Ghostty does it on
   main via a detached task, `Ghostty.Surface.swift:26-35`); `ghostty_app_free` frees the
   app (`Ghostty.App.swift:39`). Order/thread matters if you tear down.

## What I could NOT determine from the source read

- **Whether the embedder must set `wantsLayer`/provide a layer-backed view.** Not shown;
  inferred that libghostty handles it (risk #1). The Zig side (`apprt`) that consumes
  `nsview` was not in the files I read (only the C header + Swift embedder).
- **Exact `command` string grammar** (shell vs argv, quoting) — risk #2.
- **Whether `ghostty_surface_new` can partially succeed / block on PTY spawn**, and its
  failure modes beyond returning NULL. Ghostty only NULL-checks (`SurfaceView_AppKit.swift:373`).
- **Minimum required subset of the `action_cb`** for a *good* experience (title, bell,
  child-exited): I confirmed a `false`-returning stub renders + types, but did not verify
  nothing in libghostty hard-depends on a specific action being handled.
- **Whether `supports_selection_clipboard = false` is safe** — Ghostty always passes
  `true` (`Ghostty.App.swift:62`); I assume false is fine but did not confirm.
- **The precise NSEvent → `ghostty_input_key_s` mapping** (keycodes, mods, dead keys):
  it lives in `Ghostty.Input.swift` (44KB) which I did not fully port; the struct shape
  is known (`ghostty.h:350-358`) but the fill logic is nontrivial.
- **Behavior/refresh cadence of libghostty's internal render thread**, and whether
  `ghostty_surface_refresh`/`set_occlusion` are needed to avoid a stalled first frame.
