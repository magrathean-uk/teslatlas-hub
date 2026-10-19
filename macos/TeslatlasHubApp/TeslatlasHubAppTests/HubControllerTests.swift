// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import Darwin
import XCTest
@testable import Teslatlas_Hub

final class HubControllerTests: XCTestCase {
    func testLiveOnboardingUsesReadinessWithoutOfflineDoctorOrServiceMutation() throws {
        for ready in [true, false] {
            let home = try temporaryHome()
            defer { try? FileManager.default.removeItem(at: home) }
            let folder = home.appendingPathComponent("Library/Logs/Teslatlas Hub")
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            try Data("started\n".utf8).write(to: folder.appendingPathComponent("hub.out.log"))
            let runner = CommandMapRunner(responses: [
                "status": .success("""
                {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1024},"ready":\(ready),"provider":"legacy","vehicles":[{"vehicleId":"7a5d69ab-8ea8-4056-8b2f-42c41c28ae36","displayName":"Example"}],"credentials":{"present":true}}
                """),
                "doctor": .failure(HubActionError.commandFailed("hub catalogue changed during the immutable diagnostic check"))
            ])
            let events = EventRecorder()
            let controller = HubController(commandRunner: runner,
                serviceRunner: ScriptedService(events: events, loadState: .loaded),
                homeDirectory: home, serviceInstalledOverride: true)
            let done = expectation(description: "live checks complete")
            controller.runOnboardingChecks(expectRunning: true) { result in
                guard case let .success(checks) = result else { XCTFail("missing checks"); done.fulfill(); return }
                XCTAssertEqual(checks.first { $0.title == "Service" }?.passed, ready)
                XCTAssertEqual(checks.first { $0.title == "Diagnostics" }?.passed, ready)
                done.fulfill()
            }
            wait(for: [done], timeout: 2)
            XCTAssertEqual(runner.commands, ["status"])
            XCTAssertFalse(events.values.contains { $0.contains("service:") })
        }
    }

    func testActionButtonsUseSharedStyleTokensWhenEnabledAndDisabled() {
        let flat = HubActionButton(title: "Action", target: nil, action: nil)
        flat.hubStyle = .flat
        XCTAssertEqual(flat.contentTintColor,
                       flat.attributedTitle.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor)
        flat.isEnabled = false
        XCTAssertEqual(flat.contentTintColor, .disabledControlTextColor)

        let primary = HubActionButton(title: "Continue", target: nil, action: nil)
        primary.hubStyle = .primary
        XCTAssertEqual(primary.contentTintColor, .white)
        XCTAssertEqual(primary.layer?.backgroundColor, HubPalette.accent.cgColor)
        primary.isEnabled = false
        XCTAssertEqual(primary.layer?.backgroundColor, NSColor.disabledControlTextColor.cgColor)
    }

    func testMainWindowBuildsInPreviewMode() {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let windowController = MainWindowController(controller: controller)
        XCTAssertEqual(windowController.window?.title, "Teslatlas Hub")
        // The window's native size is its frame (the design snapshots render the whole frame).
        // `contentLayoutRect` also depends on whether the host application is active (an
        // active host lays the unified toolbar over the content, an inactive accessory test
        // host, R-E1F6-21, insets the content view), so the frame is the stable quantity.
        assertNativeMainWindowFrame(windowController.window?.frame.size)
        XCTAssertEqual(windowController.window?.contentMinSize, NSSize(width: 820, height: 590))
    }

    func testOnboardingPreviewRoutesUseFirstRunBackgroundOnlyInPreviewMode() {
        for route in ["welcome", "choose", "provider", "fleet", "legacy", "migration", "migration-connected"] {
            let controller = HubController(environment: [
                "TESLATLAS_HUB_UI_PREVIEW": "1",
                "TESLATLAS_HUB_ONBOARDING_PREVIEW": route
            ])
            XCTAssertEqual(controller.snapshot.health, .needsInstall, route)
            XCTAssertEqual(controller.snapshot.account, "Not configured", route)
        }

        let ordinaryPreview = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        XCTAssertEqual(ordinaryPreview.snapshot.health, .running)
        XCTAssertEqual(ordinaryPreview.snapshot.account, "Connected")

        let production = HubController(environment: [
            "TESLATLAS_HUB_ONBOARDING_PREVIEW": "welcome"
        ])
        XCTAssertFalse(production.previewMode)
        XCTAssertNil(production.onboardingPreviewRoute)
        XCTAssertEqual(production.snapshot.health, .needsInstall)
    }

    func testServiceDetailsRejectsMutationWhenOwnerGateIsClosed() throws {
        let calls = LifecycleCallRecorder()
        let controller = HubController(commandRunner: CountingRunner(),
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       serviceInstalledOverride: true)
        let details = ServiceDetailsWindowController(
            snapshot: .previewRunning,
            controller: controller,
            lifecycleActions: calls.actions,
            mutationAllowed: { false }
        )
        for identifier in ["hub.service.start-stop", "hub.service.restart",
                           "hub.service.remove", "hub.service.delete-data"] {
            let button = try XCTUnwrap(buttons(in: details.window?.contentView)
                .first { $0.identifier?.rawValue == identifier }, identifier)
            button.performClick(nil)
        }
        XCTAssertEqual(calls.names, [], "a closed owner gate must reach no lifecycle action")

        details.setMutationsEnabled(false)
        for identifier in ["hub.service.start-stop", "hub.service.restart",
                           "hub.service.remove", "hub.service.delete-data"] {
            let button = try XCTUnwrap(buttons(in: details.window?.contentView)
                .first { $0.identifier?.rawValue == identifier }, identifier)
            XCTAssertFalse(button.isEnabled, identifier)
        }
    }

    func testServiceDetailsShowsLifecycleTransitionFeedbackForEveryCommand() throws {
        let details = ServiceDetailsWindowController(
            snapshot: .previewRunning,
            controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
            lifecycleActions: .init(start: {}, stop: {}, restart: {})
        )
        let pending = try XCTUnwrap(descendantViews(in: try XCTUnwrap(details.window?.contentView)).first {
            $0.identifier?.rawValue == "hub.service.pending"
        })

        for transition: HubServiceTransition in [.starting, .stopping, .restarting, .removing, .deleting] {
            details.setServiceTransition(transition)
            XCTAssertFalse(pending.isHidden)
            XCTAssertEqual(pending.accessibilityLabel(), transition.title)
            XCTAssertTrue(labels(in: pending).contains { $0.stringValue == transition.title })
        }
        details.setServiceTransition(nil)
        XCTAssertTrue(pending.isHidden)
    }

    func testServiceDetailsUseRealSnapshotAndProvider() {
        // Catches replacing the current snapshot with a prototype or stale service value.
        var snapshot = HubSnapshot.previewRunning
        snapshot.provider = .fleet

        let rows = ServiceDetailsWindowController.details(for: snapshot)

        XCTAssertEqual(rows.first { $0.label == "Provider" }?.value, "Fleet API")
        XCTAssertEqual(rows.first { $0.label == "Version" }?.value,
                       "Teslatlas Hub \(snapshot.version)")
    }

    func testServiceDetailsOmitUnavailableTeslaAccountIdentity() {
        // Catches fabricating an account identity when Hub only knows the configuration state.
        let rows = ServiceDetailsWindowController.details(for: .firstRun)

        XCTAssertEqual(rows.first { $0.label == "Tesla account" }?.value, "Not configured")
        XCTAssertFalse(rows.contains { $0.value.contains("@") })
    }

    func testServiceDetailsRetainLifecycleControlsRemoveAndDelete() throws {
        // Catches losing a lifecycle or irreversible deletion path in the redesign.
        let calls = LifecycleCallRecorder()
        let controller = HubController(commandRunner: CountingRunner(),
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       serviceInstalledOverride: true)
        let details = ServiceDetailsWindowController(
            snapshot: .previewRunning,
            controller: controller,
            lifecycleActions: calls.actions,
            confirmationPresenter: { alert, _ in calls.confirmations.append(alert.messageText); return .alertSecondButtonReturn }
        )

        let titles = buttons(in: details.window?.contentView).map(\.title)

        XCTAssertTrue(titles.contains("Stop Hub…"))
        XCTAssertTrue(titles.contains("Restart Hub"))
        XCTAssertTrue(titles.contains("Remove Background Service…"))
        XCTAssertTrue(titles.contains("Delete Hub and Data…"))
        XCTAssertFalse(titles.contains("Update Service…"))
        XCTAssertFalse(titles.contains("Uninstall Hub…"))
        let remove = try XCTUnwrap(buttons(in: details.window?.contentView)
            .first { $0.identifier?.rawValue == "hub.service.remove" })
        remove.performClick(nil)
        let delete = try XCTUnwrap(buttons(in: details.window?.contentView)
            .first { $0.identifier?.rawValue == "hub.service.delete-data" })
        delete.performClick(nil)
        XCTAssertEqual(calls.names, ["remove", "deleteHubAndData"])
        XCTAssertEqual(calls.confirmations, ["Remove the background service?"],
                       "the main window owns the two Delete confirmations")
    }

    func testRemoveConfirmationDefaultsToCancel() throws {
        let alert = ServiceDetailsWindowController.removeConfirmation()
        XCTAssertEqual(alert.messageText, "Remove the background service?")
        XCTAssertEqual(alert.informativeText,
                       "Teslatlas Hub stops and no longer starts at login. Your Hub data, configuration and logs stay. Start Hub sets it up again.")
        XCTAssertEqual(alert.buttons.map(\.title), ["Cancel", "Remove"])
        XCTAssertEqual(alert.buttons[0].keyEquivalent, "\r")
        XCTAssertEqual(alert.buttons[1].keyEquivalent, "")

        // Cancelling calls nothing.
        let calls = LifecycleCallRecorder()
        let details = ServiceDetailsWindowController(
            snapshot: .previewRunning,
            controller: HubController(commandRunner: CountingRunner(),
                                      serviceRunner: ScriptedService(events: EventRecorder()),
                                      serviceInstalledOverride: true),
            lifecycleActions: calls.actions,
            confirmationPresenter: { _, silent in silent }
        )
        try XCTUnwrap(buttons(in: details.window?.contentView)
            .first { $0.identifier?.rawValue == "hub.service.remove" }).performClick(nil)
        XCTAssertEqual(calls.names, [])
    }

    func testSettingsBackgroundServiceExposesRunningLifecycleControlsWithConfirmation() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let dashboard = MainWindowController(controller: controller)
        dashboard.selectMainSection(.settings)

        let serviceRow = try XCTUnwrap(buttons(in: dashboard.settingsPage).first {
            $0.accessibilityLabel() == "Background service"
        })
        serviceRow.performClick(nil)

        let lifecycle = descendantViews(in: try XCTUnwrap(dashboard.window?.contentView)).first {
            $0.identifier?.rawValue == "hub.service.lifecycle"
        }
        let stop = try XCTUnwrap(buttons(in: dashboard.window?.contentView).first {
            $0.identifier?.rawValue == "hub.service.start-stop"
        })
        let restart = try XCTUnwrap(buttons(in: dashboard.window?.contentView).first {
            $0.identifier?.rawValue == "hub.service.restart"
        })
        XCTAssertFalse(lifecycle?.isHidden ?? true)
        XCTAssertEqual(stop.title, "Stop Hub…")
        XCTAssertTrue(stop.isEnabled)
        XCTAssertFalse(restart.isHidden)
        XCTAssertTrue(restart.isEnabled)

