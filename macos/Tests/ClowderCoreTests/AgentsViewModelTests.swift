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

    func testDuplicatingAnIdAtTheLengthLimitStillYieldsAValidId() {
        // A 64-scalar id is legal, so it can exist. Appending "-copy" would push the duplicate past
        // the limit and hand the user a draft that is invalid the instant it appears.
        let long = String(repeating: "a", count: 64)
        let (vm, _) = model()
        vm.apply(profiles: [AgentProfileInfo(id: long, base: "claude", displayName: "Long",
                                             enabled: true, args: "", builtin: false)])
        vm.select(long)
        vm.duplicateSelected()

        let id = vm.draft?.id ?? ""
        XCTAssertNil(vm.draft?.idError, "duplicate must be valid as-is, got \(id.count) scalars: \(id)")
        XCTAssertLessThanOrEqual(id.unicodeScalars.count, 64)
        XCTAssertTrue(id.hasSuffix("-copy"), "still recognisably a copy: \(id)")
        XCTAssertNotEqual(id, long)
    }

    func testDuplicatingTwiceAtTheLengthLimitAvoidsACollision() {
        let long = String(repeating: "a", count: 64)
        let firstCopy = String(repeating: "a", count: 59) + "-copy"
        let (vm, _) = model()
        vm.apply(profiles: [
            AgentProfileInfo(id: long, base: "claude", displayName: "Long", enabled: true,
                             args: "", builtin: false),
            AgentProfileInfo(id: firstCopy, base: "claude", displayName: "Copy", enabled: true,
                             args: "", builtin: false),
        ])
        vm.select(long)
        vm.duplicateSelected()

        let id = vm.draft?.id ?? ""
        XCTAssertNil(vm.draft?.idError, "still valid: \(id)")
        XCTAssertNotEqual(id, firstCopy, "must not collide with the existing copy")
        XCTAssertLessThanOrEqual(id.unicodeScalars.count, 64)
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

    // MARK: - Fix round 1

    func testASuccessfulAddSelectsTheNewProfileOnceTheBroadcastArrives() {
        let (vm, rec) = model()
        vm.beginAdd()
        vm.draft?.id = "plan"
        vm.draft?.displayName = "Planner"
        vm.draft?.base = "claude"
        vm.save()
        guard case let .addAgentProfile(added)? = rec.sent.last else {
            return XCTFail("expected addAgentProfile, got \(rec.sent)")
        }
        vm.apply(profiles: vm.profiles + [added])
        XCTAssertEqual(vm.selected, "plan", "a confirmed add must select the new profile")
        XCTAssertEqual(vm.draft?.isNew, false, "the draft must stop being 'new' once it exists")
        XCTAssertFalse(vm.isDirty, "otherwise Save/Revert stay lit forever and a second Save re-adds")
    }

    func testReportErrorSetsLastErrorAndDismissClearsIt() {
        let (vm, _) = model()
        vm.reportError("agent id 'opus' already exists")
        XCTAssertEqual(vm.lastError, "agent id 'opus' already exists")
        vm.dismissError()
        XCTAssertNil(vm.lastError)
    }

    func testApplyAdoptsARemoteChangeWhenTheDraftIsNotDirty() {
        let (vm, _) = model()
        vm.select("opus")
        XCTAssertFalse(vm.isDirty)
        let changed = AgentProfileInfo(id: "opus", base: "claude",
                                       displayName: "Claude (Opus, renamed elsewhere)",
                                       enabled: true, args: "--model opus", builtin: false)
        var profiles = vm.profiles
        profiles[profiles.firstIndex(where: { $0.id == "opus" })!] = changed
        vm.apply(profiles: profiles)
        XCTAssertEqual(vm.draft?.displayName, "Claude (Opus, renamed elsewhere)",
                       "an undirtied draft should pick up a remote change")
        XCTAssertFalse(vm.isDirty)
    }

    func testSaveIgnoresAMutatedDraftIdForAnExistingProfile() {
        // The editor offers no UI to change `id` on a non-new draft, but `save()` must not simply
        // trust that: it sends the baseline's id, not whatever the draft happens to hold, so a
        // mutated draft id can never reach the wire and force the daemon to reject it instead.
        let (vm, rec) = model()
        vm.select("opus")
        vm.draft?.id = "not-opus"
        vm.draft?.args = "--model opus --verbose"
        vm.save()
        guard case let .updateAgentProfile(p)? = rec.sent.last else {
            return XCTFail("expected updateAgentProfile, got \(rec.sent)")
        }
        XCTAssertEqual(p.id, "opus", "the baseline id must be sent, not the mutated draft id")
        XCTAssertEqual(p.args, "--model opus --verbose", "other edited fields still go through")
    }

    func testApplyKeepsALocalEditWhenTheProfileAlsoChangedRemotely() {
        let (vm, _) = model()
        vm.select("opus")
        vm.draft?.displayName = "My local edit"
        XCTAssertTrue(vm.isDirty)
        let changedElsewhere = AgentProfileInfo(id: "opus", base: "claude",
                                                displayName: "Someone else's edit",
                                                enabled: true, args: "--model opus", builtin: false)
        var profiles = vm.profiles
        profiles[profiles.firstIndex(where: { $0.id == "opus" })!] = changedElsewhere
        vm.apply(profiles: profiles)
        XCTAssertEqual(vm.draft?.displayName, "My local edit",
                       "a dirty draft must not be clobbered by a concurrent remote edit")
    }
}
