// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

/// The sixth round's presentation-level ownership tests (R-E1F6-5 to R-E1F6-9, 14, 15, 17,
/// 19): the interface follows the controller's lifecycle owner and cannot release it.
extension OnboardingWindowControllerTests {
    // MARK: helpers

    func holdForeign(_ harness: WindowOwnerRig, _ family: HubLifecycleFamily = .diagnostics) throws -> HubLifecycleToken {
        switch harness.controller.lifecycle.acquire(family) {
        case let .success(token): return token
        case let .failure(refusal): throw XCTSkip("the foreign owner was refused: \(refusal)")
        }
    }

    func releaseForeign(_ harness: WindowOwnerRig, _ token: HubLifecycleToken) {
        harness.controller.lifecycle.settle(token) {}
    }

    /// Starts a Logs-page diagnostics run, as the Logs window's own menu item does, held
    /// at the named boundary.
    func startLogsDiagnostics(_ harness: WindowOwnerRig, holding boundary: String) throws -> LogsWindowController {
        switch boundary {
        case "pause": harness.service.hold("pause")
        case "doctor": harness.runner.hold("doctor")
        default: harness.service.hold("start")
        }
        let logs = LogsWindowController(controller: harness.controller)
        let item = try XCTUnwrap(logs.secondaryActionsMenu.items.first { $0.title == "Run Diagnostics" })
        spinUntil(3) { item.isEnabled }
        XCTAssertTrue(NSApp.sendAction(try XCTUnwrap(item.action), to: item.target, from: item))
        spinUntil(3) { !(harness.service.heldKeys + harness.runner.heldKeys).isEmpty }
        XCTAssertEqual(harness.controller.lifecycleCapabilities.owner, .diagnostics, boundary)
        return logs
    }

    private func dashboardEnabled(_ harness: WindowOwnerRig) -> [String: Bool?] {
        var result: [String: Bool?] = [:]
        for title in ["Stop Hub…", "Restart Hub", "Run Diagnostics"] {
            result[title] = harness.dashboardButton(title)?.isEnabled
        }
        return result
    }

    // MARK: R-E1F6-5

    func testDashboardLifecycleActionsAreRefusedDuringLogsDiagnostics() throws {
        let harness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { harness.tearDown() }
        XCTAssertEqual(dashboardEnabled(harness), ["Stop Hub…": true, "Restart Hub": true, "Run Diagnostics": true],
                       "the fixture offers the lifecycle actions when nothing is running")
        let logs = try startLogsDiagnostics(harness, holding: "pause")
        defer { _ = logs }

        XCTAssertEqual(dashboardEnabled(harness), ["Stop Hub…": false, "Restart Hub": false, "Run Diagnostics": false],
                       "the dashboard follows the owner at once, without a status refresh")
        let before = harness.service.calls
        let actions = harness.window.makeServiceLifecycleActions()
        actions.start()
        actions.stop()
        actions.restart()
        harness.dashboardButton("Stop Hub…")?.performClick(nil)
        harness.dashboardButton("Restart Hub")?.performClick(nil)
        XCTAssertEqual(harness.service.calls, before, "no lifecycle action reached the service")
        XCTAssertNil(harness.window.window?.attachedSheet, "no Stop confirmation was offered")
        XCTAssertTrue(harness.errors.isEmpty)
        harness.rig.drain()
        XCTAssertEqual(dashboardEnabled(harness), ["Stop Hub…": true, "Restart Hub": true, "Run Diagnostics": true],
                       "and again when the custody has settled")
    }

    func testAlreadyOpenLegacyDetailsObservesConflictingOwner() throws {
        let harness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { harness.tearDown() }
        let details = try XCTUnwrap(harness.window.showServiceDetails(), "the legacy modal Details window")
        let root = details.window?.contentView
        func mutationButtons() -> [NSButton] {
            ["hub.service.start-stop", "hub.service.restart", "hub.service.remove", "hub.service.delete-data"]
                .compactMap { harness.detailsButton($0, in: root) }
        }
        XCTAssertEqual(mutationButtons().count, 4)
        XCTAssertTrue(mutationButtons().allSatisfy(\.isEnabled))

        // A conflicting owner appears after the window was opened: it is observed.
        let foreign = try holdForeign(harness, .signOut)
        XCTAssertTrue(mutationButtons().allSatisfy { !$0.isEnabled }, "the open modal follows the owner")
        let before = harness.service.calls
        for button in mutationButtons() {
            // A stale enabled button reaching its selector still changes nothing.
            button.isEnabled = true
            button.performClick(nil)
        }
        XCTAssertEqual(harness.service.calls, before)
        XCTAssertTrue(harness.alerts.isEmpty, "no confirmation was offered")
        releaseForeign(harness, foreign)
        XCTAssertTrue(mutationButtons().allSatisfy(\.isEnabled), "and follows the release")
        details.close()
    }

