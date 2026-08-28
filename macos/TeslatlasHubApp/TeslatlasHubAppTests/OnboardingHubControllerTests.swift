// SPDX-License-Identifier: AGPL-3.0-only

import Foundation
import XCTest
@testable import Teslatlas_Hub

final class OnboardingHubControllerTests: XCTestCase {
    func testTeslaMateCompatibilityParsingAcceptsOnlyExact41Contract() throws {
        let exact = try XCTUnwrap(HubController.parseTeslaMateCompatibility("""
        progress line
        {"status":"compatible","reasonCode":"exact_4_1_1","requiredVersion":"4.1.1","guidance":"Ready to import."}
        """))
        XCTAssertTrue(exact.compatible)
        XCTAssertEqual(exact.reasonCode, "exact_4_1_1")
        XCTAssertEqual(exact.requiredVersion, "4.1.1")
        XCTAssertEqual(exact.message, "Ready to import.")

        let mismatch = try XCTUnwrap(HubController.parseTeslaMateCompatibility("""
        {"status":"compatible","reasonCode":"migration_set_mismatch","requiredVersion":"4.1.1","guidance":"Update required."}
        """))
        XCTAssertFalse(mismatch.compatible)
        XCTAssertEqual(mismatch.reasonCode, "migration_set_mismatch")
        XCTAssertNil(HubController.parseTeslaMateCompatibility("not JSON"))
    }

    func testFleetInvocationPutsCredentialsOnlyOnStandardInput() throws {
        let credentials = HubFleetSetupCredentials(accessToken: "access-secret",
                                                   refreshToken: "refresh-secret",
                                                   clientID: "client-id",
                                                   region: "europe_middle_east_and_africa",
                                                   expiresInSeconds: 7_200)
        let invocation = try HubController.fleetSetupInvocation(
            configPath: URL(fileURLWithPath: "/tmp/hub-config.toml"),
            credentials: credentials
        )

        XCTAssertEqual(invocation.arguments, [
            "--config", "/tmp/hub-config.toml", "setup-fleet", "--all-vehicles"
        ])
        XCTAssertFalse(invocation.arguments.joined(separator: " ").contains("access-secret"))
        XCTAssertFalse(invocation.arguments.joined(separator: " ").contains("refresh-secret"))

        let payload = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(invocation.standardInput.utf8)) as? [String: Any]
        )
        XCTAssertEqual(payload["accessToken"] as? String, "access-secret")
        XCTAssertEqual(payload["refreshToken"] as? String, "refresh-secret")
        XCTAssertEqual(payload["clientId"] as? String, "client-id")
        XCTAssertEqual(payload["region"] as? String, "europe_middle_east_and_africa")
        XCTAssertEqual(payload["expiresInSeconds"] as? NSNumber, 7_200)
    }

    func testFleetProviderTOMLUpdateReplacesOnlyCollectorProvider() {
        let original = """
        data_dir = "/tmp/hub"

        [collector]
          provider = "legacy"
        interval_seconds = 30

        [terrain]
        enabled = false
        """ + "\n"

        XCTAssertEqual(HubController.settingCollectorProvider("fleet", in: original), """
        data_dir = "/tmp/hub"

        [collector]
          provider = "fleet"
        interval_seconds = 30

        [terrain]
        enabled = false
        """ + "\n")
    }

    func testOnboardingDecisionRequiresConfigAndConnectedSnapshot() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: OnboardingRunner(),
                                       installedCommandRunner: OnboardingRunner(),
                                       installer: OnboardingInstaller(),
                                       serviceRunner: OnboardingService(),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)

        XCTAssertTrue(controller.shouldShowOnboarding(for: .previewRunning))
        let configuration = home
            .appendingPathComponent("Library/Application Support/Teslatlas Hub", isDirectory: true)
            .appendingPathComponent("config.toml")
        try FileManager.default.createDirectory(at: configuration.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        try "data_dir = \"/tmp/hub\"\n".write(to: configuration, atomically: true, encoding: .utf8)

        XCTAssertFalse(controller.shouldShowOnboarding(for: .previewRunning))
        XCTAssertTrue(controller.shouldShowOnboarding(for: .firstRun))
    }

    func testOnlineMigrationStaysStoppedUntilHandoverAcknowledgement() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let events = OnboardingEvents()
        let runner = OnboardingRunner(events: events)
        let service = OnboardingService(events: events)
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       installer: OnboardingInstaller(events: events),
                                       serviceRunner: service,
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let migration = expectation(description: "online migration completes")

        controller.importTeslaMateOnline(source: "postgresql://reader@localhost/teslamate",
                                         carID: "1",
                                         passwordFile: "/private/password",
                                         encryptionKeyFile: "/private/encryption") { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            migration.fulfill()
        }

        wait(for: [migration], timeout: 2)
        XCTAssertEqual(events.values, ["check", "service:stop", "migrate"])
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        let config = home.appendingPathComponent(
            "Library/Application Support/Teslatlas Hub/config.toml"
        )
        XCTAssertTrue(try String(contentsOf: config).contains("interval_seconds = 0"))

        let blocked = expectation(description: "marker blocks start")
        controller.startHub { result in
            guard case let .failure(error) = result else { return XCTFail("start unexpectedly allowed") }
            XCTAssertTrue(error.localizedDescription.contains("handover"))
            blocked.fulfill()
        }
        wait(for: [blocked], timeout: 1)
        XCTAssertEqual(events.values, ["check", "service:stop", "migrate"])

        let verified = expectation(description: "migration checks pass")
        controller.runOnboardingChecks(expectRunning: false) { result in
            switch result {
            case let .success(checks): XCTAssertTrue(checks.allSatisfy(\.passed))
            case let .failure(error): XCTFail(error.localizedDescription)
            }
            verified.fulfill()
        }
        wait(for: [verified], timeout: 2)
        XCTAssertEqual(events.values, [
            "check", "service:stop", "migrate", "service:stop", "status", "doctor"
        ])

        let acknowledged = expectation(description: "acknowledgement starts Hub")
        controller.acknowledgeMigrationHandoverAndStart { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            acknowledged.fulfill()
        }
        wait(for: [acknowledged], timeout: 1)
        XCTAssertFalse(controller.hasPendingMigrationHandover)
        XCTAssertTrue(try String(contentsOf: config).contains("interval_seconds = 60"))
        XCTAssertEqual(events.values, [
            "check", "service:stop", "migrate", "service:stop", "status", "doctor",
            "service:start"
        ])
    }

    func testOnlineMigrationWithoutCompletionReportKeepsHandoverGateAndHubStopped() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let events = OnboardingEvents()
        let runner = OnboardingRunner(
            events: events,
            migrationResult: .success("{\"status\":\"ok\"}")
        )
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       serviceRunner: OnboardingService(events: events),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let migration = expectation(description: "missing report rejected")

        controller.importTeslaMateOnline(source: "postgresql://reader@localhost/teslamate",
                                         carID: "1",
                                         passwordFile: "/private/password",
                                         encryptionKeyFile: "/private/encryption") { result in
            guard case let .failure(error) = result else {
                return XCTFail("expected missing report failure")
            }
            XCTAssertTrue(error.localizedDescription.contains("remains stopped"))
            migration.fulfill()
        }

        wait(for: [migration], timeout: 2)
        XCTAssertEqual(events.values, ["check", "service:stop", "migrate"])
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        let config = home.appendingPathComponent(
            "Library/Application Support/Teslatlas Hub/config.toml"
        )
        XCTAssertTrue(try String(contentsOf: config).contains("interval_seconds = 0"))
    }

    private func temporaryHome() throws -> URL {
        let home = FileManager.default.temporaryDirectory
            .appendingPathComponent("teslatlas-hub-onboarding-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: false)
        return home
    }
}

