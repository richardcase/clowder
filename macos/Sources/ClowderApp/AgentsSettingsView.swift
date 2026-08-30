// SPDX-License-Identifier: Apache-2.0

import SwiftUI
import ClowderCore

/// Master/detail over the agent profiles: the list on the left, the editor on the right.
struct AgentsSettingsView: View {
    @ObservedObject var model: AgentsViewModel

    var body: some View {
        HSplitView {
            VStack(spacing: 0) {
                List(selection: Binding(
                    get: { model.selected },
                    set: { model.select($0) }
                )) {
                    ForEach(model.profiles) { p in
                        HStack(spacing: 6) {
                            Image(systemName: p.enabled ? "checkmark.circle.fill" : "circle")
                                .foregroundStyle(p.enabled ? .green : .secondary)
                                .help(p.enabled ? "Shown in New Worktree" : "Hidden from New Worktree")
                            VStack(alignment: .leading, spacing: 1) {
                                Text(p.displayName)
                                Text(p.args.isEmpty ? p.base : "\(p.base) \(p.args)")
                                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }
                            Spacer()
                            if p.builtin {
                                Text("built-in").font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                        .tag(p.id)
                    }
                }

                Divider()
                HStack(spacing: 4) {
                    Button { model.beginAdd() } label: { Image(systemName: "plus") }
                        .help("Add an agent")
                    Button {
                        if let id = model.selected { model.remove(id) }
                    } label: { Image(systemName: "minus") }
                        .disabled(!model.canRemoveSelection)
                        .help("Remove the selected agent (built-ins can only be disabled)")
                    Button { model.duplicateSelected() } label: { Image(systemName: "plus.square.on.square") }
                        .disabled(model.selectedProfile == nil)
                        .help("Duplicate the selected agent")
                    Spacer()
                }
                .buttonStyle(.borderless)
                .padding(6)
            }
            .frame(minWidth: 220)

            Group {
                if model.draft != nil {
                    AgentEditorView(model: model)
                } else {
                    Text("Select an agent, or press + to add one.")
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .frame(minWidth: 420)
        }
        .onAppear {
            model.reload()
            // Discard whatever is sitting in the pane's error slot from a previous, possibly
            // unrelated, occasion — `AgentStore.lastError` is the app's one uncorrelated error
            // channel, so an error that arrived while this pane was closed (a failed spawn, a host
            // error, anything) must not pop an "Agents" alert about it the next time the pane opens.
            model.dismissError()
        }
        .alert("Agents", isPresented: Binding(
            get: { model.lastError != nil },
            set: { if !$0 { model.dismissError() } }
        )) {
            Button("OK") { model.dismissError() }
        } message: {
            Text(model.lastError ?? "")
        }
    }
}
