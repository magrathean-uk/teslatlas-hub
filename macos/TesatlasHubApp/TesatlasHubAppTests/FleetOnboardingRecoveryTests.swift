// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Tesatlas_Hub

final class FleetOnboardingRecoveryTests: XCTestCase {
    /// M09-T12 replaces the token-paste set-up: the Fleet route's first account step makes this
    /// Hub a Fleet Hub with `interval_seconds = 0` (nothing is polled before the consent page)
    /// under the same pause, configuration, Start and typed recovery as before. No `setup-fleet`
    /// verb runs here.

    /// The loaded Hub is paused, switched to Fleet with collection off, then started again.
    func testInstalledHubIsPausedSwitchedToFleetWithCollectionOffAndStarted() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let runner = FleetRecoveryRunner(events: events, result: .success("{}"))
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: FleetRecoveryService(events: events, state: .loaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)

        let finished = expectation(description: "prepared")
        controller.prepareFleetHub { result in
            guard case .success = result else { return XCTFail("expected success: \(result)") }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        let config = try configContents(in: home)
        XCTAssertTrue(config.contains("provider = \"fleet\""))
        XCTAssertTrue(config.contains("interval_seconds = 0"), config)
        XCTAssertFalse(config.contains("interval_seconds = 60"))
        XCTAssertFalse(config.contains("fleet_telemetry") || config.contains("fleet_command_proxy"))
        XCTAssertEqual(Array(events.values.prefix(2)), ["service:pause", "service:start"])
        XCTAssertFalse(events.values.contains("setup-fleet"))
        XCTAssertFalse(events.values.contains("service:recover"))
    }

    /// A fresh Mac: the Fleet configuration is written and the ordinary Start runs; no pause.
    func testFreshHubIsConfiguredForFleetAndStarted() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let events = FleetRecoveryEvents()
        let runner = FleetRecoveryRunner(events: events, result: .success("{}"))
        let controller = HubController(commandRunner: runner,
                                       serviceRunner: FleetRecoveryService(events: events, state: .unloaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: false)

        let finished = expectation(description: "prepared")
        controller.prepareFleetHub { result in
            guard case .success = result else { return XCTFail("expected success: \(result)") }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        let config = try configContents(in: home)
        XCTAssertTrue(config.contains("provider = \"fleet\""))
        XCTAssertTrue(config.contains("interval_seconds = 0"), config)
        XCTAssertEqual(events.values.first, "service:start")
        XCTAssertFalse(events.values.contains("service:pause"))
    }

    /// A Hub that is already a Fleet Hub keeps its interval (a reconnect keeps the consent given
    /// earlier) and is neither paused nor restarted.
    func testAFleetHubKeepsItsIntervalAndIsNotPaused() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = "data_dir = \"/tmp/original\"\n\n[collector]\nprovider = \"fleet\"\ninterval_seconds = 300\n"
        try FileManager.default.createDirectory(at: configURL(in: home).deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        try original.write(to: configURL(in: home), atomically: true, encoding: .utf8)
        let events = FleetRecoveryEvents()
        let controller = HubController(commandRunner: FleetRecoveryRunner(events: events, result: .success("{}")),
                                       serviceRunner: FleetRecoveryService(events: events, state: .loaded),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)

        let finished = expectation(description: "prepared")
        controller.prepareFleetHub { result in
            guard case .success = result else { return XCTFail("expected success: \(result)") }
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertEqual(try configContents(in: home), original)
        XCTAssertFalse(events.values.contains { $0.hasPrefix("service:") })
    }

    /// A failed pause changes nothing and runs the single typed recovery.
    func testInstalledPauseFailureDoesNotChangeConfigAndRecoversOnce() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let original = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let service = FleetRecoveryService(events: events,
                                           state: .loaded,
                                           stopResult: .failure(HubActionError.commandFailed("pause failed")))
        let controller = HubController(commandRunner: FleetRecoveryRunner(events: events, result: .success("{}")),
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)

        let finished = expectation(description: "pause failure returned")
        controller.prepareFleetHub { result in
            guard case let .failure(error) = result else { return XCTFail("expected pause failure") }
            XCTAssertTrue(error.localizedDescription.contains("pause failed"))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertEqual(try configContents(in: home), original)
        XCTAssertEqual(events.values, ["service:pause", "service:recover"])
    }

    /// A Start that fails after the switch runs the typed recovery; Fleet stays selected with
    /// collection off, so nothing is polled when the Hub comes back.
    func testInstalledStartFailureRecoversAndKeepsCollectionOff() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try writeOriginalConfig(in: home)
        let events = FleetRecoveryEvents()
        let service = FleetRecoveryService(events: events, state: .loaded,
                                           startResult: .failure(HubActionError.commandFailed("start failed")))
        let controller = HubController(commandRunner: FleetRecoveryRunner(events: events, result: .success("{}")),
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true,
                                       initialSnapshot: .previewRunning)

        let finished = expectation(description: "start failure returned")
        controller.prepareFleetHub { result in
            guard case let .failure(error) = result else { return XCTFail("expected start failure") }
            XCTAssertTrue(error.localizedDescription.contains("start failed"))
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertTrue(try configContents(in: home).contains("interval_seconds = 0"))
        XCTAssertEqual(Array(events.values.prefix(3)), ["service:pause", "service:start", "service:recover"])
    }

    private func temporaryHome() throws -> URL {
        let home = HubTestScratch.directory
            .appendingPathComponent("tesatlas-fleet-recovery-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        return home
    }

    private func configURL(in home: URL) -> URL {
        home.appendingPathComponent("Library/Application Support/Tesatlas Hub/config.toml")
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

    private let startResult: Result<String, Error>

    init(events: FleetRecoveryEvents,
         state: HubServiceLoadState,
         stopResult: Result<String, Error> = .success(""),
         startResult: Result<String, Error> = .success("")) {
        self.events = events
        self.state = state
        self.stopResult = stopResult
        self.startResult = startResult
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        events.append("service:\(arguments.last ?? "unknown")")
        if arguments.last == "stop" || arguments.last == "pause" {
            completion(stopResult)
        } else if arguments.last == "start" {
            completion(startResult)
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
