import AppKit
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

    func testPreviewIsReadOnly() {
        let runner = CountingRunner()
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"], commandRunner: runner)
        XCTAssertTrue(controller.previewMode)
        XCTAssertEqual(controller.snapshot.health, .running)

        let expectation = expectation(description: "actions rejected")
        var failures = 0
        let done: (Result<Void, Error>) -> Void = { result in
            if case .failure = result { failures += 1 }
            if failures == 7 { expectation.fulfill() }
        }
        controller.installService(completion: done)
        controller.uninstallService(deleteData: false, completion: done)
        controller.importTeslaMate(source: "postgres://example", carID: "1", passwordFile: "/tmp/password", encryptionKeyFile: "/tmp/encryption", completion: done)
        controller.configureTeslaAccount(tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh"), completion: done)
        controller.performVehicleControl(.climateStart, completion: done)
        controller.stopHub(completion: done)
        controller.restartHub(completion: done)
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

    func testRefreshUsesInstalledBinaryAndReportsVersionMismatch() {
        let embedded = RecordingCommandRunner(result: .failure(HubActionError.commandFailed("unused")))
        let installed = RecordingCommandRunner(result: .success("""
        {"status":"ok","version":"9.9.9","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"vehicle":null,"legacyCredentials":{"present":false}}
        """))
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
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

    func testRefreshSummarizesMultipleInstalledVehicles() {
        let installed = RecordingCommandRunner(result: .success("""
        {"status":"ok","version":"1.0.0-alpha.1","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicle":null,"vehicles":[{"displayName":"One"},{"displayName":"Two"}],"credentials":{"present":true},"legacyCredentials":{"present":false},"fleetCredentials":{"present":true}}
        """))
        let controller = HubController(installedCommandRunner: installed,
                                       serviceRunner: ScriptedService(events: EventRecorder()),
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "multi-vehicle status loaded")

        controller.refresh { snapshot in
            XCTAssertEqual(snapshot.vehicleName, "2 vehicles")
            XCTAssertEqual(snapshot.account, "Connected")
            XCTAssertNil(snapshot.controlVehicleID)
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
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

    func testAthenaCardShowsVisibleNonChargingControlsAndHidesConnectedAccountButton() {
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
            XCTAssertTrue(windowController.connectButton.isHidden)
            XCTAssertFalse(self.buttons(in: windowController.window?.contentView)
                .contains { $0.title.localizedCaseInsensitiveContains("charge") })
            XCTAssertEqual(HubVehicleControl.allCases.map(\.rawValue), [
                "wake", "climate-start", "climate-stop", "lock", "unlock", "flash-lights", "honk-horn"
            ])
            settled.fulfill()
        }
        wait(for: [settled], timeout: 1)
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
        XCTAssertEqual(events.values, ["service:stop", "install", "migrate", "service:start"])
        XCTAssertEqual(runner.stdin, "y\nn\n")
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
        XCTAssertEqual(events.values, ["migrate", "install"])
        XCTAssertEqual(runner.stdin, "y\nn\n")
        let config = try String(contentsOf: home
            .appendingPathComponent("Library/Application Support/Teslatlas Hub/config.toml"))
        XCTAssertTrue(config.contains("[geocoder]\nenabled = false"))
        XCTAssertTrue(config.contains("[terrain]\nenabled = false"))
    }

    func testInstalledMigrationRestartsAfterMigrationFailure() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events,
                                    result: .failure(HubActionError.commandFailed("copy failed")))
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
        let finished = expectation(description: "migration failure returned")

        controller.importTeslaMate(source: "postgres://reader@localhost/teslamate",
                                   carID: "1",
                                   passwordFile: "/tmp/password",
                                   encryptionKeyFile: "/tmp/key") { result in
            guard case let .failure(error) = result else { return XCTFail("expected migration failure") }
            XCTAssertTrue(error.localizedDescription.contains("copy failed"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:stop", "install", "migrate", "service:start"])
        XCTAssertEqual(runner.stdin, "y\nn\n")
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
        let finished = expectation(description: "package failure returned")

        controller.importTeslaMate(source: "postgres://reader@localhost/teslamate",
                                   carID: "1",
                                   passwordFile: "/tmp/password",
                                   encryptionKeyFile: "/tmp/key") { result in
            guard case let .failure(error) = result else { return XCTFail("expected package failure") }
            XCTAssertTrue(error.localizedDescription.contains("package failed"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, ["service:stop", "install", "service:start"])
        XCTAssertNil(runner.stdin)
    }

    func testInstalledAccountConfigureRunsSetupBeforeInstallerAndRestarts() throws {
        let events = EventRecorder()
        let embedded = CountingRunner()
        let installed = ScriptedRunner(events: events,
                                       result: .success("ok"),
                                       unsupportedAllVehiclesAttempts: 1)
        let installer = ScriptedInstaller(events: events)
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
                                       installer: installer,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "account configured")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(events.values, [
            "service:stop", "setup", "setup", "install", "service:stop", "setup", "service:start", "command"
        ])
        XCTAssertEqual(embedded.calls, 0)
        XCTAssertNotNil(installed.stdin)
        XCTAssertTrue(installed.arguments[0].contains("--all-vehicles"))
        XCTAssertFalse(installed.arguments[1].contains("--all-vehicles"))
        XCTAssertTrue(installed.arguments[2].contains("--all-vehicles"))
        XCTAssertTrue(try configContents(in: home).contains("provider = \"legacy\""))
    }

    func testInstalledAccountConfigureRestartsOldServiceAfterSetupFailure() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(
            events: events,
            result: .failure(HubActionError.commandFailed("setup failed"))
        )
        let installer = ScriptedInstaller(events: events)
        let service = ScriptedService(events: events, loadState: .loaded)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: installer,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "setup failure returned")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case let .failure(error) = result else { return XCTFail("expected setup failure") }
            XCTAssertTrue(error.localizedDescription.contains("setup failed"))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(try configContents(in: home), original)
        XCTAssertEqual(events.values, ["service:stop", "setup", "service:start"])
    }

    func testInstalledLegacyFailureKeepsPreviouslyStoppedHubStopped() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(
            events: events,
            result: .failure(HubActionError.commandFailed("setup failed"))
        )
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = try writeCollectorConfig(in: home, provider: "fleet")
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: ScriptedInstaller(events: events),
                                       serviceRunner: ScriptedService(events: events, loadState: .unloaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "stopped setup failure returned")

        controller.configureTeslaAccount(
            tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")
        ) { result in
            guard case .failure = result else { return XCTFail("expected setup failure") }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(try configContents(in: home), original)
        XCTAssertEqual(events.values, ["service:stop", "setup"])
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
                                       serviceInstalledOverride: true)
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
                                       serviceInstalledOverride: true)
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
        XCTAssertEqual(events.values, ["service:stop", "setup", "install", "service:start"])
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
                                       serviceInstalledOverride: true)
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
        XCTAssertEqual(events.values, ["service:stop", "setup", "install"])
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

    func testUninstallExtractsEmbeddedUninstallerWithoutInstallingPackage() throws {
        let package = "/Applications/Teslatlas Hub.app/Contents/Resources/TeslatlasHubService.pkg"
        let command = try EmbeddedInstaller.uninstallCommand(
            packagePath: package,
            deleteData: true
        )
        XCTAssertTrue(command.contains("/usr/sbin/pkgutil --expand-full '\(package)'"))
        XCTAssertTrue(command.contains("Payload/Library/Application Support/Teslatlas Hub/libexec/uninstall-macos-service.sh"))
        XCTAssertTrue(command.hasSuffix("/bin/sh \"$uninstaller\" --delete-data"))
        XCTAssertFalse(command.contains("/usr/sbin/installer "))
        let syntaxCheck = Process()
        syntaxCheck.executableURL = URL(fileURLWithPath: "/bin/sh")
        syntaxCheck.arguments = ["-n", "-c", command]
        try syntaxCheck.run()
        syntaxCheck.waitUntilExit()
        XCTAssertEqual(syntaxCheck.terminationStatus, 0)
        XCTAssertThrowsError(try EmbeddedInstaller.uninstallCommand(
            packagePath: nil,
            deleteData: false
        ))
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

private final class RecordingCommandRunner: HubCommandRunning {
    private let result: Result<String, Error>
    private(set) var arguments: [[String]] = []

    init(result: Result<String, Error>) { self.result = result }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        self.arguments.append(arguments)
        completion(result)
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

private final class ScriptedInstaller: HubInstalling {
    private let events: EventRecorder
    private let result: Result<String, Error>

    init(events: EventRecorder, result: Result<String, Error> = .success("")) {
        self.events = events
        self.result = result
    }

    func install(completion: @escaping (Result<String, Error>) -> Void) {
        events.append("install")
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
