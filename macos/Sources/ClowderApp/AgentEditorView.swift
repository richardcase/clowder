// SPDX-License-Identifier: Apache-2.0

import SwiftUI
import ClowderCore

/// The per-agent form. Renders `model.draft`; every decision lives in `AgentsViewModel`.
struct AgentEditorView: View {
    @ObservedObject var model: AgentsViewModel

    private let bases = AgentProfileDraft.bases

    var body: some View {
        if let draft = model.draft {
            VStack(alignment: .leading, spacing: 12) {
                Form {
                    TextField("Name", text: Binding(
                        get: { model.draft?.displayName ?? "" },
                        set: { model.draft?.displayName = $0 }))

                    if draft.isNew {
                        TextField("Id", text: Binding(
                            get: { model.draft?.id ?? "" },
                            set: { model.draft?.id = $0 }))
                        .help("Used by `clowder spawn <project> <name> <id>`. Cannot be changed later.")
                        Picker("Agent", selection: Binding(
                            get: { model.draft?.base ?? "claude" },
                            set: { model.draft?.base = $0 })) {
                            ForEach(bases, id: \.self) { Text($0).tag($0) }
                        }
                    } else {
                        LabeledContent("Id", value: draft.id)
                        LabeledContent("Agent", value: draft.base)
                    }

                    Toggle("Show in New Worktree", isOn: Binding(
                        get: { model.draft?.enabled ?? false },
                        set: { model.draft?.enabled = $0 }))

                    TextField("Arguments", text: Binding(
                        get: { model.draft?.args ?? "" },
                        set: { model.draft?.args = $0 }))
                    .font(.system(.body, design: .monospaced))
                }

                if let err = draft.idError, draft.isNew, !draft.id.isEmpty {
                    Text(err).font(.caption).foregroundStyle(.red)
                }
                if let err = draft.displayNameError, !draft.displayName.isEmpty {
                    Text(err).font(.caption).foregroundStyle(.red)
                }
                if let err = draft.argsError {
                    Text(err).font(.caption).foregroundStyle(.red)
                } else if !model.preview.isEmpty {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Resolved").font(.caption).foregroundStyle(.secondary)
                        Text(model.preview)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                    }
                }

                Text("Arguments are appended to the agent's own. Tokens: "
                     + AgentArgs.tokens.map { "{{\($0)}}" }.joined(separator: ", "))
                    .font(.caption).foregroundStyle(.secondary)

                Spacer()
                HStack {
                    Button("Revert") { model.revert() }.disabled(!model.isDirty)
                    Spacer()
                    Button("Save") { model.save() }
                        .keyboardShortcut(.defaultAction)
                        .disabled(!model.isDirty || !draft.isValid)
                }
            }
            .padding(20)
        }
    }
}
