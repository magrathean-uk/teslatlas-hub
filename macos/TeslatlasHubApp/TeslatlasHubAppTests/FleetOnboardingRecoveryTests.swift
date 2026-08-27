import AppKit
import XCTest
@testable import Teslatlas_Hub

final class FleetOnboardingRecoveryTests: XCTestCase {
    private let credentials = HubFleetSetupCredentials(
        accessToken: "access",
        refreshToken: "refresh",
        clientID: "client",
        region: "europe_middle_east_and_africa",
        expiresInSeconds: 3600
    )

    func testInstalledSetupTimeoutKeepsFleetSelectedAndHubStopped() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let embedded = FleetRecoveryRunner(events: events, event: "embedded-setup", result: .success("unused"))
        let installed = FleetRecoveryRunner(events: events,
                                             event: "installed-setup",
                                             result: .failure(HubActionError.commandTimedOut))
        var configAtInstall = ""
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
                                       installer: FleetRecoveryInstaller(events: events) {
                                           configAtInstall = (try? self.configContents(in: home)) ?? ""
                                       },
                                       serviceRunner: FleetRecoveryService(events: events, state: .loaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)

        let finished = expectation(description: "setup timeout returned")
        controller.configureFleetAccount(credentials: credentials) { result in
            guard case let .failure(error) = result else { return XCTFail("expected setup timeout") }
            XCTAssertTrue(error.localizedDescription.contains("remains stopped"))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertNotEqual(try configContents(in: home), original)
        XCTAssertTrue(try configContents(in: home).contains("provider = \"fleet\""))
        XCTAssertTrue(configAtInstall.contains("provider = \"legacy\""))
        XCTAssertFalse(configAtInstall.contains("provider = \"fleet\""))
        XCTAssertEqual(events.values, ["state", "service:stop", "install", "installed-setup"])
    }

    func testInstalledInstallerFailureRestoresConfigAndRestartsLoadedService() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let embedded = FleetRecoveryRunner(events: events, event: "embedded-setup", result: .success("unused"))
        let installed = FleetRecoveryRunner(events: events, event: "installed-setup", result: .success("configured"))
        let installer = FleetRecoveryInstaller(
            events: events,
            result: .failure(HubActionError.commandFailed("package failed"))
        )
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
                                       installer: installer,
                                       serviceRunner: FleetRecoveryService(events: events, state: .loaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)

        let finished = expectation(description: "installer failure returned")
        controller.configureFleetAccount(credentials: credentials) { result in
            guard case let .failure(error) = result else { return XCTFail("expected installer failure") }
            XCTAssertTrue(error.localizedDescription.contains("package failed"))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertEqual(try configContents(in: home), original)
        XCTAssertEqual(events.values, ["state", "service:stop", "install", "service:start"])
    }

    func testForwardOnlyInstallerFailureRestoresConfigAndLeavesOldServiceStopped() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let runner = FleetRecoveryRunner(events: events, result: .success("unused"))
        let installer = FleetRecoveryInstaller(
            events: events,
            result: .failure(HubActionError.commandFailed("TESLATLAS_FORWARD_ONLY_UPGRADE"))
        )
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: installer,
                                       serviceRunner: FleetRecoveryService(events: events, state: .loaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)

        let finished = expectation(description: "forward-only installer failure returned")
        controller.configureFleetAccount(credentials: credentials) { result in
            guard case .failure = result else { return XCTFail("expected installer failure") }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertEqual(try configContents(in: home), original)
        XCTAssertEqual(events.values, ["state", "service:stop", "install"])
    }

    func testFreshSetupFailureDoesNotStartService() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let events = FleetRecoveryEvents()
        let embedded = FleetRecoveryRunner(events: events,
                                            event: "embedded-setup",
                                            result: .failure(HubActionError.commandFailed("setup failed")))
        let installed = FleetRecoveryRunner(events: events, event: "installed-setup", result: .success("unused"))
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
                                       installer: FleetRecoveryInstaller(events: events),
                                       serviceRunner: FleetRecoveryService(events: events, state: .unloaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: false)

        let finished = expectation(description: "fresh setup failure returned")
        controller.configureFleetAccount(credentials: credentials) { result in
            guard case .failure = result else { return XCTFail("expected setup failure") }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertEqual(events.values, ["embedded-setup"])
        XCTAssertFalse(events.values.contains("service:start"))
    }

    func testInstalledUnconfiguredSetupCreatesCredentialsBeforePackagePreflight() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let embedded = FleetRecoveryRunner(events: events,
                                            event: "embedded-setup",
                                            result: .success("configured"),
                                            versionResult: .success("teslatlas-hub 1.0.0-alpha.1\n"))
        let installed = FleetRecoveryRunner(events: events,
                                             event: "installed-setup",
                                             result: .success("unused"),
                                             versionResult: .success("teslatlas-hub 1.0.0-alpha.0\n"))
        var configAtInstall = ""
        let installer = FleetRecoveryInstaller(events: events) {
            configAtInstall = (try? self.configContents(in: home)) ?? ""
        }
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
                                       installer: installer,
                                       serviceRunner: FleetRecoveryService(events: events, state: .loaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)

        let finished = expectation(description: "installed unconfigured setup succeeds")
        controller.configureFleetAccount(credentials: credentials) { result in
            guard case .success = result else { return XCTFail("expected setup success") }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertTrue(configAtInstall.contains("provider = \"fleet\""))
        XCTAssertEqual(events.values, [
            "service:stop", "embedded-setup", "command", "command", "install", "service:start", "state", "command"
        ])
    }

