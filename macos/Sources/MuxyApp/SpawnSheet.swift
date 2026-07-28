import SwiftUI

struct SpawnSheet: View {
    let onSpawn: (String, String, String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var project = ""
    @State private var task = ""
    @State private var adapter = "claude"

    var body: some View {
        Form {
            TextField("Project path", text: $project)
            TextField("Task", text: $task)
            TextField("Adapter", text: $adapter)
            HStack {
                Button("Cancel") { dismiss() }
                Spacer()
                Button("Spawn") {
                    onSpawn(project, task, adapter.isEmpty ? "claude" : adapter)
                    dismiss()
                }
            }
        }
        .padding()
        .frame(width: 420)
    }
}
