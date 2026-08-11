import Foundation

/// The Swift half of the agent-argument rules, mirroring `clowder_config::agents`.
///
/// It exists so the Settings editor can reject a bad template as typed and show a resolved preview.
/// The daemon remains the authority — anything that slips through here still gets a clean error
/// back. Both halves are pinned to `docs/protocol/fixtures/agent-args.json`, so they cannot drift.
public enum AgentArgs {
    /// Must match `clowder_config::agents::TOKENS` exactly, in the same order.
    public static let tokens = ["project_name", "project_path", "workspace_name", "workspace_path", "branch"]

    public struct SplitError: Error, Equatable { public let message: String }

    /// Split a template the way `split_args` does: whitespace separates; `'…'` is fully literal;
    /// `"…"` honours `\"` and `\\`; `\` escapes outside quotes. No shell, no globbing, no `$VAR`.
    public static func split(_ s: String) throws -> [String] {
        var out: [String] = []
        var cur = ""
        var hasCur = false          // distinguishes `""` (an empty arg) from a gap between args
        var it = Array(s)
        var i = 0
        while i < it.count {
            let c = it[i]
            i += 1
            if c.isWhitespace {
                if hasCur { out.append(cur); cur = ""; hasCur = false }
            } else if c == "'" {
                hasCur = true
                var closed = false
                while i < it.count {
                    let n = it[i]; i += 1
                    if n == "'" { closed = true; break }
                    cur.append(n)
                }
                if !closed { throw SplitError(message: "unterminated single quote (') in arguments") }
            } else if c == "\"" {
                hasCur = true
                var closed = false
                while i < it.count {
                    let n = it[i]; i += 1
                    if n == "\"" { closed = true; break }
                    if n == "\\" {
                        guard i < it.count else { break }
                        let e = it[i]; i += 1
                        if e == "\"" || e == "\\" { cur.append(e) } else { cur.append("\\"); cur.append(e) }
                    } else {
                        cur.append(n)
                    }
                }
                if !closed { throw SplitError(message: "unterminated double quote (\") in arguments") }
            } else if c == "\\" {
                hasCur = true
                guard i < it.count else { throw SplitError(message: "trailing backslash (\\) in arguments") }
                cur.append(it[i]); i += 1
            } else {
                hasCur = true
                cur.append(c)
            }
        }
        if hasCur { out.append(cur) }
        return out
    }

    /// Nil when the template is acceptable; otherwise a user-facing reason (quoting or a bad token).
    public static func templateError(_ s: String) -> String? {
        let argv: [String]
        do { argv = try split(s) } catch let e as SplitError { return e.message } catch { return "\(error)" }
        let valid = tokens.map { "{{\($0)}}" }.joined(separator: ", ")
        for arg in argv {
            var rest = Substring(arg)
            while let start = rest.range(of: "{{") {
                let after = rest[start.upperBound...]
                guard let end = after.range(of: "}}") else { return "unclosed '{{' in \(arg)" }
                let token = String(after[..<end.lowerBound])
                if !tokens.contains(token) {
                    return "unknown token {{\(token)}} — valid tokens are \(valid)"
                }
                rest = after[end.upperBound...]
            }
        }
        return nil
    }

    /// What the arguments look like once resolved, using illustrative values — the editor's live
    /// preview. Each argv element is single-quoted when it contains whitespace, so the user can see
    /// that a value with a space stays ONE argument. Empty when the template does not parse.
    public static func preview(_ s: String) -> String {
        guard templateError(s) == nil, let argv = try? split(s) else { return "" }
        let example = [
            "project_name": "my-project",
            "project_path": "/Users/you/code/my-project",
            "workspace_name": "my-task",
            "workspace_path": "/Users/you/.local/share/clowder/worktrees/my-project-ab12cd34ef56/my-task",
            "branch": "clowder/my-task",
        ]
        return argv
            .map { arg -> String in
                var out = arg
                for (t, v) in example { out = out.replacingOccurrences(of: "{{\(t)}}", with: v) }
                return out.contains(where: { $0.isWhitespace }) ? "'\(out)'" : out
            }
            .joined(separator: " ")
    }
}
