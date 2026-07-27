import Foundation

/// Accumulates bytes and yields complete newline-terminated lines (newline stripped),
/// holding any trailing partial line until its newline arrives.
public struct LineBuffer {
    private var pending = Data()

    public init() {}

    public mutating func append(_ bytes: Data) -> [String] {
        pending.append(bytes)
        var lines: [String] = []
        while let nl = pending.firstIndex(of: 0x0A) {
            let lineData = pending.subdata(in: pending.startIndex..<nl)
            pending.removeSubrange(pending.startIndex...nl)
            lines.append(String(decoding: lineData, as: UTF8.self))
        }
        return lines
    }
}