        stop.performClick(nil)
        let confirmation = try XCTUnwrap(dashboard.window?.attachedSheet)
        XCTAssertTrue(labels(in: confirmation.contentView).contains {
            $0.stringValue == "Stop collecting vehicle data?"
        })
        try XCTUnwrap(buttons(in: confirmation.contentView).first { $0.title == "Cancel" })
            .performClick(nil)
    }

    func testServiceDetailsUseTheSharedSheetSizeAndIdentifyTheDeleteDataAction() throws {
        // Catches using a standalone detail window or an unidentifiable destructive action.
        let details = ServiceDetailsWindowController(
            snapshot: .previewRunning,
            controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        )

        XCTAssertEqual(details.window?.contentRect(forFrameRect: details.window!.frame).size,
                       HubMetrics.serviceDetailsSheetSize)
        XCTAssertNotNil(buttons(in: details.window?.contentView).first {
            $0.identifier?.rawValue == "hub.service.delete-data"
        })
    }

    func testServiceDetailsUsePrimaryModalCoordinationAndPreserveSelectedPage() throws {
        // Catches bypassing the shared sheet coordinator or resetting the selected dashboard page.
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let dashboard = MainWindowController(controller: controller)
        dashboard.selectMainSection(.vehicles)

        let details = try XCTUnwrap(dashboard.showServiceDetails())

        XCTAssertEqual(dashboard.activeModalKind, .serviceDetails)
        XCTAssertEqual(dashboard.selectedSection, .vehicles)
        XCTAssertNil(details.window?.sheetParent)
        XCTAssertTrue(details.window?.isMovable == true)
        details.window?.performClose(nil)
        XCTAssertNil(dashboard.activeModalKind)
        XCTAssertEqual(dashboard.selectedSection, .vehicles)
    }

    func testServiceDetailsRefuseToReplaceBusyOrNonDismissibleOnboarding() throws {
        // Catches details dismissing setup while its onboarding sheet must remain modal.
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let dashboard = MainWindowController(controller: controller)
        let busyOnboarding = try XCTUnwrap(dashboard.showOnboarding(route: .provider))
        busyOnboarding.setBusy(true, message: "Checking credentials…")
        defer {
            busyOnboarding.setBusy(false)
            busyOnboarding.close()
        }

        XCTAssertNil(dashboard.showServiceDetails())
        XCTAssertEqual(dashboard.activeModalKind, .onboarding)

        let firstRunController = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                               initialSnapshot: .firstRun)
        let firstRunDashboard = MainWindowController(controller: firstRunController)
        _ = try XCTUnwrap(firstRunDashboard.showFirstRunOnboarding())
        let firstRunIdentifier = try XCTUnwrap(firstRunDashboard.activeOnboardingIdentifier)
        defer {
            firstRunDashboard.completeOnboarding(identifier: firstRunIdentifier, completion: .configured)
        }

        XCTAssertNil(firstRunDashboard.showServiceDetails())
        XCTAssertEqual(firstRunDashboard.activeModalKind, .onboarding)
    }

    func testPreviewIsReadOnly() {
        let runner = CountingRunner()
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"], commandRunner: runner)
        XCTAssertTrue(controller.previewMode)
        XCTAssertEqual(controller.snapshot.health, .running)

        let expectation = expectation(description: "actions rejected")
        var failures = 0
        let done: (Result<Void, Error>) -> Void = { result in
            if case .failure = result { failures += 1 }
            if failures == 8 { expectation.fulfill() }
        }
        controller.removeBackgroundService(completion: done)
        controller.deleteHubAndData { result in
            if case .failure = result { failures += 1 }
            if failures == 8 { expectation.fulfill() }
        }
        controller.importTeslaMate(source: "postgres://example", carID: "1", passwordFile: "/tmp/password", encryptionKeyFile: "/tmp/encryption", acknowledgeV42CompatibleSchema: true, completion: done)
        controller.configureTeslaAccount(tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh"), completion: done)
        controller.performVehicleControl(.climateStart, completion: done)
        controller.stopHub(completion: done)
        controller.restartHub(completion: done)
        controller.signOutTeslaAccount(completion: done)
        wait(for: [expectation], timeout: 1)
        XCTAssertEqual(runner.calls, 0)
    }

    func testLaunchctlOnlyClassifiesTheExactMissingServiceFailureAsUnloaded() {
        let uid = getuid()
        let service = "gui/\(uid)/com.teslatlas.hub"
        let expected = "Bad request.\nCould not find service \"com.teslatlas.hub\" in domain for user gui: \(uid)\n"
        XCTAssertTrue(LaunchctlServiceController.isKnownUnloadedPrintFailure(
            status: 113, output: expected, service: service
        ))
        XCTAssertFalse(LaunchctlServiceController.isKnownUnloadedPrintFailure(
            status: 1, output: expected, service: service
        ))
        XCTAssertFalse(LaunchctlServiceController.isKnownUnloadedPrintFailure(
            status: 113, output: "database not found: error 113", service: service
        ))
    }

    func testPreviewIgnoresARealMigrationHandoverMarker() throws {
        let home = HubTestScratch.directory
            .appendingPathComponent("teslatlas-hub-preview-marker-\(UUID().uuidString)", isDirectory: true)
        let state = home.appendingPathComponent("Library/Application Support/Teslatlas Hub", isDirectory: true)
        try FileManager.default.createDirectory(at: state, withIntermediateDirectories: true)
        try Data("{}".utf8).write(to: state.appendingPathComponent(".teslamate-handover-pending"))
        defer { try? FileManager.default.removeItem(at: home) }

        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                       homeDirectory: home)

        XCTAssertFalse(controller.hasPendingMigrationHandover)
        XCTAssertNil(controller.pendingMigrationHandoverPhase)
    }

    func testMigrationHandoverFIFOIsFailClosedWithoutBlocking() throws {
        let home = HubTestScratch.directory
            .appendingPathComponent("teslatlas-hub-marker-fifo-\(UUID().uuidString)", isDirectory: true)
        let state = home.appendingPathComponent("Library/Application Support/Teslatlas Hub", isDirectory: true)
        try FileManager.default.createDirectory(at: state, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let marker = state.appendingPathComponent(".teslamate-handover-pending")
        XCTAssertEqual(Darwin.mkfifo(marker.path, S_IRUSR | S_IWUSR), 0)
        let controller = HubController(homeDirectory: home,
                                       serviceInstalledOverride: false)

        let started = Date()
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertEqual(controller.pendingMigrationHandoverPhase, .awaitingVerification)
        XCTAssertLessThan(Date().timeIntervalSince(started), 1)
    }

    func testRunFullDiagnosticsInvokesDoctorPreflightStatusAndKeepsPreviewReadOnly() {
        let runner = CommandMapRunner(responses: [
            "doctor": .success("{\"status\":\"ok\",\"catalogue\":{\"journalMode\":\"wal\"},\"credentials\":{\"legacy\":{\"present\":true},\"fleet\":{\"present\":true}}}"),
            "preflight": .success("{\"status\":\"ready\",\"provider\":\"fleet\"}"),
            "status": .success("{\"status\":\"ok\",\"provider\":\"fleet\",\"credentials\":{\"present\":true}}")
        ])
        let home = HubTestScratch.directory
            .appendingPathComponent("teslatlas-hub-diagnostics-\(UUID().uuidString)", isDirectory: true)
        try? FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       homeDirectory: home,
                                       serviceInstalledOverride: false)
        let finished = expectation(description: "full diagnostics finished")
        controller.runFullDiagnostics { text in
            XCTAssertTrue(text.contains("doctor — Hub database, tokens, TLS, collector"))
            XCTAssertTrue(text.contains("\"journalMode\":\"wal\""))
            XCTAssertTrue(text.contains("preflight — selected provider credentials"))
            XCTAssertTrue(text.contains("\"provider\":\"fleet\""))
            XCTAssertTrue(text.contains("status — vehicles and credential presence"))
            XCTAssertTrue(text.contains("Duration:"))
            XCTAssertTrue(text.contains("Read duration:"))
            XCTAssertTrue(text.contains("TeslaMate is not written"))
            XCTAssertTrue(text.contains("tokens are not deleted") || text.contains("Tokens are not deleted") || text.contains("Owner and Fleet tokens"))
            XCTAssertTrue(text.contains("== support metadata =="))
            XCTAssertTrue(text.contains("Expected Hub: \(HubRelease.bundledVersion)"))
            XCTAssertTrue(text.contains("Service: not installed"))
            XCTAssertTrue(text.contains("Provider: Not configured"))
            XCTAssertTrue(text.contains("macOS:"))
            XCTAssertTrue(text.contains("Architecture: arm64"))
            XCTAssertTrue(text.contains("Available storage:"))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)
        XCTAssertEqual(runner.commands, ["doctor", "preflight", "status"])
    }

    func testRunningHubPausesForDiagnosticsAndAlwaysResumes() {
        let runner = CommandMapRunner(responses: [
            "doctor": .success("{\"status\":\"ok\"}"),
            "preflight": .success("{\"status\":\"ready\"}"),
            "status": .success("{\"status\":\"ok\"}")
        ])
        let events = EventRecorder()
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: events, loadState: .loaded),
                                       serviceInstalledOverride: false)
        let finished = expectation(description: "running Hub diagnostics finished")
        controller.runFullDiagnostics { text in
            XCTAssertTrue(text.contains("Hub collection resumed."))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:pause", "service:start"])
        XCTAssertEqual(runner.commands, ["doctor", "preflight", "status"])
    }

    func testStoppedHubRemainsStoppedAfterDiagnostics() {
        let runner = CommandMapRunner(responses: [
            "doctor": .success("{\"status\":\"ok\"}"),
            "preflight": .success("{\"status\":\"ready\"}"),
            "status": .success("{\"status\":\"ok\"}")
        ])
        let events = EventRecorder()
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: events, loadState: .unloaded),
                                       serviceInstalledOverride: false)
        let finished = expectation(description: "stopped Hub diagnostics finished")
        controller.runFullDiagnostics { _ in finished.fulfill() }
        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, [])
    }

    func testPreviewDiagnosticsDoNotInvokeHubCommands() {
        let runner = CountingRunner()
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"], commandRunner: runner)
        let finished = expectation(description: "preview diagnostics")
        controller.runFullDiagnostics { text in
            XCTAssertTrue(text.contains("Preview mode"))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 1)
        XCTAssertEqual(runner.calls, 0)
    }

    func testOpeningDiagnosticsDoesNotRunTheExpensiveChecks() {
        let runner = CountingRunner()
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       serviceInstalledOverride: false)
        let diagnostics = DiagnosticsWindowController(controller: controller)

        XCTAssertNotNil(diagnostics.window)
        XCTAssertEqual(runner.calls, 0)
    }

    func testEmbeddedDiagnosticsOwnsNavigationAndQuitUntilServiceResumeCompletes() throws {
        let keeper = makeQuitControlKeeperWindow()
        defer { keeper.orderOut(nil) }
        let runner = PendingCommandRunner()
        let service = PendingServiceRunner(loadState: .loaded)
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: service,
                                       serviceInstalledOverride: false,
                                       initialSnapshot: .previewRunning)
        let initialStatus = expectation(description: "initial status settled before diagnostics")
        let dashboard = MainWindowController(controller: controller) { _ in initialStatus.fulfill() }
        dashboard.showWindow(nil)
        runner.complete(.success("""
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[],"credentials":{"present":true}}
        """))
        wait(for: [initialStatus], timeout: 1)
        dashboard.showEmbeddedDiagnostics()
        let root = try XCTUnwrap(dashboard.window?.contentView)
        let run = try XCTUnwrap(buttons(in: root).first { $0.title == "Run Again" })
        let back = try XCTUnwrap(buttons(in: root).first { $0.title == "Back" })
        let overview = try XCTUnwrap(buttons(in: root).first { $0.title == "Overview" })

        run.performClick(nil)

        XCTAssertEqual(service.arguments, [["service", "pause"]])
        XCTAssertTrue(dashboard.operationPreventsQuit)
        XCTAssertFalse(back.isEnabled)
        XCTAssertFalse(overview.isEnabled)
        XCTAssertFalse(AppDelegate.finishSheetsBeforeQuit(in: [try XCTUnwrap(dashboard.window)], lifecycle: .idle))

        let mainWindow = try XCTUnwrap(dashboard.window)
        let appDelegate = AppDelegate(keyWindow: { mainWindow })
        let closeItem = NSMenuItem()
        closeItem.action = #selector(AppDelegate.closeKeyWindow(_:))
        XCTAssertFalse(appDelegate.validateMenuItem(closeItem))
        appDelegate.closeKeyWindow(nil)
        XCTAssertTrue(mainWindow.isVisible)

        service.complete(.success(""))
        for command in ["doctor", "preflight", "status"] {
            XCTAssertEqual(runner.arguments.last?.last, command)
            runner.complete(.success("{\"status\":\"ok\"}"))
        }
        let awaitingResume = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            service.arguments.count == 2
        }, object: nil)
        wait(for: [awaitingResume], timeout: 2)
        XCTAssertTrue(dashboard.operationPreventsQuit)
        service.complete(.success(""))

        let unlocked = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            !dashboard.operationPreventsQuit && run.isEnabled
        }, object: nil)
        wait(for: [unlocked], timeout: 2)
        XCTAssertTrue(back.isEnabled)
        XCTAssertTrue(overview.isEnabled)
        XCTAssertTrue(AppDelegate.finishSheetsBeforeQuit(in: [try XCTUnwrap(dashboard.window)], lifecycle: .idle))
        XCTAssertTrue(appDelegate.validateMenuItem(closeItem))
        XCTAssertTrue(descendantViews(in: root).compactMap { $0 as? NSTextView }.contains {
            $0.string.contains("Hub collection resumed.")
        })
        mainWindow.performClose(nil)
        XCTAssertFalse(mainWindow.isVisible)
    }

    func testLogsDiagnosticsCustodySurvivesClosedWindowUntilResumeAndAllowsIdleQuit() throws {
        let keeper = makeQuitControlKeeperWindow()
        defer { keeper.orderOut(nil) }
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let runner = PendingCommandRunner()
        let service = PendingServiceRunner(loadState: .loaded)
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: false)
        var logs: LogsWindowController? = LogsWindowController(controller: controller)
        let delegate = AppDelegate()
        delegate.hubController = controller
        let item = try XCTUnwrap(logs?.secondaryActionsMenu.items.first { $0.title == "Run Diagnostics" })
        let initialLogs = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            item.isEnabled
        }, object: nil)
        wait(for: [initialLogs], timeout: 2)
        XCTAssertTrue(AppDelegate.finishSheetsBeforeQuit(in: [], lifecycle: controller.lifecycleCapabilities))
        XCTAssertTrue(delegate.applicationShouldTerminateAfterLastWindowClosed(NSApp))
        XCTAssertTrue(NSApp.sendAction(try XCTUnwrap(item.action), to: item.target, from: item))
        XCTAssertEqual(service.arguments, [["service", "pause"]])
        XCTAssertFalse(AppDelegate.finishSheetsBeforeQuit(in: [], lifecycle: controller.lifecycleCapabilities))
        XCTAssertFalse(delegate.applicationShouldTerminateAfterLastWindowClosed(NSApp))
        XCTAssertEqual(delegate.applicationShouldTerminate(NSApp), .terminateCancel)

        logs?.close()
        logs = nil
        service.complete(.success(""))
        for command in ["doctor", "preflight", "status"] {
            XCTAssertEqual(runner.arguments.last?.last, command)
            runner.complete(.success("{\"status\":\"ok\"}"))
        }
        let awaitingResume = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            service.arguments.count == 2
        }, object: nil)
        wait(for: [awaitingResume], timeout: 2)
        XCTAssertEqual(service.arguments.last, ["service", "start"])
        XCTAssertFalse(AppDelegate.finishSheetsBeforeQuit(in: [], lifecycle: controller.lifecycleCapabilities))
        service.complete(.success(""))
        XCTAssertTrue(AppDelegate.finishSheetsBeforeQuit(in: [], lifecycle: controller.lifecycleCapabilities))
        XCTAssertTrue(delegate.applicationShouldTerminateAfterLastWindowClosed(NSApp))
        XCTAssertEqual(delegate.applicationShouldTerminate(NSApp), .terminateNow)
    }

    private func makeQuitControlKeeperWindow() -> NSWindow {
        // Closing the tested window must not terminate the XCTest host while
        // the next selected test is running. Quit assertions use the real guard.
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 160, height: 80),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.title = "Owned quit-control test keeper"
        window.orderFront(nil)
        return window
    }

    func testDiagnosticsRowsHaveNonzeroDocumentAndRowFramesAfterRendering() throws {
        let runner = CommandMapRunner(responses: [
            "doctor": .success("{\"status\":\"ok\"}"),
            "preflight": .success("{\"status\":\"ready\"}"),
            "status": .success("{\"status\":\"ok\"}")
        ])
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       serviceInstalledOverride: false)
        let diagnostics = DiagnosticsWindowController(controller: controller)
        let root = try XCTUnwrap(diagnostics.window?.contentView)

        try XCTUnwrap(buttons(in: root).first { $0.title == "Run Again" }).performClick(nil)
        wait(for: [XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.descendantViews(in: root).contains { $0.identifier?.rawValue == "hub.diagnostics.row" }
        }, object: nil)], timeout: 3)
        let rendered = expectation(description: "diagnostic rows rendered")
        DispatchQueue.main.async {
            diagnostics.window?.layoutIfNeeded()
            root.layoutSubtreeIfNeeded()
            let views = self.descendantViews(in: root)
            let document = views.first {
                $0.identifier?.rawValue == "hub.diagnostics.rows-document"
            }
            let row = views.first {
                $0.identifier?.rawValue == "hub.diagnostics.row"
            }
            XCTAssertNotNil(document)
            XCTAssertNotNil(row)
            XCTAssertEqual(document?.isFlipped, true)
            XCTAssertGreaterThan(document?.frame.width ?? 0, 0)
            XCTAssertGreaterThan(document?.frame.height ?? 0, 0)
            XCTAssertGreaterThan(row?.frame.width ?? 0, 0)
            XCTAssertGreaterThan(row?.frame.height ?? 0, 0)
            rendered.fulfill()
        }
        wait(for: [rendered], timeout: 1)
    }

    func testDiagnosticsRerunClearsPreviouslyRenderedRawAndStructuredReport() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let runner = CommandMapRunner(responses: [
            "doctor": .success("{\"status\":\"old\"}"),
            "preflight": .success("{\"status\":\"ready\"}"),
            "status": .success("{\"status\":\"ok\"}")
        ])
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       homeDirectory: home,
                                       serviceInstalledOverride: false)
        let diagnostics = DiagnosticsWindowController(controller: controller)
        let root = try XCTUnwrap(diagnostics.window?.contentView)
        let run = try XCTUnwrap(buttons(in: root).first { $0.title == "Run Again" })

        run.performClick(nil)
        // NSStackView detaches hidden arranged views; disclose the report before
        // finding its NSTextView through the visible view hierarchy.
        try XCTUnwrap(buttons(in: root).first { $0.title == "Show raw redacted report" }).performClick(nil)
        wait(for: [XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.descendantViews(in: root).compactMap { $0 as? NSTextView }.contains { $0.string.contains("old") }
        }, object: nil)], timeout: 10)
        let firstReport = expectation(description: "first diagnostic report rendered")
        DispatchQueue.main.async {
            let raw = self.descendantViews(in: root).compactMap { $0 as? NSTextView }.first {
                $0.identifier?.rawValue == "hub.diagnostics.raw-report"
            }
            XCTAssertTrue(raw?.string.contains("old") == true)
            XCTAssertTrue(self.labels(in: root).contains { $0.stringValue == "Environment doctor" })
            run.performClick(nil)
            XCTAssertEqual(raw?.string, "")
            XCTAssertTrue(raw?.enclosingScrollView?.isHidden == true)
            XCTAssertFalse(self.labels(in: root).contains { $0.stringValue == "Environment doctor" })
            firstReport.fulfill()
        }
        wait(for: [firstReport], timeout: 1)
    }

    func testSharedReportsRedactValuesWithoutDeletingUsefulErrors() {
        let source = """
        password authentication failed for user reader
        status code: 500
        Authorization: Bearer bearer-secret-value
        {"accessToken":"access-secret-value","refresh_token":"refresh-secret-value"}
        {"ingestToken":"ingest-secret-value","private_key":"private-secret-value"}
        callback=https://fleet.example/callback?code=EU_secret_code&state=public-state
        source=postgresql://reader:database-secret@127.0.0.1/teslamate
        jwt=eyJheader.payload.signature
        vehicle=5YJ3E1EA7KF317000
        display_name="Athena Road Trip"
        {"vehicleName":"Athena JSON"}
        locationName="Home driveway" status=ready
        geofence=Private Yard mode=live
        {"location_name":"Office Garage","geofence":"Secret \\"Annex\\" Wing","geofenceName":"School Run","status":"ready"}
        vehicle_id=477a04f6-b726-50e3-86e0-a5a9143b3239
        sourceCarId=17 teslaEid=12345678901234567 selected_car_id=42 car_id=5 vehicle_id=99
        latitude=51.5074 longitude=-0.1278 url=https://example.test/?lat=51.5&lon=-0.1
        {"sourceCarId":23,"teslaEid":98765432109876543,"latitude":48.8566,"longitude":2.3522}
        installationId=9ca970df-0616-43d5-8493-e1faf00e97f1
        server=10.8.0.1
        account=owner@example.com public_ip=203.0.113.42 public_ipv6=[2001:db8::42]:443
        ipv6=[fd12:3456:789a::1]:5432 link=fe80::42%en0 loopback=::1
        coloured=\u{001B}[2m2026-08-27T19:02:26Z\u{001B}[0m \u{001B}[32mINFO\u{001B}[0m ready\u{0007}
        /Users/example/Library/Logs/Teslatlas Hub/hub.err.log
        """

        let redacted = HubShareRedactor.redact(source, homeDirectory: "/Users/example")

        XCTAssertTrue(redacted.contains("password authentication failed"))
        XCTAssertTrue(redacted.contains("status code: 500"))
        XCTAssertTrue(redacted.contains("Authorization: Bearer [redacted]"))
        XCTAssertTrue(redacted.contains("\"accessToken\":\"[redacted]"))
        XCTAssertTrue(redacted.contains("code=[redacted]&state=[redacted]"))
        XCTAssertTrue(redacted.contains("postgresql://reader:[redacted]@127.0.0.1"))
        XCTAssertTrue(redacted.contains("[redacted-jwt]"))
        XCTAssertTrue(redacted.contains("vehicle=[redacted-vin]"))
        XCTAssertTrue(redacted.contains("display_name=[redacted-name]"))
        XCTAssertTrue(redacted.contains("\"vehicleName\":\"[redacted-name]\""))
        XCTAssertTrue(redacted.contains("locationName=[redacted-location]"))
        XCTAssertTrue(redacted.contains("\"location_name\":\"[redacted-location]\""))
        XCTAssertTrue(redacted.contains("\"geofence\":\"[redacted-location]\""))
        XCTAssertTrue(redacted.contains("\"geofenceName\":\"[redacted-location]\""))
        XCTAssertTrue(redacted.contains("status=ready"))
        XCTAssertTrue(redacted.contains("mode=live"))
        XCTAssertTrue(redacted.contains("\"status\":\"ready\""))
        XCTAssertTrue(redacted.contains("vehicle_id=[redacted-id]"))
        XCTAssertTrue(redacted.contains("sourceCarId=[redacted-id]"))
        XCTAssertTrue(redacted.contains("teslaEid=[redacted-id]"))
        XCTAssertTrue(redacted.contains("selected_car_id=[redacted-id]"))
        XCTAssertTrue(redacted.contains("car_id=[redacted-id]"))
        XCTAssertTrue(redacted.contains("latitude=[redacted-location]"))
        XCTAssertTrue(redacted.contains("longitude=[redacted-location]"))
        XCTAssertTrue(redacted.contains("lat=[redacted-location]&lon=[redacted-location]"))
        XCTAssertTrue(redacted.contains("\"sourceCarId\":[redacted-id]"))
        XCTAssertTrue(redacted.contains("\"latitude\":[redacted-location]"))
        XCTAssertTrue(redacted.contains("installationId=[redacted-id]"))
        XCTAssertTrue(redacted.contains("server=[redacted-private-ip]"))
        XCTAssertTrue(redacted.contains("account=[redacted-email]"))
        XCTAssertTrue(redacted.contains("public_ip=[redacted-ip]"))
        XCTAssertTrue(redacted.contains("public_ipv6=[redacted-ip]"))
        XCTAssertTrue(redacted.contains("ipv6=[redacted-private-ip] link=[redacted-private-ip] loopback=[redacted-private-ip]"))
        XCTAssertTrue(redacted.contains("coloured=2026-08-27T19:02:26Z INFO ready"))
        XCTAssertTrue(redacted.contains("~/Library/Logs"))
        for secret in ["bearer-secret-value", "access-secret-value", "refresh-secret-value",
                       "ingest-secret-value", "private-secret-value",
                       "EU_secret_code", "public-state", "database-secret", "eyJheader.payload.signature",
                       "5YJ3E1EA7KF317000", "Athena Road Trip", "Athena JSON",
                       "Home", "driveway", "Private", "Yard", "Office", "Garage",
                       "Secret", "Annex", "Wing", "School", "Run",
                       "477a04f6-b726-50e3-86e0-a5a9143b3239", "a04f6-b726",
                       "12345678901234567", "sourceCarId=17", "selected_car_id=42",
                       "car_id=5", "vehicle_id=99",
                       "51.5074", "-0.1278", "lat=51.5", "lon=-0.1",
                       "98765432109876543", "48.8566", "2.3522",
                       "9ca970df-0616-43d5-8493-e1faf00e97f1", "10.8.0.1", "owner@example.com",
                       "203.0.113.42", "2001:db8::42", "fd12:3456:789a::1",
                       "fe80::42%en0", "\u{001B}"] {
            XCTAssertFalse(redacted.contains(secret), "leaked \(secret)")
        }
    }

    func testDiagnosticsAndLogsShareRoutesRedactLocationNamesAndGeofences() throws {
        let outputFolder = HubTestScratch.directory
            .appendingPathComponent("hub-redaction-routes-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: outputFolder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: outputFolder) }
        let diagnosticsOutput = outputFolder.appendingPathComponent("diagnostics.txt")
        let logsOutput = outputFolder.appendingPathComponent("logs.txt")
        let runner = CommandMapRunner(responses: [
            "doctor": .success("geofence=Private Yard status=ready"),
            "preflight": .success("{\"locationName\":\"Secret \\\"Annex\\\" Wing\",\"status\":\"ready\"}"),
            "status": .success("geofenceName=School Run; mode=live")
        ])
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       serviceInstalledOverride: false)
        let diagnostics = DiagnosticsWindowController(
            controller: controller,
            savePanelPresenter: { _, _, completion in
                completion(diagnosticsOutput)
            }
        )
        let root = try XCTUnwrap(diagnostics.window?.contentView)
        try XCTUnwrap(buttons(in: root).first { $0.title == "Run Again" }).performClick(nil)
        wait(for: [XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.descendantViews(in: root).compactMap { $0 as? NSTextView }.contains {
                $0.identifier?.rawValue == "hub.diagnostics.raw-report"
                    && $0.string.contains("[redacted-location]")
            }
        }, object: nil)], timeout: 3)
        let raw = try XCTUnwrap(descendantViews(in: root).compactMap { $0 as? NSTextView }.first {
            $0.identifier?.rawValue == "hub.diagnostics.raw-report"
        })
        try XCTUnwrap(buttons(in: root).first { $0.title == "Show raw redacted report" })
            .performClick(nil)
        XCTAssertFalse(raw.enclosingScrollView?.isHidden ?? true)
        for secret in ["Private", "Yard", "Secret", "Annex", "Wing", "School", "Run"] {
            XCTAssertFalse(raw.string.contains(secret))
        }
        XCTAssertTrue(raw.string.contains("status=ready"))
        XCTAssertTrue(raw.string.contains("\"status\":\"ready\""))
        XCTAssertTrue(raw.string.contains("mode=live"))
        try XCTUnwrap(buttons(in: root).first { $0.title == "Copy Report" }).performClick(nil)
        let diagnosticsCopy = try XCTUnwrap(NSPasteboard.general.string(forType: .string))
        try XCTUnwrap(buttons(in: root).first { $0.title == "Save Report…" }).performClick(nil)
        let diagnosticsSaved = try String(contentsOf: diagnosticsOutput, encoding: .utf8)
        for shared in [diagnosticsCopy, diagnosticsSaved] {
            XCTAssertTrue(shared.contains("[redacted-location]"))
            for secret in ["Private", "Yard", "Secret", "Annex", "Wing", "School", "Run"] {
                XCTAssertFalse(shared.contains(secret))
            }
        }

        let logs = LogsWindowController(
            controller: controller,
            savePanelPresenter: { _, _, completion in
                completion(logsOutput)
            }
        )
        logs.renderLogs("locationName=North Annex status=ready\ngeofence=Private Yard; mode=live")
        XCTAssertTrue(logs.textViewForTesting.string.contains("[redacted-location]"))
        for secret in ["North", "Annex", "Private", "Yard"] {
            XCTAssertFalse(logs.textViewForTesting.string.contains(secret))
        }
        XCTAssertTrue(logs.textViewForTesting.string.contains("status=ready"))
        XCTAssertTrue(logs.textViewForTesting.string.contains("mode=live"))
        try XCTUnwrap(buttons(in: logs.window?.contentView).first { $0.title == "Copy" })
            .performClick(nil)
        let copied = try XCTUnwrap(NSPasteboard.general.string(forType: .string))
        try XCTUnwrap(buttons(in: logs.window?.contentView).first { $0.title == "Save…" })
            .performClick(nil)
        let logsSaved = try String(contentsOf: logsOutput, encoding: .utf8)
        for shared in [copied, logsSaved] {
            XCTAssertTrue(shared.contains("[redacted-location]"))
            for secret in ["North", "Annex", "Private", "Yard"] {
                XCTAssertFalse(shared.contains(secret))
            }
        }
    }

    func testLogsNameStandardOutputAndErrorStreams() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let folder = home.appendingPathComponent("Library/Logs/Teslatlas Hub", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try "normal event\n".write(to: folder.appendingPathComponent("hub.out.log"),
                                    atomically: true, encoding: .utf8)
        try "error event\n".write(to: folder.appendingPathComponent("hub.err.log"),
                                   atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.modificationDate: Date(timeIntervalSince1970: 0)],
            ofItemAtPath: folder.appendingPathComponent("hub.err.log").path)
        let controller = HubController(homeDirectory: home, serviceInstalledOverride: false)
        let finished = expectation(description: "logs loaded")

        controller.logs { text in
            XCTAssertTrue(text.contains("== hub.out.log ==\nnormal event"))
            XCTAssertTrue(text.contains("== hub.err.log ==\nerror event"))
            XCTAssertTrue(text.contains("1970-01-01T00:00:00Z"))
            XCTAssertTrue(text.contains("historical"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
    }

    func testRefreshUsesInstalledBinaryAndReportsVersionMismatch() {
        let embedded = RecordingCommandRunner(result: .failure(HubActionError.commandFailed("unused")))
        let installed = RecordingCommandRunner(result: .success("""
        {"status":"ok","version":"9.9.9","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"vehicle":null,"legacyCredentials":{"present":false}}
        """))
        let controller = HubController(
                                       commandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "installed status loaded")

        controller.refresh { snapshot in
            XCTAssertEqual(snapshot.version, "9.9.9")
            XCTAssertEqual(snapshot.health, .degraded)
            XCTAssertEqual(snapshot.service, "Installed · version mismatch")
            XCTAssertTrue(snapshot.diagnosticLines.first?.contains("Version mismatch") == true)
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(embedded.arguments, [])
        XCTAssertEqual(installed.arguments, [["--config", HubTestScratch.realHomePath + "/Library/Application Support/Teslatlas Hub/config.toml", "status"]])
    }

    private static let collectionAttentionLabels: [(code: String, label: String)] = [
        ("collector_api_refused", "Owner API has refused collection"),
        ("collector_provider_refused", "The provider has refused collection"),
        ("collector_fleet_access_refused", "Fleet access has been refused"),
        ("collector_billing_suspended", "Fleet billing is suspended"),
        ("collector_cap_reached", "The local monthly cap has been reached"),
        ("collector_config_error", "Fleet configuration needs attention"),
        ("collector_account_locked", "The account is locked"),
        ("collector_stream_audit_failed", "Stream receipts cannot be written"),
        ("collector_auth_waiting", "Collection is waiting for credentials"),
        ("collector_auth_configuration_error", "Sign-in configuration needs attention"),
        ("collector_auth_sign_in_required",
         "Sign in again; collection can continue until the token expires"),
        ("collector_cadence_invalid",
         "Collection cadence needs attention; correct the setting and restart"),
        ("collector_fleet_refresh_outcome_unknown",
         "Reconnect Fleet credentials; the last refresh outcome is unknown")
    ]

    private func attentionStatus(ready: Bool, reason: String?, attention: String?,
                                 includeAttention: Bool = true) -> String {
        let attentionMember = includeAttention
            ? #","collectionAttention":\#(attention.map { "\"\($0)\"" } ?? "null")"#
            : ""
        let reasonJSON = reason.map { "\"\($0)\"" } ?? "null"
        return """
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1048576},"ready":\(ready),"readinessReason":\(reasonJSON)\(attentionMember),"provider":"legacy","vehicles":[{"vehicleId":"7a5d69ab-8ea8-4056-8b2f-42c41c28ae36","displayName":"Example"}],"credentials":{"present":true},"legacyCredentials":{"present":true}}
        """
    }

    func testRefreshShowsCollectionAttentionWithoutChangingServiceReadiness() {
        func snapshot(for json: String) -> HubSnapshot {
            let installed = RecordingCommandRunner(result: .success(json))
            let controller = HubController(
                commandRunner: installed,
                serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
                serviceInstalledOverride: true)
            let finished = expectation(description: "status loaded")
            var loaded: HubSnapshot?
            controller.refresh { value in
                loaded = value
                finished.fulfill()
            }
            wait(for: [finished], timeout: 2)
            return loaded ?? HubSnapshot.previewRunning
        }
        func attentionLines(_ value: HubSnapshot) -> [String] {
            value.diagnosticLines.filter { $0.hasPrefix("Collection attention:") }
        }

        // Every registry code is shown in plain words, beside a running service.
        for entry in Self.collectionAttentionLabels {
            let running = snapshot(for: attentionStatus(ready: true, reason: nil,
                                                        attention: entry.code))
            XCTAssertEqual(running.health, .running, entry.code)
            XCTAssertEqual(running.service, "Installed and running", entry.code)
            XCTAssertEqual(attentionLines(running), ["Collection attention: \(entry.label)"],
                           entry.code)
            // A simultaneous service failure degrades; the attention stays shown.
            let failed = snapshot(for: attentionStatus(ready: false, reason: "collector_stale",
                                                       attention: entry.code))
            XCTAssertEqual(failed.health, .degraded, entry.code)
            XCTAssertEqual(attentionLines(failed), ["Collection attention: \(entry.label)"],
                           entry.code)
            XCTAssertTrue(failed.diagnosticLines.contains("Readiness: collector_stale"))
        }
        // A null, a missing member and an unknown word: no line, no line, and the
        // general line, with the health decided by readiness alone.
        let none = snapshot(for: attentionStatus(ready: true, reason: nil, attention: nil))
        XCTAssertEqual(none.health, .running)
        XCTAssertTrue(attentionLines(none).isEmpty)
        let missing = snapshot(for: attentionStatus(ready: true, reason: nil, attention: nil,
                                                    includeAttention: false))
        XCTAssertEqual(missing.health, .running)
        XCTAssertTrue(attentionLines(missing).isEmpty)
        let unknown = snapshot(for: attentionStatus(ready: true, reason: nil,
                                                    attention: "collector_from_a_newer_hub"))
        XCTAssertEqual(unknown.health, .running)
        XCTAssertEqual(attentionLines(unknown), ["Collection attention: Collection needs attention"])
        // The attention line sits right after the readiness line.
        let order = snapshot(for: attentionStatus(ready: true, reason: nil,
                                                  attention: "collector_cap_reached"))
        let readinessIndex = order.diagnosticLines.firstIndex { $0.hasPrefix("Readiness:") }
        XCTAssertEqual(order.diagnosticLines[(readinessIndex ?? 0) + 1],
                       "Collection attention: The local monthly cap has been reached")
    }

    func testStoppedOnboardingAcceptsCollectionAttention() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let runner = CommandMapRunner(responses: [
            "status": .success(attentionStatus(ready: true, reason: nil,
                                               attention: "collector_cap_reached")),
            // The actual doctor exits successfully with attention in its report.
            "doctor": .success(#"{"status":"ok","collector":{"collectionAttention":"collector_cap_reached","readiness":"ready","running":false,"lease":null,"cadenceError":null}}"#)
        ])
        let events = EventRecorder()
        let controller = HubController(commandRunner: runner,
            serviceRunner: ScriptedService(events: events, loadState: .unloaded),
            homeDirectory: home, serviceInstalledOverride: true)
        let done = expectation(description: "stopped verification complete")
        controller.runOnboardingChecks(expectRunning: false) { result in
            guard case let .success(checks) = result else {
                XCTFail("missing checks"); done.fulfill(); return
            }
            XCTAssertEqual(checks.first { $0.title == "Service" }?.passed, true)
            XCTAssertEqual(checks.first { $0.title == "Diagnostics" }?.passed, true)
            XCTAssertEqual(checks.first { $0.title == "Diagnostics" }?.detail, "Passed")
            done.fulfill()
        }
        wait(for: [done], timeout: 2)
        XCTAssertTrue(runner.commands.contains("doctor"))
        XCTAssertFalse(events.values.contains { $0.contains("service:start") })
    }

    func testRefreshPreservesStoppedStateAcrossVersionMismatch() {
        let installed = RecordingCommandRunner(result: .success("""
        {"status":"ok","version":"9.9.9","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
        """))
        let controller = HubController(commandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder(), loadState: .unloaded),
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "stopped status loaded")

        controller.refresh { snapshot in
            XCTAssertEqual(snapshot.health, .stopped)
            XCTAssertEqual(snapshot.service, "Installed but stopped")
            XCTAssertEqual(snapshot.account, "Connected")
            XCTAssertEqual(snapshot.provider, .legacy)
            XCTAssertTrue(snapshot.diagnosticLines.first?.contains("Version mismatch") == true)
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
    }

    func testStoppedDashboardSaysStoppedWithoutRequestingSetup() {
        let installed = RecordingCommandRunner(result: .success("""
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
        """))
        let controller = HubController(commandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder(), loadState: .unloaded),
                                       serviceInstalledOverride: true)
        let settled = expectation(description: "stopped dashboard settled")
        var dashboard: MainWindowController?
        dashboard = MainWindowController(controller: controller) { snapshot in
            XCTAssertEqual(snapshot.health, .stopped)
            let copy = self.labels(in: dashboard?.window?.contentView).map(\.stringValue)
            XCTAssertTrue(copy.contains("Hub is stopped"))
            XCTAssertTrue(copy.contains("Vehicle data is not being collected."))
            XCTAssertFalse(copy.contains { $0.localizedCaseInsensitiveContains("setup required") })
            XCTAssertFalse(copy.contains { $0.localizedCaseInsensitiveContains("needs attention") })
            let start = self.buttons(in: dashboard?.window?.contentView)
                .first { $0.title == "Start Hub" }
            XCTAssertEqual((start as? HubActionButton)?.hubStyle, .primary)
            XCTAssertTrue(start?.isEnabled ?? false)
            XCTAssertNotNil(start?.layer?.backgroundColor)
            settled.fulfill()
        }

        wait(for: [settled], timeout: 2)
        withExtendedLifetime(dashboard) {}
    }

    func testRefreshSummarizesMultipleInstalledVehicles() {
        let firstID = UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5")!
        let secondID = UUID(uuidString: "FB25AA4A-A719-4575-8BB1-02D4524F2571")!
        let installed = RecordingCommandRunner(result: .success("""
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicle":null,"vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"One"},{"vehicleId":"\(secondID.uuidString)","displayName":"Two"}],"credentials":{"present":true},"legacyCredentials":{"present":false},"fleetCredentials":{"present":true}}
        """))
        let controller = HubController(commandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "multi-vehicle status loaded")

        controller.refresh { snapshot in
            XCTAssertEqual(snapshot.vehicleName, "2 vehicles")
            XCTAssertEqual(snapshot.account, "Connected")
            XCTAssertEqual(snapshot.provider, .fleet)
            XCTAssertEqual(snapshot.accountDisplay, "Connected · Fleet API")
            XCTAssertNil(snapshot.controlVehicleID)
            XCTAssertEqual(snapshot.controlVehicles.map(\.id), [firstID, secondID])
            XCTAssertEqual(snapshot.controlVehicles.map(\.displayName), ["One", "Two"])
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
    }

    func testCLIStatusTelemetryRendersTypedFieldsForTheSelectedVehicle() throws {
        let firstID = UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5")!
        let secondID = UUID(uuidString: "FB25AA4A-A719-4575-8BB1-02D4524F2571")!
        let installed = RecordingCommandRunner(result: .success("""
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":false,"provider":"fleet","vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"Aurora","latestObservedAtMs":2000,"activityState":"online","batteryLevel":81,"locationName":"Home","telemetryStatus":"cached"},{"vehicleId":"\(secondID.uuidString)","displayName":"Comet","activityState":"asleep","batteryLevel":null,"locationName":null,"telemetryStatus":"unknown"}],"credentials":{"present":true}}
        """))
        let controller = HubController(commandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "typed vehicle status loaded")
        var loaded: HubSnapshot?
        controller.refresh { snapshot in
            loaded = snapshot
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        let snapshot = try XCTUnwrap(loaded)
        XCTAssertEqual(snapshot.controlVehicles[0].activityState, "online")
        XCTAssertEqual(snapshot.controlVehicles[0].batteryLevel, 81)
        XCTAssertEqual(snapshot.controlVehicles[0].locationName, "Home")
        XCTAssertEqual(snapshot.controlVehicles[0].telemetryStatus, .cached)
        XCTAssertEqual(snapshot.controlVehicles[1].activityState, "asleep")
        XCTAssertNil(snapshot.controlVehicles[1].batteryLevel)
        XCTAssertNil(snapshot.controlVehicles[1].locationName)
        XCTAssertEqual(snapshot.controlVehicles[1].telemetryStatus, .unknown)

        let view = HubVehiclesView(actions: .noOp)
        view.apply(snapshot: snapshot, selectedVehicleID: secondID, enabled: true)
        var values = labels(in: view).map(\.stringValue)
        XCTAssertTrue(values.contains("Comet"))
        XCTAssertTrue(values.contains("Asleep"))
        XCTAssertFalse(values.contains("Online"))
        XCTAssertGreaterThanOrEqual(values.filter { $0 == "Unavailable" }.count, 2)
        XCTAssertTrue(values.contains("Unknown"))

        view.apply(snapshot: snapshot, selectedVehicleID: firstID, enabled: true)
        values = labels(in: view).map(\.stringValue)
        XCTAssertTrue(values.contains("Aurora"))
        XCTAssertTrue(values.contains("Online"))
        XCTAssertTrue(values.contains("81%"))
        XCTAssertTrue(values.contains("Home"))
        XCTAssertTrue(values.contains("Cached"))
        XCTAssertFalse(values.contains("Available"))
    }

    func testMultipleVehicleControlUsesExplicitSelectedVehicle() {
        let firstID = UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5")!
        let secondID = UUID(uuidString: "FB25AA4A-A719-4575-8BB1-02D4524F2571")!
        let status = """
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"One"},{"vehicleId":"\(secondID.uuidString)","displayName":"Two"}],"credentials":{"present":true}}
        """
        let installed = RecordingCommandRunner(result: .success(status))
        let controller = HubController(commandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "selected multi-vehicle control sent")

        controller.refresh { snapshot in
            XCTAssertNil(snapshot.controlVehicleID)
            controller.performVehicleControl(.wake, vehicleID: secondID) { result in
                if case let .failure(error) = result { XCTFail(error.localizedDescription) }
                finished.fulfill()
            }
        }

        wait(for: [finished], timeout: 2)
        let config = HubTestScratch.realHomePath + "/Library/Application Support/Teslatlas Hub/config.toml"
        XCTAssertEqual(installed.arguments, [
            ["--config", config, "status"],
            ["--config", config, "control", "--vehicle-id",
             secondID.uuidString.lowercased(), "wake", "--confirm"]
        ])
    }

    func testVehicleCommandActivityKeepsDispatchedVehicleWhenSelectionChangesWhilePending() throws {
        // Catches recording the currently selected vehicle after a pending command completes.
        let firstID = UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5")!
        let secondID = UUID(uuidString: "FB25AA4A-A719-4575-8BB1-02D4524F2571")!
        let runner = PendingVehicleControlRunner(status: """
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"One"},{"vehicleId":"\(secondID.uuidString)","displayName":"Two"}],"credentials":{"present":true}}
        """)
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
                                       serviceInstalledOverride: true)
        let loaded = expectation(description: "vehicle dashboard loaded")
        let dashboard = MainWindowController(controller: controller) { _ in loaded.fulfill() }
        wait(for: [loaded], timeout: 1)

        dashboard.vehicleCommand(.wake, vehicleID: firstID)
        let confirmation = try XCTUnwrap(dashboard.window?.attachedSheet)
        try XCTUnwrap(buttons(in: confirmation.contentView)
            .first { $0.title == "Wake Vehicle" }).performClick(nil)

        let selector = try XCTUnwrap(popups(in: dashboard.window?.contentView).first)
        XCTAssertFalse(selector.isEnabled)
        selector.selectItem(at: 1)
        selector.sendAction(selector.action, to: selector.target)
        XCTAssertTrue(runner.hasPendingControl)

        DispatchQueue.global().asyncAfter(deadline: .now() + 0.05) { NSApp.abortModal() }
        runner.completeControl(.success(""))
        let rendered = expectation(description: "accepted activity rendered")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
            let text = self.labels(in: dashboard.window?.contentView).map(\.stringValue).joined(separator: " ")
            XCTAssertTrue(text.contains("The Wake Vehicle command was accepted for One."))
            XCTAssertFalse(text.contains("The Wake Vehicle command was accepted for Two."))
            rendered.fulfill()
        }
        wait(for: [rendered], timeout: 1)
    }

    func testMultipleVehicleDashboardShowsNativeSelector() throws {
        let firstID = UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5")!
        let secondID = UUID(uuidString: "FB25AA4A-A719-4575-8BB1-02D4524F2571")!
        let status = """
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"One"},{"vehicleId":"\(secondID.uuidString)","displayName":"Two"}],"credentials":{"present":true}}
        """
        let installed = RecordingCommandRunner(result: .success(status))
        let controller = HubController(commandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
                                       serviceInstalledOverride: true)
        let settled = expectation(description: "multi-vehicle selector rendered")
        var dashboard: MainWindowController?
        dashboard = MainWindowController(controller: controller) { _ in
            let selector = self.popups(in: dashboard?.window?.contentView)
                .first { $0.itemTitles == ["One", "Two"] }
            XCTAssertNotNil(selector)
            XCTAssertFalse(selector?.isHidden ?? true)
            XCTAssertTrue(selector?.isBordered ?? false)
            settled.fulfill()
        }

        wait(for: [settled], timeout: 2)
        withExtendedLifetime(dashboard) {}
    }

    func testAccountDisplayKeepsConnectionStateSeparateFromProviderLabel() {
        var snapshot = HubSnapshot.previewRunning
        snapshot.provider = .legacy
        XCTAssertEqual(snapshot.account, "Connected")
        XCTAssertEqual(snapshot.accountDisplay, "Connected · Legacy token")

        snapshot.account = "Not configured"
        snapshot.accountState = .notConfigured
        XCTAssertEqual(snapshot.accountDisplay, "Not configured")
        snapshot.provider = nil
        XCTAssertEqual(snapshot.accountDisplay, "Not configured")
    }

    func testSignOutUsesInstalledBinaryWithoutSecretsAndRefreshesStatus() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let embedded = CountingRunner()
        let installed = CommandMapRunner(responses: [
            "sign-out": .success("{\"status\":\"signed_out\"}"),
            "status": .success("""
            {"status":"ok","version":"\(HubRelease.bundledVersion)","ready":false,"provider":"fleet","credentials":{"present":false}}
            """)
        ])
        let controller = HubController(
                                       commandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)
        let finished = expectation(description: "signed out and refreshed")

        controller.signOutTeslaAccount { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            XCTAssertEqual(controller.snapshot.account, "Not configured")
            XCTAssertEqual(controller.snapshot.provider, .fleet)
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        let config = home.appendingPathComponent(
            "Library/Application Support/Teslatlas Hub/config.toml"
        ).path
        XCTAssertEqual(installed.arguments, [
            ["--config", config, "control", "sign-out"],
            ["--config", config, "status"]
        ])
        XCTAssertEqual(embedded.calls, 0)
    }

    func testSignOutIsBlockedByPendingMigrationHandover() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let folder = home.appendingPathComponent(
            "Library/Application Support/Teslatlas Hub",
            isDirectory: true
        )
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try Data("pending".utf8).write(
            to: folder.appendingPathComponent(".teslamate-handover-pending")
        )
        let runner = CountingRunner()
        let controller = HubController(commandRunner: runner,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "sign out blocked")

        controller.signOutTeslaAccount { result in
            guard case let .failure(error) = result else {
                return XCTFail("expected sign-out failure")
            }
            XCTAssertTrue(error.localizedDescription.contains("migration handover"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 1)
        XCTAssertEqual(runner.calls, 0)
    }

    func testSignOutFailureRefreshesRemainingProviderBeforeReturningError() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let installed = CommandMapRunner(responses: [
            "sign-out": .failure(HubActionError.commandFailed("checkpoint failed")),
            "status": .success("""
            {"status":"ok","version":"\(HubRelease.bundledVersion)","ready":false,"provider":"fleet","credentials":{"present":false},"legacyCredentials":{"present":true},"fleetCredentials":{"present":false}}
            """)
        ])
        let controller = HubController(commandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)
        let finished = expectation(description: "failed sign-out refreshed")

        controller.signOutTeslaAccount { result in
            guard case let .failure(error) = result else {
                return XCTFail("expected sign-out failure")
            }
            XCTAssertTrue(error.localizedDescription.contains("status was refreshed"))
            XCTAssertEqual(controller.snapshot.account, "Connected")
            XCTAssertEqual(controller.snapshot.provider, .legacy)
            XCTAssertEqual(controller.snapshot.accountDisplay, "Connected · Legacy token")
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(installed.commands, ["sign-out", "status"])
    }

    func testSingleVehicleControlsUseInstalledBinaryExactlyOnce() {
        let vehicleID = UUID(uuidString: "7A5D69AB-8EA8-4056-8B2F-42C41C28AE36")!
        let status = """
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(vehicleID.uuidString)","displayName":"One"}],"credentials":{"present":true}}
        """
        let embedded = RecordingCommandRunner(result: .failure(HubActionError.commandFailed("unused")))
        let installed = RecordingCommandRunner(result: .success(status))
        let controller = HubController(
                                       commandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "vehicle controls sent")
        let actions = HubVehicleControl.allCases

        controller.refresh { snapshot in
            XCTAssertEqual(snapshot.controlVehicleID, vehicleID)
            func send(_ index: Int) {
                guard index < actions.count else {
                    finished.fulfill()
                    return
                }
                controller.performVehicleControl(actions[index]) { result in
                    if case let .failure(error) = result { XCTFail(error.localizedDescription) }
                    send(index + 1)
                }
            }
            send(0)
        }

        wait(for: [finished], timeout: 2)
        let config = HubTestScratch.realHomePath + "/Library/Application Support/Teslatlas Hub/config.toml"
        let expectedControls = actions.map {
            ["--config", config, "control", "--vehicle-id", vehicleID.uuidString.lowercased(),
             $0.rawValue, "--confirm"]
        }
        XCTAssertEqual(installed.arguments, [["--config", config, "status"]] + expectedControls)
        XCTAssertEqual(embedded.arguments, [])
    }

    func testPreviewVehicleCardShowsControlsAndManageTeslaInsteadOfConnectTesla() {
        let alert = MainWindowController.vehicleControlConfirmation(.climateStart,
                                                                    vehicleName: "Model 3")
        XCTAssertEqual(alert.messageText, "Start Climate for Model 3?")
        XCTAssertEqual(alert.buttons.map(\.title), ["Cancel", "Start Climate"])

        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let windowController = MainWindowController(controller: controller)
        let settled = expectation(description: "preview dashboard settled")
        DispatchQueue.main.async {
            XCTAssertTrue(self.labels(in: windowController.window?.contentView)
                .contains { $0.stringValue == "Aurora" })
            for title in ["Climate", "Access", "More"] {
                let button = self.buttons(in: windowController.window?.contentView)
                    .first { $0.title == title }
                XCTAssertNotNil(button, "missing \(title)")
                XCTAssertFalse(button?.isHidden ?? true, "hidden \(title)")
                XCTAssertTrue(button?.isEnabled ?? false, "preview disabled \(title)")
            }
            XCTAssertTrue(self.labels(in: windowController.window?.contentView)
                .contains { $0.stringValue == "Connected · Fleet API" })
            XCTAssertTrue(self.buttons(in: windowController.window?.contentView)
                .compactMap { $0 as? HubActionButton }
                .allSatisfy { !$0.isBordered })
            XCTAssertNil(windowController.window?.defaultButtonCell)
            XCTAssertTrue(self.buttons(in: windowController.window?.contentView)
                .contains { $0.title == "Stop Hub…" && $0.isHidden })
            XCTAssertTrue(self.buttons(in: windowController.window?.contentView)
                .contains { $0.title == "Restart Hub" && $0.isHidden })
            XCTAssertFalse(self.buttons(in: windowController.window?.contentView)
                .contains { $0.keyEquivalent == "\r" })
            XCTAssertFalse(self.buttons(in: windowController.window?.contentView)
                .contains { $0.title.localizedCaseInsensitiveContains("charge") })
            XCTAssertEqual(HubVehicleControl.allCases.map(\.rawValue), [
                "wake", "climate-start", "climate-stop", "lock", "unlock", "flash-lights", "honk-horn"
            ])
            settled.fulfill()
        }
        wait(for: [settled], timeout: 1)
    }

    func testStartShowsProgressImmediatelyAndSettlesToRunning() {
        let home = HubTestScratch.directory
            .appendingPathComponent("teslatlas-hub-start-test-\(UUID().uuidString)", isDirectory: true)
        try? FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let status = """
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
        """
        let installed = RecordingCommandRunner(result: .success(status))
        let service = PendingServiceRunner(loadState: .unloaded)
        let controller = HubController(commandRunner: installed,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let loaded = expectation(description: "stopped dashboard loaded")
        var dashboard: MainWindowController?
        dashboard = MainWindowController(controller: controller) { _ in loaded.fulfill() }
        wait(for: [loaded], timeout: 1)

        let start = buttons(in: dashboard?.window?.contentView)
            .first { $0.title == "Start Hub" }
        XCTAssertNotNil(start)
        start?.performClick(nil)
        XCTAssertTrue(labels(in: dashboard?.window?.contentView)
            .contains { $0.stringValue == "Starting Hub…" })
        XCTAssertTrue(labels(in: dashboard?.window?.contentView)
            .contains { $0.stringValue == "Preparing vehicle data collection." })
        XCTAssertFalse(dashboard?.connectButton.isEnabled ?? true)
        XCTAssertFalse(dashboard?.importButton.isEnabled ?? true)

        service.loadState = .loaded
        service.complete(.success(""))
        let running = expectation(description: "running dashboard settled")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
            XCTAssertTrue(self.labels(in: dashboard?.window?.contentView)
                .contains { $0.stringValue == "Hub is running" })
            XCTAssertTrue(self.labels(in: dashboard?.window?.contentView)
                .contains { $0.stringValue == "Hub runs in the background. You can close this window." })
            running.fulfill()
        }
        wait(for: [running], timeout: 1)
        withExtendedLifetime(dashboard) {}
    }

    func testCompletedMigrationShowsStartingInsteadOfAttentionDuringCollectorWarmup() {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let dashboard = MainWindowController(controller: controller)

        dashboard.settleStartedHubFromOnboarding()

        XCTAssertTrue(labels(in: dashboard.window?.contentView)
            .contains { $0.stringValue == "Starting Hub…" })
        XCTAssertFalse(labels(in: dashboard.window?.contentView)
            .contains { $0.stringValue == "Attention needed" })
    }

    func testHubStartedOnboardingCompletionSchedulesOnlySettlementRefresh() throws {
        let runner = PendingCommandRunner()
        let controller = HubController(
            commandRunner: runner,
            serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
            serviceInstalledOverride: true,
            initialSnapshot: .firstRun
        )
        let initialRefresh = expectation(description: "initial dashboard refresh")
        let dashboard = MainWindowController(
            controller: controller,
            serviceTransitionTimeout: 1,
            serviceTransitionPollInterval: 0.01,
            errorPresenter: { error in XCTFail("unexpected error: \(error)") },
            onInitialRefresh: { _ in initialRefresh.fulfill() }
        )
        XCTAssertEqual(runner.arguments.count, 1)
        runner.complete(.success("""
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
        """))
        wait(for: [initialRefresh], timeout: 1)

        _ = try XCTUnwrap(dashboard.showOnboarding(route: .provider))
        let identifier = try XCTUnwrap(dashboard.activeOnboardingIdentifier)
        dashboard.completeOnboarding(identifier: identifier, completion: .hubStarted)

        let settlementRefresh = expectation(description: "settlement refresh scheduled")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
            XCTAssertEqual(runner.arguments.count, 2,
                           "Hub-start completion schedules only the controlled settlement refresh")
            XCTAssertEqual(runner.arguments.last?.suffix(1), ["status"])
            settlementRefresh.fulfill()
        }
        wait(for: [settlementRefresh], timeout: 1)
        withExtendedLifetime(dashboard) {}
    }

    func testSuccessfulOnboardingAlwaysLandsOnOverview() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])

        let firstRunDashboard = MainWindowController(controller: controller)
        firstRunDashboard.selectMainSection(.settings)
        _ = try XCTUnwrap(firstRunDashboard.showFirstRunOnboarding())
        firstRunDashboard.completeOnboarding(
            identifier: try XCTUnwrap(firstRunDashboard.activeOnboardingIdentifier),
            completion: .configured
        )
        XCTAssertEqual(firstRunDashboard.selectedSection, .dashboard)

        let accountDashboard = MainWindowController(controller: controller)
        accountDashboard.selectMainSection(.settings)
        _ = try XCTUnwrap(accountDashboard.showOnboarding(route: .provider))
        accountDashboard.completeOnboarding(
            identifier: try XCTUnwrap(accountDashboard.activeOnboardingIdentifier),
            completion: .configured
        )
        XCTAssertEqual(accountDashboard.selectedSection, .dashboard)
    }

    func testInFlightRefreshDoesNotRepaintDashboardDuringServiceTransition() {
        let runner = PendingCommandRunner()
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder(),
                                                                      loadState: .loaded),
                                       serviceInstalledOverride: true)
        let refreshed = expectation(description: "in-flight refresh returned")
        let dashboard = MainWindowController(controller: controller) { _ in
            refreshed.fulfill()
        }

        dashboard.settleStartedHubFromOnboarding()
        runner.complete(.success("""
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1048576},"ready":false,"provider":"legacy","credentials":{"present":true}}
        """))

        wait(for: [refreshed], timeout: 1)
        let visibleText = labels(in: dashboard.window?.contentView).map(\.stringValue)
        XCTAssertTrue(visibleText.contains("Starting Hub…"))
        XCTAssertFalse(visibleText.contains("Attention needed"))
        XCTAssertFalse(visibleText.contains("Connected · Legacy token"))
        XCTAssertFalse(visibleText.contains("Healthy · 1 MB"))
    }

    func testServiceTransitionHasHardDeadlineWhenStatusProbeNeverReturns() {
        let runner = PendingCommandRunner()
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder(),
                                                                      loadState: .loaded),
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)
        let timedOut = expectation(description: "transition deadline fired")
        let initialStatus = expectation(description: "initial status settled before transition probes")
        var started = Date()
        let dashboard = MainWindowController(
            controller: controller,
            serviceTransitionTimeout: 0.1,
            serviceTransitionPollInterval: 0.01,
            errorPresenter: { error in
                XCTAssertTrue(error.localizedDescription.contains("did not finish"))
                XCTAssertLessThan(Date().timeIntervalSince(started), 0.4)
                timedOut.fulfill()
            },
            onInitialRefresh: { _ in initialStatus.fulfill() }
        )

        runner.complete(.success("""
        {"status":"ok","version":"\(HubRelease.bundledVersion)","ready":true,"provider":"fleet","credentials":{"present":true}}
        """))
        wait(for: [initialStatus], timeout: 1)
        started = Date()
        dashboard.settleStartedHubFromOnboarding()
        XCTAssertTrue(labels(in: dashboard.window?.contentView)
            .contains { $0.stringValue == "Starting Hub…" })

        wait(for: [timedOut], timeout: 0.5)
        let visibleText = labels(in: dashboard.window?.contentView).map(\.stringValue)
        XCTAssertFalse(visibleText.contains("Starting Hub…"))
        XCTAssertTrue(visibleText.contains("Hub is running"))
        XCTAssertTrue(dashboard.connectButton.isEnabled)
        XCTAssertFalse(dashboard.importButton.isEnabled)
        XCTAssertTrue(dashboard.importButton.isHidden)
    }

    func testPendingServiceCommandKeepsTransitionLockedUntilControllerCleanupCompletes() throws {
        let status = """
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
        """
        let runner = RecordingCommandRunner(result: .success(status))
        let service = PendingServiceRunner(loadState: .unloaded)
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: service,
                                       serviceInstalledOverride: true)
        let loaded = expectation(description: "stopped dashboard loaded")
        let failed = expectation(description: "controller failure presented after cleanup")
        var errors: [Error] = []
        let dashboard = MainWindowController(
            controller: controller,
            serviceTransitionTimeout: 0.1,
            serviceTransitionPollInterval: 0.01,
            errorPresenter: { error in
                errors.append(error)
                failed.fulfill()
            },
            onInitialRefresh: { _ in loaded.fulfill() }
        )
        wait(for: [loaded], timeout: 1)

        let start = try XCTUnwrap(buttons(in: dashboard.window?.contentView)
            .first { $0.title == "Start Hub" })
        start.performClick(nil)
        let stillLocked = expectation(description: "transition remains locked past settlement timeout")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
            XCTAssertTrue(self.labels(in: dashboard.window?.contentView)
                .contains { $0.stringValue == "Starting Hub…" })
            XCTAssertFalse(dashboard.connectButton.isEnabled)
            XCTAssertFalse(dashboard.importButton.isEnabled)
            XCTAssertTrue(errors.isEmpty)
            XCTAssertEqual(service.arguments, [["service", "start"]])
            start.performClick(nil)
            XCTAssertEqual(service.arguments, [["service", "start"]],
                           "a second command must not overlap controller cleanup")
            stillLocked.fulfill()
        }
        wait(for: [stillLocked], timeout: 0.5)

        service.complete(.failure(HubActionError.commandFailed("persistent startup failure")))
        wait(for: [failed], timeout: 0.5)
        XCTAssertEqual(errors.count, 1)
        XCTAssertTrue(errors[0].localizedDescription.contains("persistent startup failure"))
        XCTAssertFalse(labels(in: dashboard.window?.contentView)
            .contains { $0.stringValue == "Starting Hub…" })
        XCTAssertEqual(service.arguments, [["service", "start"]])
    }

    func testNewestRefreshOwnsSnapshotWhenStatusResponsesCompleteOutOfOrder() {
        let runner = OutOfOrderCommandRunner()
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder(),
                                                                      loadState: .loaded),
                                       serviceInstalledOverride: true)
        let older = expectation(description: "older refresh returned latest snapshot")
        let newer = expectation(description: "newer refresh completed")

        controller.refresh { snapshot in
            XCTAssertEqual(snapshot.health, .running)
            older.fulfill()
        }
        controller.refresh { snapshot in
            XCTAssertEqual(snapshot.health, .running)
            newer.fulfill()
        }
        runner.complete(at: 1, with: .success("""
        {"status":"ok","version":"\(HubRelease.bundledVersion)","ready":true,"provider":"legacy","credentials":{"present":true}}
        """))
        runner.complete(at: 0, with: .success("""
        {"status":"ok","version":"\(HubRelease.bundledVersion)","ready":false,"provider":"legacy","credentials":{"present":true}}
        """))

        wait(for: [newer, older], timeout: 1)
        XCTAssertEqual(controller.snapshot.health, .running)
    }

    func testDegradedDashboardLeadsWithDiagnosticsAndKeepsRecoverySecondary() {
        var snapshot = HubSnapshot.previewRunning
        snapshot.health = .degraded
        snapshot.service = "Installed · needs attention"
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                       initialSnapshot: snapshot)
        let dashboard = MainWindowController(controller: controller)
        let buttons = self.buttons(in: dashboard.window?.contentView)
        let diagnostics = buttons.first { $0.title == "Run Diagnostics" && !$0.isHidden }
        let restart = buttons.first { $0.title == "Restart Hub" && !$0.isHidden }
        let stop = buttons.first { $0.title == "Stop Hub…" && !$0.isHidden }

        XCTAssertNotNil(diagnostics)
        XCTAssertNotNil(restart)
        XCTAssertNotNil(stop)
        XCTAssertTrue(dashboard.window?.defaultButtonCell === diagnostics?.cell)
        XCTAssertEqual(restart?.keyEquivalent, "")
        XCTAssertEqual(stop?.keyEquivalent, "")
    }

    func testLegacyDashboardHidesVehicleControls() {
        var snapshot = HubSnapshot.previewRunning
        snapshot.provider = .legacy
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                       initialSnapshot: snapshot)
        let windowController = MainWindowController(controller: controller)
        let settled = expectation(description: "legacy dashboard settled")

        DispatchQueue.main.async {
            for title in ["Climate", "Access", "More"] {
                let button = self.buttons(in: windowController.window?.contentView)
                    .first { $0.title == title }
                XCTAssertNotNil(button, "missing \(title)")
                XCTAssertFalse(button?.isEnabled ?? true, "enabled \(title)")
            }
            XCTAssertTrue(self.labels(in: windowController.window?.contentView)
                .contains { $0.stringValue == "Vehicle commands require Tesla Fleet API." })
            XCTAssertTrue(self.labels(in: windowController.window?.contentView)
                .contains { $0.stringValue == "Connected · Legacy token" })
            settled.fulfill()
        }

        wait(for: [settled], timeout: 1)
    }

    func testDashboardUsesApprovedCardsAndRealSnapshotValues() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                       initialSnapshot: .previewRunning)
        let windowController = MainWindowController(controller: controller)
        let view = try XCTUnwrap(windowController.window?.contentView)
        let identifiers = descendantViews(in: view).compactMap { $0.identifier?.rawValue }

        XCTAssertTrue(identifiers.contains("hub.dashboard.hero"))
        XCTAssertTrue(identifiers.contains("hub.dashboard.vehicle-card"))
        XCTAssertTrue(identifiers.contains("hub.dashboard.status-card"))
        XCTAssertTrue(labels(in: view).contains {
            $0.stringValue == "Teslatlas Hub \(HubSnapshot.previewRunning.version)"
        })
        XCTAssertFalse(labels(in: view).contains { $0.stringValue.contains("1.4.0") })
    }

    func testDashboardDoesNotInventUnavailableVehicleFacts() {
        var unavailable = HubSnapshot.previewRunning
        unavailable.vehicleName = "Vehicle"
        unavailable.vehicle = "Unknown"
        unavailable.controlVehicleID = nil
        unavailable.controlVehicles = []
        let dashboard = HubDashboardView(actions: .noOp)
        dashboard.apply(snapshot: unavailable, transition: nil, activity: [])
        let text = labels(in: dashboard).map(\.stringValue).joined(separator: " ")

        XCTAssertFalse(text.contains("78%"))
        XCTAssertFalse(text.contains("Model 3"))
        XCTAssertFalse(text.contains("Home"))
    }

    func testNavigationSeparatesContentSelectionFromModalActions() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                       initialSnapshot: .previewRunning)
        let windowController = MainWindowController(controller: controller)

        windowController.selectMainSection(.vehicles)
        XCTAssertEqual(windowController.selectedSection, .vehicles)
        XCTAssertFalse(windowController.vehiclesView.isHidden)
        XCTAssertTrue(windowController.dashboardView.isHidden)

        _ = windowController.showDiagnostics()
        XCTAssertEqual(windowController.selectedSection, .vehicles)
        XCTAssertEqual(windowController.activeModalKind, .diagnostics)
    }

    func testDiagnosticsReplacementTearsDownDismissibleOnboardingBeforeChangingModalKind() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let dashboard = MainWindowController(controller: controller)
        let onboarding = try XCTUnwrap(dashboard.showOnboarding(route: .provider))
        XCTAssertNotNil(dashboard.activeOnboardingIdentifier)
        XCTAssertTrue(dashboard.accountWorkflowActive)

        let diagnostics = try XCTUnwrap(dashboard.showDiagnostics())

        XCTAssertFalse(onboarding === diagnostics)
        XCTAssertEqual(dashboard.activeModalKind, .diagnostics)
        XCTAssertNil(dashboard.activeOnboardingIdentifier)
        XCTAssertFalse(dashboard.accountWorkflowActive)
    }

    func testDiagnosticsRefusesToReplaceBusyOrNonDismissibleOnboarding() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let dashboard = MainWindowController(controller: controller)
        let busyOnboarding = try XCTUnwrap(dashboard.showOnboarding(route: .provider))
        busyOnboarding.setBusy(true, message: "Checking credentials…")

        XCTAssertNil(dashboard.showDiagnostics())
        XCTAssertEqual(dashboard.activeModalKind, .onboarding)
        XCTAssertTrue(dashboard.accountWorkflowActive)

        let firstRunController = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                               initialSnapshot: .firstRun)
        let firstRunDashboard = MainWindowController(controller: firstRunController)
        _ = try XCTUnwrap(firstRunDashboard.showFirstRunOnboarding())

        XCTAssertNil(firstRunDashboard.showDiagnostics())
        XCTAssertEqual(firstRunDashboard.activeModalKind, .onboarding)
        XCTAssertTrue(firstRunDashboard.accountWorkflowActive)
    }

    func testVehiclesPageRendersEveryRealVehicleWithoutMockMetadata() {
        let first = HubControlVehicle(id: UUID(), displayName: "Aurora", status: "Last seen just now")
        let second = HubControlVehicle(id: UUID(), displayName: "Comet", status: "No observations yet")
        var snapshot = HubSnapshot.previewRunning
        snapshot.controlVehicles = [first, second]
        snapshot.controlVehicleID = first.id
        let view = HubVehiclesView(actions: .noOp)
        view.apply(snapshot: snapshot, enabled: true)

        let text = labels(in: view).map(\.stringValue).joined(separator: " ")
        let selector = popups(in: view).first { $0.itemTitles == ["Aurora", "Comet"] }
        XCTAssertTrue(text.contains("Aurora"))
        XCTAssertTrue(text.contains("Your configured vehicles."))
        XCTAssertFalse(text.contains("Your connected vehicles."))
        XCTAssertNotNil(selector)
        XCTAssertFalse(text.contains("Model Y"))

        snapshot.controlVehicles = []
        snapshot.controlVehicleID = nil
        view.apply(snapshot: snapshot, enabled: true)
        let emptyText = labels(in: view).map(\.stringValue).joined(separator: " ")
        XCTAssertTrue(emptyText.contains("No configured vehicles"))
        XCTAssertFalse(emptyText.contains("No connected vehicles"))
    }

    func testOverviewVehicleSelectionCarriesIdentityAndCommandTargetToVehicles() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                       initialSnapshot: .previewRunning)
        let dashboard = MainWindowController(controller: controller)
        let comet = try XCTUnwrap(controller.snapshot.controlVehicles.first {
            $0.displayName == "Comet"
        })
        let openComet = try XCTUnwrap(buttons(in: dashboard.dashboardView).first {
            $0.accessibilityLabel() == "Comet"
        })

        openComet.performClick(nil)

        XCTAssertEqual(dashboard.selectedSection, .vehicles)
        let selector = try XCTUnwrap(popups(in: dashboard.vehiclesView).first {
            $0.itemTitles == ["Aurora", "Comet"]
        })
        XCTAssertEqual(selector.titleOfSelectedItem, "Comet")
        XCTAssertTrue(labels(in: dashboard.vehiclesView).contains { $0.stringValue == "Comet" })

        let card = try XCTUnwrap(descendantViews(in: dashboard.vehiclesView)
            .compactMap { $0 as? HubVehicleCardView }.first)
        let wake = NSMenuItem()
        wake.representedObject = HubVehicleControl.wake.rawValue
        _ = card.perform(NSSelectorFromString("runCommand:"), with: wake)
        let confirmation = try XCTUnwrap(dashboard.window?.attachedSheet)
        XCTAssertTrue(labels(in: confirmation.contentView).contains {
            $0.stringValue == "Wake Vehicle for \(comet.displayName)?"
        })
        try XCTUnwrap(buttons(in: confirmation.contentView).first { $0.title == "Cancel" })
            .performClick(nil)

        var refreshed = HubSnapshot.previewRunning
        refreshed.controlVehicleID = refreshed.controlVehicles.first?.id
        refreshed.controlVehicles = Array(refreshed.controlVehicles.prefix(1))
        dashboard.vehiclesView.apply(snapshot: refreshed,
                                     selectedVehicleID: comet.id,
                                     enabled: true)
        XCTAssertEqual(popups(in: dashboard.vehiclesView).first?.titleOfSelectedItem, "Aurora")
        XCTAssertTrue(labels(in: dashboard.vehiclesView).contains { $0.stringValue == "Aurora" })
    }

    func testVehicleConfirmationKeepsItsOriginalTargetAndStaleTargetIsRejected() throws {
        let firstID = UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5")!
        let secondID = UUID(uuidString: "FB25AA4A-A719-4575-8BB1-02D4524F2571")!
        let status = """
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"Aurora"},{"vehicleId":"\(secondID.uuidString)","displayName":"Comet"}],"credentials":{"present":true}}
        """
        let runner = PendingVehicleControlRunner(status: status)
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
                                       serviceInstalledOverride: true)
        let loaded = expectation(description: "vehicle dashboard loaded")
        let windowController = MainWindowController(controller: controller) { _ in loaded.fulfill() }
        wait(for: [loaded], timeout: 1)

        windowController.vehicleCommand(.wake, vehicleID: firstID)
        let confirmation = try XCTUnwrap(windowController.window?.attachedSheet)
        let selector = try XCTUnwrap(popups(in: windowController.window?.contentView)
            .first { $0.itemTitles == ["Aurora", "Comet"] })
        selector.selectItem(at: 1)
        selector.sendAction(selector.action, to: selector.target)
        try XCTUnwrap(buttons(in: confirmation.contentView)
            .first { $0.title == "Wake Vehicle" }).performClick(nil)

        XCTAssertTrue(runner.hasPendingControl)
        XCTAssertTrue(runner.arguments.last?.contains(firstID.uuidString.lowercased()) == true)
        XCTAssertFalse(runner.arguments.last?.contains(secondID.uuidString.lowercased()) == true)

        var staleSnapshot = HubSnapshot.previewRunning
        staleSnapshot.controlVehicleID = secondID
        staleSnapshot.controlVehicles = [HubControlVehicle(id: secondID, displayName: "Comet", status: "Online")]
        let staleRunner = CountingRunner()
        let staleController = HubController(commandRunner: staleRunner,
                                            serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
                                            serviceInstalledOverride: true,
                                            initialSnapshot: staleSnapshot)
        let rejected = expectation(description: "stale confirmed target rejected")
        staleController.performVehicleControl(.wake, vehicleID: firstID) { result in
            guard case let .failure(error) = result else {
                XCTFail("stale target was dispatched")
                rejected.fulfill()
                return
            }
            XCTAssertTrue(error.localizedDescription.contains("no longer configured"))
            rejected.fulfill()
        }
        wait(for: [rejected], timeout: 1)
        XCTAssertEqual(staleRunner.calls, 0)
    }

    func testFleetProviderWithoutConnectionShowsAccountNeedsConfiguration() throws {
        var snapshot = HubSnapshot.previewRunning
        snapshot.account = "Not configured"
        snapshot.accountState = .notConfigured
        snapshot.provider = .fleet
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                       initialSnapshot: snapshot)
        let windowController = MainWindowController(controller: controller)
        windowController.selectMainSection(.settings)
        XCTAssertTrue(labels(in: windowController.settingsPage)
            .contains { $0.stringValue == "Not configured" })
    }

    func testVehiclePagesShareAcceptedSnapshotEligibility() {
        let commandTitles = ["Start Climate", "Stop Climate", "Wake", "Lock", "Unlock", "Flash Lights", "Honk"]
        var valid = HubSnapshot.previewRunning
        let vehicleID = UUID()
        valid.controlVehicleID = vehicleID
        valid.controlVehicles = [HubControlVehicle(id: vehicleID, displayName: "Aurora", status: "Online")]

        let cases: [(HubSnapshot, Bool, Bool, Bool, Bool, Bool)] = [
            (valid, false, false, false, false, true),
            ({ var snapshot = valid; snapshot.health = .stopped; return snapshot }(), false, false, false, false, false),
            ({ var snapshot = valid; snapshot.account = "Not configured"; snapshot.accountState = .notConfigured; return snapshot }(), false, false, false, false, false),
            ({ var snapshot = valid; snapshot.provider = .legacy; return snapshot }(), false, false, false, false, false),
            (valid, true, false, false, false, false),
            (valid, false, true, false, false, false),
            (valid, false, false, true, false, false),
            (valid, false, false, false, true, false)
        ]

        for (snapshot, transition, accountWorkflow, serviceMutation, vehiclePending, expected) in cases {
            let enabled = MainWindowController.acceptedVehicleControlsEnabled(
                for: snapshot,
                serviceTransitionActive: transition,
                accountWorkflowActive: accountWorkflow,
                serviceDetailsMutationPending: serviceMutation,
                vehicleControlPending: vehiclePending,
                vehicleControlOutcomeUnknown: false
            )
            XCTAssertEqual(enabled, expected)

            let dashboard = HubDashboardView(actions: .noOp)
            dashboard.setInteractionsEnabled(enabled)
            dashboard.setVehicleControlsEnabled(enabled)
            dashboard.apply(snapshot: snapshot, transition: nil, activity: [])
            let vehicles = HubVehiclesView(actions: .noOp)
            vehicles.apply(snapshot: snapshot, enabled: enabled)
            for view in [dashboard, vehicles] {
                let commands = buttons(in: view).filter { commandTitles.contains($0.title) }
                XCTAssertTrue(
                    commands.allSatisfy { $0.isEnabled == expected },
                    "health=\(snapshot.health) account=\(snapshot.account) provider=\(String(describing: snapshot.provider)) expected=\(expected) states=\(commands.map { "\($0.title):\($0.isEnabled)" })"
                )
            }
        }
    }

    func testLegacyProviderRejectsVehicleCommandsBeforeRunner() {
        let vehicleID = UUID(uuidString: "7A5D69AB-8EA8-4056-8B2F-42C41C28AE36")!
        var snapshot = HubSnapshot.previewRunning
        snapshot.provider = .legacy
        snapshot.controlVehicleID = vehicleID
        snapshot.controlVehicles = [
            HubControlVehicle(id: vehicleID, displayName: "Vehicle", status: "online")
        ]
        let installed = CountingRunner()
        let controller = HubController(environment: [:],
                                       commandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder(),
                                                                      loadState: .loaded),
                                       serviceInstalledOverride: true,
                                       initialSnapshot: snapshot)
        let rejected = expectation(description: "legacy command rejected")

        controller.performVehicleControl(.wake) { result in
            guard case let .failure(error) = result else {
                XCTFail("legacy command unexpectedly accepted")
                rejected.fulfill()
                return
            }
            XCTAssertTrue(error.localizedDescription.contains("Fleet API"))
            rejected.fulfill()
        }

        wait(for: [rejected], timeout: 1)
        XCTAssertEqual(installed.calls, 0)
    }

    func testDisconnectConfirmationDefaultsToCancel() {
        let alert = MainWindowController.disconnectConfirmation()
        XCTAssertEqual(alert.buttons.map(\.title), ["Cancel", "Disconnect"])
        XCTAssertEqual(alert.buttons.first?.keyEquivalent, "\r")
        XCTAssertNotEqual(alert.buttons.last?.keyEquivalent, "\r")
    }

    func testStopHubConfirmationExplainsCollectionAndDefaultsToCancel() {
        let alert = MainWindowController.stopHubConfirmation()
        XCTAssertEqual(alert.messageText, "Stop collecting vehicle data?")
        XCTAssertTrue(alert.informativeText.contains("history stays safe"))
        XCTAssertEqual(alert.buttons.map(\.title), ["Cancel", "Stop Hub"])
        XCTAssertEqual(alert.buttons.first?.keyEquivalent, "\r")
        XCTAssertEqual(alert.buttons.last?.keyEquivalent, "")
    }

    func testAmbiguousClimateFailureWarnsAgainstRetry() {
        XCTAssertTrue(MainWindowController.vehicleControlOutcomeIsUnknown(
            HubActionError.commandTimedOut
        ))
        XCTAssertTrue(MainWindowController.vehicleControlOutcomeIsUnknown(
            HubActionError.commandFailed("vehicle command outcome is ambiguous")
        ))
        XCTAssertFalse(MainWindowController.vehicleControlOutcomeIsUnknown(
            HubActionError.commandFailed("vehicle is offline")
        ))
        let alert = MainWindowController.unknownVehicleControlOutcomeAlert()
        XCTAssertEqual(alert.messageText, "Command outcome unknown")
        XCTAssertTrue(alert.informativeText.contains("Do not repeat"))
    }

    func testImportSheetExplainsManualTeslaMateHandover() {
        XCTAssertTrue(ImportSheetController.teslaMateHandoverDetail.contains("without stopping"))
        XCTAssertTrue(ImportSheetController.teslaMateHandoverDetail.contains("yourself"))
        for fragment in ["back up TeslaMate", "update it to version 4.2.0 or newer", "start it once",
                         "wait for its database migrations to finish"] {
            XCTAssertTrue(ImportSheetController.teslaMateVersionRequirement.contains(fragment))
        }
        XCTAssertTrue(ImportSheetController.teslaMateVersionRequirement.contains("cannot prove"))

        let sheet = ImportSheetController(controller: HubController(
            environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]
        ))
        let visibleGuidance = labels(in: sheet.window?.contentView)
            .map(\.stringValue)
            .joined(separator: " ")
        for fragment in ["back up TeslaMate", "update it to version 4.2.0 or newer", "start it once",
                         "wait for its database migrations to finish"] {
            XCTAssertTrue(visibleGuidance.contains(fragment))
        }
    }

    func testCompatibilityCheckRejectsMissingVersionAcknowledgementBeforeCommands() {
        let runner = CountingRunner()
        let controller = HubController(commandRunner: runner,
                                       serviceInstalledOverride: false)
        let rejected = expectation(description: "missing version acknowledgement rejected")

        controller.checkTeslaMateCompatibility(source: "postgresql://reader@localhost/teslamate",
                                                carID: "1",
                                                passwordFile: "/tmp/password",
                                                acknowledgeV42CompatibleSchema: false) { result in
            guard case let .failure(error) = result else {
                return XCTFail("missing acknowledgement was accepted")
            }
            XCTAssertTrue(error.localizedDescription.contains("4.2.0 or newer"))
            rejected.fulfill()
        }

        wait(for: [rejected], timeout: 1)
        XCTAssertEqual(runner.calls, 0)
    }

    func testDirectMigrationRejectsMissingVersionAcknowledgementBeforeCommands() {
        let runner = CountingRunner()
        let controller = HubController(commandRunner: runner,
                                       serviceInstalledOverride: false)
        let rejected = expectation(description: "missing version acknowledgement rejected")

        controller.importTeslaMate(source: "postgresql://reader@localhost/teslamate",
                                   carID: "1",
                                   passwordFile: "/tmp/password",
                                   encryptionKeyFile: "/tmp/key",
                                   acknowledgeV42CompatibleSchema: false) { result in
            guard case let .failure(error) = result else {
                return XCTFail("missing acknowledgement was accepted")
            }
            XCTAssertTrue(error.localizedDescription.contains("4.2.0 or newer"))
            rejected.fulfill()
        }

        wait(for: [rejected], timeout: 1)
        XCTAssertEqual(runner.calls, 0)
    }

    func testDirectMigrationRejectsIncompatibleSchemaBeforeLocalMutation() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let runner = RecordingCommandRunner(result: .success(
            #"{"status":"incompatible","reasonCode":"schema_mismatch","requiredVersion":"4.2.0","guidance":"Update TeslaMate first."}"#
        ))
        let events = EventRecorder()
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: events),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let rejected = expectation(description: "incompatible source rejected before mutation")

        controller.importTeslaMate(source: "postgresql://reader@localhost/teslamate",
                                   carID: "1",
                                   passwordFile: "/tmp/password",
                                   encryptionKeyFile: "/tmp/key",
                                   acknowledgeV42CompatibleSchema: true) { result in
            guard case .failure = result else {
                return XCTFail("incompatible schema was accepted")
            }
            rejected.fulfill()
        }

        wait(for: [rejected], timeout: 1)
        XCTAssertEqual(runner.arguments.count, 1)
        XCTAssertTrue(runner.arguments[0].contains("teslamate-check"))
        XCTAssertTrue(runner.arguments[0].contains("--acknowledge-v4-2-compatible-schema"))
        XCTAssertTrue(events.values.isEmpty)
        XCTAssertFalse(controller.hasPendingMigrationHandover)
        XCTAssertFalse(FileManager.default.fileExists(atPath: home
            .appendingPathComponent("Library/Application Support/Teslatlas Hub/config.toml").path))
    }

    func testInstalledMigrationUsesLiveSnapshotWithoutStoppingTeslaMate() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success(
            #"{"status":"imported","captureMode":"online-snapshot"}"#
        ))
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "migration finished")

        controller.importTeslaMate(source: "postgresql://reader@localhost/teslamate",
                                   carID: "1",
                                   passwordFile: "/tmp/password",
                                   encryptionKeyFile: "/tmp/key",
                                   acknowledgeV42CompatibleSchema: true) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["check", "service:stop", "migrate"])
        XCTAssertNil(runner.stdin)
        XCTAssertTrue(runner.arguments.joined().contains("--online-snapshot"))
        XCTAssertTrue(runner.arguments.joined().contains("--preserve-existing-credentials"))
        let versionAcknowledgedCommands = runner.arguments.filter {
            $0.contains("teslamate-check") || $0.contains("migrate")
        }
        XCTAssertEqual(versionAcknowledgedCommands.count, 2)
        XCTAssertTrue(versionAcknowledgedCommands.allSatisfy {
            $0.contains("--acknowledge-v4-2-compatible-schema")
        })
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertFalse(events.values.contains("service:start"))
    }

    func testInstalledMigrationKeepsFleetProviderAndTokensConfig() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success(
            #"{"status":"imported","captureMode":"online-snapshot"}"#
        ))
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "migration finished")

        controller.importTeslaMate(source: "postgresql://reader@localhost/teslamate",
                                   carID: "1",
                                   passwordFile: "/tmp/password",
                                   encryptionKeyFile: "/tmp/key",
                                   acknowledgeV42CompatibleSchema: true) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["check", "service:stop", "migrate"])
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertFalse(events.values.contains("service:start"))
        let config = try configContents(in: home)
        XCTAssertTrue(config.contains("provider = \"fleet\""))
        XCTAssertTrue(config.contains("interval_seconds = 0"))
        XCTAssertEqual(HubController.collectorProvider(in: config), "fleet")
    }

    func testFreshMigrationInstallsCurrentServicePackageAfterSuccessfulCopy() throws {  // no install any more: the migration only copies; nothing starts before handover
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success(
            #"{"status":"imported","captureMode":"online-snapshot"}"#
        ))
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: false)
        let finished = expectation(description: "fresh migration installed")

        controller.importTeslaMate(source: "postgresql://reader@localhost/teslamate",
                                   carID: "1",
                                   passwordFile: "/tmp/password",
                                   encryptionKeyFile: "/tmp/key",
                                   acknowledgeV42CompatibleSchema: true) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["check", "migrate"])
        XCTAssertNil(runner.stdin)
        XCTAssertTrue(runner.arguments.joined().contains("--online-snapshot"))
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertFalse(events.values.contains("service:start"))
        let config = try String(contentsOf: home
            .appendingPathComponent("Library/Application Support/Teslatlas Hub/config.toml"))
        XCTAssertTrue(config.contains("[geocoder]\nenabled = false"))
        XCTAssertTrue(config.contains("[terrain]\nenabled = false"))
    }

    func testStartedMigrationTimeoutKeepsHandoverGateAndHubStopped() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events,
                                    result: .failure(HubActionError.commandTimedOut))
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "migration failure returned")

        controller.importTeslaMate(source: "postgres://reader@localhost/teslamate",
                                   carID: "1",
                                   passwordFile: "/tmp/password",
                                   encryptionKeyFile: "/tmp/key",
                                   acknowledgeV42CompatibleSchema: true) { result in
            guard case let .failure(error) = result else { return XCTFail("expected migration timeout") }
            XCTAssertEqual(
                error.localizedDescription,
                "TeslaMate import failed; retry the import."
            )
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["check", "service:stop", "migrate"])
        XCTAssertNil(runner.stdin)
        XCTAssertFalse(events.values.contains("service:start"))
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        let config = try configContents(in: home)
        XCTAssertTrue(config.contains("provider = \"fleet\""))
        XCTAssertTrue(config.contains("interval_seconds = 0"))
    }

    func testAmbiguousMigrationFailureKeepsHandoverGateAndHubStopped() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(
            events: events,
            result: .failure(HubActionError.commandFailed(
                "TESLATLAS_MIGRATION_OUTCOME_AMBIGUOUS: checkpoint failed"
            ))
        )
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: events),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "ambiguous migration stopped")

        controller.importTeslaMate(source: "postgres://reader@localhost/teslamate",
                                   carID: "1",
                                   passwordFile: "/tmp/password",
                                   encryptionKeyFile: "/tmp/key",
                                   acknowledgeV42CompatibleSchema: true) { result in
            guard case let .failure(error) = result else {
                return XCTFail("expected ambiguous migration failure")
            }
            XCTAssertEqual(
                error.localizedDescription,
                "TeslaMate import failed; retry the import."
            )
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertTrue(try configContents(in: home).contains("interval_seconds = 0"))
        XCTAssertEqual(events.values, ["check", "service:stop", "migrate"])
    }

    func testStartedImportNeverRollsBackConfigurationAfterFailure() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let config = home.appendingPathComponent(
            "Library/Application Support/Teslatlas Hub/config.toml"
        )
        let runner = MutatingFailureRunner(
            error: HubActionError.commandFailed("copy failed")
        ) {
            try FileManager.default.removeItem(at: config)
            try FileManager.default.createDirectory(at: config, withIntermediateDirectories: false)
        }
        let controller = HubController(
                                       commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "started import failure surfaced")

        controller.importTeslaMate(source: "postgres://reader@localhost/teslamate",
                                   carID: "1",
                                   passwordFile: "/tmp/password",
                                   encryptionKeyFile: "/tmp/key",
                                   acknowledgeV42CompatibleSchema: true) { result in
            guard case let .failure(error) = result else {
                return XCTFail("expected import failure")
            }
            XCTAssertEqual(
                error.localizedDescription,
                "TeslaMate import failed; retry the import."
            )
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertTrue(controller.hasPendingMigrationHandover)
    }

    func testInstalledAccountConfigureInstallsBeforeSingleSetupAndRestarts() throws {
        // Pause, one setup with the embedded program, then the ordinary Start (which
        // carries the held record); no installer remains.
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success("ok"))
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(
                                       commandRunner: runner,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)
        let finished = expectation(description: "account configured")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:pause", "setup", "service:start", "command"])
        XCTAssertNotNil(runner.stdin)
        XCTAssertTrue(runner.arguments[0].contains("--all-vehicles"))
        XCTAssertEqual(runner.arguments.count, 2, "one setup, then the status refresh")
        XCTAssertTrue(try configContents(in: home).contains("provider = \"legacy\""))
    }

    func testInstalledAccountSetupFailureKeepsLegacySelectedAndHubStopped() throws {
        // C1: the selected configuration is kept; the loaded Hub is recovered once and
        // the failure never claims the Hub stays stopped.
        let events = EventRecorder()
        let runner = ScriptedRunner(
            events: events,
            result: .failure(HubActionError.commandFailed("setup failed"))
        )
        let service = ScriptedService(events: events, loadState: .loaded)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)
        let finished = expectation(description: "setup failure returned")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case let .failure(error) = result else { return XCTFail("expected setup failure") }
            XCTAssertTrue(error.localizedDescription.contains("setup failed"))
            XCTAssertFalse(error.localizedDescription.contains("remains stopped"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertTrue(try configContents(in: home).contains("provider = \"legacy\""))
        XCTAssertEqual(events.values, ["service:pause", "setup", "service:recover"])
    }

    func testInstalledLegacyFailureKeepsPreviouslyStoppedHubStopped() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(
            events: events,
            result: .failure(HubActionError.commandFailed("setup failed"))
        )
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: events, loadState: .unloaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)
        let finished = expectation(description: "stopped setup failure returned")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case .failure = result else { return XCTFail("expected setup failure") }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertTrue(try configContents(in: home).contains("provider = \"legacy\""))
        XCTAssertEqual(events.values, ["service:pause", "setup", "service:recover"],
                       "a job recorded unloaded is not started by the fake's recovery; the real controller issues nothing")
    }

    func testInstalledLegacyStopFailureKeepsFleetConfigUnchanged() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success("unused"))
        let service = ScriptedService(events: events)
        service.result = .failure(HubActionError.commandFailed("stop failed"))
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)
        let finished = expectation(description: "stop failure returned")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case .failure = result else { return XCTFail("expected stop failure") }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(try configContents(in: home), original)
        XCTAssertEqual(events.values, ["service:pause", "service:recover"])
    }

    func testInstalledUnconfiguredLegacySetupRunsBeforePackagePreflight() throws {
        // Setup runs before the ordinary Start, whose own writer and Hub preflight follow.
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success("ok"))
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(
                                       commandRunner: runner,
                                       serviceRunner: ScriptedService(events: events),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "unconfigured legacy setup finished")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:pause", "setup", "service:start", "command"])
        XCTAssertTrue(try configContents(in: home).contains("provider = \"legacy\""))
    }

    func testInstalledAccountSetupRejectsOversizedConfigBeforeCommands() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let config = home.appendingPathComponent(
            "Library/Application Support/Teslatlas Hub/config.toml"
        )
        try FileManager.default.createDirectory(at: config.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        try Data(repeating: 0x61, count: 1024 * 1024 + 1).write(to: config)
        let runner = CountingRunner()
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "unsafe config rejected")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case let .failure(error) = result else {
                return XCTFail("oversized config was accepted")
            }
            XCTAssertEqual(error.localizedDescription, "Hub configuration is too large.")
            finished.fulfill()
        }

        wait(for: [finished], timeout: 1)
        XCTAssertEqual(runner.calls, 0)
    }

    func testInstalledAccountSetupRejectsSymlinkedConfigBeforeCommands() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let config = home.appendingPathComponent(
            "Library/Application Support/Teslatlas Hub/config.toml"
        )
        let target = home.appendingPathComponent("replacement-config.toml")
        try "data_dir = \"/tmp/replaced\"\n".write(to: target, atomically: true, encoding: .utf8)
        try FileManager.default.createDirectory(at: config.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        try FileManager.default.createSymbolicLink(at: config, withDestinationURL: target)
        let runner = CountingRunner()
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "symlinked config rejected")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case let .failure(error) = result else {
                return XCTFail("symlinked config was accepted")
            }
            XCTAssertEqual(error.localizedDescription, "Hub configuration is not a regular file.")
            finished.fulfill()
        }

        wait(for: [finished], timeout: 1)
        XCTAssertEqual(runner.calls, 0)
    }

    func testInstalledAccountSetupRejectsFIFOConfigWithoutBlocking() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let config = home.appendingPathComponent(
            "Library/Application Support/Teslatlas Hub/config.toml"
        )
        try FileManager.default.createDirectory(at: config.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        XCTAssertEqual(mkfifo(config.path, 0o600), 0)
        let runner = CountingRunner()
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "FIFO config rejected")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case let .failure(error) = result else {
                return XCTFail("FIFO config was accepted")
            }
            XCTAssertEqual(error.localizedDescription, "Hub configuration is not a regular file.")
            finished.fulfill()
        }

        wait(for: [finished], timeout: 1)
        XCTAssertEqual(runner.calls, 0)
    }

    func testUnconfiguredLegacyInstallerFailureKeepsNewCredentialsStopped() throws {
        // A setup failure (no installer step any more): the new credentials are not rolled back and the
        // pause is followed by exactly one typed recovery.
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events,
                                    result: .failure(HubActionError.commandFailed("setup refused")))
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: events),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "legacy setup failure recovered")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case let .failure(error) = result else {
                return XCTFail("expected setup failure")
            }
            XCTAssertTrue(error.localizedDescription.contains("setup refused"))
            XCTAssertFalse(error.localizedDescription.contains("remains stopped"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:pause", "setup", "service:recover"])
        XCTAssertTrue(try configContents(in: home).contains("provider = \"legacy\""))
    }

    func testAmbiguousLegacySwitchDoesNotRestoreOrRestartOldProvider() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(
            events: events,
            result: .failure(HubActionError.commandFailed(
                "TESLATLAS_PROVIDER_SWITCH_OUTCOME_AMBIGUOUS: old credentials could not be removed"
            ))
        )
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: ScriptedService(events: events, loadState: .loaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)
        let finished = expectation(description: "ambiguous provider switch remains stopped")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case let .failure(error) = result else {
                return XCTFail("expected ambiguous setup failure")
            }
            XCTAssertTrue(error.localizedDescription.contains("needs verification"))
            XCTAssertFalse(error.localizedDescription.contains("remains stopped"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:pause", "setup", "service:recover"])
        XCTAssertTrue(try configContents(in: home).contains("provider = \"legacy\""))
    }

    func testMigrationSourceAllowsUsernameButRejectsPassword() throws {
        XCTAssertNoThrow(try HubController.validateMigrationSource("postgresql://reader@localhost/teslamate"))
        XCTAssertThrowsError(try HubController.validateMigrationSource("postgresql://reader:secret@localhost/teslamate"))
        XCTAssertThrowsError(try HubController.validateMigrationSource("postgres://reader:s%40cret@localhost/teslamate"))
        XCTAssertThrowsError(try HubController.validateMigrationSource("https://localhost/teslamate"))
        XCTAssertThrowsError(try HubController.validateMigrationSource("postgresql:///teslamate"))
        XCTAssertThrowsError(try HubController.validateMigrationSource("postgresql://localhost"))
    }

    func testTOMLBasicStringEscapesPathWithoutBreakingQuotedValue() {
        XCTAssertEqual(HubController.tomlBasicString("/Users/O'Brien\\folder\nnext"),
                       "\"/Users/O'Brien\\\\folder\\nnext\"")
    }

    func testOfflineDefaultsAreAddedWhenTablesOrKeysAreAbsent() {
        let minimal = "data_dir = \"/tmp/hub\"\n"
        let updated = HubController.addOfflineDefaults(to: minimal)
        XCTAssertTrue(updated.contains("[geocoder]\nenabled = false"))
        XCTAssertTrue(updated.contains("[terrain]\nenabled = false"))

        let configured = minimal + "\n[geocoder] # retained\nenabled = true\n\n[terrain]\nenabled = true\n"
        XCTAssertEqual(HubController.addOfflineDefaults(to: configured), configured)

        let missingKeys = minimal + "\n[geocoder] # retained\nurl = \"https://example.invalid\"\n\n[terrain]\n# enabled intentionally absent\ncache = true\n"
        let completed = HubController.addOfflineDefaults(to: missingKeys)
        XCTAssertTrue(completed.contains("[geocoder] # retained\nenabled = false\nurl ="))
        XCTAssertTrue(completed.contains("[terrain]\nenabled = false\n# enabled intentionally absent"))
    }

    func testProcessExecutorDrainsLargeOutputAndRetainsBoundedTail() {
        let finished = expectation(description: "process completed")
        HubProcessExecutor.run(executable: URL(fileURLWithPath: "/bin/sh"),
                               arguments: ["-c", "/usr/bin/yes 0123456789 | /usr/bin/head -c 200000; /usr/bin/printf TAIL_MARKER"],
                               maximumOutputBytes: 4_096) { result in
            switch result {
            case let .success(output):
                XCTAssertLessThanOrEqual(output.utf8.count, 4_096)
                XCTAssertTrue(output.hasSuffix("TAIL_MARKER"))
            case let .failure(error):
                XCTFail(error.localizedDescription)
            }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 5)
    }

    func testProcessExecutorBoundsNonreadingStandardInputAndReapsChild() throws {
        let root = HubTestScratch.directory.appendingPathComponent(UUID().uuidString,
                                                                                 isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let pidFile = root.appendingPathComponent("child-pid")
        let releaseFile = root.appendingPathComponent("release-owned-fixture")
        let ownedProcess = Process()
        let finished = expectation(description: "nonreading child timed out")
        let started = Date()
        let stopChild: () -> Void = {
            // Release only this uniquely named fixture, even if an erroneous
            // executor completion has already fired. Never signal a reused PID.
            Self.cleanupNonreadingFixture(ownedProcess, releaseFile: releaseFile)
        }
        let watchdog = DispatchWorkItem(block: stopChild)
        DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + 3, execute: watchdog)
        defer { watchdog.cancel(); stopChild() }
        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: "/usr/bin/perl"),
            arguments: Self.nonreadingFixtureArguments(pidFile: pidFile, releaseFile: releaseFile),
            stdin: String(repeating: "synthetic input", count: 200_000),
            timeout: 0.2,
            terminationGrace: 0.1,
            outputDrainTimeout: 0.1,
            processFactory: { ownedProcess }
        ) { result in
            let completionElapsed = Date().timeIntervalSince(started)
            let publishedPID = (try? String(contentsOf: pidFile, encoding: .utf8)).flatMap { Int32($0) }
            let reapingProbe = publishedPID.map { pid -> (result: Int32, error: Int32) in
                let result = Darwin.kill(pid, 0)
                return (result, errno)
            }
            Self.validateNonreadingFixtureCompletion(ownedProcess, releaseFile: releaseFile) {
                XCTFail($0)
            }
            guard case let .failure(error) = result else {
                finished.fulfill()
                return XCTFail("nonreading child unexpectedly succeeded")
            }
            XCTAssertEqual(error.localizedDescription, "Hub command timed out.")
            XCTAssertLessThan(completionElapsed, 1.5)
            if let reapingProbe {
                XCTAssertEqual(reapingProbe.result, -1)
                XCTAssertEqual(reapingProbe.error, ESRCH)
            } else { XCTFail("child did not publish its PID") }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)
    }

    func testNonreadingFixtureCleanupAfterInjectedEarlyCompletionReapsExactOwnedProcess() throws {
        let root = HubTestScratch.directory.appendingPathComponent(UUID().uuidString,
                                                                                 isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let pidFile = root.appendingPathComponent("child-pid")
        let releaseFile = root.appendingPathComponent("release-owned-fixture")
        let process = Process()
        let readyPipe = Pipe()
        let errorPipe = Pipe()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/perl")
        process.arguments = Self.nonreadingFixtureArguments(pidFile: pidFile, releaseFile: releaseFile)
        process.standardOutput = readyPipe
        process.standardError = errorPipe
        try process.run()
        defer { Self.cleanupNonreadingFixture(process, releaseFile: releaseFile) }
        try readyPipe.fileHandleForWriting.close()
        try errorPipe.fileHandleForWriting.close()
        // The fixture emits READY only after publishing its PID. Its autonomous
        // lifetime bounds EOF even when startup fails; no XCTest waiter race.
        let ready = try readyPipe.fileHandleForReading.read(upToCount: 6) ?? Data()
        guard ready == Data("READY\n".utf8) else {
            Self.cleanupNonreadingFixture(process, releaseFile: releaseFile)
            let error = String(decoding: errorPipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            XCTFail("Owned fixture did not become ready (exit \(process.terminationStatus)): \(error)")
            return
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: pidFile.path))
        // Inject completion before exit. Capture the real validator's failure
        // callback rather than intentionally entering XCTest symbolication.
        var reportedFailures: [String] = []
        var reapedBeforeReporting = false
        Self.validateNonreadingFixtureCompletion(process, releaseFile: releaseFile) { diagnostic in
            reportedFailures.append(diagnostic)
            reapedBeforeReporting = !process.isRunning
        }
        XCTAssertEqual(reportedFailures, ["Executor reported completion while its fixture still runs"])
        XCTAssertTrue(reapedBeforeReporting)
        XCTAssertFalse(process.isRunning)
        XCTAssertEqual(process.terminationStatus, 0)
    }

    private static func cleanupNonreadingFixture(_ process: Process, releaseFile: URL) {
        try? Data().write(to: releaseFile)
        if process.processIdentifier > 0 { process.waitUntilExit() }
    }

    private static func validateNonreadingFixtureCompletion(
        _ process: Process,
        releaseFile: URL,
        reportFailure: (String) -> Void
    ) {
        let completedWhileRunning = process.isRunning
        cleanupNonreadingFixture(process, releaseFile: releaseFile)
        if completedWhileRunning {
            reportFailure("Executor reported completion while its fixture still runs")
        }
    }

    private static func nonreadingFixtureArguments(pidFile: URL, releaseFile: URL) -> [String] {
        ["-e", """
        $| = 1;
        $SIG{TERM} = 'IGNORE';
        open(my $pid, '>', $ARGV[0]) or die $!;
        print $pid $$;
        close($pid);
        print 'READY', chr(10);
        my $deadline = time() + 5;
        while (!-e $ARGV[1] && time() < $deadline) { select(undef, undef, undef, 0.01); }
        """, pidFile.path, releaseFile.path]
    }

    func testProcessExecutorTransfersLargeStandardInputThroughEOF() {
        let input = String(repeating: "abcdef", count: 200_000)
        let finished = expectation(description: "large input consumed")
        HubProcessExecutor.run(executable: URL(fileURLWithPath: "/usr/bin/wc"),
                               arguments: ["-c"], stdin: input, timeout: 3) { result in
            switch result {
            case let .success(output):
                XCTAssertEqual(Int(output.trimmingCharacters(in: .whitespacesAndNewlines)), input.utf8.count)
            case let .failure(error): XCTFail(error.localizedDescription)
            }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 4)
    }

    func testProcessExecutorUsesOneDeadlineForInputAndChildExit() {
        let finished = expectation(description: "input and exit share deadline")
        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: "/usr/bin/perl"),
            arguments: ["-e", """
                select(undef, undef, undef, 0.4);
                local $/;
                my $input = <STDIN>;
                select(undef, undef, undef, 0.4);
                print length($input);
                """],
            stdin: String(repeating: "a", count: 2 * 1024 * 1024),
            timeout: 0.6, terminationGrace: 0.1, outputDrainTimeout: 0.1
        ) { result in
            guard case let .failure(error) = result else {
                finished.fulfill()
                return XCTFail("input transfer incorrectly reset the child deadline")
            }
            XCTAssertEqual(error.localizedDescription, "Hub command timed out.")
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)
    }

    func testProcessExecutorRejectsOversizedInputBeforeLaunchingChild() {
        let marker = HubTestScratch.directory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: marker) }
        let finished = expectation(description: "oversized input rejected")
        HubProcessExecutor.run(executable: URL(fileURLWithPath: "/bin/sh"),
                               arguments: ["-c", "touch \"$TESLATLAS_INPUT_MARKER\""],
                               stdin: String(repeating: "a", count: HubProcessExecutor.maximumStandardInputBytes + 1),
                               environment: ["TESLATLAS_INPUT_MARKER": marker.path]) { result in
            guard case let .failure(error) = result else {
                finished.fulfill()
                return XCTFail("oversized input unexpectedly succeeded")
            }
            XCTAssertEqual(error.localizedDescription, "Hub command input exceeds its size limit.")
            XCTAssertFalse(FileManager.default.fileExists(atPath: marker.path))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)
    }

    func testProcessExecutorReportsClosedInputWithoutSIGPIPE() {
        let finished = expectation(description: "closed stdin fails safely")
        HubProcessExecutor.run(executable: URL(fileURLWithPath: "/usr/bin/perl"),
                               arguments: ["-e", "close(STDIN); sleep 5;"],
                               stdin: String(repeating: "a", count: 2 * 1024 * 1024),
                               timeout: 1, terminationGrace: 0.1, outputDrainTimeout: 0.1) { result in
            guard case let .failure(error) = result else {
                finished.fulfill()
                return XCTFail("closed stdin unexpectedly succeeded")
            }
            XCTAssertEqual((error as NSError).domain, NSPOSIXErrorDomain)
            XCTAssertEqual((error as NSError).code, Int(EPIPE))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)
    }

    func testProcessExecutorDeliversLineBeforeChildExit() {
        let releaseFile = HubTestScratch.directory
            .appendingPathComponent("teslatlas-process-output-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: releaseFile) }
        let lineDelivered = expectation(description: "line delivered while child is running")
        let finished = expectation(description: "process completed")

        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: "/bin/sh"),
            arguments: [
                "-c",
                """
                /usr/bin/printf 'progress\\n'
                attempt=0
                while [ "$attempt" -lt 40 ]; do
                    /bin/test -e "$TESLATLAS_TEST_RELEASE_FILE" && exit 0
                    /bin/sleep 0.025
                    attempt=$((attempt + 1))
                done
                exit 23
                """
            ],
            environment: ["TESLATLAS_TEST_RELEASE_FILE": releaseFile.path],
            onOutputLine: { line in
                guard line == "progress" else { return }
                XCTAssertTrue(FileManager.default.createFile(atPath: releaseFile.path, contents: Data()))
                lineDelivered.fulfill()
            }
        ) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [lineDelivered, finished], timeout: 3)
    }

    func testProcessExecutorStreamsLinesAndRetainsSuccessfulOutput() {
        let finished = expectation(description: "streamed process completed")
        var lines: [String] = []
        var events: [String] = []
        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: "/bin/sh"),
            arguments: [
                "-c",
                "/usr/bin/printf 'fi'; /bin/sleep 0.05; "
                    + "/usr/bin/printf 'rst\\n'; /usr/bin/printf 'last-without-newline'"
            ],
            onOutputLine: {
                lines.append($0)
                events.append("line:\($0)")
            }
        ) { result in
            events.append("completion")
            switch result {
            case let .success(output):
                XCTAssertEqual(output, "first\nlast-without-newline")
                XCTAssertEqual(lines, ["first", "last-without-newline"])
                XCTAssertEqual(events, [
                    "line:first",
                    "line:last-without-newline",
                    "completion"
                ])
            case let .failure(error):
                XCTFail(error.localizedDescription)
            }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 5)
    }

    func testProcessExecutorStreamingRetainsFailureOutput() {
        let finished = expectation(description: "streamed process failure completed")
        let progress = #"{"event":"migration_progress","completedRows":2,"totalRows":4}"#
        var lines: [String] = []
        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: "/bin/sh"),
            arguments: ["-c", "/usr/bin/printf '%s\\n' '\(progress)'; /usr/bin/printf 'failure-detail' >&2; exit 7"],
            onOutputLine: { lines.append($0) }
        ) { result in
            guard case let .failure(error) = result else {
                return XCTFail("failing process unexpectedly succeeded")
            }
            XCTAssertTrue(lines.contains(progress))
            XCTAssertTrue(error.localizedDescription.contains(progress))
            XCTAssertTrue(error.localizedDescription.contains("failure-detail"))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 5)
    }

    func testProcessExecutorStopsStreamingWhenDescendantKeepsOutputPipeOpen() {
        let finished = expectation(description: "process completion is not blocked by descendant pipe")
        let lateLine = expectation(description: "no output arrives after completion")
        lateLine.isInverted = true
        let events = EventRecorder()
        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: "/bin/sh"),
            arguments: [
                "-c",
                "( /bin/sleep 0.25; /usr/bin/printf 'late\\n' ) & /usr/bin/printf 'early\\n'"
            ],
            outputDrainTimeout: 0.03,
            onOutputLine: { line in
                events.append("line:\(line)")
                if line == "late" { lateLine.fulfill() }
            }
        ) { result in
            events.append("completion")
            guard case let .failure(error) = result else {
                return XCTFail("open descendant pipe unexpectedly succeeded")
            }
            XCTAssertEqual(error.localizedDescription, "Hub command output did not close.")
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        wait(for: [lateLine], timeout: 0.5)
        XCTAssertFalse(events.values.contains("line:late"))
        XCTAssertEqual(events.values.last, "completion")
    }

    func testProcessOutputPipeReaderCancellationStopsWhileWriterRemainsOpen() {
        let pipe = Pipe()
        defer { try? pipe.fileHandleForWriting.close() }
        let sawOutput = expectation(description: "reader consumed initial output")
        let stopped = expectation(description: "reader stopped without pipe EOF")
        let retained = BoundedProcessOutput(maximumBytes: 64)
        let reader = ProcessOutputPipeReader(fileHandle: pipe.fileHandleForReading) { chunk in
            retained.append(chunk)
            sawOutput.fulfill()
        }

        pipe.fileHandleForWriting.write(Data("initial output".utf8))
        DispatchQueue.global(qos: .userInitiated).async {
            reader.run()
            stopped.fulfill()
        }

        wait(for: [sawOutput], timeout: 1)
        reader.cancel()
        wait(for: [stopped], timeout: 1)
        XCTAssertEqual(reader.outcome, .cancelled)
        XCTAssertEqual(String(decoding: retained.snapshot(), as: UTF8.self), "initial output")
    }

    func testProcessExecutorTimeoutDoesNotWaitForBlockedOutputCallback() {
        let callbackStarted = expectation(description: "output callback started")
        let finished = expectation(description: "timeout completed independently")
        let releaseCallback = DispatchSemaphore(value: 0)
        let started = Date()

        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: "/bin/sh"),
            arguments: [
                "-c",
                "/usr/bin/printf 'progress\\n'; trap '' TERM; while :; do /bin/sleep 1; done"
            ],
            timeout: 0.05,
            terminationGrace: 0.05,
            outputDrainTimeout: 0.05,
            onOutputLine: { line in
                guard line == "progress" else { return }
                callbackStarted.fulfill()
                _ = releaseCallback.wait(timeout: .now() + 0.75)
            }
        ) { result in
            guard case let .failure(error) = result else {
                return XCTFail("hung process unexpectedly succeeded")
            }
            XCTAssertEqual(error.localizedDescription, "Hub command timed out.")
            XCTAssertLessThan(Date().timeIntervalSince(started), 0.4)
            finished.fulfill()
        }

        wait(for: [callbackStarted], timeout: 1)
        wait(for: [finished], timeout: 0.4)
        releaseCallback.signal()
    }

    func testBoundedProcessOutputRetainsOnlyNewestBytes() {
        let output = BoundedProcessOutput(maximumBytes: 8)
        output.append(Data("12345".utf8))
        output.append(Data("67890".utf8))
        XCTAssertEqual(String(decoding: output.snapshot(), as: UTF8.self), "34567890")

        output.append(Data("abcdefghijk".utf8))
        XCTAssertEqual(String(decoding: output.snapshot(), as: UTF8.self), "defghijk")
    }

    func testProcessExecutorTerminatesThenKillsHungCommandAtDeadline() {
        let finished = expectation(description: "hung process timed out")
        let started = Date()
        HubProcessExecutor.run(executable: URL(fileURLWithPath: "/bin/sh"),
                               arguments: ["-c", "trap '' TERM; while :; do /bin/sleep 1; done"],
                               timeout: 0.1,
                               terminationGrace: 0.1,
                               outputDrainTimeout: 0.1) { result in
            guard case let .failure(error) = result else {
                return XCTFail("hung process unexpectedly succeeded")
            }
            XCTAssertEqual(error.localizedDescription, "Hub command timed out.")
            XCTAssertLessThan(Date().timeIntervalSince(started), 2)
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)
    }

    func testSetupInvocationKeepsTokensOutOfArguments() throws {
        let tokens = TeslaAuthTokens(accessToken: "access-secret", refreshToken: "refresh-secret")
        let invocation = try HubController.setupInvocation(
            configPath: URL(fileURLWithPath: "/tmp/config.toml"),
            tokens: tokens,
            vehicleID: 70
        )
        XCTAssertEqual(invocation.arguments, [
            "--config", "/tmp/config.toml", "setup", "--tokens-stdin", "--vehicle-id", "70"
        ])
        XCTAssertFalse(invocation.arguments.joined().contains("access-secret"))
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(invocation.standardInput.utf8)) as? [String: String])
        XCTAssertEqual(json["accessToken"], "access-secret")
        XCTAssertEqual(json["refreshToken"], "refresh-secret")
        let all = try HubController.setupInvocation(
            configPath: URL(fileURLWithPath: "/tmp/config.toml"),
            tokens: tokens,
            vehicleID: nil
        )
        XCTAssertEqual(all.arguments, [
            "--config", "/tmp/config.toml", "setup", "--tokens-stdin", "--all-vehicles"
        ])
        XCTAssertEqual(HubController.oldCompatibleSetupInvocation(all).arguments, [
            "--config", "/tmp/config.toml", "setup", "--tokens-stdin"
        ])
        XCTAssertEqual(HubController.oldCompatibleSetupInvocation(invocation), invocation)
    }

    func testTeslaOAuthUsesPKCEAndExactCallbackContract() throws {
        let verifier = String(repeating: "v", count: 64)
        let flow = try TeslaOAuthFlow(state: "expected-state", verifier: verifier)
        let query = Dictionary(uniqueKeysWithValues:
            URLComponents(url: flow.authorizationURL, resolvingAgainstBaseURL: false)!
                .queryItems!.map { ($0.name, $0.value ?? "") })
        XCTAssertEqual(query["client_id"], "ownerapi")
        XCTAssertEqual(query["redirect_uri"], "tesla://auth/callback")
        XCTAssertEqual(query["scope"], "openid email offline_access")
        XCTAssertEqual(query["state"], "expected-state")
        XCTAssertEqual(query["code_challenge_method"], "S256")
        XCTAssertNotEqual(query["code_challenge"], verifier)

        let callback = try XCTUnwrap(URL(string:
            "tesla://auth/callback?code=one-time-code&state=expected-state&issuer=https%3A%2F%2Fauth.tesla.cn"))
        guard case let .exchange(exchange) = try flow.callback(callback) else {
            return XCTFail("expected token exchange")
        }
        XCTAssertEqual(exchange.url, TeslaOAuthFlow.chinaTokenEndpoint)
        let body = String(decoding: exchange.body, as: UTF8.self)
        XCTAssertTrue(body.contains("client_id=ownerapi"))
        XCTAssertTrue(body.contains("code_verifier=\(verifier)"))
        XCTAssertFalse(body.contains("refresh"))

        let mismatch = URL(string:
            "tesla://auth/callback?code=x&state=wrong&issuer=https%3A%2F%2Fauth.tesla.com")!
        XCTAssertThrowsError(try flow.callback(mismatch)) { error in
            XCTAssertEqual(error as? TeslaAuthError, .stateMismatch)
        }
        let cancelled = URL(string: "tesla://auth/callback?error=login_cancelled")!
        XCTAssertEqual(try flow.callback(cancelled), .cancelled)
    }

    func testTeslaTokenResponseBufferRejectsOverflowBeforeAppendingIt() {
        var buffer = TeslaTokenResponseBuffer()
        XCTAssertTrue(buffer.append(Data(repeating: 1, count: TeslaTokenResponseBuffer.maximumBytes)))
        XCTAssertEqual(buffer.data.count, TeslaTokenResponseBuffer.maximumBytes)
        XCTAssertFalse(buffer.append(Data([2])))
        XCTAssertEqual(buffer.data.count, TeslaTokenResponseBuffer.maximumBytes)
    }

    func testServiceLogTailIsBoundedAndRefusesSymlinks() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let log = home.appendingPathComponent("hub.out.log")
        let link = home.appendingPathComponent("linked.log")
        try Data(String(repeating: "a", count: 8192).utf8).write(to: log)
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: log)

        let tail = try XCTUnwrap(HubController.logTail(of: log, maximumBytes: 1024))
        XCTAssertEqual(Data(tail.utf8).count, 1024)
        XCTAssertNil(HubController.logTail(of: link, maximumBytes: 1024))
        XCTAssertNil(HubController.logTail(of: log, maximumBytes: 0))
    }

    private func temporaryHome() throws -> URL {
        let url = HubTestScratch.directory
            .appendingPathComponent("teslatlas-hub-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: false)
        return url
    }

    private func writeCollectorConfig(in home: URL, provider: String) throws -> String {
        let config = home.appendingPathComponent("Library/Application Support/Teslatlas Hub/config.toml")
        try FileManager.default.createDirectory(at: config.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        let content = "data_dir = \"/tmp/hub\"\n\n[collector]\nprovider = \"\(provider)\"\ninterval_seconds = 60\n"
        try content.write(to: config, atomically: true, encoding: .utf8)
        return content
    }

    private func configContents(in home: URL) throws -> String {
        let config = home.appendingPathComponent("Library/Application Support/Teslatlas Hub/config.toml")
        return try String(contentsOf: config, encoding: .utf8)
    }

    private func buttons(in view: NSView?) -> [NSButton] {
        guard let view else { return [] }
        var result = (view as? NSButton).map { [$0] } ?? view.subviews.flatMap { buttons(in: $0) }
        if view === view.window?.contentView {
            result += view.window?.toolbar?.items.compactMap(\.view).flatMap { buttons(in: $0) } ?? []
        }
        return result
    }

    private func labels(in view: NSView?) -> [NSTextField] {
        guard let view else { return [] }
        return (view as? NSTextField).map { [$0] } ?? view.subviews.flatMap { labels(in: $0) }
    }

    private func descendantViews(in view: NSView) -> [NSView] {
        [view] + view.subviews.flatMap { descendantViews(in: $0) }
    }

    private func popups(in view: NSView?) -> [NSPopUpButton] {
        guard let view else { return [] }
        return (view as? NSPopUpButton).map { [$0] } ?? view.subviews.flatMap { popups(in: $0) }
    }

    private func imageViews(in view: NSView?) -> [NSImageView] {
        guard let view else { return [] }
        return (view as? NSImageView).map { [$0] } ?? view.subviews.flatMap { imageViews(in: $0) }
    }

    private func boxes(in view: NSView?) -> [NSBox] {
        guard let view else { return [] }
        return (view as? NSBox).map { [$0] } ?? view.subviews.flatMap { boxes(in: $0) }
    }
}

/// Records the lifecycle actions a details window forwards.
private final class LifecycleCallRecorder {
    private(set) var names: [String] = []
    var confirmations: [String] = []

    var actions: HubServiceLifecycleActions {
        HubServiceLifecycleActions(
            start: { [self] in names.append("start") },
            stop: { [self] in names.append("stop") },
            restart: { [self] in names.append("restart") },
            remove: { [self] in names.append("remove") },
            deleteHubAndData: { [self] in names.append("deleteHubAndData") }
        )
    }
}

private extension HubDashboardActions {
    static let noOp = HubDashboardActions(
        start: {},
        stop: {},
        restart: {},
        setup: {},
        diagnostics: {},
        vehicle: HubVehicleCardActions(select: { _ in }, command: { _, _ in }),
        serviceDetails: {},
        dataFolder: {}
    )
}

private extension HubVehicleCardActions {
    static let noOp = HubVehicleCardActions(select: { _ in }, command: { _, _ in })
}

private extension HubNavigationActions {
    static let noOp = HubNavigationActions(select: { _ in }, diagnostics: {}, logs: {},
                                           serviceDetails: {}, importTeslaMate: {}, connectTesla: {},
                                           accountMenu: { NSMenu() }, appearance: {})
}

private final class CountingRunner: HubCommandRunning {
    private(set) var calls = 0
    private(set) var stdin: String?

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        calls += 1
        completion(.success(""))
    }

    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        self.stdin = stdin
        run(arguments: arguments, completion: completion)
    }
}

