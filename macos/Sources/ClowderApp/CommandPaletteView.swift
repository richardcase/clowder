import SwiftUI
import ClowderCore

struct CommandPaletteView: View {
    @EnvironmentObject var model: AppModel
    @State private var query = ""
    @State private var selectedIndex = 0
    @FocusState private var fieldFocused: Bool
    private let keymap = Keymap()

    private var results: [PaletteItem] {
        paletteResults(query: query,
                       commands: CommandRegistry.all(keymap: keymap),
                       worktrees: model.store.orderedWorktrees,
                       hosts: model.hosts,
                       activeBackend: model.activeBackend)
    }

    var body: some View {
        VStack(spacing: 0) {
            TextField("Search commands and agents…", text: $query)
                .textFieldStyle(.plain)
                .font(.title3)
                .padding(12)
                .focused($fieldFocused)
                .onSubmit { runSelected() }
            Divider()
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(results.enumerated()), id: \.element.id) { idx, item in
                            row(item, selected: idx == selectedIndex)
                                .id(idx)
                                .contentShape(Rectangle())
                                .onTapGesture { selectedIndex = idx; runSelected() }
                        }
                    }
                }
                .frame(maxHeight: 320)
                .onChange(of: selectedIndex) { proxy.scrollTo(selectedIndex, anchor: .center) }
            }
        }
        .frame(width: 560)
        .background(.regularMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .shadow(radius: 20)
        .onAppear { fieldFocused = true; selectedIndex = 0 }
        .onChange(of: query) { selectedIndex = 0 }
        .onKeyPress(.downArrow) { move(1); return .handled }
        .onKeyPress(.upArrow) { move(-1); return .handled }
        .onExitCommand { close() }   // Esc
    }

    private func row(_ item: PaletteItem, selected: Bool) -> some View {
        HStack(spacing: 8) {
            Image(systemName: icon(item.kind)).frame(width: 18)
            VStack(alignment: .leading, spacing: 1) {
                Text(item.title)
                if let s = item.subtitle {
                    Text(s).font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer()
        }
        .padding(.horizontal, 12).padding(.vertical, 7)
        .background(selected ? Color.accentColor.opacity(0.25) : Color.clear)
        .opacity(isEnabled(item) ? 1 : 0.4)
    }

    /// Command rows dim when the command doesn't apply to the current selection; agent and backend
    /// rows are always enabled (picking one always makes sense).
    private func isEnabled(_ item: PaletteItem) -> Bool {
        switch item.kind {
        case let .command(id): return model.isEnabled(id)
        case .agent: return true
        case .backend: return true
        }
    }

    private func icon(_ kind: PaletteItemKind) -> String {
        switch kind {
        case .command: return "command"
        case .agent: return "terminal"
        case .backend: return "network"
        }
    }

    private func move(_ delta: Int) {
        guard !results.isEmpty else { return }
        selectedIndex = max(0, min(results.count - 1, selectedIndex + delta))
    }

    private func runSelected() {
        guard results.indices.contains(selectedIndex) else { return }
        switch results[selectedIndex].kind {
        case let .command(id): model.run(id)
        case let .agent(pane): model.selection = .worktree(pane)
        case let .backend(id): model.requestSwitch(to: id)
        }
        close()
    }

    private func close() {
        query = ""
        model.showingPalette = false
    }
}
