import AppKit
import Darwin
import XCTest
@testable import Teslatlas_Hub

final class HubControllerTests: XCTestCase {
    func testMainWindowBuildsInPreviewMode() {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let windowController = MainWindowController(controller: controller)
        XCTAssertEqual(windowController.window?.title, "Teslatlas Hub")
        XCTAssertEqual(windowController.window?.contentRect(forFrameRect: windowController.window!.frame).size,
                       NSSize(width: 900, height: 630))
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
        controller.importTeslaMate(source: "postgres://example", carID: "1", passwordFile: "/tmp/password", encryptionKeyFile: "/tmp/encryption", completion: done)
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
        XCTAssertEqual(loaded, [["kickstart", "-k", "gui/1/com.teslatlas.hub"]])
        let unloaded = LaunchctlServiceController.commandPlan(action: .restart, loaded: false, domain: "gui/1", service: "gui/1/com.teslatlas.hub", plist: "/tmp/hub.plist")
        XCTAssertEqual(unloaded, [["bootstrap", "gui/1", "/tmp/hub.plist"], ["kickstart", "-k", "gui/1/com.teslatlas.hub"]])
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
            XCTAssertTrue(text.contains("Expected Hub: 1.0.0-alpha.1"))
            XCTAssertTrue(text.contains("Service: not installed"))
            XCTAssertTrue(text.contains("Provider: Not configured"))
            XCTAssertTrue(text.contains("macOS:"))
            XCTAssertTrue(text.contains("Architecture: arm64"))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)
        XCTAssertEqual(runner.commands, ["doctor", "preflight", "status"])
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
        installationId=9ca970df-0616-43d5-8493-e1faf00e97f1
        server=10.8.0.1
        ipv6=[fd12:3456:789a::1]:5432 link=fe80::42%en0 loopback=::1
        coloured=\u{001B}[2m2026-08-27T19:02:26Z\u{001B}[0m \u{001B}[32mINFO\u{001B}[0m ready\u{0007}
        /Users/example/Library/Logs/Teslatlas Hub/hub.err.log
        """

        let redacted = HubShareRedactor.redact(source, homeDirectory: "/Users/example")

        XCTAssertTrue(redacted.contains("password authentication failed"))
        XCTAssertTrue(redacted.contains("status code: 500"))
        XCTAssertTrue(redacted.contains("Authorization: Bearer [redacted]"))
        XCTAssertTrue(redacted.contains("\"accessToken\":\"[redacted]"))
        XCTAssertTrue(redacted.contains("code=[redacted]&state=public-state"))
        XCTAssertTrue(redacted.contains("postgresql://reader:[redacted]@127.0.0.1"))
        XCTAssertTrue(redacted.contains("[redacted-jwt]"))
        XCTAssertTrue(redacted.contains("vehicle=[redacted-vin]"))
        XCTAssertTrue(redacted.contains("display_name=[redacted-name]"))
        XCTAssertTrue(redacted.contains("\"vehicleName\":\"[redacted-name]\""))
        XCTAssertTrue(redacted.contains("vehicle_id=[redacted-id]"))
        XCTAssertTrue(redacted.contains("installationId=[redacted-id]"))
        XCTAssertTrue(redacted.contains("server=[redacted-private-ip]"))
        XCTAssertTrue(redacted.contains("ipv6=[redacted-private-ip] link=[redacted-private-ip] loopback=[redacted-private-ip]"))
        XCTAssertTrue(redacted.contains("coloured=2026-08-27T19:02:26Z INFO ready"))
        XCTAssertTrue(redacted.contains("~/Library/Logs"))
        for secret in ["bearer-secret-value", "access-secret-value", "refresh-secret-value",
                       "ingest-secret-value", "private-secret-value",
                       "EU_secret_code", "database-secret", "eyJheader.payload.signature",
                       "5YJ3E1EA7KF317000", "Athena Road Trip", "Athena JSON",
                       "477a04f6-b726-50e3-86e0-a5a9143b3239",
                       "9ca970df-0616-43d5-8493-e1faf00e97f1", "10.8.0.1", "fd12:3456:789a::1",
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
        {"status":"ok","version":"1.0.0-alpha.1","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"legacy","credentials":{"present":true}}
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
            XCTAssertTrue(copy.contains("Teslatlas Hub is stopped."))
            XCTAssertFalse(copy.contains { $0.localizedCaseInsensitiveContains("setup required") })
            XCTAssertFalse(copy.contains { $0.localizedCaseInsensitiveContains("needs attention") })
            let start = self.buttons(in: dashboard?.window?.contentView)
                .first { $0.title == "Start Hub" }
            XCTAssertEqual(start?.keyEquivalent, "\r")
            XCTAssertTrue(dashboard?.window?.defaultButtonCell === start?.cell)
            settled.fulfill()
        }

        wait(for: [settled], timeout: 2)
        withExtendedLifetime(dashboard) {}
    }

    func testRefreshSummarizesMultipleInstalledVehicles() {
        let firstID = UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5")!
        let secondID = UUID(uuidString: "FB25AA4A-A719-4575-8BB1-02D4524F2571")!
        let installed = RecordingCommandRunner(result: .success("""
        {"status":"ok","version":"1.0.0-alpha.1","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicle":null,"vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"One"},{"vehicleId":"\(secondID.uuidString)","displayName":"Two"}],"credentials":{"present":true},"legacyCredentials":{"present":false},"fleetCredentials":{"present":true}}
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
        {"status":"ok","version":"1.0.0-alpha.1","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"One"},{"vehicleId":"\(secondID.uuidString)","displayName":"Two"}],"credentials":{"present":true}}
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

    func testMultipleVehicleDashboardShowsNativeSelector() throws {
        let firstID = UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5")!
        let secondID = UUID(uuidString: "FB25AA4A-A719-4575-8BB1-02D4524F2571")!
        let status = """
        {"status":"ok","version":"1.0.0-alpha.1","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(firstID.uuidString)","displayName":"One"},{"vehicleId":"\(secondID.uuidString)","displayName":"Two"}],"credentials":{"present":true}}
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
            XCTAssertFalse(selector?.isBordered ?? true)
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
            {"status":"ok","version":"1.0.0-alpha.1","ready":false,"provider":"fleet","credentials":{"present":false}}
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
            {"status":"ok","version":"1.0.0-alpha.1","ready":false,"provider":"fleet","credentials":{"present":false},"legacyCredentials":{"present":true},"fleetCredentials":{"present":false}}
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
        {"status":"ok","version":"1.0.0-alpha.1","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"\(vehicleID.uuidString)","displayName":"One"}],"credentials":{"present":true}}
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

    func testAthenaCardShowsControlsAndManageTeslaInsteadOfConnectTesla() {
        let alert = MainWindowController.vehicleControlConfirmation(.climateStart,
                                                                    vehicleName: "Model 3")
        XCTAssertEqual(alert.messageText, "Start Climate for Model 3?")
        XCTAssertEqual(alert.buttons.map(\.title), ["Cancel", "Start Climate"])

        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let windowController = MainWindowController(controller: controller)
        let settled = expectation(description: "preview dashboard settled")
        DispatchQueue.main.async {
            XCTAssertTrue(self.labels(in: windowController.window?.contentView)
                .contains { $0.stringValue == "Athena" })
            XCTAssertFalse(self.buttons(in: windowController.window?.contentView)
                .contains { $0.title == "Vehicle Controls…" })
            for title in ["Start Climate", "Stop Climate", "Wake", "Lock",
                          "Unlock", "Flash", "Honk"] {
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
                .allSatisfy { !$0.isBordered })
            XCTAssertNil(windowController.window?.defaultButtonCell)
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

    func testDisconnectConfirmationDefaultsToCancel() {
        let alert = MainWindowController.disconnectConfirmation()
        XCTAssertEqual(alert.buttons.map(\.title), ["Cancel", "Disconnect"])
        XCTAssertEqual(alert.buttons.first?.keyEquivalent, "\r")
        XCTAssertNotEqual(alert.buttons.last?.keyEquivalent, "\r")
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

    func testImportSheetDoesNotClaimHubStartsAutomatically() {
        XCTAssertFalse(
            ImportSheetController.teslaMateStoppedConfirmationDetail.contains("starts Hub automatically")
        )
        XCTAssertTrue(
            ImportSheetController.teslaMateStoppedConfirmationDetail.contains("leaves Hub stopped")
        )
    }

    func testInstalledMigrationStopsConfirmsFinalCopyAndRestarts() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success("imported"))
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
                                   encryptionKeyFile: "/tmp/key") { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:stop", "migrate"])
        XCTAssertEqual(runner.stdin, "y\nn\n")
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertFalse(events.values.contains("install"))
        XCTAssertFalse(events.values.contains("service:start"))
    }

    func testInstalledMigrationKeepsFleetProviderAndTokensConfig() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success("imported"))
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
                                   encryptionKeyFile: "/tmp/key") { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:stop", "migrate"])
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertFalse(events.values.contains("service:start"))
        let config = try configContents(in: home)
        XCTAssertTrue(config.contains("provider = \"fleet\""))
        XCTAssertTrue(config.contains("interval_seconds = 0"))
        XCTAssertEqual(HubController.collectorProvider(in: config), "fleet")
    }

    func testFreshMigrationInstallsCurrentServicePackageAfterSuccessfulCopy() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success("imported"))
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
                                   encryptionKeyFile: "/tmp/key") { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["migrate"])
        XCTAssertEqual(runner.stdin, "y\nn\n")
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
                                   encryptionKeyFile: "/tmp/key") { result in
            guard case let .failure(error) = result else { return XCTFail("expected migration timeout") }
            XCTAssertTrue(error.localizedDescription.contains("remains stopped"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:stop", "migrate"])
        XCTAssertEqual(runner.stdin, "y\nn\n")
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
                                   encryptionKeyFile: "/tmp/key") { result in
            guard case let .failure(error) = result else {
                return XCTFail("expected ambiguous migration failure")
            }
            XCTAssertTrue(error.localizedDescription.contains("remains stopped"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertTrue(try configContents(in: home).contains("interval_seconds = 0"))
        XCTAssertEqual(events.values, ["service:stop", "migrate"])
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
                                   encryptionKeyFile: "/tmp/key") { result in
            guard case let .failure(error) = result else {
                return XCTFail("expected import failure")
            }
            XCTAssertTrue(error.localizedDescription.contains("copy failed"))
            XCTAssertTrue(error.localizedDescription.contains("remains stopped"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertTrue(controller.hasPendingMigrationHandover)
    }

    func testInstalledMigrationDoesNotMutateWithStaleBinaryWhenPackageUpdateFails() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success("unused"))
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
                                   encryptionKeyFile: "/tmp/key") { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:stop", "migrate"])
        XCTAssertEqual(runner.stdin, "y\nn\n")
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
            versionResult: .success("teslatlas-hub 1.0.0-alpha.1\n"),
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
            versionResult: .success("teslatlas-hub 1.0.0-alpha.1\n"),
            commandResult: .success("configured")
        )
        let installed = VersionAwareRunner(
            events: events,
            versionResult: .success("teslatlas-hub 1.0.0-alpha.1\n")
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
            "teslatlas-hub 1.0.0-alpha.1\n"
        ))
        XCTAssertFalse(HubController.isBundledServiceVersionOutput(
            "teslatlas-hub 1.0.0-alpha.0"
        ))
        XCTAssertFalse(HubController.isBundledServiceVersionOutput(
            "prefix teslatlas-hub 1.0.0-alpha.1"
        ))
        XCTAssertFalse(HubController.isBundledServiceVersionOutput(
            "teslatlas-hub 1.0.0-alpha.1\nextra"
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

private final class MutatingFailureRunner: HubCommandRunning {
    private let error: Error
    private let mutation: () throws -> Void

    init(error: Error, mutation: @escaping () throws -> Void) {
        self.error = error
        self.mutation = mutation
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
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
