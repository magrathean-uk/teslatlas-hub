// SPDX-License-Identifier: AGPL-3.0-only

import Darwin
import XCTest
@testable import Teslatlas_Hub

final class DevelopmentHubRuntimeTests: XCTestCase {
    func testApplicationControllerKeepsProductionDefaultWithoutOptIn() throws {
        let controller = try HubController.applicationController(environment: [:])

        XCTAssertNil(controller.developmentConfiguration)
        XCTAssertTrue(controller.allowsServiceInstallation)
        XCTAssertTrue(controller.installer is EmbeddedInstaller)
        XCTAssertEqual(controller.snapshot.health, .needsInstall)
    }

    func testDevelopmentEnvironmentFailsClosedUnlessCompleteAndExplicit() throws {
        XCTAssertThrowsError(try DevelopmentHubConfiguration.from(environment: [
            DevelopmentHubConfiguration.binaryVariable: "/usr/bin/false"
        ]))
        XCTAssertThrowsError(try DevelopmentHubConfiguration.from(environment: [
            DevelopmentHubConfiguration.enableVariable: "true",
            DevelopmentHubConfiguration.binaryVariable: "/usr/bin/false",
            DevelopmentHubConfiguration.configVariable: "/tmp/config.toml",
            DevelopmentHubConfiguration.stateVariable: "/tmp/state",
            DevelopmentHubConfiguration.logVariable: "/tmp/logs"
        ]))
        XCTAssertThrowsError(try DevelopmentHubConfiguration.from(environment: [
            DevelopmentHubConfiguration.enableVariable: "1",
            DevelopmentHubConfiguration.binaryVariable: "/usr/bin/false",
            DevelopmentHubConfiguration.configVariable: "/tmp/config.toml",
            DevelopmentHubConfiguration.stateVariable: "/tmp/state",
            DevelopmentHubConfiguration.logVariable: "/tmp/logs",
            DevelopmentHubConfiguration.modeVariable: "production"
        ]))
        XCTAssertThrowsError(try DevelopmentHubConfiguration.from(environment: [
            DevelopmentHubConfiguration.enableVariable: "1",
            DevelopmentHubConfiguration.binaryVariable: "relative/hub",
            DevelopmentHubConfiguration.configVariable: "/tmp/config.toml",
            DevelopmentHubConfiguration.stateVariable: "/tmp/state",
            DevelopmentHubConfiguration.logVariable: "/tmp/logs"
        ]))
    }

