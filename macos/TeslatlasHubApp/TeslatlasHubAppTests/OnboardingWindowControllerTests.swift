// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import Darwin
import XCTest
@testable import Teslatlas_Hub

final class OnboardingWindowControllerTests: XCTestCase {
    func testStaleSSHSecretCleanupAdmitsOnlyOwnedUUIDDirectories() throws {
        let manager = FileManager.default
        let root = manager.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-cleanup-test-\(UUID().uuidString)", isDirectory: true)
        let eligible = root.appendingPathComponent(
            "th-\(UUID().uuidString)", isDirectory: true
        )
        let legacy = root.appendingPathComponent(
            "teslatlas-hub-import-\(UUID().uuidString)", isDirectory: true
        )
        let unrelated = root.appendingPathComponent("th-not-a-uuid", isDirectory: true)
        let outside = root.appendingPathComponent("outside", isDirectory: true)
        let linked = root.appendingPathComponent("th-\(UUID().uuidString)")
        try manager.createDirectory(at: eligible, withIntermediateDirectories: true)
        try manager.createDirectory(at: legacy, withIntermediateDirectories: true)
        try Data("secret".utf8).write(to: eligible.appendingPathComponent("ssh-password"))
        try manager.createDirectory(at: unrelated, withIntermediateDirectories: true)
        try manager.createDirectory(at: outside, withIntermediateDirectories: true)
        try manager.createSymbolicLink(at: linked, withDestinationURL: outside)
        defer { try? manager.removeItem(at: root) }

        XCTAssertEqual(TeslaMateServerImporter.cleanupStaleTemporaryDirectories(in: root), 2)
        XCTAssertFalse(manager.fileExists(atPath: eligible.path))
        XCTAssertFalse(manager.fileExists(atPath: legacy.path))
        XCTAssertTrue(manager.fileExists(atPath: unrelated.path))
        XCTAssertTrue(manager.fileExists(atPath: linked.path))
        XCTAssertTrue(manager.fileExists(atPath: outside.path))
    }

    func testStaleSSHSecretCleanupRecordsAnUnavailableTemporaryRoot() {
        let missingRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-missing-cleanup-\(UUID().uuidString)",
                                    isDirectory: true)

