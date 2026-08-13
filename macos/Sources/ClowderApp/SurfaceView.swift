import AppKit
import ClowderCore
import GhosttyKit

/// A bare NSView that hosts one libghostty surface. libghostty installs its own
/// Metal layer on the view and self-renders; we only create the surface (which
/// spawns `clowder attach <pane>`), push size/scale, and forward key input.
final class SurfaceView: NSView {
    private var surface: ghostty_surface_t?
    private let app: ghostty_app_t
    private let command: String
    private let socketPath: String
    private var wantsFocus = false
    /// Last size handed to libghostty, in backing pixels. `sizeDidChange` runs on every SwiftUI
    /// update, not just real resizes, so identical pushes are dropped here.
    private var pushedPixels: (UInt32, UInt32)?

    init(app: ghostty_app_t, paneId: UInt64, clowderBinary: String, socketPath: String) {
        self.app = app
        self.command = "\(clowderBinary) attach \(paneId)"
        self.socketPath = socketPath
        super.init(frame: NSRect(x: 0, y: 0, width: 800, height: 500))
        // Per the embedding study, libghostty installs its Metal layer on this
        // view — it must be layer-backed BEFORE the surface is created.
        wantsLayer = true
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override var acceptsFirstResponder: Bool { true }

    /// Called when this surface becomes first responder (e.g. the user clicks it).
    var onFocus: (() -> Void)?

    /// Runs a clowder command chosen from this pane's context menu (splits, close).
    var onCommand: ((CommandID) -> Void)?

    /// Whether Close Pane applies right now — the context menu asks before it is shown.
    var canClosePane: (() -> Bool)?

    /// Where copies land and pastes come from. A stored property so a test double could stand in.
    var pasteboard: ClowderCore.Pasteboard = SystemPasteboard()

    override func becomeFirstResponder() -> Bool {
        let ok = super.becomeFirstResponder()
        if ok { onFocus?() }
        return ok
    }

    /// Tie libghostty surface focus to whether this pane is the focused split leaf.
    func setFocused(_ focused: Bool) {
        wantsFocus = focused
        if let surface { ghostty_surface_set_focus(surface, focused) }
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        guard window != nil else { return }
        guard surface == nil else {
            // A cached surface coming back on screen (SurfaceHost keeps one view per pane, and only
            // the selected pane is in the hierarchy). Its grid is whatever it was when it was last
            // visible, so a window resize that happened meanwhile has to be applied now.
            pushSize()
            return
        }
        createSurface()
    }

    private func createSurface() {
        var config = ghostty_surface_config_new()
        config.platform_tag = GHOSTTY_PLATFORM_MACOS
        config.platform.macos.nsview = Unmanaged.passUnretained(self).toOpaque()
        config.userdata = Unmanaged.passUnretained(self).toOpaque()
        config.scale_factor = Double(window?.backingScaleFactor ?? 2.0)

        // The command string must stay alive across ghostty_surface_new; env passes
        // the daemon socket so `clowder attach` finds it.
        command.withCString { cmd in
            "CLOWDER_SOCK".withCString { envKey in
                socketPath.withCString { envVal in
                    var env = ghostty_env_var_s(key: envKey, value: envVal)
                    withUnsafeMutablePointer(to: &env) { envPtr in
                        config.command = cmd
                        config.env_vars = envPtr
                        config.env_var_count = 1
                        surface = ghostty_surface_new(app, &config)
                    }
                }
            }
        }

        guard let surface else {
            NSLog("clowder: ghostty_surface_new returned null")
            return
        }
        ghostty_surface_set_content_scale(surface, config.scale_factor, config.scale_factor)
        pushSize()
        ghostty_surface_set_focus(surface, wantsFocus)
    }

    /// Hand libghostty a new drawable size, in **backing pixels** — points would silently give a
    /// half-size grid on Retina. Driven by SwiftUI through `TerminalContainer.updateNSView`, which
    /// is the layout path that actually fires here; `setFrameSize` alone does not, which is what
    /// left resized windows rendering at the old grid (#87).
    func sizeDidChange(_ size: CGSize) {
        guard let surface else { return }
        let backing = convertToBacking(NSRect(origin: .zero, size: size))
        let w = UInt32(max(1, backing.width))
        let h = UInt32(max(1, backing.height))
        guard pushedPixels ?? (0, 0) != (w, h) else { return }
        pushedPixels = (w, h)
        ghostty_surface_set_size(surface, w, h)
    }

    private func pushSize() { sizeDidChange(bounds.size) }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        sizeDidChange(newSize)
    }