    func testStopAndRemoveRecheckOwnershipAfterConfirmation() throws {
        // Stop: the sheet is open, then another owner takes the lifecycle, then it is confirmed.
        let stopHarness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { stopHarness.tearDown() }
        stopHarness.window.showEmbeddedServiceDetails()
        let stop = try XCTUnwrap(stopHarness.detailsButton("hub.service.start-stop"))
        stop.performClick(nil)
        let sheet = try XCTUnwrap(stopHarness.window.window?.attachedSheet, "the Stop confirmation is open")
        let foreign = try holdForeign(stopHarness)
        try XCTUnwrap(stopHarness.buttons(in: sheet.contentView).first { $0.title == "Stop Hub" }).performClick(nil)
        spinBriefly()
        XCTAssertEqual(stopHarness.service.count("run:stop"), 0, "the confirmation used its earlier admission snapshot")
        XCTAssertTrue(stopHarness.errors.isEmpty, "the capability was rechecked: no transition began, no error was shown")
        releaseForeign(stopHarness, foreign)
        XCTAssertFalse(stopHarness.window.operationPreventsQuit, "and no stopping transition is pending")

        // Remove: the details window's own confirmation.
        let removeHarness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { removeHarness.tearDown() }
        let presenter = RecordingAlertPresenter()
        presenter.answer = .alertSecondButtonReturn
        var removeForeign: HubLifecycleToken?
        presenter.whileConfirming = { _ in removeForeign = try? self.holdForeign(removeHarness) }
        let previous = HubUIPresentation.replaceAlertPresenterForTesting(presenter)
        defer { HubUIPresentation.replaceAlertPresenterForTesting(previous) }
        removeHarness.window.showEmbeddedServiceDetails()
        try XCTUnwrap(removeHarness.detailsButton("hub.service.remove")).performClick(nil)
        XCTAssertNotNil(removeForeign, "the conflict arose while the confirmation was open")
        XCTAssertEqual(removeHarness.service.count("run:remove"), 0)
        XCTAssertTrue(removeHarness.errors.isEmpty)
        if let removeForeign { releaseForeign(removeHarness, removeForeign) }

        // Delete: the second confirmation.
        let deleteHarness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { deleteHarness.tearDown() }
        deleteHarness.answers = [.alertSecondButtonReturn, .alertSecondButtonReturn]
        var deleteForeign: HubLifecycleToken?
        deleteHarness.whileConfirming = { alert in
            if alert.messageText == "Permanently delete Hub data?" {
                deleteForeign = try? self.holdForeign(deleteHarness)
            }
        }
        deleteHarness.window.showEmbeddedServiceDetails()
        try XCTUnwrap(deleteHarness.detailsButton("hub.service.delete-data")).performClick(nil)
        XCTAssertEqual(deleteHarness.alerts.count, 2)
        XCTAssertNotNil(deleteForeign)
        XCTAssertEqual(deleteHarness.service.count("delete"), 0)
        XCTAssertTrue(deleteHarness.errors.isEmpty)
        if let deleteForeign { releaseForeign(deleteHarness, deleteForeign) }
        XCTAssertFalse(deleteHarness.window.operationPreventsQuit, "no deleting transition began")
    }

    // MARK: R-E1F6-6

