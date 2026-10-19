// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

/// R-E1F6-18, controller and observer routes: the typed request each route makes to the
/// controller, with and without a conflicting owner, and the observers' admission.
extension HubControllerTests {
    private struct APIRoute {
        let name: String
        let family: HubLifecycleFamily
        var options = OwnerRig.Options(loaded: .loaded)
        let effect: (OwnerRig) -> Bool
        let invoke: (OwnerRig) throws -> Void
    }

    private func assertAPIEntry(_ route: APIRoute, file: StaticString = #filePath, line: UInt = #line) throws {
        let foreignFamily: HubLifecycleFamily = route.family == .signOut ? .diagnostics : .signOut
        do {
            let rig = try OwnerRig(route.options)
            defer { rig.tearDown() }
            try route.invoke(rig)
            rig.drain()
            XCTAssertGreaterThanOrEqual(rig.acquisitions(route.family), 1, "\(route.name): acquired its owner", file: file, line: line)
            XCTAssertTrue(route.effect(rig), "\(route.name): the accepted request had its effect", file: file, line: line)
            XCTAssertEqual(rig.releases(route.family), rig.acquisitions(route.family), "\(route.name): released", file: file, line: line)
        }
        do {
            let rig = try OwnerRig(route.options)
            defer { rig.tearDown() }
            guard case let .success(foreign) = rig.controller.lifecycle.acquire(foreignFamily) else {
                return XCTFail("\(route.name): no foreign owner", file: file, line: line)
            }
            try route.invoke(rig)
            spinBriefly(0.05)
            XCTAssertEqual(rig.acquisitions(route.family), route.family == foreignFamily ? 1 : 0,
                           "\(route.name): not admitted while another owner holds the lifecycle", file: file, line: line)
            if route.family != .reconcile {
                XCTAssertFalse(route.effect(rig), "\(route.name): the refused request had no effect", file: file, line: line)
            }
            rig.controller.lifecycle.settle(foreign) {}
            if route.family == .reconcile {
                // A launch reconcile that met another owner is deferred, not dropped: it runs once that owner is gone.
                rig.drain()
                spinUntil(3) { route.effect(rig) }
                XCTAssertTrue(route.effect(rig), "\(route.name): the deferred request ran after the other owner settled", file: file, line: line)
                XCTAssertEqual(rig.acquisitions(.reconcile), 1, "\(route.name): and acquired its owner exactly once", file: file, line: line)
            }
        }
    }

    private var localSource: String { "postgresql://reader@127.0.0.1:55438/teslamate_snapshot" }
    private var remoteSource: String { "postgresql://reader@teslamate.local/teslamate" }

    // MARK: 9. Migration stages (the controller requests the onboarding window makes)

    func testEntryConnectedCompatibilityUsesLifecycleOwner() throws {
        try assertAPIEntry(APIRoute(name: "ConnectedCompatibility", family: .migration, options: .init(installed: false),
                                    effect: { $0.runner.count("teslamate-check") >= 1 }) { rig in
            rig.controller.checkTeslaMateCompatibility(source: self.remoteSource, carID: "1", passwordFile: "/p",
                                                       acknowledgeV42CompatibleSchema: true) { _ in }
        })
    }

    func testEntryVersionAcknowledgementRecheckUsesLifecycleOwner() throws {
        try assertAPIEntry(APIRoute(name: "VersionAcknowledgementRecheck", family: .migration, options: .init(installed: false),
                                    effect: { $0.runner.count("teslamate-check") >= 1 }) { rig in
            // Ticking the acknowledgement re-runs the check against the connected session.
            var session: TeslaMateServerImportSession?
            _ = rig.controller.connectMigrationServer(host: "teslamate.local", user: "user", port: 22,
                                                      authentication: .key(identityFile: nil),
                                                      usePasswordlessSudo: false) {
                if case let .success(value) = $0 { session = value }
            }
            if let session {
                rig.controller.checkTeslaMateCompatibility(source: session.source, carID: session.carID,
                                                           passwordFile: session.passwordFile.path,
                                                           acknowledgeV42CompatibleSchema: true) { _ in }
            } else {
                // Refused at the connection: the recheck is asked for against a known source.
                rig.controller.checkTeslaMateCompatibility(source: self.remoteSource, carID: "1", passwordFile: "/p",
                                                           acknowledgeV42CompatibleSchema: true) { _ in }
            }
        })
    }