    /// The window moved to a display with a different scale factor (or gained one). libghostty needs
    /// both halves re-pushed: the scale, and then the size — the pixel count changed even though the
    /// point size did not. Mirrors what Ghostty's own AppKit view does.
    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        guard let surface, let window else { return }

        // The layer is libghostty's; retag it without an implicit animation.
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        layer?.contentsScale = window.backingScaleFactor
        CATransaction.commit()

        let backing = convertToBacking(frame)
        let xScale = frame.width > 0 ? backing.width / frame.width : window.backingScaleFactor
        let yScale = frame.height > 0 ? backing.height / frame.height : window.backingScaleFactor
        ghostty_surface_set_content_scale(surface, xScale, yScale)

        pushedPixels = nil   // same points, different pixels — the dedupe must not swallow this
        pushSize()
    }

    // Accumulates text the IME commits during interpretKeyEvents/handleEvent.
    private var keyTextAccumulator: [String]?
    private var markedText = ""

    override func keyDown(with event: NSEvent) {
        keyTextAccumulator = []
        let handledByIME = inputContext?.handleEvent(event) ?? false
        let commits = keyTextAccumulator ?? []
        keyTextAccumulator = nil

        // IME committed text -> send it as text and stop.
        if !commits.isEmpty {
            for t in commits { sendText(t) }
            return
        }
        // Still composing: setMarkedText already pushed preedit. Stop.
        if handledByIME && hasMarkedText() { return }
        // Not consumed by IME: encode normally (Enter, Ctrl-*, arrows, plain char).
        sendKey(event, GHOSTTY_ACTION_PRESS)
    }

    override func keyUp(with event: NSEvent) { sendKey(event, GHOSTTY_ACTION_RELEASE) }

    private func sendText(_ text: String) {
        guard let surface, !text.isEmpty else { return }
        text.withCString { ghostty_surface_text(surface, $0, UInt(strlen($0))) }
    }

    private func asString(_ any: Any) -> String? {
        if let s = any as? String { return s }
        if let a = any as? NSAttributedString { return a.string }
        return nil
    }

    /// Forward a key via `ghostty_surface_key` (libghostty encodes Enter/Backspace/
    /// Ctrl-* itself from the keycode/mods; `text` is set only for printable input).
    private func sendKey(_ event: NSEvent, _ action: ghostty_input_action_e) {
        guard let surface else { return }
        var key = ghostty_input_key_s()
        key.action = action
        key.keycode = UInt32(event.keyCode)
        key.mods = ghosttyMods(event.modifierFlags)
        key.consumed_mods = ghosttyMods(event.modifierFlags.subtracting([.control, .command]))
        key.composing = false
        key.unshifted_codepoint =
            event.charactersIgnoringModifiers?.unicodeScalars.first?.value ?? 0

        let text = event.characters ?? ""
        if let first = text.utf8.first, first >= 0x20 {
            text.withCString { ptr in
                key.text = ptr
                _ = ghostty_surface_key(surface, key)
            }
        } else {
            _ = ghostty_surface_key(surface, key)
        }
    }

    // MARK: - Mouse

    private func mousePoint(_ event: NSEvent) -> (Double, Double) {
        let p = convert(event.locationInWindow, from: nil)
        // libghostty wants a top-left origin; AppKit's is bottom-left.
        return (Double(p.x), Double(bounds.height - p.y))
    }

    private func sendMousePos(_ event: NSEvent) {
        guard let surface else { return }
        let (x, y) = mousePoint(event)
        ghostty_surface_mouse_pos(surface, x, y, ghosttyMods(event.modifierFlags))
    }

    private func sendMouseButton(_ event: NSEvent,
                                 _ state: ghostty_input_mouse_state_e,
                                 _ button: ghostty_input_mouse_button_e) {
        guard let surface else { return }
        sendMousePos(event)
        _ = ghostty_surface_mouse_button(surface, state, button, ghosttyMods(event.modifierFlags))
    }

    override func mouseDown(with e: NSEvent)  { sendMouseButton(e, GHOSTTY_MOUSE_PRESS,   GHOSTTY_MOUSE_LEFT) }
    override func mouseUp(with e: NSEvent)    { sendMouseButton(e, GHOSTTY_MOUSE_RELEASE, GHOSTTY_MOUSE_LEFT) }
    override func mouseDragged(with e: NSEvent) { sendMousePos(e) }
    /// libghostty's `right-click-action` defaults to `context-menu`: on a right-press it selects
    /// the word (or link) under the cursor and deliberately reports the event as *not* consumed,
    /// which is its way of asking the host to show a menu. It does consume the event when a
    /// mouse-reporting program like vim owns the mouse — so honouring the return value suppresses
    /// the menu in exactly the cases where it would be wrong.
    override func rightMouseDown(with e: NSEvent) {
        guard let surface else { return }
        sendMousePos(e)
        let consumed = ghostty_surface_mouse_button(surface, GHOSTTY_MOUSE_PRESS, GHOSTTY_MOUSE_RIGHT,
                                                   ghosttyMods(e.modifierFlags))
        if !consumed { showContextMenu(for: e) }
    }
    override func rightMouseUp(with e: NSEvent)    { sendMouseButton(e, GHOSTTY_MOUSE_RELEASE, GHOSTTY_MOUSE_RIGHT) }
    override func rightMouseDragged(with e: NSEvent) { sendMousePos(e) }
    override func otherMouseDown(with e: NSEvent)  { sendMouseButton(e, GHOSTTY_MOUSE_PRESS,   GHOSTTY_MOUSE_MIDDLE) }
    override func otherMouseUp(with e: NSEvent)    { sendMouseButton(e, GHOSTTY_MOUSE_RELEASE, GHOSTTY_MOUSE_MIDDLE) }
    override func otherMouseDragged(with e: NSEvent) { sendMousePos(e) }

    override func scrollWheel(with e: NSEvent) {
        guard let surface else { return }
        ghostty_surface_mouse_scroll(surface, Double(e.scrollingDeltaX), Double(e.scrollingDeltaY), 0)
    }

    private func ghosttyMods(_ flags: NSEvent.ModifierFlags) -> ghostty_input_mods_e {
        var raw: UInt32 = 0
        if flags.contains(.shift) { raw |= GHOSTTY_MODS_SHIFT.rawValue }
        if flags.contains(.control) { raw |= GHOSTTY_MODS_CTRL.rawValue }
        if flags.contains(.option) { raw |= GHOSTTY_MODS_ALT.rawValue }
        if flags.contains(.command) { raw |= GHOSTTY_MODS_SUPER.rawValue }
        return ghostty_input_mods_e(raw)
    }

    deinit {
        if let surface { ghostty_surface_free(surface) }
    }
}