private final class CommandMapRunner: HubCommandRunning {
    private(set) var commands: [String] = []
    private(set) var arguments: [[String]] = []
    private let responses: [String: Result<String, Error>]

    init(responses: [String: Result<String, Error>]) {
        self.responses = responses
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        self.arguments.append(arguments)
        let command = arguments.last { $0 != "--config" && !$0.hasSuffix("config.toml") } ?? arguments.last ?? ""
        commands.append(command)
        completion(responses[command] ?? .failure(HubActionError.commandFailed("unexpected \(command)")))
    }
}

private final class RecordingCommandRunner: HubCommandRunning {
    private let result: Result<String, Error>
    private(set) var arguments: [[String]] = []

    init(result: Result<String, Error>) { self.result = result }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        self.arguments.append(arguments)
        completion(result)
    }
}

private final class PendingServiceRunner: HubServiceControlling {
    var loadState: HubServiceLoadState
    private(set) var arguments: [[String]] = []
    private var completions: [(Result<String, Error>) -> Void] = []
    private var pendingResults: [Result<String, Error>] = []

    init(loadState: HubServiceLoadState) {
        self.loadState = loadState
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        self.arguments.append(arguments)
        if pendingResults.isEmpty {
            completions.append(completion)
        } else {
            completion(pendingResults.removeFirst())
        }
    }
    func run(arguments: [String], owner: HubLifecycleToken,
             completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, completion: completion)
    }
    func restartRecordHeld(by owner: HubLifecycleToken) -> Bool { false }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        completion(loadState)
    }

    func complete(_ result: Result<String, Error>) {
        guard !completions.isEmpty else {
            pendingResults.append(result)
            return
        }
        completions.removeFirst()(result)
    }

    func complete(at index: Int, _ result: Result<String, Error>) {
        completions.remove(at: index)(result)
    }

    // New lifecycle requirements, implemented explicitly (no protocol-extension defaults).
    private(set) var recoverCalls = 0
    private(set) var releaseCalls = 0
    private(set) var deleteCalls = 0
    var restartDutyOutstanding: Bool { false }
    func whenRestartDutySettled(_ callback: @escaping () -> Void) { callback() }
    func recoverAfterPause(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) {
        recoverCalls += 1
        completion(.success(""))
    }
    func releaseRestartRecord(owner: HubLifecycleToken) {
        releaseCalls += 1
    }

    func reconcile(owner: HubLifecycleToken, ensure: @escaping () throws -> HubLaunchAgentChange,
                   completion: @escaping (HubReconcileOutcome) -> Void) { completion(.completed) }
    func deleteHubAndData(owner: HubLifecycleToken, completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        deleteCalls += 1
        completion(.failure(HubActionError.preview))
    }
}

