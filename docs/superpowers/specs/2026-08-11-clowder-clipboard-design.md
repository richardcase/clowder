# Clipboard support for terminal panes

*2026-08-11 — closes [#81](https://github.com/richardcase/clowder/issues/81)*

## The report

> Currently the terminal windows (and probably the agent windows) do not support cut and paste.
>
> We want to be able to cut text from the windows and also paste test to the windows.
>
> This should be possible via standard shortcut keys and also via the context menu.
>
> Selecting text via the mouse should also automatically copy it to the clipboard

"Terminal windows" and "agent windows" are the same object in clowder — every pane in the detail
region is a `SurfaceView` running `clowder attach <pane>`, whether it hosts an agent or a project
shell. Fixing one fixes both.

## Root cause

libghostty does not own a clipboard. It delegates every clipboard operation to the embedding
application through function pointers in `ghostty_runtime_config_s`, and clowder passes stubs
(`App.swift:78-88`):

```swift
runtime.supports_selection_clipboard = false
runtime.action_cb = { _, _, _ in false }
runtime.read_clipboard_cb = { _, _, _ in false }
runtime.confirm_read_clipboard_cb = { _, _, _, _ in }
runtime.write_clipboard_cb = { _, _, _, _, _ in }
```

There is no `NSPasteboard` reference anywhere in `macos/`. This was a deliberate deferral, recorded
in `docs/superpowers/research/2026-07-26-libghostty-macos-embedding.md`, which already predicted the
exact symptoms: *"`read_clipboard_cb` … Stub OK (`return false`). Paste won't work"* and
*"`write_clipboard_cb` … Copy won't populate NSPasteboard."*

Everything else is already in place. libghostty tracks mouse selection internally — `mouseDragged`
forwards positions at `SurfaceView.swift:176` and that is what drives it — and it ships
`copy_to_clipboard` / `paste_from_clipboard` / `select_all` binding actions. The data has nowhere to
go, and no data comes back.

## Four findings from the pinned libghostty

Verified against the committed `macos/Sources/GhosttyKit/include/ghostty.h` (byte-identical to
upstream at `GHOSTTY_PIN=2de5e7d38e1354759211722a8687c0815d2cf02c`) and that commit's
`src/apprt/embedded.zig`, `src/Surface.zig`, `src/config/Config.zig`, `src/input/Binding.zig`.

**The clipboard callbacks receive the *surface's* userdata, not the app's.** `embedded.zig:752`
calls `self.app.opts.write_clipboard(self.userdata, …)` where `self` is the Surface, and
`clipboardRequest` does the same for reads. `SurfaceView.swift:53` already sets
`config.userdata = Unmanaged.passUnretained(self).toOpaque()`. So each C callback can recover its
own `SurfaceView` with `Unmanaged.fromOpaque(...).takeUnretainedValue()` — no app-level registry,
and `runtime.userdata` can stay `nil`.

**Right-click already asks the host for a context menu.** `right-click-action` defaults to
`context-menu` (`Config.zig`), and on a right-press `Surface.zig:4073-4099` word-selects (or
link-selects) under the cursor and then deliberately returns `false`, commented *"Don't consume so
that we show the context menu in apprt."* So the hook is the return value of
`ghostty_surface_mouse_button`, which `SurfaceView.swift:177` currently discards. This also gives
correct behaviour under mouse-reporting programs for free: when vim owns the mouse the event is
consumed (`true`) and no menu should appear.

**`copy-on-select` is already on; only the transport is missing.** `Config.zig:2434` defaults it to
`.true` on macOS, which writes the selection to the *selection* clipboard. `embedded.zig:669` gates
that on `supports_selection_clipboard`, which clowder sets to `false` — so the write is dropped.
Setting the flag and routing `GHOSTTY_CLIPBOARD_SELECTION` to `NSPasteboard.general` satisfies the
issue's auto-copy requirement with no polling, no timer, and no `GHOSTTY_ACTION_SELECTION_CHANGED`
handling. macOS has no primary selection, so collapsing both clipboard kinds onto the general
pasteboard is the correct mapping rather than a shortcut. Middle-click paste-from-selection starts
working as a side effect.

**Paste protection makes the confirm callback mandatory, not optional.**
`clipboard-paste-protection` defaults to `true`. When libghostty judges a paste unsafe — multi-line
text while the running program has *not* enabled bracketed paste — `completeClipboardRequest` fails
with `error.UnsafePaste` and calls `confirm_read_clipboard` instead of pasting
(`embedded.zig:699-720`). A no-op there means the paste is silently swallowed *and* its heap-allocated
request state is leaked. Implementing `read_clipboard_cb` alone would ship a paste that works at a
`claude` prompt (bracketed paste on) and mysteriously fails at a bare `zsh` prompt. The two must land
together.

## Decisions

| Question | Decision | Why |
|---|---|---|
| What is "cut"? | **Copy only.** No Cut item; Edit > Cut stays greyed. | Terminal scrollback is not an editable buffer and libghostty has no cut action. Terminal.app, iTerm2 and Ghostty all do this. |
| Unsafe paste | **Confirmation alert**, honouring libghostty's judgement. | These panes drive coding agents; a pasted newline can auto-execute. Also the only way the paste path is correct at all (see above). |
| OSC 52 | **Ghostty defaults** — writes allowed silently, reads prompt. | Keeps `tmux`/remote copy working while a program in a pane cannot read the clipboard unseen. |
| Copy-on-select | **Always on**, no preference. | It is what the issue asks for, and it is one flag plus a callback. A setting would add persistence and Settings-window surface for no decision the user has expressed. |
| Context menu | Copy, Paste, Select All, ─, Split Right, Split Down, Close Pane. | Mirrors Ghostty's own context menu, and the split/close items reuse existing `CommandID`s. |

## The design

Two layers, along the seam the repo already uses: `clowder-app` has no test target, so decisions
live in `ClowderCore` and the app target only marshals C types and drives AppKit.

### ClowderCore — pure and unit-tested

`TerminalClipboard.swift`
: A `Pasteboard` protocol (`string()`, `write(plain:html:)`) so ClowderCore stays AppKit-free and
  fakeable, a `ClipboardContent { mime, data }` mirror of `ghostty_clipboard_content_s`, and the
  mime-selection logic. `write_clipboard_cb` hands over an *array* of mime/data pairs — the default
  `copy_to_clipboard:mixed` mode emits `text/plain` and `text/html` — so something must choose.
  `plainText(from:)` prefers a `text/plain` mime (tolerating `text/plain;charset=utf-8`), falls back
  to the sole entry, and rejects empty. `html(from:)` carries the rich flavour when present.

`PasteConfirmation.swift`
: `PasteRequestKind { paste, osc52Read, osc52Write }` mirroring `ghostty_clipboard_request_e`, and a
  pure `alert(kind:text:)` returning title/message/confirm-button text. An unsafe paste and "a
  program in this pane wants to read your clipboard" are different warnings and must read
  differently. Handles preview truncation and line counting. The app target only feeds the result to
  `NSAlert`.

`TerminalMenu.swift`
: `contextMenu(hasSelection:pasteboardHasText:canClosePane:) -> [TerminalMenuItem?]` (`nil` =
  separator), with `TerminalMenuAction { copy, paste, selectAll, command(CommandID) }`. The
  split/close items reuse `CommandID.splitRight` / `.splitDown` / `.closePane` from `Keymap.swift`
  so titles and behaviour cannot drift from the clowder menu and the ⌘K palette.

### ClowderApp — marshalling only

`App.swift`
: `supports_selection_clipboard = true`, and the three clipboard stubs become capture-free C
  function pointers that recover the `SurfaceView` from `userdata` and call instance methods.
  `action_cb` is left alone: menu validation queries `ghostty_surface_has_selection` when the menu
  opens, so `SELECTION_CHANGED` is not needed.

`SurfaceView.swift`
: The callback bodies, and the AppKit surface:

  - `writeClipboard` → `TerminalClipboard` → pasteboard. Both `STANDARD` and `SELECTION` land on
    `NSPasteboard.general`.
  - `readClipboard` → pasteboard string → `ghostty_surface_complete_clipboard_request(…,
    confirmed: false)`. Returns `false` when there is nothing to paste, so libghostty frees the
    request rather than leaking it.
  - `confirmReadClipboard` → `PasteConfirmation` → `NSAlert` → complete with `confirmed: true`, or
    with an empty string on cancel.
  - **Lifetime rule:** the `state` pointer is heap-allocated by libghostty and released *only* by
    `complete_clipboard_request`. Every path — cancel, empty clipboard, surface already freed — must
    call it exactly once.
  - `@objc copy(_:)`, `paste(_:)`, `selectAll(_:)` calling `ghostty_surface_binding_action` with the
    action names verified in `Binding.zig` (`copy_to_clipboard` 373, `paste_from_clipboard` 376,
    `select_all` 439), plus `validateMenuItem` gating Copy on `ghostty_surface_has_selection`.

    This uses AppKit's **stock** Edit menu rather than a new SwiftUI `CommandGroup`: it already
    carries ⌘C/⌘V/⌘A and targets the first responder, which is the focused `SurfaceView`
    (`TerminalContainer.swift:22-24`). Cut and Undo stay correctly greyed precisely because we do
    not implement them. When Copy is disabled AppKit does not consume ⌘C, so it falls through to
    `keyDown` and libghostty exactly as today.
  - `rightMouseDown` presents an `NSMenu` built from `TerminalMenu` when
    `ghostty_surface_mouse_button` returns `false`.
  - `onCommand: ((CommandID) -> Void)?`, mirroring the existing `onFocus` closure, for the
    split/close items.

`SurfaceHost.swift`
: A `runCommand` closure, assigned to the view on every `view(for:)` call so it is order-independent
  and survives `retarget`. `App.swift` wires it to `AppModel.run`. This keeps the closure out of
  `ContentView` → `SplitContainer` → `TerminalContainer`, none of which otherwise care.

`AppModel` is untouched and stays libghostty-free.

## Scope

No Rust changes. The `clowder attach` CLI client draws inside whatever terminal the user already
runs, so copy and paste there belong to that terminal, not to clowder.

## Verification

Unit tests in `macos/Tests/ClowderCoreTests/` cover the ClowderCore layer: mime preference and
fallback, the empty cases, per-kind alert wording, preview truncation with a true line count, and
context-menu enablement and separator order.

The libghostty wiring cannot be unit-tested — it needs a live surface — so it is verified by hand
against a built `dist/Clowder.app`. The load-bearing checks are the ones that would pass under a
half-wired implementation: a multi-line paste at a bare `zsh` prompt must show the confirmation
alert *and* the same text must paste silently inside Claude Code (bracketed paste on), and
right-click must show a menu at a shell prompt but **not** inside `vim -c 'set mouse=a'`.
