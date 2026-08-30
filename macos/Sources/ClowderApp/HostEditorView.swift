// SPDX-License-Identifier: Apache-2.0

import SwiftUI
import ClowderCore

/// The per-host form. Purely a renderer: every rule (`nameError`, `addressError`, `tlsError`,
/// `isValid`) comes from `HostDraft`, and every operation from `HostsViewModel`. Fingerprint
/// formatting (`groupedFingerprint`) lives in ClowderCore too, not here — `PairingSheet` needs the
/// same formatting and a view-to-view dependency would create a cycle between this file and it.
struct HostEditorView: View {
    @ObservedObject var model: HostsViewModel
    @State private var showingPairing = false

    private var isReadOnly: Bool {
        // A `[remote] host` entry is defined in config.toml, which clowder never rewrites.
        model.draft?.isNew == false && !model.canEditSelection
    }

    var body: some View {
        Form {
            if isReadOnly {
                Text("Defined by [remote] host in config.toml — edit that file, or add a separate entry.")
                    .font(.caption).foregroundStyle(.secondary)
            }

            Section {
                TextField("Nickname", text: binding(\.name))
                if let e = model.draft?.nameError, !(model.draft?.name.isEmpty ?? true) {
                    Text(e).font(.caption).foregroundStyle(.red)
                }
                TextField("Address (host:port)", text: binding(\.address))
                if let e = model.draft?.addressError, !(model.draft?.address.isEmpty ?? true) {
                    Text(e).font(.caption).foregroundStyle(.red)
                }
            }

            Section {
                Toggle("Use TLS", isOn: binding(\.tls))
                SecureField(model.selectedHost?.hasToken == true ? "•••••••• (stored)" : "Token",
                            text: Binding(
                                get: { model.draft?.token ?? "" },
                                set: { model.draft?.token = $0.isEmpty ? nil : $0 }
                            ))
                Text("Typing a token replaces the stored one. Leave it blank to keep the current token.")
                    .font(.caption).foregroundStyle(.secondary)
                if let e = model.draft?.tlsError {
                    Text(e).font(.caption).foregroundStyle(.red)
                }
            }

            if model.draft?.isNew == false {
                Section("Trust") {
                    if let fp = model.selectedHost?.fingerprint {
                        Text(groupedFingerprint(fp))
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                    } else {
                        Text("Not paired — this host is trusted on first use.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    Button(model.selectedHost?.isTrusted == true ? "Re-pair…" : "Pair…") {
                        showingPairing = true
                    }
                    .disabled(isReadOnly)
                }
            }

            Section {
                HStack {
                    Spacer()
                    Button("Revert") { model.select(model.selected) }
                        .disabled(!model.isDirty)
                    Button("Save") { Task { await model.save() } }
                        .keyboardShortcut(.defaultAction)
                        .disabled(isReadOnly || !(model.draft?.isValid ?? false) || model.isBusy || !model.isDirty)
                }
            }
        }
        .formStyle(.grouped)
        .disabled(model.isBusy)
        .sheet(isPresented: $showingPairing) {
            PairingSheet(model: model, isPresented: $showingPairing)
        }
    }

    private func binding<T>(_ keyPath: WritableKeyPath<HostDraft, T>) -> Binding<T> where T: Equatable {
        Binding(
            get: { model.draft?[keyPath: keyPath] ?? HostDraft()[keyPath: keyPath] },
            set: { model.draft?[keyPath: keyPath] = $0 }
        )
    }
}
