// SPDX-License-Identifier: AGPL-3.0-only

import Foundation
import XCTest
@testable import Teslatlas_Hub

final class OnboardingHubControllerTests: XCTestCase {
    func testMigrationPreflightFailureHidesCommandOutput() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let secret = "preflight-secret-\(UUID().uuidString)"
        let runner = OnboardingRunner(compatibilityResult: .failure(
            HubActionError.commandExited(1, "SSH failed: \(secret)")
        ))
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       serviceRunner: OnboardingService(),
                                       homeDirectory: home,
                                       serviceInstalledOverride: false)
        let finished = expectation(description: "preflight failure presented safely")

        controller.importTeslaMateOnline(source: "postgresql://reader@localhost/teslamate",
                                         carID: "1",
                                         passwordFile: "/private/password",
                                         encryptionKeyFile: "/private/encryption",
                                         acknowledgeV42CompatibleSchema: true) { result in
            guard case let .failure(error) = result else {
                return XCTFail("failed preflight unexpectedly imported")
            }
            XCTAssertEqual(
                error.localizedDescription,
                "Could not verify TeslaMate. Check the server connection and try again."
            )
            XCTAssertFalse(error.localizedDescription.contains(secret))
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertFalse(HubAppLog.shared.recentText().contains(secret))
    }

    func testMigrationFailurePresentsOnlyActionableCredentialRecovery() {
        let rawDiagnostic = """
        {"event":"migration_progress","completedRows":11000000,"totalRows":11063752,"phase":"copy_positions"}
        INFO imported position batch 10999
        ERROR legacy refresh outcome is unresolved; explicit re-login is required
        """
        let commandError = HubActionError.commandExited(1, rawDiagnostic)

        let presented = HubController.migrationStoppedError(commandError).localizedDescription

        XCTAssertEqual(
            presented,
            "Sign in to Tesla again in TeslaMate, then retry the import."
        )
        XCTAssertFalse(presented.contains("migration_progress"))
        XCTAssertFalse(presented.contains("11000000"))
        XCTAssertEqual(commandError.localizedDescription, rawDiagnostic)
    }

    func testMigrationFailureReasonCodesAreBoundedAndDeterministic() {
        let cases: [(Error, String)] = [
            (HubActionError.commandTimedOut, "timeout"),
            (HubActionError.commandExited(1, "SQLite error: database or disk is full"),
             "disk_space"),
            (HubActionError.commandExited(1, "TeslaMate schema version is incompatible"),
             "schema_version"),
            (HubActionError.commandExited(255, "ssh tunnel: connection refused"),
             "ssh_tunnel"),
            (HubActionError.commandExited(
                1,
                "cannot store TeslaMate token pair: legacy refresh outcome is unresolved"
            ), "credentials"),
            (HubActionError.commandExited(1, "SQLite database is locked"), "sqlite_database"),
            (HubActionError.commandExited(1, "unexpected secret-passphrase=do-not-log"),
             "generic")
        ]

        for (error, expected) in cases {
            XCTAssertEqual(HubController.migrationFailureReasonCode(error), expected)
            XCTAssertLessThanOrEqual(expected.utf8.count, 32)
        }
    }

    func testMigrationProcessFailureLogsOnlyBoundedReasonCode() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let secret = "migration-secret-\(UUID().uuidString)"
        let runner = OnboardingRunner(migrationResult: .failure(
            HubActionError.commandExited(
                1,
                "cannot store TeslaMate token pair: legacy refresh outcome is unresolved \(secret)"
            )
        ))
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       serviceRunner: OnboardingService(),
                                       homeDirectory: home,
                                       serviceInstalledOverride: false)
        let finished = expectation(description: "migration failure recorded")

        controller.importTeslaMateOnline(source: "postgresql://reader@localhost/teslamate",
                                         carID: "1",
                                         passwordFile: "/private/password",
                                         encryptionKeyFile: "/private/encryption",
                                         acknowledgeV42CompatibleSchema: true) { result in
            guard case .failure = result else { return XCTFail("expected migration failure") }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        let diagnostics = HubAppLog.shared.recentText()
        XCTAssertTrue(diagnostics.contains("teslamate_import import.process.failed"))
        XCTAssertTrue(diagnostics.contains("reason_code=credentials"))
        XCTAssertFalse(diagnostics.contains(secret))
    }

    func testMigrationFailureHidesGenericCommandOutput() {
        let rawDiagnostic = """
        {"event":"migration_progress","completedRows":42,"totalRows":100,"phase":"copy_positions"}
        INFO internal import detail
        ERROR unexpected private diagnostic
        """
        let commandError = HubActionError.commandExited(1, rawDiagnostic)

        let presented = HubController.migrationStoppedError(commandError).localizedDescription

        XCTAssertEqual(presented, "TeslaMate import failed; retry the import.")
        XCTAssertFalse(presented.contains("migration_progress"))
        XCTAssertFalse(presented.contains("private diagnostic"))
        XCTAssertEqual(commandError.localizedDescription, rawDiagnostic)
    }

    func testMigrationProgressParsingAcceptsOnlyStructuredEvents() throws {
        let progress = try XCTUnwrap(HubController.parseMigrationProgress(
            #"{"event":"migration_progress","completedRows":123,"totalRows":456,"phase":"copy_positions"}"#
        ))
        XCTAssertEqual(progress.completedRows, 123)
        XCTAssertEqual(progress.totalRows, 456)
        XCTAssertEqual(progress.phase, "copy_positions")

        let clamped = try XCTUnwrap(HubController.parseMigrationProgress(
            #"{"event":"migration_progress","completedRows":5,"totalRows":4}"#
        ))
        XCTAssertEqual(clamped.completedRows, 4)
        XCTAssertNil(HubController.parseMigrationProgress(
            #"{"event":"other","completedRows":1,"totalRows":2}"#
        ))
        XCTAssertNil(HubController.parseMigrationProgress(
            #"{"event":"migration_progress","completedRows":0,"totalRows":0}"#
        ))
        XCTAssertNil(HubController.parseMigrationProgress("not json"))
    }

    func testTeslaMateCompatibilityParsingAcceptsOnlyAcknowledged42CompatibleSchema() throws {
        let compatible = try XCTUnwrap(HubController.parseTeslaMateCompatibility("""
        progress line
        {"status":"compatible","reasonCode":"v4_2_compatible_schema","requiredVersion":"4.2.0","guidance":"Ready to import."}
        """))
        XCTAssertTrue(compatible.compatible)
        XCTAssertEqual(compatible.reasonCode, "v4_2_compatible_schema")
        XCTAssertEqual(compatible.requiredVersion, "4.2.0")
        XCTAssertEqual(compatible.message, "Ready to import.")

        let mismatch = try XCTUnwrap(HubController.parseTeslaMateCompatibility("""
        {"status":"compatible","reasonCode":"exact_4_2_0","requiredVersion":"4.2.0","guidance":"Invalid exactness claim."}
        """))
        XCTAssertFalse(mismatch.compatible)
        XCTAssertEqual(mismatch.reasonCode, "exact_4_2_0")
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
        let runner = OnboardingRunner(events: events, statusResults: [
            .success(readinessStatus(ready: false, collectorInstanceID: nil)),
            .success(readinessStatus(ready: false, collectorInstanceID: nil)),
            .success(readinessStatus(
                ready: true,
                collectorInstanceID: "22222222-2222-4222-8222-222222222222",
                startedAtMs: 4_000_000_000_000
            ))
        ])
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
                                         encryptionKeyFile: "/private/encryption",
                                         acknowledgeV42CompatibleSchema: true) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            migration.fulfill()
        }

        wait(for: [migration], timeout: 2)
        XCTAssertEqual(events.values, ["check", "service:stop", "migrate"])
        XCTAssertEqual(runner.argumentCalls.filter { $0.contains("teslamate-check") }.count, 1)
        XCTAssertEqual(runner.argumentCalls.filter { $0.contains("migrate") }.count, 1)
        XCTAssertTrue(runner.argumentCalls
            .filter { $0.contains("teslamate-check") || $0.contains("migrate") }
            .allSatisfy { $0.contains("--acknowledge-v4-2-compatible-schema") })
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
            "status", "service:start", "status"
        ])
    }

    func testHandoverRejectsStaleReadyLeaseUntilANewCollectorIsReady() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let config = try prepareAwaitingHandover(in: home, previousIntervalSeconds: 60)
        let events = OnboardingEvents()
        let scheduler = ManualOnboardingScheduler()
        let oldInstance = "11111111-1111-4111-8111-111111111111"
        let newInstance = "22222222-2222-4222-8222-222222222222"
        let runner = OnboardingRunner(events: events, statusResults: [
            .success(readinessStatus(
                ready: true,
                collectorInstanceID: oldInstance,
                startedAtMs: 50_000
            )),
            .success(readinessStatus(
                ready: true,
                collectorInstanceID: oldInstance,
                startedAtMs: 50_000
            )),
            .success(readinessStatus(
                ready: false,
                collectorInstanceID: oldInstance,
                startedAtMs: 50_000
            )),
            .success(readinessStatus(
                ready: true,
                collectorInstanceID: newInstance,
                startedAtMs: 101_000
            ))
        ])
        let controller = HubController(
            commandRunner: runner,
            installedCommandRunner: runner,
            serviceRunner: OnboardingService(events: events),
            homeDirectory: home,
            serviceInstalledOverride: true,
            migrationStartupReadinessPollInterval: 1,
            migrationStartupReadinessMaxAttempts: 3,
            migrationStartupReadinessNow: { Date(timeIntervalSince1970: 100) },
            migrationStartupReadinessSchedule: scheduler.schedule
        )
        let finished = expectation(description: "fresh collector becomes ready")
        var completed = false

        controller.acknowledgeMigrationHandoverAndStart { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            completed = true
            finished.fulfill()
        }

        XCTAssertFalse(completed)
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertEqual(scheduler.pendingCount, 1)
        XCTAssertTrue(try String(contentsOf: config).contains("interval_seconds = 60"))

        scheduler.runNext()
        XCTAssertFalse(completed)
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertEqual(scheduler.pendingCount, 1)

        scheduler.runNext()
        wait(for: [finished], timeout: 1)
        XCTAssertFalse(controller.hasPendingMigrationHandover)
        XCTAssertEqual(events.values, [
            "status", "service:start", "status", "status", "status"
        ])
    }

    func testHandoverStartRejectsConcurrentInvocationBeforeStartingAnotherService() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let config = try prepareAwaitingHandover(in: home, previousIntervalSeconds: 60)
        let events = OnboardingEvents()
        let runner = OnboardingRunner(events: events, statusResults: [
            .success(readinessStatus(ready: false, collectorInstanceID: nil)),
            .success(readinessStatus(ready: false, collectorInstanceID: nil))
        ])
        let service = OnboardingService(events: events, holdStartCompletions: true)
        let controller = HubController(
            commandRunner: runner,
            installedCommandRunner: runner,
            serviceRunner: service,
            homeDirectory: home,
            serviceInstalledOverride: true,
            migrationStartupReadinessMaxAttempts: 1
        )
        let firstFinished = expectation(description: "first start reports its held failure")
        let duplicateFinished = expectation(description: "duplicate start is rejected")

        controller.acknowledgeMigrationHandoverAndStart { result in
            guard case .failure = result else {
                return XCTFail("held first start unexpectedly succeeded")
            }
            firstFinished.fulfill()
        }
        controller.acknowledgeMigrationHandoverAndStart { result in
            guard case let .failure(error) = result else {
                return XCTFail("duplicate handover start unexpectedly succeeded")
            }
            XCTAssertEqual(error.localizedDescription, "Hub startup is already in progress.")
            duplicateFinished.fulfill()
        }

        XCTAssertEqual(service.startCallCount, 1)
        XCTAssertEqual(events.values, ["status", "service:start"])
        service.completeHeldStarts(with: .failure(
            HubActionError.commandFailed("held service start failure")
        ))
        wait(for: [duplicateFinished, firstFinished], timeout: 1)
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertTrue(try String(contentsOf: config).contains("interval_seconds = 0"))
    }

    func testHandoverFallsBackToPostRequestStartTimeWhenBaselineStatusIsUnavailable() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = try prepareAwaitingHandover(in: home, previousIntervalSeconds: 60)
        let events = OnboardingEvents()
        let scheduler = ManualOnboardingScheduler()
        let runner = OnboardingRunner(events: events, statusResults: [
            .success(#"{"status":"ok","ready":false}"#),
            .success(readinessStatus(
                ready: true,
                collectorInstanceID: "33333333-3333-4333-8333-333333333333",
                startedAtMs: 99_999
            )),
            .success(readinessStatus(
                ready: true,
                collectorInstanceID: "33333333-3333-4333-8333-333333333333",
                startedAtMs: 100_000
            ))
        ])
        let controller = HubController(
            commandRunner: runner,
            installedCommandRunner: runner,
            serviceRunner: OnboardingService(events: events),
            homeDirectory: home,
            serviceInstalledOverride: true,
            migrationStartupReadinessPollInterval: 1,
            migrationStartupReadinessMaxAttempts: 2,
            migrationStartupReadinessNow: { Date(timeIntervalSince1970: 100) },
            migrationStartupReadinessSchedule: scheduler.schedule
        )
        let finished = expectation(description: "timestamp witness becomes fresh")

        controller.acknowledgeMigrationHandoverAndStart { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertEqual(scheduler.pendingCount, 1)
        scheduler.runNext()
        wait(for: [finished], timeout: 1)
        XCTAssertFalse(controller.hasPendingMigrationHandover)
        XCTAssertEqual(events.values, ["status", "service:start", "status", "status"])
    }

    func testHandoverReadinessFailuresStopHubRestorePauseAndKeepGate() throws {
        let failureCases: [(name: String, status: Result<String, Error>)] = [
            (
                "process failure",
                .failure(HubActionError.commandFailed("private status diagnostic"))
            ),
            ("malformed status", .success("not JSON")),
            (
                "not ready before timeout",
                .success(readinessStatus(ready: false, collectorInstanceID: nil))
            )
        ]

        for failureCase in failureCases {
            let home = try temporaryHome()
            defer { try? FileManager.default.removeItem(at: home) }
            let config = try prepareAwaitingHandover(in: home, previousIntervalSeconds: 60)
            let events = OnboardingEvents()
            let runner = OnboardingRunner(events: events, statusResults: [
                .success(readinessStatus(ready: false, collectorInstanceID: nil)),
                failureCase.status
            ])
            let controller = HubController(
                commandRunner: runner,
                installedCommandRunner: runner,
                serviceRunner: OnboardingService(events: events),
                homeDirectory: home,
                serviceInstalledOverride: true,
                migrationStartupReadinessPollInterval: 1,
                migrationStartupReadinessMaxAttempts: 1,
                migrationStartupReadinessNow: { Date(timeIntervalSince1970: 100) },
                migrationStartupReadinessSchedule: { _, _ in
                    XCTFail("\(failureCase.name) scheduled beyond the bounded final attempt")
                }
            )
            let finished = expectation(description: failureCase.name)

            controller.acknowledgeMigrationHandoverAndStart { result in
                guard case let .failure(error) = result else {
                    return XCTFail("\(failureCase.name) cleared the handover gate")
                }
                XCTAssertFalse(error.localizedDescription.contains("private status diagnostic"))
                finished.fulfill()
            }

            wait(for: [finished], timeout: 1)
            XCTAssertTrue(controller.hasPendingMigrationHandover, failureCase.name)
            XCTAssertTrue(
                try String(contentsOf: config).contains("interval_seconds = 0"),
                failureCase.name
            )
            XCTAssertEqual(
                events.values,
                ["status", "service:start", "status", "service:stop"],
                failureCase.name
            )
        }
    }

    func testHandoverReadinessDeadlineStopsBeforeIssuingAnotherStatusCommand() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let config = try prepareAwaitingHandover(in: home, previousIntervalSeconds: 60)
        let events = OnboardingEvents()
        let scheduler = ManualOnboardingScheduler()
        var now = Date(timeIntervalSince1970: 100)
        let runner = OnboardingRunner(events: events, statusResults: [
            .success(readinessStatus(ready: false, collectorInstanceID: nil)),
            .success(readinessStatus(ready: false, collectorInstanceID: nil)),
            .success(readinessStatus(
                ready: true,
                collectorInstanceID: "44444444-4444-4444-8444-444444444444",
                startedAtMs: 101_000
            ))
        ])
        let controller = HubController(
            commandRunner: runner,
            installedCommandRunner: runner,
            serviceRunner: OnboardingService(events: events),
            homeDirectory: home,
            serviceInstalledOverride: true,
            migrationStartupReadinessPollInterval: 1,
            migrationStartupReadinessTimeout: 1,
            migrationStartupReadinessMaxAttempts: 3,
            migrationStartupReadinessNow: { now },
            migrationStartupReadinessSchedule: scheduler.schedule
        )
        let finished = expectation(description: "readiness deadline expires")

        controller.acknowledgeMigrationHandoverAndStart { result in
            guard case .failure = result else {
                return XCTFail("deadline cleared the handover gate")
            }
            finished.fulfill()
        }

        XCTAssertEqual(scheduler.pendingCount, 1)
        now = Date(timeIntervalSince1970: 101)
        scheduler.runNext()
        wait(for: [finished], timeout: 1)
        XCTAssertTrue(controller.hasPendingMigrationHandover)
        XCTAssertTrue(try String(contentsOf: config).contains("interval_seconds = 0"))
        XCTAssertEqual(events.values, ["status", "service:start", "status", "service:stop"])
    }

    func testHandoverWithCollectorDisabledAcceptsReadyWithoutCollectorLease() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let config = try prepareAwaitingHandover(in: home, previousIntervalSeconds: 0)
        let events = OnboardingEvents()
        let runner = OnboardingRunner(events: events, statusResults: [
            .success(readinessStatus(ready: true, collectorInstanceID: nil)),
            .success(readinessStatus(ready: true, collectorInstanceID: nil))
        ])
        let controller = HubController(
            commandRunner: runner,
            installedCommandRunner: runner,
            serviceRunner: OnboardingService(events: events),
            homeDirectory: home,
            serviceInstalledOverride: true,
            migrationStartupReadinessMaxAttempts: 1
        )
        let finished = expectation(description: "collector-disabled Hub is ready")

        controller.acknowledgeMigrationHandoverAndStart { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 1)
        XCTAssertFalse(controller.hasPendingMigrationHandover)
        XCTAssertTrue(try String(contentsOf: config).contains("interval_seconds = 0"))
        XCTAssertEqual(events.values, ["status", "service:start", "status"])
    }

    func testOnlineMigrationForwardsStructuredProgressOnMainThread() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let runner = OnboardingRunner(progressLines: [
            #"{"event":"migration_progress","completedRows":0,"totalRows":10,"phase":"preparing"}"#,
            "unrelated output",
            #"{"event":"migration_progress","completedRows":7,"totalRows":10,"phase":"copy_positions"}"#
        ])
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       serviceRunner: OnboardingService(),
                                       homeDirectory: home,
                                       serviceInstalledOverride: false)
        let finished = expectation(description: "progress import completes")
        var updates: [HubMigrationProgress] = []

        controller.importTeslaMateOnline(source: "postgresql://reader@localhost/teslamate",
                                         carID: "1",
                                         passwordFile: "/private/password",
                                         encryptionKeyFile: "/private/encryption",
                                         acknowledgeV42CompatibleSchema: true,
                                         progress: {
                                             XCTAssertTrue(Thread.isMainThread)
                                             updates.append($0)
                                         }) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        XCTAssertEqual(updates.map(\.completedRows), [0, 7])
        XCTAssertEqual(updates.map(\.totalRows), [10, 10])
        XCTAssertEqual(updates.compactMap(\.phase), ["preparing", "copy_positions"])
    }

    func testOnlineMigrationSuppressesProgressDeliveredAfterCompletion() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let lateProgress = expectation(description: "no progress after terminal completion")
        lateProgress.isInverted = true
        let runner = OnboardingRunner(lateProgressLines: [
            #"{"event":"migration_progress","completedRows":9,"totalRows":10,"phase":"copy_positions"}"#
        ])
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       serviceRunner: OnboardingService(),
                                       homeDirectory: home,
                                       serviceInstalledOverride: false)
        let finished = expectation(description: "import completed")

        controller.importTeslaMateOnline(source: "postgresql://reader@localhost/teslamate",
                                         carID: "1",
                                         passwordFile: "/private/password",
                                         encryptionKeyFile: "/private/encryption",
                                         acknowledgeV42CompatibleSchema: true,
                                         progress: { _ in lateProgress.fulfill() }) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            finished.fulfill()
        }

        wait(for: [finished], timeout: 2)
        wait(for: [lateProgress], timeout: 0.2)
    }

    func testOnlineMigrationRejectsMissingVersionAcknowledgementBeforeCommands() throws {
        let home = try temporaryHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let events = OnboardingEvents()
        let runner = OnboardingRunner(events: events)
        let controller = HubController(commandRunner: runner,
                                       installedCommandRunner: runner,
                                       serviceRunner: OnboardingService(events: events),
                                       homeDirectory: home,
                                       serviceInstalledOverride: true)
        let rejected = expectation(description: "missing acknowledgement rejected")

        controller.importTeslaMateOnline(source: "postgresql://reader@localhost/teslamate",
                                         carID: "1",
                                         passwordFile: "/private/password",
                                         encryptionKeyFile: "/private/encryption",
                                         acknowledgeV42CompatibleSchema: false) { result in
            guard case let .failure(error) = result else {
                return XCTFail("missing acknowledgement was accepted")
            }
            XCTAssertTrue(error.localizedDescription.contains("4.2.0 or newer"))
            rejected.fulfill()
        }

        wait(for: [rejected], timeout: 1)
        XCTAssertTrue(events.values.isEmpty)
        XCTAssertTrue(runner.argumentCalls.isEmpty)
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
                                         encryptionKeyFile: "/private/encryption",
                                         acknowledgeV42CompatibleSchema: true) { result in
            guard case let .failure(error) = result else {
                return XCTFail("expected missing report failure")
            }
            XCTAssertEqual(
                error.localizedDescription,
                "TeslaMate import failed; retry the import."
            )
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

    private func prepareAwaitingHandover(in home: URL,
                                         previousIntervalSeconds: Int) throws -> URL {
        let folder = home.appendingPathComponent(
            "Library/Application Support/Teslatlas Hub",
            isDirectory: true
        )
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let config = folder.appendingPathComponent("config.toml")
        try """
        data_dir = "\(folder.appendingPathComponent("data").path)"

        [collector]
        provider = "legacy"
        interval_seconds = 0
        """.write(to: config, atomically: true, encoding: .utf8)
        try """
        {"phase":"awaiting_handover","previousIntervalSeconds":\(previousIntervalSeconds),"previousProvider":"legacy"}
        """.write(
            to: folder.appendingPathComponent(".teslamate-handover-pending"),
            atomically: true,
            encoding: .utf8
        )
        return config
    }

    private func readinessStatus(ready: Bool,
                                 collectorInstanceID: String?,
                                 startedAtMs: Int64 = 50_000) -> String {
        let collector: String
        if let collectorInstanceID {
            collector = """
            {"instanceId":"\(collectorInstanceID)","startedAtMs":\(startedAtMs),"heartbeatAtMs":\(startedAtMs + 1_000),"leaseUntilMs":\(startedAtMs + 31_000)}
            """
        } else {
            collector = "null"
        }
        return """
        {"status":"ok","version":"2026.36.1","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1048576},"ready":\(ready),"readinessReason":\(ready ? "null" : "\"collector_absent\""),"provider":"legacy","vehicle":{"vehicleId":"7a5d69ab-8ea8-4056-8b2f-42c41c28ae36","displayName":"Athena","sourceCarId":1,"teslaEid":1,"latestObservationId":1,"latestObservedAtMs":1000,"latestReceivedAtMs":1000},"vehicles":[{"vehicleId":"7a5d69ab-8ea8-4056-8b2f-42c41c28ae36","displayName":"Athena","sourceCarId":1,"teslaEid":1,"latestObservationId":1,"latestObservedAtMs":1000,"latestReceivedAtMs":1000}],"credentials":{"present":true},"legacyCredentials":{"present":true,"expiresAt":null,"nextRefreshAt":null},"fleetCredentials":{"present":false,"expiresAt":null,"nextRefreshAt":null,"scopes":null,"scopeStatus":null},"fleetTelemetry":{"enabled":false,"configured":false,"mode":"disabled","operationalState":"disabled","paidVehicleDataPolling":false,"deliveryPolicy":null},"collector":\(collector)}
        """
    }
}

