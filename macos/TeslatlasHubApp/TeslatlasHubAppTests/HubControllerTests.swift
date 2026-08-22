import AppKit
import XCTest
@testable import Teslatlas_Hub

final class HubControllerTests: XCTestCase {
    func testMainWindowBuildsInPreviewMode() {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let windowController = MainWindowController(controller: controller)
        XCTAssertEqual(windowController.window?.title, "Teslatlas Hub")
        XCTAssertEqual(windowController.window?.contentRect(forFrameRect: windowController.window!.frame).size,
                       NSSize(width: 900, height: 568))
    }

    func testServiceDetailsUpdateButtonInstallsCurrentPackage() throws {
        let installer = RecordingInstaller()
        let controller = HubController(commandRunner: CountingRunner(),
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
            if failures == 6 { expectation.fulfill() }
        }
        controller.installService(completion: done)
        controller.uninstallService(deleteData: false, completion: done)
        controller.importTeslaMate(source: "postgres://example", carID: "1", passwordFile: "/tmp/password", encryptionKeyFile: "/tmp/encryption", completion: done)
        controller.configureTeslaAccount(tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh"), completion: done)
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

    func testInstalledMigrationStopsConfirmsFinalCopyAndRestarts() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success("imported"))
        let installer = ScriptedInstaller(events: events)
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: runner,
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

    func testInstalledAccountConfigureRestartsAfterSuccessfulInstaller() throws {
        let events = EventRecorder()
        let runner = ScriptedRunner(events: events, result: .success("ok"))
        let installer = ScriptedInstaller(events: events)
        let service = ScriptedService(events: events)
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: runner,
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
        XCTAssertEqual(events.values, ["service:stop", "install", "setup", "service:start", "command"])
    }

    func testMigrationSourceAllowsUsernameButRejectsPassword() throws {
        XCTAssertNoThrow(try HubController.validateMigrationSource("postgresql://reader@localhost/teslamate"))
        XCTAssertThrowsError(try HubController.validateMigrationSource("postgresql://reader:secret@localhost/teslamate"))
        XCTAssertThrowsError(try HubController.validateMigrationSource("postgres://reader:s%40cret@localhost/teslamate"))
    }

    func testTOMLBasicStringEscapesPathWithoutBreakingQuotedValue() {
        XCTAssertEqual(HubController.tomlBasicString("/Users/O'Brien\\folder\nnext"),
                       "\"/Users/O'Brien\\\\folder\\nnext\"")
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

    func testOlderInstallUninstallInstallsCurrentPackageBeforeRunningUninstaller() throws {
        let script = "/Library/Application Support/Teslatlas Hub/libexec/uninstall-macos-service.sh"
        let package = "/Applications/Teslatlas Hub.app/Contents/Resources/TeslatlasHubService.pkg"
        let fallback = try EmbeddedInstaller.uninstallCommand(
            scriptPath: script,
            packagePath: package,
            installedUninstallerAvailable: false,
            deleteData: true
        )
        XCTAssertEqual(
            fallback,
            "/usr/sbin/installer -pkg '\(package)' -target / && /usr/bin/test -x '\(script)' && /bin/sh '\(script)' --delete-data"
        )

        let installed = try EmbeddedInstaller.uninstallCommand(
            scriptPath: script,
            packagePath: nil,
            installedUninstallerAvailable: true,
            deleteData: false
        )
        XCTAssertEqual(installed, "/bin/sh '\(script)'")
        XCTAssertThrowsError(try EmbeddedInstaller.uninstallCommand(
            scriptPath: script,
            packagePath: nil,
            installedUninstallerAvailable: false,
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

    private func temporaryHome() throws -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: false)
        return url
    }

    private func buttons(in view: NSView?) -> [NSButton] {
        guard let view else { return [] }
        return (view as? NSButton).map { [$0] } ?? view.subviews.flatMap { buttons(in: $0) }
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
    private(set) var stdin: String?

    init(events: EventRecorder, result: Result<String, Error>) {
        self.events = events
        self.result = result
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        let event = arguments.contains("migrate") ? "migrate" :
            (arguments.contains("setup") ? "setup" : "command")
        events.append(event)
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
    var result: Result<String, Error> = .success("")

    init(events: EventRecorder) { self.events = events }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        events.append("service:\(arguments.last ?? "unknown")")
        completion(result)
    }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        completion(.unloaded)
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
