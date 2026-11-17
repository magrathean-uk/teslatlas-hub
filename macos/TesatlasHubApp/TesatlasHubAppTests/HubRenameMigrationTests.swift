// SPDX-License-Identifier: AGPL-3.0-only

import XCTest
@testable import Tesatlas_Hub

/// The app's side of the rename hand-over (DEC-160) with injected runs, stores
/// and environments: nothing touches the real home, launchd or the Keychain.
final class HubRenameMigrationTests: XCTestCase {
    private let earlierVariable = "TESLATLAS_HUB_REDUCE_MOTION" // rename:keep
    private let earlierDomain = "eu.teslatlas.hub.app" // rename:keep

    func testSpellingsAreDerivedOnlyFromTheCurrentForm() {
        XCTAssertEqual(HubRenameMigration.earlier("eu.tesatlas.hub.app"), earlierDomain)
        XCTAssertNil(HubRenameMigration.earlier(earlierDomain))
        XCTAssertEqual(HubRenameMigration.current(earlierVariable), "TESATLAS_HUB_REDUCE_MOTION")
        XCTAssertNil(HubRenameMigration.current("HOME"))
    }

    func testEarlierEnvironmentVariablesAreAddedOnlyWhereTheCurrentNameIsUnset() {
        let additions = HubRenameMigration.environmentAdditions([
            earlierVariable: "1",
            "TESLATLAS_HUB_INCREASE_CONTRAST": "1", // rename:keep
            "TESATLAS_HUB_INCREASE_CONTRAST": "0",
            "HOME": "/Users/owner",
        ])
        XCTAssertEqual(additions, ["TESATLAS_HUB_REDUCE_MOTION": "1"])
    }

    func testSettingsAreCopiedOnceWithRenamedKeysTheCurrentValuesWinAndTheEarlierDomainStays() {
        var domains: [String: [String: Any]] = [
            earlierDomain: ["TeslatlasHubAppearance": "dark", "NSWindow Frame main": "1 2 3 4"], // rename:keep
            "eu.tesatlas.hub.app": ["NSWindow Frame main": "5 6 7 8"],
        ]
        HubRenameMigration.adoptEarlierPreferences(currentDomain: "eu.tesatlas.hub.app",
                                                   read: { domains[$0] },
                                                   write: { domains[$1] = $0 })
        let current = domains["eu.tesatlas.hub.app"]
        XCTAssertEqual(current?["TesatlasHubAppearance"] as? String, "dark")
        XCTAssertEqual(current?["NSWindow Frame main"] as? String, "5 6 7 8", "same key, different value: current wins")
        XCTAssertEqual(current?[HubRenameMigration.adoptedMarker] as? Bool, true)
        XCTAssertEqual(domains[earlierDomain]?["NSWindow Frame main"] as? String, "1 2 3 4", "the earlier domain is kept")
        // A second launch finds the marker and writes nothing, even after the
        // owner removed an adopted key.
        domains["eu.tesatlas.hub.app"]?["TesatlasHubAppearance"] = nil
        HubRenameMigration.adoptEarlierPreferences(currentDomain: "eu.tesatlas.hub.app",
                                                   read: { domains[$0] },
                                                   write: { _, _ in XCTFail("adopted once only") })
    }

    func testAnInterruptedSettingsCopyIsMadeAgainAndNothingIsLost() {
        var domains: [String: [String: Any]] = [earlierDomain: ["TeslatlasHubAppearance": "dark"]] // rename:keep
        // The write never became durable (the app died first).
        HubRenameMigration.adoptEarlierPreferences(currentDomain: "eu.tesatlas.hub.app",
                                                   read: { domains[$0] },
                                                   write: { _, _ in })
        XCTAssertNotNil(domains[earlierDomain])
        HubRenameMigration.adoptEarlierPreferences(currentDomain: "eu.tesatlas.hub.app",
                                                   read: { domains[$0] },
                                                   write: { domains[$1] = $0 })
        XCTAssertEqual(domains["eu.tesatlas.hub.app"]?["TesatlasHubAppearance"] as? String, "dark")
        XCTAssertNotNil(domains[earlierDomain])
    }

    func testBeforeTheRenameNoSettingsMove() {
        HubRenameMigration.adoptEarlierPreferences(currentDomain: earlierDomain,
                                                   read: { _ in XCTFail("no read"); return nil },
                                                   write: { _, _ in XCTFail("no write") })
    }

    private func recordingStartUp(_ effects: Box) -> HubRenameMigration.StartUp {
        HubRenameMigration.StartUp(environment: { [earlierVariable] in [earlierVariable: "1", "XCTestConfigurationFilePath": "/x"] },
                                   setEnvironment: { name, _ in effects.items.append("setenv \(name)") },
                                   adoptPreferences: { effects.items.append("settings") },
                                   handOver: { effects.items.append("hand-over") })
    }

