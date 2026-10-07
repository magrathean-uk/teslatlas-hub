// SPDX-License-Identifier: AGPL-3.0-only

import Darwin
import AppKit
import XCTest
import Security
@testable import Teslatlas_Hub

final class DevelopmentHubRuntimeTests: XCTestCase {
    // A visible first-run fixture must not queue the application's ordinary
    // last-window Quit and later exit the test host when Process.waitUntilExit
    // pumps AppKit events. This private invisible anchor lives only in the host.
    private static let firstRunTestHostAnchor: NSWindow = {
        let window = NSWindow(contentRect: NSRect(x: -100, y: -100, width: 1, height: 1),
                              styleMask: [], backing: .buffered, defer: false)
        window.alphaValue = 0
        window.isExcludedFromWindowsMenu = true
        window.orderFront(nil)
        return window
    }()

    func testExecutableAndConfigRejectGroupWritableAncestors() throws {
        let fixture = try makeFixture(createConfig: true)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let groupParent = fixture.root.appendingPathComponent("group-parent", isDirectory: true)
        try FileManager.default.createDirectory(at: groupParent, withIntermediateDirectories: false)
        try FileManager.default.setAttributes([.posixPermissions: 0o775], ofItemAtPath: groupParent.path)
        let binary = groupParent.appendingPathComponent("teslatlas-hub")
        try writeExecutable(at: binary)
        let config = groupParent.appendingPathComponent("config.toml")
        try Data("test-only\n".utf8).write(to: config)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: config.path)
        for (variable, path) in [(DevelopmentHubConfiguration.binaryVariable, binary.path), (DevelopmentHubConfiguration.configVariable, config.path)] {
            var environment = fixture.environment
            environment[variable] = path
            let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(environment: environment))
            XCTAssertThrowsError(try configuration.validate(requireConfig: true))
            if variable == DevelopmentHubConfiguration.configVariable {
                try FileManager.default.removeItem(at: config)
                XCTAssertThrowsError(try configuration.validate(requireConfig: false))
            }
        }
        try XCTUnwrap(DevelopmentHubConfiguration.from(environment: fixture.environment)).validate(requireConfig: true)
    }
    func testControlDirectoryRejectsGroupWritableAncestor() throws {
        let fixture = try makeFixture(createConfig: true)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let control = fixture.root.appendingPathComponent("control", isDirectory: true)
        try FileManager.default.createDirectory(at: control, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        var environment = fixture.environment
        environment[DevelopmentHubConfiguration.controlVariable] = control.path
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(environment: environment))
        try configuration.validate(requireConfig: true)
        try FileManager.default.setAttributes([.posixPermissions: 0o775], ofItemAtPath: fixture.root.path)
        XCTAssertThrowsError(try configuration.validate(requireConfig: true))
    }
    func testControlDirectoryDefaultsToExistingDevRuntime() throws {
        let fixture = try makeFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        var environment = fixture.environment
        environment.removeValue(forKey: DevelopmentHubConfiguration.controlVariable)
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(environment: environment))
        XCTAssertEqual(configuration.controlDirectory.path, NSHomeDirectory() + "/dev/runtime")
        XCTAssertEqual(configuration.plist.deletingLastPathComponent(), configuration.controlDirectory)
        let descriptor = try DevelopmentEventLog.validateDirectoryReadOnly(configuration.controlDirectory, allowOwnedGroupWritableAncestors: false)
        close(descriptor)
    }
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
        ), [["bootout", service], ["bootstrap", domain, plist]])
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

    func testInvalidHistoryOnlyControlStopsBeforeServePreflightOrLaunch() throws {
        let fixture = try makeFixture(createConfig: true, mode: .standalone)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        let marker = HistoryOnlyControl.url(for: configuration.config)
        try Data("unexpected\n".utf8).write(to: marker, options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o600)],
                                              ofItemAtPath: marker.path)
        var calls: [(URL, [String])] = []
        let controller = DevelopmentLaunchctlServiceController(
            configuration: configuration,
            processRunner: { executable, arguments, _, completion in
                calls.append((executable, arguments))
                completion(.success(""))
            }
        )
        let rejected = expectation(description: "invalid mode marker rejected")
        controller.run(arguments: ["service", "start"]) { result in
            guard case .failure = result else { return XCTFail("invalid marker was accepted") }
            rejected.fulfill()
        }
        wait(for: [rejected], timeout: 1)
        XCTAssertTrue(calls.isEmpty)
        XCTAssertFalse(FileManager.default.fileExists(atPath: configuration.plist.path))
    }

    func testHistoryOnlyControlSelectsStrictStandaloneServePreflight() throws {
        let fixture = try makeFixture(createConfig: true, mode: .standalone)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        try HistoryOnlyControl.write(for: configuration.config)
        var calls: [(URL, [String])] = []
        let controller = DevelopmentLaunchctlServiceController(
            configuration: configuration,
            processRunner: { executable, arguments, _, completion in
                calls.append((executable, arguments))
                completion(.failure(HubActionError.commandFailed("configuration rejected")))
            }
        )
        let rejected = expectation(description: "history preflight rejected before launch")
        controller.run(arguments: ["service", "start"]) { result in
            guard case .failure = result else { return XCTFail("unsafe start was allowed") }
            rejected.fulfill()
        }
        wait(for: [rejected], timeout: 1)
        XCTAssertEqual(calls.count, 1)
        let preflight = try XCTUnwrap(calls.first)
        XCTAssertEqual(preflight.1, ["--config", configuration.config.path,
                                     "serve-preflight", "--mode", "standalone", "--history-only"])
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
        var httpsChecks = 0
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
            readinessSchedule: { _, action in action() },
            httpsProbe: { selected, pid, timeout, completion in
                XCTAssertEqual(pid, 4102)
                XCTAssertEqual(selected, configuration)
                XCTAssertEqual(timeout, 5)
                httpsChecks += 1
                completion(true)
            }
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
        XCTAssertEqual(httpsChecks, 2)
        XCTAssertTrue(FileManager.default.fileExists(atPath: configuration.plist.path))
        XCTAssertEqual(configuration.plist.deletingLastPathComponent(), fixture.root)
        XCTAssertEqual(try permissions(of: configuration.plist), 0o600)
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.state.appendingPathComponent(configuration.plist.lastPathComponent).path))
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

    func testLivePIDAndHealthyCatalogueWithoutHTTPSNeverCompletesStartup() throws {
        let fixture = try makeFixture(createConfig: true, mode: .standalone)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(
            environment: fixture.environment
        ))
        let service = "gui/\(getuid())/\(configuration.serviceLabel)"
        var launchPlanCompleted = false
        var cleanupRequested = false
        var statusChecks = 0
        var httpsChecks = 0
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
            readinessSchedule: { _, action in action() },
            httpsProbe: { _, _, _, completion in httpsChecks += 1; completion(false) }
        )
        let rejected = expectation(description: "persistent unready status rejected")

        controller.run(arguments: ["service", "start"]) { result in
            guard case let .failure(error) = result else {
                XCTFail("persistent ready:false status reported success")
                rejected.fulfill()
                return
            }
            XCTAssertTrue(error.localizedDescription.contains("ready over HTTPS"))
            rejected.fulfill()
        }

        wait(for: [rejected], timeout: 1)
        XCTAssertEqual(statusChecks, 3)
        XCTAssertEqual(httpsChecks, 3)
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
            "RUST_LOG": "info,tower_http=debug",
            DevelopmentHubConfiguration.logVariable: configuration.logDirectory.path
        ])
        XCTAssertEqual(controller.launchAgentPropertyList()["StandardOutPath"] as? String, "/dev/null")
        XCTAssertEqual(controller.launchAgentPropertyList()["StandardErrorPath"] as? String, "/dev/null")
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

    func testHTTPSReadinessEndpointRejectsUnsafeAndUnsupportedConfiguration() throws {
        let safe = "[tls]\npublic_url = 'https://127.0.0.1:21446' # local\ncertificate_path = '/owner/tls/cert.pem'\nprivate_key_path = '/never/read/key.pem'\n"
        let endpoint = try DevelopmentHubHTTPSReadiness.endpoint(Data(safe.utf8))
        XCTAssertEqual(endpoint.url.absoluteString, "https://127.0.0.1:21446/readyz")
        XCTAssertEqual(endpoint.certificate.path, "/owner/tls/cert.pem")
        for invalid in [
            safe.replacingOccurrences(of: "https://", with: "http://"),
            safe.replacingOccurrences(of: "127.0.0.1:21446", with: "user:password@127.0.0.1:21446"),
            safe.replacingOccurrences(of: "21446'", with: "21446/redirect'"),
            safe.replacingOccurrences(of: "21446'", with: "21446/?credential=value'"),
            safe.replacingOccurrences(of: "'/owner/tls/cert.pem'", with: "'../cert.pem'"),
            safe + "public_url = 'https://other/'\n",
            safe + "[tls]\n",
            safe.replacingOccurrences(of: "[tls]", with: "[collector]"),
            "tls = { public_url = 'https://127.0.0.1:21446', certificate_path = '/owner/tls/cert.pem' }"
        ] { XCTAssertThrowsError(try DevelopmentHubHTTPSReadiness.endpoint(Data(invalid.utf8))) }
    }

    func testHTTPSReadinessProtectedReadsRejectSymlinksAndPermissionChanges() throws {
        let fixture = try makeFixture(createConfig: true)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        XCTAssertEqual(try DevelopmentHubHTTPSReadiness.protectedData(fixture.config), Data("data_dir = \"\(fixture.state.path)\"\n".utf8))
        try FileManager.default.setAttributes([.posixPermissions: 0o640], ofItemAtPath: fixture.config.path)
        XCTAssertThrowsError(try DevelopmentHubHTTPSReadiness.protectedData(fixture.config))
        XCTAssertEqual(try permissions(of: fixture.config), 0o640)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: fixture.config.path)
        let link = fixture.root.appendingPathComponent("linked-config")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: fixture.config)
        XCTAssertThrowsError(try DevelopmentHubHTTPSReadiness.protectedData(link))
        let fifo = fixture.root.appendingPathComponent("fifo-config")
        XCTAssertEqual(mkfifo(fifo.path, 0o600), 0)
        XCTAssertThrowsError(try DevelopmentHubHTTPSReadiness.protectedData(fifo))
    }

    func testReadinessCertificateAdmitsOwnedVolumeAncestorsWithoutRelaxingConfig() throws {
        let fixture = try makeHTTPSFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root); ReadinessURLProtocol.handler = nil }
        let volume = fixture.root.appendingPathComponent("volume", isDirectory: true)
        let dev = volume.appendingPathComponent("dev", isDirectory: true)
        let tls = dev.appendingPathComponent("private-state/tls", isDirectory: true)
        try FileManager.default.createDirectory(at: tls, withIntermediateDirectories: true,
                                               attributes: [.posixPermissions: 0o700])
        for ancestor in [volume, dev] {
            try FileManager.default.setAttributes([.posixPermissions: 0o775], ofItemAtPath: ancestor.path)
            XCTAssertEqual(chown(ancestor.path, getuid(), getgid()), 0)
        }
        let certificate = tls.appendingPathComponent("certificate.pem")
        try FileManager.default.moveItem(at: fixture.root.appendingPathComponent("certificate.pem"), to: certificate)
        let original = try String(contentsOf: fixture.config, encoding: .utf8)
        let config = original.replacingOccurrences(of: fixture.root.appendingPathComponent("certificate.pem").path,
                                                  with: certificate.path)
        try Data(config.utf8).write(to: fixture.config)
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(environment: fixture.environment))
        try configuration.validate(requireConfig: true)
        let admittedCertificate = try DevelopmentHubHTTPSReadiness.protectedData(certificate,
                                                                               allowOwnedGroupWritableAncestors: true)
        XCTAssertFalse(admittedCertificate.isEmpty)
        XCTAssertThrowsError(try DevelopmentHubHTTPSReadiness.protectedData(certificate),
                             "the configuration reader must retain strict ancestors")
        let digest = DevelopmentHubHTTPSReadiness.digest(Data(config.utf8))
        ReadinessURLProtocol.handler = { instance in
            instance.client?.urlProtocol(instance, didReceive: HTTPURLResponse(url: instance.request.url!,
                statusCode: 200, httpVersion: "HTTP/1.1", headerFields: [
                    "x-teslatlas-native-config-sha256": digest, "x-teslatlas-native-process-id": "4102"
                ])!, cacheStoragePolicy: .notAllowed)
            instance.client?.urlProtocol(instance, didLoad: Data(#"{"status":"ready"}"#.utf8))
            instance.client?.urlProtocolDidFinishLoading(instance)
        }
        let done = expectation(description: "readiness reaches transport with owned volume ancestors")
        let settings = URLSessionConfiguration.ephemeral
        settings.protocolClasses = [ReadinessURLProtocol.self]
        DevelopmentHubHTTPSReadiness.probe(configuration: configuration, processID: 4102, timeout: 1,
            sessionConfiguration: settings) { ready in XCTAssertTrue(ready); done.fulfill() }
        wait(for: [done], timeout: 2)

        try FileManager.default.setAttributes([.posixPermissions: 0o777], ofItemAtPath: volume.path)
        XCTAssertThrowsError(try DevelopmentHubHTTPSReadiness.protectedData(certificate, allowOwnedGroupWritableAncestors: true))
        try FileManager.default.setAttributes([.posixPermissions: 0o775], ofItemAtPath: volume.path)
        try FileManager.default.setAttributes([.posixPermissions: 0o775], ofItemAtPath: tls.path)
        XCTAssertThrowsError(try DevelopmentHubHTTPSReadiness.protectedData(certificate, allowOwnedGroupWritableAncestors: true),
                             "the immediate TLS directory must remain private")
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: tls.path)
        try FileManager.default.setAttributes([.posixPermissions: 0o640], ofItemAtPath: certificate.path)
        XCTAssertThrowsError(try DevelopmentHubHTTPSReadiness.protectedData(certificate, allowOwnedGroupWritableAncestors: true))
        XCTAssertEqual(try permissions(of: certificate), 0o640, "admission must never repair permissions")
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: certificate.path)
        let alias = fixture.root.appendingPathComponent("volume-alias", isDirectory: true)
        try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: volume)
        XCTAssertThrowsError(try DevelopmentHubHTTPSReadiness.protectedData(alias.appendingPathComponent("dev/private-state/tls/certificate.pem"),
                                                                          allowOwnedGroupWritableAncestors: true))
    }

    func testReadOnlyDirectoryPolicyRejectsForeignOwnerAndGroupWrite() {
        var entry = stat()
        entry.st_mode = mode_t(S_IFDIR | 0o775)
        entry.st_uid = getuid()
        entry.st_gid = getgid()
        func admits(_ leaf: Bool = false, exception: Bool = true) -> Bool {
            DevelopmentEventLog.admitsDirectoryEntry(entry, leaf: leaf,
                                                     allowOwnedGroupWritableAncestors: exception)
        }
        XCTAssertTrue(admits())
        XCTAssertFalse(admits(exception: false))
        XCTAssertFalse(admits(true), "owned-group permission never relaxes the private leaf")
        entry.st_gid = getgid() &+ 1
        XCTAssertFalse(admits(), "a different group cannot receive the exception")
        entry.st_gid = getgid()
        entry.st_uid = 0
        XCTAssertFalse(admits(), "root ownership alone does not admit group-writable ancestors")
        entry.st_uid = getuid() &+ 1
        entry.st_mode = mode_t(S_IFDIR | 0o755)
        XCTAssertFalse(admits(), "a foreign owner is rejected even without group write")
        entry.st_uid = getuid()
        entry.st_mode = mode_t(S_IFDIR | 0o777)
        XCTAssertFalse(admits(), "world write is always rejected")
        entry.st_mode = mode_t(S_IFDIR | 0o700)
        XCTAssertTrue(admits(true))
    }

    func testDevelopmentRefreshRequiresLiveHTTPSAndRetainsKnownDataWhenStatusFails() throws {
        let fixture = try makeFixture(createConfig: true, mode: .standalone)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(environment: fixture.environment))
        var liveReady = false
        var probeChecks = 0
        let service = DevelopmentLaunchctlServiceController(configuration: configuration,
            processRunner: { _, _, _, completion in
                completion(.success(self.runningLaunchctlOutput(configuration: configuration, pid: 4102)))
            }, httpsProbe: { _, _, _, completion in probeChecks += 1; completion(liveReady) })
        let runner = DevelopmentStatusRunner(result: .success("""
        {"status":"ok","version":"\(HubRelease.bundledVersion)","database":{"path":"\(fixture.state.path)/catalogue.sqlite3","bytes":2040000000},"ready":true,"credentials":{"present":true},"vehicles":[{"vehicleId":"B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5","displayName":"Existing"}]}
        """))
        let controller = HubController(commandRunner: runner, installedCommandRunner: runner,
            serviceRunner: service, serviceInstalledOverride: true, developmentConfiguration: configuration)
        let unavailable = expectation(description: "live PID without HTTPS is unavailable")
        controller.refresh { snapshot in
            XCTAssertEqual(snapshot.health, .degraded)
            XCTAssertTrue(snapshot.service.contains("HTTPS readiness unavailable"))
            XCTAssertEqual(snapshot.controlVehicles.count, 1)
            XCTAssertNil(snapshot.controlVehicleID)
            unavailable.fulfill()
        }
        wait(for: [unavailable], timeout: 1)
        liveReady = true
        let ready = expectation(description: "trusted HTTPS restores running")
        controller.refresh { snapshot in
            XCTAssertEqual(snapshot.health, .running)
            XCTAssertEqual(snapshot.service, "Development Hub running")
            ready.fulfill()
        }
        wait(for: [ready], timeout: 1)
        let knownDatabase = controller.snapshot.database
        runner.result = .failure(HubActionError.commandFailed("status unavailable"))
        let failed = expectation(description: "status failure retains data without claiming healthy")
        controller.refresh { snapshot in
            XCTAssertEqual(snapshot.health, .degraded)
            XCTAssertTrue(snapshot.statusUnavailable)
            XCTAssertEqual(snapshot.controlVehicles.count, 1)
            XCTAssertEqual(snapshot.controlVehicles.first?.displayName, "Existing")
            XCTAssertEqual(snapshot.database, "Last known: \(knownDatabase)")
            XCTAssertEqual(snapshot.databaseState, .unknown)
            XCTAssertNil(snapshot.controlVehicleID)
            failed.fulfill()
        }
        wait(for: [failed], timeout: 1)
        XCTAssertEqual(probeChecks, 3)
    }

    func testReadinessCertificatePinEnforcesHostIdentityAndExpiry() throws {
        let fixture = try makeFixture(createConfig: true)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        func certificate(_ name: String) throws -> SecCertificate {
            let settings = fixture.root.appendingPathComponent("\(name).cnf")
            try Data("[req]\ndistinguished_name=dn\nx509_extensions=server\nprompt=no\n[dn]\nCN=localhost\n[server]\nsubjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n".utf8).write(to: settings)
            let der = fixture.root.appendingPathComponent("\(name).der")
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/openssl")
            process.arguments = ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-config", settings.path,
                                 "-keyout", fixture.root.appendingPathComponent("\(name).key").path, "-outform", "DER", "-out", der.path]
            process.standardOutput = FileHandle.nullDevice
            process.standardError = FileHandle.nullDevice
            try process.run(); process.waitUntilExit()
            XCTAssertEqual(process.terminationStatus, 0)
            return try XCTUnwrap(SecCertificateCreateWithData(nil, try Data(contentsOf: der) as CFData))
        }
        let selected = try certificate("selected")
        let competing = try certificate("competing")
        func trust(_ cert: SecCertificate, date: Date = Date()) throws -> SecTrust {
            var value: SecTrust?
            XCTAssertEqual(SecTrustCreateWithCertificates(cert, SecPolicyCreateSSL(true, "localhost" as CFString), &value), errSecSuccess)
            let result = try XCTUnwrap(value)
            XCTAssertEqual(SecTrustSetVerifyDate(result, date as CFDate), errSecSuccess)
            return result
        }
        XCTAssertTrue(DevelopmentHubHTTPSReadiness.accepts(trust: try trust(selected), certificate: selected, host: "localhost"))
        XCTAssertFalse(DevelopmentHubHTTPSReadiness.accepts(trust: try trust(selected), certificate: selected, host: "other.invalid"))
        XCTAssertFalse(DevelopmentHubHTTPSReadiness.accepts(trust: try trust(competing), certificate: selected, host: "localhost"))
        XCTAssertFalse(DevelopmentHubHTTPSReadiness.accepts(trust: try trust(selected, date: Date().addingTimeInterval(3 * 86_400)), certificate: selected, host: "localhost"))
    }

    func testUnavailableInitialStatusDoesNotClaimAnEmptyVehicleCatalogue() throws {
        let fixture = try makeFixture(createConfig: true)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(environment: fixture.environment))
        let runner = DevelopmentStatusRunner(result: .failure(HubActionError.commandFailed("status unavailable")))
        let controller = HubController(commandRunner: runner, installedCommandRunner: runner,
            serviceRunner: DevelopmentLoadedService(), serviceInstalledOverride: true,
            developmentConfiguration: configuration)
        let done = expectation(description: "unavailable initial status remains unknown")
        var window: MainWindowController?
        window = MainWindowController(controller: controller) { snapshot in
            XCTAssertEqual(snapshot.health, .degraded)
            XCTAssertTrue(snapshot.statusUnavailable)
            func labels(_ view: NSView?) -> [String] {
                guard let view else { return [] }
                return ((view as? NSTextField).map { [$0.stringValue] } ?? []) + view.subviews.flatMap { labels($0) }
            }
            let copy = labels(window?.window?.contentView)
            XCTAssertTrue(copy.contains("Vehicle status unavailable"))
            XCTAssertFalse(copy.contains("No vehicles yet"))
            func buttons(_ view: NSView?) -> [NSButton] {
                guard let view else { return [] }
                return ((view as? NSButton).map { [$0] } ?? []) + view.subviews.flatMap { buttons($0) }
            }
            XCTAssertTrue(buttons(window?.window?.contentView).contains { $0.title.contains("Stop Hub") && $0.isEnabled })
            done.fulfill()
        }
        wait(for: [done], timeout: 1)
        withExtendedLifetime(window) {}
    }

    func testDeferredInitialStatusShowsCheckingAndDisablesMutationsUntilSettlement() throws {
        let fixture = try makeFixture(createConfig: true, mode: .standalone)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(environment: fixture.environment))
        let runner = DeferredDevelopmentStatusRunner()
        let controller = HubController(commandRunner: runner, installedCommandRunner: runner,
            serviceRunner: DevelopmentLoadedService(state: .unloaded), serviceInstalledOverride: true,
            developmentConfiguration: configuration)
        let settled = expectation(description: "initial unavailable status settled")
        let window = MainWindowController(controller: controller) { snapshot in
            XCTAssertEqual(snapshot.health, .stopped)
            XCTAssertTrue(snapshot.statusUnavailable)
            XCTAssertFalse(snapshot.checkingStatus)
            settled.fulfill()
        }
        func labels(_ view: NSView?) -> [String] {
            guard let view else { return [] }
            return ((view as? NSTextField).map { [$0.stringValue] } ?? []) + view.subviews.flatMap { labels($0) }
        }
        func buttons(_ view: NSView?) -> [NSButton] {
            guard let view else { return [] }
            return ((view as? NSButton).map { [$0] } ?? []) + view.subviews.flatMap { buttons($0) }
        }
        let root = window.window?.contentView
        let first = labels(root)
        XCTAssertTrue(first.contains("Checking Hub"))
        XCTAssertTrue(first.contains("Checking Hub · status unavailable"))
        XCTAssertTrue(first.contains("Status unavailable"))
        XCTAssertTrue(first.contains("Vehicle status unavailable"))
        XCTAssertFalse(first.contains("Setup required"))
        XCTAssertFalse(first.contains("No vehicles yet"))
        XCTAssertFalse(window.connectButton.isEnabled)
        XCTAssertFalse(window.importButton.isEnabled)
        let mutations = buttons(root).filter {
            !$0.isHiddenOrHasHiddenAncestor && ["Stop Hub…", "Stop Hub", "Restart", "Set Up Hub", "Run Diagnostics", "Connect Tesla"].contains($0.title)
        }
        XCTAssertFalse(mutations.isEmpty)
        XCTAssertTrue(mutations.allSatisfy { !$0.isEnabled })
        window.showEmbeddedDiagnostics()
        XCTAssertTrue(labels(root).contains("Checking Hub"), "diagnostics cannot replace the pending dashboard with a service-changing operation")
        XCTAssertEqual(runner.arguments.count, 1)
        window.showEmbeddedServiceDetails()
        let details = try XCTUnwrap(window.detailsWindow)
        let detailCaptions = serviceDetailCaptions(in: root)
        XCTAssertTrue(detailCaptions.contains("Checking Hub"))
        XCTAssertTrue(detailCaptions.contains("Status unavailable"))
        XCTAssertFalse(detailCaptions.contains("Active"))
        for title in ["Update Service…", "Uninstall Hub…", "Delete Hub and Data…"] {
            let button = try XCTUnwrap(buttons(window.window?.contentView).first { $0.title == title })
            XCTAssertFalse(button.isEnabled)
            button.performClick(nil)
        }
        XCTAssertFalse(details.mutationInProgress)
        XCTAssertEqual(runner.arguments.count, 1, "embedded mutations cannot start while status is pending")
        window.selectMainSection(.dashboard)
        XCTAssertEqual(runner.arguments, [["--config", fixture.config.path, "status"]])
        runner.complete(.failure(HubActionError.commandFailed("status unavailable")))
        wait(for: [settled], timeout: 1)
        XCTAssertFalse(labels(root).contains("Checking Hub"))
        XCTAssertTrue(labels(root).contains("Hub is stopped"))
        XCTAssertTrue(labels(root).contains("Vehicle status unavailable"))
        XCTAssertTrue(buttons(root).contains { $0.title == "Start Hub" && $0.isEnabled && !$0.isHiddenOrHasHiddenAncestor })
        withExtendedLifetime(window) {}
    }

    func testCheckingFirstFrameRetainsKnownAccountProviderAndHistoryCaptions() throws {
        let fixture = try makeFixture(createConfig: true, mode: .standalone)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(environment: fixture.environment))
        let runner = DeferredDevelopmentStatusRunner()
        let known = HubSnapshot.previewRunning
        let controller = HubController(commandRunner: runner, installedCommandRunner: runner,
            serviceRunner: DevelopmentLoadedService(state: .unloaded), serviceInstalledOverride: true,
            initialSnapshot: known, developmentConfiguration: configuration)
        let window = MainWindowController(controller: controller)
        func labels(_ view: NSView?) -> [String] {
            guard let view else { return [] }
            return ((view as? NSTextField).map { [$0.stringValue] } ?? []) + view.subviews.flatMap { labels($0) }
        }
        let first = labels(window.window?.contentView)
        XCTAssertTrue(first.contains("Checking Hub"))
        XCTAssertTrue(first.contains("Last known: \(known.accountDisplay)"))
        XCTAssertTrue(first.contains("Last known: \(known.database)"))
        XCTAssertTrue(first.contains(known.vehicleName))
        XCTAssertFalse(window.connectButton.isEnabled)
        XCTAssertEqual(controller.snapshot.accountState, .connected)
        XCTAssertEqual(controller.snapshot.provider, known.provider)
        XCTAssertEqual(runner.arguments.count, 1)
        window.showEmbeddedServiceDetails()
        let detailCaptions = serviceDetailCaptions(in: window.window?.contentView)
        XCTAssertTrue(detailCaptions.contains("Checking Hub"))
        XCTAssertTrue(detailCaptions.contains("Last known: \(known.accountDisplay)"))
        XCTAssertTrue(detailCaptions.contains("Last known: \(known.database)"))
        XCTAssertFalse(detailCaptions.contains("Active"))
        withExtendedLifetime(window) {}
    }

    func testImmediateFirstRunLaunchPolicyPresentsOnboardingWithoutWaitingForStatus() throws {
        _ = Self.firstRunTestHostAnchor
        let fixture = try makeFixture(createConfig: true)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        let runner = DeferredDevelopmentStatusRunner()
        let controller = HubController(environment: [:], commandRunner: runner, installedCommandRunner: runner,
            serviceRunner: DevelopmentLoadedService(state: .unloaded), homeDirectory: fixture.root,
            serviceInstalledOverride: false)
        XCTAssertTrue(controller.shouldShowOnboardingBeforeInitialRefresh)
        let window = MainWindowController(controller: controller)
        // AppDelegate intentionally keeps this main window hidden and dispatches
        // this entry point immediately for known first-run/handover reasons.
        let onboarding = try XCTUnwrap(window.showFirstRunOnboarding())
        XCTAssertEqual(window.activeModalKind, .onboarding)
        XCTAssertTrue(onboarding.window?.isVisible == true)
        XCTAssertTrue(window.accountWorkflowActive)
        XCTAssertEqual(runner.arguments.count, 1, "the initial status callback remains deferred")
        XCTAssertFalse(window.connectButton.isEnabled)
        onboarding.window?.orderOut(nil)
        window.window?.orderOut(nil)
        withExtendedLifetime(window) {}
    }

    func testReadinessTransportBindsExactConfigAndRejectsRedirectAndOversizedBody() throws {
        let fixture = try makeHTTPSFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root); ReadinessURLProtocol.handler = nil }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(environment: fixture.environment))
        let admitted = try Data(contentsOf: fixture.config)
        let loadedDigest = DevelopmentHubHTTPSReadiness.digest(admitted)
        enum Response { case ready(String?, String?), redirect, oversized, invalidBody }
        let cases: [(Response, Bool)] = [(.ready(loadedDigest, "4102"), true), (.ready(nil, "4102"), false),
            (.ready("malformed", "4102"), false), (.ready(String(repeating: "0", count: 64), "4102"), false),
            (.ready(loadedDigest, "4103"), false), (.ready(loadedDigest, nil), false),
            (.ready(loadedDigest, "04102"), false), (.ready(loadedDigest, "malformed"), false),
            (.redirect, false), (.oversized, false), (.invalidBody, false)]
        for (response, expected) in cases {
            var requests = 0
            ReadinessURLProtocol.handler = { instance in
                requests += 1
                XCTAssertEqual(instance.request.httpMethod, "GET")
                XCTAssertEqual(instance.request.url?.path, "/readyz")
                XCTAssertNil(instance.request.value(forHTTPHeaderField: "Authorization"))
                XCTAssertNil(instance.request.value(forHTTPHeaderField: "Cookie"))
                var headers: [String: String] = ["x-teslatlas-native-config-sha256": loadedDigest, "x-teslatlas-native-process-id": "4102"]
                if case let .ready(value, pid) = response {
                    headers["x-teslatlas-native-config-sha256"] = value
                    headers["x-teslatlas-native-process-id"] = pid
                }
                if case .redirect = response {
                    let redirect = URLRequest(url: URL(string: "https://other.invalid/readyz")!)
                    instance.client?.urlProtocol(instance, wasRedirectedTo: redirect,
                        redirectResponse: HTTPURLResponse(url: instance.request.url!, statusCode: 302,
                            httpVersion: "HTTP/1.1", headerFields: ["Location": redirect.url!.absoluteString])!)
                    return
                }
                instance.client?.urlProtocol(instance, didReceive: HTTPURLResponse(url: instance.request.url!,
                    statusCode: 200, httpVersion: "HTTP/1.1", headerFields: headers)!, cacheStoragePolicy: .notAllowed)
                let body: Data
                switch response {
                case .oversized: body = Data(repeating: 0x20, count: 16_385)
                case .invalidBody: body = Data(#"{"status":"not_ready"}"#.utf8)
                default: body = Data(#"{"status":"ready"}"#.utf8)
                }
                instance.client?.urlProtocol(instance, didLoad: body)
                instance.client?.urlProtocolDidFinishLoading(instance)
            }
            let done = expectation(description: "bounded transport response")
            let settings = URLSessionConfiguration.ephemeral
            settings.protocolClasses = [ReadinessURLProtocol.self]
            DevelopmentHubHTTPSReadiness.probe(configuration: configuration, processID: 4102, timeout: 1,
                sessionConfiguration: settings) { ready in XCTAssertEqual(ready, expected); done.fulfill() }
            wait(for: [done], timeout: 2)
            XCTAssertEqual(requests, 1, "the transport must not follow a redirect")
        }
        // Listener URL, certificate, live PID and ready body are unchanged; the
        // admitted non-TLS configuration differs from the server's loaded bytes.
        try (admitted + Data("\n[http]\nallowed_origins = ['https://example.invalid']\n".utf8)).write(to: fixture.config)
        ReadinessURLProtocol.handler = { instance in
            instance.client?.urlProtocol(instance, didReceive: HTTPURLResponse(url: instance.request.url!,
                statusCode: 200, httpVersion: "HTTP/1.1",
                headerFields: ["x-teslatlas-native-config-sha256": loadedDigest, "x-teslatlas-native-process-id": "4102"])!, cacheStoragePolicy: .notAllowed)
            instance.client?.urlProtocol(instance, didLoad: Data(#"{"status":"ready"}"#.utf8))
            instance.client?.urlProtocolDidFinishLoading(instance)
        }
        let changed = expectation(description: "same listener cannot attest different config bytes")
        let settings = URLSessionConfiguration.ephemeral; settings.protocolClasses = [ReadinessURLProtocol.self]
        DevelopmentHubHTTPSReadiness.probe(configuration: configuration, processID: 4102, timeout: 1,
            sessionConfiguration: settings) { ready in XCTAssertFalse(ready); changed.fulfill() }
        wait(for: [changed], timeout: 2)
    }

    func testReadinessTransportDeadlineCompletesExactlyOnce() throws {
        let fixture = try makeHTTPSFixture()
        defer { try? FileManager.default.removeItem(at: fixture.root); ReadinessURLProtocol.handler = nil }
        let configuration = try XCTUnwrap(DevelopmentHubConfiguration.from(environment: fixture.environment))
        ReadinessURLProtocol.handler = { _ in /* a live transport that never responds */ }
        let settings = URLSessionConfiguration.ephemeral; settings.protocolClasses = [ReadinessURLProtocol.self]
        let done = expectation(description: "HTTPS readiness deadline")
        done.assertForOverFulfill = true
        var completions = 0
        let started = ProcessInfo.processInfo.systemUptime
        DevelopmentHubHTTPSReadiness.probe(configuration: configuration, processID: 4102, timeout: 0.15,
            sessionConfiguration: settings) { ready in
                completions += 1; XCTAssertFalse(ready); done.fulfill()
            }
        wait(for: [done], timeout: 1)
        XCTAssertLessThan(ProcessInfo.processInfo.systemUptime - started, 1)
        let settled = expectation(description: "cancellation callbacks settled")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { settled.fulfill() }
        wait(for: [settled], timeout: 1)
        XCTAssertEqual(completions, 1)
    }

    func testShutdownTerminatesOnlyOwnedStatusReadsAndRejectsNewStatusLaunches() throws {
        let fixture = try makeFixture(createConfig: true)
        defer { try? FileManager.default.removeItem(at: fixture.root) }
        try Data("#!/bin/sh\nexec /bin/sleep 60\n".utf8).write(to: fixture.binary)
        let lifecycle = HubStatusProcessLifecycle()
        let unrelatedServe = Process()
        unrelatedServe.executableURL = fixture.binary
        unrelatedServe.arguments = ["--config", fixture.config.path, "serve"]
        unrelatedServe.standardOutput = FileHandle.nullDevice
        unrelatedServe.standardError = FileHandle.nullDevice
        try unrelatedServe.run()
        defer { if unrelatedServe.isRunning { unrelatedServe.terminate(); unrelatedServe.waitUntilExit() } }
        XCTAssertFalse(HubStatusProcessLifecycle.isStatusRead(unrelatedServe.arguments!))
        let exited = expectation(description: "owned blocking status helper exited")
        HubProcessExecutor.run(executable: fixture.binary, arguments: ["--config", fixture.config.path, "status"],
            timeout: 20, statusLifecycle: lifecycle) { result in
                if case .success = result { XCTFail("shutdown must interrupt the blocked status read") }
                exited.fulfill()
            }
        let started = expectation(description: "owned status process registered")
        let deadline = ProcessInfo.processInfo.systemUptime + 1
        func awaitLaunch() {
            if lifecycle.activeCount == 1 { started.fulfill() }
            else if ProcessInfo.processInfo.systemUptime >= deadline { XCTFail("status helper did not launch"); started.fulfill() }
            else { DispatchQueue.main.asyncAfter(deadline: .now() + 0.01) { awaitLaunch() } }
        }
        awaitLaunch()
        wait(for: [started], timeout: 2)
        lifecycle.shutdown()
        wait(for: [exited], timeout: 2)
        XCTAssertEqual(lifecycle.activeCount, 0)
        XCTAssertTrue(unrelatedServe.isRunning, "the unrelated Serve process is outside status cleanup")
        let rejected = expectation(description: "no status start after shutdown")
        HubProcessExecutor.run(executable: fixture.binary, arguments: ["--config", fixture.config.path, "status"],
            statusLifecycle: lifecycle) { result in
                guard case let .failure(error) = result else { XCTFail("status started after shutdown"); rejected.fulfill(); return }
                XCTAssertTrue(error.localizedDescription.contains("application shutdown"))
                rejected.fulfill()
            }
        wait(for: [rejected], timeout: 1)
        XCTAssertEqual(lifecycle.activeCount, 0)
        XCTAssertTrue(unrelatedServe.isRunning)
    }

    private func serviceDetailCaptions(in view: NSView?) -> [String] {
        guard let view else { return [] }
        if view.identifier?.rawValue == "hub.service.details" {
            func labels(_ view: NSView) -> [String] {
                ((view as? NSTextField).map { [$0.stringValue] } ?? []) + view.subviews.flatMap(labels)
            }
            return labels(view)
        }
        return view.subviews.flatMap { serviceDetailCaptions(in: $0) }
    }

    private func makeHTTPSFixture() throws -> Fixture {
        let fixture = try makeFixture(createConfig: true, mode: .standalone)
        let pem = fixture.root.appendingPathComponent("certificate.pem")
        let process = Process(); process.executableURL = URL(fileURLWithPath: "/usr/bin/openssl")
        process.arguments = ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=localhost",
                             "-keyout", fixture.root.appendingPathComponent("key.pem").path, "-out", pem.path]
        process.standardOutput = FileHandle.nullDevice; process.standardError = FileHandle.nullDevice
        try process.run(); process.waitUntilExit(); XCTAssertEqual(process.terminationStatus, 0)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: pem.path)
        try Data("data_dir = '\(fixture.state.path)'\nbind = '127.0.0.1:21446'\n[tls]\npublic_url = 'https://127.0.0.1:21446'\ncertificate_path = '\(pem.path)'\nprivate_key_path = '\(fixture.root.path)/key.pem'\n".utf8).write(to: fixture.config)
        return fixture
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
            .appendingPathComponent("dev/TeslatlasHubDevelopmentTests-\(UUID().uuidString)",
                                    isDirectory: true)
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
            DevelopmentHubConfiguration.logVariable: logs.path,
            DevelopmentHubConfiguration.controlVariable: root.path
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

private final class ReadinessURLProtocol: URLProtocol {
    static var handler: ((ReadinessURLProtocol) -> Void)?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() { Self.handler?(self) }
    override func stopLoading() {}
}

private final class DevelopmentStatusRunner: HubCommandRunning {
    var result: Result<String, Error>
    private(set) var arguments: [[String]] = []

    init(result: Result<String, Error>) { self.result = result }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        self.arguments.append(arguments)
        completion(result)
    }
}

private final class DeferredDevelopmentStatusRunner: HubCommandRunning {
    private var completion: ((Result<String, Error>) -> Void)?
    private(set) var arguments: [[String]] = []
    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        self.arguments.append(arguments); self.completion = completion
    }
    func complete(_ result: Result<String, Error>) {
        let callback = completion; completion = nil; callback?(result)
    }
}

private final class DevelopmentLoadedService: HubServiceControlling {
    private let state: HubServiceLoadState
    init(state: HubServiceLoadState = .loaded) { self.state = state }
    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        completion(.success(""))
    }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        completion(state)
    }
}
