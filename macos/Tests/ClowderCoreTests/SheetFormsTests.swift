import XCTest
@testable import ClowderCore

final class SheetFormsTests: XCTestCase {
    func testAddProjectFormRequiresANonEmptyPath() {
        XCTAssertFalse(AddProjectForm(path: "").isValid)
        XCTAssertFalse(AddProjectForm(path: "   ").isValid)
        XCTAssertTrue(AddProjectForm(path: "/code/alpha").isValid)
    }

    func testNewWorktreeFormMirrorsTheDaemonsNameRules() {
        func form(_ name: String) -> NewWorktreeForm {
            NewWorktreeForm(projectPath: "/p", name: name, adapter: "claude")
        }
        for ok in ["a", "add-projects", "fix_bug", "v1.2", "M10a"] {
            XCTAssertTrue(form(ok).isValid, "should accept \(ok)")
            XCTAssertNil(form(ok).nameError)
        }
        for bad in ["", "   ", String(repeating: "a", count: 65), ".", "..", "a..b",
                    "x.lock", "my feature", "feat/x", ".hidden", "-dash", "v1.", "café"] {
            XCTAssertFalse(form(bad).isValid, "should reject \(bad)")
            XCTAssertNotNil(form(bad).nameError, "rejection must explain itself: \(bad)")
        }
    }

    func testNewWorktreeFormRequiresAProject() {
        XCTAssertFalse(NewWorktreeForm(projectPath: "", name: "ok", adapter: "claude").isValid)
    }

    /// The same case table the Rust `validate_workspace_name` test reads. Two implementations of
    /// one rule set drift silently otherwise — the daemon's trailing-dot rule was added after the
    /// spec's rule list was written, and nothing would have caught a mirror built from the old one.
    func testAgreesWithTheSharedNameCases() throws {
        struct Case: Decodable { let name: String; let valid: Bool }
        let here = URL(fileURLWithPath: #filePath)
        let repo = here.deletingLastPathComponent()   // ClowderCoreTests
            .deletingLastPathComponent()              // Tests
            .deletingLastPathComponent()              // macos
            .deletingLastPathComponent()              // repo root
        let data = try Data(contentsOf: repo.appendingPathComponent(
            "docs/protocol/fixtures/worktree-names.json"))
        for c in try JSONDecoder().decode([Case].self, from: data) {
            let form = NewWorktreeForm(projectPath: "/p", name: c.name, adapter: "claude")
            XCTAssertEqual(form.nameError == nil, c.valid,
                           "disagreed on \(c.name.debugDescription) — if you changed a rule, update the shared cases and the Rust validator")
        }
    }

    func testNameErrorNamesTheProblem() {
        let e = NewWorktreeForm(projectPath: "/p", name: "my feature", adapter: "claude").nameError
        XCTAssertTrue(e?.contains("letters") == true, "unhelpful: \(e ?? "nil")")
    }
}