private final class PendingCommandRunner: HubCommandRunning {
    private var completion: ((Result<String, Error>) -> Void)?
    private var pendingResult: Result<String, Error>?
    private(set) var arguments: [[String]] = []

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        self.arguments.append(arguments)
        self.completion = completion
        if let pendingResult {
            self.pendingResult = nil
            complete(pendingResult)
        }
    }

    func complete(_ result: Result<String, Error>) {
        guard let completion else {
            pendingResult = result
            return
        }
        self.completion = nil
        completion(result)
    }
}

private final class PendingVehicleControlRunner: HubCommandRunning {
    private let status: String
    private var controlCompletion: ((Result<String, Error>) -> Void)?
    private(set) var arguments: [[String]] = []

    init(status: String) { self.status = status }

    var hasPendingControl: Bool { controlCompletion != nil }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        self.arguments.append(arguments)
        if arguments.contains("status") {
            completion(.success(status))
        } else {
            controlCompletion = completion
        }
    }

    func completeControl(_ result: Result<String, Error>) {
        guard let controlCompletion else {
            XCTFail("vehicle control was not pending")
            return
        }
        self.controlCompletion = nil
        controlCompletion(result)
    }
}

private final class OutOfOrderCommandRunner: HubCommandRunning {
    private var completions: [(Result<String, Error>) -> Void] = []

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        completions.append(completion)
    }

    func complete(at index: Int, with result: Result<String, Error>) {
        completions[index](result)
    }
}

