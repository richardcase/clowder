import Foundation

public enum Axis: String, Decodable, Equatable, Sendable {
    case horizontal, vertical
}

public enum SplitDirection: String, Encodable, Equatable, Sendable {
    case right, down
}

/// Mirrors the Rust `PaneTree` (internally tagged on "kind"). Recursive → `indirect`,
/// with a manual decoder (Swift can't synthesize Codable for a tagged recursive enum).
public indirect enum PaneTree: Decodable, Equatable, Sendable {
    case leaf(pane: UInt64)
    case split(id: UInt64, axis: Axis, ratio: Double, first: PaneTree, second: PaneTree)

    private enum CodingKeys: String, CodingKey { case kind, pane, id, axis, ratio, first, second }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "leaf":
            self = .leaf(pane: try c.decode(UInt64.self, forKey: .pane))
        case "split":
            self = .split(
                id: try c.decode(UInt64.self, forKey: .id),
                axis: try c.decode(Axis.self, forKey: .axis),
                ratio: try c.decode(Double.self, forKey: .ratio),
                first: try c.decode(PaneTree.self, forKey: .first),
                second: try c.decode(PaneTree.self, forKey: .second))
        case let other:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: c, debugDescription: "unknown PaneTree kind: \(other)")
        }
    }

    /// Pane ids in render order (first-then-second).
    public var leaves: [UInt64] {
        switch self {
        case let .leaf(pane): return [pane]
        case let .split(_, _, _, first, second): return first.leaves + second.leaves
        }
    }
}
