import AppKit
import GhosttyKit

/// Owns one SurfaceView per pane so switching agents never restarts `clowder attach`.
@MainActor
final class SurfaceHost {
    private let app: ghostty_app_t
    private let clowderBinary: String
    private let socketPath: String
    private var views: [UInt64: SurfaceView] = [:]

    init(app: ghostty_app_t, clowderBinary: String, socketPath: String) {
        self.app = app
        self.clowderBinary = clowderBinary
        self.socketPath = socketPath
    }

    func view(for pane: UInt64) -> SurfaceView {
        if let v = views[pane] { return v }
        let v = SurfaceView(app: app, paneId: pane, clowderBinary: clowderBinary, socketPath: socketPath)
        views[pane] = v
        return v
    }
}