private final class OnboardingEvents {
    private(set) var values: [String] = []

    func append(_ value: String) {
        values.append(value)
    }
}

private final class ManualOnboardingScheduler {
    private var pending: [() -> Void] = []

    var pendingCount: Int { pending.count }

    func schedule(after _: TimeInterval, action: @escaping () -> Void) {
        pending.append(action)
    }

    func runNext() {
        guard !pending.isEmpty else {
            XCTFail("no scheduled readiness poll")
            return
        }
        pending.removeFirst()()
    }
}

private final class OnboardingRunner: HubCommandRunning {
    private let events: OnboardingEvents?
    private let compatibilityResult: Result<String, Error>
    private let migrationResult: Result<String, Error>
    private let progressLines: [String]
    private let lateProgressLines: [String]
    private var statusResults: [Result<String, Error>]
    private(set) var argumentCalls: [[String]] = []

    init(events: OnboardingEvents? = nil,
         compatibilityResult: Result<String, Error> = .success(
             #"{"status":"compatible","reasonCode":"v4_2_compatible_schema","requiredVersion":"4.2.0","guidance":"Ready."}"#
         ),
         migrationResult: Result<String, Error> = .success(
             "{\"status\":\"imported\",\"captureMode\":\"online-snapshot\"}"
         ),
         progressLines: [String] = [],
         lateProgressLines: [String] = [],
         statusResults: [Result<String, Error>] = []) {
        self.events = events
        self.compatibilityResult = compatibilityResult
        self.migrationResult = migrationResult
        self.progressLines = progressLines
        self.lateProgressLines = lateProgressLines
        self.statusResults = statusResults
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        argumentCalls.append(arguments)
        if arguments.contains("teslamate-check") {
            events?.append("check")
            completion(compatibilityResult)
        } else if arguments.contains("migrate") {
            events?.append("migrate")
            completion(migrationResult)
        } else if arguments.contains("status") {
            events?.append("status")
            if statusResults.isEmpty {
                completion(.success("""
                {"status":"ok","version":"2026.36.1","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1048576},"ready":false,"vehicles":[{"vehicleId":"7a5d69ab-8ea8-4056-8b2f-42c41c28ae36","displayName":"Athena"}],"credentials":{"present":true}}
                """))
            } else {
                completion(statusResults.removeFirst())
            }
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

    func run(arguments: [String],
             onOutputLine: @escaping (String) -> Void,
             completion: @escaping (Result<String, Error>) -> Void) {
        if arguments.contains("migrate") {
            progressLines.forEach(onOutputLine)
        }
        run(arguments: arguments, completion: completion)
        if arguments.contains("migrate") {
            DispatchQueue.main.async {
                self.lateProgressLines.forEach(onOutputLine)
            }
        }
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
    private let holdStartCompletions: Bool
    private var heldStartCompletions: [(Result<String, Error>) -> Void] = []
    private(set) var startCallCount = 0

    init(events: OnboardingEvents? = nil, holdStartCompletions: Bool = false) {
        self.events = events
        self.holdStartCompletions = holdStartCompletions
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        let action = arguments.last ?? "unknown"
        events?.append("service:\(action)")
        if action == "start" {
            startCallCount += 1
            if holdStartCompletions {
                heldStartCompletions.append(completion)
                return
            }
        }
        completion(.success(""))
    }

    func completeHeldStarts(with result: Result<String, Error>) {
        let completions = heldStartCompletions
        heldStartCompletions.removeAll()
        completions.forEach { $0(result) }
    }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        completion(.unloaded)
    }
}
