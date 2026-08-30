// SPDX-License-Identifier: Apache-2.0

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

final class NewWorktreeFormDefaultsTests: XCTestCase {
    private func project(_ path: String) -> SidebarProject {
        SidebarProject(path: path, name: path, kind: "git", worktrees: [], attentionCount: 0)
    }
    private func adapter(_ id: String) -> AdapterInfo { AdapterInfo(id: id, displayName: id) }

    func testFillsAnEmptyFormFromTheInitialSelection() {
        var form = NewWorktreeForm(projectPath: "", name: "", adapter: "")
        form.applyDefaults(projects: [project("/a"), project("/b")],
                           adapters: [adapter("claude"), adapter("codex")],
                           initialProjectPath: "/b")
        XCTAssertEqual(form.projectPath, "/b")
        XCTAssertEqual(form.adapter, "claude")
    }

    func testFallsBackToTheFirstProjectWhenThereIsNoInitialSelection() {
        var form = NewWorktreeForm(projectPath: "", name: "", adapter: "")
        form.applyDefaults(projects: [project("/a")], adapters: [adapter("claude")],
                           initialProjectPath: "")
        XCTAssertEqual(form.projectPath, "/a")
    }

    func testReapplyingNeverMovesAChoiceTheUserAlreadyMade() {
        // The sheet re-applies defaults whenever the agent list changes, which can happen while the
        // user is mid-edit (a profile toggled in Settings, a reconnect). It must not yank them back.
        var form = NewWorktreeForm(projectPath: "/chosen", name: "task", adapter: "codex")
        form.applyDefaults(projects: [project("/a"), project("/chosen")],
                           adapters: [adapter("claude"), adapter("codex")],
                           initialProjectPath: "/a")
        XCTAssertEqual(form.projectPath, "/chosen", "re-applying must not reset the project")
        XCTAssertEqual(form.adapter, "codex", "re-applying must not reset the agent")
        XCTAssertEqual(form.name, "task")
    }

    func testRepointsTheAgentOnlyWhenTheChosenOneIsNoLongerOffered() {
        var form = NewWorktreeForm(projectPath: "/chosen", name: "", adapter: "codex")
        // codex disabled in Settings while the sheet is open.
        form.applyDefaults(projects: [project("/chosen")], adapters: [adapter("claude")],
                           initialProjectPath: "/chosen")
        XCTAssertEqual(form.adapter, "claude")
    }

    func testEveryAgentDisabledLeavesNoSelection() {
        var form = NewWorktreeForm(projectPath: "/chosen", name: "", adapter: "codex")
        form.applyDefaults(projects: [project("/chosen")], adapters: [], initialProjectPath: "/chosen")
        XCTAssertEqual(form.adapter, "", "no agent may be pre-selected when none is offered")
    }
}

final class HostDraftTests: XCTestCase {
    private func fixtureCases(_ name: String, file: StaticString = #filePath) throws -> [(String, Bool)] {
        struct Case: Decodable { let name: String; let valid: Bool }
        let here = URL(fileURLWithPath: "\(file)")
        let repo = here.deletingLastPathComponent()   // ClowderCoreTests
            .deletingLastPathComponent()              // Tests
            .deletingLastPathComponent()              // macos
            .deletingLastPathComponent()              // repo root
        let data = try Data(contentsOf: repo.appendingPathComponent("docs/protocol/fixtures/\(name)"))
        return try JSONDecoder().decode([Case].self, from: data).map { ($0.name, $0.valid) }
    }

    func testNameAgreesWithTheSharedFixture() throws {
        let cases = try fixtureCases("host-names.json")
        XCTAssertFalse(cases.isEmpty, "fixture must not be empty")
        for (name, valid) in cases {
            var draft = HostDraft()
            draft.name = name
            XCTAssertEqual(draft.nameError == nil, valid,
                           "disagreed on \(name.debugDescription) — if you changed a rule, update the "
                           + "shared cases AND clowder_config::hosts::validate_name")
        }
    }

    func testHostNamesAreNotValidatedLikeWorktreeNames() {
        // The two validators are deliberately different. `...` and `a..b` are fine host names but are
        // rejected as worktree names; conflating them would break hosts the CLI accepts.
        for good in ["...", "a..b"] {
            var draft = HostDraft(); draft.name = good
            XCTAssertNil(draft.nameError, "\(good) is a valid HOST name")
            XCTAssertNotNil(NewWorktreeForm(projectPath: "/p", name: good, adapter: "claude").nameError,
                            "\(good) should still be an invalid WORKTREE name")
        }
    }

