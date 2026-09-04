// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import Darwin
import XCTest
@testable import Teslatlas_Hub

final class HubControllerTests: XCTestCase {
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
        XCTAssertEqual(windowController.window?.contentRect(forFrameRect: windowController.window!.frame).size,
                       NSSize(width: 900, height: 630))
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

    func testServiceDetailsUpdateButtonInstallsCurrentPackage() throws {
        let installer = RecordingInstaller()
        let controller = HubController(commandRunner: CountingRunner(),
                                       installedCommandRunner: CountingRunner(),
                                       installer: installer,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       serviceInstalledOverride: true)
        let changed = expectation(description: "service package updated")
        let details = ServiceDetailsWindowController(snapshot: .previewRunning,
                                                     controller: controller,
                                                     onChanged: { changed.fulfill() })
        let button = try XCTUnwrap(buttons(in: details.window?.contentView)
            .first { $0.title == "Update Service…" })
        XCTAssertNotNil(button.target)
        XCTAssertNotNil(button.action)
        button.performClick(nil)
        wait(for: [changed], timeout: 2)
        XCTAssertEqual(installer.installCalls, 1)
    }

    func testServiceDetailsRejectsMutationWhenOwnerGateIsClosed() throws {
        let installer = RecordingInstaller()
        let controller = HubController(commandRunner: CountingRunner(),
                                       installedCommandRunner: CountingRunner(),
                                       installer: installer,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       serviceInstalledOverride: true)
        let details = ServiceDetailsWindowController(
            snapshot: .previewRunning,
            controller: controller,
            mutationAllowed: { false },
            onChanged: { XCTFail("blocked mutation reported a change") }
        )
        let update = try XCTUnwrap(buttons(in: details.window?.contentView)
            .first { $0.title == "Update Service…" })
        update.performClick(nil)
        XCTAssertEqual(installer.installCalls, 0)

        details.setMutationsEnabled(false)
        for title in ["Update Service…", "Uninstall Hub…"] {
            let button = try XCTUnwrap(buttons(in: details.window?.contentView)
                .first { $0.title == title })
            XCTAssertFalse(button.isEnabled)
        }
    }

