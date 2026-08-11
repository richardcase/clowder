import AppKit
import ClowderCore
import GhosttyKit

/// `ClowderCore.Pasteboard` backed by the real system pasteboard.
/// The protocol is module-qualified throughout: AppKit exports a `Pasteboard` of its own.
struct SystemPasteboard: ClowderCore.Pasteboard {
    let pasteboard: NSPasteboard

    init(_ pasteboard: NSPasteboard = .general) { self.pasteboard = pasteboard }

    func string() -> String? {
        guard let s = pasteboard.string(forType: .string), !s.isEmpty else { return nil }
        return s
    }

    func write(plain: String, html: String?) {
        pasteboard.clearContents()
        pasteboard.setString(plain, forType: .string)
        if let html { pasteboard.setString(html, forType: .html) }
    }
}

// MARK: - libghostty runtime clipboard callbacks

/// libghostty owns no clipboard: it calls back into the embedding app for every read and write.
/// The callbacks are C function pointers and so cannot capture, but each one is handed the
/// *surface's* userdata — which `SurfaceView.createSurface` already sets to the view itself — so
/// they recover their `SurfaceView` and forward to it.
///
/// `embedded.zig` passes `self.userdata` from the Surface, not the App, which is what makes this
/// work without an app-level surface registry.
enum GhosttyClipboard {
    static func install(_ runtime: inout ghostty_runtime_config_s) {
        // macOS has no primary selection, so we claim support and land SELECTION writes on the
        // general pasteboard. That is what turns on libghostty's own copy-on-select (default true
        // on macOS): without this flag it computes the selection and then drops the write.
        runtime.supports_selection_clipboard = true

        runtime.write_clipboard_cb = { userdata, _, contents, count, confirm in
            guard let view = surfaceView(userdata) else { return }
            var flavours: [ClipboardContent] = []
            flavours.reserveCapacity(Int(count))
            for i in 0..<Int(count) {
                guard let entry = contents?[i] else { continue }
                flavours.append(ClipboardContent(mime: entry.mime.map(String.init(cString:)) ?? "",
                                                 data: entry.data.map(String.init(cString:)) ?? ""))
            }
            view.writeClipboard(flavours, confirm: confirm)
        }

        runtime.read_clipboard_cb = { userdata, kind, state in
            guard let view = surfaceView(userdata), let state else { return false }
            return view.readClipboard(kind: kind, state: state)
        }

        runtime.confirm_read_clipboard_cb = { userdata, text, state, request in
            guard let view = surfaceView(userdata), let state else { return }
            view.confirmReadClipboard(text: text.map(String.init(cString:)) ?? "",
                                      state: state,
                                      request: request)
        }
    }
}

/// A free function, not a static member: the callbacks above are C function pointers, and even a
/// metatype reference would count as captured context.
private func surfaceView(_ userdata: UnsafeMutableRawPointer?) -> SurfaceView? {
    guard let userdata else { return nil }
    return Unmanaged<SurfaceView>.fromOpaque(userdata).takeUnretainedValue()
}

extension PasteRequestKind {
    init(_ request: ghostty_clipboard_request_e) {
        switch request {
        case GHOSTTY_CLIPBOARD_REQUEST_OSC_52_READ: self = .osc52Read
        case GHOSTTY_CLIPBOARD_REQUEST_OSC_52_WRITE: self = .osc52Write
        default: self = .paste
        }
    }
}