// MARK: - Clipboard

extension SurfaceView {
    private var hasSelection: Bool {
        guard let surface else { return false }
        return ghostty_surface_has_selection(surface)
    }

    /// Run one of libghostty's own keybinding actions by name.
    @discardableResult
    private func perform(_ action: String) -> Bool {
        guard let surface else { return false }
        return action.withCString { ghostty_surface_binding_action(surface, $0, UInt(strlen($0))) }
    }

    // MARK: Runtime callbacks

    /// libghostty copied something. Both clipboard kinds land on the general pasteboard: macOS has
    /// no primary selection, so a SELECTION write (which is what copy-on-select produces) has
    /// nowhere else sensible to go.
    func writeClipboard(_ flavours: [ClipboardContent], confirm: Bool) {
        guard let plain = TerminalClipboard.plainText(from: flavours) else { return }
        let html = TerminalClipboard.html(from: flavours)
        guard confirm else {
            pasteboard.write(plain: plain, html: html)
            return
        }
        // A program in the pane asked to replace the clipboard (OSC 52 with clipboard-write = ask).
        // Nothing to complete here — this request arrives through the write callback, not as a
        // clipboard request, so declining simply means not writing.
        ask(.osc52Write, text: plain) { [weak self] confirmed in
            guard confirmed, let self else { return }
            self.pasteboard.write(plain: plain, html: html)
        }
    }