private final class MutatingFailureRunner: HubCommandRunning {
    private let error: Error
    private let mutation: () throws -> Void

    init(error: Error, mutation: @escaping () throws -> Void) {
        self.error = error
        self.mutation = mutation
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        if arguments.contains("teslamate-check") {
            completion(.success(
                #"{"status":"compatible","reasonCode":"v4_2_compatible_schema","requiredVersion":"4.2.0","guidance":"Ready."}"#
            ))
            return
        }
        do {
            try mutation()
            completion(.failure(error))
        } catch {
            completion(.failure(error))
        }
    }
}

private final class EventRecorder {
    private let lock = NSLock()
    private var events: [String] = []

    var values: [String] {
        lock.lock()
        defer { lock.unlock() }
        return events
    }

    func append(_ event: String) {
        lock.lock()
        events.append(event)
        lock.unlock()
    }
}

private final class ScriptedRunner: HubCommandRunning {
    private let events: EventRecorder
    private let result: Result<String, Error>
    private var unsupportedAllVehiclesAttempts: Int
    private(set) var stdin: String?
    private(set) var arguments: [[String]] = []

    init(events: EventRecorder,
         result: Result<String, Error>,
         unsupportedAllVehiclesAttempts: Int = 0) {
        self.events = events
        self.result = result
        self.unsupportedAllVehiclesAttempts = unsupportedAllVehiclesAttempts
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        self.arguments.append(arguments)
        if arguments.contains("teslamate-check") {
            events.append("check")
            completion(.success(
                #"{"status":"compatible","reasonCode":"v4_2_compatible_schema","requiredVersion":"4.2.0","guidance":"Ready."}"#
            ))
            return
        }
        let event = arguments.contains("migrate") ? "migrate" :
            (arguments.contains("setup") ? "setup" : "command")
        events.append(event)
        if arguments.contains("--all-vehicles"), unsupportedAllVehiclesAttempts > 0 {
            unsupportedAllVehiclesAttempts -= 1
            completion(.failure(HubActionError.commandFailed(
                "error: unexpected argument '--all-vehicles' found"
            )))
            return
        }
        completion(result)
    }

    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        self.stdin = stdin
        run(arguments: arguments, completion: completion)
    }
}

