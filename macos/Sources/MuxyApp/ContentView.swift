import SwiftUI
import MuxyCore

struct ContentView: View {
    @EnvironmentObject var model: AppModel
    let surfaceHost: SurfaceHost

    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 220, ideal: 260)
        } detail: {
            detail
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { model.showingSpawn = true } label: { Image(systemName: "plus") }
                    .disabled(model.connectionState != .live)
                    .help("Spawn a new agent")
            }
        }
        .sheet(isPresented: $model.showingSpawn) {
            SpawnSheet { project, task, adapter in
                model.spawn(project: project, task: task, adapter: adapter)
            }
        }
        .safeAreaInset(edge: .bottom) { statusBar }
        .overlay {
            if model.showingPalette {
                ZStack(alignment: .top) {
                    Color.black.opacity(0.2).ignoresSafeArea()
                        .onTapGesture { model.showingPalette = false }
                    CommandPaletteView()
                        .padding(.top, 80)
                }
            }
        }
    }

    private var sidebar: some View {
        List(selection: $model.selectedPane) {
            ForEach(model.store.byProject, id: \.project) { group in
                Section(header: Text(projectLabel(group.project))) {
                    ForEach(group.agents) { agent in
                        HStack(spacing: 8) {
                            Circle()
                                .fill(color(for: agent.state))
                                .frame(width: 8, height: 8)
                            Text(agent.task).lineLimit(1)
                            Spacer()
                        }
                        .tag(agent.pane)
                    }
                }
            }
        }
        .overlay {
            if model.store.agents.isEmpty && model.connectionState == .live {
                Text("No agents yet — spawn one with +").foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private var detail: some View {
        if let pane = model.selectedPane, let agent = model.store.agents[pane] {
            if agent.state == .exited {
                // The agent's process is gone: its `muxy attach` has exited and libghostty
                // would otherwise sit on "Process exited. Press any key to close." Show a
                // placeholder instead — and never re-attach to a dead pane on re-select.
                exitedPlaceholder(agent)
            } else {
                TerminalContainer(pane: pane, surfaceHost: surfaceHost)
                    .id(pane)
            }
        } else {
            Text("Select an agent").foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func exitedPlaceholder(_ agent: AgentInfo) -> some View {
        VStack(spacing: 8) {
            Image(systemName: "moon.zzz.fill")
                .font(.largeTitle)
                .foregroundStyle(.secondary)
            Text("Agent exited").font(.title3)
            Text(agent.task).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder private var statusBar: some View {
        VStack(spacing: 0) {
            if case let .closed(reason) = model.connectionState {
                // Live connection state — persists until reconnect, so not dismissable.
                banner(reason, color: .red)
            } else if let err = model.store.lastError {
                // A one-shot error — dismissable.
                banner(err, color: .orange, onDismiss: { model.dismissError() })
            }
        }
    }

    private func banner(_ text: String, color: Color, onDismiss: (() -> Void)? = nil) -> some View {
        HStack {
            Image(systemName: "exclamationmark.triangle.fill")
            Text(text).lineLimit(2)
            Spacer()
            if let onDismiss {
                Button(action: onDismiss) {
                    Image(systemName: "xmark.circle.fill")
                }
                .buttonStyle(.plain)
                .help("Dismiss")
            }
        }
        .font(.callout)
        .padding(8)
        .frame(maxWidth: .infinity)
        .background(color.opacity(0.15))
        .foregroundStyle(color)
    }

    private func projectLabel(_ path: String) -> String {
        (path as NSString).lastPathComponent.isEmpty ? path : (path as NSString).lastPathComponent
    }

    private func color(for state: AttentionState) -> Color {
        switch state {
        case .needsInput: return .red        // the whole point — must be loud
        case .working:    return .green
        case .completed:  return .blue
        case .exited:     return .gray
        case .idle:       return .secondary
        }
    }
}
