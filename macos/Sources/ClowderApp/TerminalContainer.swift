import SwiftUI
import AppKit

/// Bridges a retained per-pane SurfaceView into SwiftUI. Keyed by pane at the call
/// site with `.id(pane)`, so selecting a different agent makes a different view.
///
/// `size` is the measured size of the space SwiftUI gave the leaf, passed in from the caller's
/// GeometryReader. It is a stored property on purpose: that is what makes a window resize an
/// *update* to this representable, and `updateNSView` the hook that reaches libghostty. An
/// `NSView.setFrameSize` override is not enough under SwiftUI hosting — relying on it alone is
/// what left resized windows rendering at their old grid (#87).
struct TerminalContainer: NSViewRepresentable {
    let pane: UInt64
    let surfaceHost: SurfaceHost
    let size: CGSize
    var isFocused: Bool = false
    var onFocus: (() -> Void)? = nil

    func makeNSView(context: Context) -> SurfaceView {
        let view = surfaceHost.view(for: pane)
        view.onFocus = onFocus
        return view
    }

    func updateNSView(_ nsView: SurfaceView, context: Context) {
        nsView.onFocus = onFocus
        nsView.sizeDidChange(size)
        nsView.setFocused(isFocused)
        // Only the focused leaf claims first responder (native click-focus handles the rest).
        if isFocused, nsView.window?.firstResponder !== nsView {
            DispatchQueue.main.async { nsView.window?.makeFirstResponder(nsView) }
        }
    }
}
