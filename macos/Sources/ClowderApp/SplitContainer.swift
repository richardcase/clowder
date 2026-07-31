import SwiftUI
import AppKit
import ClowderCore

/// Recursively renders a PaneTree: leaves become terminals, splits delegate to SplitNode
/// (which owns a draggable divider).
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
                        .allowsHitTesting(false)
                )
                .id(pane)
        case let .split(id, axis, ratio, first, second):
            SplitNode(id: id, axis: axis, ratio: ratio, first: first, second: second,
                      surfaceHost: surfaceHost, focusedPane: $focusedPane)
                .id(id)
        }
    }
}

/// One split node: two children along `axis` with a draggable divider. The rendered ratio is
/// `localRatio ?? ratio`; dragging updates `localRatio` (clamped) and, on release, sends
/// `setSplitRatio`; `.onChange(of: ratio)` syncs `localRatio` to the daemon echo so there's no
/// snap-back and external changes are honored.
private struct SplitNode: View {
    let id: UInt64
    let axis: ClowderCore.Axis
    let ratio: Double
    let first: PaneTree
    let second: PaneTree
    let surfaceHost: SurfaceHost
    @Binding var focusedPane: UInt64?
    @EnvironmentObject var model: AppModel

    @State private var localRatio: Double?
    @State private var dragStart: Double?

    private let thickness: CGFloat = 6

    private var effective: Double { localRatio ?? ratio }

    var body: some View {
        GeometryReader { geo in
            let horizontal = axis == .horizontal
            let total = horizontal ? geo.size.width : geo.size.height
            let firstLen = max(0, total * effective - thickness / 2)
            Group {
                if horizontal {
                    HStack(spacing: 0) {
                        SplitContainer(node: first, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                            .frame(width: firstLen)
                        divider(total: total, horizontal: true)
                        SplitContainer(node: second, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                    }
                } else {
                    VStack(spacing: 0) {
                        SplitContainer(node: first, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                            .frame(height: firstLen)
                        divider(total: total, horizontal: false)
                        SplitContainer(node: second, surfaceHost: surfaceHost, focusedPane: $focusedPane)
                    }
                }
            }
            .onChange(of: ratio) { _, newValue in localRatio = newValue }   // sync from daemon echo / external (macOS 14 two-param form)
        }
    }

    @ViewBuilder
    private func divider(total: CGFloat, horizontal: Bool) -> some View {
        Rectangle()
            .fill(Color.gray.opacity(0.35))
            .frame(width: horizontal ? thickness : nil, height: horizontal ? nil : thickness)
            .contentShape(Rectangle())
            .onHover { inside in
                if inside { (horizontal ? NSCursor.resizeLeftRight : NSCursor.resizeUpDown).set() }
                else { NSCursor.arrow.set() }
            }
            .gesture(
                DragGesture()
                    .onChanged { value in
                        let start = dragStart ?? effective
                        if dragStart == nil { dragStart = start }
                        let delta = horizontal ? value.translation.width : value.translation.height
                        localRatio = min(0.95, max(0.05, start + delta / max(total, 1)))
                    }
                    .onEnded { _ in
                        if let r = localRatio { model.setDividerRatio(split: id, ratio: r) }
                        dragStart = nil
                    }
            )
    }
}
