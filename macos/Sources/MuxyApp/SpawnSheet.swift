import SwiftUI

struct SpawnSheet: View {
    let onSpawn: (String, String, String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var project = ""
    @State private var task = ""
    @State private var adapter = "claude"

    private var isValid: Bool {
        !project.trimmingCharacters(in: .whitespaces).isEmpty &&
        !task.trimmingCharacters(in: .whitespaces).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Spawn Agent").font(.headline)
            Form {
                TextField("Project path", text: $project)
                    .textFieldStyle(.roundedBorder)
                TextField("Task", text: $task)
                    .textFieldStyle(.roundedBorder)
                TextField("Adapter", text: $adapter)
                    .textFieldStyle(.roundedBorder)
            }
            HStack {
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Spawn") {
                    let a = adapter.trimmingCharacters(in: .whitespaces)
                    onSpawn(project.trimmingCharacters(in: .whitespaces),
                            task.trimmingCharacters(in: .whitespaces),
                            a.isEmpty ? "claude" : a)
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!isValid)
            }
        }
        .padding(20)
        .frame(width: 440)
    }
}
