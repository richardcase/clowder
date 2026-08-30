// SPDX-License-Identifier: Apache-2.0

import AppKit
import Combine
import ClowderCore

/// Owns the menu-bar status item: a live attention count + a menu of agents needing attention.
@MainActor
final class StatusBarController: NSObject {
    private let appModel: AppModel
    private let backends: BackendSwitching
    private let showWindow: () -> Void
    private let statusItem: NSStatusItem
    private var cancellable: AnyCancellable?

    init(appModel: AppModel,
         backends: BackendSwitching,
         showWindow: @escaping () -> Void) {
        self.appModel = appModel
        self.backends = backends
        self.showWindow = showWindow
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
        if let pane = sender.representedObject as? UInt64 { appModel.selection = .worktree(pane) }
        showWindow()
    }
    @objc private func showWindowAction() { showWindow() }
    @objc private func quitAction() { NSApp.terminate(nil) }
    @objc private func selectBackend(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? BackendID else { return }
        backends.switchBackend(to: id)
    }
}

extension StatusBarController: NSMenuDelegate {
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        backends.refreshHosts()          // the registry may have changed in a shell

        let active = backends.activeBackend

        let local = addItem(to: menu, "Local", #selector(selectBackend(_:)))
        local.representedObject = BackendID.local
        local.state = active == .local ? .on : .off

        for host in backends.hosts {
            let title = host.isTrusted ? host.name : "\(host.name) — not paired"
            let item = addItem(to: menu, title, #selector(selectBackend(_:)))
            item.representedObject = host.backend
            item.state = host.backend == active ? .on : .off
        }
        if backends.hosts.isEmpty {
            let none = NSMenuItem(title: "No remote hosts configured", action: nil, keyEquivalent: "")
            none.isEnabled = false
            menu.addItem(none)
        }

        menu.addItem(.separator())

        let needy = appModel.store.worktreesNeedingAttention
        if needy.isEmpty {
            let item = NSMenuItem(title: "No agents need attention", action: nil, keyEquivalent: "")
            item.isEnabled = false
            menu.addItem(item)
        } else {
            for worktree in needy {
                let proj = (worktree.project as NSString).lastPathComponent
                let projName = proj.isEmpty ? worktree.project : proj
                let marker = worktree.state == .needsInput ? "🔴" : "🔵"   // NeedsInput vs Completed
                let item = NSMenuItem(title: "\(marker) \(projName) — \(worktree.name)",
                                      action: #selector(selectAgent(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = worktree.pane
                menu.addItem(item)
            }
        }
        menu.addItem(.separator())
        addItem(to: menu, "Show clowder Window", #selector(showWindowAction))
        let quit = addItem(to: menu, "Quit clowder", #selector(quitAction))
        quit.keyEquivalent = "q"
    }
}
