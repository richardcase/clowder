import SwiftUI
import ClowderCore

/// The always-visible backend indicator at the bottom of the sidebar. A menu whose label says
/// which backend is live and how healthy it is, and whose contents switch to another.
struct ConnectionChipView: View {
    @EnvironmentObject private var model: AppModel
    /// The active backend's supervisor state, lifted from the delegate (Core has no access to it).
    ///
    /// A closure rather than a plain value: `DaemonSupervisor` is not `ObservableObject` (and
    /// neither is the delegate), so nothing pushes its changes into SwiftUI on its own — the exact
    /// class of bug this task fixes for `isRemote`. Reading it as a closure means every time THIS
    /// view's `body` re-runs it fetches the delegate's CURRENT value rather than one frozen at
    /// construction time. `body` re-runs on every `AppModel` publish this view observes
    /// (`activeBackend`/`hosts`/`connectionState`), which includes the reconnect loop's per-attempt
    /// republish of `connectionState` even when the value is unchanged — so a supervisor that fails
    /// while the control channel is retrying is picked up within one backoff cycle. See the task
    /// report for the residual gap: a supervisor transition with no accompanying `AppModel` publish
    /// at all would not repaint until something else triggers one.
    let supervisorState: () -> DaemonSupervisor.State
    let onRetry: () -> Void

    private var chip: ConnectionChip {
        connectionChip(backend: model.activeBackend, hosts: model.hosts,
                       connection: model.connectionState, supervisor: supervisorState())
    }

    var body: some View {
        Menu {
            // Refresh the host registry as the menu opens, so a host added on the CLI while the app
            // is running shows up without a restart. This has to live INSIDE `content:`, not as a
            // `.onTapGesture` on the `Menu` itself: `Menu`'s press-to-open handling (an
            // `NSPopUpButton`/`NSMenu` on macOS) consumes the mouse-down before a sibling gesture
            // recognizer ever sees it, so a tap gesture on the `Menu` is a silent no-op — the same
            // well-known issue as `.onTapGesture` on a plain `Button`. SwiftUI evaluates `content:`
            // when the menu is about to present, so an invisible view's `.onAppear` here fires
            // exactly on open. Do not "simplify" this back onto the `Menu`.
            Color.clear
                .frame(width: 0, height: 0)
                .onAppear { model.requestHostRefresh() }

            Button {
                model.requestSwitch(to: .local)
            } label: {
                Label("Local", systemImage: model.activeBackend == .local ? "checkmark" : "")
            }
            .disabled(model.activeBackend == .local)

            if model.hosts.isEmpty {
                Divider()
                Text("No remote hosts configured")
            } else {
                Divider()
                ForEach(model.hosts) { host in
                    Button {
                        model.requestSwitch(to: host.backend)
                    } label: {
                        // An unpaired host still connects (trust-on-first-use) — say so rather
                        // than hiding it, so the user knows which hosts they have verified.
                        Text(host.isTrusted ? host.name : "\(host.name) — not paired")
                    }
                    .disabled(host.backend == model.activeBackend)
                }
            }

            if chip.canRetry {
                Divider()
                Button("Retry", action: onRetry)
            }

            Divider()
            SettingsLink { Text("Manage Hosts…") }
        } label: {
            HStack(spacing: 6) {
                Circle().fill(color(chip.tone)).frame(width: 7, height: 7)
                Image(systemName: chip.symbol).imageScale(.small)
                VStack(alignment: .leading, spacing: 0) {
                    Text(chip.title).font(.caption).lineLimit(1)
                    if let detail = chip.detail {
                        Text(detail).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                Spacer()
            }
            .contentShape(Rectangle())
        }
        .menuStyle(.borderlessButton)
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
    }

    private func color(_ tone: ChipTone) -> Color {
        switch tone {
        case .ok: return .green
        case .pending: return .secondary
        case .warning: return .orange
        case .error: return .red
        }
    }
}