    func testEntryOnlineImportUsesLifecycleOwner() throws {
        try assertAPIEntry(APIRoute(name: "OnlineImport", family: .migration, options: .init(installed: false),
                                    effect: { $0.runner.count("migrate") >= 1 }) { rig in
            rig.controller.importTeslaMateOnline(source: self.remoteSource, carID: "1", passwordFile: "/p",
                                                 encryptionKeyFile: "/k", acknowledgeV42CompatibleSchema: true) { _ in }
        })
    }

    func testEntryHistoryOnlyImportUsesLifecycleOwner() throws {
        try assertAPIEntry(APIRoute(name: "HistoryOnlyImport", family: .migration, options: .init(installed: false),
                                    effect: { $0.runner.count("migrate:history") >= 1 }) { rig in
            rig.controller.importTeslaMateHistoryOnlyOnline(source: self.localSource, carID: "1", passwordFile: "/p",
                                                            acknowledgeV42CompatibleSchema: true) { _ in }
        })
    }

    // MARK: 11. Verification and handover

    func testEntryAutomaticVerificationUsesLifecycleOwner() throws {
        try assertAPIEntry(APIRoute(name: "AutomaticVerification", family: .verification,
                                    options: .init(loaded: .loaded, marker: "awaiting_verification", markerInterval: 60),
                                    effect: { $0.service.count("run:stop") >= 1 && $0.runner.count("doctor") >= 1 }) { rig in
            // The window starts it by itself when it resumes at the verification page.
            rig.controller.runOnboardingChecks(expectRunning: false) { _ in }
        })
    }

    func testEntryVerificationRetryUsesLifecycleOwner() throws {
        try assertAPIEntry(APIRoute(name: "VerificationRetry", family: .verification,
                                    options: .init(loaded: .loaded, marker: "awaiting_verification", markerInterval: 60),
                                    effect: { $0.service.count("run:stop") >= 2 }) { rig in
            // A first run, then Run Again: the same controller API, each time under its own owner.
            rig.controller.runOnboardingChecks(expectRunning: false) { _ in }
            rig.drain()
            rig.controller.runOnboardingChecks(expectRunning: false) { _ in }
        })
    }

    func testEntryHistoryOnlyVerificationUsesLifecycleOwner() throws {
        try assertAPIEntry(APIRoute(name: "HistoryOnlyVerification", family: .verification,
                                    options: .init(loaded: .loaded, marker: "awaiting_verification", markerHistoryOnly: true),
                                    effect: { $0.runner.count("preflight:history") >= 1 }) { rig in
            rig.controller.runOnboardingChecks(expectRunning: false) { _ in }
        })
    }

    func testEntryLiveVerificationUsesLifecycleOwner() throws {
        try assertAPIEntry(APIRoute(name: "LiveVerification", family: .verification, options: .init(loaded: .loaded),
                                    effect: { $0.runner.count("status") >= 1 }) { rig in
            rig.controller.runOnboardingChecks(expectRunning: true) { _ in }
        })
    }

    func testEntryHandoverFinishStartUsesLifecycleOwner() throws {
        try assertAPIEntry(APIRoute(name: "HandoverFinishStart", family: .handover,
                                    options: .init(loaded: .loaded, marker: "awaiting_handover", markerInterval: 0),
                                    effect: { $0.service.count("run:start") >= 1 }) { rig in
            rig.controller.acknowledgeMigrationHandoverAndStart { _ in }
        })
    }

    // MARK: 14. Startup and launch

