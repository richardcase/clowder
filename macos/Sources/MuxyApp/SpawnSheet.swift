import SwiftUI
import MuxyCore

struct SpawnSheet: View {
    let adapters: [AdapterInfo]
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
                Picker("Adapter", selection: $adapter) {
                    ForEach(adapters) { a in
                        Text(a.displayName).tag(a.id)
                    }
                }
            }
            HStack {
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Spawn") {
                    onSpawn(project.trimmingCharacters(in: .whitespaces),
                            task.trimmingCharacters(in: .whitespaces),
                            adapter)
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
