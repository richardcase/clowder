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
}