    func testLogsDiagnosticsBlocksDashboardStartStopAndRestartUntilResumeSettles() throws {
        for boundary in ["pause", "doctor", "resume"] {
            let harness = try WindowOwnerRig(.init(loaded: .loaded))
            defer { harness.tearDown() }
            let logs = try startLogsDiagnostics(harness, holding: boundary)
            defer { _ = logs }
            XCTAssertEqual(dashboardEnabled(harness), ["Stop Hub…": false, "Restart Hub": false, "Run Diagnostics": false],
                           "\(boundary): the lifecycle actions are unavailable")
            let before = harness.service.calls
            let actions = harness.window.makeServiceLifecycleActions()
            actions.start(); actions.stop(); actions.restart()
            harness.dashboardButton("Stop Hub…")?.performClick(nil)
            XCTAssertEqual(harness.service.calls, before, "\(boundary): refused, not deferred")
            XCTAssertEqual(harness.rig.releases(.diagnostics), 0, "\(boundary): the custody is still held")
            harness.rig.drain()
            XCTAssertEqual(harness.rig.releases(.diagnostics), 1, boundary)
            // A rejected Stop is not deferred: after the run the user may issue a fresh one.
            XCTAssertEqual(harness.service.count("run:stop"), 0, boundary)
            harness.window.makeServiceLifecycleActions().restart()
            spinUntil(3) { harness.service.count("run:restart") == 1 }
            XCTAssertEqual(harness.service.count("run:restart"), 1, "\(boundary): a fresh request is admitted")
            harness.rig.drain()
        }
    }

    // MARK: R-E1F6-7

    func testDisconnectPreventsDeleteConfirmationsUntilSettlement() throws {
        let harness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { harness.tearDown() }
        let presenter = RecordingAlertPresenter()
        presenter.answer = .alertSecondButtonReturn
        let previous = HubUIPresentation.replaceAlertPresenterForTesting(presenter)
        defer { HubUIPresentation.replaceAlertPresenterForTesting(previous) }
        harness.runner.hold("control")
        harness.send("disconnectTeslaPressed")
        XCTAssertEqual(presenter.confirmations.first?.messageText, "Disconnect Tesla from Hub?")
        spinUntil(3) { harness.runner.heldKeys.contains("control") }
        XCTAssertEqual(harness.controller.lifecycleCapabilities.owner, .signOut)

        harness.window.showEmbeddedServiceDetails()
        let delete = try XCTUnwrap(harness.detailsButton("hub.service.delete-data"))
        XCTAssertFalse(delete.isEnabled, "Delete is unavailable while the sign-out holds the lifecycle")
        delete.isEnabled = true
        delete.performClick(nil)
        harness.window.makeServiceLifecycleActions().deleteHubAndData()
        XCTAssertTrue(harness.alerts.isEmpty, "no Delete confirmation was offered")
        XCTAssertEqual(harness.service.count("delete"), 0)

        harness.rig.drain()
        spinUntil(3) { harness.controller.lifecycleCapabilities.isIdle }
        harness.answers = [.alertFirstButtonReturn]
        harness.window.makeServiceLifecycleActions().deleteHubAndData()
        XCTAssertEqual(harness.alerts.first?.messageText, "Delete Teslatlas Hub and all its data?",
                       "after settlement the first confirmation is offered again")
    }

    // MARK: R-E1F6-8

    func testOAuthCompletionUsesItsOwnerAndIgnoresCancelledGeneration() throws {
        let harness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { harness.tearDown() }
        let tokens = TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")

        // Completion continues under the context held since before authentication began.
        guard case let .success(signIn) = harness.controller.beginLegacySignIn() else { return XCTFail("not admitted") }
        XCTAssertEqual(harness.controller.lifecycleCapabilities.owner, .account)
        XCTAssertEqual(harness.rig.acquisitions(.account), 1)
        var done: Result<Void, Error>?
        signIn.configure(tokens: tokens) { done = $0 }
        harness.rig.drain()
        spinUntil(3) { done != nil }
        guard case .success? = done else { return XCTFail("setup did not finish: \(String(describing: done))") }
        XCTAssertEqual(harness.rig.acquisitions(.account), 1, "the completion did not acquire again")
        XCTAssertEqual(Set(harness.service.owners.map(\.id)).count, 1, "every service call carried the one owner")
        XCTAssertEqual(harness.rig.releases(.account), 1)

        // A cancelled generation: the late callback configures nothing.
        guard case let .success(cancelled) = harness.controller.beginLegacySignIn() else { return XCTFail("not admitted again") }
        cancelled.cancel()
        XCTAssertTrue(harness.controller.lifecycleCapabilities.isIdle)
        let before = harness.rig.signature()
        var late: Result<Void, Error>?
        cancelled.configure(tokens: tokens) { late = $0 }
        guard case .failure? = late else { return XCTFail("a cancelled sign-in configured an account") }
        XCTAssertEqual(harness.rig.signature(), before, "the late authentication callback changed nothing")

        // And after a completed Delete.
        guard case let .success(beforeDelete) = harness.controller.beginLegacySignIn() else { return XCTFail("not admitted") }
        beforeDelete.cancel()
        var deleted: Result<HubDataRemovalReport, Error>?
        harness.controller.deleteHubAndData { deleted = $0 }
        spinUntil(5) { deleted != nil }
        XCTAssertTrue(harness.controller.hubDeletionCompleted)
        let afterDelete = harness.rig.signature()
        var lateAfterDelete: Result<Void, Error>?
        beforeDelete.configure(tokens: tokens) { lateAfterDelete = $0 }
        guard case .failure? = lateAfterDelete else { return XCTFail("configured an account after Delete") }
        XCTAssertEqual(harness.rig.signature(), afterDelete)
        guard case .failure = harness.controller.beginLegacySignIn() else { return XCTFail("a sign-in began after Delete") }
    }