    /// libghostty wants clipboard contents. Returning false tells it to release the request state,
    /// which is why an empty pasteboard must not be answered with an empty string.
    ///
    /// `kind` is ignored on purpose: a SELECTION read (middle-click paste) should come from the
    /// same place a SELECTION write went, and on macOS that is the general pasteboard.
    func readClipboard(kind: ghostty_clipboard_e, state: UnsafeMutableRawPointer) -> Bool {
        guard let surface, let text = pasteboard.string() else { return false }
        text.withCString { ghostty_surface_complete_clipboard_request(surface, $0, state, false) }
        return true
    }

    /// libghostty judged the request unsafe (a multi-line paste outside bracketed-paste mode) or
    /// unauthorised (OSC 52 read with `clipboard-read = ask`) and is asking the user.
    ///
    /// The `state` pointer is heap-allocated by libghostty and released *only* by
    /// `complete_clipboard_request`, so both answers complete it — exactly once. The one path that
    /// does not is a surface freed while the alert was up, where completing would be a
    /// use-after-free; that leaks one request rather than crashing.
    func confirmReadClipboard(text: String,
                              state: UnsafeMutableRawPointer,
                              request: ghostty_clipboard_request_e) {
        ask(PasteRequestKind(request), text: text) { [weak self] confirmed in
            guard let self, let surface = self.surface else { return }
            // Declining completes with empty text but still says `confirmed` — an OSC 52 read
            // completed with `confirmed: false` raises UnauthorizedPaste again and re-enters this
            // callback, which would loop the prompt forever. Empty text is what makes that safe:
            // a paste of nothing returns early, and an OSC 52 read replies with an empty
            // clipboard, which is the denial the program should see.
            let reply = confirmed ? text : ""
            reply.withCString {
                ghostty_surface_complete_clipboard_request(surface, $0, state, true)
            }
        }
    }

    /// Put a confirmation in front of the user and report what they chose.
    ///
    /// Always deferred to the next main-queue turn: these callbacks arrive from inside libghostty,
    /// and running a modal loop there would re-enter it.
    private func ask(_ kind: PasteRequestKind, text: String, then finish: @escaping (Bool) -> Void) {
        let content = PasteConfirmation.alert(kind: kind, text: text)
        DispatchQueue.main.async { [weak self] in
            let alert = NSAlert()
            alert.alertStyle = .warning
            alert.messageText = content.title
            alert.informativeText = content.message
            alert.addButton(withTitle: content.confirmTitle)
            alert.addButton(withTitle: "Cancel")
            if let window = self?.window {
                alert.beginSheetModal(for: window) { finish($0 == .alertFirstButtonReturn) }
            } else {
                finish(alert.runModal() == .alertFirstButtonReturn)
            }
        }
    }
}

// MARK: - Menus

