// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

/// The sixth round's controller-level lifecycle ownership tests (R-E1F6-1 to R-E1F6-14):
/// one owner at a time, held across every phase of a workflow, released exactly once.
extension HubControllerTests {
    // MARK: helpers

    fileprivate typealias Rig = OwnerRig

    fileprivate func fleetCredentials() -> HubFleetSetupCredentials {
        HubFleetSetupCredentials(accessToken: "a", refreshToken: "r", clientID: "c",
                                 region: "europe_middle_east_and_africa", expiresInSeconds: 3600)
    }

    fileprivate func acquireForeign(_ rig: Rig, _ family: HubLifecycleFamily = .diagnostics) throws -> HubLifecycleToken {
        switch rig.controller.lifecycle.acquire(family) {
        case let .success(token): return token
        case let .failure(refusal): throw XCTSkip("could not take the foreign owner: \(refusal)")
        }
    }

    fileprivate func releaseForeign(_ rig: Rig, _ token: HubLifecycleToken) {
        rig.controller.lifecycle.settle(token) {}
    }

    // MARK: R-E1F6-1

    func testLifecycleAdmissionIsAtomicBeforeTheFirstEffect() throws {
        // Many threads ask at once: exactly one owner is admitted each round.
        let authority = HubLifecycleAuthority()
        for _ in 0..<25 {
            let lock = NSLock()
            var admitted: [HubLifecycleToken] = []
            var refused = 0
            DispatchQueue.concurrentPerform(iterations: 64) { index in
                let family = HubLifecycleFamily.allCases[index % HubLifecycleFamily.allCases.count]
                switch authority.acquire(family) {
                case let .success(token): lock.lock(); admitted.append(token); lock.unlock()
                case .failure: lock.lock(); refused += 1; lock.unlock()
                }
            }
            XCTAssertEqual(admitted.count, 1, "exactly one of 64 simultaneous requests is admitted")
            XCTAssertEqual(refused, 63)
            XCTAssertEqual(authority.capabilities.owner, admitted.first?.family)
            XCTAssertTrue(authority.settle(try XCTUnwrap(admitted.first)) {})
            XCTAssertTrue(authority.capabilities.isIdle)
        }

        // The history-only policy check of Start and Restart is part of the operation: with
        // another owner holding the lifecycle it must not run at all.
        let rig = try Rig(.init(loaded: .loaded, historyOnlyConfig: true))
        defer { rig.tearDown() }
        rig.service.hold("pause")
        var diagnostics = 0
        rig.controller.runFullDiagnostics { _ in diagnostics += 1 }
        XCTAssertEqual(rig.capabilities.owner, .diagnostics)
        let before = rig.signature()
        var outcomes: [Result<Void, Error>] = []
        rig.controller.startHub { outcomes.append($0) }
        rig.controller.restartHub { outcomes.append($0) }
        XCTAssertEqual(outcomes.count, 2, "both were answered at once")
        for outcome in outcomes {
            guard case let .failure(error) = outcome else { return XCTFail("admitted inside a diagnostics custody") }
            XCTAssertTrue(isLifecycleRefusal(error), error.localizedDescription)
        }
        XCTAssertEqual(rig.signature(), before, "the policy check, a command, a write: none ran")
        XCTAssertEqual(rig.runner.count("preflight:history"), 0)
        rig.drain()
        XCTAssertEqual(diagnostics, 1)
    }

    func testStaleTokenCannotAdvanceOrReleaseANewerOwner() throws {
        let authority = HubLifecycleAuthority()
        let first = try { () throws -> HubLifecycleToken in
            guard case let .success(token) = authority.acquire(.start) else { throw XCTSkip("no token") }
            return token
        }()
        XCTAssertTrue(authority.advance(first, to: "policy"))
        var delivered = 0
        XCTAssertTrue(authority.settle(first) { delivered += 1 })
        XCTAssertEqual(delivered, 1)
        XCTAssertFalse(authority.settle(first) { delivered += 1 }, "a duplicate settlement is ignored")
        XCTAssertEqual(delivered, 1)

        guard case let .success(second) = authority.acquire(.start) else { return XCTFail("not admitted when idle") }
        XCTAssertGreaterThan(second.generation, first.generation, "the generation advances")
        XCTAssertNotEqual(first, second, "the same family is still another operation")
        XCTAssertFalse(authority.advance(first, to: "late"), "a stale token cannot advance the newer owner")
        XCTAssertFalse(authority.beginChild(first), "a stale token cannot register a child")
        authority.endChild(first)
        XCTAssertFalse(authority.settle(first) { XCTFail("a stale settlement must deliver nothing") })
        XCTAssertFalse(authority.isCurrent(first))
        XCTAssertTrue(authority.isCurrent(second))
        XCTAssertEqual(authority.capabilities.owner, .start)
        XCTAssertNotEqual(authority.capabilities.phase, "late")
        guard case .failure(.busy(.start)) = authority.acquire(.stop) else {
            return XCTFail("the newer owner was released by the stale token")
        }
        XCTAssertTrue(authority.settle(second) {})
        XCTAssertTrue(authority.capabilities.isIdle)
        let released = authority.journal.filter { if case .released = $0 { return true } else { return false } }
        XCTAssertEqual(released.count, 2, "each owner is released exactly once")
    }

    // MARK: R-E1F6-2

    func testForbiddenOperationPairsNeverOverlap() throws {
        try runForbiddenPairMatrix()
    }

    // MARK: R-E1F6-3

    func testParentServiceChildrenUseOneOwnerWithoutReacquisition() throws {
        // Account setup (Pause, Start, recovery), diagnostics (Pause, Start) and handover
        // (Start) each call the service several times: every call carries the one owner.
        let rig = try Rig(.init(loaded: .loaded, marker: "awaiting_handover"))
        defer { rig.tearDown() }
        rig.controller.configureFleetAccount(credentials: fleetCredentials()) { _ in }
        rig.drain()
        let account = rig.service.owners
        XCTAssertGreaterThanOrEqual(account.count, 2, "pause and start at least")
        XCTAssertEqual(Set(account.map(\.id)).count, 1, "one owner for every child call")
        XCTAssertEqual(rig.acquisitions(.account), 1, "no child reacquired through a public API")
        XCTAssertEqual(rig.journal.filter { if case .acquired = $0 { return true } else { return false } }.count, 1)

        let diagnosticsRig = try Rig(.init(loaded: .loaded))
        defer { diagnosticsRig.tearDown() }
        diagnosticsRig.controller.runFullDiagnostics { _ in }
        diagnosticsRig.drain()
        XCTAssertEqual(Set(diagnosticsRig.service.owners.map(\.id)).count, 1)
        XCTAssertEqual(diagnosticsRig.acquisitions(.diagnostics), 1)
        XCTAssertEqual(diagnosticsRig.journal.filter { if case .acquired = $0 { return true } else { return false } }.count, 1)
        XCTAssertTrue(diagnosticsRig.service.calls.contains("run:pause"))
        XCTAssertTrue(diagnosticsRig.service.calls.contains("run:start"))
    }

    func testAnotherOperationCannotConsumeOrClearRestartDuty() throws {
        // The service controller binds the held record to the owner that made it.
        let rig = LifecycleRig(loaded: true)
        let mine = rig.owner
        let other = rig.otherOwner()
        var result: Result<String, Error>?
        rig.controller.run(arguments: ["service", "pause"], owner: mine) { result = $0 }
        rig.scheduler.advance(by: 0)
        XCTAssertNotNil(result)
        XCTAssertTrue(rig.controller.restartDutyOutstanding)
        XCTAssertTrue(rig.controller.restartRecordHeld(by: mine))
        XCTAssertFalse(rig.controller.restartRecordHeld(by: other))
        let commands = rig.launchctlEvents

        // Another owner can neither consume the duty ...
        var recovery: Result<String, Error>?
        rig.controller.recoverAfterPause(owner: other) { recovery = $0 }
        rig.scheduler.advance(by: 0)
        guard case let .success(note)? = recovery else { return XCTFail("a foreign recovery is a no-op") }
        XCTAssertEqual(note, "")
        XCTAssertEqual(rig.launchctlEvents, commands, "no recovery command was issued for another owner")
        XCTAssertTrue(rig.controller.restartDutyOutstanding, "the duty is still outstanding")

        // ... nor clear it, by release or by Stop, Remove, Start or Restart.
        rig.controller.releaseRestartRecord(owner: other)
        XCTAssertTrue(rig.controller.restartDutyOutstanding)
        for action in ["stop", "remove", "start", "restart", "pause"] {
            var outcome: Result<String, Error>?
            rig.controller.run(arguments: ["service", action], owner: other) { outcome = $0 }
            rig.scheduler.advance(by: 0)
            guard case .failure? = outcome else { return XCTFail("\(action) by another owner must be refused") }
            XCTAssertTrue(rig.controller.restartDutyOutstanding, action)
            XCTAssertEqual(rig.launchctlEvents, commands, "\(action) issued no command")
        }
        var reconcileOutcome: HubReconcileOutcome?
        rig.controller.reconcile(owner: other, ensure: { try rig.agent.ensureInstalled(requireConfig: false) }) {
            reconcileOutcome = $0
        }
        rig.scheduler.advance(by: 0)
        XCTAssertNotNil(reconcileOutcome)
        XCTAssertFalse(rig.events.contains("ensure"), "a foreign reconcile writes nothing while the duty is held")
        XCTAssertTrue(rig.controller.restartDutyOutstanding)

        // The owner can: its recovery runs the one typed attempt and clears the record.
        var own: Result<String, Error>?
        rig.controller.recoverAfterPause(owner: mine) { own = $0 }
        rig.scheduler.advance(by: 0)
        XCTAssertNotNil(own)
        XCTAssertFalse(rig.controller.restartDutyOutstanding)
        XCTAssertFalse(rig.controller.restartRecordHeld(by: mine))
    }