    // MARK: R-E1F6-9

    func testSSHPreparationAndCleanupRemainControllerOwnedAfterWindowCloses() throws {
        // A window that closes while the connection is being made.
        let harness = try WindowOwnerRig(.init(installed: false))
        defer { harness.tearDown() }
        harness.rig.connector.hold()
        var onboarding: OnboardingWindowController? = try XCTUnwrap(harness.window.showOnboarding(route: .migration))
        try fillServerField(in: onboarding, host: "tm.example")
        onboarding?.sendConnect()
        XCTAssertTrue(harness.rig.connector.isHolding, "the preparation is under way")
        XCTAssertEqual(harness.controller.lifecycleCapabilities.owner, .migration, "held by the controller")
        onboarding?.window?.close()
        XCTAssertEqual(harness.controller.lifecycleCapabilities.owner, .migration,
                       "closing the window did not release the owner")
        onboarding = nil
        harness.rig.connector.releaseAll()
        spinUntil(3) { harness.controller.lifecycleCapabilities.isIdle }
        XCTAssertTrue(harness.controller.lifecycleCapabilities.isIdle, "the preparation settled by itself")
        XCTAssertEqual(harness.rig.releases(.migration), 1)
        XCTAssertEqual(harness.controller.registeredMigrationSessionCount, 0, "nothing was left registered")
        for directory in harness.rig.connector.directories {
            XCTAssertFalse(FileManager.default.fileExists(atPath: directory.path), "the temporary directory was removed")
        }

        // A window that connects and is then closed: the idle session is closed too.
        let second = try WindowOwnerRig(.init(installed: false))
        defer { second.tearDown() }
        let window = try XCTUnwrap(second.window.showOnboarding(route: .migration))
        try fillServerField(in: window, host: "tm.example")
        window.sendConnect()
        spinUntil(3) { second.controller.registeredMigrationSessionCount == 1 || second.rig.connector.callCount == 1 }
        spinUntil(3) { second.controller.lifecycleCapabilities.isIdle }
        XCTAssertEqual(second.controller.registeredMigrationSessionCount, 1, "an idle prepared session is registered")
        XCTAssertTrue(second.controller.lifecycleCapabilities.isIdle, "and it does not hold the lifecycle")
        window.window?.close()
        XCTAssertEqual(second.controller.registeredMigrationSessionCount, 0, "closing the window closed the session")
        let directory = try XCTUnwrap(second.rig.connector.directories.first)
        XCTAssertFalse(FileManager.default.fileExists(atPath: directory.path))
    }

    func testResidualImportSheetCannotBypassLifecycleAdmission() throws {
        let rig = try OwnerRig(.init(installed: false))
        defer { rig.tearDown() }
        let sheet = ImportSheetController(controller: rig.controller)
        let root = try XCTUnwrap(sheet.window?.contentView)
        func allViews(_ view: NSView) -> [NSView] { [view] + view.subviews.flatMap(allViews) }
        let fields = allViews(root).compactMap { $0 as? NSTextField }.filter { $0.isEditable }
        for field in fields where field.stringValue.isEmpty { field.stringValue = "/private/file" }
        let acknowledgement = try XCTUnwrap(allViews(root).compactMap { $0 as? NSButton }
            .first { $0.title.hasPrefix("I confirm") })
        acknowledgement.state = .on
        let importButton = try XCTUnwrap(allViews(root).compactMap { $0 as? NSButton }.first { $0.title == "Import" })

        let presenter = RecordingAlertPresenter()
        let previous = HubUIPresentation.replaceAlertPresenterForTesting(presenter)
        defer { HubUIPresentation.replaceAlertPresenterForTesting(previous) }
        let foreign = try { () throws -> HubLifecycleToken in
            guard case let .success(token) = rig.controller.lifecycle.acquire(.diagnostics) else { throw XCTSkip("no owner") }
            return token
        }()
        let before = rig.signature()
        importButton.performClick(nil)
        XCTAssertEqual(presenter.errors.count, 1, "the refusal was presented")
        XCTAssertEqual(rig.signature(), before, "the residual sheet started nothing")
        XCTAssertFalse(rig.markerExists)
        rig.controller.lifecycle.settle(foreign) {}
        importButton.performClick(nil)
        XCTAssertEqual(rig.acquisitions(.migration), 1, "admitted once the lifecycle was free")
        rig.drain()
    }

