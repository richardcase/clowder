import XCTest
@testable import ClowderCore

final class AgentArgsTests: XCTestCase {
    private struct Case: Decodable {
        let input: String
        let argv: [String]?
        let error: String?
    }

    private func cases(file: StaticString = #filePath) throws -> [Case] {
        let here = URL(fileURLWithPath: "\(file)")
        let repo = here.deletingLastPathComponent()   // ClowderCoreTests
            .deletingLastPathComponent()              // Tests
            .deletingLastPathComponent()              // macos
            .deletingLastPathComponent()              // repo root
        let data = try Data(contentsOf: repo.appendingPathComponent("docs/protocol/fixtures/agent-args.json"))
        return try JSONDecoder().decode([Case].self, from: data)
    }

    func testSplitAgreesWithTheSharedFixture() throws {
        let all = try cases()
        XCTAssertFalse(all.isEmpty, "fixture must not be empty")
        for c in all {
            switch (c.argv, c.error) {
            case let (argv?, _):
                XCTAssertEqual(try? AgentArgs.split(c.input), argv,
                               "split disagreed on \(c.input.debugDescription) — if you changed a rule, "
                               + "update the shared cases AND clowder_config::agents::split_args")
            case (nil, "quote"):
                XCTAssertThrowsError(try AgentArgs.split(c.input), c.input)
            case (nil, "token"):
                XCTAssertNoThrow(try AgentArgs.split(c.input), c.input)
            default:
                XCTFail("case \(c.input.debugDescription) has neither argv nor a known error")
            }
        }
    }

    func testTemplateErrorAgreesWithTheSharedFixture() throws {
        for c in try cases() {
            if c.argv != nil {
                XCTAssertNil(AgentArgs.templateError(c.input), c.input)
            } else {
                XCTAssertNotNil(AgentArgs.templateError(c.input), c.input)
            }
        }
    }

    func testTemplateErrorNamesTheOffendingToken() {
        let e = AgentArgs.templateError("--x {{nope}}")
        XCTAssertTrue(e?.contains("nope") == true, "unhelpful: \(e ?? "nil")")
        XCTAssertTrue(e?.contains("workspace_name") == true, "must list the valid tokens: \(e ?? "nil")")
    }

    func testPreviewShowsResolvedArgumentsOneQuotedElementEach() {
        let out = AgentArgs.preview("--prompt \"work on {{workspace_name}}\" --p {{project_name}}")
        XCTAssertEqual(out, "--prompt 'work on my-task' --p my-project")
    }

    func testPreviewOfABadTemplateIsEmptyRatherThanMisleading() {
        XCTAssertEqual(AgentArgs.preview("\"unterminated"), "")
    }

    /// Regression for a divergence the fixture cannot express without changing the cross-language
    /// contract: Rust's `split_args` iterates `chars()` (Unicode scalars), so a combining mark right
    /// after a closing `"` is just the next ordinary character. A naive Swift `Character` walk instead
    /// groups that combining mark onto the quote it follows into ONE extended grapheme cluster — so
    /// the scan never sees a bare `"` there, the quote never closes, and Swift would wrongly report an
    /// unterminated double quote where Rust accepts the input. `\u{0301}` (combining acute) is used
    /// explicitly so the divergence is visible in the test itself.
    func testSplitWalksUnicodeScalarsLikeRustNotGraphemeClusters() throws {
        let input = "\"x\"\u{0301}"
        XCTAssertEqual(try AgentArgs.split(input), ["x\u{0301}"])
    }

    func testPreviewEscapesAnApostropheInsideAQuotedElement() {
        let out = AgentArgs.preview("--note \"it's mine\"")
        XCTAssertEqual(out, "--note 'it'\\''s mine'")
    }
}
