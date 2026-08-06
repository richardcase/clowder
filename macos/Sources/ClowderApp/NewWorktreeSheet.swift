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
                Picker("Agent", selection: $form.adapter) {
                    ForEach(adapters) { a in Text(a.displayName).tag(a.id) }
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
                .disabled(!form.isValid)
            }
        }
        .padding(20)
        .frame(width: 460)
        .onAppear {
            form.projectPath = initialProjectPath.isEmpty ? (projects.first?.path ?? "") : initialProjectPath
            form.adapter = adapters.first?.id ?? "claude"
        }
    }
}
