// SPDX-License-Identifier: Apache-2.0

import SwiftUI
import ClowderCore

/// The Settings window (⌘,). `TabView` so panes can be added later without restructuring the scene.
struct SettingsView: View {
    let hosts: HostsViewModel?
    let agents: AgentsViewModel?

    var body: some View {
        TabView {
            Group {
                if let hosts {
                    HostsSettingsView(model: hosts)
                } else {
                    // Unbundled dev builds may bootstrap without a registry.
                    Text("Host management is unavailable in this build.")
                        .foregroundStyle(.secondary)
                }
            }
            .tabItem { Label("Hosts", systemImage: "network") }

            Group {
                if let agents {
                    AgentsSettingsView(model: agents)
                } else {
                    Text("Agent settings need a running daemon.")
                        .foregroundStyle(.secondary)
                }
            }
            .tabItem { Label("Agents", systemImage: "cpu") }
        }
    }
}
