import SwiftUI
import ClowderCore

/// The Settings window (⌘,). One tab today; `TabView` so General/Keys can be added later without
/// restructuring the scene.
struct SettingsView: View {
    let hosts: HostsViewModel?

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
        }
    }
}
