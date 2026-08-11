import AppKit
import ClowderCore
import GhosttyKit

/// Owns one SurfaceView per pane so switching agents never restarts `clowder attach`.
@MainActor
final class SurfaceHost {
    private let app: ghostty_app_t
    private let clowderBinary: String
    private(set) var socketPath: String
    private var views: [UInt64: SurfaceView] = [:]

    /// Runs a clowder command chosen from a pane's context menu. Set by the app delegate once the
    /// AppModel exists; re-applied on every `view(for:)` so ordering and `retarget` can't strand it.
    var runCommand: ((CommandID) -> Void)?

    /// Whether Close Pane applies to the focused pane right now.
    var canClosePane: (() -> Bool)?

    init(app: ghostty_app_t, clowderBinary: String, socketPath: String) {
        self.app = app
        self.clowderBinary = clowderBinary
        self.socketPath = socketPath
    }

    func view(for pane: UInt64) -> SurfaceView {
        let v = views[pane]
            ?? SurfaceView(app: app, paneId: pane, clowderBinary: clowderBinary, socketPath: socketPath)
        views[pane] = v
        v.onCommand = runCommand
        v.canClosePane = canClosePane
        return v
    }

    /// Point future panes at a different render socket (a live backend swap). Dropping every
    /// SurfaceView frees its ghostty surface, which terminates that pane's `clowder attach` child —
    /// so panes rebuild against the new socket when next requested.
    func retarget(socketPath: String) {
        views.removeAll()
        self.socketPath = socketPath
    }

    /// Drop a pane's cached surface so the next `view(for:)` runs a fresh `clowder attach`.
    /// Required for restart, which deliberately reuses the pane id: without this, the cached
    /// SurfaceView (and its already-dead `clowder attach` child) is returned unchanged, so the
    /// detail pane keeps showing "Process exited. Press any key to close." even after the daemon
    /// revives the agent under the same pane.
    func forget(pane: UInt64) { views[pane] = nil }
}
