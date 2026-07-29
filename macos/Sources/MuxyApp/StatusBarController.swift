import AppKit
import Combine
import MuxyCore

/// Owns the menu-bar status item: a live attention count + a menu of agents needing attention.
@MainActor
final class StatusBarController: NSObject {
    private let appModel: AppModel
    private let showWindow: () -> Void
    private let statusItem: NSStatusItem
    private var cancellable: AnyCancellable?

    init(appModel: AppModel, showWindow: @escaping () -> Void) {
        self.appModel = appModel
        self.showWindow = showWindow
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        super.init()
        // objectWillChange fires before the @Published update, so refresh on the next tick.
        cancellable = appModel.objectWillChange.sink { [weak self] _ in
            DispatchQueue.main.async { self?.refresh() }
        }
        refresh()
    }

    private func refresh() {
        let needy = appModel.store.agentsNeedingAttention
        let n = needy.count

        if let button = statusItem.button {
            if n > 0 {
                button.image = NSImage(systemSymbolName: "bell.badge.fill", accessibilityDescription: "agents need attention")
                button.imagePosition = .imageLeading
                button.title = " \(n)"
            } else {
                button.image = NSImage(systemSymbolName: "bell", accessibilityDescription: "muxy")
                button.imagePosition = .imageOnly
                button.title = ""
            }
        }

        let menu = NSMenu()
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
        addItem(to: menu, "Show muxy Window", #selector(showWindowAction))
        let quit = addItem(to: menu, "Quit muxy", #selector(quitAction))
        quit.keyEquivalent = "q"

        statusItem.menu = menu
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
}
