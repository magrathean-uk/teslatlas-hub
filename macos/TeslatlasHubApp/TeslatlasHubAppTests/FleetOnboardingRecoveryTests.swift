// SPDX-License-Identifier: AGPL-3.0-only

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

    /// C1: the loaded Hub is paused for account work; a failed setup keeps Fleet selected
    /// (no configuration restore) and the single typed recovery restarts what was loaded.
    func testInstalledSetupTimeoutKeepsFleetSelectedAndRestartsLoadedHub() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let runner = FleetRecoveryRunner(events: events,
                                         event: "installed-setup",
                                         result: .failure(HubActionError.commandTimedOut))
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: FleetRecoveryService(events: events, state: .loaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)

        let finished = expectation(description: "setup timeout returned")
        controller.configureFleetAccount(credentials: credentials) { result in
            guard case let .failure(error) = result else { return XCTFail("expected setup timeout") }
            XCTAssertTrue(error.localizedDescription.contains("needs verification"))
            XCTAssertFalse(error.localizedDescription.contains("remains stopped"),
                           "the failure must not claim the Hub stays stopped")
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertNotEqual(try configContents(in: home), original)
        XCTAssertTrue(try configContents(in: home).contains("provider = \"fleet\""))
        XCTAssertEqual(events.values, ["service:pause", "installed-setup", "service:recover"])
    }

    func testFreshSetupFailureDoesNotStartService() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let events = FleetRecoveryEvents()
        let runner = FleetRecoveryRunner(events: events,
                                         event: "embedded-setup",
                                         result: .failure(HubActionError.commandFailed("setup failed")))
        let controller = HubController(commandRunner: runner,
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

    func testInstalledUnconfiguredSetupCreatesCredentialsBeforeStartPreflight() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let runner = FleetRecoveryRunner(events: events,
                                         event: "embedded-setup",
                                         result: .success("configured"))
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: FleetRecoveryService(events: events, state: .loaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)

        let finished = expectation(description: "installed unconfigured setup succeeds")
        controller.configureFleetAccount(credentials: credentials) { result in
            guard case .success = result else { return XCTFail("expected setup success") }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertTrue(try configContents(in: home).contains("provider = \"fleet\""))
        // Pause, set up with the embedded program, then the ordinary Start (whose own
        // writer and preflight come first); the status refresh follows.
        XCTAssertEqual(Array(events.values.prefix(3)), ["service:pause", "embedded-setup", "service:start"])
        XCTAssertFalse(events.values.contains("service:recover"))
    }

    func testInstalledStopFailureDoesNotChangeConfigAndRecoversOnce() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let runner = FleetRecoveryRunner(events: events, result: .success("configured"))
        let service = FleetRecoveryService(events: events,
                                           state: .loaded,
                                           stopResult: .failure(HubActionError.commandFailed("pause failed")))
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)

        let finished = expectation(description: "pause failure returned")
        controller.configureFleetAccount(credentials: credentials) { result in
            guard case let .failure(error) = result else { return XCTFail("expected pause failure") }
            XCTAssertTrue(error.localizedDescription.contains("pause failed"))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertEqual(try configContents(in: home), original)
        XCTAssertEqual(events.values, ["service:pause", "service:recover"])
    }

    private func temporaryHome() throws -> URL {
        let home = HubTestScratch.directory
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
        if arguments.last == "stop" || arguments.last == "pause" {
            completion(stopResult)
        } else {
            completion(.success(""))
        }
    }
    func run(arguments: [String], owner: HubLifecycleToken,
             completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, completion: completion)
    }
    func restartRecordHeld(by owner: HubLifecycleToken) -> Bool { false }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        events.append("state")
        completion(state)
    }

    // New lifecycle requirements, implemented explicitly (no protocol-extension defaults).
    private(set) var recoverCalls = 0
    private(set) var releaseCalls = 0
    private(set) var deleteCalls = 0
    var restartDutyOutstanding: Bool { false }
    func whenRestartDutySettled(_ callback: @escaping () -> Void) { callback() }
    func recoverAfterPause(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) {
        events.append("service:recover")
        recoverCalls += 1
        completion(.success(""))
    }
    func releaseRestartRecord(owner: HubLifecycleToken) {
        events.append("service:release")
        releaseCalls += 1
    }

    func reconcile(owner: HubLifecycleToken, ensure: @escaping () throws -> HubLaunchAgentChange,
                   completion: @escaping (HubReconcileOutcome) -> Void) { completion(.completed) }
    func deleteHubAndData(owner: HubLifecycleToken, completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        deleteCalls += 1
        completion(.failure(HubActionError.preview))
    }
}
