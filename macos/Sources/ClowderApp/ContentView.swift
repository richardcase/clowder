// SPDX-License-Identifier: Apache-2.0

import SwiftUI
import AppKit
import ClowderCore

struct ContentView: View {
    @EnvironmentObject var model: AppModel
    let surfaceHost: SurfaceHost
    /// The active backend's supervisor state, threaded down to `ConnectionChipView`. See that
    /// view's doc comment for why this is a closure rather than a plain value.
    let supervisorState: () -> DaemonSupervisor.State
    let onRetry: () -> Void

    @State private var expanded: Set<String> = ContentView.loadExpanded()
    @State private var showCopiedToast = false
    @State private var copiedToastWorkItem: DispatchWorkItem?

    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 220, ideal: 260)
        } detail: {
            detail
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { model.showingAddProject = true } label: { Image(systemName: "plus") }
                    .disabled(model.connectionState != .live)
                    .help("Add a project")
            }
        }
        .sheet(isPresented: $model.showingAddProject) {
            // Read live off `model.activeBackend` (`@Published`) rather than a flag computed once
            // when the scene was built — that flag went stale across a backend swap and offered
            // the local file browser for a remote backend (or hid it for a local one).
            AddProjectSheet(canBrowse: model.activeBackend == .local) { path in
                model.addProject(path: path)
            }
        }
        .sheet(isPresented: $model.showingNewWorktree) {
            NewWorktreeSheet(projects: model.store.sidebar,
                              adapters: model.adapters,
                              initialProjectPath: model.newWorktreeProject) { project, name, adapter in
                model.spawn(project: project, name: name, adapter: adapter)
            }
        }
        .safeAreaInset(edge: .bottom) { statusBar }
        .overlay(alignment: .bottom) {
            if showCopiedToast {
                copiedToast
                    .padding(.bottom, 40)
                    .transition(.opacity.combined(with: .scale(scale: 0.95)))
            }
        }
        // A landed/discarded/lost worktree's SurfaceView (and its ghostty surface) would
        // otherwise stay cached for the app's lifetime — evict it once the worktree is gone from
        // the store, whatever removed it (land, discard, or the daemon losing track of it).
        .onChange(of: model.store.worktrees) { oldValue, newValue in
            for pane in Set(oldValue.keys).subtracting(newValue.keys) {
                surfaceHost.forget(pane: pane)
            }
        }
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
        .confirmationDialog(
            lifecycleTitle,
            isPresented: Binding(
                get: { model.pendingLifecycle != nil },
                set: { if !$0 { model.cancelLifecycle() } }
            ),
            presenting: model.pendingLifecycle
        ) { pending in
            Button(pending.action == .discard ? "Discard" : "Land",
                   role: pending.action == .discard ? .destructive : nil) {
                model.confirmLifecycle()
            }
            Button("Cancel", role: .cancel) { model.cancelLifecycle() }
        } message: { pending in
            Text(pending.action == .discard
                 ? "Deletes branch clowder/\(pending.name) and its work. This can't be undone."
                 : "Finalizes the work onto branch clowder/\(pending.name) and removes the agent.")
        }
    }

    private var lifecycleTitle: String {
        switch model.pendingLifecycle?.action {
        case .discard: return "Discard this agent?"
        case .land: return "Land this agent?"
        case nil: return ""
        }
    }

    private var sidebar: some View {
        List(selection: $model.selection) {
            ForEach(model.store.sidebar) { project in
                DisclosureGroup(isExpanded: binding(for: project.path)) {
                    ForEach(project.worktrees) { worktree in
                        HStack(spacing: 8) {
                            Circle().fill(color(for: worktree.state)).frame(width: 8, height: 8)
                            Text(worktree.name).lineLimit(1)
                            Spacer()
                        }
                        .tag(SidebarSelection.worktree(worktree.pane))
                        .contextMenu { worktreeMenu(worktree) }
                    }
                } label: {
                    projectRow(project)
                        .tag(SidebarSelection.project(project.path))
                        .contextMenu { projectMenu(project) }
                }
            }
        }
        .safeAreaInset(edge: .bottom) {
            Divider()
            ConnectionChipView(supervisorState: supervisorState, onRetry: onRetry)
        }
        .overlay {
            if model.store.sidebar.isEmpty && model.connectionState == .live {
                Text("No projects yet — add one with +").foregroundStyle(.secondary)
            }
        }
    }

    private func binding(for path: String) -> Binding<Bool> {
        Binding(
            get: { expanded.contains(path) },
            set: { isOpen in
                if isOpen { expanded.insert(path) } else { expanded.remove(path) }
                ContentView.saveExpanded(expanded)
            }
        )
    }

    private static let expandedKey = "clowder.sidebar.expandedProjects"
    private static func loadExpanded() -> Set<String> {
        Set(UserDefaults.standard.stringArray(forKey: expandedKey) ?? [])
    }
    private static func saveExpanded(_ s: Set<String>) {
        UserDefaults.standard.set(Array(s), forKey: expandedKey)
    }

    /// The project row: name, kind badge, and the attention rollup. The rollup renders regardless
    /// of the DisclosureGroup's expansion state, so a collapsed project can never hide a waiting agent.
    private func projectRow(_ project: SidebarProject) -> some View {
        HStack(spacing: 6) {
            Image(systemName: project.kind == "jj"
                  ? "point.3.connected.trianglepath.dotted" : "arrow.triangle.branch")
                .foregroundStyle(.secondary)
                .help(project.kind == "jj" ? "jj workspace" : "git worktree")
            Text(project.name).lineLimit(1)
            Spacer()
            if project.attentionCount > 0 {
                Text("\(project.attentionCount)")
                    .font(.caption2).monospacedDigit()
                    .padding(.horizontal, 5).padding(.vertical, 1)
                    .background(Capsule().fill(Color.red.opacity(0.85)))
                    .foregroundStyle(.white)
                    .help("\(project.attentionCount) waiting for input")
            }
            Button {
                model.newWorktreeProject = project.path
                model.showingNewWorktree = true
            } label: { Image(systemName: "plus") }
            .buttonStyle(.plain)
            .help("New worktree in \(project.name)")
        }
    }

    @ViewBuilder private func projectMenu(_ project: SidebarProject) -> some View {
        Button("New Worktree…") {
            model.newWorktreeProject = project.path
            model.showingNewWorktree = true
        }
        Button("Reveal in Finder") {
            NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: project.path)
        }
        Divider()
        // Not gated here — the daemon refuses while worktrees exist and its message names the
        // count, surfaced via the existing error banner. One authority for the rule.
        Button("Remove Project", role: .destructive) { model.removeProject(path: project.path) }
    }

    @ViewBuilder private func worktreeMenu(_ worktree: WorktreeInfo) -> some View {
        if worktree.state == .exited {
            Button("Restart Agent") {
                model.selection = .worktree(worktree.pane)
                surfaceHost.forget(pane: worktree.pane)   // evict the dead cached surface first
                model.restartSelectedWorktree()
            }
            Divider()
        }
        Button("Land") { model.selection = .worktree(worktree.pane); model.requestLifecycle(.land) }
        Button("Discard", role: .destructive) {
            model.selection = .worktree(worktree.pane)
            model.requestLifecycle(.discard)
        }
    }

    /// Three states: a resolved pane (terminal or agent), a project selection whose terminal is
    /// still opening (`selectedPane == nil` while `openProjectTerminal` is in flight), or nothing.
    @ViewBuilder private var detail: some View {
        if let pane = model.selectedPane {
            if let worktree = model.store.worktrees[pane], worktree.state == .exited {
                // The agent's process is gone: its `clowder attach` has exited and libghostty
                // would otherwise sit on "Process exited. Press any key to close." Show a
                // placeholder instead — and never re-attach to a dead pane on re-select.
                exitedPlaceholder(worktree)
            } else {
                SplitContainer(node: model.currentTree ?? .leaf(pane: pane),
                               surfaceHost: surfaceHost,
                               focusedPane: $model.focusedPane)
                    .id(pane)   // rebuild when switching agents; same agent's tree changes diff in place
            }
        } else if case let .project(path) = model.selection {
            if model.closedProjectTerminals.contains(path) {
                // The terminal was open and the user exited it (or it otherwise died). Offer a
                // way back in — but never auto-reopen, which would loop against a shell that
                // exits immediately. Also covers removing-then-re-adding the SAME project row,
                // which reselects nothing (the row is already selected) so no new open request
                // would otherwise fire.
                closedProjectTerminalPlaceholder(path)
            } else {
                ProgressView("Starting terminal…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        } else {
            Text("Select a project or worktree").foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func exitedPlaceholder(_ worktree: WorktreeInfo) -> some View {
        VStack(spacing: 10) {
            Image(systemName: "moon.zzz.fill")
                .font(.largeTitle)
                .foregroundStyle(.secondary)
            Text("Agent exited").font(.title3)
            Text(worktree.name).foregroundStyle(.secondary)
            Button("Restart Agent") {
                surfaceHost.forget(pane: worktree.pane)   // evict the dead cached surface first
                model.restartSelectedWorktree()
            }
            .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func closedProjectTerminalPlaceholder(_ path: String) -> some View {
        VStack(spacing: 10) {
            Image(systemName: "moon.zzz.fill")
                .font(.largeTitle)
                .foregroundStyle(.secondary)
            Text("Terminal closed").font(.title3)
            Button("Reopen") { model.openTerminal(forProject: path) }
                .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder private var statusBar: some View {
        VStack(spacing: 0) {
            if case .reconnecting = model.connectionState {
                // Auto-reconnect in progress — persists until we're live again, not dismissable.
                banner("Reconnecting to daemon…", color: .orange)
            } else if case let .closed(reason) = model.connectionState {
                // Terminal connection state — persists, not dismissable.
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
        .contentShape(Rectangle())
        .onTapGesture { copyBanner(text) }
        .onHover { hovering in
            if hovering { NSCursor.pointingHand.push() } else { NSCursor.pop() }
        }
        .help("Click to copy")
    }

    /// Copies a banner's message to the system clipboard and shows a transient confirmation. The
    /// dismiss button (when present) consumes mouse-down before this row's tap gesture sees it, so
    /// dismissing an error never also copies it.
    private func copyBanner(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)

        copiedToastWorkItem?.cancel()
        withAnimation { showCopiedToast = true }
        let workItem = DispatchWorkItem { withAnimation { showCopiedToast = false } }
        copiedToastWorkItem = workItem
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5, execute: workItem)
    }

    private var copiedToast: some View {
        Text("Copied to clipboard")
            .font(.callout)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(.regularMaterial, in: Capsule())
            .shadow(radius: 2)
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
