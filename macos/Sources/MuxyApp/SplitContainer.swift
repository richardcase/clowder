import SwiftUI
import MuxyCore

/// Recursively renders a PaneTree: leaves become terminals, splits lay their two children
/// along the axis at the tree's ratio (fixed divider — dragging is M1c-3).
struct SplitContainer: View {
    let node: PaneTree
    let surfaceHost: SurfaceHost
    @Binding var focusedPane: UInt64?

    var body: some View {
        switch node {
        case let .leaf(pane):
            TerminalContainer(pane: pane, surfaceHost: surfaceHost,
                              isFocused: focusedPane == pane,
                              onFocus: { focusedPane = pane })
                .overlay(
                    RoundedRectangle(cornerRadius: 3)
                        .strokeBorder(focusedPane == pane ? Color.accentColor : Color.clear, lineWidth: 2)
                )
        case let .split(_, axis, ratio, first, second):
            GeometryReader { geo in
                let horizontal = axis == .horizontal
                let total = horizontal ? geo.size.width : geo.size.height
                let firstLen = max(0, total * ratio - 0.5)
                if horizontal {
                    HStack(spacing: 0) {
                        SplitContainer(node: first, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                            .frame(width: firstLen)
                        Divider()
                        SplitContainer(node: second, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                    }
                } else {
                    VStack(spacing: 0) {
                        SplitContainer(node: first, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                            .frame(height: firstLen)
                        Divider()
                        SplitContainer(node: second, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                    }
                }
            }
        }
    }
}