    // MARK: R-E1F6-14

    func testAppearanceAndFrameAutosaveCannotWriteDuringOrAfterDelete() throws {
        var options = OwnerRig.Options(loaded: .loaded)
        options.realBarrier = true
        let harness = try WindowOwnerRig(options)
        defer { harness.tearDown() }
        HubWindowRestoration.endSuppressing()

        // Open: the appearance toggle writes (to the recording store, never to disk).
        harness.send("appearancePressed")
        XCTAssertEqual(harness.appearanceWrites.count, 1)
        let ordinary = HubUtilityWindowStyle.makeWindow(title: "ordinary", size: NSSize(width: 300, height: 200),
                                                        minimum: NSSize(width: 100, height: 100))
        XCTAssertTrue(ordinary.isRestorable, "an ordinary window is restorable")

        // During Delete.
        harness.service.hold("delete")
        harness.answers = [.alertSecondButtonReturn, .alertSecondButtonReturn]
        harness.controller.deleteHubAndData { _ in }
        spinUntil { harness.service.heldKeys.contains("delete") }
        XCTAssertTrue(HubWindowRestoration.isSuppressing, "the controller installed the barrier itself")
        harness.send("appearancePressed")
        XCTAssertEqual(harness.appearanceWrites.count, 1, "no appearance write during Delete")
        let during = HubUtilityWindowStyle.makeWindow(title: "during", size: NSSize(width: 300, height: 200),
                                                      minimum: NSSize(width: 100, height: 100))
        XCTAssertFalse(during.isRestorable, "a window created during Delete is not restorable")
        XCTAssertEqual(during.frameAutosaveName, "", "and carries no frame autosave name")

        // After Delete.
        harness.service.release("delete")
        spinUntil { harness.controller.hubDeletionCompleted }
        harness.send("appearancePressed")
        XCTAssertEqual(harness.appearanceWrites.count, 1, "nor after it")
        let after = HubUtilityWindowStyle.makeWindow(title: "after", size: NSSize(width: 300, height: 200),
                                                     minimum: NSSize(width: 100, height: 100))
        XCTAssertFalse(after.isRestorable)
        XCTAssertTrue(HubWindowRestoration.isSuppressing)
        [ordinary, during, after].forEach { $0.close() }
    }

    // MARK: R-E1F6-15

    func testAllExistingSurfacesUpdateCapabilitiesWithoutAStatusRefresh() throws {
        let harness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { harness.tearDown() }
        let diagnostics = DiagnosticsWindowController(controller: harness.controller)
        let logs = LogsWindowController(controller: harness.controller)
        let details = try XCTUnwrap(harness.window.showServiceDetails())
        let runButton = try XCTUnwrap(harness.buttons(in: diagnostics.window?.contentView).first { $0.title == "Run Again" })
        let item = try XCTUnwrap(logs.secondaryActionsMenu.items.first { $0.title == "Run Diagnostics" })
        spinUntil(3) { item.isEnabled }
        let statusBefore = harness.runner.count("status")
        let start = try XCTUnwrap(harness.detailsButton("hub.service.start-stop", in: details.window?.contentView))
        XCTAssertTrue(runButton.isEnabled && item.isEnabled && start.isEnabled)

        let foreign = try holdForeign(harness, .signOut)
        XCTAssertFalse(runButton.isEnabled, "Diagnostics Run follows the owner")
        XCTAssertFalse(item.isEnabled, "Logs Run Diagnostics follows the owner")
        XCTAssertFalse(start.isEnabled, "the Details window follows the owner")
        XCTAssertEqual(harness.dashboardButton("Stop Hub…")?.isEnabled, false, "the dashboard follows the owner")
        XCTAssertEqual(harness.runner.count("status"), statusBefore, "no status refresh was needed")

        releaseForeign(harness, foreign)
        XCTAssertTrue(runButton.isEnabled && item.isEnabled && start.isEnabled)
        XCTAssertEqual(harness.dashboardButton("Stop Hub…")?.isEnabled, true)
        XCTAssertEqual(harness.runner.count("status"), statusBefore)
        details.close()
        diagnostics.close()
        logs.close()
    }