private final class ScriptedService: HubServiceControlling {
    private let events: EventRecorder
    private let loadState: HubServiceLoadState
    var result: Result<String, Error> = .success("")
    /// Per-action overrides of `result`, keyed by the last service argument.
    var actionResults: [String: Result<String, Error>] = [:]
    private(set) var stateQueries = 0
    var recordsStateQueries = false

    init(events: EventRecorder, loadState: HubServiceLoadState = .unloaded) {
        self.events = events
        self.loadState = loadState
    }

    /// Holds the pause's completion until the test releases it (a custody window held open).
    var holdPause = false
    private(set) var heldPause: ((Result<String, Error>) -> Void)?
    /// Holds Delete's completion until the test releases it (a Delete left in flight).
    var holdDelete = false
    private(set) var heldDelete: ((Result<HubDataRemovalReport, Error>) -> Void)?

    func releasePause() {
        let held = heldPause
        heldPause = nil
        held?(actionResults["pause"] ?? result)
    }

    func releaseDelete(_ outcome: Result<HubDataRemovalReport, Error>) {
        let held = heldDelete
        heldDelete = nil
        held?(outcome)
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        events.append("service:\(arguments.last ?? "unknown")")
        if holdPause, arguments.last == "pause" { heldPause = completion; return }
        completion(actionResults[arguments.last ?? ""] ?? result)
    }
    func run(arguments: [String], owner: HubLifecycleToken,
             completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, completion: completion)
    }
    func restartRecordHeld(by owner: HubLifecycleToken) -> Bool { false }

    /// Consumed one per query; the last entry repeats. Empty: `loadState` always.
    var loadStateSequence: [HubServiceLoadState] = []

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        stateQueries += 1
        if recordsStateQueries { events.append("state") }
        if !loadStateSequence.isEmpty {
            completion(loadStateSequence.count > 1 ? loadStateSequence.removeFirst() : loadStateSequence[0])
            return
        }
        completion(loadState)
    }

    // New lifecycle requirements, implemented explicitly (no protocol-extension defaults).
    var recoverResult: Result<String, Error> = .success("")
    private(set) var recoverCalls = 0
    private(set) var releaseCalls = 0
    private(set) var deleteCalls = 0
    var deleteResult: Result<HubDataRemovalReport, Error> = .failure(HubActionError.preview)
    var restartDutyOutstanding: Bool { false }
    func whenRestartDutySettled(_ callback: @escaping () -> Void) { callback() }
    func recoverAfterPause(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) {
        events.append("service:recover")
        recoverCalls += 1
        completion(recoverResult)
    }
    func releaseRestartRecord(owner: HubLifecycleToken) {
        events.append("service:release")
        releaseCalls += 1
    }

    func reconcile(owner: HubLifecycleToken, ensure: @escaping () throws -> HubLaunchAgentChange,
                   completion: @escaping (HubReconcileOutcome) -> Void) {
        let change: HubLaunchAgentChange
        do { change = try ensure() } catch { completion(.writerFailed(error)); return }
        guard change != .unchanged else { completion(.completed); return }
        loadedState { [self] state in
            guard case .loaded = state else { completion(.completed); return }
            run(arguments: ["service", "restart"]) { result in
                if case let .failure(error) = result { completion(.restartFailed(error)) } else { completion(.completed) }
            }
        }
    }
    func deleteHubAndData(owner: HubLifecycleToken, completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        deleteCalls += 1
        if holdDelete { heldDelete = completion; return }
        completion(deleteResult)
    }
}


// MARK: - Lifecycle wiring (E1)

extension HubControllerTests {
    /// Every `service` action a site issues, in order, plus the sites' own commands.
    private final class SiteRunner: HubCommandRunning {
        static let historyCompatibility = #"{"status":"compatible","reasonCode":"v4_2_compatible_schema","requiredVersion":"4.2.0","guidance":"Ready.","credentialScope":"history-only","applicationVersionStatus":"unknown"}"#
        static let compatibility = #"{"status":"compatible","reasonCode":"v4_2_compatible_schema","requiredVersion":"4.2.0","guidance":"Ready."}"#
        static let historyReady = #"{"status":"ready","credentialScope":"history-only","publishedVehicles":1,"collectorEnabled":false}"#
        private(set) var commands: [String] = []
        var historyPreflightFails = false

        func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
            let command = arguments.first { !$0.hasPrefix("-") && !$0.hasSuffix("config.toml") } ?? ""
            commands.append(command)
            if arguments.contains("teslamate-check") {
                completion(.success(arguments.contains("--history-only") ? Self.historyCompatibility : Self.compatibility))
            } else if arguments.contains("migrate") {
                completion(.success(#"{"status":"imported","captureMode":"online-snapshot"}"#))
            } else if arguments.contains("preflight") && arguments.contains("--history-only") {
                completion(historyPreflightFails ? .failure(HubActionError.commandFailed("not ready")) : .success(Self.historyReady))
            } else {
                completion(.success("{}"))
            }
        }

        func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
            run(arguments: arguments, completion: completion)
        }
    }

    private func siteHome() throws -> URL {
        let home = try temporaryHome()
        _ = try writeCollectorConfig(in: home, provider: "legacy")
        return home
    }

    private func writeMarker(in home: URL, phase: String, historyOnly: Bool) throws {
        let folder = home.appendingPathComponent("Library/Application Support/Teslatlas Hub")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try """
        {"phase":"\(phase)","previousIntervalSeconds":60,"previousProvider":"legacy","historyOnly":\(historyOnly)}
        """.write(to: folder.appendingPathComponent(".teslamate-handover-pending"), atomically: true, encoding: .utf8)
        if historyOnly {
            try HistoryOnlyControl.write(for: folder.appendingPathComponent("config.toml"))
        }
    }

