import AppKit
import Combine
import ClowderCore

/// Owns the menu-bar status item: a live attention count + a menu of agents needing attention.
@MainActor
final class StatusBarController: NSObject {
    private let appModel: AppModel
    private let showWindow: () -> Void
    /// The current remote host (nil = local daemon). Read on menu open so it reflects live swaps.
    private let remoteHost: () -> String?
    /// The remote host from config (target for "Connect to remote" while in local mode).
    private let configuredRemoteHost: () -> String?
    /// Switch the backend live (nil = local daemon, non-nil = that remote host).
    private let switchBackend: (String?) -> Void
    private let statusItem: NSStatusItem
    private var cancellable: AnyCancellable?

    init(appModel: AppModel,
         showWindow: @escaping () -> Void,
         remoteHost: @escaping () -> String?,
         configuredRemoteHost: @escaping () -> String?,
         switchBackend: @escaping (String?) -> Void) {
        self.appModel = appModel
        self.showWindow = showWindow
        self.remoteHost = remoteHost
        self.configuredRemoteHost = configuredRemoteHost
        self.switchBackend = switchBackend
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        super.init()
        let menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu
        // objectWillChange fires before the @Published update, so refresh on the next tick.
        cancellable = appModel.objectWillChange.sink { [weak self] _ in
            DispatchQueue.main.async { self?.refresh() }
        }
        refresh()
    }

    /// Updates only the status button (count/icon); the menu is built lazily on open.
    private func refresh() {
        let n = appModel.store.attentionCount
        if let button = statusItem.button {
            if n > 0 {
                button.image = NSImage(systemSymbolName: "bell.badge.fill", accessibilityDescription: "agents need attention")
                button.imagePosition = .imageLeading
                button.title = " \(n)"
            } else {
                button.image = NSImage(systemSymbolName: "bell", accessibilityDescription: "clowder")
                button.imagePosition = .imageOnly
                button.title = ""
            }
        }
    }

    @discardableResult
    private func addItem(to menu: NSMenu, _ title: String, _ action: Selector) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
        item.target = self
        menu.addItem(item)
        return item
    }

    @objc private func selectAgent(_ sender: NSMenuItem) {
        if let pane = sender.representedObject as? UInt64 { appModel.selectedPane = pane }
        showWindow()
    }
    @objc private func showWindowAction() { showWindow() }
    @objc private func quitAction() { NSApp.terminate(nil) }
    @objc private func useLocalAction() { switchBackend(nil) }
    @objc private func useRemoteAction(_ sender: NSMenuItem) { switchBackend(sender.representedObject as? String) }
}

extension StatusBarController: NSMenuDelegate {
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()

        // Backend status header (disabled): "Remote: <host>" or "Local".
        let backendLabel = remoteHost().map { "Remote: \($0)" } ?? "Local"
        let backendItem = NSMenuItem(title: backendLabel, action: nil, keyEquivalent: "")
        backendItem.isEnabled = false
        menu.addItem(backendItem)

        // Live backend swap: offer the other mode. When remote → "Use local"; when local with a
        // configured remote host → "Connect to <host>". When local + none configured, no swap item.
        if remoteHost() != nil {
            addItem(to: menu, "Use local", #selector(useLocalAction))
        } else if let configured = configuredRemoteHost() {
            let item = addItem(to: menu, "Connect to \(configured)", #selector(useRemoteAction(_:)))
            item.representedObject = configured
        }
        menu.addItem(.separator())

        let needy = appModel.store.agentsNeedingAttention
        if needy.isEmpty {
            let item = NSMenuItem(title: "No agents need attention", action: nil, keyEquivalent: "")
            item.isEnabled = false
            menu.addItem(item)
        } else {
            for agent in needy {
                let proj = (agent.project as NSString).lastPathComponent
                let name = proj.isEmpty ? agent.project : proj
                let marker = agent.state == .needsInput ? "🔴" : "🔵"   // NeedsInput vs Completed
                let item = NSMenuItem(title: "\(marker) \(name) — \(agent.task)",
                                      action: #selector(selectAgent(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = agent.pane
                menu.addItem(item)
            }
        }
        menu.addItem(.separator())
        addItem(to: menu, "Show clowder Window", #selector(showWindowAction))
        let quit = addItem(to: menu, "Quit clowder", #selector(quitAction))
        quit.keyEquivalent = "q"
    }
}