    func testDevelopmentPathsAcceptOwnedPrivateLocations() throws {
        let fixture = try makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }

        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))

        XCTAssertEqual(configuration.binary, fixture.binary)
        XCTAssertEqual(configuration.config, fixture.config)
        XCTAssertEqual(configuration.stateDirectory, fixture.state)
        XCTAssertEqual(configuration.logDirectory, fixture.logs)
        XCTAssertEqual(configuration.mode, .fixture)
        XCTAssertTrue(configuration.serviceLabel.hasPrefix("com.teslatlas.hub.development."))
        XCTAssertEqual(configuration.serviceLabel,
                       try XCTUnwrap(DevelopmentHubConfiguration.from(
                           environment: fixture.environment
                       )).serviceLabel)
    }

    func testDevelopmentPathsRejectSymlinksAndLoosePrivatePermissions() throws {
        let fixture = try makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let linkedState = fixture.root.appendingPathComponent("linked-state")
        try FileManager.default.createSymbolicLink(at: linkedState,
                                                   withDestinationURL: fixture.state)
        var linkedEnvironment = fixture.environment
        linkedEnvironment[DevelopmentHubConfiguration.stateVariable] = linkedState.path
        let linkedConfiguration = try XCTUnwrap(
            DevelopmentHubConfiguration.from(environment: linkedEnvironment)
        )
        XCTAssertThrowsError(try linkedConfiguration.validate(requireConfig: false))

        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o755)],
                                              ofItemAtPath: fixture.logs.path)
        let looseConfiguration = try XCTUnwrap(
            DevelopmentHubConfiguration.from(environment: fixture.environment)
        )
        XCTAssertThrowsError(try looseConfiguration.validate(requireConfig: false))
    }

    func testDevelopmentPathsRejectReplacedBinaryAndLogSymlinkOnRevalidation() throws {
        let fixture = try makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))

        try FileManager.default.removeItem(at: fixture.binary)
        try FileManager.default.createSymbolicLink(at: fixture.binary,
                                                   withDestinationURL: URL(fileURLWithPath: "/usr/bin/false"))
        XCTAssertThrowsError(try configuration.validate(requireConfig: false))

        try FileManager.default.removeItem(at: fixture.binary)
        try writeExecutable(at: fixture.binary)
        let output = fixture.logs.appendingPathComponent("hub.out.log")
        try FileManager.default.createSymbolicLink(at: output,
                                                   withDestinationURL: fixture.config)
        XCTAssertThrowsError(try configuration.validate(requireConfig: false))
    }

    func testPrivateLaunchLogsCreateAndRepairWithoutChangingContents() throws {
        let fixture = try makeFixture(createConfig: true)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        let originalOutput = Data("launchd output stays intact\n".utf8)
        try originalOutput.write(to: configuration.standardOutputLog)
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o644)],
                                              ofItemAtPath: configuration.standardOutputLog.path)

        try configuration.preparePrivateLaunchLogs()

        XCTAssertEqual(try Data(contentsOf: configuration.standardOutputLog), originalOutput)
        XCTAssertEqual(try permissions(of: configuration.standardOutputLog), 0o600)
        XCTAssertEqual(try permissions(of: configuration.standardErrorLog), 0o600)
        XCTAssertNoThrow(try configuration.validate(requireConfig: true))
    }

    func testPrivateLaunchLogsRejectUnsafeExistingEntries() throws {
        do {
            let fixture = try makeFixture()
            defer { try? FileManager.default.removeItem(at: fixture.root) }
            let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
                environment: fixture.environment
            ))
            try FileManager.default.createSymbolicLink(at: configuration.standardOutputLog,
                                                       withDestinationURL: fixture.config)

            XCTAssertThrowsError(try configuration.preparePrivateLaunchLogs())
        }

        do {
            let fixture = try makeFixture()
            defer { try? FileManager.default.removeItem(at: fixture.root) }
            let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
                environment: fixture.environment
            ))
            let source = fixture.root.appendingPathComponent("linked-log-source")
            try Data("do not modify\n".utf8).write(to: source)
            try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o600)],
                                                  ofItemAtPath: source.path)
            try FileManager.default.linkItem(at: source,
                                             to: configuration.standardOutputLog)

            XCTAssertThrowsError(try configuration.preparePrivateLaunchLogs())
            XCTAssertEqual(try Data(contentsOf: source), Data("do not modify\n".utf8))
        }

        do {
            let fixture = try makeFixture()
            defer { try? FileManager.default.removeItem(at: fixture.root) }
            let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
                environment: fixture.environment
            ))
            try Data("unsupported mode\n".utf8).write(to: configuration.standardOutputLog)
            try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o640)],
                                                  ofItemAtPath: configuration.standardOutputLog.path)

            XCTAssertThrowsError(try configuration.preparePrivateLaunchLogs())
            XCTAssertEqual(try permissions(of: configuration.standardOutputLog), 0o640)
        }
    }

    func testRelaunchCanStopWithEveryDevelopmentPathDamaged() throws {
        let fixture = try makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }

        try FileManager.default.removeItem(at: fixture.binary)
        try FileManager.default.createSymbolicLink(
            at: fixture.binary,
            withDestinationURL: URL(fileURLWithPath: "/usr/bin/false")
        )
        try FileManager.default.removeItem(at: fixture.state)
        try FileManager.default.createSymbolicLink(at: fixture.state,
                                                   withDestinationURL: fixture.root)
        try FileManager.default.removeItem(at: fixture.logs)
        try FileManager.default.createSymbolicLink(at: fixture.logs,
                                                   withDestinationURL: fixture.root)
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        let relaunched = try HubController.applicationController(environment: fixture.environment)
        XCTAssertEqual(relaunched.developmentConfiguration, configuration)
        XCTAssertTrue(relaunched.installer is DevelopmentHubInstaller)
        try FileManager.default.createSymbolicLink(
            at: configuration.standardOutputLog,
            withDestinationURL: fixture.config
        )

        XCTAssertThrowsError(try configuration.validate(requireConfig: false))
        XCTAssertNoThrow(try configuration.validateStopTarget())
        let domain = "gui/\(getuid())"
        let service = "\(domain)/\(configuration.serviceLabel)"
        XCTAssertEqual(
            DevelopmentLaunchctlServiceController.commandPlan(
                action: .stop,
                loaded: true,
                domain: domain,
                service: service,
                plist: configuration.plist.path
            ),
            [["bootout", service]]
        )
        XCTAssertTrue(service.contains("/com.teslatlas.hub.development."))
        XCTAssertNotEqual(service, "\(domain)/com.teslatlas.hub")
    }

    func testApplicationControllerInjectsFailClosedDevelopmentInstaller() throws {
        let fixture = try makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let controller = try HubController.applicationController(environment: fixture.environment)

        XCTAssertTrue(controller.installer is DevelopmentHubInstaller)
        let install = expectation(description: "direct development install rejected")
        controller.installer.install { result in
            guard case let .failure(error) = result else {
                XCTFail("development installer unexpectedly installed production service")
                install.fulfill()
                return
            }
            XCTAssertTrue(error.localizedDescription.contains("cannot mutate"))
            install.fulfill()
        }
        let uninstall = expectation(description: "direct development uninstall rejected")
        controller.installer.uninstall(deleteData: true) { result in
            guard case let .failure(error) = result else {
                XCTFail("development installer unexpectedly uninstalled production service")
                uninstall.fulfill()
                return
            }
            XCTAssertTrue(error.localizedDescription.contains("cannot mutate"))
            uninstall.fulfill()
        }
        wait(for: [install, uninstall], timeout: 1)
    }

    func testDevelopmentServicePlansStartStopAndRestartWithoutProductionLabel() {
        let domain = "gui/501"
        let service = "gui/501/com.teslatlas.hub.development.1234"
        let plist = "/owned/.development.plist"

        XCTAssertEqual(DevelopmentLaunchctlServiceController.commandPlan(
            action: .start, loaded: false, domain: domain, service: service, plist: plist
        ), [["bootstrap", domain, plist]])
        XCTAssertEqual(DevelopmentLaunchctlServiceController.commandPlan(
            action: .start, loaded: true, domain: domain, service: service, plist: plist
        ), [["kickstart", service]])
        XCTAssertEqual(DevelopmentLaunchctlServiceController.commandPlan(
            action: .stop, loaded: true, domain: domain, service: service, plist: plist
        ), [["bootout", service]])
        XCTAssertEqual(DevelopmentLaunchctlServiceController.commandPlan(
            action: .restart, loaded: true, domain: domain, service: service, plist: plist
        ), [["bootout", service], ["bootstrap", domain, plist]])
        XCTAssertNotEqual(service, "gui/501/com.teslatlas.hub")
    }

    func testUnsafeServePreflightFailsBeforeLaunchAgentWriteOrBootstrap() throws {
        let fixture = try makeFixture(createConfig: true, mode: .edge)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        var calls: [(URL, [String])] = []
        let controller = DevelopmentLaunchctlServiceController(
            configuration: configuration,
            processRunner: { executable, arguments, _, completion in
                calls.append((executable, arguments))
                completion(.failure(HubActionError.commandFailed(
                    "Edge source-run Serve requires only collector.edge"
                )))
            }
        )
        let rejected = expectation(description: "unsafe source-run preflight rejected")

        controller.run(arguments: ["service", "start"]) { result in
            guard case let .failure(error) = result else {
                XCTFail("unsafe source-run mode reached launchd")
                rejected.fulfill()
                return
            }
            XCTAssertTrue(error.localizedDescription.contains("collector.edge"))
            rejected.fulfill()
        }

        wait(for: [rejected], timeout: 1)
        XCTAssertEqual(calls.count, 1)
        XCTAssertEqual(calls[0].0, configuration.binary)
        XCTAssertEqual(calls[0].1, [
            "--config", configuration.config.path,
            "serve-preflight", "--mode", "edge"
        ])
        XCTAssertFalse(FileManager.default.fileExists(atPath: configuration.plist.path))
        XCTAssertFalse(calls.contains { $0.0.path == "/bin/launchctl" })
    }

    func testValidServePreflightPermitsLaunchPlan() throws {
        let fixture = try makeFixture(createConfig: true, mode: .standalone)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        let service = "gui/\(getuid())/\(configuration.serviceLabel)"
        var calls: [(URL, [String])] = []
        var launchPlanCompleted = false
        let controller = DevelopmentLaunchctlServiceController(
            configuration: configuration,
            processRunner: { executable, arguments, _, completion in
                calls.append((executable, arguments))
                if executable == configuration.binary && arguments.contains("serve-preflight") {
                    completion(.success(#"{"status":"ready","mode":"standalone"}"#))
                } else if executable == configuration.binary {
                    completion(.success(#"{"status":"ok","ready":true}"#))
                } else if arguments == ["print", service] {
                    if launchPlanCompleted {
                        completion(.success(self.runningLaunchctlOutput(
                            configuration: configuration,
                            pid: 4102
                        )))
                    } else {
                        completion(.failure(HubActionError.commandExited(
                            113,
                            "Could not find service \"\(configuration.serviceLabel)\" in domain for user gui: \(getuid())"
                        )))
                    }
                } else if arguments == ["bootstrap", "gui/\(getuid())", configuration.plist.path] {
                    launchPlanCompleted = true
                    completion(.success(""))
                } else {
                    completion(.success(""))
                }
            },
            readinessPollInterval: 0,
            readinessMaxAttempts: 3,
            readinessSchedule: { _, action in action() }
        )
        let started = expectation(description: "valid source-run launch planned")

        controller.run(arguments: ["service", "start"]) { result in
            if case let .failure(error) = result {
                XCTFail("valid preflight did not reach bootstrap: \(error)")
            }
            started.fulfill()
        }

        wait(for: [started], timeout: 1)
        XCTAssertEqual(calls.map(\.1), [
            [
                "--config", configuration.config.path,
                "serve-preflight", "--mode", "standalone"
            ],
            ["print", service],
            ["bootstrap", "gui/\(getuid())", configuration.plist.path],
            ["print", service],
            ["--config", configuration.config.path, "status"],
            ["print", service],
            ["--config", configuration.config.path, "status"]
        ])
        XCTAssertTrue(FileManager.default.fileExists(atPath: configuration.plist.path))
    }

    func testStartFailsWhenIntendedProcessNeverBecomesReady() throws {
        let fixture = try makeFixture(createConfig: true, mode: .standalone)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        let service = "gui/\(getuid())/\(configuration.serviceLabel)"
        var launchPlanCompleted = false
        var cleanupRequested = false
        var statusChecks = 0
        let controller = DevelopmentLaunchctlServiceController(
            configuration: configuration,
            processRunner: { executable, arguments, _, completion in
                if executable == configuration.binary, arguments.contains("serve-preflight") {
                    completion(.success(#"{"status":"ready","mode":"standalone"}"#))
                } else if executable == configuration.binary {
                    statusChecks += 1
                    completion(.success(#"{"status":"ok","ready":false,"readinessReason":"catalogue_unavailable"}"#))
                } else if arguments == ["print", service], !launchPlanCompleted {
                    completion(.failure(HubActionError.commandExited(
                        113,
                        "Could not find service \"\(configuration.serviceLabel)\" in domain for user gui: \(getuid())"
                    )))
                } else if arguments.first == "bootstrap" {
                    launchPlanCompleted = true
                    completion(.success(""))
                } else if arguments == ["bootout", service] {
                    cleanupRequested = true
                    completion(.success(""))
                } else if arguments == ["print", service], cleanupRequested {
                    completion(.failure(HubActionError.commandExited(
                        113,
                        "Could not find service \"\(configuration.serviceLabel)\" in domain for user gui: \(getuid())"
                    )))
                } else if arguments == ["print", service] {
                    completion(.success(self.runningLaunchctlOutput(
                        configuration: configuration,
                        pid: 4103
                    )))
                } else {
                    completion(.success(""))
                }
            },
            readinessPollInterval: 0,
            readinessMaxAttempts: 3,
            readinessSchedule: { _, action in action() }
        )
        let rejected = expectation(description: "persistent unready status rejected")

        controller.run(arguments: ["service", "start"]) { result in
            guard case let .failure(error) = result else {
                XCTFail("persistent ready:false status reported success")
                rejected.fulfill()
                return
            }
            XCTAssertTrue(error.localizedDescription.contains("invalid status response"))
            XCTAssertTrue(error.localizedDescription.contains(#""ready":false"#))
            rejected.fulfill()
        }

        wait(for: [rejected], timeout: 1)
        XCTAssertEqual(statusChecks, 3)
        XCTAssertTrue(cleanupRequested)
    }

    func testPersistentStartupFailurePastDeadlineUnloadsBeforeCompletion() throws {
        let fixture = try makeFixture(createConfig: true, mode: .standalone)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        let service = "gui/\(getuid())/\(configuration.serviceLabel)"
        var now: TimeInterval = 100
        var launchPlanCompleted = false
        var cleanupRequested = false
        var completionObservedCleanup = false
        var statusTimeouts: [TimeInterval] = []
        let controller = DevelopmentLaunchctlServiceController(
            configuration: configuration,
            processRunner: { executable, arguments, timeout, completion in
                if executable == configuration.binary, arguments.contains("serve-preflight") {
                    completion(.success(#"{"status":"ready","mode":"standalone"}"#))
                } else if executable == configuration.binary {
                    statusTimeouts.append(timeout)
                    now += 61
                    completion(.success(#"{"status":"ok","ready":false}"#))
                } else if arguments == ["print", service], !launchPlanCompleted {
                    completion(.failure(HubActionError.commandExited(
                        113,
                        "Could not find service \"\(configuration.serviceLabel)\" in domain for user gui: \(getuid())"
                    )))
                } else if arguments.first == "bootstrap" {
                    launchPlanCompleted = true
                    completion(.success(""))
                } else if arguments == ["bootout", service] {
                    cleanupRequested = true
                    completion(.success(""))
                } else if arguments == ["print", service], cleanupRequested {
                    completion(.failure(HubActionError.commandExited(
                        113,
                        "Could not find service \"\(configuration.serviceLabel)\" in domain for user gui: \(getuid())"
                    )))
                } else if arguments == ["print", service] {
                    completion(.success(self.runningLaunchctlOutput(
                        configuration: configuration,
                        pid: 4104
                    )))
                } else {
                    completion(.success(""))
                }
            },
            readinessPollInterval: 0.5,
            readinessMaxAttempts: 121,
            readinessTimeout: 60,
            readinessSchedule: { _, action in action() },
            readinessClock: { now }
        )
        let rejected = expectation(description: "deadline failure completes after unload")

        controller.run(arguments: ["service", "start"]) { result in
            completionObservedCleanup = cleanupRequested
            guard case let .failure(error) = result else {
                XCTFail("persistent failure past the deadline reported success")
                rejected.fulfill()
                return
            }
            XCTAssertTrue(error.localizedDescription.contains("ready"))
            rejected.fulfill()
        }

        wait(for: [rejected], timeout: 1)
        XCTAssertEqual(statusTimeouts, [30])
        XCTAssertTrue(cleanupRequested)
        XCTAssertTrue(completionObservedCleanup,
                      "the app-facing completion must not unlock until bootout has finished")
    }

    func testStartRejectsCompetingHealthyListenerWhenOwnedProcessIsNotRunning() throws {
        let fixture = try makeFixture(createConfig: true, mode: .standalone)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        let service = "gui/\(getuid())/\(configuration.serviceLabel)"
        try Data("Address already in use (os error 48)\n".utf8)
            .write(to: configuration.standardErrorLog)
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o600)],
                                              ofItemAtPath: configuration.standardErrorLog.path)
        var launchPlanCompleted = false
        var cleanupRequested = false
        var statusChecks = 0
        let controller = DevelopmentLaunchctlServiceController(
            configuration: configuration,
            processRunner: { executable, arguments, _, completion in
                if executable == configuration.binary, arguments.contains("serve-preflight") {
                    completion(.success(#"{"status":"ready","mode":"standalone"}"#))
                } else if executable == configuration.binary {
                    statusChecks += 1
                    completion(.success(#"{"status":"ok","ready":true}"#))
                } else if arguments == ["print", service], !launchPlanCompleted {
                    completion(.failure(HubActionError.commandExited(
                        113,
                        "Could not find service \"\(configuration.serviceLabel)\" in domain for user gui: \(getuid())"
                    )))
                } else if arguments.first == "bootstrap" {
                    launchPlanCompleted = true
                    completion(.success(""))
                } else if arguments == ["bootout", service] {
                    cleanupRequested = true
                    completion(.success(""))
                } else if arguments == ["print", service], cleanupRequested {
                    completion(.failure(HubActionError.commandExited(
                        113,
                        "Could not find service \"\(configuration.serviceLabel)\" in domain for user gui: \(getuid())"
                    )))
                } else if arguments == ["print", service] {
                    completion(.success("""
                    \(service) = {
                        state = waiting
                        program = \(configuration.binary.path)
                        pid = 0
                    }
                    """))
                } else {
                    completion(.success(""))
                }
            },
            readinessPollInterval: 0,
            readinessMaxAttempts: 1,
            readinessSchedule: { _, action in action() }
        )
        let rejected = expectation(description: "competing listener rejected")

        controller.run(arguments: ["service", "start"]) { result in
            guard case let .failure(error) = result else {
                XCTFail("a competing listener counted as the owned process")
                rejected.fulfill()
                return
            }
            XCTAssertTrue(error.localizedDescription.contains("not running the intended binary"))
            XCTAssertTrue(error.localizedDescription.contains("Address already in use"))
            XCTAssertTrue(error.localizedDescription.contains(configuration.logDirectory.path))
            rejected.fulfill()
        }

        wait(for: [rejected], timeout: 1)
        XCTAssertEqual(statusChecks, 0)
        XCTAssertTrue(cleanupRequested)
    }

    func testStartSurfacesLifetimeLockFailure() throws {
        let fixture = try makeFixture(createConfig: true, mode: .standalone)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        let service = "gui/\(getuid())/\(configuration.serviceLabel)"
        try Data("Hub lifetime lock is already held\n".utf8)
            .write(to: configuration.standardErrorLog)
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o600)],
                                              ofItemAtPath: configuration.standardErrorLog.path)
        var launchPlanCompleted = false
        var cleanupRequested = false
        let controller = DevelopmentLaunchctlServiceController(
            configuration: configuration,
            processRunner: { executable, arguments, _, completion in
                if executable == configuration.binary, arguments.contains("serve-preflight") {
                    completion(.success(#"{"status":"ready","mode":"standalone"}"#))
                } else if arguments == ["print", service], !launchPlanCompleted {
                    completion(.failure(HubActionError.commandExited(
                        113,
                        "Could not find service \"\(configuration.serviceLabel)\" in domain for user gui: \(getuid())"
                    )))
                } else if arguments.first == "bootstrap" {
                    launchPlanCompleted = true
                    completion(.success(""))
                } else if arguments == ["bootout", service] {
                    cleanupRequested = true
                    completion(.success(""))
                } else if arguments == ["print", service], cleanupRequested {
                    completion(.failure(HubActionError.commandExited(
                        113,
                        "Could not find service \"\(configuration.serviceLabel)\" in domain for user gui: \(getuid())"
                    )))
                } else if arguments == ["print", service] {
                    completion(.failure(HubActionError.commandExited(
                        113,
                        "Could not find service \"\(configuration.serviceLabel)\" in domain for user gui: \(getuid())"
                    )))
                } else {
                    completion(.success(""))
                }
            },
            readinessPollInterval: 0,
            readinessMaxAttempts: 1,
            readinessSchedule: { _, action in action() }
        )
        let rejected = expectation(description: "lifetime lock surfaced")

        controller.run(arguments: ["service", "start"]) { result in
            guard case let .failure(error) = result else {
                XCTFail("lifetime-lock failure reported success")
                rejected.fulfill()
                return
            }
            XCTAssertTrue(error.localizedDescription.contains("Hub lifetime lock is already held"))
            rejected.fulfill()
        }

        wait(for: [rejected], timeout: 1)
        XCTAssertTrue(cleanupRequested)
    }

    func testDevelopmentLaunchAgentPropagatesExactServeAndTraceEnvironment() throws {
        let fixture = try makeFixture(createConfig: true)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        let controller = DevelopmentLaunchctlServiceController(configuration: configuration)

        let environment = try XCTUnwrap(
            controller.launchAgentPropertyList()["EnvironmentVariables"] as? [String: String]
        )
        XCTAssertEqual(environment, [
            DevelopmentHubConfiguration.enableVariable: "1",
            DevelopmentHubConfiguration.modeVariable: "fixture",
            "RUST_LOG": "info,tower_http=debug"
        ])
    }

    func testDevelopmentLaunchAgentPropagatesExplicitStandaloneAndEdgeModes() throws {
        for mode in [DevelopmentHubMode.standalone, .edge] {
            let fixture = try makeFixture(createConfig: true, mode: mode)
            defer { try? FileManager.default.removeItem(at: fixture.root) }
            let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
                environment: fixture.environment
            ))
            let controller = DevelopmentLaunchctlServiceController(configuration: configuration)
            let environment = try XCTUnwrap(
                controller.launchAgentPropertyList()["EnvironmentVariables"] as? [String: String]
            )

            XCTAssertEqual(configuration.mode, mode)
            XCTAssertEqual(environment[DevelopmentHubConfiguration.enableVariable], "1")
            XCTAssertEqual(environment[DevelopmentHubConfiguration.modeVariable], mode.rawValue)
        }
    }

    func testDevelopmentStatusAndLogsUseExplicitPaths() throws {
        let fixture = try makeFixture(createConfig: true)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        try Data("development output\n".utf8)
            .write(to: configuration.standardOutputLog)
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o600)],
                                              ofItemAtPath: configuration.standardOutputLog.path)
        let runner = DevelopmentStatusRunner(result: .success("""
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"\(fixture.state.path)/catalogue.sqlite3","bytes":1},"ready":true,"credentials":{"present":false}}
        """))
        let controller = HubController(
            commandRunner: runner,
            installedCommandRunner: runner,
            serviceRunner: DevelopmentLoadedService(),
            serviceInstalledOverride: true,
            developmentConfiguration: configuration
        )
        let refreshed = expectation(description: "development status")
        controller.refresh { snapshot in
            XCTAssertEqual(snapshot.health, .running)
            XCTAssertEqual(snapshot.service, "Development Hub running")
            XCTAssertTrue(snapshot.diagnosticLines.contains {
                $0 == "Configuration: \(fixture.config.path)"
            })
            XCTAssertEqual(snapshot.dataDirectory, fixture.state)
            refreshed.fulfill()
        }
        wait(for: [refreshed], timeout: 1)
        XCTAssertEqual(runner.arguments, [["--config", fixture.config.path, "status"]])

        let loadedLogs = expectation(description: "development logs")
        controller.logs { report in
            XCTAssertTrue(report.contains("development output"))
            loadedLogs.fulfill()
        }
        wait(for: [loadedLogs], timeout: 1)
    }

    func testDevelopmentModeBlocksProductionInstallerMutations() throws {
        let fixture = try makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        let controller = HubController(serviceInstalledOverride: true,
                                       developmentConfiguration: configuration)
        XCTAssertFalse(controller.allowsServiceInstallation)

        let install = expectation(description: "install rejected")
        controller.installService { result in
            guard case let .failure(error) = result else { XCTFail(); install.fulfill(); return }
            XCTAssertTrue(error.localizedDescription.contains("cannot install"))
            install.fulfill()
        }
        let uninstall = expectation(description: "uninstall rejected")
        controller.uninstallService(deleteData: true) { result in
            guard case let .failure(error) = result else { XCTFail(); uninstall.fulfill(); return }
            XCTAssertTrue(error.localizedDescription.contains("cannot uninstall"))
            uninstall.fulfill()
        }
        wait(for: [install, uninstall], timeout: 1)
    }

    private struct Fixture {
        let root: URL
        let binary: URL
        let config: URL
        let state: URL
        let logs: URL
        let environment: [String: String]
    }

    private func runningLaunchctlOutput(configuration: DevelopmentHubConfiguration,
                                        pid: Int) -> String {
        """
        gui/\(getuid())/\(configuration.serviceLabel) = {
            state = running
            program = \(configuration.binary.path)
            arguments = {
                \(configuration.binary.path)
                --config
                \(configuration.config.path)
                serve
            }
            pid = \(pid)
        }
        """
    }

    private func makeFixture(createConfig: Bool = false,
                             mode: DevelopmentHubMode? = nil) throws -> Fixture {
        let root = URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true)
            .appendingPathComponent(
                "Library/Caches/TeslatlasHubDevelopmentTests-\(UUID().uuidString)",
                isDirectory: true
            )
        let state = root.appendingPathComponent("state", isDirectory: true)
        let logs = root.appendingPathComponent("logs", isDirectory: true)
        try FileManager.default.createDirectory(at: state, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        for directory in [root, state, logs] {
            try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o700)],
                                                  ofItemAtPath: directory.path)
        }
        let binary = root.appendingPathComponent("teslatlas-hub")
        try writeExecutable(at: binary)
        let config = root.appendingPathComponent("config.toml")
        if createConfig {
            try Data("data_dir = \"\(state.path)\"\n".utf8).write(to: config)
            try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o600)],
                                                  ofItemAtPath: config.path)
        }
        var environment = [
            DevelopmentHubConfiguration.enableVariable: "1",
            DevelopmentHubConfiguration.binaryVariable: binary.path,
            DevelopmentHubConfiguration.configVariable: config.path,
            DevelopmentHubConfiguration.stateVariable: state.path,
            DevelopmentHubConfiguration.logVariable: logs.path
        ]
        if let mode { environment[DevelopmentHubConfiguration.modeVariable] = mode.rawValue }
        return Fixture(
            root: root,
            binary: binary,
            config: config,
            state: state,
            logs: logs,
            environment: environment
        )
    }

    private func writeExecutable(at url: URL) throws {
        try Data("#!/bin/sh\nexit 0\n".utf8).write(to: url)
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o700)],
                                              ofItemAtPath: url.path)
    }

    private func permissions(of url: URL) throws -> Int {
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        return try XCTUnwrap((attributes[.posixPermissions] as? NSNumber)?.intValue)
    }
}

private final class DevelopmentStatusRunner: HubCommandRunning {
    let result: Result<String, Error>
    private(set) var arguments: [[String]] = []

    init(result: Result<String, Error>) { self.result = result }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        self.arguments.append(arguments)
        completion(result)
    }
}

private final class DevelopmentLoadedService: HubServiceControlling {
    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        completion(.success(""))
    }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        completion(.loaded)
    }
}