    // MARK: R-E1F6-4

    func testEveryLifecycleExitSettlesChildrenAndReleasesExactlyOnce() throws {
        struct Exit {
            let label: String
            let family: HubLifecycleFamily
            var options = Rig.Options(loaded: .loaded)
            let arrange: (Rig) -> Void
            let run: (Rig, @escaping () -> Void) -> Void
        }
        let credentials = fleetCredentials()
        let source = "postgresql://reader@127.0.0.1:55438/teslamate_snapshot"
        let exits: [Exit] = [
            Exit(label: "admission refused after acquisition (handover pending)", family: .start,
                 options: .init(marker: "awaiting_handover"), arrange: { _ in }) { rig, done in
                rig.controller.startHub { _ in done() }
            },
            Exit(label: "failed pause", family: .account, arrange: { $0.service.results["pause"] = .failure(HubActionError.commandFailed("pause")) }) { rig, done in
                rig.controller.configureFleetAccount(credentials: credentials) { _ in done() }
            },
            Exit(label: "child failure", family: .signOut, arrange: { $0.runner.results["control"] = .failure(HubActionError.commandFailed("control")) }) { rig, done in
                rig.controller.signOutTeslaAccount { _ in done() }
            },
            Exit(label: "child timeout", family: .signOut, arrange: { $0.runner.results["control"] = .failure(HubActionError.commandTimedOut) }) { rig, done in
                rig.controller.signOutTeslaAccount { _ in done() }
            },
            Exit(label: "stop failure before sign-out", family: .signOut, arrange: { $0.service.results["stop"] = .failure(HubActionError.commandFailed("stop")) }) { rig, done in
                rig.controller.signOutTeslaAccount { _ in done() }
            },
            Exit(label: "recovery after a failed resume", family: .diagnostics, arrange: { $0.service.results["start"] = .failure(HubActionError.commandFailed("start")) }) { rig, done in
                rig.controller.runFullDiagnostics { _ in done() }
            },
            Exit(label: "policy refusal", family: .diagnostics, options: .init(loaded: .loaded, historyOnlyConfig: true), arrange: { $0.runner.results["preflight:history"] = .failure(HubActionError.commandFailed("not ready")) }) { rig, done in
                rig.controller.runFullDiagnostics { _ in done() }
            },
            Exit(label: "migration child failure", family: .migration, options: .init(installed: false), arrange: { $0.runner.results["migrate:history"] = .failure(HubActionError.commandFailed("migrate")) }) { rig, done in
                rig.controller.importTeslaMateHistoryOnlyOnline(source: source, carID: "1", passwordFile: "/p",
                                                               acknowledgeV42CompatibleSchema: true) { _ in done() }
            },
            Exit(label: "readiness rollback", family: .handover, options: .init(marker: "awaiting_handover"), arrange: { $0.runner.results["status"] = .failure(HubActionError.commandFailed("status")) }) { rig, done in
                rig.controller.acknowledgeMigrationHandoverAndStart { _ in done() }
            },
            Exit(label: "final refresh", family: .signOut, arrange: { _ in }) { rig, done in
                rig.controller.signOutTeslaAccount { _ in done() }
            },
            Exit(label: "vehicle control validation", family: .vehicleControl, options: .init(snapshot: .firstRun), arrange: { _ in }) { rig, done in
                rig.controller.performVehicleControl(.lock) { _ in done() }
            }
        ]
        for exit in exits {
            let rig = try Rig(exit.options)
            defer { rig.tearDown() }
            exit.arrange(rig)
            var completions = 0
            exit.run(rig) { completions += 1 }
            rig.drain()
            spinUntil(2) { completions >= 1 }
            XCTAssertEqual(completions, 1, "\(exit.label): one completion")
            XCTAssertEqual(rig.acquisitions(exit.family), 1, "\(exit.label)")
            XCTAssertEqual(rig.releases(exit.family), 1, "\(exit.label): released exactly once")
            XCTAssertTrue(rig.capabilities.isIdle, "\(exit.label): the lifecycle is open again")
            XCTAssertTrue(rig.service.heldKeys.isEmpty && rig.runner.heldKeys.isEmpty, exit.label)
            XCTAssertFalse(rig.service.restartRecordHeld(by: rig.service.owners.last ?? makeLifecycleToken()),
                           "\(exit.label): no restart duty is left behind")
        }

        // Cancellation: a Tesla sign-in cancelled before its tokens arrive, and a migration
        // preparation whose window went away, both settle and release.
        let cancelRig = try Rig()
        defer { cancelRig.tearDown() }
        guard case let .success(signIn) = cancelRig.controller.beginLegacySignIn() else { return XCTFail("not admitted") }
        XCTAssertEqual(cancelRig.capabilities.owner, .account, "held before authentication begins")
        var cancelled = 0
        signIn.cancel { cancelled += 1 }
        signIn.cancel { cancelled += 1 }
        XCTAssertEqual(cancelled, 1 + 1, "a repeated cancel settles nothing twice")
        XCTAssertEqual(cancelRig.releases(.account), 1)
        XCTAssertTrue(cancelRig.capabilities.isIdle)
    }

    func testTimedOutChildRetainsOwnershipUntilReaped() throws {
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.signOut) else { return XCTFail("not admitted") }
        let runner = OwnerRunner()
        let operation = HubLifecycleOperation(token: token, authority: authority,
                                              commandRunner: runner, serviceRunner: OwnerService())
        runner.hold("control")
        var childResult: Result<String, Error>?
        operation.run(["--config", "/x/config.toml", "control", "sign-out"]) { childResult = $0 }
        XCTAssertEqual(authority.childCount(token), 1)

        var settled = 0
        operation.settle { settled += 1 }
        XCTAssertEqual(settled, 0, "the owner is not released while its child is still running")
        XCTAssertEqual(authority.capabilities.owner, .signOut)
        XCTAssertFalse(authority.advance(token, to: "late"), "a settling owner starts no further phase")
        operation.run(["--config", "/x/config.toml", "status"]) { _ in }
        XCTAssertEqual(runner.count("status"), 0, "no new child is launched once settlement has begun")
        guard case .failure(.busy(.signOut)) = authority.acquire(.start) else {
            return XCTFail("a new owner was admitted before the child was reaped")
        }

