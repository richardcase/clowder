import Foundation

public enum PaletteItemKind: Hashable, Sendable {
    case command(CommandID)
    case agent(pane: UInt64)
}

public struct PaletteItem: Identifiable, Sendable {
    public let id: PaletteItemKind
    public let title: String
    public let subtitle: String?
    public let kind: PaletteItemKind
    public init(id: PaletteItemKind, title: String, subtitle: String?, kind: PaletteItemKind) {
        self.id = id
        self.title = title
        self.subtitle = subtitle
        self.kind = kind
    }
}

/// Case-insensitive subsequence match. Returns a rank (lower is better: the index of the
/// first matched character, so a prefix match ranks best) or nil if `text` doesn't contain
/// `query` as a subsequence. Empty query matches everything at rank 0.
func fuzzyRank(_ query: String, _ text: String) -> Int? {
    if query.isEmpty { return 0 }
    let q = Array(query.lowercased())
    let t = Array(text.lowercased())
    var qi = 0
    var firstMatch: Int?
    for (ti, ch) in t.enumerated() where qi < q.count && ch == q[qi] {
        if firstMatch == nil { firstMatch = ti }
        qi += 1
    }
    return qi == q.count ? (firstMatch ?? 0) : nil
}

/// Fuzzy-filter commands (matched on title) and worktrees (matched on "project name") into one
/// ranked list — commands section first, then worktrees. Ties keep input order.
public func paletteResults(query: String, commands: [Command], worktrees: [WorktreeInfo]) -> [PaletteItem] {
    let trimmed = query.trimmingCharacters(in: .whitespaces)

    let cmdItems = commands.enumerated().compactMap { (i, c) -> (Int, Int, PaletteItem)? in
        guard let r = fuzzyRank(trimmed, c.title) else { return nil }
        return (r, i, PaletteItem(id: .command(c.id), title: c.title, subtitle: c.subtitle, kind: .command(c.id)))
    }
    let agentItems = worktrees.enumerated().compactMap { (i, a) -> (Int, Int, PaletteItem)? in
        guard let r = fuzzyRank(trimmed, "\(a.project) \(a.name)") else { return nil }
        let proj = (a.project as NSString).lastPathComponent
        let sub = proj.isEmpty ? a.project : proj
        return (r, i, PaletteItem(id: .agent(pane: a.pane), title: a.name, subtitle: sub, kind: .agent(pane: a.pane)))
    }

    let sortedCmds = cmdItems.sorted { ($0.0, $0.1) < ($1.0, $1.1) }.map(\.2)
    let sortedAgents = agentItems.sorted { ($0.0, $0.1) < ($1.0, $1.1) }.map(\.2)
    return sortedCmds + sortedAgents
}