private final class OnboardingEvents {
    private(set) var values: [String] = []

    func append(_ value: String) {
        values.append(value)
    }
}

private final class OnboardingRunner: HubCommandRunning {
    private let events: OnboardingEvents?
    private let migrationResult: Result<String, Error>

    init(events: OnboardingEvents? = nil,
         migrationResult: Result<String, Error> = .success(
             "{\"status\":\"imported\",\"captureMode\":\"online-snapshot\"}"
         )) {
        self.events = events
        self.migrationResult = migrationResult
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        if arguments.contains("teslamate-check") {
            events?.append("check")
            completion(.success("""
            {"status":"compatible","reasonCode":"exact_4_1_1","requiredVersion":"4.1.1","guidance":"Ready."}
            """))
        } else if arguments.contains("migrate") {
            events?.append("migrate")
            completion(migrationResult)
        } else if arguments.contains("status") {
            events?.append("status")
            completion(.success("""
            {"status":"ok","version":"1.0.0-beta.1","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1048576},"ready":false,"vehicles":[{"vehicleId":"7a5d69ab-8ea8-4056-8b2f-42c41c28ae36","displayName":"Athena"}],"credentials":{"present":true}}
            """))
        } else if arguments.contains("doctor") {
            events?.append("doctor")
            completion(.success("{\"status\":\"ok\"}"))
        } else {
            completion(.failure(HubActionError.commandFailed("unexpected command")))
        }
    }

    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, completion: completion)
    }
}

private final class OnboardingInstaller: HubInstalling {
    private let events: OnboardingEvents?

    init(events: OnboardingEvents? = nil) {
        self.events = events
    }

    func install(completion: @escaping (Result<String, Error>) -> Void) {
        events?.append("install")
        completion(.success(""))
    }

    func uninstall(deleteData: Bool, completion: @escaping (Result<String, Error>) -> Void) {
        completion(.success(""))
    }
}

private final class OnboardingService: HubServiceControlling {
    private let events: OnboardingEvents?

    init(events: OnboardingEvents? = nil) {
        self.events = events
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        events?.append("service:\(arguments.last ?? "unknown")")
        completion(.success(""))
    }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        completion(.unloaded)
    }
}