        // The child is reaped (its timeout termination has finished): only now it releases.
        runner.release("control")
        XCTAssertNotNil(childResult)
        XCTAssertEqual(settled, 1)
        XCTAssertTrue(authority.capabilities.isIdle)
        XCTAssertEqual(authority.journal.filter { if case .released = $0 { return true } else { return false } }.count, 1)
    }

    // MARK: F6-3

    func testAChildThatMayStillWriteIsReportedButKeepsItsOwnerUntilItIsGone() throws {
        // The executor could not show the child gone (not reaped, or its output still attached): the
        // failure is reported at once, but the owner is not released and nothing else is admitted until
        // the quiescence says the child can no longer write.
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.signOut) else { return XCTFail("not admitted") }
        let runner = OwnerRunner()
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        runner.results["control"] = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        let operation = HubLifecycleOperation(token: token, authority: authority,
                                              commandRunner: runner, serviceRunner: OwnerService())
        var childResult: Result<String, Error>?
        operation.run(["--config", "/x/config.toml", "control", "sign-out"]) { childResult = $0 }
        guard case let .failure(error)? = childResult, case HubActionError.commandNotQuiescent = error else {
            return XCTFail("the failure is reported to the caller at once: \(String(describing: childResult))")
        }
        XCTAssertEqual(authority.childCount(token), 1, "the child is still counted: it may still write")

        var settled = 0
        operation.settle { settled += 1 }
        XCTAssertEqual(settled, 0, "the owner is not released while the child cannot be shown gone")
        XCTAssertEqual(authority.capabilities.owner, .signOut)
        for family in [HubLifecycleFamily.delete, .start, .account] {
            guard case .failure(.busy(.signOut)) = authority.acquire(family) else {
                return XCTFail("\(family) was admitted while the child may still write")
            }
        }

        quiescence.childReaped()
        XCTAssertEqual(settled, 1, "released exactly once, when the child is gone")
        XCTAssertTrue(authority.capabilities.isIdle)
        XCTAssertEqual(authority.childCount(token), 0)
    }

    func testANotQuiescentFailureIsUnderstoodByEveryConsumerOfCommandErrors() {
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        let error = HubActionError.commandNotQuiescent(HubActionError.commandFailed("vehicle is offline"), quiescence)
        // A vehicle command whose process could not be stopped may still reach the vehicle: the outcome is unknown.
        XCTAssertTrue(MainWindowController.vehicleControlOutcomeIsUnknown(error))
        XCTAssertFalse(MainWindowController.vehicleControlOutcomeIsUnknown(HubActionError.commandFailed("vehicle is offline")))
        XCTAssertEqual(HubAppLog.errorCode(error), "command_not_quiescent")
        XCTAssertEqual(TeslaMateServerImporter.discoveryFailureReason(error), "process_not_stopped")
        XCTAssertTrue(error.localizedDescription.contains("could not be stopped"), error.localizedDescription)
        // F7-6: the migration failure log and the development event log read the underlying error.
        let timedOut = HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence)
        XCTAssertEqual(HubController.migrationFailureReasonCode(timedOut), "timeout")
        XCTAssertEqual(HubController.migrationFailureReasonCode(HubActionError.commandTimedOut), "timeout")
        let diskFull = HubActionError.commandNotQuiescent(HubActionError.commandFailed("no space left on device"), quiescence)
        XCTAssertEqual(HubController.migrationFailureReasonCode(diskFull), "disk_space")
        quiescence.childReaped()
    }

    // MARK: F7-1

    func testAHeldChildIsReleasedOnTheMainQueueWhenItIsReapedFromABackgroundQueue() throws {
        // The executor's termination handler runs on a Foundation background queue. The owner's release and
        // the window completion it triggers must still arrive on the main queue (window code is main-only).
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.signOut) else { return XCTFail("not admitted") }
        let runner = OwnerRunner()
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        runner.results["control"] = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        let operation = HubLifecycleOperation(token: token, authority: authority,
                                              commandRunner: runner, serviceRunner: OwnerService())
        operation.run(["--config", "/x/config.toml", "control", "sign-out"]) { _ in }
        let lock = NSLock()
        var deliveries: [Bool] = []
        operation.settle { lock.lock(); deliveries.append(Thread.isMainThread); lock.unlock() }
        XCTAssertTrue(deliveries.isEmpty, "not delivered while the child may still write")

        DispatchQueue.global(qos: .utility).async { quiescence.childReaped() }
        spinUntil(5) { lock.lock(); defer { lock.unlock() }; return !deliveries.isEmpty }
        spinBriefly(0.1)
        lock.lock(); let seen = deliveries; lock.unlock()
        XCTAssertEqual(seen, [true], "delivered exactly once, on the main thread")
        XCTAssertTrue(authority.capabilities.isIdle)
        XCTAssertEqual(authority.childCount(token), 0)
    }

    func testSignOutIsDeliveredOnceOnTheMainThreadWhenItsHeldChildIsReapedOffTheMainThread() throws {
        let rig = try Rig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        rig.runner.results["control"] = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        let lock = NSLock()
        var deliveries: [Bool] = []
        rig.controller.signOutTeslaAccount { _ in lock.lock(); deliveries.append(Thread.isMainThread); lock.unlock() }
        spinUntil(5) { rig.runner.count("control") == 1 && rig.runner.count("status") >= 1 }
        spinBriefly(0.2)
        lock.lock(); let early = deliveries; lock.unlock()
        XCTAssertTrue(early.isEmpty, "not delivered while the child may still write")

        DispatchQueue.global(qos: .utility).async { quiescence.childReaped() }
        spinUntil(5) { lock.lock(); defer { lock.unlock() }; return !deliveries.isEmpty }
        spinBriefly(0.2)
        lock.lock(); let seen = deliveries; lock.unlock()
        XCTAssertEqual(seen, [true], "the sign-out completion arrives exactly once, on the main thread")
        XCTAssertEqual(rig.releases(.signOut), 1)
        XCTAssertTrue(rig.capabilities.isIdle)
    }

    // MARK: F7-2

    func testAStartWhoseChecksCannotBeStoppedKeepsItsOwnerAndRefusesDeleteUntilTheChildIsReaped() throws {
        // The Hub CLI preflight inside Start ran out of time and its process could not be shown gone. The
        // service controller wraps that failure in its own message; the hold must survive the wrapper.
        let rig = LifecycleRig(loaded: false)
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        rig.preflightResult = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.start) else { return XCTFail("not admitted") }
        let operation = HubLifecycleOperation(token: token, authority: authority,
                                              commandRunner: OwnerRunner(), serviceRunner: rig.controller)
        var startResult: Result<String, Error>?
        operation.service(["service", "start"]) { startResult = $0 }
        rig.scheduler.advance(by: 0)
        guard case let .failure(error)? = startResult else { return XCTFail("the start did not fail: \(String(describing: startResult))") }
        XCTAssertTrue(error.localizedDescription.contains("could not be stopped"), error.localizedDescription)
        XCTAssertEqual(authority.childCount(token), 1, "the preflight child may still write: it stays counted")

        var settled = 0
        operation.settle { settled += 1 }
        XCTAssertEqual(settled, 0, "the Start owner is not released")
        guard case .failure(.busy(.start)) = authority.acquire(.delete) else {
            return XCTFail("Delete was admitted while the preflight child may still run")
        }

        quiescence.childReaped()
        XCTAssertEqual(settled, 1, "released exactly once, when the child is gone")
        XCTAssertTrue(authority.capabilities.isIdle)
        guard case .success = authority.acquire(.delete) else { return XCTFail("Delete after the child is gone") }
    }

    func testAServiceStateCheckThatCannotBeStoppedKeepsItsOwnerUntilTheChildIsReaped() throws {
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.diagnostics) else { return XCTFail("not admitted") }
        let service = OwnerService()
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        service.loaded = .unknown(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        let operation = HubLifecycleOperation(token: token, authority: authority,
                                              commandRunner: OwnerRunner(), serviceRunner: service)
        var states = 0
        operation.loadedState { _ in states += 1 }
        XCTAssertEqual(states, 1, "reported at once")
        XCTAssertEqual(authority.childCount(token), 1, "the launchctl child may still run")
        var settled = 0
        operation.settle { settled += 1 }
        XCTAssertEqual(settled, 0)
        quiescence.childReaped()
        XCTAssertEqual(settled, 1)
        XCTAssertTrue(authority.capabilities.isIdle)
    }

    func testDiscoveryFailureKeepsTheQuiescenceOfItsSSHProcess() {
        let diagnostic = TeslaMateSSHDiagnostic(reasonCode: "timed_out", title: "t", summary: "s", suggestions: [],
                                                recoveryActions: [.openLogs])
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        let stuck = HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence)
        let kept = TeslaMateServerImporter.discoveryFailure(stuck, diagnostic: diagnostic)
        guard case let HubActionError.commandNotQuiescent(inner, held) = kept else {
            return XCTFail("the diagnostic dropped the quiescence: \(kept)")
        }
        XCTAssertTrue(held === quiescence)
        XCTAssertEqual(inner as? TeslaMateSSHDiagnostic, diagnostic)
        // The window reads the diagnostic through the wrapper.
        XCTAssertEqual(TeslaMateServerImporter.connectionDiagnostic(for: kept, authentication: .key(identityFile: nil)), diagnostic)
        // A reaped process, and any other error, is the plain diagnostic.
        quiescence.childReaped()
        XCTAssertEqual(TeslaMateServerImporter.discoveryFailure(stuck, diagnostic: diagnostic) as? TeslaMateSSHDiagnostic, diagnostic)
        XCTAssertEqual(TeslaMateServerImporter.discoveryFailure(HubActionError.commandTimedOut, diagnostic: diagnostic) as? TeslaMateSSHDiagnostic,
                       diagnostic)
    }

    func testAnSSHPreparationKeepsItsOwnerWhileItsProcessMayStillRun() throws {
        let rig = try Rig()
        defer { rig.tearDown() }
        let diagnostic = TeslaMateSSHDiagnostic(reasonCode: "timed_out", title: "t", summary: "s", suggestions: [],
                                                recoveryActions: [.openLogs])
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        rig.connector.failure = HubActionError.commandNotQuiescent(diagnostic, quiescence)
        var outcomes: [Result<TeslaMateServerImportSession, Error>] = []
        _ = rig.controller.connectMigrationServer(host: "teslamate.local", user: "user", port: 22,
                                                  authentication: .key(identityFile: nil),
                                                  usePasswordlessSudo: false) { outcomes.append($0) }
        spinUntil(5) { rig.connector.callCount == 1 }
        spinBriefly(0.2)
        XCTAssertTrue(outcomes.isEmpty, "not reported while the SSH process may still run")
        XCTAssertEqual(rig.capabilities.owner, .migration)
        var refusal: Error?
        rig.controller.deleteHubAndData { if case let .failure(error) = $0 { refusal = error } }
        XCTAssertTrue(refusal.map(isLifecycleRefusal) ?? false, "Delete is refused: \(String(describing: refusal))")

        quiescence.childReaped()
        spinUntil(5) { !outcomes.isEmpty }
        XCTAssertEqual(outcomes.count, 1)
        guard case let .failure(error)? = outcomes.first else { return XCTFail("the failure is reported once the process is gone") }
        XCTAssertEqual(TeslaMateServerImporter.connectionDiagnostic(for: error, authentication: .key(identityFile: nil)), diagnostic)
        XCTAssertEqual(rig.releases(.migration), 1)
        XCTAssertTrue(rig.capabilities.isIdle)
    }

    // MARK: F7-3

    func testAPauseBasedOwnerRestartsTheHubAtSettlementOnlyOnceItsHeldChildIsGone() throws {
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.account) else { return XCTFail("not admitted") }
        let runner = OwnerRunner()
        let service = OwnerService()
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        runner.results["setup"] = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        let operation = HubLifecycleOperation(token: token, authority: authority, commandRunner: runner, serviceRunner: service)
        operation.service(["service", "pause"]) { _ in }
        XCTAssertTrue(service.restartRecordHeld(by: token), "the pause owes a start to this owner")
        operation.run(["--config", "/x/config.toml", "setup"]) { _ in }
        XCTAssertEqual(authority.childCount(token), 1, "the setup child may still write")

        var settled = 0
        operation.settle { settled += 1 }
        XCTAssertEqual(service.count("recover"), 0, "the Hub is not started again while the setup child may still write")
        XCTAssertEqual(settled, 0)
        XCTAssertEqual(authority.capabilities.owner, .account)

        quiescence.childReaped()
        XCTAssertEqual(service.count("recover"), 1, "the owed start runs once the child is gone")
        XCTAssertEqual(settled, 1)
        XCTAssertTrue(authority.capabilities.isIdle)
    }

    func testAnExplicitRecoveryAfterAPauseWaitsForTheHeldChildToo() throws {
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.diagnostics) else { return XCTFail("not admitted") }
        let runner = OwnerRunner()
        let service = OwnerService()
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        runner.results["doctor"] = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        let operation = HubLifecycleOperation(token: token, authority: authority, commandRunner: runner, serviceRunner: service)
        operation.service(["service", "pause"]) { _ in }
        operation.run(["--config", "/x/config.toml", "doctor"]) { _ in }
        // A second child of the same owner is still running.
        runner.hold("status")
        operation.run(["--config", "/x/config.toml", "status"]) { _ in }
        XCTAssertEqual(authority.childCount(token), 2)
        var recovered: [Result<String, Error>] = []
        operation.recoverAfterPause { recovered.append($0) }
        XCTAssertEqual(service.count("recover"), 0, "no start while the doctor child may still write")
        XCTAssertTrue(recovered.isEmpty)

        quiescence.childReaped()
        XCTAssertEqual(service.count("recover"), 0, "nor while another child of the owner is still running")
        XCTAssertTrue(recovered.isEmpty)
        XCTAssertTrue(runner.release("status"))
        XCTAssertEqual(service.count("recover"), 1, "the owed start runs when the last other child has gone")
        XCTAssertEqual(recovered.count, 1)
        operation.settle {}
        XCTAssertTrue(authority.capabilities.isIdle)
    }

    // MARK: F7-4

    func testAHeldChildIsVisibleInTheCapabilitiesAndHasItsOwnQuitMessage() throws {
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.signOut) else { return XCTFail("not admitted") }
        let runner = OwnerRunner()
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        runner.results["control"] = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        var published: [HubLifecycleCapabilities] = []
        let observation = authority.observe { published.append($0) }
        defer { observation.invalidate() }
        let operation = HubLifecycleOperation(token: token, authority: authority,
                                              commandRunner: runner, serviceRunner: OwnerService())
        XCTAssertFalse(authority.capabilities.childHeld)
        let ordinary = HubApplication.quitRefusalText(for: authority.capabilities)
        operation.run(["--config", "/x/config.toml", "control", "sign-out"]) { _ in }
        operation.settle {}
        XCTAssertTrue(authority.capabilities.childHeld, "a child that cannot be shown gone is visible to Quit")
        XCTAssertEqual(authority.capabilities.owner, .signOut)
        XCTAssertTrue(authority.capabilities.preventsQuit)
        spinUntil(2) { published.contains { $0.childHeld } }
        XCTAssertTrue(published.contains { $0.childHeld }, "the interface is told when the state begins")
        let held = HubApplication.quitRefusalText(for: authority.capabilities)
        XCTAssertNotEqual(held.title, ordinary.title)
        XCTAssertNotEqual(held.detail, ordinary.detail)
        XCTAssertTrue(held.detail.contains("stuck helper process"), held.detail)
        XCTAssertTrue(ordinary.title.contains("finishing an operation"), ordinary.title)

        quiescence.childReaped()
        XCTAssertFalse(authority.capabilities.childHeld)
        XCTAssertTrue(authority.capabilities.isIdle)
        spinUntil(2) { published.last?.childHeld == false && published.last?.owner == nil }
        XCTAssertEqual(published.last?.childHeld, false, "and when it ends")
    }

    func testAReapedHeldChildStopsBeingHeldWhileItsOwnerIsStillWorking() throws {
        // The child is gone before the owner settles: the state is no longer "waiting for a helper process",
        // although the owner (and the generic Quit refusal) remain.
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.signOut) else { return XCTFail("not admitted") }
        let runner = OwnerRunner()
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        runner.results["control"] = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        var published: [HubLifecycleCapabilities] = []
        let observation = authority.observe { published.append($0) }
        defer { observation.invalidate() }
        let operation = HubLifecycleOperation(token: token, authority: authority,
                                              commandRunner: runner, serviceRunner: OwnerService())
        operation.run(["--config", "/x/config.toml", "control", "sign-out"]) { _ in }
        XCTAssertTrue(authority.capabilities.childHeld)
        let heldPublication = authority.capabilities.publication

        quiescence.childReaped()
        XCTAssertFalse(authority.capabilities.childHeld, "the reaped child is no longer held")
        XCTAssertEqual(authority.capabilities.owner, .signOut, "the owner has not settled")
        XCTAssertEqual(authority.childCount(token), 0)
        XCTAssertGreaterThan(authority.capabilities.publication, heldPublication)
        spinUntil(2) { published.last?.childHeld == false }
        XCTAssertEqual(published.last?.childHeld, false, "the interface is told")
        XCTAssertEqual(published.last?.owner, .signOut)
        XCTAssertEqual(HubApplication.quitRefusalText(for: authority.capabilities).title, "Hub is finishing an operation")
        operation.settle {}
        XCTAssertTrue(authority.capabilities.isIdle)
    }

    func testAnOrdinaryFailureStillEndsTheChildAtOnce() throws {
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.signOut) else { return XCTFail("not admitted") }
        let runner = OwnerRunner()
        runner.results["control"] = .failure(HubActionError.commandTimedOut)
        let operation = HubLifecycleOperation(token: token, authority: authority,
                                              commandRunner: runner, serviceRunner: OwnerService())
        var childResult: Result<String, Error>?
        operation.run(["--config", "/x/config.toml", "control", "sign-out"]) { childResult = $0 }
        XCTAssertNotNil(childResult)
        XCTAssertEqual(authority.childCount(token), 0, "a reaped child is released with its result")
        // A "not quiescent" error whose child has in fact gone does not hold anything either.
        let quiet = HubChildQuiescence()
        runner.results["status"] = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiet))
        var second: Result<String, Error>?
        operation.run(["--config", "/x/config.toml", "status"]) { second = $0 }
        XCTAssertNotNil(second)
        XCTAssertEqual(authority.childCount(token), 0, "an already quiescent child is released at once")
    }

    func testSignOutKeepsItsOwnerWhileTheControlChildMayStillWrite() throws {
        let rig = try Rig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        rig.runner.results["control"] = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        var outcome: Result<Void, Error>?
        rig.controller.signOutTeslaAccount { outcome = $0 }
        spinUntil(5) { rig.runner.count("control") == 1 && rig.runner.count("status") >= 1 }
        XCTAssertEqual(rig.runner.count("control"), 1, "the control command ran")
        spinBriefly(0.2)
        XCTAssertNil(outcome, "the sign-out is not delivered while its child may still write")
        XCTAssertEqual(rig.capabilities.owner, .signOut)
        var refusal: Error?
        rig.controller.deleteHubAndData { if case let .failure(error) = $0 { refusal = error } }
        XCTAssertTrue(refusal.map(isLifecycleRefusal) ?? false, "Delete is refused: \(String(describing: refusal))")
        XCTAssertEqual(rig.service.count("delete"), 0)

        quiescence.childReaped()
        spinUntil(5) { outcome != nil }
        guard case .failure? = outcome else { return XCTFail("the failed sign-out is delivered once the child is gone") }
        XCTAssertEqual(rig.releases(.signOut), 1)
        XCTAssertTrue(rig.capabilities.isIdle)
        var deleted: Result<HubDataRemovalReport, Error>?
        rig.controller.deleteHubAndData { deleted = $0 }
        spinUntil(5) { deleted != nil }
        guard case .success? = deleted else { return XCTFail("Delete after the child is gone: \(String(describing: deleted))") }
    }

    func testSettlingAnOwnerThatOwesARestartRecoversBeforeItReleases() throws {
        // Cancelling or finishing early never abandons a restart still owed to the owner: the recovery runs
        // first, and only then is the lifecycle released.
        let rig = LifecycleRig(loaded: true)
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.diagnostics) else { return XCTFail("a fresh authority admits") }
        struct SilentRunner: HubCommandRunning {
            func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) { completion(.success("")) }
        }
        let operation = HubLifecycleOperation(token: token, authority: authority,
                                              commandRunner: SilentRunner(), serviceRunner: rig.controller)
        var paused: Result<String, Error>?
        rig.controller.run(arguments: ["service", "pause"], owner: token) { paused = $0 }
        rig.scheduler.advance(by: 0)
        XCTAssertNotNil(paused)
        XCTAssertTrue(rig.controller.restartRecordHeld(by: token), "the pause owes a start to this owner")
        let commandsBefore = rig.launchctlEvents

        var delivered = 0
        operation.settle { delivered += 1 }
        rig.scheduler.advance(by: 0)
        XCTAssertGreaterThan(rig.launchctlEvents.count, commandsBefore.count, "the owed start was attempted at settlement")
        XCTAssertFalse(rig.controller.restartDutyOutstanding, "the duty is settled before the release")
        XCTAssertEqual(delivered, 1)
        XCTAssertTrue(authority.capabilities.isIdle, "released exactly once, after the recovery")
    }

    func testAClosingLifecycleRefusesNewWorkEvenWithNoOwner() throws {
        let authority = HubLifecycleAuthority()
        authority.beginClosing()
        guard case let .failure(refusal) = authority.acquire(.start) else { return XCTFail("closing admitted new work") }
        XCTAssertEqual(refusal, .closing)
    }

    // MARK: F6-4

    func testEnteringClosingDecidesForegroundOwnershipAndTransitionUnderOneLock() throws {
        // A foreground owner holds the lifecycle: Quit cannot enter closing, and nothing changes.
        let held = HubLifecycleAuthority()
        guard case let .success(token) = held.acquire(.signOut) else { return XCTFail("not admitted") }
        XCTAssertEqual(held.beginClosing(), .refused)
        XCTAssertEqual(held.capabilities.state, .open, "a refused entry leaves the lifecycle open")
        XCTAssertEqual(held.capabilities.owner, .signOut)
        held.settle(token) {}
        XCTAssertEqual(held.beginClosing(), .entered(ownerHeld: false))
        XCTAssertEqual(held.capabilities.state, .closing)
        XCTAssertEqual(held.beginClosing(), .alreadyClosing, "a second Quit changes nothing")

        // A background owner does not refuse: Quit enters closing and waits for it.
        let background = HubLifecycleAuthority()
        guard case let .success(reconcile) = background.acquire(.reconcile) else { return XCTFail("not admitted") }
        XCTAssertEqual(background.beginClosing(), .entered(ownerHeld: true))
        XCTAssertEqual(background.capabilities.state, .closing)
        background.settle(reconcile) {}

        // Delete before its data step refuses; after it, Quit is the way out.
        let deleting = HubLifecycleAuthority()
        guard case let .success(delete) = deleting.acquire(.delete) else { return XCTFail("not admitted") }
        XCTAssertEqual(deleting.beginClosing(), .refused)
        XCTAssertEqual(deleting.capabilities.state, .deleting)
        deleting.settle(delete, terminal: .deleted) {}
        XCTAssertEqual(deleting.capabilities.state, .deleted)
        XCTAssertEqual(deleting.beginClosing(), .entered(ownerHeld: false))
    }

    func testNoForegroundOwnerIsEverHeldTogetherWithAnAcceptedClosing() throws {
        // Many interleavings of "admit a foreground owner" and "enter closing": the two outcomes that
        // can both succeed are never both successes.
        for iteration in 0..<400 {
            let authority = HubLifecycleAuthority()
            let lock = NSLock()
            var admitted: HubLifecycleToken?
            var entry: HubLifecycleAuthority.ClosingEntry?
            DispatchQueue.concurrentPerform(iterations: 2) { index in
                if index == 0 {
                    if case let .success(token) = authority.acquire(.signOut) { lock.lock(); admitted = token; lock.unlock() }
                } else {
                    let result = authority.beginClosing()
                    lock.lock(); entry = result; lock.unlock()
                }
            }
            if admitted != nil {
                XCTAssertEqual(entry, .refused, "iteration \(iteration): closing was accepted over a foreground owner")
                XCTAssertEqual(authority.capabilities.state, .open, "iteration \(iteration)")
            } else {
                XCTAssertEqual(entry, .entered(ownerHeld: false), "iteration \(iteration)")
                XCTAssertEqual(authority.capabilities.state, .closing, "iteration \(iteration)")
            }
        }
    }

    func testASettlingOwnerRefusesANewChildWhileItWaitsForTheLastOne() throws {
        // Settlement waits for the outstanding child; a late callback in that window must not register
        // (and so launch) another phase, and the release still happens exactly once when the child ends.
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.start) else { return XCTFail("a fresh authority admits") }
        XCTAssertTrue(authority.beginChild(token))
        var delivered = 0
        XCTAssertTrue(authority.settle(token) { delivered += 1 })
        XCTAssertEqual(authority.childCount(token), 1, "the outstanding child holds the release")
        XCTAssertEqual(delivered, 0)
        XCTAssertFalse(authority.beginChild(token), "a settling owner registers no further child")
        XCTAssertEqual(authority.childCount(token), 1)
        authority.endChild(token)
        XCTAssertEqual(delivered, 1, "delivered once, after the last child")
        XCTAssertTrue(authority.capabilities.isIdle)
    }

    func testLateCallbackCannotLaunchWorkAfterSettlement() throws {
        // A child's completion is delivered twice, the second time after the owner has gone
        // and another one holds the lifecycle.
        let rig = try Rig()
        defer { rig.tearDown() }
        rig.runner.duplicates = ["control"]
        var completions = 0
        rig.controller.signOutTeslaAccount { _ in completions += 1 }
        rig.drain()
        spinUntil(2) { completions >= 1 }
        XCTAssertEqual(completions, 1, "the duplicate callback completed nothing twice")
        XCTAssertEqual(rig.releases(.signOut), 1)

        let newer = try acquireForeign(rig, .start)
        let commandsBefore = rig.runner.names
        // A late duplicate: the child's completion delivered again now.
        rig.runner.hold("control")
        let operation = HubLifecycleOperation(token: newer, authority: rig.controller.lifecycle,
                                              commandRunner: rig.runner, serviceRunner: rig.service)
        var late = 0
        operation.run(["--config", rig.configPath.path, "control", "sign-out"]) { _ in late += 1 }
        rig.runner.duplicates = ["control"]
        rig.runner.release("control")
        XCTAssertEqual(late, 1, "the child ends once even when its callback repeats")
        XCTAssertEqual(rig.runner.names.count, commandsBefore.count + 1, "only the one command was launched")
        XCTAssertTrue(rig.controller.lifecycle.isCurrent(newer), "the newer owner is untouched")
        XCTAssertEqual(rig.capabilities.owner, .start)
        XCTAssertEqual(rig.releases(.signOut), 1, "the first owner was not released a second time")

        // After the newer owner settles, an operation of the old token launches nothing.
        releaseForeign(rig, newer)
        let beforeStale = rig.runner.names
        var staleOutcome: Result<String, Error>?
        operation.run(["--config", rig.configPath.path, "status"]) { staleOutcome = $0 }
        guard case .failure? = staleOutcome else { return XCTFail("a settled operation launched a child") }
        XCTAssertEqual(rig.runner.names, beforeStale)
    }

    // MARK: R-E1F6-6

    func testSecondDiagnosticsCannotEnterAnExistingCustody() throws {
        let rig = try Rig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        rig.service.hold("pause")
        var reports: [String] = []
        rig.controller.runFullDiagnostics { _ in reports.append("first") }
        XCTAssertEqual(rig.service.heldKeys, ["pause"])
        var second: String?
        rig.controller.runFullDiagnostics { second = $0 }
        XCTAssertNotNil(second, "the second run is answered at once")
        XCTAssertTrue(second?.contains("not run") == true, second ?? "")
        XCTAssertEqual(rig.service.count("run:pause"), 1, "the service was not paused again")
        XCTAssertEqual(rig.service.count("loaded"), 1, "no second state query")
        rig.drain()
        XCTAssertEqual(reports, ["first"])
        XCTAssertEqual(rig.releases(.diagnostics), 1)
    }

    // MARK: R-E1F6-7

    func testDeleteIsRefusedDuringSignOutStopControlAndRefresh() throws {
        for boundary in ["stop", "control", "refresh"] {
            let rig = try Rig(.init(loaded: .loaded))
            defer { rig.tearDown() }
            switch boundary {
            case "stop": rig.service.hold("stop")
            case "control": rig.runner.hold("control")
            default: rig.runner.hold("status")
            }
            var signedOut = 0
            rig.controller.signOutTeslaAccount { _ in signedOut += 1 }
            spinUntil(3) { !(rig.service.heldKeys + rig.runner.heldKeys).isEmpty }
            let held = rig.service.heldKeys + rig.runner.heldKeys
            XCTAssertEqual(held.count, 1, "\(boundary): sign-out is held at its boundary: \(held)")
            XCTAssertEqual(rig.capabilities.owner, .signOut, boundary)

            let before = rig.signature()
            var refusal: Error?
            rig.controller.deleteHubAndData { if case let .failure(error) = $0 { refusal = error } }
            XCTAssertNotNil(refusal, "\(boundary): Delete was answered at once")
            XCTAssertTrue(refusal.map(isLifecycleRefusal) ?? false, "\(boundary): \(refusal?.localizedDescription ?? "")")
            XCTAssertEqual(rig.signature(), before, "\(boundary): Delete changed nothing")
            XCTAssertEqual(rig.service.count("delete"), 0)

            rig.drain()
            XCTAssertEqual(signedOut, 1, boundary)
            XCTAssertEqual(rig.releases(.signOut), 1, boundary)
            // Settled: Delete is admitted again.
            var deleted: Result<HubDataRemovalReport, Error>?
            rig.controller.deleteHubAndData { deleted = $0 }
            spinUntil(5) { deleted != nil }
            guard case .success? = deleted else { return XCTFail("\(boundary): Delete after settlement: \(String(describing: deleted))") }
        }
    }

    func testSignOutIsRefusedDuringDelete() throws {
        let rig = try Rig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        rig.service.hold("delete")
        var deleteDone = 0
        rig.controller.deleteHubAndData { _ in deleteDone += 1 }
        spinUntil { rig.service.heldKeys.contains("delete") }
        XCTAssertEqual(rig.capabilities.state, .deleting)
        let before = rig.signature()
        var outcome: Result<Void, Error>?
        rig.controller.signOutTeslaAccount { outcome = $0 }
        guard case let .failure(error)? = outcome else { return XCTFail("sign-out was admitted during Delete") }
        XCTAssertTrue(isLifecycleRefusal(error), error.localizedDescription)
        XCTAssertEqual(rig.signature(), before, "no Stop, no control command")
        rig.drain()
        XCTAssertEqual(deleteDone, 1)
        XCTAssertEqual(rig.capabilities.state, .deleted)
    }

    // MARK: R-E1F6-8

    func testAccountSetupOwnsPauseConfigurationSetupStartAndRecovery() throws {
        for boundary in ["pause", "setup-fleet", "start"] {
            let rig = try Rig(.init(loaded: .loaded))
            defer { rig.tearDown() }
            switch boundary {
            case "pause": rig.service.hold("pause")
            case "setup-fleet": rig.runner.hold("setup-fleet")
            default: rig.service.hold("start")
            }
            var done = 0
            rig.controller.configureFleetAccount(credentials: fleetCredentials()) { _ in done += 1 }
            XCTAssertEqual((rig.service.heldKeys + rig.runner.heldKeys).count, 1, boundary)
            XCTAssertEqual(rig.capabilities.owner, .account, boundary)
            let before = rig.signature()
            var refusals = 0
            rig.controller.deleteHubAndData { if case .failure = $0 { refusals += 1 } }
            rig.controller.stopHub { if case .failure = $0 { refusals += 1 } }
            rig.controller.configureFleetAccount(credentials: fleetCredentials()) { if case .failure = $0 { refusals += 1 } }
            rig.controller.runFullDiagnostics { if $0.contains("not run") { refusals += 1 } }
            XCTAssertEqual(refusals, 4, boundary)
            XCTAssertEqual(rig.signature(), before, "\(boundary): the refused requests changed nothing")
            rig.drain()
            XCTAssertEqual(done, 1, boundary)
            XCTAssertEqual(rig.releases(.account), 1, boundary)
            XCTAssertFalse(rig.service.restartRecordHeld(by: rig.service.owners.last ?? makeLifecycleToken()))
        }
        // The recovery after a failed setup stays inside the same owner.
        let rig = try Rig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        rig.runner.results["setup-fleet"] = .failure(HubActionError.commandFailed("setup failed"))
        rig.controller.configureFleetAccount(credentials: fleetCredentials()) { _ in }
        rig.drain()
        XCTAssertTrue(rig.service.calls.contains("recover"))
        XCTAssertEqual(rig.acquisitions(.account), 1)
        XCTAssertEqual(Set(rig.service.owners.map(\.id)).count, 1)
    }

    func testFirstRunSetupAcquiresBeforeCreatingConfiguration() throws {
        let rig = try Rig(.init(installed: false, config: false))
        defer { rig.tearDown() }
        let foreign = try acquireForeign(rig)
        var outcomes: [Result<Void, Error>] = []
        rig.controller.configureFleetAccount(credentials: fleetCredentials()) { outcomes.append($0) }
        rig.controller.configureTeslaAccount(tokens: TeslaAuthTokens(accessToken: "a", refreshToken: "r")) { outcomes.append($0) }
        XCTAssertEqual(outcomes.count, 2)
        XCTAssertTrue(outcomes.allSatisfy { if case .failure = $0 { return true } else { return false } })
        XCTAssertFalse(FileManager.default.fileExists(atPath: rig.configPath.path), "no configuration was created")
        XCTAssertFalse(FileManager.default.fileExists(atPath: rig.configPath.deletingLastPathComponent().path),
                       "no data folder was created")
        XCTAssertTrue(rig.runner.names.isEmpty && rig.service.calls.isEmpty)

        releaseForeign(rig, foreign)
        var done: Result<Void, Error>?
        rig.controller.configureFleetAccount(credentials: fleetCredentials()) { done = $0 }
        rig.drain()
        spinUntil { done != nil }
        XCTAssertTrue(FileManager.default.fileExists(atPath: rig.configPath.path), "admitted once the owner was free")
        XCTAssertTrue(rig.runner.names.contains("setup-fleet"))
    }

    // MARK: R-E1F6-9

    func testMigrationAcquiresBeforePreparationOrMarkerWrites() throws {
        let rig = try Rig(.init(installed: false))
        defer { rig.tearDown() }
        let foreign = try acquireForeign(rig)
        let before = rig.signature()
        var refused = 0
        let source = "postgresql://reader@127.0.0.1:55438/teslamate_snapshot"
        let online = "postgresql://reader@teslamate.local/teslamate"
        rig.controller.checkTeslaMateCompatibility(source: source, carID: "1", passwordFile: "/p",
                                                   acknowledgeV42CompatibleSchema: true, historyOnly: true) {
            if case .failure = $0 { refused += 1 }
        }
        rig.controller.importTeslaMateHistoryOnlyOnline(source: source, carID: "1", passwordFile: "/p",
                                                        acknowledgeV42CompatibleSchema: true) {
            if case .failure = $0 { refused += 1 }
        }
        rig.controller.importTeslaMateOnline(source: online, carID: "1", passwordFile: "/p", encryptionKeyFile: "/k",
                                             acknowledgeV42CompatibleSchema: true) {
            if case .failure = $0 { refused += 1 }
        }
        let preparation = rig.controller.connectMigrationServer(host: "teslamate.local", user: "user", port: 22,
                                                                authentication: .key(identityFile: nil),
                                                                usePasswordlessSudo: false) {
            if case .failure = $0 { refused += 1 }
        }
        XCTAssertNil(preparation, "a refused preparation has no handle")
        XCTAssertEqual(refused, 4)
        XCTAssertEqual(rig.signature(), before, "no command, no marker, no configuration, no tunnel")
        XCTAssertEqual(rig.connector.callCount, 0)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: rig.connector.root.path), [],
                       "no temporary directory was created")
        XCTAssertFalse(rig.markerExists)

        releaseForeign(rig, foreign)
        var connected: Result<TeslaMateServerImportSession, Error>?
        _ = rig.controller.connectMigrationServer(host: "teslamate.local", user: "user", port: 22,
                                                  authentication: .key(identityFile: nil),
                                                  usePasswordlessSudo: false) { connected = $0 }
        spinUntil { connected != nil }
        XCTAssertEqual(rig.connector.callCount, 1)
        guard case let .success(session)? = connected else { return XCTFail("not admitted when free") }
        // The prepared session is idle and registered: it does not keep the lifecycle busy.
        XCTAssertTrue(rig.capabilities.isIdle)
        XCTAssertEqual(rig.controller.registeredMigrationSessionCount, 1)
        rig.controller.closeMigrationSession(session)
        XCTAssertEqual(rig.controller.registeredMigrationSessionCount, 0)
        XCTAssertFalse(FileManager.default.fileExists(atPath: try XCTUnwrap(rig.connector.directories.first).path),
                       "the controller removed the temporary directory")
    }

    // MARK: R-E1F6-10

    func testVerificationOwnsStopChecksAndMarkerSettlement() throws {
        for boundary in ["stop", "doctor", "marker"] {
            let rig = try Rig(.init(loaded: .loaded, marker: "awaiting_verification", markerInterval: 60))
            defer { rig.tearDown() }
            switch boundary {
            case "stop": rig.service.hold("stop")
            case "doctor": rig.runner.hold("doctor")
            default: rig.runner.hold("status")
            }
            var done = 0
            rig.controller.runOnboardingChecks(expectRunning: false) { _ in done += 1 }
            spinUntil(3) { !(rig.service.heldKeys + rig.runner.heldKeys).isEmpty }
            XCTAssertEqual((rig.service.heldKeys + rig.runner.heldKeys).count, 1, boundary)
            XCTAssertEqual(rig.capabilities.owner, .verification, boundary)
            let before = rig.signature()
            var refused = 0
            rig.controller.deleteHubAndData { if case .failure = $0 { refused += 1 } }
            rig.controller.startHub { if case .failure = $0 { refused += 1 } }
            rig.controller.runOnboardingChecks(expectRunning: false) { if case .failure = $0 { refused += 1 } }
            rig.controller.acknowledgeMigrationHandoverAndStart { if case .failure = $0 { refused += 1 } }
            XCTAssertEqual(refused, 4, boundary)
            XCTAssertEqual(rig.signature(), before, boundary)
            rig.drain()
            spinUntil(2) { done >= 1 }
            XCTAssertEqual(done, 1, boundary)
            XCTAssertEqual(rig.releases(.verification), 1, boundary)
        }
    }

    func testHandoverOwnsReadinessCleanupAndRollback() throws {
        // Start, readiness and rollback are all inside the one owner.
        for boundary in ["start", "readiness", "rollback"] {
            let rig = try Rig(.init(loaded: .loaded, marker: "awaiting_handover", markerInterval: 0))
            defer { rig.tearDown() }
            var outcome: Result<Void, Error>?
            switch boundary {
            case "start":
                rig.service.hold("start")
                rig.controller.acknowledgeMigrationHandoverAndStart { outcome = $0 }
            case "readiness":
                // The first status read is the baseline; the second is the readiness poll.
                rig.runner.hold("status")
                rig.controller.acknowledgeMigrationHandoverAndStart { outcome = $0 }
                XCTAssertEqual(rig.runner.heldKeys, ["status"], "held at the baseline read")
                rig.runner.hold("status")
                rig.runner.release("status")
            default:
                rig.runner.results["status"] = .failure(HubActionError.commandFailed("not ready"))
                rig.service.hold("stop")
                rig.controller.acknowledgeMigrationHandoverAndStart { outcome = $0 }
            }
            XCTAssertEqual((rig.service.heldKeys + rig.runner.heldKeys).count, 1, "\(boundary): held at its boundary")
            XCTAssertEqual(rig.capabilities.owner, .handover, boundary)
            XCTAssertNil(outcome, "\(boundary): the handover has not finished")
            let before = rig.signature()
            var refused = 0
            rig.controller.deleteHubAndData { if case .failure = $0 { refused += 1 } }
            rig.controller.stopHub { if case .failure = $0 { refused += 1 } }
            rig.controller.runFullDiagnostics { if $0.contains("not run") { refused += 1 } }
            rig.controller.signOutTeslaAccount { if case .failure = $0 { refused += 1 } }
            XCTAssertEqual(refused, 4, boundary)
            XCTAssertEqual(rig.signature(), before, "\(boundary): nothing ran while the handover held the lifecycle")
            XCTAssertEqual(rig.releases(.handover), 0, boundary)
            rig.drain()
            spinUntil(3) { outcome != nil }
            XCTAssertNotNil(outcome, boundary)
            XCTAssertEqual(rig.releases(.handover), 1, boundary)
        }
    }

    func testHandoverFailureReleasesOnlyAfterRollbackSettles() throws {
        let rig = try Rig(.init(loaded: .loaded, marker: "awaiting_handover", markerInterval: 0))
        defer { rig.tearDown() }
        rig.runner.results["status"] = .failure(HubActionError.commandFailed("not ready"))
        rig.service.hold("stop")
        var outcome: Result<Void, Error>?
        rig.controller.acknowledgeMigrationHandoverAndStart { outcome = $0 }
        spinUntil(3) { rig.service.heldKeys.contains("stop") }
        XCTAssertTrue(rig.service.heldKeys.contains("stop"), "rollback is held at its Stop")
        XCTAssertEqual(rig.capabilities.owner, .handover)
        XCTAssertNil(outcome)
        XCTAssertEqual(rig.releases(.handover), 0, "not released while the rollback is unsettled")
        rig.service.release("stop")
        spinUntil(3) { outcome != nil }
        guard case let .failure(error)? = outcome else { return XCTFail("the handover reported success") }
        XCTAssertTrue(error.localizedDescription.contains("stopped"), error.localizedDescription)
        XCTAssertEqual(rig.releases(.handover), 1)
        XCTAssertTrue(rig.capabilities.isIdle, "released by the time the interface hears of it")
        XCTAssertTrue(rig.markerExists, "the migration handover remains pending")
    }

    // MARK: R-E1F6-11

    func testVehicleControlAndLifecycleOwnersExcludeEachOther() throws {
        // Recording effects only: the runner double accepts the command; nothing is sent.
        let rig = try Rig(.init(loaded: .loaded, snapshot: .previewRunning))
        defer { rig.tearDown() }
        rig.runner.hold("control")
        var controlled = 0
        rig.controller.performVehicleControl(.lock) { _ in controlled += 1 }
        XCTAssertEqual(rig.runner.heldKeys, ["control"])
        XCTAssertEqual(rig.capabilities.owner, .vehicleControl)
        let generation = rig.controller.snapshot.controlVehicleID
        let before = rig.signature()
        var refused = 0
        rig.controller.stopHub { if case .failure = $0 { refused += 1 } }
        rig.controller.startHub { if case .failure = $0 { refused += 1 } }
        rig.controller.restartHub { if case .failure = $0 { refused += 1 } }
        rig.controller.signOutTeslaAccount { if case .failure = $0 { refused += 1 } }
        rig.controller.configureFleetAccount(credentials: fleetCredentials()) { if case .failure = $0 { refused += 1 } }
        rig.controller.deleteHubAndData { if case .failure = $0 { refused += 1 } }
        rig.controller.runFullDiagnostics { if $0.contains("not run") { refused += 1 } }
        rig.controller.performVehicleControl(.unlock) { if case .failure = $0 { refused += 1 } }
        XCTAssertEqual(refused, 8)
        XCTAssertEqual(rig.signature(), before, "no other owner changed the accepted generation")
        XCTAssertEqual(rig.controller.snapshot.controlVehicleID, generation)
        rig.drain()
        XCTAssertEqual(controlled, 1)
        XCTAssertEqual(rig.runner.count("control"), 1, "exactly the one recorded command")

        // And the reverse: while another owner holds the lifecycle no control starts.
        let other = try Rig(.init(loaded: .loaded, snapshot: .previewRunning))
        defer { other.tearDown() }
        other.service.hold("pause")
        other.controller.runFullDiagnostics { _ in }
        var outcome: Result<Void, Error>?
        other.controller.performVehicleControl(.lock) { outcome = $0 }
        guard case let .failure(error)? = outcome else { return XCTFail("control admitted inside a custody") }
        XCTAssertTrue(isLifecycleRefusal(error))
        XCTAssertEqual(other.runner.count("control"), 0)
        other.drain()
    }

    // MARK: R-E1F6-12

    func testDeferredReconcileRechecksStateAndCannotUndoIntentionalStop() throws {
        // Coalesced: two requests made while another owner holds the lifecycle.
        let rig = try Rig(.init(loaded: .loaded, change: .replaced))
        defer { rig.tearDown() }
        let foreign = try acquireForeign(rig)
        var completions = 0
        rig.controller.reconcileLaunchAgent { completions += 1 }
        rig.controller.reconcileLaunchAgent { completions += 1 }
        XCTAssertEqual(rig.events.all, [], "no writer step while another owner holds the lifecycle")
        XCTAssertEqual(completions, 0)
        releaseForeign(rig, foreign)
        spinUntil(3) { completions == 2 }
        rig.drain()
        XCTAssertEqual(completions, 2, "every coalesced caller hears exactly one completion")
        XCTAssertEqual(rig.events.all.filter { $0 == "ensure" }.count, 1, "one coalesced run")
        XCTAssertEqual(rig.acquisitions(.reconcile), 1)

        // An intentional Stop admitted after the request invalidates it. The Stop comes in
        // the instant the owner is released, before the deferred request is looked at again.
        let stopRig = try Rig(.init(loaded: .loaded, change: .replaced))
        defer { stopRig.tearDown() }
        let held = try acquireForeign(stopRig)
        var reconciled = 0
        stopRig.controller.reconcileLaunchAgent { reconciled += 1 }
        releaseForeign(stopRig, held)
        var stopped: Result<Void, Error>?
        stopRig.controller.stopHub { stopped = $0 }
        spinUntil(3) { stopped != nil && reconciled == 1 }
        stopRig.drain()
        XCTAssertEqual(reconciled, 1, "the caller still hears its completion")
        XCTAssertFalse(stopRig.events.all.contains("ensure"), "the stale request wrote nothing")
        XCTAssertFalse(stopRig.service.calls.contains("reconcile"))
        XCTAssertEqual(stopRig.service.count("run:restart"), 0, "and restarted nothing")
        XCTAssertEqual(stopRig.acquisitions(.reconcile), 0)

        // A service file removed meanwhile is not written back: installation is re-read.
        let removeRig = try Rig(.init(loaded: .loaded, change: .replaced))
        defer { removeRig.tearDown() }
        let third = try acquireForeign(removeRig)
        var done = 0
        removeRig.controller.reconcileLaunchAgent { done += 1 }
        removeRig.agent.installed = false
        releaseForeign(removeRig, third)
        spinUntil(3) { done == 1 }
        removeRig.drain()
        XCTAssertEqual(done, 1)
        XCTAssertFalse(removeRig.events.all.contains("ensure"), "the file is no longer installed: nothing is written")
    }

    func testReconcileCannotEnterACompoundWorkflowGap() throws {
        // Account setup releases the service queue between its phases; reconcile may not
        // use the gap between the Pause and the setup command.
        let rig = try Rig(.init(loaded: .loaded, change: .replaced))
        defer { rig.tearDown() }
        rig.runner.hold("setup-fleet")
        rig.controller.configureFleetAccount(credentials: fleetCredentials()) { _ in }
        XCTAssertEqual(rig.runner.heldKeys, ["setup-fleet"], "setup is in the gap after the pause")
        var done = 0
        rig.controller.reconcileLaunchAgent { done += 1 }
        XCTAssertFalse(rig.service.calls.contains("reconcile"), "reconcile did not run between the phases")
        XCTAssertFalse(rig.events.all.contains("ensure"))
        XCTAssertEqual(done, 0)
        rig.drain()
        spinUntil(3) { done == 1 }
        XCTAssertEqual(done, 1)
        XCTAssertEqual(rig.acquisitions(.account), 1)
        XCTAssertEqual(rig.acquisitions(.reconcile), 1, "it ran once the account owner was done")
    }

    // MARK: R-E1F6-13

    func testDirectDeleteInstallsRestorationAndPreferenceBarrier() throws {
        let rig = try Rig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        var written = 0
        XCTAssertTrue(rig.controller.performPreferenceWrite { written += 1 }, "writes are admitted when open")
        rig.service.hold("delete")
        var deleted: Result<HubDataRemovalReport, Error>?
        rig.controller.deleteHubAndData { deleted = $0 }
        XCTAssertEqual(rig.barrier.events.first, "install", "the barrier is installed at admission")
        XCTAssertTrue(rig.barrier.events.contains("apply"))
        spinUntil { rig.service.heldKeys.contains("delete") }
        XCTAssertFalse(rig.controller.performPreferenceWrite { written += 1 }, "a preference write is refused during Delete")
        XCTAssertEqual(written, 1)
        rig.service.release("delete")
        spinUntil { deleted != nil }
        XCTAssertFalse(rig.barrier.events.contains("remove"), "a completed Delete keeps the barrier")
        XCTAssertFalse(rig.controller.performPreferenceWrite { written += 1 }, "and after it")
        XCTAssertEqual(written, 1)

        // A refusal before the data step lifts it again.
        let refused = try Rig(.init(loaded: .loaded))
        defer { refused.tearDown() }
        refused.service.deleteResult = .failure(HubActionError.commandFailed("Nothing was changed."))
        var outcome: Result<HubDataRemovalReport, Error>?
        refused.controller.deleteHubAndData { outcome = $0 }
        spinUntil { outcome != nil }
        XCTAssertEqual(refused.barrier.events, ["install", "apply", "remove"])
        XCTAssertTrue(refused.controller.performPreferenceWrite { written += 1 })
        XCTAssertEqual(written, 2)
    }

    func testEveryMutationIsRefusedAfterDeleteDataStepCompletes() throws {
        let rig = try Rig(.init(loaded: .loaded, snapshot: .previewRunning, change: .replaced))
        defer { rig.tearDown() }
        var deleted: Result<HubDataRemovalReport, Error>?
        rig.controller.deleteHubAndData { deleted = $0 }
        spinUntil { deleted != nil }
        XCTAssertTrue(rig.controller.hubDeletionCompleted)
        XCTAssertEqual(rig.capabilities.state, .deleted)
        XCTAssertNil(rig.capabilities.owner, "released, but into deleted: never ordinary idle")
        XCTAssertFalse(rig.capabilities.isIdle)

        let before = rig.signature()
        var refused = 0
        let expected = 12
        rig.controller.startHub { if case .failure = $0 { refused += 1 } }
        rig.controller.stopHub { if case .failure = $0 { refused += 1 } }
        rig.controller.restartHub { if case .failure = $0 { refused += 1 } }
        rig.controller.removeBackgroundService { if case .failure = $0 { refused += 1 } }
        rig.controller.deleteHubAndData { if case .failure = $0 { refused += 1 } }
        rig.controller.configureFleetAccount(credentials: fleetCredentials()) { if case .failure = $0 { refused += 1 } }
        rig.controller.signOutTeslaAccount { if case .failure = $0 { refused += 1 } }
        rig.controller.runFullDiagnostics { if $0.contains("not run") { refused += 1 } }
        rig.controller.performVehicleControl(.lock) { if case .failure = $0 { refused += 1 } }
        rig.controller.runOnboardingChecks(expectRunning: false) { if case .failure = $0 { refused += 1 } }
        rig.controller.acknowledgeMigrationHandoverAndStart { if case .failure = $0 { refused += 1 } }
        rig.controller.importTeslaMateHistoryOnlyOnline(source: "postgresql://reader@127.0.0.1:55438/t", carID: "1",
                                                        passwordFile: "/p", acknowledgeV42CompatibleSchema: true) {
            if case .failure = $0 { refused += 1 }
        }
        var reconciled = false
        rig.controller.reconcileLaunchAgent { reconciled = true }
        XCTAssertTrue(reconciled, "reconcile adds no work and says so")
        XCTAssertEqual(refused, expected)
        XCTAssertEqual(rig.signature(), before, "nothing ran after the data step")
        XCTAssertFalse(rig.controller.performPreferenceWrite {})
        XCTAssertEqual(rig.capabilities.state, .deleted)
    }

    func testDeleteRefusalReleasesOnlyAfterResumeAndRefreshSettlement() throws {
        let rig = try Rig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        rig.service.deleteResult = .failure(HubActionError.commandFailed("Nothing was deleted."))
        rig.runner.hold("status")
        var outcome: Result<HubDataRemovalReport, Error>?
        rig.controller.deleteHubAndData { outcome = $0 }
        spinUntil { rig.runner.heldKeys.contains("status") }
        XCTAssertEqual(rig.runner.heldKeys, ["status"], "the refresh after the refusal is in flight")
        XCTAssertFalse(rig.status.isPaused, "status reads were resumed first")
        XCTAssertTrue(rig.barrier.events.contains("remove"), "the barrier was lifted")
        XCTAssertEqual(rig.capabilities.state, .deleting, "still the owner's until the refresh has settled")
        XCTAssertNil(outcome)
        XCTAssertEqual(rig.releases(.delete), 0)
        var refused = 0
        rig.controller.stopHub { if case .failure = $0 { refused += 1 } }
        XCTAssertEqual(refused, 1, "no other owner enters while the refusal settles")

        rig.runner.release("status")
        spinUntil { outcome != nil }
        guard case .failure? = outcome else { return XCTFail("a refused Delete reported success") }
        XCTAssertEqual(rig.releases(.delete), 1)
        XCTAssertEqual(rig.capabilities.state, .open)
        XCTAssertTrue(rig.capabilities.isIdle, "released into open operation")
    }

    // MARK: R-E1F6-14

    func testObserversRemainAvailableDuringOrdinaryLifecycleWork() throws {
        let rig = try Rig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        rig.service.hold("pause")
        rig.controller.runFullDiagnostics { _ in }
        XCTAssertEqual(rig.capabilities.owner, .diagnostics)
        var refreshed = 0
        rig.controller.refresh { _ in refreshed += 1 }
        spinUntil(3) { refreshed == 1 }
        XCTAssertEqual(refreshed, 1)
        XCTAssertGreaterThanOrEqual(rig.runner.count("status"), 1, "a status read overlaps the owner")
        var logged = 0
        rig.controller.logs { _ in logged += 1 }
        spinUntil(3) { logged == 1 }
        XCTAssertEqual(logged, 1, "log tails are resource-level observations")
        _ = rig.controller.diagnostics()
        XCTAssertEqual(rig.acquisitions(.diagnostics), 1)
        XCTAssertEqual(rig.journal.filter { if case .acquired = $0 { return true } else { return false } }.count, 1,
                       "no observer acquired a lifecycle owner")
        rig.drain()

        // They stop once the data step has run, and once Quit is accepted.
        let deleted = try Rig(.init(loaded: .loaded))
        defer { deleted.tearDown() }
        var done: Result<HubDataRemovalReport, Error>?
        deleted.controller.deleteHubAndData { done = $0 }
        spinUntil { done != nil }
        let statusBefore = deleted.runner.count("status")
        var settledAfterDelete = 0
        deleted.controller.refresh { _ in settledAfterDelete += 1 }
        XCTAssertEqual(settledAfterDelete, 1, "answered from the retained snapshot")
        XCTAssertEqual(deleted.runner.count("status"), statusBefore, "no status read after the data step")

        let closing = try Rig(.init(loaded: .loaded))
        defer { closing.tearDown() }
        closing.controller.beginClosing()
        var closedRefresh = 0
        closing.controller.refresh { _ in closedRefresh += 1 }
        XCTAssertEqual(closedRefresh, 1)
        XCTAssertEqual(closing.runner.count("status"), 0, "no observer subprocess once closing")
        XCTAssertFalse(closing.controller.performPreferenceWrite {})
    }

    // MARK: F8-3

    func testTheDoctorsResumeStartWaitsForAHeldDoctorChild() throws {
        let rig = try Rig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        rig.runner.results["doctor"] = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        var report: String?
        rig.controller.runFullDiagnostics { report = $0 }
        spinUntil(5) { rig.runner.count("status") >= 1 && rig.capabilities.childHeld }
        spinBriefly(0.3)
        XCTAssertEqual(rig.service.count("run:pause"), 1)
        XCTAssertTrue(rig.capabilities.childHeld, "the doctor child may still write")
        XCTAssertEqual(rig.service.count("run:start"), 0, "the Hub is not started again while the doctor child may still write")
        XCTAssertNil(report)
        XCTAssertEqual(rig.capabilities.owner, .diagnostics)

        quiescence.childReaped()
        spinUntil(5) { report != nil }
        XCTAssertEqual(rig.service.count("run:start"), 1, "the Hub is started once the doctor child is gone")
        XCTAssertTrue(report?.contains("Hub collection resumed.") ?? false, report ?? "")
        XCTAssertTrue(rig.capabilities.isIdle)
        XCTAssertEqual(rig.releases(.diagnostics), 1)
    }

    // MARK: F8-5

    func testTheSSHDiscoveryMapsAProcessThatCannotBeStoppedThroughItsRealClosure() throws {
        let root = HubTestScratch.directory
            .appendingPathComponent("hub-owner-discovery-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        let lock = NSLock()
        var launched: [(arguments: [String], stdin: String)] = []
        let runner: TeslaMateServerImporter.DiscoveryRunner = { arguments, stdin, _, completion in
            lock.lock(); launched.append((arguments, stdin)); lock.unlock()
            completion(.failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence)))
        }
        var outcomes: [Result<TeslaMateServerImportSession, Error>] = []
        TeslaMateServerImporter.connect(host: "teslamate.local", user: "user", port: 22,
                                        authentication: .key(identityFile: nil), usePasswordlessSudo: false,
                                        temporaryRoot: root, runDiscovery: runner) { outcomes.append($0) }
        spinUntil(5) { !outcomes.isEmpty }
        XCTAssertEqual(outcomes.count, 1)
        lock.lock(); let seen = launched; lock.unlock()
        XCTAssertEqual(seen.count, 1, "the discovery ran through the injected runner, not a real ssh")
        XCTAssertTrue(seen.first?.arguments.contains("user@teslamate.local") ?? false, "\(String(describing: seen.first?.arguments))")
        XCTAssertTrue(seen.first?.stdin.contains("docker") ?? false, "the discovery script is what is fed to the process")
        guard case let .failure(error)? = outcomes.first,
              case let HubActionError.commandNotQuiescent(inner, held) = error else {
            return XCTFail("the SSH process may still run, so the failure must carry it: \(String(describing: outcomes.first))")
        }
        XCTAssertTrue(held === quiescence)
        XCTAssertEqual((inner as? TeslaMateSSHDiagnostic)?.reasonCode, "process_not_stopped")

        // Once the process is gone the very same failure is the plain diagnostic.
        quiescence.childReaped()
        TeslaMateServerImporter.connect(host: "teslamate.local", user: "user", port: 22,
                                        authentication: .key(identityFile: nil), usePasswordlessSudo: false,
                                        temporaryRoot: root, runDiscovery: runner) { outcomes.append($0) }
        spinUntil(5) { outcomes.count == 2 }
        guard case let .failure(second)? = outcomes.last else { return XCTFail("no second outcome") }
        XCTAssertEqual((second as? TeslaMateSSHDiagnostic)?.reasonCode, "process_not_stopped", "\(second)")
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: root.path), [],
                       "the temporary secrets directory of each failed discovery is removed")
    }

    // MARK: F8-6

    func testTheDiagnosticsReportIsDeliveredOnTheMainThreadWhateverQueueTheRecoveryFinishesOn() throws {
        for scenario in ["the pause fails and the recovery restarts the Hub", "the resume start fails and the recovery restarts the Hub"] {
            let rig = try Rig(.init(loaded: .loaded))
            defer { rig.tearDown() }
            rig.service.completionQueue = DispatchQueue(label: "e1fix9-service-controller")
            if scenario.contains("pause") {
                rig.service.results["pause"] = .failure(HubActionError.commandFailed("pause failed"))
            } else {
                rig.service.results["start"] = .failure(HubActionError.commandFailed("start failed"))
            }
            let lock = NSLock()
            var deliveries: [Bool] = []
            rig.controller.runFullDiagnostics { _ in lock.lock(); deliveries.append(Thread.isMainThread); lock.unlock() }
            spinUntil(5) { lock.lock(); defer { lock.unlock() }; return !deliveries.isEmpty }
            spinBriefly(0.2)
            lock.lock(); let seen = deliveries; lock.unlock()
            XCTAssertEqual(seen, [true], "\(scenario): the report arrives once, on the main thread (window code is main-only)")
            XCTAssertEqual(rig.service.count("recover"), 1, scenario)
            XCTAssertEqual(rig.releases(.diagnostics), 1, scenario)
            XCTAssertTrue(rig.capabilities.isIdle, scenario)
        }
    }
}