extension SurfaceView: NSMenuItemValidation {
    // AppKit's stock Edit menu already carries ⌘C/⌘V/⌘A and targets the first responder, which is
    // the focused pane. Implementing these selectors is all it takes to light it up — and *not*
    // implementing `cut:` is what correctly leaves Cut greyed out, since a terminal's scrollback
    // is not an editable buffer. AppKit never validates an item whose action no responder
    // implements, so Cut and Undo stay disabled without us saying anything.
    @objc func copy(_ sender: Any?) { perform("copy_to_clipboard") }
    @objc func paste(_ sender: Any?) { perform("paste_from_clipboard") }
    // NSResponder already declares selectAll(_:), so this one overrides rather than adds.
    @objc override func selectAll(_ sender: Any?) { perform("select_all") }

    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        switch item.action {
        case #selector(copy(_:)): return hasSelection
        case #selector(paste(_:)): return pasteboard.string() != nil
        default: return true
        }
    }

    fileprivate func showContextMenu(for event: NSEvent) {
        let menu = NSMenu()
        menu.autoenablesItems = false
        for entry in TerminalMenu.contextMenu(hasSelection: hasSelection,
                                              pasteboardHasText: pasteboard.string() != nil,
                                              canClosePane: canClosePane?() ?? false) {
            guard let entry else {
                menu.addItem(.separator())
                continue
            }
            let item = NSMenuItem(title: entry.title, action: nil, keyEquivalent: "")
            item.isEnabled = entry.isEnabled
            if entry.isEnabled {
                item.target = self
                item.action = selector(for: entry.action)
                item.representedObject = MenuCommand(entry.action)
            }
            menu.addItem(item)
        }
        NSMenu.popUpContextMenu(menu, with: event, for: self)
    }

    private func selector(for action: TerminalMenuAction) -> Selector {
        switch action {
        case .copy: return #selector(copy(_:))
        case .paste: return #selector(paste(_:))
        case .selectAll: return #selector(selectAll(_:))
        case .command: return #selector(runMenuCommand(_:))
        }
    }

    @objc private func runMenuCommand(_ sender: NSMenuItem) {
        guard let wrapper = sender.representedObject as? MenuCommand,
              case let .command(id) = wrapper.action else { return }
        onCommand?(id)
    }
}

/// Boxes a `TerminalMenuAction` so it can ride along in `NSMenuItem.representedObject`.
private final class MenuCommand: NSObject {
    let action: TerminalMenuAction
    init(_ action: TerminalMenuAction) { self.action = action }
}

extension SurfaceView: NSTextInputClient {
    func insertText(_ string: Any, replacementRange: NSRange) {
        guard let s = asString(string) else { return }
        if keyTextAccumulator != nil {
            keyTextAccumulator?.append(s)      // committed during keyDown
        } else {
            sendText(s)
        }
        markedText = ""
        if let surface { ghostty_surface_preedit(surface, nil, 0) }  // clear preedit
    }

    func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        markedText = asString(string) ?? ""
        guard let surface else { return }
        if markedText.isEmpty {
            ghostty_surface_preedit(surface, nil, 0)
        } else {
            markedText.withCString { ghostty_surface_preedit(surface, $0, UInt(strlen($0))) }
        }
    }

    func unmarkText() {
        markedText = ""
        if let surface { ghostty_surface_preedit(surface, nil, 0) }
    }

    func hasMarkedText() -> Bool { !markedText.isEmpty }

    func markedRange() -> NSRange {
        markedText.isEmpty ? NSRange(location: NSNotFound, length: 0)
                           : NSRange(location: 0, length: markedText.utf16.count)
    }

    func selectedRange() -> NSRange { NSRange(location: NSNotFound, length: 0) }

    func attributedSubstring(forProposedRange range: NSRange,
                             actualRange: NSRangePointer?) -> NSAttributedString? { nil }

    func validAttributesForMarkedText() -> [NSAttributedString.Key] { [] }

    func characterIndex(for point: NSPoint) -> Int { 0 }

    func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        guard let surface, let window else { return .zero }
        var x = 0.0, y = 0.0, w = 0.0, h = 0.0
        ghostty_surface_ime_point(surface, &x, &y, &w, &h)   // top-left origin, points
        let local = NSRect(x: x, y: bounds.height - y, width: max(w, 1), height: max(h, 1))
        let inWindow = convert(local, to: nil)
        return window.convertToScreen(inWindow)
    }

    override func doCommand(by selector: Selector) {
        // Intentionally empty: keyDown's fallback path encodes command keys
        // (Enter, Backspace, arrows) via ghostty_surface_key.
    }
}
