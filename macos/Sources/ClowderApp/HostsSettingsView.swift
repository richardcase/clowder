import SwiftUI
import ClowderCore

/// Master/detail over the host registry: the list on the left, the editor on the right.
struct HostsSettingsView: View {
    @ObservedObject var model: HostsViewModel

    var body: some View {
        HSplitView {
            VStack(spacing: 0) {
                List(selection: Binding(
                    get: { model.selected },
                    set: { model.select($0) }
                )) {
                    ForEach(model.hosts) { host in
                        HStack(spacing: 6) {
                            Image(systemName: host.isTrusted ? "lock.fill" : "lock.open")
                                .foregroundStyle(host.isTrusted ? .green : .secondary)
                                .help(host.isTrusted ? "Paired" : "Not paired")
                            VStack(alignment: .leading, spacing: 1) {
                                Text(host.name)
                                Text(host.address).font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            if !host.isEditable {
                                Text("config.toml").font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                        .tag(host.id)
                    }
                }

                Divider()
                HStack(spacing: 4) {
                    Button { model.beginAdd() } label: { Image(systemName: "plus") }
                        .help("Add a host")
                    Button {
                        if let id = model.selected { Task { await model.remove(id) } }
                    } label: { Image(systemName: "minus") }
                        .disabled(!model.canEditSelection)
                        .help("Remove the selected host")
                    Spacer()
                }
                .buttonStyle(.borderless)
                .padding(6)
            }
            .frame(minWidth: 220)

            Group {
                if model.draft != nil {
                    HostEditorView(model: model)
                } else {
                    Text("Select a host, or press + to add one.")
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .frame(minWidth: 380)
        }
        .task { await model.reload() }
        .alert("Host registry", isPresented: Binding(
            get: { model.lastError != nil },
            set: { if !$0 { model.dismissError() } }
        )) {
            Button("OK") { model.dismissError() }
        } message: {
            Text(model.lastError ?? "")
        }
    }
}
