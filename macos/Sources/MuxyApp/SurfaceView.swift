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

    override func keyDown(with event: NSEvent) {
        // Best-effort text forwarding for the spike (rendering is the primary proof).
        guard let surface, let chars = event.characters, !chars.isEmpty else { return }
        chars.withCString { ptr in
            ghostty_surface_text(surface, ptr, UInt(strlen(ptr)))
        }
    }

    deinit {
        if let surface { ghostty_surface_free(surface) }
    }
}