    private final class Box { var items: [String] = [] }

    /// REV-CX-M08-T12A finding 8: this build (before the swap) changes no
    /// environment variable, copies no settings and runs no helper.
    func testTheCurrentBuildTakesNoStartUpStep() {
        let beforeTheSwap = HubRenameMigration.productName == "Teslatlas Hub" // rename:keep
        XCTAssertEqual(HubRenameMigration.isRenamedBuild, !beforeTheSwap)
        let effects = Box()
        HubRenameMigration.prepareBeforeLaunch(renamed: false, steps: recordingStartUp(effects))
        XCTAssertEqual(effects.items, [])
        if beforeTheSwap {
            HubRenameMigration.prepareBeforeLaunch(steps: recordingStartUp(effects))
            XCTAssertEqual(effects.items, [], "the default follows this build's own name")
        }
        // A renamed build in a test host sets the variables only.
        HubRenameMigration.prepareBeforeLaunch(renamed: true, steps: recordingStartUp(effects))
        XCTAssertEqual(effects.items, ["setenv TESATLAS_HUB_REDUCE_MOTION"])
    }

    func testADeferralShowsTheStepsOwnReason() {
        let outcome = HubRenameMigration.handOver(run: { _ in (75, self.report("deferred", detail: "window claim held")) },
                                                  installAgent: { XCTFail("not after a deferral") },
                                                  prepareLogs: {})
        XCTAssertEqual(outcome, .retry("window claim held"))
    }

    private func report(_ status: String, install: Bool = false, start: Bool = false, detail: String? = nil) -> Data {
        var object: [String: Any] = ["status": status, "install": install, "start": start]
        if let detail { object["detail"] = detail }
        return try! JSONSerialization.data(withJSONObject: object)
    }

    func testAHandOverInstallsTheCurrentAgentThenCompletesWithStart() {
        var runs: [[String]] = []
        var steps: [String] = []
        let outcome = HubRenameMigration.handOver(run: { arguments in
            runs.append(arguments)
            return arguments.count == 1 ? (0, self.report("prepared", install: true, start: true)) : (0, self.report("done"))
        }, installAgent: { steps.append("install") }, prepareLogs: { steps.append("logs") })
        XCTAssertEqual(outcome, .proceed)
        XCTAssertEqual(runs, [["migrate-names"], ["migrate-names", "--complete", "--start"]])
        XCTAssertEqual(steps, ["install", "logs"])
    }

    func testAStoppedHubIsInstalledWithoutStarting() {
        var runs: [[String]] = []
        let outcome = HubRenameMigration.handOver(run: { arguments in
            runs.append(arguments)
            return arguments.count == 1 ? (0, self.report("prepared", install: true)) : (0, self.report("done"))
        }, installAgent: {}, prepareLogs: { XCTFail("no logs without a start") })
        XCTAssertEqual(outcome, .proceed)
        XCTAssertEqual(runs.last, ["migrate-names", "--complete"])
    }

    func testNothingToHandOverProceedsAfterOneRun() {
        var runs = 0
        let outcome = HubRenameMigration.handOver(run: { _ in runs += 1; return (0, self.report("inert")) },
                                                  installAgent: { XCTFail("nothing to install") },
                                                  prepareLogs: {})
        XCTAssertEqual(outcome, .proceed)
        XCTAssertEqual(runs, 1)
    }

    func testADeferredOrFailedStepIsOfferedAgainAndNeverProceeds() {
        let deferred = HubRenameMigration.handOver(run: { _ in (75, self.report("deferred")) },
                                                   installAgent: { XCTFail("not after a deferral") },
                                                   prepareLogs: {})
        guard case .retry = deferred else { return XCTFail("a deferral is retried") }
        let failed = HubRenameMigration.handOver(run: { _ in (1, self.report("failed", detail: "disk full")) },
                                                 installAgent: { XCTFail("not after a failure") },
                                                 prepareLogs: {})
        XCTAssertEqual(failed, .retry("disk full"))
        let unreadable = HubRenameMigration.handOver(run: { _ in (0, Data("not json".utf8)) },
                                                     installAgent: { XCTFail("not without a report") },
                                                     prepareLogs: {})
        guard case .retry = unreadable else { return XCTFail("an unreadable report is retried") }
    }