    func testPendingServiceDetailsMutationBlocksCloseAndPrimaryModalReplacementUntilCompletion() throws {
        // Catches a pending operation losing its owner and leaving the dashboard mutation gate locked.
        let installer = PendingInstaller()
        let controller = HubController(
            commandRunner: CountingRunner(),
            installedCommandRunner: CountingRunner(),
            installer: installer,
            serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
            serviceInstalledOverride: true,
            initialSnapshot: .previewRunning
        )
        let dashboard = MainWindowController(controller: controller)
        let details = try XCTUnwrap(dashboard.showServiceDetails())
        let update = try XCTUnwrap(buttons(in: details.window?.contentView)
            .first { $0.title == "Update Service…" })
        let close = try XCTUnwrap(details.window?.standardWindowButton(.closeButton))

        update.performClick(nil)

        XCTAssertEqual(installer.installCalls, 1)
        XCTAssertFalse(close.isEnabled)
        XCTAssertFalse(details.windowShouldClose(try XCTUnwrap(details.window)))
        XCTAssertNil(dashboard.showDiagnostics())

        installer.completeInstall(.success(""))
        let restored = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in close.isEnabled }, object: nil)
        wait(for: [restored], timeout: 2)
        let diagnostics = dashboard.showDiagnostics()
        XCTAssertNotNil(diagnostics)
        diagnostics?.window?.performClose(nil)
    }

    func testConfirmedDeleteDataActionUsesDeleteDataAndDismissesAfterUnlocking() throws {
        // Catches the direct danger action using the keep-data path or dismissing before its lock is cleared.
        let installer = PendingInstaller()
        let controller = HubController(
            commandRunner: CountingRunner(),
            installedCommandRunner: CountingRunner(),
            installer: installer,
            serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
            serviceInstalledOverride: true,
            initialSnapshot: .previewRunning
        )
        var events: [String] = []
        let changed = expectation(description: "delete-data refresh requested")
        let dismissed = expectation(description: "delete-data details dismissed")
        let details = ServiceDetailsWindowController(
            snapshot: .previewRunning,
            controller: controller,
            onMutationStateChanged: { pending in events.append(pending ? "locked" : "unlocked") },
            onChanged: {
                events.append("changed")
                changed.fulfill()
            },
            onDismiss: {
                events.append("dismissed")
                dismissed.fulfill()
            }
        )
        let delete = try XCTUnwrap(buttons(in: details.window?.contentView).first {
            $0.identifier?.rawValue == "hub.service.delete-data"
        })
        let close = try XCTUnwrap(details.window?.standardWindowButton(.closeButton))

        delete.performClick(nil)

        XCTAssertEqual(installer.deleteDataChoices, [true])
        XCTAssertEqual(events, ["locked"])
        XCTAssertFalse(close.isEnabled)

        installer.completeUninstall(.success(""))
        wait(for: [changed, dismissed], timeout: 1)
        XCTAssertEqual(events, ["locked", "unlocked", "changed", "dismissed"])
    }

    func testDeleteDataFailureRestoresTheSharedMutationGate() throws {
        // Catches a failed destructive action leaving the sheet locked after its error is dismissed.
        let installer = PendingInstaller()
        let controller = HubController(
            commandRunner: CountingRunner(),
            installedCommandRunner: CountingRunner(),
            installer: installer,
            serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
            serviceInstalledOverride: true,
            initialSnapshot: .previewRunning
        )
        var events: [String] = []
        var presentedErrors: [String] = []
        let unlocked = expectation(description: "delete-data failure unlocked")
        let details = ServiceDetailsWindowController(
            snapshot: .previewRunning,
            controller: controller,
            onMutationStateChanged: { pending in
                events.append(pending ? "locked" : "unlocked")
                if !pending { unlocked.fulfill() }
            },
            onChanged: { XCTFail("failed delete-data action refreshed details") },
            errorPresenter: { error in presentedErrors.append(error.localizedDescription) }
        )
        let delete = try XCTUnwrap(buttons(in: details.window?.contentView).first {
            $0.identifier?.rawValue == "hub.service.delete-data"
        })
        let close = try XCTUnwrap(details.window?.standardWindowButton(.closeButton))

        delete.performClick(nil)
        XCTAssertEqual(installer.deleteDataChoices, [true])
        XCTAssertFalse(close.isEnabled)

        installer.completeUninstall(.failure(HubActionError.commandFailed("uninstall failed")))
        wait(for: [unlocked], timeout: 1)

        XCTAssertEqual(events, ["locked", "unlocked"])
        XCTAssertEqual(presentedErrors, ["uninstall failed"])
        XCTAssertTrue(close.isEnabled)
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

    func testServiceDetailsRetainUpdateAndBothUninstallOutcomes() throws {
        // Catches losing a maintenance or irreversible deletion path in the redesign.
        let controller = ServiceDetailsWindowController(
            snapshot: .previewRunning,
            controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
            onChanged: {}
        )

        let titles = buttons(in: controller.window?.contentView).map(\.title)

        XCTAssertTrue(titles.contains("Update Service…"))
        XCTAssertTrue(titles.contains("Uninstall Hub…"))
        XCTAssertEqual(ServiceDetailsWindowController.deleteDataConfirmation().buttons.map(\.title),
                       ["Cancel", "Delete Data and Uninstall"])
    }

    func testServiceDetailsUseTheSharedSheetSizeAndIdentifyTheDeleteDataAction() throws {
        // Catches using a standalone detail window or an unidentifiable destructive action.
        let details = ServiceDetailsWindowController(
            snapshot: .previewRunning,
            controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
            onChanged: {}
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
        controller.installService(completion: done)
        controller.uninstallService(deleteData: false, completion: done)
        controller.importTeslaMate(source: "postgres://example", carID: "1", passwordFile: "/tmp/password", encryptionKeyFile: "/tmp/encryption", acknowledgeV42CompatibleSchema: true, completion: done)
        controller.configureTeslaAccount(tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh"), completion: done)
        controller.performVehicleControl(.climateStart, completion: done)
        controller.stopHub(completion: done)
        controller.restartHub(completion: done)
        controller.signOutTeslaAccount(completion: done)
        wait(for: [expectation], timeout: 1)
        XCTAssertEqual(runner.calls, 0)
    }

    func testServicePlanBootstrapsOnlyWhenUnloaded() {
        let loaded = LaunchctlServiceController.commandPlan(action: .restart, loaded: true, domain: "gui/1", service: "gui/1/com.teslatlas.hub", plist: "/tmp/hub.plist")
        XCTAssertEqual(loaded, [["bootout", "gui/1/com.teslatlas.hub"], ["bootstrap", "gui/1", "/tmp/hub.plist"]])
        let unloaded = LaunchctlServiceController.commandPlan(action: .restart, loaded: false, domain: "gui/1", service: "gui/1/com.teslatlas.hub", plist: "/tmp/hub.plist")
        XCTAssertEqual(unloaded, [["bootstrap", "gui/1", "/tmp/hub.plist"]])
        XCTAssertEqual(LaunchctlServiceController.commandPlan(action: .start, loaded: false, domain: "gui/1", service: "gui/1/com.teslatlas.hub", plist: "/tmp/hub.plist"), [["bootstrap", "gui/1", "/tmp/hub.plist"]])
        XCTAssertEqual(LaunchctlServiceController.commandPlan(action: .start, loaded: true, domain: "gui/1", service: "gui/1/com.teslatlas.hub", plist: "/tmp/hub.plist"), [["kickstart", "gui/1/com.teslatlas.hub"]])
        XCTAssertEqual(LaunchctlServiceController.commandPlan(action: .stop, loaded: true, domain: "gui/1", service: "gui/1/com.teslatlas.hub", plist: "/tmp/hub.plist"), [["bootout", "gui/1/com.teslatlas.hub"]])
        XCTAssertEqual(LaunchctlServiceController.commandPlan(action: .stop, loaded: false, domain: "gui/1", service: "gui/1/com.teslatlas.hub", plist: "/tmp/hub.plist"), [])
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
        let home = FileManager.default.temporaryDirectory
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
        let home = FileManager.default.temporaryDirectory
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
        let home = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-diagnostics-\(UUID().uuidString)", isDirectory: true)
        try? FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
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
            XCTAssertTrue(text.contains("Expected Hub: 1.0.0"))
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
                                       installedCommandRunner: runner,
                                       serviceRunner: ScriptedService(events: events, loadState: .loaded),
                                       serviceInstalledOverride: false)
        let finished = expectation(description: "running Hub diagnostics finished")
        controller.runFullDiagnostics { text in
            XCTAssertTrue(text.contains("Hub collection resumed."))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:stop", "service:start"])
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
                                       installedCommandRunner: runner,
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
                                       installedCommandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       serviceInstalledOverride: false)
        let diagnostics = DiagnosticsWindowController(controller: controller)

        XCTAssertNotNil(diagnostics.window)
        XCTAssertEqual(runner.calls, 0)
    }

    func testDiagnosticsRowsHaveNonzeroDocumentAndRowFramesAfterRendering() throws {
        let runner = CommandMapRunner(responses: [
            "doctor": .success("{\"status\":\"ok\"}"),
            "preflight": .success("{\"status\":\"ready\"}"),
            "status": .success("{\"status\":\"ok\"}")
        ])
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
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
        let runner = CommandMapRunner(responses: [
            "doctor": .success("{\"status\":\"old\"}"),
            "preflight": .success("{\"status\":\"ready\"}"),
            "status": .success("{\"status\":\"ok\"}")
        ])
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       serviceInstalledOverride: false)
        let diagnostics = DiagnosticsWindowController(controller: controller)
        let root = try XCTUnwrap(diagnostics.window?.contentView)
        let run = try XCTUnwrap(buttons(in: root).first { $0.title == "Run Again" })

        run.performClick(nil)
        wait(for: [XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            self.descendantViews(in: root).compactMap { $0 as? NSTextView }.contains { $0.string.contains("old") }
        }, object: nil)], timeout: 3)
        let firstReport = expectation(description: "first diagnostic report rendered")
        DispatchQueue.main.async {
            let raw = self.descendantViews(in: root).compactMap { $0 as? NSTextView }.first {
                $0.identifier?.rawValue == "hub.diagnostics.raw-report"
            }
            XCTAssertTrue(raw?.string.contains("old") == true)
            XCTAssertTrue(self.labels(in: root).contains { $0.stringValue == "Environment doctor" })
            try? XCTUnwrap(self.buttons(in: root).first { $0.title == "Show raw redacted report" }).performClick(nil)
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

    func testLogsNameStandardOutputAndErrorStreams() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let folder = home.appendingPathComponent("Library/Logs/Teslatlas Hub", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try "normal event\n".write(to: folder.appendingPathComponent("hub.out.log"),
                                    atomically: true, encoding: .utf8)
        try "error event\n".write(to: folder.appendingPathComponent("hub.err.log"),
                                   atomically: true, encoding: .utf8)
        let controller = HubController(homeDirectory: home, serviceInstalledOverride: false)
        let finished = expectation(description: "logs loaded")

        controller.logs { text in
            XCTAssertTrue(text.contains("== hub.out.log ==\nnormal event"))
            XCTAssertTrue(text.contains("== hub.err.log ==\nerror event"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
    }

    func testRefreshUsesInstalledBinaryAndReportsVersionMismatch() {
        let embedded = RecordingCommandRunner(result: .failure(HubActionError.commandFailed("unused")))
        let installed = RecordingCommandRunner(result: .success("""
        {"status":"ok","version":"9.9.9","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"vehicle":null,"legacyCredentials":{"present":false}}
        """))
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
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
        XCTAssertEqual(installed.arguments, [["--config", NSHomeDirectory() + "/Library/Application Support/Teslatlas Hub/config.toml", "status"]])
    }

    func testRefreshPreservesStoppedStateAcrossVersionMismatch() {
        let installed = RecordingCommandRunner(result: .success("""
        {"status":"ok","version":"9.9.9","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
        """))
        let controller = HubController(installedCommandRunner: installed,
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
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
        """))
        let controller = HubController(installedCommandRunner: installed,
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
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicle":null,"vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"One"},{"vehicleId":"\(secondID.uuidString)","displayName":"Two"}],"credentials":{"present":true},"legacyCredentials":{"present":false},"fleetCredentials":{"present":true}}
        """))
        let controller = HubController(installedCommandRunner: installed,
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

    func testMultipleVehicleControlUsesExplicitSelectedVehicle() {
        let firstID = UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5")!
        let secondID = UUID(uuidString: "FB25AA4A-A719-4575-8BB1-02D4524F2571")!
        let status = """
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"One"},{"vehicleId":"\(secondID.uuidString)","displayName":"Two"}],"credentials":{"present":true}}
        """
        let installed = RecordingCommandRunner(result: .success(status))
        let controller = HubController(installedCommandRunner: installed,
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
        let config = NSHomeDirectory() + "/Library/Application Support/Teslatlas Hub/config.toml"
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
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"One"},{"vehicleId":"\(secondID.uuidString)","displayName":"Two"}],"credentials":{"present":true}}
        """)
        let controller = HubController(installedCommandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
                                       serviceInstalledOverride: true)
        let loaded = expectation(description: "vehicle dashboard loaded")
        let dashboard = MainWindowController(controller: controller) { _ in loaded.fulfill() }
        wait(for: [loaded], timeout: 1)

        let command = try XCTUnwrap(buttons(in: dashboard.window?.contentView)
            .first { $0.title == "Wake" })
        command.performClick(nil)
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
            XCTAssertTrue(text.contains("Wake Vehicle accepted for One"))
            XCTAssertFalse(text.contains("Wake Vehicle accepted for Two"))
            rendered.fulfill()
        }
        wait(for: [rendered], timeout: 1)
    }

    func testMultipleVehicleDashboardShowsNativeSelector() throws {
        let firstID = UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5")!
        let secondID = UUID(uuidString: "FB25AA4A-A719-4575-8BB1-02D4524F2571")!
        let status = """
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"One"},{"vehicleId":"\(secondID.uuidString)","displayName":"Two"}],"credentials":{"present":true}}
        """
        let installed = RecordingCommandRunner(result: .success(status))
        let controller = HubController(installedCommandRunner: installed,
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
            {"status":"ok","version":"1.0.0","ready":false,"provider":"fleet","credentials":{"present":false}}
            """)
        ])
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
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
        let controller = HubController(installedCommandRunner: runner,
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
            {"status":"ok","version":"1.0.0","ready":false,"provider":"fleet","credentials":{"present":false},"legacyCredentials":{"present":true},"fleetCredentials":{"present":false}}
            """)
        ])
        let controller = HubController(installedCommandRunner: installed,
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
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(vehicleID.uuidString)","displayName":"One"}],"credentials":{"present":true}}
        """
        let embedded = RecordingCommandRunner(result: .failure(HubActionError.commandFailed("unused")))
        let installed = RecordingCommandRunner(result: .success(status))
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
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
        let config = NSHomeDirectory() + "/Library/Application Support/Teslatlas Hub/config.toml"
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
            XCTAssertFalse(self.buttons(in: windowController.window?.contentView)
                .contains { $0.title == "Vehicle Controls…" })
            for title in ["Start Climate", "Stop Climate", "Wake", "Lock",
                          "Unlock", "Flash Lights", "Honk"] {
                let button = self.buttons(in: windowController.window?.contentView)
                    .first { $0.title == title }
                XCTAssertNotNil(button, "missing \(title)")
                XCTAssertFalse(button?.isHidden ?? true, "hidden \(title)")
                XCTAssertTrue(button?.isEnabled ?? false, "preview disabled \(title)")
            }
            XCTAssertFalse(windowController.connectButton.isHidden)
            XCTAssertEqual(windowController.connectButton.title, "Manage Tesla")
            XCTAssertFalse(windowController.connectButton.isBordered)
            XCTAssertTrue(self.labels(in: windowController.window?.contentView)
                .contains { $0.stringValue == "Connected · Fleet API" })
            XCTAssertTrue(self.buttons(in: windowController.window?.contentView)
                .compactMap { $0 as? HubActionButton }
                .allSatisfy { !$0.isBordered })
            XCTAssertNil(windowController.window?.defaultButtonCell)
            XCTAssertTrue(self.buttons(in: windowController.window?.contentView)
                .contains { $0.title == "Stop Hub…" && !$0.isHidden })
            XCTAssertTrue(self.buttons(in: windowController.window?.contentView)
                .contains { $0.title == "Restart Hub" && !$0.isHidden })
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
        let home = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-start-test-\(UUID().uuidString)", isDirectory: true)
        try? FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let status = """
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
        """
        let installed = RecordingCommandRunner(result: .success(status))
        let service = PendingServiceRunner(loadState: .unloaded)
        let controller = HubController(installedCommandRunner: installed,
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
            installedCommandRunner: runner,
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
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
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

    func testInFlightRefreshDoesNotRepaintDashboardDuringServiceTransition() {
        let runner = PendingCommandRunner()
        let controller = HubController(installedCommandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder(),
                                                                      loadState: .loaded),
                                       serviceInstalledOverride: true)
        let refreshed = expectation(description: "in-flight refresh returned")
        let dashboard = MainWindowController(controller: controller) { _ in
            refreshed.fulfill()
        }

        dashboard.settleStartedHubFromOnboarding()
        runner.complete(.success("""
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1048576},"ready":false,"provider":"legacy","credentials":{"present":true}}
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
        let controller = HubController(installedCommandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder(),
                                                                      loadState: .loaded),
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)
        let timedOut = expectation(description: "transition deadline fired")
        let started = Date()
        let dashboard = MainWindowController(
            controller: controller,
            serviceTransitionTimeout: 0.1,
            serviceTransitionPollInterval: 0.01,
            errorPresenter: { error in
                XCTAssertTrue(error.localizedDescription.contains("did not finish"))
                XCTAssertLessThan(Date().timeIntervalSince(started), 0.4)
                timedOut.fulfill()
            }
        )

        dashboard.settleStartedHubFromOnboarding()
        XCTAssertTrue(labels(in: dashboard.window?.contentView)
            .contains { $0.stringValue == "Starting Hub…" })

        wait(for: [timedOut], timeout: 0.5)
        let visibleText = labels(in: dashboard.window?.contentView).map(\.stringValue)
        XCTAssertFalse(visibleText.contains("Starting Hub…"))
        XCTAssertTrue(visibleText.contains("Hub is running"))
        XCTAssertTrue(dashboard.connectButton.isEnabled)
        XCTAssertTrue(dashboard.importButton.isEnabled)
    }

    func testServiceTransitionDeadlineIncludesPendingServiceCommand() throws {
        let status = """
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
        """
        let runner = RecordingCommandRunner(result: .success(status))
        let service = PendingServiceRunner(loadState: .unloaded)
        let controller = HubController(installedCommandRunner: runner,
                                       serviceRunner: service,
                                       serviceInstalledOverride: true)
        let loaded = expectation(description: "stopped dashboard loaded")
        let timedOut = expectation(description: "pending service command timed out in UI")
        let dashboard = MainWindowController(
            controller: controller,
            serviceTransitionTimeout: 0.1,
            serviceTransitionPollInterval: 0.01,
            errorPresenter: { _ in timedOut.fulfill() },
            onInitialRefresh: { _ in loaded.fulfill() }
        )
        wait(for: [loaded], timeout: 1)

        let start = try XCTUnwrap(buttons(in: dashboard.window?.contentView)
            .first { $0.title == "Start Hub" })
        start.performClick(nil)
        wait(for: [timedOut], timeout: 0.5)

        XCTAssertFalse(labels(in: dashboard.window?.contentView)
            .contains { $0.stringValue == "Starting Hub…" })
        XCTAssertTrue(dashboard.connectButton.isEnabled)
        XCTAssertTrue(dashboard.importButton.isEnabled)
    }

    func testExpiredServiceCommandFailureCannotCancelNewTransition() throws {
        let status = """
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
        """
        let runner = RecordingCommandRunner(result: .success(status))
        let service = PendingServiceRunner(loadState: .unloaded)
        let controller = HubController(installedCommandRunner: runner,
                                       serviceRunner: service,
                                       serviceInstalledOverride: true)
        let loaded = expectation(description: "stopped dashboard loaded")
        let firstTimeout = expectation(description: "first transition expired")
        var errors: [Error] = []
        let dashboard = MainWindowController(
            controller: controller,
            serviceTransitionTimeout: 0.08,
            serviceTransitionPollInterval: 0.01,
            errorPresenter: { error in
                errors.append(error)
                if errors.count == 1 { firstTimeout.fulfill() }
            },
            onInitialRefresh: { _ in loaded.fulfill() }
        )
        wait(for: [loaded], timeout: 1)

        try XCTUnwrap(buttons(in: dashboard.window?.contentView)
            .first { $0.title == "Start Hub" }).performClick(nil)
        wait(for: [firstTimeout], timeout: 0.5)
        try XCTUnwrap(buttons(in: dashboard.window?.contentView)
            .first { $0.title == "Start Hub" }).performClick(nil)

        service.complete(at: 0, .failure(HubActionError.commandFailed("expired failure")))
        let checked = expectation(description: "late failure ignored")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.02) {
            XCTAssertTrue(self.labels(in: dashboard.window?.contentView)
                .contains { $0.stringValue == "Starting Hub…" })
            XCTAssertFalse(dashboard.connectButton.isEnabled)
            XCTAssertEqual(errors.count, 1)
            checked.fulfill()
        }
        wait(for: [checked], timeout: 0.2)
    }

    func testExpiredServiceCommandSuccessCannotSettleNewTransition() throws {
        let status = """
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
        """
        let runner = RecordingCommandRunner(result: .success(status))
        let service = PendingServiceRunner(loadState: .unloaded)
        let controller = HubController(installedCommandRunner: runner,
                                       serviceRunner: service,
                                       serviceInstalledOverride: true)
        let loaded = expectation(description: "stopped dashboard loaded")
        let firstTimeout = expectation(description: "first transition expired")
        var errorCount = 0
        let dashboard = MainWindowController(
            controller: controller,
            serviceTransitionTimeout: 0.08,
            serviceTransitionPollInterval: 0.01,
            errorPresenter: { _ in
                errorCount += 1
                if errorCount == 1 { firstTimeout.fulfill() }
            },
            onInitialRefresh: { _ in loaded.fulfill() }
        )
        wait(for: [loaded], timeout: 1)

        try XCTUnwrap(buttons(in: dashboard.window?.contentView)
            .first { $0.title == "Start Hub" }).performClick(nil)
        wait(for: [firstTimeout], timeout: 0.5)
        try XCTUnwrap(buttons(in: dashboard.window?.contentView)
            .first { $0.title == "Start Hub" }).performClick(nil)

        service.loadState = .loaded
        service.complete(at: 0, .success(""))
        let checked = expectation(description: "late success ignored")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.02) {
            XCTAssertTrue(self.labels(in: dashboard.window?.contentView)
                .contains { $0.stringValue == "Starting Hub…" })
            XCTAssertFalse(dashboard.connectButton.isEnabled)
            XCTAssertEqual(errorCount, 1)
            checked.fulfill()
        }
        wait(for: [checked], timeout: 0.2)
    }

    func testNewestRefreshOwnsSnapshotWhenStatusResponsesCompleteOutOfOrder() {
        let runner = OutOfOrderCommandRunner()
        let controller = HubController(installedCommandRunner: runner,
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
        {"status":"ok","version":"1.0.0","ready":true,"provider":"legacy","credentials":{"present":true}}
        """))
        runner.complete(at: 0, with: .success("""
        {"status":"ok","version":"1.0.0","ready":false,"provider":"legacy","credentials":{"present":true}}
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
            for title in ["Start Climate", "Stop Climate", "Wake", "Lock",
                          "Unlock", "Flash Lights", "Honk"] {
                let button = self.buttons(in: windowController.window?.contentView)
                    .first { $0.title == title }
                XCTAssertNotNil(button, "missing \(title)")
                XCTAssertTrue(button?.isHidden ?? false, "visible \(title)")
                XCTAssertFalse(button?.isEnabled ?? true, "enabled \(title)")
            }
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
        let view = HubVehiclesView(actions: .noOp)
        view.apply(snapshot: snapshot, enabled: true)

        let text = labels(in: view).map(\.stringValue).joined(separator: " ")
        XCTAssertTrue(text.contains("Aurora"))
        XCTAssertTrue(text.contains("Comet"))
        XCTAssertFalse(text.contains("Model Y"))
    }

    func testVehicleConfirmationKeepsItsOriginalTargetAndStaleTargetIsRejected() throws {
        let firstID = UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5")!
        let secondID = UUID(uuidString: "FB25AA4A-A719-4575-8BB1-02D4524F2571")!
        let status = """
        {"status":"ok","version":"1.0.0","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"Aurora"},{"vehicleId":"\(secondID.uuidString)","displayName":"Comet"}],"credentials":{"present":true}}
        """
        let runner = PendingVehicleControlRunner(status: status)
        let controller = HubController(installedCommandRunner: runner,
                                       serviceRunner: ScriptedService(events: EventRecorder(), loadState: .loaded),
                                       serviceInstalledOverride: true)
        let loaded = expectation(description: "vehicle dashboard loaded")
        let windowController = MainWindowController(controller: controller) { _ in loaded.fulfill() }
        wait(for: [loaded], timeout: 1)

        try XCTUnwrap(buttons(in: windowController.window?.contentView)
            .first { $0.title == "Wake" }).performClick(nil)
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
        let staleController = HubController(installedCommandRunner: staleRunner,
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

    func testFleetProviderWithoutConnectionShowsConnectTesla() throws {
        var snapshot = HubSnapshot.previewRunning
        snapshot.account = "Not configured"
        snapshot.provider = .fleet
        let navigation = HubNavigationBar(actions: .noOp)

        navigation.apply(snapshot: snapshot, enabled: true)

        let account = try XCTUnwrap(buttons(in: navigation)
            .first { $0.identifier?.rawValue == "hub.nav.account" })
        XCTAssertEqual(account.title, "Connect Tesla")
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
            ({ var snapshot = valid; snapshot.account = "Not configured"; return snapshot }(), false, false, false, false, false),
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
                                       installedCommandRunner: installed,
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
                                       installedCommandRunner: runner,
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
                                       installedCommandRunner: runner,
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
                                       installedCommandRunner: runner,
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
        let installer = ScriptedInstaller(events: events)
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: installer,
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
        XCTAssertFalse(events.values.contains("install"))
        XCTAssertFalse(events.values.contains("service:start"))
    }

    func testInstalledMigrationKeepsFleetProviderAndTokensConfig() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success(
            #"{"status":"imported","captureMode":"online-snapshot"}"#
        ))
        let installer = ScriptedInstaller(events: events)
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: installer,
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

    func testFreshMigrationInstallsCurrentServicePackageAfterSuccessfulCopy() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success(
            #"{"status":"imported","captureMode":"online-snapshot"}"#
        ))
        let installer = ScriptedInstaller(events: events)
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: installer,
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
        XCTAssertFalse(events.values.contains("install"))
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
        let installer = ScriptedInstaller(events: events)
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: installer,
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
                                       installedCommandRunner: runner,
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
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: CountingRunner(),
                                       installer: ScriptedInstaller(events: EventRecorder()),
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

    func testInstalledMigrationDoesNotMutateWithStaleBinaryWhenPackageUpdateFails() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success(
            #"{"status":"imported","captureMode":"online-snapshot"}"#
        ))
        let installer = ScriptedInstaller(
            events: events,
            result: .failure(HubActionError.commandFailed("package failed"))
        )
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: installer,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "dashboard import ignores package installer")

        controller.importTeslaMate(source: "postgres://reader@localhost/teslamate",
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
        XCTAssertFalse(events.values.contains("install"))
        XCTAssertFalse(events.values.contains("service:start"))
        XCTAssertTrue(controller.hasPendingMigrationHandover)
    }

    func testInstalledAccountConfigureInstallsBeforeSingleSetupAndRestarts() throws {
        let events = EventRecorder()
        let embedded = CountingRunner()
        let installed = ScriptedRunner(events: events, result: .success("ok"))
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        var configAtInstall = ""
        let installer = ScriptedInstaller(events: events) {
            configAtInstall = (try? self.configContents(in: home)) ?? ""
        }
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
                                       installer: installer,
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
        XCTAssertEqual(events.values, [
            "service:stop", "install", "setup", "service:start", "command"
        ])
        XCTAssertEqual(embedded.calls, 0)
        XCTAssertNotNil(installed.stdin)
        XCTAssertTrue(installed.arguments[0].contains("--all-vehicles"))
        XCTAssertEqual(installed.arguments.count, 2)
        XCTAssertTrue(configAtInstall.contains("provider = \"fleet\""))
        XCTAssertTrue(try configContents(in: home).contains("provider = \"legacy\""))
    }

    func testInstalledAccountSetupFailureKeepsLegacySelectedAndHubStopped() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(
            events: events,
            result: .failure(HubActionError.commandFailed("setup failed"))
        )
        let installer = ScriptedInstaller(events: events)
        let service = ScriptedService(events: events, loadState: .loaded)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: installer,
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
            XCTAssertTrue(error.localizedDescription.contains("remains stopped"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertTrue(try configContents(in: home).contains("provider = \"legacy\""))
        XCTAssertEqual(events.values, ["service:stop", "install", "setup"])
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
                                       installedCommandRunner: runner,
                                       installer: ScriptedInstaller(events: events),
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
        XCTAssertEqual(events.values, ["service:stop", "install", "setup"])
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
                                       installedCommandRunner: runner,
                                       installer: ScriptedInstaller(events: events),
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
        XCTAssertEqual(events.values, ["service:stop"])
    }

    func testInstalledAccountConfigureRestartsOldServiceAfterInstallFailure() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success("ok"))
        let installer = ScriptedInstaller(
            events: events,
            result: .failure(HubActionError.commandFailed("package failed"))
        )
        let service = ScriptedService(events: events, loadState: .loaded)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: installer,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)
        let finished = expectation(description: "install failure returned")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case let .failure(error) = result else { return XCTFail("expected install failure") }
            XCTAssertTrue(error.localizedDescription.contains("package failed"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(try configContents(in: home), original)
        XCTAssertEqual(events.values, ["service:stop", "install", "service:start"])
    }

    func testForwardOnlyInstallFailureLeavesNewServiceStopped() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success("ok"))
        let installer = ScriptedInstaller(
            events: events,
            result: .failure(HubActionError.commandFailed(
                "TESLATLAS_FORWARD_ONLY_UPGRADE: schema migration failed"
            ))
        )
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: installer,
                                       serviceRunner: ScriptedService(events: events),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)
        let finished = expectation(description: "forward-only failure returned")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case let .failure(error) = result else { return XCTFail("expected install failure") }
            XCTAssertTrue(error.localizedDescription.contains("TESLATLAS_FORWARD_ONLY_UPGRADE"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(try configContents(in: home), original)
        XCTAssertEqual(events.values, ["service:stop", "install"])
    }

    func testInstalledUnconfiguredLegacySetupRunsBeforePackagePreflight() throws {
        let events = EventRecorder()
        let embedded = VersionAwareRunner(
            events: events,
            versionResult: .success("teslatlas-hub 1.0.0\n"),
            commandResult: .success("ok")
        )
        let installed = VersionAwareRunner(
            events: events,
            versionResult: .success("teslatlas-hub 1.0.0-alpha.0\n")
        )
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
                                       installer: ScriptedInstaller(events: events),
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
        XCTAssertEqual(events.values, [
            "service:stop", "setup", "version", "version", "install", "service:start", "command"
        ])
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
                                       installedCommandRunner: runner,
                                       installer: RecordingInstaller(),
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
                                       installedCommandRunner: runner,
                                       installer: RecordingInstaller(),
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
                                       installedCommandRunner: runner,
                                       installer: RecordingInstaller(),
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

    func testInstalledUnconfiguredLegacyReusesExactPackagedService() throws {
        let events = EventRecorder()
        let embedded = VersionAwareRunner(
            events: events,
            versionResult: .success("teslatlas-hub 1.0.0\n"),
            commandResult: .success("configured")
        )
        let installed = VersionAwareRunner(
            events: events,
            versionResult: .success("teslatlas-hub 1.0.0\n")
        )
        let installer = RecordingInstaller()
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
                                       installer: installer,
                                       serviceRunner: ScriptedService(events: events),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "matching service reused")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(installer.installCalls, 0)
        XCTAssertEqual(events.values, [
            "service:stop", "setup", "version", "version", "service:start", "command"
        ])
        XCTAssertTrue(try configContents(in: home).contains("provider = \"legacy\""))
    }

    func testBundledServiceVersionMatchIsExact() {
        XCTAssertTrue(HubController.isBundledServiceVersionOutput(
            "teslatlas-hub 1.0.0\n"
        ))
        XCTAssertFalse(HubController.isBundledServiceVersionOutput(
            "teslatlas-hub 1.0.0-alpha.0"
        ))
        XCTAssertFalse(HubController.isBundledServiceVersionOutput(
            "prefix teslatlas-hub 1.0.0"
        ))
        XCTAssertFalse(HubController.isBundledServiceVersionOutput(
            "teslatlas-hub 1.0.0\nextra"
        ))
    }

    func testUnconfiguredLegacyInstallerFailureKeepsNewCredentialsStopped() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success("configured"))
        let installer = ScriptedInstaller(
            events: events,
            result: .failure(HubActionError.commandFailed("package failed"))
        )
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: installer,
                                       serviceRunner: ScriptedService(events: events),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "configured legacy remains stopped")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case let .failure(error) = result else {
                return XCTFail("expected package failure")
            }
            XCTAssertTrue(error.localizedDescription.contains("configured"))
            XCTAssertTrue(error.localizedDescription.contains("remains stopped"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:stop", "setup", "command", "install"])
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
                                       installedCommandRunner: runner,
                                       installer: ScriptedInstaller(events: events),
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
            XCTAssertTrue(error.localizedDescription.contains("remains stopped"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:stop", "install", "setup"])
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

    func testProcessExecutorDeliversLineBeforeChildExit() {
        let releaseFile = FileManager.default.temporaryDirectory
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

    func testUninstallPassesSeparateKeepAndDeleteDataChoices() {
        let installer = RecordingInstaller()
        let service = ScriptedService(events: EventRecorder())
        let controller = HubController(commandRunner: CountingRunner(),
                                       installer: installer,
                                       serviceRunner: service,
                                       serviceInstalledOverride: false)
        let kept = expectation(description: "keep-data uninstall finished")
        controller.uninstallService(deleteData: false) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            kept.fulfill()
        }
        wait(for: [kept], timeout: 2)

        let finished = expectation(description: "delete-data uninstall finished")
        controller.uninstallService(deleteData: true) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)
        XCTAssertEqual(installer.deleteDataChoices, [false, true])
    }

    func testInstallStagesAndVerifiesSignedPackageBeforeInstallerRuns() throws {
        let package = "/Applications/Teslatlas Hub.app/Contents/Resources/TeslatlasHubService.pkg"
        let app = "/Applications/Teslatlas Hub.app"
        let digest = String(repeating: "a", count: 64)
        let command = try EmbeddedInstaller.installCommand(
            packagePath: package,
            appPath: app,
            expectedSHA256: digest,
            expectedTeamID: "4AA2EMZ2HA"
        )
        XCTAssertTrue(command.contains("/private/var/tmp/teslatlas-hub-install.XXXXXX"))
        XCTAssertTrue(command.contains("/usr/bin/codesign --verify --deep --strict"))
        XCTAssertTrue(command.contains("/usr/sbin/spctl --assess --type execute"))
        XCTAssertTrue(command.contains("/usr/bin/install -o root -g wheel -m 0600"))
        XCTAssertTrue(command.contains("/bin/test"))
        XCTAssertFalse(command.contains("/usr/bin/test"))
        XCTAssertTrue(FileManager.default.isExecutableFile(atPath: "/bin/test"))
        XCTAssertTrue(command.contains("/usr/bin/shasum -a 256 \"$staged\""))
        XCTAssertTrue(command.contains("/usr/sbin/pkgutil --check-signature \"$staged\""))
        XCTAssertTrue(command.contains("/usr/sbin/spctl --assess --type install"))
        XCTAssertTrue(command.hasSuffix("/usr/sbin/installer -pkg \"$staged\" -target /"))
        XCTAssertFalse(command.contains("/usr/sbin/installer -pkg '\(package)'"))
        let syntaxCheck = Process()
        syntaxCheck.executableURL = URL(fileURLWithPath: "/bin/sh")
        syntaxCheck.arguments = ["-n", "-c", command]
        try syntaxCheck.run()
        syntaxCheck.waitUntilExit()
        XCTAssertEqual(syntaxCheck.terminationStatus, 0)
        XCTAssertThrowsError(try EmbeddedInstaller.installCommand(
            packagePath: package,
            appPath: app,
            expectedSHA256: "bad",
            expectedTeamID: "4AA2EMZ2HA"
        ))
        XCTAssertThrowsError(try EmbeddedInstaller.installCommand(
            packagePath: package,
            appPath: app,
            expectedSHA256: digest,
            expectedTeamID: "bad"
        ))
    }

    func testUninstallUsesOnlyTheInstalledRootOwnedUninstaller() throws {
        let command = EmbeddedInstaller.uninstallCommand(deleteData: true)
        XCTAssertTrue(command.contains("/Library/Application Support/Teslatlas Hub"))
        XCTAssertTrue(command.contains("$libexec/uninstall-macos-service.sh"))
        XCTAssertTrue(command.contains("/usr/bin/stat -f '%u:%g'"))
        XCTAssertTrue(command.contains("/bin/test"))
        XCTAssertFalse(command.contains("/usr/bin/test"))
        XCTAssertTrue(FileManager.default.isExecutableFile(atPath: "/bin/test"))
        XCTAssertTrue(command.contains("-perm +022"))
        XCTAssertTrue(command.hasSuffix("/bin/sh \"$uninstaller\" --delete-data"))
        XCTAssertFalse(command.contains("pkgutil --expand"))
        XCTAssertFalse(command.contains("TeslatlasHubService.pkg"))
        let syntaxCheck = Process()
        syntaxCheck.executableURL = URL(fileURLWithPath: "/bin/sh")
        syntaxCheck.arguments = ["-n", "-c", command]
        try syntaxCheck.run()
        syntaxCheck.waitUntilExit()
        XCTAssertEqual(syntaxCheck.terminationStatus, 0)
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
        let url = FileManager.default.temporaryDirectory
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
                                           manageTesla: { _ in })
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
    private var completions: [(Result<String, Error>) -> Void] = []
    private var pendingResults: [Result<String, Error>] = []

    init(loadState: HubServiceLoadState) {
        self.loadState = loadState
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        if pendingResults.isEmpty {
            completions.append(completion)
        } else {
            completion(pendingResults.removeFirst())
        }
    }

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

private final class VersionAwareRunner: HubCommandRunning {
    private let events: EventRecorder
    private let versionResult: Result<String, Error>
    private let commandResult: Result<String, Error>

    init(events: EventRecorder,
         versionResult: Result<String, Error>,
         commandResult: Result<String, Error> = .success("{}")) {
        self.events = events
        self.versionResult = versionResult
        self.commandResult = commandResult
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        if arguments == ["--version"] {
            events.append("version")
            completion(versionResult)
        } else {
            events.append(arguments.contains("setup") ? "setup" : "command")
            completion(commandResult)
        }
    }
}

private final class ScriptedInstaller: HubInstalling {
    private let events: EventRecorder
    private let result: Result<String, Error>
    private let onInstall: (() -> Void)?

    init(events: EventRecorder,
         result: Result<String, Error> = .success(""),
         onInstall: (() -> Void)? = nil) {
        self.events = events
        self.result = result
        self.onInstall = onInstall
    }

    func install(completion: @escaping (Result<String, Error>) -> Void) {
        events.append("install")
        onInstall?()
        completion(result)
    }

    func uninstall(deleteData: Bool, completion: @escaping (Result<String, Error>) -> Void) {
        completion(.success(""))
    }
}

private final class ScriptedService: HubServiceControlling {
    private let events: EventRecorder
    private let loadState: HubServiceLoadState
    var result: Result<String, Error> = .success("")

    init(events: EventRecorder, loadState: HubServiceLoadState = .unloaded) {
        self.events = events
        self.loadState = loadState
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        events.append("service:\(arguments.last ?? "unknown")")
        completion(result)
    }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        completion(loadState)
    }
}

private final class RecordingInstaller: HubInstalling {
    private(set) var installCalls = 0
    private(set) var deleteDataChoices: [Bool] = []

    func install(completion: @escaping (Result<String, Error>) -> Void) {
        installCalls += 1
        completion(.success(""))
    }

    func uninstall(deleteData: Bool, completion: @escaping (Result<String, Error>) -> Void) {
        deleteDataChoices.append(deleteData)
        completion(.success(""))
    }
}

private final class PendingInstaller: HubInstalling {
    private(set) var installCalls = 0
    private(set) var deleteDataChoices: [Bool] = []
    private var installCompletion: ((Result<String, Error>) -> Void)?
    private var uninstallCompletion: ((Result<String, Error>) -> Void)?

    func install(completion: @escaping (Result<String, Error>) -> Void) {
        installCalls += 1
        installCompletion = completion
    }

    func uninstall(deleteData: Bool, completion: @escaping (Result<String, Error>) -> Void) {
        deleteDataChoices.append(deleteData)
        uninstallCompletion = completion
    }

    func completeInstall(_ result: Result<String, Error>) {
        let completion = installCompletion
        installCompletion = nil
        completion?(result)
    }

    func completeUninstall(_ result: Result<String, Error>) {
        let completion = uninstallCompletion
        uninstallCompletion = nil
        completion?(result)
    }
}
