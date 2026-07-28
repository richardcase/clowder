import SwiftUI
import AppKit

/// Bridges a retained per-pane SurfaceView into SwiftUI. Keyed by pane at the call
/// site with `.id(pane)`, so selecting a different agent makes a different view.
struct TerminalContainer: NSViewRepresentable {
    let pane: UInt64
    let surfaceHost: SurfaceHost

    func makeNSView(context: Context) -> SurfaceView {
        let view = surfaceHost.view(for: pane)
        DispatchQueue.main.async { view.window?.makeFirstResponder(view) }
        return view
    }

    func updateNSView(_ nsView: SurfaceView, context: Context) {}
}
