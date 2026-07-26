import Foundation
import GhosttyKit

// Link smoke test: prove the SwiftPM app target links libghostty and can call
// its C ABI through the GhosttyKit module map, before writing the embedding.
let info = ghostty_info()
var version = "?"
if let v = info.version {
    version = String(data: Data(bytes: UnsafeRawPointer(v), count: Int(info.version_len)),
                     encoding: .utf8) ?? "?"
}
print("MuxyApp: libghostty linked — version=\(version) build_mode=\(info.build_mode.rawValue)")