    func testNewSurfaceUsesCurrentOwnerCapabilities() throws {
        let harness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { harness.tearDown() }
        let foreign = try holdForeign(harness, .signOut)
        // Surfaces created while the owner is held read the current snapshot at once.
        let details = try XCTUnwrap(harness.window.showServiceDetails())
        XCTAssertTrue(["hub.service.start-stop", "hub.service.restart", "hub.service.remove", "hub.service.delete-data"]
            .compactMap { harness.detailsButton($0, in: details.window?.contentView) }
            .allSatisfy { !$0.isEnabled }, "a new Details window starts disabled")
        let diagnostics = DiagnosticsWindowController(controller: harness.controller)
        let run = harness.buttons(in: diagnostics.window?.contentView).first { $0.title == "Run Again" }
        XCTAssertEqual(run?.isEnabled, false, "a new Diagnostics window starts disabled")
        let logs = LogsWindowController(controller: harness.controller)
        let item = try XCTUnwrap(logs.secondaryActionsMenu.items.first { $0.title == "Run Diagnostics" })
        spinBriefly(0.3)
        XCTAssertFalse(item.isEnabled, "a new Logs window starts with Run Diagnostics disabled")
        releaseForeign(harness, foreign)
        details.close(); diagnostics.close(); logs.close()
    }

    func testStaleEnabledActionStillReceivesControllerRefusal() throws {
        let harness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { harness.tearDown() }
        // Details opened with a permissive gate: the window's own capability check is stale.
        let details = ServiceDetailsWindowController(
            snapshot: harness.controller.snapshot, controller: harness.controller,
            lifecycleActions: .init(
                start: { harness.controller.startHub { harness.recordStale($0) } },
                stop: { harness.controller.stopHub { harness.recordStale($0) } },
                restart: { harness.controller.restartHub { harness.recordStale($0) } },
                remove: { harness.controller.removeBackgroundService { harness.recordStale($0) } },
                deleteHubAndData: { harness.controller.deleteHubAndData { _ in harness.staleRefusals += 1 } }),
            mutationAllowed: { true })
        details.setMutationsEnabled(true)
        let foreign = try holdForeign(harness)
        let before = harness.service.calls
        for identifier in ["hub.service.start-stop", "hub.service.restart"] {
            harness.detailsButton(identifier, in: details.window?.contentView)?.performClick(nil)
        }
        details.window?.contentView.map { harness.allViews($0) }?.compactMap { $0 as? NSButton }
            .first { $0.identifier?.rawValue == "hub.service.delete-data" }?.performClick(nil)
        XCTAssertEqual(harness.staleRefusals, 3, "each reached the controller and was refused there")
        XCTAssertEqual(harness.service.calls, before, "and nothing ran")
        releaseForeign(harness, foreign)
        details.close()
    }

    // MARK: R-E1F6-17