    func testStopSitesIssueTheirActions() throws {
        let token = TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        let credentials = HubFleetSetupCredentials(accessToken: "a", refreshToken: "r", clientID: "c",
                                                   region: "europe_middle_east_and_africa", expiresInSeconds: 3600)
        // (label, expected first action, installed, snapshot, trigger)
        struct Site {
            let label: String
            let expected: String
            var installed = true
            var snapshot: HubSnapshot?
            var failStart = false
            var historyMarker: (phase: String, historyOnly: Bool)?
            var readOnlyFolder = false
            let trigger: (HubController, @escaping () -> Void) -> Void
        }
        let sites: [Site] = [
            Site(label: "stopHub", expected: "stop") { controller, done in controller.stopHub { _ in done() } },
            Site(label: "signOutTeslaAccount", expected: "stop") { controller, done in
                controller.signOutTeslaAccount { _ in done() }
            },
            Site(label: "prepareOnlineMigration history-only", expected: "stop", installed: false) { controller, done in
                controller.importTeslaMateHistoryOnlyOnline(
                    source: "postgresql://reader@127.0.0.1:55438/teslamate_snapshot", carID: "1",
                    passwordFile: "/private/password", acknowledgeV42CompatibleSchema: true) { _ in done() }
            },
            Site(label: "prepareOnlineMigration installed", expected: "stop") { controller, done in
                controller.importTeslaMateOnline(
                    source: "postgresql://reader@localhost/teslamate", carID: "1",
                    passwordFile: "/private/password", encryptionKeyFile: "/private/key",
                    acknowledgeV42CompatibleSchema: true) { _ in done() }
            },
            Site(label: "acknowledgeMigrationHandoverAndStart failed history-only start", expected: "start",
                 failStart: true, historyMarker: ("awaiting_handover", true)) { controller, done in
                controller.acknowledgeMigrationHandoverAndStart { _ in done() }
            },
            Site(label: "completeMigrationHandoverStart cleanup failed", expected: "start",
                 historyMarker: ("awaiting_handover", true), readOnlyFolder: true) { controller, done in
                controller.acknowledgeMigrationHandoverAndStart { _ in done() }
            },
            Site(label: "rollbackUnreadyMigrationHandoverStart", expected: "start",
                 historyMarker: ("awaiting_handover", false)) { controller, done in
                controller.acknowledgeMigrationHandoverAndStart { _ in done() }
            },
            Site(label: "runOnboardingChecks history-only", expected: "stop",
                 historyMarker: ("awaiting_verification", true)) { controller, done in
                controller.runOnboardingChecks(expectRunning: false) { _ in done() }
            },
            Site(label: "runOnboardingChecks handover pending", expected: "stop",
                 historyMarker: ("awaiting_verification", false)) { controller, done in
                controller.runOnboardingChecks(expectRunning: false) { _ in done() }
            },
            Site(label: "configureFleetAccount not connected", expected: "pause") { controller, done in
                controller.configureFleetAccount(credentials: credentials) { _ in done() }
            },
            Site(label: "configureFleetAccount connected", expected: "pause", snapshot: .previewRunning) { controller, done in
                controller.configureFleetAccount(credentials: credentials) { _ in done() }
            },
            Site(label: "configureTeslaAccount not connected", expected: "pause") { controller, done in
                controller.configureTeslaAccount(tokens: token) { _ in done() }
            },
            Site(label: "configureTeslaAccount connected", expected: "pause", snapshot: .previewRunning) { controller, done in
                controller.configureTeslaAccount(tokens: token) { _ in done() }
            },
            Site(label: "runFullDiagnostics custody", expected: "pause") { controller, done in
                controller.runFullDiagnostics { _ in done() }
            }
        ]
        XCTAssertEqual(sites.count, 14, "thirteen existing sites plus the new sign-out stop")

        for site in sites {
            let home = try siteHome()
            defer {
                chmod(home.appendingPathComponent("Library/Application Support/Teslatlas Hub").path, 0o700)
                try? FileManager.default.removeItem(at: home)
            }
            if let marker = site.historyMarker {
                try writeMarker(in: home, phase: marker.phase, historyOnly: marker.historyOnly)
            }
            let events = EventRecorder()
            let service = ScriptedService(events: events, loadState: .loaded)
            if site.failStart { service.actionResults["start"] = .failure(HubActionError.commandFailed("start failed")) }
            let runner = SiteRunner()
            let controller = HubController(
                commandRunner: runner,
                serviceRunner: service,
                homeDirectory: home,
                serviceInstalledOverride: site.installed,
                initialSnapshot: site.snapshot,
                migrationStartupReadinessPollInterval: 1,
                migrationStartupReadinessMaxAttempts: 1,
                migrationStartupReadinessNow: { Date(timeIntervalSince1970: 100) },
                migrationStartupReadinessSchedule: { _, _ in }
            )
            if site.readOnlyFolder {
                chmod(home.appendingPathComponent("Library/Application Support/Teslatlas Hub").path, 0o500)
            }
            let finished = expectation(description: site.label)
            site.trigger(controller) { finished.fulfill() }
            wait(for: [finished], timeout: 5)
            let actions = events.values.filter { $0.hasPrefix("service:") }.map { String($0.dropFirst("service:".count)) }
            XCTAssertEqual(actions.first, site.expected, "\(site.label): \(actions)")
            switch site.expected {
            case "pause": XCTAssertFalse(actions.contains("stop"), "\(site.label) is transient: \(actions)")
            case "stop": XCTAssertFalse(actions.contains("pause"), "\(site.label) is persistent: \(actions)")
            default: XCTAssertEqual(actions, ["start", "stop"], "\(site.label) returns to the hold with a persistent stop")
            }
        }
    }

    func testSignOutStopsBeforeControlAndAbortsOnStopFailure() throws {
        let home = try siteHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let events = EventRecorder()
        let runner = EventingRunner(events: events)
        let service = ScriptedService(events: events)
        let controller = HubController(commandRunner: runner, serviceRunner: service, homeDirectory: home,
                                       serviceInstalledOverride: true, initialSnapshot: .previewRunning)
        let signedOut = expectation(description: "signed out")
        controller.signOutTeslaAccount { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            signedOut.fulfill()
        }
        wait(for: [signedOut], timeout: 2)
        XCTAssertEqual(Array(events.values.prefix(2)), ["service:stop", "control"], "the persistent stop comes first")

        let failureEvents = EventRecorder()
        let failing = ScriptedService(events: failureEvents)
        failing.result = .failure(HubActionError.commandFailed("stop failed"))
        let abortedController = HubController(commandRunner: EventingRunner(events: failureEvents), serviceRunner: failing,
                                              homeDirectory: home, serviceInstalledOverride: true,
                                              initialSnapshot: .previewRunning)
        let aborted = expectation(description: "sign-out aborted")
        abortedController.signOutTeslaAccount { result in
            guard case let .failure(error) = result else { return XCTFail("a failed stop must abort the sign-out") }
            XCTAssertTrue(error.localizedDescription.contains("stop failed"))
            aborted.fulfill()
        }
        wait(for: [aborted], timeout: 2)
        XCTAssertFalse(failureEvents.values.contains("control"), "control sign-out must not run after a failed stop")
    }

