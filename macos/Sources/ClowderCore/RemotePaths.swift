import Foundation

/// Absolute dir where the M7b forwarder (`clowder connect`) binds its local sockets:
/// `<control-sock parent>/remote`, matching the Rust `forward` derivation
/// (`crates/clowder-client/src/main.rs` → `crates/clowder-client/src/forward.rs`).
///
/// This is the load-bearing seam: the Swift app and the Rust forwarder must compute the *same*
/// directory. Always feed this the **default local control path** — never the forwarder's own
/// socket — so it can't double up to `.../remote/remote`.
public func forwarderSocketDir(controlPath: String) -> String {
    return (controlPath as NSString).deletingLastPathComponent + "/remote"
}
