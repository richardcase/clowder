import Foundation

/// What the sidebar has selected. A project resolves to its terminal's pane (once open);
/// a worktree resolves to its agent's pane, which is also the worktree's durable identity —
/// the daemon re-spawns an agent under its original pane id.
public enum SidebarSelection: Hashable, Sendable {
    /// Canonical project path.
    case project(String)
    /// The worktree's pane id.
    case worktree(UInt64)
}