    func testInstalledUnconfiguredSetupReusesExactPackagedService() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let embedded = FleetRecoveryRunner(events: events,
                                            event: "embedded-setup",
                                            result: .success("configured"),
                                            versionResult: .success("teslatlas-hub 1.0.0-alpha.1\n"))
        let installed = FleetRecoveryRunner(
            events: events,
            event: "installed-setup",
            result: .success("unused"),
            versionResult: .success("teslatlas-hub 1.0.0-alpha.1\n")
        )
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
                                       installer: FleetRecoveryInstaller(events: events),
                                       serviceRunner: FleetRecoveryService(events: events, state: .loaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)

        let finished = expectation(description: "matching Fleet service reused")
        controller.configureFleetAccount(credentials: credentials) { result in
            guard case .success = result else { return XCTFail("expected setup success") }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertTrue(try configContents(in: home).contains("provider = \"fleet\""))
        XCTAssertFalse(events.values.contains("install"))
        XCTAssertEqual(events.values, [
            "service:stop", "embedded-setup", "command", "command", "service:start", "state", "command"
        ])
    }

    func testInstalledUnconfiguredInstallerFailureKeepsFleetConfiguredAndStopped() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let embedded = FleetRecoveryRunner(events: events,
                                            event: "embedded-setup",
                                            result: .success("configured"))
        let installed = FleetRecoveryRunner(events: events,
                                             event: "installed-setup",
                                             result: .success("unused"))
        let installer = FleetRecoveryInstaller(
            events: events,
            result: .failure(HubActionError.commandFailed("package failed"))
        )
        let controller = HubController(commandRunner: embedded,
                                       installedCommandRunner: installed,
                                       installer: installer,
                                       serviceRunner: FleetRecoveryService(events: events, state: .loaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let finished = expectation(description: "configured Fleet remains stopped")

        controller.configureFleetAccount(credentials: credentials) { result in
            guard case let .failure(error) = result else {
                return XCTFail("expected installer failure")
            }
            XCTAssertTrue(error.localizedDescription.contains("configured"))
            XCTAssertTrue(error.localizedDescription.contains("remains stopped"))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertTrue(try configContents(in: home).contains("provider = \"fleet\""))
        XCTAssertEqual(events.values, ["service:stop", "embedded-setup", "command", "install"])
    }

    func testInstalledStopFailureDoesNotChangeConfig() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let runner = FleetRecoveryRunner(events: events, result: .success("configured"))
        let service = FleetRecoveryService(events: events,
                                           state: .loaded,
                                           stopResult: .failure(HubActionError.commandFailed("stop failed")))
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: FleetRecoveryInstaller(events: events),
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)

        let finished = expectation(description: "stop failure returned")
        controller.configureFleetAccount(credentials: credentials) { result in
            guard case let .failure(error) = result else { return XCTFail("expected stop failure") }
            XCTAssertTrue(error.localizedDescription.contains("stop failed"))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertEqual(try configContents(in: home), original)
        XCTAssertEqual(events.values, ["state", "service:stop"])
    }

    private func temporaryHome() throws -> URL {
        let home = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-fleet-recovery-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        return home
    }

    private func configURL(in home: URL) -> URL {
        home.appendingPathComponent("Library/Application Support/Teslatlas Hub/config.toml")
    }

    private func writeOriginalConfig(in home: URL) throws -> String {
        let config = configURL(in: home)
        try FileManager.default.createDirectory(at: config.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        let original = "data_dir = \"/tmp/original\"\n\n[collector]\nprovider = \"legacy\"\ninterval_seconds = 60\n"
        try original.write(to: config, atomically: true, encoding: .utf8)
        return original
    }

    private func configContents(in home: URL) throws -> String {
        try String(contentsOf: configURL(in: home), encoding: .utf8)
    }
}

private final class FleetRecoveryEvents {
    private var events: [String] = []

    var values: [String] { events }

    func append(_ event: String) { events.append(event) }
}

private final class FleetRecoveryRunner: HubCommandRunning {
    private let events: FleetRecoveryEvents
    private let event: String
    private let result: Result<String, Error>
    private let versionResult: Result<String, Error>?

    init(events: FleetRecoveryEvents,
         event: String = "setup",
         result: Result<String, Error>,
         versionResult: Result<String, Error>? = nil) {
        self.events = events
        self.event = event
        self.result = result
        self.versionResult = versionResult
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        if arguments == ["--version"] {
            events.append("command")
            completion(versionResult ?? result)
            return
        }
        events.append(arguments.contains("setup-fleet") ? event : "command")
        completion(result)
    }

    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, completion: completion)
    }
}

private final class FleetRecoveryInstaller: HubInstalling {
    private let events: FleetRecoveryEvents
    private let result: Result<String, Error>
    private let onInstall: (() -> Void)?

    init(events: FleetRecoveryEvents,
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

private final class FleetRecoveryService: HubServiceControlling {
    private let events: FleetRecoveryEvents
    private let state: HubServiceLoadState
    private let stopResult: Result<String, Error>

    init(events: FleetRecoveryEvents,
         state: HubServiceLoadState,
         stopResult: Result<String, Error> = .success("")) {
        self.events = events
        self.state = state
        self.stopResult = stopResult
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        events.append("service:\(arguments.last ?? "unknown")")
        if arguments.last == "stop" {
            completion(stopResult)
        } else {
            completion(.success(""))
        }
    }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        events.append("state")
        completion(state)
    }
}
