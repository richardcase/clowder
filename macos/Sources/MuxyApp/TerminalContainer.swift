import SwiftUI
import AppKit

/// Bridges a retained per-pane SurfaceView into SwiftUI. Keyed by pane at the call
/// site with `.id(pane)`, so selecting a different agent makes a different view.
struct TerminalContainer: NSViewRepresentable {
    let pane: UInt64
    let surfaceHost: SurfaceHost
    var isFocused: Bool = false
    var onFocus: (() -> Void)? = nil

    func makeNSView(context: Context) -> SurfaceView {
        let view = surfaceHost.view(for: pane)
        view.onFocus = onFocus
        return view
    }

    func updateNSView(_ nsView: SurfaceView, context: Context) {
        nsView.onFocus = onFocus
        // Only the focused leaf claims first responder (native click-focus handles the rest).
        if isFocused, nsView.window?.firstResponder !== nsView {
            DispatchQueue.main.async { nsView.window?.makeFirstResponder(nsView) }
        }
    }
}