    private final class EventingRunner: HubCommandRunning {
        private let events: EventRecorder
        init(events: EventRecorder) { self.events = events }
        func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
            if arguments.contains("control") { events.append("control") }
            completion(.success("{}"))
        }
    }

    func testControlCommandTimeoutIs45Seconds() {
        XCTAssertEqual(EmbeddedHubCommandRunner.timeout(for: ["--config", "/c.toml", "control", "sign-out"]), 45)
        XCTAssertEqual(EmbeddedHubCommandRunner.timeout(for: ["--config", "/c.toml", "status"]), 30)
        XCTAssertEqual(EmbeddedHubCommandRunner.timeout(for: ["--config", "/c.toml", "preflight"]), 30)
        XCTAssertEqual(EmbeddedHubCommandRunner.timeout(for: ["--config", "/c.toml", "setup"]), 5 * 60)
    }

    func testServiceInstalledFollowsLaunchAgent() throws {
        let home = try siteHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let recorder = LifecycleEvents()
        let agent = FakeLaunchAgent(events: recorder)
        for installed in [false, true] {
            agent.installed = installed
            let events = EventRecorder()
            let controller = HubController(commandRunner: EventingRunner(events: events),
                                           serviceRunner: ScriptedService(events: events),
                                           launchAgent: agent, dataRemover: nil, homeDirectory: home,
                                           initialSnapshot: .previewRunning)
            let done = expectation(description: "sign-out \(installed)")
            controller.signOutTeslaAccount { _ in done.fulfill() }
            wait(for: [done], timeout: 2)
            XCTAssertEqual(events.values.contains("service:stop"), installed,
                           "a nil override follows the agent's isInstalled")
        }
    }

    func testReconcileIsNoOpWithoutPlistInPreviewDevelopmentAndUnderXCTest() throws {
        let home = try siteHome()
        defer { try? FileManager.default.removeItem(at: home) }
        struct Scenario {
            let label: String
            let environment: [String: String]
            let installed: Bool
        }
        let scenarios = [
            Scenario(label: "XCTest host", environment: ["XCTestConfigurationFilePath": "/x"], installed: true),
            Scenario(label: "preview", environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"], installed: true),
            Scenario(label: "no plist", environment: [:], installed: false)
        ]
        for scenario in scenarios {
            let recorder = LifecycleEvents()
            let agent = FakeLaunchAgent(events: recorder)
            agent.installed = scenario.installed
            agent.change = .replaced
            let events = EventRecorder()
            let service = ScriptedService(events: events, loadState: .loaded)
            let controller = HubController(environment: scenario.environment,
                                           commandRunner: CountingRunner(), serviceRunner: service,
                                           launchAgent: agent, dataRemover: nil, homeDirectory: home)
            let done = expectation(description: scenario.label)
            controller.reconcileLaunchAgent { done.fulfill() }
            wait(for: [done], timeout: 2)
            XCTAssertEqual(recorder.all, [], scenario.label)
            XCTAssertEqual(events.values, [], scenario.label)
            XCTAssertEqual(service.stateQueries, 0, scenario.label)
        }

        // Development: a source-run Hub never reconciles the production file.
        let recorder = LifecycleEvents()
        let agent = FakeLaunchAgent(events: recorder)
        agent.change = .replaced
        let events = EventRecorder()
        let controller = HubController(environment: [:], commandRunner: CountingRunner(),
                                       serviceRunner: ScriptedService(events: events, loadState: .loaded),
                                       launchAgent: agent, dataRemover: nil, homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       developmentConfiguration: try developmentConfiguration())
        let done = expectation(description: "development")
        controller.reconcileLaunchAgent { done.fulfill() }
        wait(for: [done], timeout: 2)
        XCTAssertEqual(recorder.all, [])
        XCTAssertEqual(events.values, [])
    }

    func testReconcileRestartsOnlyChangedLoadedJob() throws {
        let home = try siteHome()
        defer { try? FileManager.default.removeItem(at: home) }
        struct Case {
            let change: HubLaunchAgentChange
            let state: HubServiceLoadState
            let restarts: Bool
            let label: String
        }
        let cases = [
            Case(change: .unchanged, state: .loaded, restarts: false, label: "unchanged and loaded"),
            Case(change: .replaced, state: .unloaded, restarts: false, label: "changed but not loaded"),
            Case(change: .created, state: .unloaded, restarts: false, label: "created and not loaded"),
            Case(change: .replaced, state: .loaded, restarts: true, label: "changed and loaded")
        ]
        for scenario in cases {
            let recorder = LifecycleEvents()
            let agent = FakeLaunchAgent(events: recorder)
            agent.change = scenario.change
            let events = EventRecorder()
            let controller = HubController(environment: [:], commandRunner: CountingRunner(),
                                           serviceRunner: ScriptedService(events: events, loadState: scenario.state),
                                           launchAgent: agent, dataRemover: nil, homeDirectory: home)
            let done = expectation(description: scenario.label)
            controller.reconcileLaunchAgent { done.fulfill() }
            wait(for: [done], timeout: 2)
            XCTAssertEqual(events.values.contains("service:restart"), scenario.restarts, scenario.label)
            XCTAssertFalse(events.values.contains("service:start"), "reconcile never starts an unloaded job: \(scenario.label)")
            XCTAssertEqual(recorder.all, ["ensure"], "\(scenario.label): reconcile does not require the configuration")
        }

        // A writer error is a diagnostic, never a modal or a restart.
        let recorder = LifecycleEvents()
        let agent = FakeLaunchAgent(events: recorder)
        agent.ensureError = HubActionError.commandFailed("Move Teslatlas Hub to the Applications folder")
        let events = EventRecorder()
        let controller = HubController(environment: [:], commandRunner: CountingRunner(),
                                       serviceRunner: ScriptedService(events: events, loadState: .loaded),
                                       launchAgent: agent, dataRemover: nil, homeDirectory: home)
        let done = expectation(description: "writer failure")
        controller.reconcileLaunchAgent { done.fulfill() }
        wait(for: [done], timeout: 2)
        let published = expectation(description: "diagnostic published")
        DispatchQueue.main.async { published.fulfill() }
        wait(for: [published], timeout: 2)
        XCTAssertEqual(events.values, [])
        XCTAssertTrue(controller.diagnostics().contains { $0.contains("Applications folder") })
    }

    func testRemoveBackgroundServiceRefusedInPreviewAndDevelopment() throws {
        let events = EventRecorder()
        let preview = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                    commandRunner: CountingRunner(),
                                    serviceRunner: ScriptedService(events: events))
        let previewRefused = expectation(description: "preview refused")
        preview.removeBackgroundService { result in
            guard case let .failure(error) = result else { return XCTFail("preview removed") }
            XCTAssertEqual(error.localizedDescription, HubActionError.preview.localizedDescription)
            previewRefused.fulfill()
        }
        wait(for: [previewRefused], timeout: 1)
        XCTAssertEqual(events.values, [], "no service call")

        let rejection = HubActionError.commandFailed("A source-run development Hub cannot remove the production service.")
        let controller = HubController(environment: [:], commandRunner: CountingRunner(),
                                       serviceRunner: ScriptedService(events: events),
                                       serviceInstalledOverride: true,
                                       developmentConfiguration: try developmentConfiguration())
        let refused = expectation(description: "development refused")
        controller.removeBackgroundService { result in
            guard case let .failure(error) = result else { return XCTFail("development removed") }
            XCTAssertEqual(error.localizedDescription, rejection.localizedDescription)
            refused.fulfill()
        }
        wait(for: [refused], timeout: 1)
        XCTAssertEqual(events.values, [], "no service call")
    }

    private func developmentConfiguration() throws -> DevelopmentHubConfiguration {
        let root = HubTestScratch.validatedFixtureRoot("ControllerDev")
        let state = root.appendingPathComponent("state", isDirectory: true)
        let logs = root.appendingPathComponent("logs", isDirectory: true)
        try FileManager.default.createDirectory(at: state, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        for directory in [root, state, logs] {
            try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o700)], ofItemAtPath: directory.path)
        }
        addTeardownBlock { try? FileManager.default.removeItem(at: root) }
        let binary = root.appendingPathComponent("teslatlas-hub")
        try Data("#!/bin/sh\nexit 0\n".utf8).write(to: binary)
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o755)], ofItemAtPath: binary.path)
        let config = root.appendingPathComponent("config.toml")
        let environment = [
            DevelopmentHubConfiguration.enableVariable: "1",
            DevelopmentHubConfiguration.binaryVariable: binary.path,
            DevelopmentHubConfiguration.configVariable: config.path,
            DevelopmentHubConfiguration.stateVariable: state.path,
            DevelopmentHubConfiguration.logVariable: logs.path,
            DevelopmentHubConfiguration.controlVariable: root.path
        ]
        return try XCTUnwrap(DevelopmentHubConfiguration.from(environment: environment))
    }

    // MARK: Pause sites (RD1)

    private enum PauseSite: CaseIterable {
        case fleetNotConnected, fleetConnected, teslaNotConnected, teslaConnected, custody
        var connected: Bool { self == .fleetConnected || self == .teslaConnected }
    }

    private func runPauseSite(_ site: PauseSite, home: URL, service: ScriptedService,
                              runner: HubCommandRunning) -> Result<Void, Error>? {
        let controller = HubController(commandRunner: runner, serviceRunner: service, homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: site.connected ? .previewRunning : nil)
        var outcome: Result<Void, Error>?
        let done = expectation(description: "site \(site)")
        switch site {
        case .fleetNotConnected, .fleetConnected:
            controller.configureFleetAccount(credentials: HubFleetSetupCredentials(
                accessToken: "a", refreshToken: "r", clientID: "c",
                region: "europe_middle_east_and_africa", expiresInSeconds: 3600)) { outcome = $0; done.fulfill() }
        case .teslaNotConnected, .teslaConnected:
            controller.configureTeslaAccount(tokens: TeslaAuthTokens(accessToken: "a", refreshToken: "r")) {
                outcome = $0; done.fulfill()
            }
        case .custody:
            controller.runFullDiagnostics { report in
                outcome = report.contains("(failed)") ? .failure(HubActionError.commandFailed(report)) : .success(())
                done.fulfill()
            }
        }
        wait(for: [done], timeout: 5)
        return outcome
    }

    func testPauseSitesRecoverAfterFailedPauseOrWork() throws {
        enum Failure: CaseIterable { case pauseFails, pauseFailsStaysLoaded, workFails }
        for site in PauseSite.allCases {
            for failure in Failure.allCases {
                let label = "\(site) \(failure)"
                let home = try siteHome()
                defer { try? FileManager.default.removeItem(at: home) }
                let events = EventRecorder()
                let service = ScriptedService(events: events, loadState: .loaded)
                var runner: HubCommandRunning = SiteRunner()
                switch failure {
                case .pauseFails:
                    service.actionResults["pause"] = .failure(HubActionError.commandFailed("pause failed (query)"))
                case .pauseFailsStaysLoaded:
                    service.actionResults["pause"] = .failure(HubActionError.commandFailed("bootout failed"))
                case .workFails:
                    runner = ScriptedRunner(events: EventRecorder(), result: .failure(HubActionError.commandFailed("setup failed")))
                }
                _ = runPauseSite(site, home: home, service: service, runner: runner)
                let actions = events.values.filter { $0.hasPrefix("service:") }
                if site == .custody, failure == .workFails {
                    // Disposition: custody diagnostics cannot fail as work (a failing check
                    // becomes a "(failed)" report section), so the held record is covered by
                    // the ordinary Start and no typed recovery is owed.
                    XCTAssertEqual(actions, ["service:pause", "service:start"], label)
                    continue
                }
                XCTAssertEqual(actions.filter { $0 == "service:recover" }.count, 1, "\(label): exactly one typed recovery \(actions)")
                XCTAssertFalse(actions.contains("service:start"), "\(label): no ordinary start after a failed pause or failed work")
                XCTAssertEqual(actions.first, "service:pause", label)
                XCTAssertEqual(actions.last, "service:recover", label)
            }
        }
    }

    func testPauseSiteSuccessUsesOrdinaryStartWithHeldRecord() throws {
        // A successful pause and successful work use the ordinary Start (one recover-free path).
        for site in PauseSite.allCases {
            let home = try siteHome()
            defer { try? FileManager.default.removeItem(at: home) }
            let events = EventRecorder()
            let service = ScriptedService(events: events, loadState: .loaded)
            _ = runPauseSite(site, home: home, service: service, runner: SiteRunner())
            let actions = events.values.filter { $0.hasPrefix("service:") }
            XCTAssertEqual(actions.first, "service:pause", "\(site)")
            XCTAssertTrue(actions.contains("service:start"), "\(site): the ordinary Start follows \(actions)")
            XCTAssertFalse(actions.contains("service:recover"), "\(site): no recovery after success")
        }

        // With the real controller: a failed ordinary Start settles its own recovery, and the
        // site's typed recovery then finds a settled record and issues nothing more.
        let home = try siteHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let rig = LifecycleRig(loaded: true)
        rig.launchctl.failNext("bootstrap", with: HubActionError.commandFailed("bootstrap failed"))
        let controller = HubController(commandRunner: SiteRunner(), serviceRunner: rig.controller, homeDirectory: home,
                                       serviceInstalledOverride: true, initialSnapshot: .previewRunning)
        let done = expectation(description: "failed ordinary start")
        controller.configureTeslaAccount(tokens: TeslaAuthTokens(accessToken: "a", refreshToken: "r")) { result in
            guard case let .failure(error) = result else { XCTFail("expected the start failure"); done.fulfill(); return }
            XCTAssertTrue(error.localizedDescription.contains("bootstrap failed"))
            done.fulfill()
        }
        wait(for: [done], timeout: 5)
        let commands = rig.launchctlEvents
        XCTAssertEqual(commands.filter { $0 == "bootout" }.count, 1, "no second bootout")
        XCTAssertEqual(commands.filter { $0 == "bootstrap" }.count, 2, "the ordinary attempt plus exactly one recovery")
        XCTAssertFalse(rig.controller.restartDutyOutstanding)
        let after = rig.launchctlEvents.count
        rig.recover()
        XCTAssertEqual(rig.launchctlEvents.count, after, "a later typed recovery finds no record and issues nothing")
    }

    func testPauseSiteWithFailedStartAndFailedRecoveryReportsByRefreshedStatus() throws {
        // The ordinary Start fails and its own recovery fails too: the service reports an
        // unconfirmed restart. The site refreshes and shows the stopped text only when
        // status shows the Hub stopped.
        let sites: [PauseSite] = [.fleetNotConnected, .fleetConnected, .teslaNotConnected, .teslaConnected]
        for site in sites {
            for running in [true, false] {
                let label = "\(site) running=\(running)"
                let home = try siteHome()
                defer { try? FileManager.default.removeItem(at: home) }
                let events = EventRecorder()
                let service = ScriptedService(events: events, loadState: running ? .loaded : .unloaded)
                service.actionResults["start"] = .failure(HubServiceRestartReport(
                    original: HubActionError.commandFailed("bootstrap failed"), outcome: .unconfirmed))
                let outcome = runPauseSite(site, home: home, service: service, runner: SiteRunner())
                guard case let .failure(error)? = outcome else { XCTFail("\(label): expected a failure"); continue }
                let text = error.localizedDescription
                XCTAssertTrue(text.contains("bootstrap failed"), "\(label): \(text)")
                XCTAssertTrue(text.contains("The restart could not be confirmed."), "\(label): \(text)")
                XCTAssertEqual(text.contains("The Hub did not restart. Choose Start Hub."), !running,
                               "\(label): the stopped text appears only when status shows the Hub stopped: \(text)")
            }
        }
    }

    private func custodyReport(service: ScriptedService, home: URL) -> String {
        let controller = HubController(commandRunner: SiteRunner(), serviceRunner: service, homeDirectory: home,
                                       serviceInstalledOverride: true)
        var report = ""
        let done = expectation(description: "custody diagnostics")
        controller.runFullDiagnostics { report = $0; done.fulfill() }
        wait(for: [done], timeout: 5)
        return report
    }

    func testCustodyFailedStartAndFailedPauseReportByRefreshedStatus() throws {
        let didNotRestart = "The Hub did not restart. Choose Start Hub."
        for running in [true, false] {
            // Failed ordinary Start whose own recovery could not be confirmed.
            do {
                let label = "failed Start running=\(running)"
                let home = try siteHome()
                defer { try? FileManager.default.removeItem(at: home) }
                let service = ScriptedService(events: EventRecorder())
                service.loadStateSequence = [.loaded, running ? .loaded : .unloaded]
                service.actionResults["start"] = .failure(HubServiceRestartReport(
                    original: HubActionError.commandFailed("bootstrap failed"), outcome: .unconfirmed))
                let report = custodyReport(service: service, home: home)
                XCTAssertTrue(report.contains("== service resume (failed) =="), "\(label): \(report)")
                XCTAssertTrue(report.contains("bootstrap failed"), label)
                XCTAssertEqual(report.components(separatedBy: "The restart could not be confirmed.").count - 1, 1, "\(label): \(report)")
                XCTAssertEqual(report.contains(didNotRestart), !running,
                               "\(label): the stopped text appears only when status shows the Hub stopped: \(report)")
            }
            // Failed ordinary Start and a typed recovery that fails as well.
            do {
                let label = "failed Start and failed recovery running=\(running)"
                let home = try siteHome()
                defer { try? FileManager.default.removeItem(at: home) }
                let service = ScriptedService(events: EventRecorder())
                service.loadStateSequence = [.loaded, running ? .loaded : .unloaded]
                service.actionResults["start"] = .failure(HubServiceRestartReport(
                    original: HubActionError.commandFailed("bootstrap failed"), outcome: .unconfirmed))
                service.recoverResult = .failure(HubServiceRestartReport(
                    original: HubActionError.commandFailed(""), outcome: .unconfirmed))
                let report = custodyReport(service: service, home: home)
                XCTAssertTrue(report.contains("bootstrap failed"), label)
                XCTAssertEqual(report.components(separatedBy: "The restart could not be confirmed.").count - 1, 1, "\(label): \(report)")
                XCTAssertEqual(report.contains(didNotRestart), !running, "\(label): \(report)")
            }
            // Failed Pause whose recovery could not be confirmed.
            do {
                let label = "failed Pause running=\(running)"
                let home = try siteHome()
                defer { try? FileManager.default.removeItem(at: home) }
                let service = ScriptedService(events: EventRecorder())
                service.loadStateSequence = [.loaded, running ? .loaded : .unloaded]
                service.actionResults["pause"] = .failure(HubActionError.commandFailed("bootout failed"))
                service.recoverResult = .failure(HubServiceRestartReport(
                    original: HubActionError.commandFailed(""), outcome: .unconfirmed))
                let report = custodyReport(service: service, home: home)
                XCTAssertTrue(report.contains("== service pause (failed) =="), "\(label): \(report)")
                XCTAssertTrue(report.contains("== service recovery (failed) =="), "\(label): \(report)")
                XCTAssertTrue(report.contains("The restart could not be confirmed."), "\(label): \(report)")
                XCTAssertEqual(report.contains(didNotRestart), !running,
                               "\(label): the stopped text appears only when status shows the Hub stopped: \(report)")
            }
        }
        // A restart that succeeded is reported as such and never as "did not restart".
        let home = try siteHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let service = ScriptedService(events: EventRecorder())
        service.loadStateSequence = [.loaded, .unloaded]
        service.actionResults["start"] = .failure(HubServiceRestartReport(
            original: HubActionError.commandFailed("bootstrap failed"), outcome: .restarted(appliesNewDefinition: false)))
        let report = custodyReport(service: service, home: home)
        XCTAssertTrue(report.contains("The Hub was started again."), report)
        XCTAssertFalse(report.contains(didNotRestart), report)
    }

    func testPauseSitesMakeNoLoadedStateQueryAfterPause() throws {
        for site in PauseSite.allCases {
            let home = try siteHome()
            defer { try? FileManager.default.removeItem(at: home) }
            let events = EventRecorder()
            let service = ScriptedService(events: events, loadState: .loaded)
            service.recordsStateQueries = true
            _ = runPauseSite(site, home: home, service: service, runner: SiteRunner())
            let values = events.values
            let pause = try XCTUnwrap(values.firstIndex(of: "service:pause"), "\(site) \(values)")
            let start = try XCTUnwrap(values.firstIndex(of: "service:start"), "\(site) \(values)")
            XCTAssertFalse(values[pause..<start].contains("state"),
                           "\(site): no loaded-state query between the pause and the start \(values)")
            // Custody asks once, before the pause, only to decide whether to pause.
            XCTAssertEqual(values[..<pause].filter { $0 == "state" }.count, site == .custody ? 1 : 0, "\(site)")
        }
    }

    func testCustodyRefusedHistoryOnlyPolicyReleasesRecord() throws {
        let home = try siteHome()
        defer { try? FileManager.default.removeItem(at: home) }
        try writeMarker(in: home, phase: "awaiting_verification", historyOnly: true)
        let events = EventRecorder()
        let service = ScriptedService(events: events, loadState: .loaded)
        let runner = SiteRunner()
        runner.historyPreflightFails = true
        _ = runPauseSite(.custody, home: home, service: service, runner: runner)
        let actions = events.values.filter { $0.hasPrefix("service:") }
        XCTAssertEqual(actions, ["service:pause", "service:release"], "the refused policy releases the record and never starts")
    }

    func testFirstRunSetupStartsServiceThroughWriter() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let events = EventRecorder()
        let controller = HubController(commandRunner: ScriptedRunner(events: events, result: .success("configured")),
                                       serviceRunner: ScriptedService(events: events),
                                       homeDirectory: home, serviceInstalledOverride: false)
        let done = expectation(description: "first run")
        controller.configureTeslaAccount(tokens: TeslaAuthTokens(accessToken: "a", refreshToken: "r")) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            done.fulfill()
        }
        wait(for: [done], timeout: 3)
        XCTAssertEqual(Array(events.values.prefix(2)), ["setup", "service:start"], "setup, then the explicit first Start")
        XCTAssertFalse(events.values.contains("service:pause"))
    }

    // MARK: Delete Hub and Data (controller level)

    func testDeleteHubAndDataRefusedInPreviewAndDevelopment() throws {
        let events = EventRecorder()
        let service = ScriptedService(events: events)
        let preview = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"], commandRunner: CountingRunner(),
                                    serviceRunner: service)
        let previewRefused = expectation(description: "preview refused")
        preview.deleteHubAndData { result in
            guard case let .failure(error) = result else { return XCTFail("preview deleted") }
            XCTAssertEqual(error.localizedDescription, HubActionError.preview.localizedDescription)
            previewRefused.fulfill()
        }
        let development = HubController(environment: [:], commandRunner: CountingRunner(), serviceRunner: service,
                                        serviceInstalledOverride: true,
                                        developmentConfiguration: try developmentConfiguration())
        let developmentRefused = expectation(description: "development refused")
        development.deleteHubAndData { result in
            guard case let .failure(error) = result else { return XCTFail("development deleted") }
            XCTAssertTrue(error.localizedDescription.contains("cannot delete the production service or its data"))
            developmentRefused.fulfill()
        }
        wait(for: [previewRefused, developmentRefused], timeout: 2)
        XCTAssertEqual(service.deleteCalls, 0)
        XCTAssertFalse(preview.hubDeletionStarted)
        XCTAssertFalse(development.hubDeletionStarted)
        XCTAssertFalse(HubStatusProcessLifecycle.shared.isPaused)
    }

    // MARK: Delete and diagnostics custody exclude each other (R-E1F5-1)

    private func waitUntil(_ description: String, timeout: TimeInterval = 5, _ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.02)) }
        XCTAssertTrue(condition(), description)
    }

    func testDeleteRequestedDuringHeldDiagnosticsCustodyIsRefusedAndDeletesNothing() throws {
        defer { HubStatusProcessLifecycle.shared.resume() }
        let home = try siteHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let service = ScriptedService(events: EventRecorder(), loadState: .loaded)
        service.holdPause = true
        let controller = HubController(commandRunner: SiteRunner(), serviceRunner: service, homeDirectory: home,
                                       serviceInstalledOverride: true)
        let diagnosticsDone = expectation(description: "custody ends")
        controller.runFullDiagnostics { _ in diagnosticsDone.fulfill() }
        defer { service.releasePause() }
        XCTAssertNotNil(service.heldPause, "the custody is held at its pause")
        XCTAssertTrue(controller.diagnosticsPreventQuit)

        let refused = expectation(description: "Delete refused")
        controller.deleteHubAndData { result in
            guard case let .failure(error) = result else { XCTFail("Delete ran inside the custody window"); refused.fulfill(); return }
            XCTAssertTrue(error.localizedDescription.contains("diagnostics"), error.localizedDescription)
            XCTAssertTrue(error.localizedDescription.contains("Nothing was changed"), error.localizedDescription)
            refused.fulfill()
        }
        wait(for: [refused], timeout: 5)
        XCTAssertEqual(service.deleteCalls, 0, "no stop, no plist removal, no data step")
        XCTAssertFalse(controller.hubDeletionStarted)
        XCTAssertFalse(HubStatusProcessLifecycle.shared.isPaused, "status reads were never paused")

        // The custody finishes normally and Delete is possible again afterwards.
        service.releasePause()
        wait(for: [diagnosticsDone], timeout: 5)
        XCTAssertFalse(controller.diagnosticsPreventQuit)
        service.deleteResult = .success(HubDataRemovalReport(deleted: ["D1"], kept: [], warnings: []))
        let deleted = expectation(description: "Delete after the custody")
        controller.deleteHubAndData { result in
            if case .failure = result { XCTFail("Delete is refused after the custody ended") }
            deleted.fulfill()
        }
        wait(for: [deleted], timeout: 5)
        XCTAssertEqual(service.deleteCalls, 1)
    }

    func testDiagnosticsCustodyRequestedDuringDeleteIsRefusedAndRunsNothing() throws {
        defer { HubStatusProcessLifecycle.shared.resume() }
        let home = try siteHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let events = EventRecorder()
        let service = ScriptedService(events: events, loadState: .loaded)
        service.holdDelete = true
        let controller = HubController(commandRunner: SiteRunner(), serviceRunner: service, homeDirectory: home,
                                       serviceInstalledOverride: true)
        let deleteDone = expectation(description: "Delete refused later")
        controller.deleteHubAndData { _ in deleteDone.fulfill() }
        waitUntil("Delete reached the service") { service.heldDelete != nil }
        defer { service.releaseDelete(.failure(HubActionError.commandFailed("released"))) }

        var report: String?
        controller.runFullDiagnostics { report = $0 }
        let text = try XCTUnwrap(report, "the refused custody completes at once")
        XCTAssertTrue(text.contains("Delete Hub and Data"), text)
        XCTAssertFalse(controller.diagnosticsPreventQuit, "no custody was taken")
        XCTAssertEqual(service.stateQueries, 0, "no service query")
        XCTAssertFalse(events.values.contains("service:pause"), "no pause")

        // A refused Delete hands the lifecycle back: diagnostics run again.
        service.releaseDelete(.failure(HubActionError.commandFailed("Nothing was changed.")))
        wait(for: [deleteDone], timeout: 5)
        let ran = expectation(description: "diagnostics after the refusal")
        controller.runFullDiagnostics { text in
            XCTAssertFalse(text.contains("Delete Hub and Data"), text)
            ran.fulfill()
        }
        wait(for: [ran], timeout: 5)
        XCTAssertGreaterThan(service.stateQueries, 0, "the custody ran")
    }

    func testSecondDeleteWhileOneIsInFlightIsRefused() throws {
        defer { HubStatusProcessLifecycle.shared.resume() }
        let service = ScriptedService(events: EventRecorder())
        service.holdDelete = true
        let controller = HubController(commandRunner: SiteRunner(), serviceRunner: service,
                                       homeDirectory: try temporaryHome(), serviceInstalledOverride: true)
        let first = expectation(description: "first Delete ends")
        controller.deleteHubAndData { _ in first.fulfill() }
        waitUntil("Delete reached the service") { service.heldDelete != nil }
        let second = expectation(description: "second Delete refused")
        controller.deleteHubAndData { result in
            guard case let .failure(error) = result else { XCTFail("a second Delete ran"); second.fulfill(); return }
            XCTAssertTrue(error.localizedDescription.contains("already started"), error.localizedDescription)
            second.fulfill()
        }
        wait(for: [second], timeout: 5)
        XCTAssertEqual(service.deleteCalls, 1)
        service.releaseDelete(.failure(HubActionError.commandFailed("Nothing was changed.")))
        wait(for: [first], timeout: 5)
    }

    func testDiagnosticsCustodyStaysRefusedAfterDeleteCompleted() throws {
        defer { HubStatusProcessLifecycle.shared.resume() }
        let home = try siteHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let service = ScriptedService(events: EventRecorder(), loadState: .loaded)
        service.deleteResult = .success(HubDataRemovalReport(deleted: ["D1"], kept: [], warnings: []))
        let controller = HubController(commandRunner: SiteRunner(), serviceRunner: service, homeDirectory: home,
                                       serviceInstalledOverride: true)
        let deleted = expectation(description: "Delete completed")
        controller.deleteHubAndData { _ in deleted.fulfill() }
        wait(for: [deleted], timeout: 5)
        var report: String?
        controller.runFullDiagnostics { report = $0 }
        XCTAssertTrue(try XCTUnwrap(report).contains("Delete Hub and Data"))
        XCTAssertEqual(service.stateQueries, 0)
        XCTAssertFalse(controller.diagnosticsPreventQuit)
    }

    func testDeleteResumesStatusReadsOnRefusal() throws {
        defer { HubStatusProcessLifecycle.shared.resume() }
        let service = ScriptedService(events: EventRecorder())
        service.deleteResult = .failure(HubActionError.commandFailed("Nothing was changed, because it is a link."))
        let controller = HubController(commandRunner: CountingRunner(), serviceRunner: service,
                                       launchAgent: nil, dataRemover: nil,
                                       homeDirectory: try temporaryHome(), serviceInstalledOverride: true)
        let refused = expectation(description: "pre-data refusal")
        controller.deleteHubAndData { result in
            guard case .failure = result else { return XCTFail("expected the refusal") }
            refused.fulfill()
        }
        wait(for: [refused], timeout: 5)
        XCTAssertEqual(service.deleteCalls, 1)
        XCTAssertFalse(HubStatusProcessLifecycle.shared.isPaused, "status reads resume")
        XCTAssertFalse(controller.hubDeletionStarted, "the started flag clears")
        XCTAssertFalse(controller.hubDeletionCompleted)
    }

    func testDeleteRefusedWhenStatusReadsDoNotStopResumesReads() throws {
        let lifecycle = HubStatusProcessLifecycle()
        let stubborn = Process()
        stubborn.executableURL = URL(fileURLWithPath: "/bin/sh")
        stubborn.arguments = ["-c", "trap '' TERM; exec /bin/sleep 30"]
        stubborn.terminationHandler = { lifecycle.finished($0) }
        try lifecycle.launch(stubborn)
        defer {
            Darwin.kill(stubborn.processIdentifier, SIGKILL)
            stubborn.waitUntilExit()
        }
        Thread.sleep(forTimeInterval: 0.4) // let the shell install its trap

        let service = ScriptedService(events: EventRecorder())
        let controller = HubController(commandRunner: CountingRunner(), serviceRunner: service,
                                       launchAgent: nil, dataRemover: nil,
                                       homeDirectory: try temporaryHome(), serviceInstalledOverride: true,
                                       statusLifecycle: lifecycle, statusPauseBound: 0.2)
        let refused = expectation(description: "reads busy")
        controller.deleteHubAndData { result in
            guard case let .failure(error) = result else { XCTFail("expected the refusal"); refused.fulfill(); return }
            XCTAssertTrue(error.localizedDescription.contains("still reading the Hub's status"))
            XCTAssertTrue(error.localizedDescription.contains("Nothing was changed"))
            refused.fulfill()
        }
        wait(for: [refused], timeout: 5)
        XCTAssertEqual(service.deleteCalls, 0, "no service delete call")
        XCTAssertFalse(lifecycle.isPaused, "status reads resume")
        XCTAssertFalse(controller.hubDeletionStarted)
        XCTAssertFalse(controller.hubDeletionCompleted)
        // The refusal hands the lifecycle back: a diagnostics custody is allowed again.
        let custody = expectation(description: "custody allowed again")
        controller.runFullDiagnostics { report in
            XCTAssertFalse(report.contains("Delete Hub and Data"), "the Delete claim was released")
            custody.fulfill()
        }
        wait(for: [custody], timeout: 5)
        XCTAssertFalse(controller.diagnosticsPreventQuit)
    }

    func testDeleteMarksCompletedAfterDataStep() throws {
        defer { HubStatusProcessLifecycle.shared.resume() }
        let service = ScriptedService(events: EventRecorder())
        service.deleteResult = .success(HubDataRemovalReport(deleted: ["D1"], kept: [], warnings: []))
        let controller = HubController(commandRunner: CountingRunner(), serviceRunner: service,
                                       launchAgent: nil, dataRemover: nil,
                                       homeDirectory: try temporaryHome(), serviceInstalledOverride: true)
        XCTAssertFalse(controller.hubDeletionCompleted)
        let finished = expectation(description: "deleted")
        controller.deleteHubAndData { result in
            guard case let .success(report) = result else { return XCTFail("expected the report") }
            XCTAssertEqual(report.deleted, ["D1"])
            XCTAssertTrue(controller.hubDeletionStarted)
            XCTAssertTrue(controller.hubDeletionCompleted, "completion is set before the report is handed over")
            finished.fulfill()
        }
        wait(for: [finished], timeout: 5)
        XCTAssertTrue(HubStatusProcessLifecycle.shared.isPaused, "after the data step reads stay paused")
    }

    func testStatusPauseTerminatesRunningReadsAndRefusesNew() throws {
        let lifecycle = HubStatusProcessLifecycle()
        let running = Process()
        running.executableURL = URL(fileURLWithPath: "/bin/sleep")
        running.arguments = ["30"]
        running.terminationHandler = { lifecycle.finished($0) }
        try lifecycle.launch(running)
        XCTAssertEqual(lifecycle.activeCount, 1)

        XCTAssertTrue(lifecycle.pause(waitingUpTo: 5), "true only once the read has exited")
        XCTAssertFalse(running.isRunning)
        XCTAssertEqual(lifecycle.activeCount, 0)
        XCTAssertTrue(lifecycle.isPaused)
        let refused = Process()
        refused.executableURL = URL(fileURLWithPath: "/usr/bin/true")
        XCTAssertThrowsError(try lifecycle.launch(refused), "a paused lifecycle refuses new reads")

        lifecycle.resume()
        XCTAssertFalse(lifecycle.isPaused)
        let again = Process()
        again.executableURL = URL(fileURLWithPath: "/usr/bin/true")
        again.terminationHandler = { lifecycle.finished($0) }
        XCTAssertNoThrow(try lifecycle.launch(again))
        again.waitUntilExit()

        // A read that outlives the bound makes the pause fail.
        let stubborn = Process()
        stubborn.executableURL = URL(fileURLWithPath: "/bin/sh")
        stubborn.arguments = ["-c", "trap '' TERM; exec /bin/sleep 30"]
        stubborn.terminationHandler = { lifecycle.finished($0) }
        try lifecycle.launch(stubborn)
        Thread.sleep(forTimeInterval: 0.4) // let the shell install its trap
        XCTAssertFalse(lifecycle.pause(waitingUpTo: 0.3))
        lifecycle.resume()
        Darwin.kill(stubborn.processIdentifier, SIGKILL)
        stubborn.waitUntilExit()
    }

    func testDeletionStopsWindowRestoration() {
        defer { HubWindowRestoration.endSuppressing() }
        let existing = HubUtilityWindowStyle.makeWindow(title: "Existing", size: NSSize(width: 300, height: 200),
                                                        minimum: NSSize(width: 200, height: 100))
        existing.setFrameAutosaveName("Hub.Utility.Existing")
        XCTAssertEqual(existing.frameAutosaveName, "Hub.Utility.Existing")
        XCTAssertTrue(existing.isRestorable)
        defer { existing.close() }

        HubWindowRestoration.beginSuppressing(windows: { [existing] })
        XCTAssertFalse(existing.isRestorable, "current windows become non-restorable")
        XCTAssertEqual(existing.frameAutosaveName, "", "utility autosave names are cleared")

        let created = HubUtilityWindowStyle.makeWindow(title: "Created", size: NSSize(width: 300, height: 200),
                                                       minimum: NSSize(width: 200, height: 100))
        defer { created.close() }
        XCTAssertFalse(created.isRestorable, "subsequently created windows are non-restorable")
        XCTAssertEqual(created.frameAutosaveName, "")
        let sheet = HubOnboardingSheetStyle.makeWindow(contentSize: NSSize(width: 300, height: 200), dismissible: true)
        defer { sheet.close() }
        XCTAssertFalse(sheet.isRestorable)

        HubWindowRestoration.endSuppressing()
        let normal = HubOnboardingSheetStyle.makeWindow(contentSize: NSSize(width: 300, height: 200), dismissible: true)
        defer { normal.close() }
        XCTAssertTrue(normal.isRestorable, "suppression ends when a deletion is refused")
    }

    func testDeleteQuitRefusedWhilePendingAndAllowedOnceCompleted() throws {
        defer {
            HubStatusProcessLifecycle.shared.resume()
            HubWindowRestoration.endSuppressing()
        }
        let service = PendingDeleteService()
        let controller = HubController(environment: [:], commandRunner: CountingRunner(),
                                       serviceRunner: service, launchAgent: nil, dataRemover: nil,
                                       homeDirectory: try temporaryHome(), serviceInstalledOverride: true)
        let settled = expectation(description: "initial refresh")
        let main = MainWindowController(
            controller: controller, serviceTransitionTimeout: 5, serviceTransitionPollInterval: 0.05,
            errorPresenter: { _ in },
            confirmationPresenter: { alert, _ in alert.buttons.count == 1 ? .alertFirstButtonReturn : .alertSecondButtonReturn },
            revealApp: { _ in }, terminateApp: {}, olderInstallerPresent: { false },
            onInitialRefresh: { _ in settled.fulfill() }
        )
        wait(for: [settled], timeout: 3)
        main.showEmbeddedServiceDetails()
        let delete = try XCTUnwrap(buttons(in: main.window?.contentView).first {
            $0.identifier?.rawValue == "hub.service.delete-data"
        })
        delete.performClick(nil)
        let deadline = Date().addingTimeInterval(5)
        while service.deleteCompletion == nil, Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.02)) }
        XCTAssertNotNil(service.deleteCompletion, "the deleting transition reached the service")
        XCTAssertTrue(HubWindowRestoration.isSuppressing, "Delete makes windows non-restorable when it begins")

        // Pending: Quit is refused through the shared guard and the delegate.
        XCTAssertTrue(main.operationPreventsQuit)
        XCTAssertFalse(main.navigationAvailable, "navigation stays unavailable while deleting")
        let window = try XCTUnwrap(main.window)
        XCTAssertFalse(AppDelegate.finishSheetsBeforeQuit(in: [window], lifecycle: controller.lifecycleCapabilities))
        XCTAssertEqual(AppDelegate.terminationReply(windows: [window], lifecycle: controller.lifecycleCapabilities, restartDutyOutstanding: false), .terminateCancel)
        let delegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { _ in })
        let hosted = NSApplication.shared.windows.contains(window)
        if hosted { XCTAssertEqual(delegate.applicationShouldTerminate(NSApplication.shared), .terminateCancel) }

        // Completed: the retained transition no longer blocks Quit, and navigation stays off.
        service.deleteCompletion?(.success(HubDataRemovalReport(deleted: ["D1"], kept: [], warnings: [])))
        let completedDeadline = Date().addingTimeInterval(5)
        while !controller.hubDeletionCompleted, Date() < completedDeadline { RunLoop.current.run(until: Date().addingTimeInterval(0.02)) }
        XCTAssertTrue(controller.hubDeletionCompleted)
        XCTAssertFalse(main.operationPreventsQuit)
        XCTAssertFalse(main.navigationAvailable, "the deleting transition is retained")
        XCTAssertTrue(AppDelegate.finishSheetsBeforeQuit(in: [window], lifecycle: controller.lifecycleCapabilities))
        XCTAssertEqual(AppDelegate.terminationReply(windows: [window], lifecycle: controller.lifecycleCapabilities, restartDutyOutstanding: false), .terminateNow)
        if hosted { XCTAssertEqual(delegate.applicationShouldTerminate(NSApplication.shared), .terminateNow) }

        // A window with no transition of its own does not block Quit either.
        let other = MainWindowController(
            controller: controller, serviceTransitionTimeout: 5, serviceTransitionPollInterval: 0.05,
            errorPresenter: { _ in }, onInitialRefresh: nil)
        other.showEmbeddedServiceDetails()
        XCTAssertFalse(other.operationPreventsQuit)
    }

    func testStoppingStillBlocksQuitEvenOnceDeletionCompleted() throws {
        // The completion flag exempts the retained .deleting transition only.
        defer {
            HubStatusProcessLifecycle.shared.resume()
            HubWindowRestoration.endSuppressing()
        }
        let service = PendingDeleteService()
        service.loaded = .loaded
        service.holdStop = true
        let controller = HubController(environment: [:], commandRunner: CountingRunner(),
                                       serviceRunner: service, launchAgent: nil, dataRemover: nil,
                                       homeDirectory: try temporaryHome(), serviceInstalledOverride: true)
        let settled = expectation(description: "initial refresh")
        let main = MainWindowController(
            controller: controller, serviceTransitionTimeout: 5, serviceTransitionPollInterval: 0.05,
            errorPresenter: { _ in },
            confirmationPresenter: { _, silent in silent },
            revealApp: { _ in }, terminateApp: {}, olderInstallerPresent: { false },
            onInitialRefresh: { _ in settled.fulfill() }
        )
        wait(for: [settled], timeout: 3)

        // The data step has run for this controller.
        controller.deleteHubAndData { _ in }
        let deleteDeadline = Date().addingTimeInterval(5)
        while service.deleteCompletion == nil, Date() < deleteDeadline { RunLoop.current.run(until: Date().addingTimeInterval(0.02)) }
        service.deleteCompletion?(.success(HubDataRemovalReport(deleted: ["D1"], kept: [], warnings: [])))
        let completedDeadline = Date().addingTimeInterval(5)
        while !controller.hubDeletionCompleted, Date() < completedDeadline { RunLoop.current.run(until: Date().addingTimeInterval(0.02)) }
        XCTAssertTrue(controller.hubDeletionCompleted)
        XCTAssertFalse(main.operationPreventsQuit, "no transition: Quit is free")

        // After a completed Delete the controller admits no mutation, so a Stop cannot begin
        // from the window. The one transition a window can still hold is the onboarding
        // completion's `.starting`: it must still cancel Quit with the completion flag set.
        main.showEmbeddedServiceDetails()
        let stop = try XCTUnwrap(buttons(in: main.window?.contentView).first {
            $0.identifier?.rawValue == "hub.service.start-stop"
        })
        XCTAssertFalse(stop.isEnabled, "no service action is offered once the lifecycle is deleted")
        stop.performClick(nil)
        XCTAssertNil(main.window?.attachedSheet, "no confirmation is offered")
        XCTAssertTrue(service.stopCalls == 0, "the service was not asked to stop")
        main.settleStartedHubFromOnboarding()
        XCTAssertTrue(controller.hubDeletionCompleted)
        XCTAssertTrue(main.operationPreventsQuit, ".starting still cancels Quit with the completion flag set")
        let window = try XCTUnwrap(main.window)
        XCTAssertFalse(AppDelegate.finishSheetsBeforeQuit(in: [window], lifecycle: controller.lifecycleCapabilities))
        XCTAssertEqual(AppDelegate.terminationReply(windows: [window], lifecycle: controller.lifecycleCapabilities, restartDutyOutstanding: false), .terminateCancel)
    }

    private final class PendingDeleteService: HubServiceControlling {
        var deleteCompletion: ((Result<HubDataRemovalReport, Error>) -> Void)?
        var loaded: HubServiceLoadState = .unloaded
        var holdStop = false
        private(set) var stopCalls = 0
        var heldStop: ((Result<String, Error>) -> Void)?
        var restartDutyOutstanding: Bool { false }
        func whenRestartDutySettled(_ callback: @escaping () -> Void) { callback() }
        func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
            if arguments.last == "stop" { stopCalls += 1 }
            if holdStop, arguments.last == "stop" { heldStop = completion; return }
            completion(.success(""))
        }
        func run(arguments: [String], owner: HubLifecycleToken,
                 completion: @escaping (Result<String, Error>) -> Void) {
            run(arguments: arguments, completion: completion)
        }
        func restartRecordHeld(by owner: HubLifecycleToken) -> Bool { false }
        func loadedState(completion: @escaping (HubServiceLoadState) -> Void) { completion(loaded) }
        func recoverAfterPause(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) { completion(.success("")) }
        func releaseRestartRecord(owner: HubLifecycleToken) {}

        func reconcile(owner: HubLifecycleToken, ensure: @escaping () throws -> HubLaunchAgentChange,
                       completion: @escaping (HubReconcileOutcome) -> Void) { completion(.completed) }
        func deleteHubAndData(owner: HubLifecycleToken, completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
            deleteCompletion = completion
        }
    }
}
