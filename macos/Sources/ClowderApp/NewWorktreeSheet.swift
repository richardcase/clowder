import SwiftUI
import ClowderCore

struct NewWorktreeSheet: View {
    let projects: [SidebarProject]
    let adapters: [AdapterInfo]
    /// Prefill from the current selection, or the last-used project.
    let initialProjectPath: String
    let onCreate: (String, String, String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var form = NewWorktreeForm()

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("New Worktree").font(.headline)
            Form {
                Picker("Project", selection: $form.projectPath) {
                    ForEach(projects) { p in Text(p.name).tag(p.path) }
                }
                TextField("Name", text: $form.name).textFieldStyle(.roundedBorder)
                if adapters.isEmpty {
                    Text("No agents are enabled — turn one on in Settings → Agents.")
                        .font(.caption).foregroundStyle(.secondary)
                } else {
                    Picker("Agent", selection: $form.adapter) {
                        ForEach(adapters) { a in Text(a.displayName).tag(a.id) }
                    }
                }
            }
            if let err = form.nameError, !form.name.isEmpty {
                Text(err).font(.caption).foregroundStyle(.red)
            }
            HStack {
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button("Create") {
                    onCreate(form.projectPath,
                             form.name.trimmingCharacters(in: .whitespaces),
                             form.adapter)
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!form.isValid || adapters.isEmpty)
            }
        }
        .padding(20)
        .frame(width: 460)
        .onAppear { applyDefaults() }
        // `onAppear` fires once, but the enabled-agent list can arrive later (the control connection
        // delivers `agentProfileList` asynchronously) or change while the sheet is open. Re-applying
        // keeps the picker live rather than stuck on a stale fallback — and `applyDefaults` only
        // fills in what the user has not chosen, so re-running it cannot move their selection.
        .onChange(of: adapters) { applyDefaults() }
    }

    private func applyDefaults() {
        form.applyDefaults(projects: projects, adapters: adapters,
                           initialProjectPath: initialProjectPath)
    }
}