        XCTAssertEqual(
            TeslaMateServerImporter.cleanupStaleTemporaryDirectories(in: missingRoot),
            0
        )
        let diagnostics = HubAppLog.shared.recentText()
        XCTAssertTrue(diagnostics.contains("temporary_files.scan_failed"))
        XCTAssertTrue(diagnostics.contains("error_code=cocoa_"))
    }

    func testDashboardReportsInitialRefreshWithoutLeavingLaunchBlank() {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let refreshed = expectation(description: "initial dashboard refresh delivered")
        var delivered: HubSnapshot?
        let dashboard = MainWindowController(controller: controller) {
            delivered = $0
            refreshed.fulfill()
        }
        XCTAssertNotNil(dashboard.window)
        wait(for: [refreshed], timeout: 1)

        XCTAssertEqual(delivered?.account, "Connected")
    }

    func testFirstRunKeepsDashboardAndPresentsNonDismissibleOnboarding() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                       initialSnapshot: .firstRun)
        let dashboard = MainWindowController(controller: controller)
        let onboarding = try XCTUnwrap(dashboard.showFirstRunOnboarding())

        XCTAssertNotNil(dashboard.window)
        XCTAssertEqual(dashboard.activeModalKind, .onboarding)
        XCTAssertEqual(onboarding.dismissalPolicy, .firstRun)
        XCTAssertFalse(onboarding.windowShouldClose(try XCTUnwrap(onboarding.window)))
    }

    func testLaterOnboardingCanDismissOnlyWhileIdle() throws {
        let onboarding = OnboardingWindowController(
            controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
            initialRoute: .provider,
            dismissalPolicy: .accountManagement,
            onComplete: { _ in }
        )
        let window = try XCTUnwrap(onboarding.window)
        XCTAssertTrue(onboarding.windowShouldClose(window))
        onboarding.setBusy(true)
        XCTAssertFalse(onboarding.windowShouldClose(window))
    }

    func testResumedMigrationHandoverCannotClose() throws {
        let home = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-handover-close-\(UUID().uuidString)",
                                    isDirectory: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let state = home.appendingPathComponent("Library/Application Support/Teslatlas Hub",
                                                isDirectory: true)
        try FileManager.default.createDirectory(at: state, withIntermediateDirectories: true)
        try Data("pending".utf8).write(to: state.appendingPathComponent(".teslamate-handover-pending"))
        let controller = HubController(homeDirectory: home, serviceInstalledOverride: true)
        let onboarding = OnboardingWindowController(
            controller: controller,
            resumeMigrationHandoverPhase: .awaitingHandover,
            dismissalPolicy: .accountManagement,
            onComplete: { _ in }
        )
        let window = try XCTUnwrap(onboarding.window)

        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertFalse(onboarding.windowShouldClose(window))
        XCTAssertFalse(try XCTUnwrap(window.standardWindowButton(.closeButton)).isEnabled)
    }

    func testFirstRunReplacesDismissibleOnboardingWithNonDismissibleSheet() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                       initialSnapshot: .firstRun)
        let dashboard = MainWindowController(controller: controller)
        let account = try XCTUnwrap(dashboard.showOnboarding(route: .provider))
        let firstRun = try XCTUnwrap(dashboard.showFirstRunOnboarding())

        XCTAssertFalse(account === firstRun)
        XCTAssertEqual(firstRun.dismissalPolicy, .firstRun)
        XCTAssertFalse(firstRun.windowShouldClose(try XCTUnwrap(firstRun.window)))
        XCTAssertEqual(dashboard.activeModalKind, .onboarding)
        XCTAssertTrue(dashboard.accountWorkflowActive)
    }

    func testStaleOnboardingDismissalCannotClearReplacementModal() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                       initialSnapshot: .firstRun)
        let dashboard = MainWindowController(controller: controller)
        _ = try XCTUnwrap(dashboard.showOnboarding(route: .provider))
        let oldIdentifier = try XCTUnwrap(dashboard.activeOnboardingIdentifier)
        _ = try XCTUnwrap(dashboard.showFirstRunOnboarding())

        dashboard.handleOnboardingDismissal(identifier: oldIdentifier)

        XCTAssertEqual(dashboard.activeModalKind, .onboarding)
        XCTAssertTrue(dashboard.accountWorkflowActive)
    }

    func testOnboardingDismissalIsConsumedExactlyOnce() throws {
        let dashboard = MainWindowController(
            controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        )
        _ = try XCTUnwrap(dashboard.showOnboarding(route: .provider))
        let identifier = try XCTUnwrap(dashboard.activeOnboardingIdentifier)

        dashboard.handleOnboardingDismissal(identifier: identifier)
        dashboard.handleOnboardingDismissal(identifier: identifier)

        XCTAssertNil(dashboard.activeModalKind)
        XCTAssertFalse(dashboard.accountWorkflowActive)
    }

    func testClosingLastWindowTerminatesOnlyTheGUI() {
        XCTAssertTrue(AppDelegate().applicationShouldTerminateAfterLastWindowClosed(NSApp))
    }

    func testApplicationMenuProvidesStandardTextEditingCommands() throws {
        let delegate = AppDelegate()
        let menu = AppDelegate.makeMainMenu(actionTarget: delegate)
        let edit = try XCTUnwrap(menu.items.compactMap(\.submenu).first { $0.title == "Edit" })
        for title in ["Undo", "Redo", "Cut", "Copy", "Paste", "Delete", "Select All"] {
            XCTAssertNotNil(edit.item(withTitle: title), "Missing \(title)")
        }
        let selectAll = try XCTUnwrap(edit.item(withTitle: "Select All"))
        XCTAssertEqual(selectAll.keyEquivalent, "a")
        XCTAssertEqual(selectAll.keyEquivalentModifierMask, .command)
        let view = try XCTUnwrap(menu.items.compactMap(\.submenu).first { $0.title == "View" })
        let logs = try XCTUnwrap(view.item(withTitle: "Hub Logs"))
        XCTAssertEqual(logs.keyEquivalent, "l")
        XCTAssertEqual(logs.keyEquivalentModifierMask, .command)
        XCTAssertTrue(logs.target === delegate)
        let application = try XCTUnwrap(menu.items.first?.submenu)
        let legal = try XCTUnwrap(application.item(withTitle: "Legal & Licence…"))
        XCTAssertTrue(legal.target === delegate)
        XCTAssertEqual(legal.action, #selector(AppDelegate.showLegalNotice(_:)))
        XCTAssertTrue(AppDelegate.legalNoticeText.contains("AGPL-3.0-only"))
        XCTAssertTrue(AppDelegate.legalNoticeText.contains(
            "Teslatlas Hub — originally authored by György Bolyki and published by MAGRATHEAN UK LTD. Source: https://github.com/magrathean-uk/teslatlas-hub"
        ))
        let source = try XCTUnwrap(application.items.first {
            $0.title == "Corresponding Source for v\(HubRelease.bundledVersion)…"
        })
        XCTAssertTrue(source.target === delegate)
        XCTAssertEqual(source.action, #selector(AppDelegate.openCorrespondingSource(_:)))
    }

    func testCorrespondingSourceURLPreservesPublishedTagsAndPinsStableSource() throws {
        XCTAssertEqual(
            HubRelease.correspondingSourceURL(for: "1.0.0-beta.1")?.absoluteString,
            "https://github.com/magrathean-uk/teslatlas-hub/releases/tag/v1.0.0-beta.1"
        )
        XCTAssertEqual(
            HubRelease.correspondingSourceURL(for: "1.0.0")?.absoluteString,
            "https://github.com/magrathean-uk/teslatlas-hub/tree/v1.0.0"
        )
        XCTAssertNil(HubRelease.correspondingSourceURL(for: "1.0.0/../../main"))
        XCTAssertNil(HubRelease.correspondingSourceURL(for: "$(TESLATLAS_HUB_VERSION)"))
        XCTAssertEqual(
            HubRelease.licenceURL(for: "1.0.0-beta.1")?.absoluteString,
            "https://github.com/magrathean-uk/teslatlas-hub/blob/v1.0.0-beta.1/LICENSE"
        )
        XCTAssertEqual(
            HubRelease.licenceURL(for: "1.0.0")?.absoluteString,
            "https://github.com/magrathean-uk/teslatlas-hub/blob/v1.0.0/LICENSE"
        )
        XCTAssertNil(HubRelease.licenceURL(for: "1.0.0/../../main"))
    }

    func testAppDiagnosticsPersistBoundedShareSafeEvents() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-log-test-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("app.log")
        let log = HubAppLog(fileURL: file)
        log.record("ssh.failed", category: "teslamate_import", level: "ERROR", fields: [
            "password": "secret-value",
            "path": NSHomeDirectory() + "/.ssh/id_rsa"
        ])

        let text = log.recentText()
        XCTAssertTrue(text.contains("teslamate_import ssh.failed"))
        XCTAssertTrue(text.contains("password=[redacted]"))
        XCTAssertTrue(text.contains("path=~/.ssh/id_rsa"))
        XCTAssertFalse(text.contains("secret-value"))
        XCTAssertEqual(HubAppLog.errorCode(HubActionError.missingResource("secret")),
                       "missing_resource")
        XCTAssertEqual(HubAppLog.errorCode(TeslaAuthError.stateMismatch), "state_mismatch")
        XCTAssertEqual(HubAppLog.errorCode(TeslaAuthError.exchangeFailed), "exchange_failed")
        XCTAssertEqual(HubAppLog.errorCode(CocoaError(.fileNoSuchFile)), "cocoa_4")
        XCTAssertEqual(HubAppLog.errorCode(POSIXError(.EACCES)), "posix_13")
        let attributes = try FileManager.default.attributesOfItem(atPath: file.path)
        let permissions = try XCTUnwrap(attributes[.posixPermissions] as? NSNumber).intValue
        XCTAssertEqual(permissions & 0o777, 0o600)
    }

    func testSavedSupportReportsAreOwnerOnly() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-report-test-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let report = directory.appendingPathComponent("diagnostics.txt")
        try Data("old".utf8).write(to: report)
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: report.path)

        try HubAppLog.writePrivateReport("redacted report", to: report)

        XCTAssertEqual(try String(contentsOf: report, encoding: .utf8), "redacted report")
        let attributes = try FileManager.default.attributesOfItem(atPath: report.path)
        let permissions = try XCTUnwrap(attributes[.posixPermissions] as? NSNumber).intValue
        XCTAssertEqual(permissions & 0o777, 0o600)
    }

    func testSavedSupportReportsRefuseSymlinksAndFIFOs() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-report-safety-test-\(UUID().uuidString)",
                                    isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let target = directory.appendingPathComponent("target.txt")
        let linked = directory.appendingPathComponent("linked.txt")
        try Data("unchanged".utf8).write(to: target)
        try FileManager.default.createSymbolicLink(at: linked, withDestinationURL: target)

        XCTAssertThrowsError(try HubAppLog.writePrivateReport("private", to: linked))
        XCTAssertEqual(try String(contentsOf: target, encoding: .utf8), "unchanged")

        let fifo = directory.appendingPathComponent("report.fifo")
        XCTAssertEqual(Darwin.mkfifo(fifo.path, S_IRUSR | S_IWUSR), 0)
        let started = Date()
        XCTAssertThrowsError(try HubAppLog.writePrivateReport("private", to: fifo))
        XCTAssertLessThan(Date().timeIntervalSince(started), 1)
    }

    func testHostedXCTestLogsStayOutOfTheUserLogDirectory() {
        let home = URL(fileURLWithPath: "/Users/example", isDirectory: true)
        let temporary = URL(fileURLWithPath: "/private/var/tmp", isDirectory: true)
        let testURL = HubAppLog.defaultLogURL(
            environment: ["XCTestConfigurationFilePath": "/tmp/tests.xctestconfiguration"],
            homeDirectory: home,
            temporaryDirectory: temporary,
            processIdentifier: 42
        )
        let productionURL = HubAppLog.defaultLogURL(
            environment: [:],
            homeDirectory: home,
            temporaryDirectory: temporary,
            processIdentifier: 42
        )

        XCTAssertEqual(testURL.path, "/private/var/tmp/TeslatlasHubTests-42/app.log")
        XCTAssertEqual(productionURL.path, "/Users/example/Library/Logs/Teslatlas Hub/app.log")
    }

    func testAppDiagnosticsBoundLargeEventsAndRotateWithoutReadingAnUnboundedFile() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-log-bound-test-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("app.log")
        let log = HubAppLog(fileURL: file)
        let payload = String(repeating: "x", count: 64 * 1024)

        for index in 0..<80 {
            log.record("large.event", category: "test", fields: ["index": String(index), "value": payload])
        }

        let size = try XCTUnwrap(
            FileManager.default.attributesOfItem(atPath: file.path)[.size] as? NSNumber
        ).intValue
        XCTAssertLessThanOrEqual(size, 1024 * 1024)
        XCTAssertLessThanOrEqual(Data(log.recentText(maximumBytes: 4096).utf8).count, 4096)
        XCTAssertTrue(log.recentText().contains("[truncated]"))
    }

    func testAppDiagnosticsRefuseAReplacedSymlink() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-log-link-test-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let target = directory.appendingPathComponent("target")
        let file = directory.appendingPathComponent("app.log")
        try Data("unchanged".utf8).write(to: target)
        try FileManager.default.createSymbolicLink(at: file, withDestinationURL: target)

        let log = HubAppLog(fileURL: file)
        log.record("must.not.follow", category: "test")

        XCTAssertEqual(try String(contentsOf: target, encoding: .utf8), "unchanged")
        XCTAssertEqual(log.recentText(), HubAppLog.unavailableText)
    }

    func testAppDiagnosticsRefuseASymlinkedDirectory() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-log-directory-test-\(UUID().uuidString)",
                                    isDirectory: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let target = root.appendingPathComponent("target", isDirectory: true)
        let linked = root.appendingPathComponent("linked", isDirectory: true)
        try FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
        try FileManager.default.createSymbolicLink(at: linked, withDestinationURL: target)

        let log = HubAppLog(fileURL: linked.appendingPathComponent("app.log"))
        log.record("must.not.follow.directory", category: "test")

        XCTAssertFalse(FileManager.default.fileExists(
            atPath: target.appendingPathComponent("app.log").path
        ))
        XCTAssertEqual(log.recentText(), HubAppLog.unavailableText)
    }

    func testAppAndServiceDiagnosticsRejectAFIFOWithoutBlocking() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-log-fifo-test-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let file = directory.appendingPathComponent("app.log")
        XCTAssertEqual(Darwin.mkfifo(file.path, S_IRUSR | S_IWUSR), 0)

        let log = HubAppLog(fileURL: file)
        let started = Date()
        log.record("must.not.block", category: "test")
        XCTAssertEqual(log.recentText(), HubAppLog.unavailableText)
        XCTAssertNil(HubController.logTail(of: file, maximumBytes: 4096))
        XCTAssertLessThan(Date().timeIntervalSince(started), 1)
    }

    func testStateBranchesByPathAndProviderAndBackTracks() {
        var state = HubOnboardingState()
        XCTAssertEqual(state.route, .welcome)
        XCTAssertEqual(state.step, 1)

        state.advance()
        XCTAssertEqual(state.route, .choose)
        XCTAssertEqual(state.step, 2)
        state.advance()
        XCTAssertEqual(state.route, .provider)
        XCTAssertEqual(state.step, 3)

        state.provider = .legacy
        state.advance()
        XCTAssertEqual(state.route, .legacy)
        state.back()
        XCTAssertEqual(state.route, .provider)

        state.provider = .fleet
        state.advance()
        XCTAssertEqual(state.route, .fleet)
        state.back()
        XCTAssertEqual(state.route, .provider)
        state.back()
        XCTAssertEqual(state.route, .choose)
        state.back()
        XCTAssertEqual(state.route, .welcome)
    }

    func testMigrationBranchUsesStepThreeAndBackReturnsToChoice() {
        var state = HubOnboardingState(route: .choose, path: .migration, provider: .fleet)
        state.advance()
        XCTAssertEqual(state.route, .migration)
        XCTAssertEqual(state.step, 3)
        state.back()
        XCTAssertEqual(state.route, .choose)
    }

    func testInterruptedMigrationReturnsToImportInsteadOfVerification() {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     resumeMigrationHandoverPhase: .importing,
                                                     onComplete: { _ in })

        XCTAssertTrue(buttons(in: onboarding.window?.contentView)
            .contains { $0.title == "Connect to Server" })
        XCTAssertTrue(labels(in: onboarding.window?.contentView)
            .contains { $0.stringValue.contains("previous import did not finish") })
        XCTAssertFalse(labels(in: onboarding.window?.contentView)
            .contains { $0.stringValue == "Checking your setup…" })
    }

    func testVisibleOnboardingCanNavigateToRequestedAccountRoute() {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     initialRoute: .provider,
                                                     onComplete: { _ in })
        XCTAssertEqual(onboarding.currentRoute, .provider)

        onboarding.navigate(to: .legacy)
        XCTAssertEqual(onboarding.currentRoute, .legacy)
        XCTAssertTrue(labels(in: onboarding.window?.contentView)
            .contains { $0.stringValue == "Connect with a token" })

        onboarding.navigate(to: .migration)
        XCTAssertEqual(onboarding.currentRoute, .migration)
        XCTAssertTrue(buttons(in: onboarding.window?.contentView)
            .contains { $0.title == "Connect to Server" })
    }

    func testBusyOnboardingCannotCloseOrChangeAccountRoute() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     initialRoute: .legacy,
                                                     dismissalPolicy: .accountManagement,
                                                     onComplete: { _ in })
        let window = try XCTUnwrap(onboarding.window)

        onboarding.setBusy(true)
        XCTAssertFalse(onboarding.windowShouldClose(window))
        XCTAssertFalse(try XCTUnwrap(window.standardWindowButton(.closeButton)).isEnabled)
        onboarding.navigate(to: .migration)
        XCTAssertEqual(onboarding.currentRoute, .legacy)

        onboarding.setBusy(false)
        XCTAssertTrue(onboarding.windowShouldClose(window))
        XCTAssertTrue(try XCTUnwrap(window.standardWindowButton(.closeButton)).isEnabled)

        XCTAssertFalse(OnboardingWindowController.routeChangeAllowed(
            busy: false,
            authenticationActive: true,
            migrationHandoverPending: false
        ))
        XCTAssertFalse(OnboardingWindowController.routeChangeAllowed(
            busy: false,
            authenticationActive: false,
            migrationHandoverPending: true
        ))
        XCTAssertTrue(OnboardingWindowController.routeChangeAllowed(
            busy: false,
            authenticationActive: false,
            migrationHandoverPending: false
        ))
    }

    func testAccountWorkflowDisablesDashboardMutationsUntilDismissed() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let dashboard = MainWindowController(controller: controller)
        dashboard.detailsButton.performClick(nil)
        let details = try XCTUnwrap(dashboard.detailsWindow)
        let detailsMutations = buttons(in: details.window?.contentView).filter {
            ["Update Service…", "Uninstall Hub…"].contains($0.title)
        }
        XCTAssertEqual(detailsMutations.count, 2)
        XCTAssertTrue(detailsMutations.allSatisfy(\.isEnabled))

        let onboarding = try XCTUnwrap(dashboard.showOnboarding(route: .legacy))

        XCTAssertTrue(dashboard.accountWorkflowActive)
        XCTAssertFalse(dashboard.connectButton.isEnabled)
        XCTAssertFalse(dashboard.importButton.isEnabled)
        XCTAssertFalse(dashboard.detailsButton.isEnabled)
        XCTAssertTrue(detailsMutations.allSatisfy { !$0.isEnabled })
        for title in ["Stop Hub…", "Start Climate", "Wake", "Lock"] {
            let button = try XCTUnwrap(buttons(in: dashboard.window?.contentView)
                .first { $0.title == title })
            XCTAssertFalse(button.isEnabled, "\(title) remained active during account setup")
        }

        onboarding.close()
        XCTAssertFalse(dashboard.accountWorkflowActive)
        XCTAssertTrue(dashboard.connectButton.isEnabled)
        XCTAssertTrue(dashboard.importButton.isEnabled)
        XCTAssertTrue(dashboard.detailsButton.isEnabled)
        XCTAssertTrue(detailsMutations.allSatisfy(\.isEnabled))

        let refreshed = expectation(description: "dashboard actions restored")
        DispatchQueue.main.async {
            let climate = self.buttons(in: dashboard.window?.contentView)
                .first { $0.title == "Start Climate" }
            XCTAssertTrue(climate?.isEnabled ?? false)
            refreshed.fulfill()
        }
        wait(for: [refreshed], timeout: 1)
    }

    func testPermanentDataDeletionConfirmationDefaultsToCancel() {
        let alert = ServiceDetailsWindowController.deleteDataConfirmation()
        XCTAssertEqual(alert.buttons.map(\.title), ["Cancel", "Delete Data and Uninstall"])
        XCTAssertEqual(alert.buttons[0].keyEquivalent, "\r")
        XCTAssertEqual(alert.buttons[1].keyEquivalent, "")
    }

    func testChoosePageShowsMigrationCopyAndIconWhenSelected() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller, onComplete: { _ in })
        let continueButton = try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "Continue" })
        continueButton.performClick(nil)
        XCTAssertNil(continueButton.image)
        XCTAssertNotNil(try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "Back" }).image)

        let migration = try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "Migrate from TeslaMate" })
        XCTAssertFalse(migration.isHidden)
        XCTAssertEqual(migration.toolTip,
                       "Import your existing vehicle history from a TeslaMate server over SSH.")
        XCTAssertFalse(labels(in: onboarding.window?.contentView)
            .contains { $0.stringValue.contains("Exact TeslaMate") })
        let fresh = try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "New installation" })
        XCTAssertEqual(fresh.toolTip,
                       "Connect a Tesla account and start collecting data with a clean database.")

        migration.performClick(nil)
        let back = try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "Back" })
        back.performClick(nil)
        XCTAssertTrue(labels(in: onboarding.window?.contentView)
            .contains { $0.stringValue == "How would you like to start?" })
    }

    func testWelcomeUsesCompactSourceComposition() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     previewRoute: "welcome",
                                                     onComplete: { _ in })
        let view = try XCTUnwrap(onboarding.window?.contentView)
        view.layoutSubtreeIfNeeded()
        let text = labels(in: view).map(\.stringValue)
        XCTAssertEqual(view.bounds.size, NSSize(width: 485, height: 282))
        XCTAssertTrue(text.contains("Teslatlas Hub"))
        XCTAssertTrue(text.contains(
            "Your own Tesla telemetry collector, running privately on this Mac."
        ))
        XCTAssertTrue(text.contains("Written in Rust for a small, fast, single binary"))
        XCTAssertTrue(text.contains("No Docker required — runs as a native service"))
        XCTAssertTrue(text.contains("First-class on macOS and Debian Linux"))
        XCTAssertTrue(text.contains("Stores vehicle data in a local SQLite database"))
        XCTAssertFalse(text.contains("Written purely in Rust."))
        XCTAssertFalse(text.contains("No Docker."))
        XCTAssertFalse(text.contains("Developed natively for macOS and Debian."))
        XCTAssertFalse(text.contains("Uses SQLite."))

        let title = try XCTUnwrap(labels(in: view).first { $0.stringValue == "Teslatlas Hub" })
        let subtitle = try XCTUnwrap(labels(in: view).first {
            $0.stringValue == "Your own Tesla telemetry collector, running privately on this Mac."
        })
        XCTAssertEqual(title.alignment, .left)
        XCTAssertEqual(title.font?.pointSize, 17.5)
        XCTAssertEqual(subtitle.alignment, .left)
        XCTAssertEqual(subtitle.font?.pointSize, 12)

        let featureIcons = imageViews(in: view).filter {
            $0.identifier?.rawValue == "onboarding.welcome.feature-icon"
        }
        XCTAssertEqual(featureIcons.count, 4)
        XCTAssertTrue(
            featureIcons.allSatisfy {
                abs($0.frame.width - 16) <= 1 && $0.frame.height <= 25
            },
            "Unexpected feature icon frames: \(featureIcons.map(\.frame))"
        )

        let visibleButtons = buttons(in: view).filter { !$0.isHidden }
        XCTAssertEqual(visibleButtons.map(\.title), ["Continue"])
        let continueButton = try XCTUnwrap(visibleButtons
            .first { $0.title == "Continue" })
        XCTAssertNil(continueButton.image)
        XCTAssertEqual(continueButton.controlSize, .regular)
        XCTAssertLessThan(continueButton.frame.width, 100)
    }

    func testWelcomeHeaderAndFooterUseTranslatedChromeGeometry() throws {
        let onboarding = OnboardingWindowController(
            controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
            previewRoute: "welcome",
            onComplete: { _ in }
        )
        let root = try XCTUnwrap(onboarding.window?.contentView)
        root.layoutSubtreeIfNeeded()
        let header = try XCTUnwrap(view(in: root, identifier: "onboarding.header"))
        let body = try XCTUnwrap(view(in: root, identifier: "onboarding.welcome.body"))
        let footer = try XCTUnwrap(view(in: root, identifier: "onboarding.footer"))
        let marks = allViews(in: root).filter {
            $0.identifier?.rawValue == "onboarding.progress-mark"
        }

        XCTAssertEqual(header.frame.height, 38, accuracy: 0.5)
        XCTAssertEqual(body.frame.minX, 28, accuracy: 0.5)
        XCTAssertEqual(footer.frame.height, 48, accuracy: 0.5)
        XCTAssertEqual(marks.count, 5)
        XCTAssertEqual(marks.first?.frame.size, NSSize(width: 20, height: 7))
        XCTAssertTrue(marks.dropFirst().allSatisfy {
            $0.frame.size == NSSize(width: 7, height: 7)
        })
    }

    func testSourceCompositionPropagatesSharedChromeAndRouteSpecificSheetHeights() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let expectedHeights: [(String, CGFloat)] = [
            ("welcome", 282),
            ("choose", 350),
            ("provider", 350),
            ("fleet", 455),
            ("legacy", 390),
            ("migration", 400),
            ("verify", 498),
            ("finish", 285)
        ]

        for (route, expectedHeight) in expectedHeights {
            let onboarding = OnboardingWindowController(
                controller: controller,
                previewRoute: route,
                onComplete: { _ in }
            )
            let root = try XCTUnwrap(onboarding.window?.contentView)
            root.layoutSubtreeIfNeeded()

            XCTAssertEqual(root.bounds.width, 485, accuracy: 0.5, route)
            XCTAssertEqual(root.bounds.height, expectedHeight, accuracy: 0.5, route)
            XCTAssertEqual(
                try XCTUnwrap(view(in: root, identifier: "onboarding.header")).frame.height,
                38,
                accuracy: 0.5,
                route
            )
            XCTAssertEqual(
                try XCTUnwrap(view(in: root, identifier: "onboarding.footer")).frame.height,
                48,
                accuracy: 0.5,
                route
            )
            XCTAssertNil(imageViews(in: root).first { image in
                image.identifier?.rawValue != "onboarding.progress-mark"
                    && image.identifier?.rawValue != "onboarding.welcome.feature-icon"
                    && image.accessibilityLabel() == "Teslatlas Hub"
            }, route)
        }
    }

    func testChooseUsesExactCopyAndTwoFullWidthVerticalSelectionCards() throws {
        let onboarding = OnboardingWindowController(
            controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
            previewRoute: "choose",
            onComplete: { _ in }
        )
        let root = try XCTUnwrap(onboarding.window?.contentView)
        root.layoutSubtreeIfNeeded()
        let text = labels(in: root).map(\.stringValue)

        XCTAssertTrue(text.contains("How would you like to start?"))
        XCTAssertTrue(text.contains("Set up a fresh Hub or bring your history over from TeslaMate."))
        XCTAssertTrue(text.contains(
            "Connect a Tesla account and start collecting data with a clean database."
        ))
        XCTAssertTrue(text.contains(
            "Import your existing vehicle history from a TeslaMate server over SSH."
        ))
        XCTAssertTrue(text.contains("Select an option to continue"))

        let fresh = try XCTUnwrap(buttons(in: root).first { $0.title == "New installation" })
        let migration = try XCTUnwrap(buttons(in: root).first { $0.title == "Migrate from TeslaMate" })
        XCTAssertGreaterThan(fresh.frame.width, 400)
        XCTAssertEqual(fresh.frame.width, migration.frame.width, accuracy: 0.5)
        XCTAssertNotEqual(fresh.superview?.frame.minY, migration.superview?.frame.minY)
        XCTAssertFalse(buttons(in: root).contains { $0.title == "Continue" && !$0.isHidden })
    }

    func testMigrationFormUsesCompactSourceFieldsAndFooterConnectionGate() throws {
        let onboarding = OnboardingWindowController(
            controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
            previewRoute: "migration",
            onComplete: { _ in }
        )
        let root = try XCTUnwrap(onboarding.window?.contentView)
        root.layoutSubtreeIfNeeded()
        let text = labels(in: root).map(\.stringValue)

        XCTAssertTrue(text.contains("Migrate from TeslaMate"))
        XCTAssertTrue(text.contains(
            "Connect to your TeslaMate server to import its vehicle history."
        ))
        XCTAssertTrue(buttons(in: root).contains {
            $0.title == "This user needs sudo to read the TeslaMate database"
        })
        XCTAssertEqual(popups(in: root).first?.itemTitles, ["SSH key", "Password"])
        XCTAssertTrue(popups(in: root).allSatisfy { $0.controlSize != .large })
        let inputFields = labels(in: root).filter { $0.isEditable }
        XCTAssertTrue(inputFields.allSatisfy { $0.controlSize != .large })
        let connect = try XCTUnwrap(buttons(in: root).first { $0.title == "Connect to Server" })
        XCTAssertFalse(connect.isEnabled)
        XCTAssertTrue(view(in: root, identifier: "onboarding.footer")?.isDescendant(of: connect) == false)
        XCTAssertTrue(connect.isDescendant(of: try XCTUnwrap(view(in: root, identifier: "onboarding.footer"))))
    }

    func testPreviewFixturesRenderConnectedVerifyAndMigrationFinishSourceStatesWithoutSecrets() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let connected = OnboardingWindowController(
            controller: controller,
            previewRoute: "migration-connected",
            onComplete: { _ in }
        )
        let verify = OnboardingWindowController(
            controller: controller,
            previewRoute: "verify",
            onComplete: { _ in }
        )
        let finish = OnboardingWindowController(
            controller: controller,
            previewRoute: "finish-migration",
            onComplete: { _ in }
        )

        let connectedText = labels(in: connected.window?.contentView).map(\.stringValue)
        XCTAssertTrue(connectedText.contains("Connected to teslamate.local"))
        XCTAssertTrue(connectedText.contains("Found a TeslaMate database ready to import."))
        XCTAssertTrue(buttons(in: connected.window?.contentView).contains {
            $0.title == "I confirm this server runs TeslaMate 4.2.0 or newer"
        })
        XCTAssertTrue(buttons(in: connected.window?.contentView).contains { $0.title == "Import Data" })

        let verifyText = labels(in: verify.window?.contentView).map(\.stringValue)
        XCTAssertTrue(verifyText.contains("Checking your Hub"))
        XCTAssertTrue(verifyText.contains("Making sure everything is wired up correctly."))
        XCTAssertEqual(
            allViews(in: try XCTUnwrap(verify.window?.contentView)).filter {
                $0.identifier?.rawValue == "onboarding.verify.row"
            }.count,
            6
        )
        XCTAssertTrue(buttons(in: verify.window?.contentView).contains { $0.title == "View Logs" })
        XCTAssertTrue(buttons(in: verify.window?.contentView).contains { $0.title == "Continue" })

        let finishText = labels(in: finish.window?.contentView).map(\.stringValue)
        XCTAssertTrue(finishText.contains("Migration complete"))
        XCTAssertTrue(finishText.contains("Your TeslaMate history has been imported into Hub."))
        XCTAssertTrue(buttons(in: finish.window?.contentView).contains {
            $0.title == "I have disabled Tesla access in TeslaMate to avoid duplicate requests"
        })
        XCTAssertFalse(try XCTUnwrap(buttons(in: finish.window?.contentView)
            .first { $0.title == "Start Hub" }).isEnabled)

        let combined = (connectedText + verifyText + finishText).joined(separator: " ")
        XCTAssertFalse(combined.contains("authorized-access-token"))
        XCTAssertFalse(combined.contains("authorized-refresh-token"))
    }

    func testMigrationOffersNormalUserKeyPasswordAndPasswordlessSudo() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     previewRoute: "migration",
                                                     onComplete: { _ in })
        let view = onboarding.window?.contentView
        let text = labels(in: view).map(\.stringValue)
        XCTAssertTrue(text.contains("user"))
        XCTAssertFalse(text.contains {
            $0.localizedCaseInsensitiveContains("normal server account")
        })
        XCTAssertFalse(buttons(in: view).contains { $0.title == "Choose Key…" })
        let sudo = try XCTUnwrap(buttons(in: view)
            .first { $0.title == "This user needs sudo to read the TeslaMate database" })
        XCTAssertEqual(sudo.state, .off)
        let connect = try XCTUnwrap(buttons(in: view).first { $0.title == "Connect to Server" })
        XCTAssertEqual(connect.contentTintColor, .white)
        XCTAssertNil(connect.image)

        let authentication = try XCTUnwrap(popups(in: view).first {
            $0.itemTitles == ["SSH key", "Password"]
        })
        authentication.selectItem(withTitle: "Password")
        _ = NSApp.sendAction(authentication.action!, to: authentication.target, from: authentication)

        let updatedView = onboarding.window?.contentView
        XCTAssertTrue(labels(in: updatedView).contains { $0.stringValue == "Password" })
        XCTAssertTrue(secureFields(in: updatedView).contains { $0.placeholderString == "SSH password" })
        XCTAssertFalse(buttons(in: updatedView).contains { $0.title == "Choose Key…" })
    }

    func testImportBusyStateShowsOnlyOneHeadingAndDeterminateProgress() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     previewRoute: "migration",
                                                     onComplete: { _ in })

        onboarding.setBusy(true, message: "Importing data…")

        let view = onboarding.window?.contentView
        let text = labels(in: view).map(\.stringValue)
        XCTAssertEqual(text.filter { $0 == "Importing data…" }.count, 1)
        XCTAssertFalse(text.contains("Import from TeslaMate"))
        XCTAssertTrue(text.contains { $0.hasPrefix("Step ") })
        XCTAssertFalse(text.contains {
            $0.localizedCaseInsensitiveContains("Enter your TeslaMate")
                || $0.localizedCaseInsensitiveContains("Elapsed")
                || $0.localizedCaseInsensitiveContains("Hub stays stopped")
        })
        let bars = progressIndicators(in: view).filter { $0.style == .bar }
        XCTAssertEqual(bars.count, 1)
        let progressBar = try XCTUnwrap(bars.first)
        XCTAssertFalse(progressBar.isIndeterminate)
        let progress = try XCTUnwrap(HubController.parseMigrationProgress(
            #"{"event":"migration_progress","completedRows":25,"totalRows":100}"#
        ))
        onboarding.updateMigrationProgress(progress)
        XCTAssertEqual(progressBar.maxValue, 100)
        XCTAssertEqual(progressBar.doubleValue, 25)
        onboarding.updateMigrationProgress(HubMigrationProgress(
            completedRows: 125,
            totalRows: 100,
            phase: nil
        ))
        XCTAssertEqual(progressBar.doubleValue, 100)
        onboarding.updateMigrationProgress(HubMigrationProgress(
            completedRows: 10,
            totalRows: 100,
            phase: nil
        ))
        XCTAssertEqual(progressBar.doubleValue, 100)
        XCTAssertFalse(text.contains("Server"))
        XCTAssertFalse(buttons(in: view).contains {
            $0.title == "Connect to Server" && !$0.isHidden
        })
        XCTAssertGreaterThanOrEqual(buttons(in: view).count, 2,
                                      "Focused import retains the shared footer controls")
    }

    func testNewInstallationBusyStateShowsOnlyCleanSetupFlow() {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     initialRoute: .legacy,
                                                     onComplete: { _ in })

        onboarding.setBusy(true, message: "Setting up Hub…")

        let view = onboarding.window?.contentView
        let text = labels(in: view).map(\.stringValue)
        XCTAssertEqual(text.filter { $0 == "Setting up Hub…" }.count, 1)
        XCTAssertFalse(text.contains("Connect with a Legacy Token"))
        XCTAssertFalse(text.contains("Access token"))
        XCTAssertTrue(text.contains { $0.hasPrefix("Step ") })
        XCTAssertEqual(progressIndicators(in: view).filter { $0.style == .spinning }.count, 1)
        XCTAssertGreaterThanOrEqual(buttons(in: view).count, 2,
                                      "Focused setup retains the shared footer controls")
    }

    func testMigrationUsesExplicitKeyboardNavigationOrder() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     previewRoute: "migration",
                                                     onComplete: { _ in })
        let view = onboarding.window?.contentView
        let server = try XCTUnwrap(labels(in: view).first {
            $0.placeholderString == "teslamate.local"
        })
        let user = try XCTUnwrap(labels(in: view).first { $0.stringValue == "user" })
        let port = try XCTUnwrap(labels(in: view).first { $0.stringValue == "22" })
        let authentication = try XCTUnwrap(popups(in: view).first {
            $0.itemTitles == ["SSH key", "Password"]
        })
        XCTAssertTrue(server.nextKeyView === user)
        XCTAssertTrue(user.nextKeyView === port)
        XCTAssertTrue(port.nextKeyView === authentication)
        XCTAssertTrue(onboarding.window?.firstResponder === server.currentEditor())

        authentication.selectItem(withTitle: "Password")
        _ = NSApp.sendAction(authentication.action!, to: authentication.target, from: authentication)
        let password = try XCTUnwrap(secureFields(in: onboarding.window?.contentView)
            .first { $0.placeholderString == "SSH password" })
        XCTAssertTrue(authentication.nextKeyView === password)
    }

    func testMigrationFormLocksWhileConnecting() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     previewRoute: "migration",
                                                     onComplete: { _ in })
        let view = onboarding.window?.contentView
        let server = try XCTUnwrap(labels(in: view).first {
            $0.placeholderString == "teslamate.local"
        })
        let authentication = try XCTUnwrap(popups(in: view).first {
            $0.itemTitles == ["SSH key", "Password"]
        })

        onboarding.setBusy(true, message: "Connecting…")
        let connect = try XCTUnwrap(buttons(in: view).first { $0.title == "Connecting…" })
        XCTAssertFalse(server.isEnabled)
        XCTAssertFalse(authentication.isEnabled)
        XCTAssertFalse(connect.isEnabled)

        onboarding.setBusy(false)
        XCTAssertTrue(server.isEnabled)
        XCTAssertTrue(authentication.isEnabled)
        XCTAssertEqual(connect.title, "Connect to Server")
    }

    func testTunnelFailuresAreSafeAndActionable() {
        XCTAssertEqual(
            TeslaMateServerImporter.classifyVersionForTests(
                image: "teslamate/teslamate:v4.1.1", labelVersion: nil
            ),
            .tooOld("4.1.1")
        )
        XCTAssertEqual(
            TeslaMateServerImporter.classifyVersionForTests(
                image: "docker.io/teslamate/teslamate:4.2.0", labelVersion: nil
            ),
            .supported("4.2.0")
        )
        XCTAssertEqual(
            TeslaMateServerImporter.classifyVersionForTests(
                image: "teslamate/teslamate:4.2", labelVersion: nil
            ),
            .supported("4.2")
        )
        XCTAssertEqual(
            TeslaMateServerImporter.classifyVersionForTests(
                image: "teslamate/teslamate:4.3.2", labelVersion: nil
            ),
            .supported("4.3.2")
        )
        XCTAssertEqual(
            TeslaMateServerImporter.classifyVersionForTests(
                image: "custom/teslamate:4.2.0", labelVersion: "4.2.0"
            ),
            .unknown
        )
        XCTAssertEqual(
            TeslaMateServerImporter.classifyVersionForTests(
                image: "teslamate/teslamate:latest", labelVersion: "4.2.0"
            ),
            .unknown
        )
        XCTAssertEqual(
            TeslaMateServerImporter.classifyVersionForTests(
                image: "teslamate/teslamate:4.2.0", labelVersion: "4.1.1"
            ),
            .tooOld("4.1.1")
        )

        let isolation = TeslaMateServerImporter.sshIsolationArgumentsForTests.joined(separator: " ")
        XCTAssertTrue(isolation.contains("ControlMaster=no"))
        XCTAssertTrue(isolation.contains("ControlPath=none"))
        XCTAssertTrue(isolation.contains("ControlPersist=no"))
        XCTAssertTrue(isolation.contains("ForkAfterAuthentication=no"))
        XCTAssertTrue(isolation.contains("RemoteCommand=none"))
        XCTAssertTrue(isolation.contains("StrictHostKeyChecking=yes"))
        XCTAssertFalse(isolation.contains("StrictHostKeyChecking=accept-new"))
        let ownedTunnel = TeslaMateServerImporter.ownedTunnelArgumentsForTests(
            controlPath: "/tmp/owned-control"
        ).joined(separator: " ")
        XCTAssertTrue(ownedTunnel.contains("ControlMaster=yes"))
        XCTAssertTrue(ownedTunnel.contains("ControlPath=/tmp/owned-control"))
        XCTAssertTrue(ownedTunnel.contains("ControlPersist=no"))
        XCTAssertTrue(ownedTunnel.contains("StrictHostKeyChecking=yes"))
        XCTAssertFalse(ownedTunnel.contains("StrictHostKeyChecking=accept-new"))

        let hostKeyDiagnostic = TeslaMateServerImporter.connectionDiagnostic(
            for: HubActionError.commandExited(255, "Host key verification failed"),
            authentication: .password("unused")
        )
        XCTAssertEqual(hostKeyDiagnostic.reasonCode, "host_key_failed")
        XCTAssertEqual(hostKeyDiagnostic.title, "Server identity needs verification")
        XCTAssertEqual(
            hostKeyDiagnostic.suggestions,
            ["Verify the server fingerprint through a trusted channel, then add or update its entry in ~/.ssh/known_hosts."]
        )

        let keyDiagnostic = TeslaMateServerImporter.connectionDiagnostic(
            for: HubActionError.commandExited(255, "Permission denied"),
            authentication: .key(identityFile: nil)
        )
        XCTAssertEqual(keyDiagnostic.reasonCode, "authentication_failed")
        XCTAssertEqual(keyDiagnostic.recoveryActions, [.chooseKey, .usePassword, .openLogs])
        XCTAssertFalse(keyDiagnostic.safeReport.contains("Permission denied"))

        XCTAssertEqual(
            TeslaMateServerImporter.tunnelFailureMessage("open failed: administratively prohibited"),
            "The SSH server does not permit database forwarding. Enable TCP forwarding for this account."
        )
        XCTAssertEqual(
            TeslaMateServerImporter.tunnelFailureMessage("Permission denied for secret-host"),
            "SSH authentication failed while opening the database tunnel."
        )
        XCTAssertFalse(
            TeslaMateServerImporter.tunnelFailureMessage("unexpected secret-host diagnostic")
                .contains("secret-host")
        )
        XCTAssertEqual(
            TeslaMateServerImporter.discoveryFailureReason(
                HubActionError.commandExited(255, "ssh: connect to private-host failed")
            ),
            "ssh_authentication_or_connection"
        )
        XCTAssertEqual(
            TeslaMateServerImporter.discoveryFailureMessage(
                HubActionError.commandExited(22, "private container diagnostic")
            ),
            "The TeslaMate database container is not running or could not be found."
        )
        XCTAssertFalse(
            TeslaMateServerImporter.discoveryFailureMessage(
                HubActionError.commandExited(27, "unexpected secret-host diagnostic")
            ).contains("secret-host")
        )
        XCTAssertEqual(
            TeslaMateServerImporter.discoveryFailureReason(
                HubActionError.commandExited(27, "multiple instances")
            ),
            "multiple_teslamate_instances"
        )
        XCTAssertEqual(
            TeslaMateServerImporter.discoveryFailureReason(
                HubActionError.commandExited(28, "multiple databases")
            ),
            "multiple_database_instances"
        )
        XCTAssertEqual(
            TeslaMateServerImporter.discoveryFailureMessage(
                HubActionError.commandExited(255, "ssh: connect to private-host port 40022: Connection refused")
            ),
            "The SSH server refused the connection. Check that SSH is running and the port is correct."
        )
        XCTAssertEqual(
            TeslaMateServerImporter.discoveryFailureReason(
                HubActionError.commandExited(1, "sudo: a password is required")
            ),
            "passwordless_sudo_required"
        )
        XCTAssertTrue(
            TeslaMateServerImporter.discoveryFailureMessage(
                HubActionError.commandExited(1, "permission denied while trying to connect to /var/run/docker.sock")
            ).contains("cannot access Docker")
        )
        XCTAssertEqual(
            TeslaMateServerImporter.discoveryFailureReason(
                HubActionError.commandExited(127, "sh: docker: not found")
            ),
            "docker_missing"
        )
        XCTAssertFalse(
            TeslaMateServerImporter.discoveryFailureMessage(
                HubActionError.commandExited(1, "sudo: private-host is not in the sudoers file")
            ).contains("private-host")
        )
        XCTAssertEqual(
            TeslaMateServerImporter.tunnelFailureMessage(
                "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED for secret-host"
            ),
            "SSH host identity verification failed. Verify or update this server in your SSH known-hosts file."
        )
        XCTAssertFalse(
            TeslaMateServerImporter.tunnelFailureMessage(
                "ssh: Could not resolve hostname secret-host: nodename nor servname provided"
            ).contains("secret-host")
        )
        XCTAssertFalse(
            TeslaMateServerImporter.tunnelFailureMessage(
                "unix_listener: path /private/tmp/private-socket too long for Unix domain socket"
            ).contains("private-socket")
        )
    }

    func testLogsLineNumbersArePresentationOnly() {
        XCTAssertEqual(LogsWindowController.numberedPresentation("alpha\nbeta\n"),
                       "01  alpha\n02  beta")
    }

    func testCommandLLogSheetRetainsRefreshDiagnosticsCopySaveAndPrivacy() {
        let logs = LogsWindowController(controller: HubController(
            environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]
        ))
        let titles = buttons(in: logs.window?.contentView).map(\.title)
        XCTAssertTrue(Set(["Refresh", "Run Diagnostics", "Copy", "Save…"])
            .isSubset(of: Set(titles)))
        XCTAssertTrue(labels(in: logs.window?.contentView).contains {
            $0.stringValue.contains("redact credentials")
        })
    }

    func testCommandLLogWindowOffersFullDiagnosticsAndSharing() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let logs = LogsWindowController(controller: controller)
        let titles = buttons(in: logs.window?.contentView).map(\.title)

        XCTAssertTrue(titles.contains("Refresh"))
        XCTAssertTrue(titles.contains("Run Diagnostics"))
        XCTAssertTrue(titles.contains("Copy"))
        XCTAssertTrue(titles.contains("Save…"))

        let diagnostics = try XCTUnwrap(buttons(in: logs.window?.contentView)
            .first { $0.title == "Run Diagnostics" })
        diagnostics.performClick(nil)
        XCTAssertTrue(diagnostics.isEnabled)
        XCTAssertTrue(textViews(in: logs.window?.contentView).contains {
            $0.string.contains("== full Hub diagnostics ==")
                && $0.string.contains("Preview mode")
        })

        let copy = try XCTUnwrap(buttons(in: logs.window?.contentView)
            .first { $0.title == "Copy" })
        copy.performClick(nil)
        let copied = try XCTUnwrap(NSPasteboard.general.string(forType: .string))
        XCTAssertTrue(copied.contains("== full Hub diagnostics =="))
        XCTAssertTrue(copied.contains("== support metadata =="))
        XCTAssertEqual(LogsWindowController.diagnosticsStatus(for: copied), "Diagnostics complete")
        XCTAssertEqual(
            LogsWindowController.diagnosticsStatus(for: "== doctor (failed) ==\nproblem"),
            "Diagnostics found issues"
        )
    }

    func testCommandLRedactsServiceSecretsBeforeDisplay() throws {
        let home = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-visible-log-test-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let folder = home.appendingPathComponent("Library/Logs/Teslatlas Hub", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try "Authorization: Bearer visible-secret\nvehicle=5YJ3E1EA7KF317000\n"
            .write(to: folder.appendingPathComponent("hub.err.log"), atomically: true, encoding: .utf8)
        let controller = HubController(homeDirectory: home, serviceInstalledOverride: false)
        let logs = LogsWindowController(controller: controller)

        let deadline = Date().addingTimeInterval(2)
        while Date() < deadline,
              !textViews(in: logs.window?.contentView).contains(where: {
                  $0.string.contains("== Hub service logs ==")
              }) {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        let visible = try XCTUnwrap(textViews(in: logs.window?.contentView).first?.string)
        XCTAssertTrue(visible.contains("Authorization: Bearer [redacted]"))
        XCTAssertTrue(visible.contains("vehicle=[redacted-vin]"))
        XCTAssertFalse(visible.contains("visible-secret"))
        XCTAssertFalse(visible.contains("5YJ3E1EA7KF317000"))
    }

    func testSelectedDesignsRenderAtNativeSize() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let dashboard = MainWindowController(controller: controller)
        let onboarding = OnboardingWindowController(controller: controller,
                                                     previewRoute: "choose-migration",
                                                     onComplete: { _ in })
        let welcome = OnboardingWindowController(controller: controller,
                                                 previewRoute: "welcome",
                                                 onComplete: { _ in })
        let migration = OnboardingWindowController(controller: controller,
                                                    previewRoute: "migration",
                                                    onComplete: { _ in })
        XCTAssertEqual(dashboard.window?.contentView?.bounds.size, NSSize(width: 900, height: 630))
        XCTAssertEqual(onboarding.window?.contentView?.bounds.size, NSSize(width: 485, height: 350))

        let destination: URL
        if let folder = ProcessInfo.processInfo.environment["TESLATLAS_HUB_SNAPSHOT_DIR"] {
            destination = URL(fileURLWithPath: folder, isDirectory: true)
        } else {
            destination = try defaultSnapshotDirectory()
        }
        try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
        try render(dashboard.window, to: destination.appendingPathComponent("dashboard.png"))
        try render(onboarding.window, to: destination.appendingPathComponent("onboarding-choice.png"))
        try render(welcome.window, to: destination.appendingPathComponent("onboarding-welcome.png"))
        try render(migration.window, to: destination.appendingPathComponent("onboarding-migration-key.png"))
        let authentication = try XCTUnwrap(popups(in: migration.window?.contentView).first {
            $0.itemTitles == ["SSH key", "Password"]
        })
        authentication.selectItem(withTitle: "Password")
        _ = NSApp.sendAction(authentication.action!, to: authentication.target, from: authentication)
        try render(migration.window, to: destination.appendingPathComponent("onboarding-migration-password.png"))
    }

    private func defaultSnapshotDirectory() throws -> URL {
        var directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        while directory.path != "/" {
            if FileManager.default.fileExists(
                atPath: directory.appendingPathComponent("Cargo.toml").path
            ) {
                return directory.appendingPathComponent("target/design-qa", isDirectory: true)
            }
            directory.deleteLastPathComponent()
        }
        return FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-design-qa", isDirectory: true)
    }

    private func render(_ window: NSWindow?, to destination: URL) throws {
        let view = try XCTUnwrap(window?.contentView?.superview ?? window?.contentView)
        view.layoutSubtreeIfNeeded()
        view.displayIfNeeded()
        let representation = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: representation)
        let png = try XCTUnwrap(representation.representation(using: .png, properties: [:]))
        try png.write(to: destination, options: .atomic)
    }

    private func buttons(in view: NSView?) -> [NSButton] {
        guard let view else { return [] }
        return (view as? NSButton).map { [$0] } ?? view.subviews.flatMap { buttons(in: $0) }
    }

    private func labels(in view: NSView?) -> [NSTextField] {
        guard let view else { return [] }
        return (view as? NSTextField).map { [$0] } ?? view.subviews.flatMap { labels(in: $0) }
    }

    private func imageViews(in view: NSView?) -> [NSImageView] {
        guard let view else { return [] }
        return (view as? NSImageView).map { [$0] }
            ?? view.subviews.flatMap { imageViews(in: $0) }
    }

    private func allViews(in view: NSView) -> [NSView] {
        [view] + view.subviews.flatMap(allViews)
    }

    private func view(in root: NSView, identifier: String) -> NSView? {
        allViews(in: root).first { $0.identifier?.rawValue == identifier }
    }

    private func progressIndicators(in view: NSView?) -> [NSProgressIndicator] {
        guard let view else { return [] }
        return (view as? NSProgressIndicator).map { [$0] } ?? view.subviews.flatMap(progressIndicators)
    }

    private func popups(in view: NSView?) -> [NSPopUpButton] {
        guard let view else { return [] }
        return (view as? NSPopUpButton).map { [$0] } ?? view.subviews.flatMap { popups(in: $0) }
    }

    private func secureFields(in view: NSView?) -> [NSSecureTextField] {
        guard let view else { return [] }
        return (view as? NSSecureTextField).map { [$0] }
            ?? view.subviews.flatMap { secureFields(in: $0) }
    }

    private func textViews(in view: NSView?) -> [NSTextView] {
        guard let view else { return [] }
        return (view as? NSTextView).map { [$0] }
            ?? view.subviews.flatMap { textViews(in: $0) }
    }
}
