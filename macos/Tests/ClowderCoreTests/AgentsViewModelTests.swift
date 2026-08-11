import XCTest
@testable import ClowderCore

@MainActor
final class AgentsViewModelTests: XCTestCase {
    private final class Recorder {
        var sent: [ControlRequest] = []
        var failNext = false
    }

    private func model() -> (AgentsViewModel, Recorder) {
        let rec = Recorder()
        let vm = AgentsViewModel(send: { req in
            if rec.failNext { rec.failNext = false; throw NSError(domain: "test", code: 1) }
            rec.sent.append(req)
        })
        vm.apply(profiles: [
            AgentProfileInfo(id: "claude", base: "claude", displayName: "Claude Code",
                             enabled: true, args: "", builtin: true),
            AgentProfileInfo(id: "codex", base: "codex", displayName: "OpenAI Codex",
                             enabled: true, args: "", builtin: true),
            AgentProfileInfo(id: "opus", base: "claude", displayName: "Claude (Opus)",
                             enabled: true, args: "--model opus", builtin: false),
        ])
        return (vm, rec)
    }

    func testReloadAsksTheDaemon() {
        let (vm, rec) = model()
        vm.reload()
        XCTAssertEqual(rec.sent, [.listAgentProfiles])
    }

    func testSelectFillsTheDraftFromTheProfile() {
        let (vm, _) = model()
        vm.select("opus")
        XCTAssertEqual(vm.draft?.id, "opus")
        XCTAssertEqual(vm.draft?.args, "--model opus")
        XCTAssertEqual(vm.draft?.isNew, false)
        XCTAssertFalse(vm.isDirty, "a freshly selected draft is not dirty")
    }

    func testSaveAnEditSendsAnUpdate() {
        let (vm, rec) = model()
        vm.select("opus")
        vm.draft?.args = "--model opus --verbose"
        XCTAssertTrue(vm.isDirty)
        vm.save()
        guard case let .updateAgentProfile(p)? = rec.sent.last else {
            return XCTFail("expected updateAgentProfile, got \(rec.sent)")
        }
        XCTAssertEqual(p.id, "opus")
        XCTAssertEqual(p.args, "--model opus --verbose")
    }

    func testSaveANewProfileSendsAnAdd() {
        let (vm, rec) = model()
        vm.beginAdd()
        vm.draft?.id = "plan"
        vm.draft?.displayName = "Planner"
        vm.draft?.base = "claude"
        vm.save()
        guard case let .addAgentProfile(p)? = rec.sent.last else {
            return XCTFail("expected addAgentProfile, got \(rec.sent)")
        }
        XCTAssertEqual(p.id, "plan")
        XCTAssertFalse(p.builtin)
    }

    func testSaveRefusesAnInvalidDraftAndExplainsWhy() {
        let (vm, rec) = model()
        vm.select("opus")
        vm.draft?.args = "--x {{nope}}"
        vm.save()
        XCTAssertTrue(rec.sent.isEmpty, "nothing may be sent for an invalid draft")
        XCTAssertTrue(vm.lastError?.contains("nope") == true, "unhelpful: \(vm.lastError ?? "nil")")
        XCTAssertEqual(vm.draft?.args, "--x {{nope}}", "a refused save must not disturb what was typed")
    }

    func testDuplicateProducesAnEditableCopyWithAFreshId() {
        let (vm, rec) = model()
        vm.select("opus")
        vm.duplicateSelected()
        XCTAssertEqual(vm.draft?.isNew, true)
        XCTAssertEqual(vm.draft?.base, "claude")
        XCTAssertEqual(vm.draft?.args, "--model opus")
        XCTAssertNotEqual(vm.draft?.id, "opus", "a duplicate needs its own id")
        XCTAssertNil(vm.draft?.idError, "the suggested id must be valid as-is: \(vm.draft?.id ?? "")")
        XCTAssertTrue(rec.sent.isEmpty, "duplicate is local until saved")
    }

    func testDuplicatingABuiltinSavesAsANewNonBuiltinProfile() {
        let (vm, rec) = model()
        vm.select("claude")
        vm.duplicateSelected()
        vm.save()
        guard case let .addAgentProfile(p)? = rec.sent.last else {
            return XCTFail("duplicating a builtin must ADD, never update it: \(rec.sent)")
        }
        XCTAssertEqual(p.id, "claude-copy")
        XCTAssertEqual(p.base, "claude")
        XCTAssertFalse(p.builtin)
    }

    func testRemoveSendsARemoveAndIsRefusedForBuiltins() {
        let (vm, rec) = model()
        vm.remove("opus")
        XCTAssertEqual(rec.sent.last, .removeAgentProfile(id: "opus"))

        rec.sent.removeAll()
        vm.remove("claude")
        XCTAssertTrue(rec.sent.isEmpty, "a builtin removal must not reach the daemon")
        XCTAssertTrue(vm.lastError?.contains("built-in") == true, "unhelpful: \(vm.lastError ?? "nil")")
    }

    func testCanRemoveSelectionIsFalseForBuiltinsAndNoSelection() {
        let (vm, _) = model()
        XCTAssertFalse(vm.canRemoveSelection)
        vm.select("claude")
        XCTAssertFalse(vm.canRemoveSelection)
        vm.select("opus")
        XCTAssertTrue(vm.canRemoveSelection)
    }

    func testRevertRestoresTheStoredValues() {
        let (vm, _) = model()
        vm.select("opus")
        vm.draft?.displayName = "changed"
        XCTAssertTrue(vm.isDirty)
        vm.revert()
        XCTAssertEqual(vm.draft?.displayName, "Claude (Opus)")
        XCTAssertFalse(vm.isDirty)
    }

    func testApplyProfilesKeepsTheSelectionAndClearsADirtyDraftOnlyIfGone() {
        let (vm, _) = model()
        vm.select("opus")
        vm.draft?.displayName = "changed"
        // A broadcast caused by someone else's edit must not silently discard what the user typed.
        vm.apply(profiles: vm.profiles)
        XCTAssertEqual(vm.draft?.displayName, "changed")
        XCTAssertEqual(vm.selected, "opus")

        // ...but a profile that has gone away cannot stay selected.
        vm.apply(profiles: vm.profiles.filter { $0.id != "opus" })
        XCTAssertNil(vm.selected)
        XCTAssertNil(vm.draft)
    }

    func testASendFailureSurfacesAsAnError() {
        let (vm, rec) = model()
        rec.failNext = true
        vm.reload()
        XCTAssertNotNil(vm.lastError)
        vm.dismissError()
        XCTAssertNil(vm.lastError)
    }
}