    func testNameErrorNamesTheProblem() {
        var draft = HostDraft(); draft.name = "has space"
        XCTAssertTrue(draft.nameError?.contains("letters") == true, "unhelpful: \(draft.nameError ?? "nil")")
        draft.name = ".."
        XCTAssertTrue(draft.nameError?.contains("'..'") == true, "unhelpful: \(draft.nameError ?? "nil")")
    }

    func testAddressRequiresAHostAndAPort() {
        for good in ["h:7777", "10.0.0.5:1", "studio.tail1234.ts.net:7777", "[::1]:7777", "[fd7a::1]:22"] {
            var draft = HostDraft(); draft.address = good
            XCTAssertNil(draft.addressError, "\(good) should be valid")
        }
        for bad in ["", "h", "h:", ":7777", "h:0", "h:70000", "h:abc", "::1:7777", "[::1]7777", "a b:7777"] {
            var draft = HostDraft(); draft.address = bad
            XCTAssertNotNil(draft.addressError, "\(bad) should be invalid")
        }
    }

    func testIsValidRequiresBothFields() {
        var draft = HostDraft()
        XCTAssertFalse(draft.isValid, "an empty draft is not valid")
        draft.name = "studio"
        XCTAssertFalse(draft.isValid, "a name alone is not valid")
        draft.address = "s:7777"
        XCTAssertTrue(draft.isValid)
        draft.name = "bad name"
        XCTAssertFalse(draft.isValid)
    }

    func testATokenImpliesTLSIsRequired() {
        // The CLI refuses a token without TLS at add/set time; say so before the user submits.
        var draft = HostDraft()
        draft.name = "studio"; draft.address = "s:7777"
        draft.token = "s3cr3t"; draft.tls = false
        XCTAssertFalse(draft.isValid)
        XCTAssertNotNil(draft.tlsError)
        draft.tls = true
        XCTAssertTrue(draft.isValid)
        XCTAssertNil(draft.tlsError)
    }
}

final class AgentProfileDraftTests: XCTestCase {
    func testValidDraftIsValid() {
        let d = AgentProfileDraft(id: "opus", base: "claude", displayName: "Claude (Opus)",
                                  enabled: true, args: "--model opus", isNew: true)
        XCTAssertNil(d.idError)
        XCTAssertNil(d.displayNameError)
        XCTAssertNil(d.argsError)
        XCTAssertTrue(d.isValid)
    }

    func testIdFollowsTheHostNameRule() throws {
        var d = AgentProfileDraft(id: "has space", base: "claude", displayName: "x", enabled: true,
                                  args: "", isNew: true)
        XCTAssertNotNil(d.idError)
        d.id = ""
        XCTAssertNotNil(d.idError)
        d.id = "a.b-c_1"
        XCTAssertNil(d.idError)
    }

    func testBlankDisplayNameAndBadArgsAreRejected() {
        var d = AgentProfileDraft(id: "opus", base: "claude", displayName: "  ", enabled: true,
                                  args: "", isNew: true)
        XCTAssertNotNil(d.displayNameError)
        XCTAssertFalse(d.isValid)

        d.displayName = "Opus"
        d.args = "--x {{nope}}"
        XCTAssertNotNil(d.argsError)
        XCTAssertFalse(d.isValid)
    }

    func testNewlineOnlyDisplayNameIsBlank() {
        // Mirrors clowder_config::agents::validate_profile, which uses `trim()` (strips newlines) —
        // not just `.whitespaces`, which in Foundation does NOT include newlines. A "\n" name must
        // be rejected here exactly as the daemon would reject it.
        let d = AgentProfileDraft(id: "opus", base: "claude", displayName: "\n", enabled: true,
                                  args: "", isNew: true)
        XCTAssertNotNil(d.displayNameError, "a newline-only name must count as blank")
        XCTAssertFalse(d.isValid)
    }

    func testArgsAtTheLengthLimitIsAccepted() {
        let d = AgentProfileDraft(id: "opus", base: "claude", displayName: "Opus", enabled: true,
                                  args: String(repeating: "a", count: 4096), isNew: true)
        XCTAssertNil(d.argsError, "4096 chars is the boundary, still valid")
        XCTAssertTrue(d.isValid)
    }

    func testArgsOverTheLengthLimitIsRejected() {
        let d = AgentProfileDraft(id: "opus", base: "claude", displayName: "Opus", enabled: true,
                                  args: String(repeating: "a", count: 4097), isNew: true)
        XCTAssertNotNil(d.argsError, "4097 chars exceeds the args limit")
        XCTAssertFalse(d.isValid)
    }
}
