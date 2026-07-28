import AppKit
import GhosttyKit

/// Owns one SurfaceView per pane so switching agents never restarts `muxy attach`.
@MainActor
final class SurfaceHost {
    private let app: ghostty_app_t
    private let muxyBinary: String
    private let socketPath: String
    private var views: [UInt64: SurfaceView] = [:]

    init(app: ghostty_app_t, muxyBinary: String, socketPath: String) {
        self.app = app
        self.muxyBinary = muxyBinary
        self.socketPath = socketPath
    }

    func view(for pane: UInt64) -> SurfaceView {
        if let v = views[pane] { return v }
        let v = SurfaceView(app: app, paneId: pane, muxyBinary: muxyBinary, socketPath: socketPath)
        views[pane] = v
        return v
    }
}
