import SwiftUI
import AppKit
import ClowderCore

struct AddProjectSheet: View {
    /// False when attached to a remote daemon — a local directory picker is meaningless there.
    let canBrowse: Bool
    let onAdd: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var form = AddProjectForm()

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add Project").font(.headline)
            HStack {
                TextField("Path to a git or jj repository", text: $form.path)
                    .textFieldStyle(.roundedBorder)
                if canBrowse {
                    Button("Browse…") { browse() }
                }
            }
            Text("Must be a git or jj repository on the daemon's host.")
                .font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button("Add") {
                    onAdd(form.path.trimmingCharacters(in: .whitespaces))
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!form.isValid)
            }
        }
        .padding(20)
        .frame(width: 460)
    }

    private func browse() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url {
            form.path = url.path
        }
    }
}
