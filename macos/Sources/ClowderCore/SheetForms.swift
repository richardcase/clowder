import Foundation

/// The Add Project sheet's state. The daemon validates for real (it must — in remote mode the
/// path is on another host); this only gates the button.
public struct AddProjectForm: Equatable, Sendable {
    public var path: String
    public init(path: String = "") { self.path = path }
    public var isValid: Bool { !path.trimmingCharacters(in: .whitespaces).isEmpty }
}

/// The New Worktree sheet's state. `nameError` mirrors the daemon's `validate_workspace_name`
/// so the sheet can explain a bad name immediately. The daemon remains the authority — a name
/// that slips through here still gets a clean error back.
public struct NewWorktreeForm: Equatable, Sendable {
    public var projectPath: String
    public var name: String
    public var adapter: String

    public init(projectPath: String = "", name: String = "", adapter: String = "claude") {
        self.projectPath = projectPath
        self.name = name
        self.adapter = adapter
    }

    public var isValid: Bool { !projectPath.isEmpty && nameError == nil }

    /// Nil when the name is acceptable; otherwise a user-facing reason. Validates `name` AS SENT
    /// — no trimming — so this agrees with the daemon's `validate_workspace_name`, which also
    /// does not trim (whitespace, including a leading/trailing space, is rejected by the charset
    /// check below, not silently accepted).
    public var nameError: String? {
        let n = name
        if n.isEmpty { return "Name must not be empty" }
        if n.count > 64 { return "Name must be 64 characters or fewer" }
        if n == "." || n == ".." { return "Name must not be \(n)" }
        if n.contains("..") { return "Name must not contain '..'" }
        if n.hasSuffix(".lock") { return "Name must not end with '.lock' (git reserves it)" }
        if n.hasSuffix(".") { return "Name must not end with '.' (git rejects it as a ref)" }
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "._-"))
        if n.unicodeScalars.contains(where: { !allowed.contains($0) || !$0.isASCII }) {
            return "Name may contain only letters, digits, '.', '_' or '-'"
        }
        if n.hasPrefix(".") || n.hasPrefix("-") { return "Name must not start with \(n.prefix(1))" }
        return nil
    }
}