    func testEveryCloseRoutePreservesPolicyAndCannotReleaseAnActiveOwner() throws {
        // MainClose and WindowMenuClose: refused while a protected operation is active.
        let harness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { harness.tearDown() }
        let logs = try startLogsDiagnostics(harness, holding: "pause")
        let mainWindow = try XCTUnwrap(harness.window.window)
        XCTAssertFalse(harness.window.windowShouldClose(mainWindow), "MainClose: refused during the custody")
        let menuDelegate = AppDelegate(keyWindow: { mainWindow })
        menuDelegate.hubController = harness.controller
        let closeItem = NSMenuItem()
        closeItem.action = #selector(AppDelegate.closeKeyWindow(_:))
        XCTAssertFalse(menuDelegate.validateMenuItem(closeItem), "WindowMenuClose: Close is unavailable")
        menuDelegate.closeKeyWindow(nil)
        XCTAssertEqual(harness.rig.releases(.diagnostics), 0)

        // LogsClose: the Logs window may be closed or discarded; the run continues.
        logs.window?.close()
        XCTAssertEqual(harness.controller.lifecycleCapabilities.owner, .diagnostics, "LogsClose did not release")
        XCTAssertEqual(harness.rig.releases(.diagnostics), 0)

        // DetailsClose: Details may close while controller-owned work continues.
        let details = try XCTUnwrap(harness.window.showServiceDetails())
        details.window?.close()
        XCTAssertEqual(harness.controller.lifecycleCapabilities.owner, .diagnostics, "DetailsClose did not release")
        XCTAssertEqual(harness.rig.releases(.diagnostics), 0)
        harness.rig.drain()
        XCTAssertEqual(harness.rig.releases(.diagnostics), 1, "only the controller's settlement released it")

        // DiagnosticsClose: refused during its own run.
        let diagnosticsHarness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { diagnosticsHarness.tearDown() }
        diagnosticsHarness.service.hold("pause")
        let diagnostics = DiagnosticsWindowController(controller: diagnosticsHarness.controller)
        let run = try XCTUnwrap(diagnosticsHarness.buttons(in: diagnostics.window?.contentView).first { $0.title == "Run Again" })
        spinUntil(2) { run.isEnabled }
        run.performClick(nil)
        let diagnosticsWindow = try XCTUnwrap(diagnostics.window)
        XCTAssertFalse(diagnostics.windowShouldClose(diagnosticsWindow), "DiagnosticsClose: refused during its run")
        XCTAssertEqual(diagnosticsHarness.rig.releases(.diagnostics), 0)
        diagnosticsHarness.rig.drain()
        spinUntil(3) { diagnostics.windowShouldClose(diagnosticsWindow) }
        XCTAssertTrue(diagnostics.windowShouldClose(diagnosticsWindow), "and allowed afterwards")
        diagnostics.close()

        // OnboardingClose: busy and pending-handover restrictions; an idle window closes
        // without releasing an owner started elsewhere.
        let onboardingHarness = try WindowOwnerRig(.init(loaded: .loaded))
        defer { onboardingHarness.tearDown() }
        let onboarding = try XCTUnwrap(onboardingHarness.window.showOnboarding(route: .provider))
        let sheet = try XCTUnwrap(onboarding.window)
        onboarding.setBusy(true)
        XCTAssertFalse(onboarding.windowShouldClose(sheet), "OnboardingClose: refused while busy")
        onboarding.setBusy(false)
        onboardingHarness.service.hold("pause")
        onboardingHarness.controller.configureFleetAccount(
            credentials: HubFleetSetupCredentials(accessToken: "a", refreshToken: "r", clientID: "c",
                                                  region: "europe_middle_east_and_africa", expiresInSeconds: 3600)) { _ in }
        XCTAssertEqual(onboardingHarness.controller.lifecycleCapabilities.owner, .account)
        XCTAssertTrue(onboarding.windowShouldClose(sheet), "an idle onboarding window may close")
        sheet.close()
        XCTAssertEqual(onboardingHarness.controller.lifecycleCapabilities.owner, .account,
                       "closing the window did not release the account owner")
        XCTAssertEqual(onboardingHarness.rig.releases(.account), 0)
        onboardingHarness.rig.drain()
        XCTAssertEqual(onboardingHarness.rig.releases(.account), 1)
    }

    // MARK: R-E1F6-19

    func testHeldDiagnosticsHarnessWaitsForFinalCompletionBeforeReturning() throws {
        let harness = try DeleteHarness(testCase: self)
        // The resume is delayed too: the run has two held steps.
        harness.service.holdStart = true
        harness.startHeldDiagnostics()
        XCTAssertTrue(harness.controller.diagnosticsPreventQuit)
        XCTAssertFalse(harness.heldDiagnosticsFinished)

        // The same cleanup helper the two custody tests use.
        harness.tearDown()
        XCTAssertTrue(harness.heldDiagnosticsFinished, "the cleanup returned only after the final completion")
        XCTAssertNil(harness.controller.lifecycleCapabilities.owner)
        XCTAssertNil(harness.service.heldPause)
        XCTAssertNil(harness.service.heldStart)
    }
}

extension WindowOwnerRig {
    func recordStale(_ result: Result<Void, Error>) {
        if case .failure = result { staleRefusals += 1 }
    }
}

extension OnboardingWindowController {
    /// The Connect action of the migration page (the footer's primary button).
    func sendConnect() {
        _ = perform(NSSelectorFromString("checkMigrationCompatibility"))
    }
}

