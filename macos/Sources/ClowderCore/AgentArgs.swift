// SPDX-License-Identifier: Apache-2.0

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
    ///
    /// Walks `unicodeScalars`, not `Character`s: Rust's `split_args` iterates `chars()` (Unicode
    /// scalars), and Swift's `Character` groups a trailing combining mark onto whatever precedes it
    /// — even a delimiter like `"` — into one extended grapheme cluster. Iterating `Character`s would
    /// let a combining mark glued to a closing quote hide that quote from this scan, so the two
    /// implementations could disagree about whether input is even valid. Scalars keep them in lockstep.
    public static func split(_ s: String) throws -> [String] {
        var out: [String] = []
        var cur = ""
        var hasCur = false          // distinguishes `""` (an empty arg) from a gap between args
        let scalars = Array(s.unicodeScalars)
        var i = 0
        while i < scalars.count {
            let c = scalars[i]
            i += 1
            if c.properties.isWhitespace {
                if hasCur { out.append(cur); cur = ""; hasCur = false }
            } else if c == "'" {
                hasCur = true
                var closed = false
                while i < scalars.count {
                    let n = scalars[i]; i += 1
                    if n == "'" { closed = true; break }
                    cur.unicodeScalars.append(n)
                }
                if !closed { throw SplitError(message: "unterminated single quote (') in arguments") }
            } else if c == "\"" {
                hasCur = true
                var closed = false
                while i < scalars.count {
                    let n = scalars[i]; i += 1
                    if n == "\"" { closed = true; break }
                    if n == "\\" {
                        guard i < scalars.count else { break }
                        let e = scalars[i]; i += 1
                        if e == "\"" || e == "\\" {
                            cur.unicodeScalars.append(e)
                        } else {
                            cur.unicodeScalars.append("\\")
                            cur.unicodeScalars.append(e)
                        }
                    } else {
                        cur.unicodeScalars.append(n)
                    }
                }
                if !closed { throw SplitError(message: "unterminated double quote (\") in arguments") }
            } else if c == "\\" {
                hasCur = true
                guard i < scalars.count else { throw SplitError(message: "trailing backslash (\\) in arguments") }
                cur.unicodeScalars.append(scalars[i]); i += 1
            } else {
                hasCur = true
                cur.unicodeScalars.append(c)
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
                guard out.contains(where: { $0.isWhitespace }) else { return out }
                // POSIX-safe single quoting: a literal `'` cannot appear inside a single-quoted
                // string, so close the quote, emit an escaped apostrophe, and reopen it. Naively
                // wrapping `it's mine` as `'it's mine'` is not valid quoting — a value with both a
                // space and an apostrophe would render as something that does not parse the way the
                // preview implies.
                let escaped = out.replacingOccurrences(of: "'", with: "'\\''")
                return "'\(escaped)'"
            }
            .joined(separator: " ")
    }
}