    func testAFailedInstallOrStartIsRetriedAndTheSecondAttemptFinishes() {
        struct WriteFailed: Error {}
        var attempt = 0
        let first = HubRenameMigration.handOver(run: { _ in (0, self.report("prepared", install: true, start: true)) },
                                                installAgent: { throw WriteFailed() },
                                                prepareLogs: {})
        guard case .retry = first else { return XCTFail("a failed install is retried") }
        let second = HubRenameMigration.handOver(run: { arguments in
            attempt += 1
            return arguments.count == 1
                ? (0, self.report("prepared", install: true, start: true))
                : (1, self.report("failed", detail: "not ready"))
        }, installAgent: {}, prepareLogs: {})
        XCTAssertEqual(second, .retry("not ready"))
        let third = HubRenameMigration.handOver(run: { arguments in
            arguments.count == 1 ? (0, self.report("prepared", install: true, start: true)) : (0, self.report("done"))
        }, installAgent: {}, prepareLogs: {})
        XCTAssertEqual(third, .proceed)
        XCTAssertEqual(attempt, 2)
    }

    func testAReplacementWithARealFaultIsOfferedAgainWithTheStepsOwnReason() {
        let fault = "the Hub service did not start under its current name: "
            + "the Hub service is running but cannot serve its history (catalogue_unavailable)"
        let outcome = HubRenameMigration.handOver(run: { arguments in
            arguments.count == 1
                ? (0, self.report("prepared", install: true, start: true))
                : (1, self.report("failed", detail: fault))
        }, installAgent: {}, prepareLogs: {})
        XCTAssertEqual(outcome, .retry(fault))
    }

    func testAStepStillRunningIsOfferedAgainAsInProgressAndNothingFollows() {
        let preparing = HubRenameMigration.handOver(run: { _ in nil },
                                                    installAgent: { XCTFail("nothing while the move runs") },
                                                    prepareLogs: {})
        XCTAssertEqual(preparing, .retry(HubRenameMigration.inProgressMessage))
        var runs: [[String]] = []
        let completing = HubRenameMigration.handOver(run: { arguments in
            runs.append(arguments)
            return arguments.count == 1 ? (0, self.report("prepared", install: true, start: true)) : nil
        }, installAgent: {}, prepareLogs: {})
        XCTAssertEqual(completing, .retry(HubRenameMigration.inProgressMessage))
        XCTAssertEqual(runs.count, 2)
    }

    func testAStepPastTheLimitIsLeftAloneAndATryAgainStartsNoSecondMove() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let marks = folder.appendingPathComponent("marks").path
        let slow = folder.appendingPathComponent("slow").path
        FileManager.default.createFile(atPath: slow, contents: Data())
        // A stand-in for the bundled binary: slow while `slow` exists.
        let script = """
        echo "started $1" >> '\(marks)'
        if [ -f '\(slow)' ]; then sleep 2; fi
        echo "ended $1" >> '\(marks)'
        echo '{"status":"inert","install":false,"start":false}'
        """
        let runner = HubRenameMigration.StepRunner(executable: URL(fileURLWithPath: "/bin/sh"),
                                                   leadingArguments: ["-c", script, "sh"],
                                                   limit: 0.3)
        let lines = { (try? String(contentsOfFile: marks, encoding: .utf8))?.split(separator: "\n").map(String.init) ?? [] }

        let started = Date()
        XCTAssertNil(runner.run(["migrate-names"]), "past its limit the run is left going")
        XCTAssertLessThan(Date().timeIntervalSince(started), 1.5, "the app stops waiting at the limit")
        XCTAssertNil(runner.run(["migrate-names"]), "a Try Again while it runs")
        XCTAssertEqual(lines(), ["started migrate-names"], "no second move starts")

        // Never terminated: the late step ends by itself.
        let deadline = Date().addingTimeInterval(10)
        while !lines().contains("ended migrate-names"), Date() < deadline { Thread.sleep(forTimeInterval: 0.05) }
        XCTAssertEqual(lines(), ["started migrate-names", "ended migrate-names"])
        Thread.sleep(forTimeInterval: 0.3)

        try FileManager.default.removeItem(atPath: slow)
        let next = try XCTUnwrap(runner.run(["migrate-names"]), "once it has ended, Try Again runs again")
        XCTAssertEqual(next.0, 0)
        XCTAssertEqual(try JSONDecoder().decode(HubRenameMigration.StepReport.self, from: next.1).status, "inert")
    }

    func testOnlyTheInstalledAppRunsTheHandOver() {
        XCTAssertFalse(HubRenameMigration.appliesTo(environment: ["XCTestConfigurationFilePath": "/x"]))
        XCTAssertFalse(HubRenameMigration.appliesTo(environment: [DevelopmentHubConfiguration.enableVariable: "1"]))
    }
}