extension OnboardingWindowControllerTests {
    func fillServerField(in onboarding: OnboardingWindowController?, host: String) throws {
        func views(_ view: NSView) -> [NSView] { [view] + view.subviews.flatMap(views) }
        let root = try XCTUnwrap(onboarding?.window?.contentView)
        let field = try XCTUnwrap(views(root).compactMap { $0 as? NSTextField }
            .first { $0.placeholderString == "teslamate.local" })
        field.stringValue = host
    }
}

// MARK: - R-E1F6-11 (window level): vehicle control against the snapshot and its outcome

extension OnboardingWindowControllerTests {
    func testSnapshotRefreshCannotEnableLifecycleActionsDuringVehicleControl() throws {
        var options = OwnerRig.Options(loaded: .loaded)
        options.runningStatus = true
        let harness = try WindowOwnerRig(options)
        defer { harness.tearDown() }
        XCTAssertEqual(harness.controller.snapshot.health, .running, "the fixture shows a running, connected Hub")
        harness.runner.hold("control")
        harness.controller.performVehicleControl(.lock) { _ in }
        XCTAssertEqual(harness.controller.lifecycleCapabilities.owner, .vehicleControl, "the command is pending")

        // The Hub now reports itself stopped: a refresh would expose Start, Restart and Stop.
        harness.service.loaded = .unloaded
        harness.runner.results["status"] = .failure(HubActionError.commandFailed("no report"))
        let onboarding = try XCTUnwrap(harness.window.showOnboarding(route: .provider))
        let cancel = try XCTUnwrap(harness.buttons(in: onboarding.window?.contentView).first { $0.title == "Cancel" })
        cancel.performClick(nil)      // dismissing the account page makes the window refresh
        spinUntil(3) { harness.window.dashboardView != nil && harness.dashboardButton("Start Hub") != nil }
        let start = try XCTUnwrap(harness.dashboardButton("Start Hub"), "the refreshed dashboard offers Start")
        XCTAssertFalse(start.isEnabled, "but not while the vehicle command is pending")
        let before = harness.service.calls
        harness.activate(start)
        harness.window.makeServiceLifecycleActions().restart()
        XCTAssertEqual(harness.service.calls, before, "and nothing reaches the service")
        harness.rig.drain()
        XCTAssertTrue(harness.dashboardButton("Start Hub")?.isEnabled == true, "available again once the command has settled")
    }

    func testUnknownVehicleOutcomeStillSuppressesAutomaticRetry() throws {
        var options = OwnerRig.Options(loaded: .loaded)
        options.runningStatus = true
        let harness = try WindowOwnerRig(options)
        defer { harness.tearDown() }
        harness.runner.results["control"] = .failure(HubActionError.commandTimedOut)
        let card = try XCTUnwrap(Mirror(reflecting: harness.window.vehiclesView!).descendant("detailCard") as? HubVehicleCardView)
        harness.window.selectMainSection(.vehicles)
        func commandButtons() -> [NSButton] {
            harness.buttons(in: harness.window.vehiclesView).filter { ["Climate", "Access", "More"].contains($0.title) }
        }
        XCTAssertTrue(commandButtons().allSatisfy(\.isEnabled), "commands are offered before any command")
        let item = NSMenuItem(title: "Lock", action: nil, keyEquivalent: "")
        item.representedObject = HubVehicleControl.lock.rawValue
        _ = card.perform(NSSelectorFromString("runCommand:"), with: item)
        harness.confirm(HubVehicleControl.lock.title)
        harness.rig.drain()
        spinBriefly(0.2)
        XCTAssertEqual(harness.runner.count("control"), 1, "the one recorded command")
        XCTAssertTrue(MainWindowController.vehicleControlOutcomeIsUnknown(HubActionError.commandTimedOut))
        XCTAssertTrue(commandButtons().allSatisfy { !$0.isEnabled }, "an unknown outcome disables the commands")

        // Neither a refresh nor the owner settling brings a command back or repeats one.
        var refreshed = 0
        harness.controller.refresh { _ in refreshed += 1 }
        spinUntil(3) { refreshed == 1 }
        harness.window.selectMainSection(.dashboard)
        harness.window.selectMainSection(.vehicles)
        spinBriefly(0.2)
        XCTAssertTrue(commandButtons().allSatisfy { !$0.isEnabled }, "still suppressed")
        XCTAssertEqual(harness.runner.count("control"), 1, "no automatic retry")
        XCTAssertTrue(harness.controller.lifecycleCapabilities.isIdle)
    }
}