    func testEntryStartupCleanupUsesLifecycleOwner() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        XCTAssertEqual(rig.controller.runStartupCleanup(), 0)
        XCTAssertEqual(rig.cleanerCalls, 1, "startup coordination ran the bounded cleanup once")
        // It needs no exclusive owner (per-directory ownership protects live sessions) ...
        guard case let .success(foreign) = rig.controller.lifecycle.acquire(.diagnostics) else { return XCTFail("no owner") }
        _ = rig.controller.runStartupCleanup()
        XCTAssertEqual(rig.cleanerCalls, 2, "a held owner does not stop the directory cleanup")
        rig.controller.lifecycle.settle(foreign) {}
        // ... but it is not offered once Delete or Quit has begun.
        rig.controller.beginClosing()
        _ = rig.controller.runStartupCleanup()
        XCTAssertEqual(rig.cleanerCalls, 2, "no cleanup once the lifecycle is closing")
        let deleted = try OwnerRig(.init(loaded: .loaded))
        defer { deleted.tearDown() }
        var done: Result<HubDataRemovalReport, Error>?
        deleted.controller.deleteHubAndData { done = $0 }
        spinUntil { done != nil }
        _ = deleted.controller.runStartupCleanup()
        XCTAssertEqual(deleted.cleanerCalls, 0, "nor after a completed Delete")
    }

    func testEntryLaunchReconcileUsesLifecycleOwner() throws {
        try assertAPIEntry(APIRoute(name: "LaunchReconcile", family: .reconcile,
                                    options: .init(loaded: .loaded, change: .replaced),
                                    effect: { $0.service.count("run:restart") >= 1 }) { rig in
            rig.controller.reconcileLaunchAgent()
        })
        // An unloaded job is never started.
        let unloaded = try OwnerRig(.init(loaded: .unloaded, change: .replaced))
        defer { unloaded.tearDown() }
        unloaded.controller.reconcileLaunchAgent()
        unloaded.drain()
        XCTAssertEqual(unloaded.acquisitions(.reconcile), 1)
        XCTAssertEqual(unloaded.service.count("run:start"), 0)
        XCTAssertEqual(unloaded.service.count("run:restart"), 0)
    }

    // MARK: 15. Observers: admitted without a competing owner

    func testObserversReadNothingOnceTheLifecycleIsClosing() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        var refreshed = 0
        rig.controller.refresh { _ in refreshed += 1 }
        spinUntil(3) { refreshed == 1 }
        XCTAssertEqual(refreshed, 1)
        let statusReads = rig.runner.count("status")
        XCTAssertGreaterThan(statusReads, 0, "an open lifecycle reads the status")
        rig.controller.beginClosing()
        XCTAssertEqual(rig.capabilities.state, .closing)
        var afterClose = 0
        rig.controller.refresh { _ in afterClose += 1 }
        spinUntil(3) { afterClose == 1 }
        XCTAssertEqual(afterClose, 1, "the caller is still answered, from the last snapshot")
        XCTAssertEqual(rig.runner.count("status"), statusReads, "no status read once Quit has been accepted")
    }

    func testEntryInitialRefreshUsesLifecycleOwner() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        guard case let .success(foreign) = rig.controller.lifecycle.acquire(.signOut) else { return XCTFail("no owner") }
        let statusBefore = rig.runner.count("status")
        var delivered = false
        let window = MainWindowController(controller: rig.controller, serviceTransitionTimeout: 5,
                                          serviceTransitionPollInterval: 0.05, errorPresenter: { _ in },
                                          onInitialRefresh: { _ in delivered = true })
        defer { window.close() }
        spinUntil(5) { delivered }
        XCTAssertTrue(delivered)
        XCTAssertGreaterThan(rig.runner.count("status"), statusBefore, "the first status read overlaps the owner")
        XCTAssertEqual(rig.journal.filter { if case .acquired = $0 { return true } else { return false } }.count, 1,
                       "and acquired nothing")
        rig.controller.lifecycle.settle(foreign) {}
    }

    func testEntryTimerRefreshUsesLifecycleOwner() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        guard case let .success(foreign) = rig.controller.lifecycle.acquire(.diagnostics) else { return XCTFail("no owner") }
        let before = rig.runner.count("status")
        var refreshed = 0
        // The 15-second timer's block ends in this call.
        rig.controller.refresh { _ in refreshed += 1 }
        spinUntil(3) { refreshed == 1 }
        XCTAssertEqual(refreshed, 1)
        XCTAssertGreaterThan(rig.runner.count("status"), before)
        XCTAssertEqual(rig.acquisitions(.diagnostics), 1, "the refresh took no owner of its own")
        rig.controller.lifecycle.settle(foreign) {}
    }

    func testEntryTransitionRefreshUsesLifecycleOwner() throws {
        let harness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { harness.tearDown() }
        guard case let .success(foreign) = harness.controller.lifecycle.acquire(.signOut) else { return XCTFail("no owner") }
        let before = harness.runner.count("status")
        // The settlement polling after a start completed from onboarding refreshes status.
        harness.window.settleStartedHubFromOnboarding()
        spinUntil(3) { harness.runner.count("status") > before }
        XCTAssertGreaterThan(harness.runner.count("status"), before, "the transition's refresh overlaps the owner")
        XCTAssertEqual(harness.rig.acquisitions(.start), 0)
        harness.controller.lifecycle.settle(foreign) {}
    }

    func testEntryHandoverReadinessUsesLifecycleOwner() throws {
        let rig = try OwnerRig(.init(loaded: .loaded, marker: "awaiting_handover", markerInterval: 0))
        defer { rig.tearDown() }
        rig.runner.hold("status")
        rig.controller.acknowledgeMigrationHandoverAndStart { _ in }
        rig.runner.hold("status")
        rig.runner.release("status")
        XCTAssertEqual(rig.runner.heldKeys, ["status"], "held at the readiness read")
        XCTAssertEqual(rig.capabilities.owner, .handover)
        // The readiness read is the handover's own child, not a new owner; another status
        // read still overlaps.
        let acquired = rig.journal.filter { if case .acquired = $0 { return true } else { return false } }.count
        var refreshed = 0
        rig.controller.refresh { _ in refreshed += 1 }
        spinUntil(3) { refreshed == 1 || !rig.runner.heldKeys.isEmpty }
        XCTAssertEqual(rig.journal.filter { if case .acquired = $0 { return true } else { return false } }.count, acquired)
        rig.drain()
    }

    func testEntryLogsRefreshUsesLifecycleOwner() throws {
        let harness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { harness.tearDown() }
        guard case let .success(foreign) = harness.controller.lifecycle.acquire(.signOut) else { return XCTFail("no owner") }
        let logs = LogsWindowController(controller: harness.controller)
        // The initial load and Refresh read the log files: no owner needed, none taken.
        spinUntil(3) { !logs.textViewForTesting.string.isEmpty }
        XCTAssertFalse(logs.textViewForTesting.string.isEmpty, "logs loaded while another owner held the lifecycle")
        logs.refresh()
        spinBriefly(0.3)
        XCTAssertEqual(harness.rig.journal.filter { if case .acquired = $0 { return true } else { return false } }.count, 1)
        harness.controller.lifecycle.settle(foreign) {}
    }

    func testEntryDiagnosticsSummaryUsesLifecycleOwner() throws {
        let harness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { harness.tearDown() }
        guard case let .success(foreign) = harness.controller.lifecycle.acquire(.signOut) else { return XCTFail("no owner") }
        // The read-only summary is separate from the full diagnostics run.
        _ = harness.controller.diagnostics()
        let page = DiagnosticsWindowController(controller: harness.controller)
        XCTAssertNotNil(page.window, "the Diagnostics page builds its summary without an owner")
        XCTAssertEqual(harness.rig.journal.filter { if case .acquired = $0 { return true } else { return false } }.count, 1)
        harness.controller.lifecycle.settle(foreign) {}
        page.close()
    }
}
