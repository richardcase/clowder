import SwiftUI
import MuxyCore

struct ContentView: View {
    @EnvironmentObject var model: AppModel
    let surfaceHost: SurfaceHost

    var body: some View {
        VStack {
            Text("muxy").font(.largeTitle)
            switch model.connectionState {
            case .connecting: Text("Connecting…")
            case .live: Text("\(model.store.agents.count) agent(s)")
            case .closed(let reason): Text(reason).foregroundStyle(.red)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
