import AppKit
import XCTest
@testable import Teslatlas_Hub

final class OnboardingWindowControllerTests: XCTestCase {
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
        let attributes = try FileManager.default.attributesOfItem(atPath: file.path)
        let permissions = try XCTUnwrap(attributes[.posixPermissions] as? NSNumber).intValue
        XCTAssertEqual(permissions & 0o777, 0o600)
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
        XCTAssertEqual(log.recentText(), "No app diagnostics are available yet.\n")
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
                                                     onComplete: {})

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
                                                     onComplete: {})
        XCTAssertEqual(onboarding.currentRoute, .provider)

        onboarding.navigate(to: .legacy)
        XCTAssertEqual(onboarding.currentRoute, .legacy)
        XCTAssertTrue(labels(in: onboarding.window?.contentView)
            .contains { $0.stringValue == "Connect with a Legacy Token" })

        onboarding.navigate(to: .migration)
        XCTAssertEqual(onboarding.currentRoute, .migration)
        XCTAssertTrue(buttons(in: onboarding.window?.contentView)
            .contains { $0.title == "Connect to Server" })
    }

    func testBusyOnboardingCannotCloseOrChangeAccountRoute() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     initialRoute: .legacy,
                                                     onComplete: {})
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
        for title in ["Stop Hub", "Restart", "Start Climate", "Wake", "Lock"] {
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
        let onboarding = OnboardingWindowController(controller: controller, onComplete: {})
        let continueButton = try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "Continue" })
        continueButton.performClick(nil)
        XCTAssertNil(continueButton.image)
        XCTAssertNil(try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "Back" }).image)

        let migration = try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "Migrate from TeslaMate" })
        XCTAssertFalse(migration.isHidden)
        XCTAssertEqual(migration.toolTip, "Bring your existing drives and charging history.")
        XCTAssertFalse(labels(in: onboarding.window?.contentView)
            .contains { $0.stringValue.contains("Exact TeslaMate") })
        let fresh = try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "New installation" })
        XCTAssertEqual(fresh.toolTip, "Connect Tesla Fleet Telemetry or Legacy Token.")

        migration.performClick(nil)
        let back = try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "Back" })
        back.performClick(nil)
        XCTAssertTrue(labels(in: onboarding.window?.contentView)
            .contains { $0.stringValue == "Teslatlas Hub" })
    }

    func testWelcomeUsesSelectedCenteredDesignCopy() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     previewRoute: "welcome",
                                                     onComplete: {})
        let text = labels(in: onboarding.window?.contentView).map(\.stringValue)
        XCTAssertTrue(text.contains("Teslatlas Hub"))
        XCTAssertTrue(text.contains(
            "Teslatlas Hub is the backend service replacing TeslaMate. It records the same data as TeslaMate without:"
        ))
        XCTAssertTrue(text.contains("Written purely in Rust."))
        XCTAssertTrue(text.contains("No Docker."))
        XCTAssertTrue(text.contains("Open source and developed natively for macOS and Debian."))
        XCTAssertTrue(text.contains("Uses SQLite."))
        XCTAssertFalse(text.contains("Private by default"))
        XCTAssertFalse(text.contains("Your data stays on this Mac"))
        XCTAssertFalse(text.contains("Made for Teslatlas"))
        XCTAssertFalse(text.contains("Your vehicle data stays on this Mac."))
        XCTAssertNotNil(Bundle.main.image(forResource: "RustLogo"))
        let continueButton = try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "Continue" })
        XCTAssertNil(continueButton.image)
    }

    func testMigrationOffersNormalUserKeyPasswordAndPasswordlessSudo() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     previewRoute: "migration",
                                                     onComplete: {})
        let view = onboarding.window?.contentView
        let text = labels(in: view).map(\.stringValue)
        XCTAssertTrue(text.contains("user"))
        XCTAssertTrue(text.contains(
            "Use a normal server account. It must access the TeslaMate containers directly or through passwordless sudo."
        ))
        XCTAssertTrue(buttons(in: view).contains { $0.title == "Choose Key…" })
        let sudo = try XCTUnwrap(buttons(in: view)
            .first { $0.title == "Use passwordless sudo for Docker access" })
        XCTAssertEqual(sudo.state, .on)
        let connect = try XCTUnwrap(buttons(in: view).first { $0.title == "Connect to Server" })
        XCTAssertEqual(connect.contentTintColor, .white)
        XCTAssertNil(connect.image)
        XCTAssertTrue(try XCTUnwrap(buttons(in: view).first { $0.title == "Continue" }).isHidden)

        let authentication = try XCTUnwrap(popups(in: view).first {
            $0.itemTitles == ["SSH key or agent", "Password"]
        })
        authentication.selectItem(withTitle: "Password")
        _ = NSApp.sendAction(authentication.action!, to: authentication.target, from: authentication)

        let updatedView = onboarding.window?.contentView
        XCTAssertTrue(labels(in: updatedView).contains { $0.stringValue == "Password" })
        XCTAssertTrue(secureFields(in: updatedView).contains { $0.placeholderString == "SSH password" })
        XCTAssertFalse(buttons(in: updatedView).contains { $0.title == "Choose Key…" })
    }

    func testMigrationUsesExplicitKeyboardNavigationOrder() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     previewRoute: "migration",
                                                     onComplete: {})
        let view = onboarding.window?.contentView
        let server = try XCTUnwrap(labels(in: view).first {
            $0.placeholderString == "Server name or IP address"
        })
        let user = try XCTUnwrap(labels(in: view).first { $0.stringValue == "user" })
        let port = try XCTUnwrap(labels(in: view).first { $0.stringValue == "22" })
        let authentication = try XCTUnwrap(popups(in: view).first {
            $0.itemTitles == ["SSH key or agent", "Password"]
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
                                                     onComplete: {})
        let view = onboarding.window?.contentView
        let server = try XCTUnwrap(labels(in: view).first {
            $0.placeholderString == "Server name or IP address"
        })
        let authentication = try XCTUnwrap(popups(in: view).first {
            $0.itemTitles == ["SSH key or agent", "Password"]
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
    }

    func testSelectedDesignsRenderAtNativeSize() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let dashboard = MainWindowController(controller: controller)
        let onboarding = OnboardingWindowController(controller: controller,
                                                     previewRoute: "choose-migration",
                                                     onComplete: {})
        let welcome = OnboardingWindowController(controller: controller,
                                                 previewRoute: "welcome",
                                                 onComplete: {})
        let migration = OnboardingWindowController(controller: controller,
                                                    previewRoute: "migration",
                                                    onComplete: {})
        XCTAssertEqual(dashboard.window?.contentView?.bounds.size, NSSize(width: 900, height: 630))
        XCTAssertEqual(onboarding.window?.contentView?.bounds.size, NSSize(width: 900, height: 630))

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
            $0.itemTitles == ["SSH key or agent", "Password"]
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
        throw CocoaError(.fileNoSuchFile)
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
