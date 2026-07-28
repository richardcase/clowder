import AppKit
import GhosttyKit

/// A bare NSView that hosts one libghostty surface. libghostty installs its own
/// Metal layer on the view and self-renders; we only create the surface (which
/// spawns `muxy attach <pane>`), push size/scale, and forward key input.
final class SurfaceView: NSView {
    private var surface: ghostty_surface_t?
    private let app: ghostty_app_t
    private let command: String
    private let socketPath: String

    init(app: ghostty_app_t, paneId: UInt64, muxyBinary: String, socketPath: String) {
        self.app = app
        self.command = "\(muxyBinary) attach \(paneId)"
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

    override func becomeFirstResponder() -> Bool {
        let ok = super.becomeFirstResponder()
        if ok { onFocus?() }
        return ok
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        guard window != nil, surface == nil else { return }
        createSurface()
    }

    private func createSurface() {
        var config = ghostty_surface_config_new()
        config.platform_tag = GHOSTTY_PLATFORM_MACOS
        config.platform.macos.nsview = Unmanaged.passUnretained(self).toOpaque()
        config.userdata = Unmanaged.passUnretained(self).toOpaque()
        config.scale_factor = Double(window?.backingScaleFactor ?? 2.0)

        // The command string must stay alive across ghostty_surface_new; env passes
        // the daemon socket so `muxy attach` finds it.
        command.withCString { cmd in
            "MUXY_SOCK".withCString { envKey in
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
            NSLog("muxy: ghostty_surface_new returned null")
            return
        }
        ghostty_surface_set_content_scale(surface, config.scale_factor, config.scale_factor)
        pushSize()
        ghostty_surface_set_focus(surface, true)
    }

    private func pushSize() {
        guard let surface else { return }
        let backing = convertToBacking(bounds)
        let w = UInt32(max(1, backing.width))
        let h = UInt32(max(1, backing.height))
        ghostty_surface_set_size(surface, w, h)
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
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
    override func rightMouseDown(with e: NSEvent)  { sendMouseButton(e, GHOSTTY_MOUSE_PRESS,   GHOSTTY_MOUSE_RIGHT) }
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
