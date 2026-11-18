// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Tesatlas_Hub

/// M09-T05: the migration path's stop gate, encryption-key entry, takeover step, its Retry and
/// sign-in fallbacks, the finish copy and the Status line, against the M09-T04 CLI contract
/// (`take-over-source-token`) through a fake runner. No SSH, Docker or Hub binary runs here.
final class SourceTakeoverFlowTests: XCTestCase {
    private static let key = "aaaa-bbbb-aaaa-bbbb"
    private static let databasePassword = "fictional-db-password-91c2"
    private static let source = "postgresql://reader@127.0.0.1:55439/sourcedb"

    // MARK: Stop gate

    func testImportWaitsForTheStopCheckAndRunsOnlyOnceTheSourceIsStopped() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        fixture.stops.results = [.success(.running), .success(.running), .success(.running), .success(.stopped)]
        try fixture.connectAndAcknowledge()

        // The connection's own stop check saw the source running: Import stays unavailable.
        XCTAssertEqual(fixture.window.sourceRunStateForTests, .running)
        fixture.window.enterSourceKeyForTests(Self.key)
        XCTAssertEqual(fixture.window.continueTitleForTests, "Import Data")
        XCTAssertFalse(fixture.window.continueEnabledForTests)
        XCTAssertTrue(fixture.labels().contains {
            $0.contains("still running") && $0.contains("Check Again")
        })

        // Asking anyway re-checks first and still does not import.
        fixture.window.continueForTests()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertEqual(fixture.window.sourceRunStateForTests, .running)
        XCTAssertFalse(fixture.events.all.contains("migrate"))

        // Check Again: still running, still blocked.
        fixture.window.checkSourceAgainForTests()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertFalse(fixture.window.continueEnabledForTests)

        // Once stopped, Import is offered; it checks once more and then imports.
        fixture.stops.results = [.success(.stopped), .success(.stopped)]
        fixture.window.checkSourceAgainForTests()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertEqual(fixture.window.sourceRunStateForTests, .stopped)
        XCTAssertTrue(fixture.window.continueEnabledForTests)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.currentRoute == .verify }

        let order = fixture.events.all
        let migrate = try XCTUnwrap(order.firstIndex(of: "migrate"))
        // Connect, Import (refused), Check Again twice, then the check Import runs itself.
        XCTAssertEqual(order.filter { $0 == "stop-check" }.count, 6, "\(order)")
        XCTAssertEqual(order[migrate + 1], "stop-check", "\(order)")
        XCTAssertEqual(order.filter { $0 == "migrate" }.count, 1)
        // The check ran over the session's own directory and the destination it was opened for.
        let request = try XCTUnwrap(fixture.stops.requests.last)
        XCTAssertEqual(request.destination, "user@server.local")
        XCTAssertEqual(request.port, 22)
        XCTAssertFalse(request.usePasswordlessSudo)
    }

    func testAStopCheckThatCannotAnswerNeverCountsAsStopped() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        fixture.stops.results = [.failure(HubActionError.commandFailed("Hub could not read whether the source application is stopped."))]
        try fixture.connectAndAcknowledge()

        XCTAssertNil(fixture.window.sourceRunStateForTests)
        XCTAssertFalse(fixture.window.hasMigrationSessionForTests)
        XCTAssertEqual(fixture.window.sourceKeyFieldValueForTests, "")
        fixture.window.enterSourceKeyForTests(Self.key)
        XCTAssertEqual(fixture.window.continueTitleForTests, "Connect to Server")
        XCTAssertTrue(fixture.window.continueEnabledForTests)
        XCTAssertTrue(fixture.labels().contains { $0.contains("could not read whether the source application is stopped") })

        fixture.stops.results = [.failure(HubActionError.commandExited(255, "Control socket connect failed"))]
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.hasMigrationSessionForTests && !fixture.window.isBusyForTests }
        fixture.window.acknowledgeSourceReleaseForTests()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertFalse(fixture.window.hasMigrationSessionForTests)
        XCTAssertFalse(fixture.events.all.contains("migrate"))
        XCTAssertFalse(fixture.events.all.contains("takeover"))
    }

    func testStopCheckAnswerParsing() throws {
        XCTAssertEqual(try SourceStopCheck.parse("found=1\nrunning=0\n"), .stopped)
        XCTAssertEqual(try SourceStopCheck.parse("found=2\nrunning=1\n"), .running)
        XCTAssertEqual(try SourceStopCheck.parse("noise line\nrunning=0\nfound=1"), .stopped)
        for refused in ["found=0\nrunning=0\n", "found=1\n", "running=0\n", "",
                        "found=1\nrunning=2\n", "found=1\nfound=1\nrunning=0\n",
                        "found=one\nrunning=0\n", "found=1\nrunning=-1\n", "found=1\nrunning=\n",
                        "found=1\nrunning=0x0\n"] {
            XCTAssertThrowsError(try SourceStopCheck.parse(refused), refused)
        }
    }

    func testStopCheckScriptReadsOnlyStateAndNamesThroughDocker() throws {
        let stopped = try StopCheckScriptHarness.run(running: "false")
        XCTAssertEqual(stopped.status, 0, stopped.output)
        XCTAssertEqual(try SourceStopCheck.parse(stopped.output), .stopped)

        let running = try StopCheckScriptHarness.run(running: "true")
        XCTAssertEqual(running.status, 0, running.output)
        XCTAssertEqual(try SourceStopCheck.parse(running.output), .running)

        // Anything but an explicit "false" is running.
        let unknown = try StopCheckScriptHarness.run(running: "")
        XCTAssertEqual(try SourceStopCheck.parse(unknown.output), .running)

        let none = try StopCheckScriptHarness.run(running: "false", withSource: false)
        XCTAssertThrowsError(try SourceStopCheck.parse(none.output))

        for call in stopped.calls + running.calls {
            XCTAssertFalse(call.contains("{{range .Config.Env}}{{println .}}{{end}}"), "no environment value is read")
            XCTAssertFalse(call.contains(SourceDiscovery.allowListedValuesTemplate), "no environment value is read")
            XCTAssertFalse(call.contains("exec"), "nothing runs inside a container")
        }
        XCTAssertFalse(stopped.output.contains("fictional-env-secret"))
        XCTAssertFalse(SourceStopCheck.script.contains("println .}}"))
    }

    func testStopCheckRunsOverTheSessionControlSocketWithoutPrompting() throws {
        let socketFolder = HubTestScratch.fixtureRoot("stop-check-socket")
        try FileManager.default.createDirectory(at: socketFolder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: socketFolder) }
        XCTAssertNil(SourceStopCheck.controlSocket(inSessionDirectory: socketFolder))
        // A regular file with the socket's name is not a socket.
        try Data().write(to: socketFolder.appendingPathComponent("c0"))
        XCTAssertNil(SourceStopCheck.controlSocket(inSessionDirectory: socketFolder))

        let arguments = SourceStopCheck.arguments(controlSocket: URL(fileURLWithPath: "/tmp/th-x/c1"),
                                                  destination: "user@server.local", port: 2222,
                                                  usePasswordlessSudo: true)
        func option(_ name: String) -> Bool { arguments.contains(name) }
        XCTAssertTrue(option("ControlMaster=no"))
        XCTAssertTrue(option("ControlPath=/tmp/th-x/c1"))
        XCTAssertTrue(option("BatchMode=yes"))
        XCTAssertTrue(option("StrictHostKeyChecking=yes"))
        XCTAssertTrue(option("ForwardAgent=no"))
        XCTAssertEqual(Array(arguments.suffix(4)), ["env", "TESATLAS_USE_SUDO=1", "sh", "-s"])
        XCTAssertFalse(arguments.contains("-i"))
        XCTAssertFalse(arguments.contains { $0.contains("ControlPath=none") })
    }

    // MARK: Key entry and the takeover command

    func testKeyIsASecureFieldPassedOnStandardInputAndNeverKept() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        try fixture.connectAndAcknowledge()

        let field = try XCTUnwrap(fixture.view(identifier: "onboarding.source-encryption-key"))
        XCTAssertTrue(field is NSSecureTextField)
        // Without a key nothing can start.
        XCTAssertFalse(fixture.window.continueEnabledForTests)
        fixture.window.enterSourceKeyForTests(Self.key)
        XCTAssertTrue(fixture.window.continueEnabledForTests)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.currentRoute == .verify }

        let takeovers = fixture.runner.calls.filter { $0.arguments.contains("take-over-source-token") }
        XCTAssertEqual(takeovers.count, 1)
        let call = try XCTUnwrap(takeovers.first)
        XCTAssertEqual(Array(call.arguments.suffix(4)),
                       ["take-over-source-token", "--source", Self.source, "--accept-source-stopped"])
        XCTAssertEqual(call.arguments.first, "--config")
        let input = try XCTUnwrap(call.stdin)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(input.utf8)) as? [String: String])
        XCTAssertEqual(object, ["postgresPassword": Self.databasePassword, "encryptionKey": Self.key])
        XCTAssertFalse(input.lowercased().contains("token"))

        // The key travelled once, on standard input only.
        for other in fixture.runner.calls where other.stdin == nil || !other.arguments.contains("take-over-source-token") {
            XCTAssertFalse(other.arguments.joined(separator: " ").contains(Self.key))
            XCTAssertFalse(other.stdin?.contains(Self.key) ?? false)
        }
        XCTAssertFalse(call.arguments.joined(separator: " ").contains(Self.key))
        XCTAssertEqual(fixture.window.sourceKeyFieldValueForTests, "")
        XCTAssertFalse(HubAppLog.shared.recentText().contains(Self.key))
        XCTAssertFalse(String(describing: UserDefaults.standard.dictionaryRepresentation()).contains(Self.key))
        XCTAssertFalse(try fixture.anyFileUnderHomeContains(Self.key))
        XCTAssertEqual(fixture.controller.registeredMigrationSessionCount, 0)
        XCTAssertTrue(fixture.controller.sourceTakeoverStatus.isTakenOver)
    }

    func testTakeoverRefusesAnEmptyKeyBeforeAnyCommand() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        let session = try Fixture.makeSession()
        defer { session.close() }
        let done = expectation(description: "refused")
        fixture.controller.takeOverSourceToken(session: session, encryptionKey: "") { result in
            guard case let .failure(error) = result else { return XCTFail("empty key accepted") }
            XCTAssertEqual(error.localizedDescription, "Enter the source application's encryption key.")
            done.fulfill()
        }
        wait(for: [done], timeout: 2)
        XCTAssertTrue(fixture.runner.calls.isEmpty)
    }

    func testTakeoverInputCarriesExactlyTheTwoContractKeys() throws {
        let input = try HubController.sourceTakeoverInput(postgresPassword: "p\"w", encryptionKey: "k\\y")
        defer { input.wipe() }
        let object = try XCTUnwrap(input.withBytes { try JSONSerialization.jsonObject(with: Data($0)) } as? [String: String])
        XCTAssertEqual(Set(object.keys), ["postgresPassword", "encryptionKey"])
        XCTAssertEqual(object["postgresPassword"], "p\"w")
        XCTAssertEqual(object["encryptionKey"], "k\\y")
    }

    func testOnlyTheCompleteContractReportIsSuccess() {
        XCTAssertTrue(HubSourceTakeoverReport.isComplete(TakeoverRunner.report))
        XCTAssertTrue(HubSourceTakeoverReport.isComplete("progress\n" + TakeoverRunner.report + "\n"))
        for broken in [
            "",
            "{}",
            #"{"status":"taken_over","origin":"source_token","firstRefresh":"next_window","sourceSessionsSeen":1,"sourceNeverMutated":true}"#,
            #"{"status":"taken_over","origin":"source_token","firstRefresh":"next_window","sourceSessionsSeen":0,"sourceNeverMutated":false}"#,
            #"{"status":"taken_over","origin":"hub_login","firstRefresh":"next_window","sourceSessionsSeen":0,"sourceNeverMutated":true}"#,
            #"{"status":"imported","origin":"source_token","firstRefresh":"next_window","sourceSessionsSeen":0,"sourceNeverMutated":true}"#,
            #"{"status":"taken_over","origin":"source_token","firstRefresh":"now","sourceSessionsSeen":0,"sourceNeverMutated":true}"#,
            #"{"status":"taken_over","origin":"source_token","sourceSessionsSeen":0,"sourceNeverMutated":true}"#
        ] {
            XCTAssertFalse(HubSourceTakeoverReport.isComplete(broken), broken)
        }
    }

    func testEveryContractRefusalReachesTheOwnerByItsCode() {
        for code in HubSourceTakeoverRefusal.knownCodes {
            let refusal = HubSourceTakeoverRefusal.from(HubActionError.commandExited(1, #"{"error":"\#(code)"}"#))
            XCTAssertEqual(refusal.code, code)
            XCTAssertFalse(refusal.message.isEmpty)
        }
        XCTAssertEqual(HubSourceTakeoverRefusal.from(HubActionError.commandTimedOut).code, "takeover_failed")
        XCTAssertEqual(HubSourceTakeoverRefusal.from(HubActionError.commandExited(1, "setup_input_invalid")).code,
                       "takeover_failed")
        XCTAssertEqual(HubSourceTakeoverRefusal.message(code: "slot_occupied"),
                       "Hub already has its own login; sign out first if you want to take over the source application's.")
    }

    // MARK: Failure, Retry and Sign in with Tesla instead

    func testFailedTakeoverOffersRetryAndNeverUndoesTheImport() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        fixture.runner.takeoverResults = [
            .failure(HubActionError.commandExited(1, #"{"error":"source_token_key_mismatch"}"#)),
            .success("not a report"),
            .success(TakeoverRunner.report)
        ]
        try fixture.connectAndAcknowledge()
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.takeoverFailureCodeForTests != nil }

        XCTAssertTrue(fixture.window.awaitingTakeoverForTests)
        XCTAssertEqual(fixture.window.takeoverFailureCodeForTests, "source_token_key_mismatch")
        XCTAssertEqual(fixture.window.currentRoute, .migration)
        XCTAssertEqual(fixture.window.continueTitleForTests, "Retry")
        XCTAssertTrue(fixture.window.backHiddenForTests)
        let text = fixture.labels()
        XCTAssertTrue(text.contains("History imported; Tesla login not taken over"))
        XCTAssertTrue(text.contains { $0.contains("does not open the source application's stored login") })
        XCTAssertTrue(fixture.buttonTitles().contains("Sign in with Tesla Instead"))
        // The key was not kept after the call: Retry needs it again.
        XCTAssertEqual(fixture.window.sourceKeyFieldValueForTests, "")
        XCTAssertFalse(fixture.window.continueEnabledForTests)

        // The import stands: its handover gate is in place and nothing was rolled back.
        XCTAssertEqual(fixture.controller.pendingMigrationHandoverPhase, .awaitingTakeover)
        XCTAssertEqual(fixture.controller.sourceTakeoverStatus, .absent)
        XCTAssertTrue(fixture.window.hasMigrationSessionForTests)
        XCTAssertEqual(fixture.service.calls, [])

        // A report that is not the contract's is a failure too, and writes nothing.
        fixture.window.enterSourceKeyForTests(Self.key)
        XCTAssertTrue(fixture.window.continueEnabledForTests)
        fixture.window.continueForTests()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertEqual(fixture.window.takeoverFailureCodeForTests, "takeover_failed")
        XCTAssertEqual(fixture.controller.sourceTakeoverStatus, .absent)

        // Retry runs the takeover alone: no second import.
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.currentRoute == .verify }
        XCTAssertEqual(fixture.events.all.filter { $0 == "migrate" }.count, 1)
        XCTAssertEqual(fixture.events.all.filter { $0 == "takeover" }.count, 3)
        XCTAssertTrue(fixture.controller.sourceTakeoverStatus.isTakenOver)
        XCTAssertEqual(fixture.controller.registeredMigrationSessionCount, 0)
    }

    func testSignInInsteadKeepsTheImportAndFollowsTheRealTransition() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        fixture.runner.takeoverResults = [.failure(HubActionError.commandExited(1, "source_token_key_mismatch"))]
        try fixture.connectAndAcknowledge()
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.takeoverFailureCodeForTests != nil }
        fixture.window.declineTakeoverForTests()
        try fixture.waitUntil { fixture.window.currentRoute == .legacy }
        XCTAssertTrue(fixture.window.takeoverDeclinedForTests)
        XCTAssertFalse(fixture.window.hasMigrationSessionForTests)
        XCTAssertEqual(fixture.controller.pendingMigrationHandoverPhase, .awaitingSignIn)
        XCTAssertEqual(fixture.events.all.filter { $0 == "migrate" }.count, 1)
        // Use the very same owner sign-in context the ordinary Connect Tesla button uses.
        // Stop at a closed window so no authentication UI or Tesla traffic can occur.
        let signIn = try fixture.controller.beginLegacySignIn().get()
        let checked = expectation(description: "sign-in window checked")
        signIn.requireOpenWindow { result in
            if case .success = result { XCTFail("a closed window must refuse sign-in") }
            checked.fulfill()
        }
        wait(for: [checked], timeout: 3)
        signIn.cancel()
        XCTAssertTrue(fixture.service.calls.contains("service start"))
        XCTAssertTrue(fixture.runner.calls.contains { $0.arguments.contains("window") })
        XCTAssertTrue(fixture.runner.calls.contains { $0.arguments.contains("preflight") })
        XCTAssertTrue(fixture.events.all.contains("healthz"))
        let config = try String(contentsOf: fixture.folder.appendingPathComponent("config.toml"), encoding: .utf8)
        XCTAssertTrue(config.contains("enabled = true"))
        XCTAssertTrue(config.contains("interval_seconds = 60"))
        XCTAssertEqual(fixture.events.all.filter { $0 == "migrate" }.count, 1)
        try fixture.restartWindow()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertEqual(fixture.window.currentRoute, .legacy)
    }

    func testLostReplyReconcilesCommittedLoginAndCaptionFailureCannotHideWarning() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        fixture.runner.commitEvenOnFailure = true
        fixture.runner.takeoverResults = [.failure(HubActionError.commandExited(1, "lost reply"))]
        let caption = fixture.folder.appendingPathComponent(".source-takeover-first-refresh")
        try FileManager.default.createDirectory(at: caption, withIntermediateDirectories: true)
        try fixture.connectAndAcknowledge()
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.currentRoute == .verify }
        XCTAssertTrue(fixture.controller.sourceTakeoverStatus.isTakenOver)
        XCTAssertEqual(fixture.controller.pendingMigrationHandoverPhase, .awaitingVerification)
        try fixture.controller.advanceTakeoverPhase(to: .awaitingHandover)
        try fixture.restartWindow()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertEqual(fixture.window.currentRoute, .finish)
        XCTAssertTrue(fixture.labels().contains { $0.contains("Running both on one login can lock you out") })
        XCTAssertEqual(fixture.acknowledgements(), ["I will keep the source application stopped"])
        XCTAssertFalse(fixture.window.continueEnabledForTests)
        let checkbox = try XCTUnwrap(PageReader(fixture.window).checkbox(label: "I will keep the source application stopped"))
        checkbox.performClick(nil)
        XCTAssertTrue(fixture.window.continueEnabledForTests)
    }

    func testEmptyAndInvalidRepliesUseDurableOwnershipRatherThanTheReport() throws {
        for reply in ["", "not a report", TakeoverRunner.report.replacingOccurrences(of: "taken_over", with: "already_taken_over")] {
            let fixture = try Fixture()
            defer { fixture.tearDown() }
            fixture.runner.commitEvenOnFailure = true
            fixture.runner.takeoverResults = [.success(reply)]
            try fixture.connectAndAcknowledge()
            fixture.window.enterSourceKeyForTests(Self.key)
            fixture.window.continueForTests()
            try fixture.waitUntil { fixture.window.currentRoute == .verify }
            XCTAssertTrue(fixture.controller.sourceTakeoverStatus.isTakenOver)
        }
        XCTAssertTrue(HubSourceTakeoverReport.isComplete(TakeoverRunner.report.replacingOccurrences(of: "taken_over", with: "already_taken_over")))
    }

    func testUnknownStatusNeverClaimsAbsenceOrAllowsDeclining() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        fixture.runner.takeoverResults = [.failure(HubActionError.commandExited(1, "lost reply"))]
        fixture.runner.statusOutputs = ["invalid status", "invalid status"]
        try fixture.connectAndAcknowledge()
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.takeoverFailureCodeForTests != nil }
        XCTAssertEqual(fixture.controller.sourceTakeoverStatus, .unknown)
        XCTAssertFalse(fixture.labels().contains { $0.contains("login not taken over") })
        fixture.window.declineTakeoverForTests()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertEqual(fixture.window.currentRoute, .migration)
        XCTAssertEqual(fixture.controller.pendingMigrationHandoverPhase, .awaitingTakeover)
    }

    func testRestartResumesOnlyTakeoverAndReconnectsAfterDeadSession() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        fixture.runner.takeoverResults = [.failure(HubActionError.commandExited(1, "source_token_key_mismatch"))]
        try fixture.connectAndAcknowledge()
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.takeoverFailureCodeForTests != nil }
        let history = fixture.folder.appendingPathComponent("imported-history-fixture")
        try Data("unchanged imported history".utf8).write(to: history)
        let before = try Data(contentsOf: history)
        try fixture.restartWindow()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertEqual(fixture.window.currentRoute, .migration)
        XCTAssertTrue(fixture.window.awaitingTakeoverForTests)
        XCTAssertEqual(fixture.window.continueTitleForTests, "Reconnect to Server")
        XCTAssertFalse(fixture.window.continueEnabledForTests)
        XCTAssertTrue(fixture.labels().contains { $0.contains("History is already imported. Reconnect") })
        try fixture.fillConnectionForm()
        try fixture.clickContinue()
        try fixture.waitUntil { fixture.window.hasMigrationSessionForTests && !fixture.window.isBusyForTests }
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.stops.results = [.failure(HubActionError.commandExited(255, "dead session"))]
        fixture.window.continueForTests()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertFalse(fixture.window.hasMigrationSessionForTests)
        XCTAssertEqual(fixture.window.sourceKeyFieldValueForTests, "")
        XCTAssertTrue(fixture.labels().contains { $0.contains("dead session") })
        try fixture.fillConnectionForm()
        try fixture.clickContinue()
        try fixture.waitUntil { fixture.window.hasMigrationSessionForTests && !fixture.window.isBusyForTests }
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.currentRoute == .verify }
        XCTAssertEqual(fixture.events.all.filter { $0 == "migrate" }.count, 1)
        XCTAssertEqual(fixture.events.all.filter { $0 == "takeover" }.count, 2)
        XCTAssertEqual(try Data(contentsOf: history), before)
        XCTAssertGreaterThanOrEqual(fixture.connections, 3)
        let lastTakeover = try XCTUnwrap(fixture.events.all.lastIndex(of: "takeover"))
        XCTAssertEqual(fixture.events.all[lastTakeover - 1], "stop-check")
    }

    func testRestartAfterCommitBeforeReplyResumesWithoutImportOrSecret() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        fixture.runner.takeoverResults = [.failure(HubActionError.commandExited(1, "before commit"))]
        try fixture.connectAndAcknowledge()
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.takeoverFailureCodeForTests != nil }
        fixture.runner.durableOrigin = "source_token"  // store committed while app was gone
        try fixture.restartWindow()
        try fixture.waitUntil { fixture.window.currentRoute == .verify }
        XCTAssertEqual(fixture.events.all.filter { $0 == "migrate" }.count, 1)
        XCTAssertEqual(fixture.events.all.filter { $0 == "takeover" }.count, 1)
        XCTAssertEqual(fixture.window.sourceKeyFieldValueForTests, "")
        XCTAssertTrue(fixture.controller.sourceTakeoverStatus.isTakenOver)
    }

    func testKeyIsClearedOnModeSwitchSessionCloseBackAndWindowClose() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        try fixture.connectAndAcknowledge()
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.configureLocalHistoryForTests(source: Self.source, carID: "1", passwordFile: "/tmp/fake")
        XCTAssertEqual(fixture.window.sourceKeyFieldValueForTests, "")
        fixture.window.showSourceConnectionFormForTests()
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.abandonSourceForTests()
        XCTAssertEqual(fixture.window.sourceKeyFieldValueForTests, "")
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.navigate(to: .choose)
        XCTAssertEqual(fixture.window.sourceKeyFieldValueForTests, "")
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.close()
        XCTAssertEqual(fixture.window.sourceKeyFieldValueForTests, "")
    }

    func testRefreshProgressDrivesStatusAndFinishAdviceWithoutClockComparison() throws {
        let fixture = try Fixture(migrationWindow: false)
        defer { fixture.tearDown() }
        for progress in ["none_sent", "attempt_recorded", "sign_in_required", "completed"] {
            fixture.runner.statusOutputs = [Fixture.status(origin: "source_token", progress: progress)]
            let account = try fixture.refreshedAccount()
            XCTAssertEqual(fixture.controller.sourceTakeoverStatus, .takenOver(progress: progress))
            XCTAssertEqual(account, fixture.controller.sourceTakeoverStatus.accountText)
            try fixture.writeMarker(.awaitingHandover)
            fixture.runner.durableOrigin = "source_token"
            fixture.runner.progress = progress
            try fixture.restartWindow()
            try fixture.waitUntil { !fixture.window.isBusyForTests }
            let text = fixture.labels().joined(separator: "\n")
            XCTAssertEqual(fixture.acknowledgements(), ["I will keep the source application stopped"])
            switch progress {
            case "none_sent":
                XCTAssertTrue(text.contains("source login is untouched"))
                XCTAssertTrue(text.contains("first refresh at the next window"))
            case "completed":
                XCTAssertTrue(text.contains("copy no longer works"))
                XCTAssertFalse(text.contains("untouched"))
            case "attempt_recorded":
                XCTAssertTrue(account.contains("Hub is refreshing the login now"))
                XCTAssertTrue(text.contains("Keep the source application stopped until the outcome is known"))
                XCTAssertFalse(text.contains("Sign in again in Hub"))
                XCTAssertFalse(text.contains("also needs a new sign-in"))
                XCTAssertFalse(text.contains("first refresh at the next window"))
            default:
                XCTAssertTrue(text.contains("A refresh may have been sent"))
                XCTAssertTrue(text.contains("also needs a new sign-in"))
                XCTAssertFalse(text.contains("first refresh at the next window"))
                XCTAssertFalse(text.contains("untouched"))
            }
        }
    }

    func testDurableAbsenceAndOtherOriginDoNotAcquireTakeoverWarningsFromCaption() throws {
        let fixture = try Fixture(migrationWindow: false)
        defer { fixture.tearDown() }
        try Data(#"{"takenOverAt":1800000000}"#.utf8).write(to: fixture.folder.appendingPathComponent(".source-takeover-first-refresh"))
        for origin: String? in [nil, "hub_login"] {
            fixture.runner.durableOrigin = origin
            try fixture.writeMarker(.awaitingHandover)
            try fixture.restartWindow()
            try fixture.waitUntil { !fixture.window.isBusyForTests }
            XCTAssertFalse(fixture.controller.sourceTakeoverStatus.isTakenOver)
            XCTAssertEqual(fixture.acknowledgements(), ["I have disabled Tesla access in the source application to avoid duplicate requests"])
        }
    }

    func testSensitiveTransportWipesBeforeWaitingForChildAndOnSpawnError() throws {
        for executable in ["/bin/cat", "/missing/test-only-executable"] {
            var data = Data("synthetic secret".utf8)
            let input = HubSensitiveInput(&data)
            XCTAssertTrue(data.isEmpty)
            let done = expectation(description: "secret transport completed")
            var primitives = HubProcessPrimitives.live
            primitives.wait = { semaphore, deadline in
                XCTAssertTrue(input.isWiped, "bytes must be wiped before waiting for process exit")
                return HubProcessPrimitives.live.wait(semaphore, deadline)
            }
            HubProcessExecutor.run(executable: URL(fileURLWithPath: executable), arguments: [],
                sensitiveInput: input, primitives: primitives) { result in
                    XCTAssertTrue(input.isWiped)
                    XCTAssertTrue(input.withBytes { $0.allSatisfy { $0 == 0 } })
                    switch result {
                    case .success:
                        XCTAssertEqual(executable, "/bin/cat")
                    case let .failure(error):
                        XCTAssertNotEqual(executable, "/bin/cat", error.localizedDescription)
                    }
                    done.fulfill()
                }
            wait(for: [done], timeout: 5)
        }
    }

    func testInitialDiscoveryFindsStoppedSourceAndRefusesNoneOrAmbiguity() throws {
        let stopped = try StopCheckScriptHarness.run(running: "false", discovery: true)
        XCTAssertEqual(stopped.status, 0, stopped.output)
        XCTAssertEqual(try SourceDiscovery.parse(stopped.output)["car"], "42")
        XCTAssertTrue(stopped.calls.contains { $0.contains("ps") && $0.contains("-a") })
        XCTAssertEqual(try StopCheckScriptHarness.run(running: "false", withSource: false, discovery: true).status, 20)
        XCTAssertEqual(try StopCheckScriptHarness.run(running: "false", discovery: true, duplicateSource: true).status, 27)
    }

    func testPasswordReconnectUsesVisibleFormAndClearsOnFailureSuccessAndClose() throws {
        let fixture = try Fixture(migrationWindow: false)
        defer { fixture.tearDown() }
        try fixture.writeMarker(.awaitingTakeover)
        try fixture.restartWindow()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertTrue(fixture.labels().contains { $0.contains("History is already imported. Reconnect") })
        fixture.connectionFailures = [HubActionError.commandExited(255, "Permission denied")]
        try fixture.fillConnectionForm(password: true)
        let passwordField = try XCTUnwrap(fixture.view(identifier: "onboarding.source-password") as? NSSecureTextField)
        try fixture.clickContinue()
        XCTAssertEqual(passwordField.stringValue, "")
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertTrue(fixture.labels().contains("SSH authentication failed"))
        XCTAssertTrue(fixture.labels().contains("The server did not accept the selected account or authentication method."))
        XCTAssertNotNil(fixture.view(identifier: "onboarding.source-password"))
        XCTAssertFalse(fixture.window.continueEnabledForTests)
        let attempts = fixture.connections
        fixture.window.continueForTests() // Even a submitted empty form must never reach the connector.
        XCTAssertEqual(fixture.connections, attempts)
        XCTAssertTrue(fixture.labels().contains { $0.contains("Enter the SSH password again") })
        try fixture.fillConnectionForm(password: true)
        try fixture.clickContinue()
        try fixture.waitUntil { fixture.window.hasMigrationSessionForTests && !fixture.window.isBusyForTests }
        XCTAssertEqual(passwordField.stringValue, "")
        let reconnect = try XCTUnwrap(fixture.view(identifier: "onboarding.takeover-reconnect") as? NSButton)
        reconnect.performClick(nil)
        XCTAssertFalse(fixture.window.hasMigrationSessionForTests)
        try fixture.fillConnectionForm(password: true)
        let retainedField = try XCTUnwrap(fixture.view(identifier: "onboarding.source-password") as? NSSecureTextField)
        fixture.window.close()
        XCTAssertEqual(retainedField.stringValue, "")
        XCTAssertEqual(fixture.passwordChecks, [true, true])
        XCTAssertEqual(fixture.events.all.filter { $0 == "stop-check" }.count, 1)
        XCTAssertFalse(fixture.events.all.contains("migrate"))
        XCTAssertFalse(try fixture.anyFileUnderHomeContains(Fixture.sshPassword))
        XCTAssertFalse(try fixture.anyFileUnderHomeContains(Self.key))
    }

    func testExistingOwnerLoginResumesVerificationFromEitherInterruptedPhase() throws {
        for phase: HubMigrationHandoverPhase in [.awaitingTakeover, .awaitingSignIn] {
            let fixture = try Fixture(migrationWindow: false)
            defer { fixture.tearDown() }
            try fixture.writeMarker(phase)
            fixture.runner.durableOrigin = "hub_login"
            try fixture.restartWindow()
            try fixture.waitUntil { fixture.window.currentRoute == .verify }
            XCTAssertEqual(fixture.controller.sourceTakeoverStatus, .otherLogin)
            XCTAssertFalse(fixture.window.awaitingTakeoverForTests)
            XCTAssertEqual(fixture.connections, 0)
            XCTAssertFalse(fixture.events.all.contains("migrate"))
            XCTAssertFalse(fixture.events.all.contains("takeover"))
            XCTAssertFalse(fixture.runner.calls.contains { $0.arguments.contains("preflight") })
            XCTAssertFalse(fixture.service.calls.contains("service start"))
        }
    }

    func testOwnerLoginAppearingBeforeDeclineMovesToVerification() throws {
        let fixture = try Fixture(migrationWindow: false)
        defer { fixture.tearDown() }
        try fixture.writeMarker(.awaitingTakeover)
        try fixture.restartWindow()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        fixture.runner.durableOrigin = "hub_login"
        let decline = try XCTUnwrap(fixture.view(identifier: "onboarding.takeover-sign-in") as? NSButton)
        decline.performClick(nil)
        try fixture.waitUntil { fixture.window.currentRoute == .verify }
        XCTAssertEqual(fixture.controller.sourceTakeoverStatus, .otherLogin)
        XCTAssertFalse(fixture.events.all.contains("takeover"))
    }

    func testStatusSampledBeforeTakeoverCannotOverwriteCommittedOwnership() throws {
        let fixture = try Fixture(migrationWindow: false)
        defer { fixture.tearDown() }
        try fixture.writeMarker(.awaitingTakeover)
        let session = try Fixture.makeSession()
        defer { session.close() }
        fixture.runner.holdNextStatus = true
        let oldRefresh = expectation(description: "old refresh settled")
        fixture.controller.refresh { _ in oldRefresh.fulfill() }
        try fixture.waitUntil { fixture.runner.heldStatus != nil }
        let takenOver = expectation(description: "take-over reconciled")
        fixture.controller.takeOverSourceToken(session: session, encryptionKey: Self.key) { result in
            if case let .failure(error) = result { XCTFail(error.localizedDescription) }
            takenOver.fulfill()
        }
        wait(for: [takenOver], timeout: 3)
        XCTAssertEqual(fixture.controller.sourceTakeoverStatus, .takenOver(progress: "none_sent"))
        XCTAssertEqual(fixture.controller.pendingMigrationHandoverPhase, .awaitingVerification)
        // No new dashboard refresh was started. Its own latest-request guard therefore
        // admits this old reply, exercising the shared take-over cache's ordering.
        let oldReply = try XCTUnwrap(fixture.runner.heldStatus)
        fixture.runner.heldStatus = nil
        oldReply()
        wait(for: [oldRefresh], timeout: 3)
        XCTAssertEqual(fixture.controller.sourceTakeoverStatus, .takenOver(progress: "none_sent"))
        // A genuinely later absence is still accepted, for example after sign-out.
        fixture.runner.durableOrigin = nil
        _ = try fixture.refreshedAccount()
        XCTAssertEqual(fixture.controller.sourceTakeoverStatus, .absent)
    }

    func testOccupiedOwnerSlotDuringTakeoverContinuesToVerification() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        fixture.runner.takeoverResults = [.failure(HubActionError.commandExited(1, "slot_occupied"))]
        try fixture.connectAndAcknowledge()
        fixture.runner.durableOrigin = "hub_login"
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.currentRoute == .verify }
        XCTAssertEqual(fixture.controller.sourceTakeoverStatus, .otherLogin)
        XCTAssertEqual(fixture.events.all.filter { $0 == "migrate" }.count, 1)
        XCTAssertEqual(fixture.events.all.filter { $0 == "takeover" }.count, 1)
        XCTAssertEqual(fixture.window.sourceKeyFieldValueForTests, "")
    }

    func testRepeatedImportedSignInRoutePreservesItsConfiguration() throws {
        let fixture = try Fixture(migrationWindow: false)
        defer { fixture.tearDown() }
        try fixture.writeMarker(.awaitingSignIn)
        let config = fixture.folder.appendingPathComponent("config.toml")
        try Data("bind = \"127.0.0.1:8080\"\n# owner setting\n[collector]\nprovider = \"legacy\"\ninterval_seconds = 0\n[collector.legacy_auth]\nenabled = false\n[terrain]\nenabled = false\n".utf8).write(to: config)
        var first: Data?
        for _ in 0..<2 {
            let signIn = try fixture.controller.beginLegacySignIn().get()
            let done = expectation(description: "closed window refuses sign-in")
            signIn.requireOpenWindow { result in
                if case .success = result { XCTFail("closed window must refuse") }
                done.fulfill()
            }
            wait(for: [done], timeout: 3)
            signIn.cancel()
            let data = try Data(contentsOf: config)
            if let first { XCTAssertEqual(data, first) } else { first = data }
        }
        XCTAssertEqual(fixture.service.calls.filter { $0 == "service start" }.count, 2)
        XCTAssertEqual(fixture.runner.calls.filter { $0.arguments.contains("preflight") }.count, 2)
        XCTAssertEqual(fixture.events.all.filter { $0 == "healthz" }.count, 2)
        XCTAssertTrue(String(decoding: first!, as: UTF8.self).contains("# owner setting"))
        XCTAssertTrue(String(decoding: first!, as: UTF8.self).contains("interval_seconds = 60"))
    }

    func testSignInHealthUsesTLSConfigurationAndHealthzOnPrivateAndLoopbackListeners() throws {
        let fixture = try Fixture(migrationWindow: false, validatedPaths: true)
        defer { fixture.tearDown(); SignInHealthURLProtocol.handler = nil }
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: fixture.home.path)
        let certificate = fixture.home.appendingPathComponent("certificate.pem")
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/openssl")
        process.arguments = ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=localhost",
                             "-keyout", "/dev/null", "-out", certificate.path]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        let generated = expectation(description: "synthetic certificate generated")
        process.terminationHandler = { _ in generated.fulfill() }
        try process.run()
        wait(for: [generated], timeout: 10)
        XCTAssertEqual(process.terminationStatus, 0)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: certificate.path)
        let config = fixture.folder.appendingPathComponent("config.toml")
        let settings = URLSessionConfiguration.ephemeral
        settings.protocolClasses = [SignInHealthURLProtocol.self]
        fixture.healthSettings = settings
        try fixture.writeMarker(.awaitingSignIn)
        for host in ["127.0.0.1", "192.168.20.5"] {
            let content = "bind = '\(host):21446'\n[tls]\npublic_url = 'https://\(host):21446'\ncertificate_path = '\(certificate.path)'\nprivate_key_path = '/deliberately-unread-private-key'\n"
            try Data(content.utf8).write(to: config)
            var requests = 0
            SignInHealthURLProtocol.handler = { request in
                requests += 1
                XCTAssertEqual(request.url?.absoluteString, "https://\(host):21446/healthz")
                return 200
            }
            let signIn = try fixture.controller.beginLegacySignIn().get()
            let done = expectation(description: "TLS health allows the route to reach the closed-window check")
            signIn.requireOpenWindow { result in
                switch result {
                case .success: XCTFail("closed window must refuse sign-in")
                case let .failure(error):
                    XCTAssertEqual((error as? HubSignInRefusal)?.code, "window_closed",
                        "route failed before the window check: \(error.localizedDescription); events \(fixture.events.all)")
                }
                done.fulfill()
            }
            wait(for: [done], timeout: 3)
            signIn.cancel()
            XCTAssertEqual(requests, 1)
            XCTAssertEqual(fixture.runner.calls.filter { $0.arguments.contains("window") }.count,
                host == "127.0.0.1" ? 1 : 2)
        }
        // Plain loopback still works; a failed health request is refused rather than
        // being confused with success at readiness or an arbitrary HTTP response.
        try Data("bind = \"127.0.0.1:21447\" # local listener\n".utf8).write(to: config)
        for response in [200, 503] {
            SignInHealthURLProtocol.handler = { request in
                XCTAssertEqual(request.url?.absoluteString, "http://127.0.0.1:21447/healthz")
                return response
            }
            let done = expectation(description: "plain health answer judged")
            HubController.waitForSignInHealth(configPath: config, sessionConfiguration: settings, timeout: 0.01) { result in
                switch result {
                case .success: XCTAssertEqual(response, 200)
                case .failure: XCTAssertEqual(response, 503)
                }
                done.fulfill()
            }
            wait(for: [done], timeout: 3)
        }
        XCTAssertThrowsError(try HubController.signInHealthEndpoint(Data("bind = '192.168.20.5:21446'\n".utf8)))
        XCTAssertThrowsError(try HubController.signInHealthEndpoint(Data("[tls]\npublic_url = 'http://192.168.20.5:21446'\ncertificate_path = '/fake.pem'\n".utf8)))
    }

}

// MARK: - Fixture

private final class Events {
    private let lock = NSLock()
    private var items: [String] = []
    func append(_ item: String) { lock.lock(); items.append(item); lock.unlock() }
    var all: [String] { lock.lock(); defer { lock.unlock() }; return items }
}

/// The M09-T04 contract, faked: `take-over-source-token` answers from `takeoverResults`.
private final class TakeoverRunner: HubCommandRunning {
    struct Call { let arguments: [String]; let stdin: String? }
    static let report = #"{"status":"taken_over","origin":"source_token","firstRefresh":"next_window","sourceSessionsSeen":0,"sourceNeverMutated":true}"#
    static let compatible = #"{"status":"compatible","reasonCode":"v4_4_compatible_schema","requiredVersion":"4.4.0","sourceSchemaRecognised":true,"schemaSetLabel":"0123456789ab","guidance":"Ready."}"#

    private let lock = NSLock()
    private var recorded: [Call] = []
    private let events: Events
    var takeoverResults: [Result<String, Error>] = []
    var statusOutputs: [String] = []
    var durableOrigin: String?
    var progress = "none_sent"
    var commitEvenOnFailure = false
    var holdNextStatus = false
    var heldStatus: (() -> Void)?


    init(events: Events) { self.events = events }

    var calls: [Call] { lock.lock(); defer { lock.unlock() }; return recorded }

    private func answer(_ arguments: [String], stdin: String?,
                        completion: @escaping (Result<String, Error>) -> Void) {
        lock.lock(); recorded.append(Call(arguments: arguments, stdin: stdin)); lock.unlock()
        if arguments.contains("take-over-source-token") {
            events.append("takeover")
            let result = takeoverResults.isEmpty ? .success(Self.report) : takeoverResults.removeFirst()
            if commitEvenOnFailure { durableOrigin = "source_token" }
            else if case let .success(output) = result, HubSourceTakeoverReport.isComplete(output) { durableOrigin = "source_token" }
            completion(result)
        } else if arguments.contains("source-check") {
            events.append("check")
            completion(.success(Self.compatible))
        } else if arguments.contains("migrate") {
            events.append("migrate")
            completion(.success(#"{"status":"imported","captureMode":"online-snapshot"}"#))
        } else if arguments.contains("preflight") {
            completion(.success(#"{"status":"ready","state":"awaiting_sign_in","provider":"legacy","collectorEnabled":false}"#))
        } else if arguments.contains("window") {
            completion(.success(#"{"phase":"closed","faulted":false}"#))
        } else if arguments.contains("status") {
            let sampled = statusOutputs.isEmpty ? Fixture.status(origin: durableOrigin, progress: progress) : statusOutputs.removeFirst()
            if holdNextStatus {
                holdNextStatus = false
                heldStatus = { completion(.success(sampled)) }
            } else { completion(.success(sampled)) }
        } else {
            completion(.failure(HubActionError.commandFailed("unexpected command")))
        }
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        answer(arguments, stdin: nil, completion: completion)
    }

    func run(arguments: [String], sensitiveInput: HubSensitiveInput,
             completion: @escaping (Result<String, Error>) -> Void) {
        let text = sensitiveInput.withBytes { String(decoding: $0, as: UTF8.self) }
        sensitiveInput.wipe()
        answer(arguments, stdin: text, completion: completion)
    }

    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        answer(arguments, stdin: stdin, completion: completion)
    }

    func run(arguments: [String], onOutputLine: @escaping (String) -> Void,
             completion: @escaping (Result<String, Error>) -> Void) {
        answer(arguments, stdin: nil, completion: completion)
    }

    func run(arguments: [String], timeout: TimeInterval,
             completion: @escaping (Result<String, Error>) -> Void) {
        answer(arguments, stdin: nil, completion: completion)
    }
}

private final class StopChecks {
    private let events: Events
    var results: [Result<SourceRunState, Error>] = []
    private(set) var requests: [HubSourceStopCheckRequest] = []

    init(events: Events) { self.events = events }

    var checker: HubSourceStopChecker {
        { [self] request, completion in
            events.append("stop-check")
            requests.append(request)
            let result = results.isEmpty ? .success(.stopped) : results.removeFirst()
            DispatchQueue.global().async { completion(result) }
        }
    }
}

private final class TakeoverService: HubServiceControlling {
    private(set) var calls: [String] = []
    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) { completion(.unloaded) }
    func run(arguments: [String], owner: HubLifecycleToken,
             completion: @escaping (Result<String, Error>) -> Void) {
        calls.append(arguments.joined(separator: " "))
        completion(.success(""))
    }
    func recoverAfterPause(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) {
        completion(.success(""))
    }
    func releaseRestartRecord(owner: HubLifecycleToken) {}
    func restartRecordHeld(by owner: HubLifecycleToken) -> Bool { false }
    func deleteHubAndData(owner: HubLifecycleToken,
                          completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        completion(.failure(HubActionError.preview))
    }
    func reconcile(owner: HubLifecycleToken, ensure: @escaping () throws -> HubLaunchAgentChange,
                   completion: @escaping (HubReconcileOutcome) -> Void) { completion(.completed) }
    var restartDutyOutstanding: Bool { false }
    func whenRestartDutySettled(_ callback: @escaping () -> Void) { callback() }
}

private final class Fixture {
    let home: URL
    let events = Events()
    let runner: TakeoverRunner
    let stops: StopChecks
    let service = TakeoverService()
    var controller: HubController!
    var connections = 0
    var connectionFailures: [Error] = []
    var passwordChecks: [Bool] = []
    var healthSettings: URLSessionConfiguration?
    static let sshPassword = "synthetic-ssh-password-61b4"
    var window: OnboardingWindowController!

    var folder: URL { home.appendingPathComponent("Library/Application Support/Tesatlas Hub", isDirectory: true) }

    init(takenOverAt: Int64? = nil, migrationWindow: Bool = true, validatedPaths: Bool = false) throws {
        home = validatedPaths ? HubTestScratch.validatedFixtureRoot("takeover-health") : HubTestScratch.directory
            .appendingPathComponent("tesatlas-hub-takeover-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: home.appendingPathComponent("Library/Application Support/Tesatlas Hub"),
                                                withIntermediateDirectories: true)
        runner = TakeoverRunner(events: events)
        stops = StopChecks(events: events)
        if let takenOverAt {
            try #"{"takenOverAt":\#(takenOverAt)}"#.write(
                to: home.appendingPathComponent("Library/Application Support/Tesatlas Hub/.source-takeover-first-refresh"),
                atomically: true, encoding: .utf8)
        }
        makeController()
        if migrationWindow {
            window = OnboardingWindowController(controller: controller, initialRoute: .migration,
                                                onComplete: { _ in })
        }
    }

    private func makeController() {
        controller = HubController(commandRunner: runner,
                                   serviceRunner: service,
                                   homeDirectory: home,
                                   serviceInstalledOverride: false,
                                   migrationConnector: { [unowned self] _, _, _, authentication, _, completion in
                                       self.connections += 1
                                       if case let .password(value) = authentication {
                                           self.passwordChecks.append(value == Self.sshPassword)
                                       }
                                       if !self.connectionFailures.isEmpty {
                                           completion(.failure(self.connectionFailures.removeFirst()))
                                           return
                                       }
                                       do { completion(.success(try Fixture.makeSession())) }
                                       catch { completion(.failure(error)) }
                                   },
                                   sourceStopChecker: stops.checker,
                                   importedHistoryHealthCheck: { [unowned self] _, completion in
                                       self.events.append("healthz")
                                       if let settings = self.healthSettings {
                                           HubController.waitForSignInHealth(configPath: self.controller.maintenanceConfigPath,
                                               sessionConfiguration: settings, timeout: 0.2, completion: completion)
                                       } else { completion(.success(())) }
                                   })
    }

    func writeMarker(_ phase: HubMigrationHandoverPhase) throws {
        try Data("{\"phase\":\"\(phase.rawValue)\",\"previousIntervalSeconds\":60,\"previousProvider\":\"legacy\",\"historyOnly\":false}".utf8)
            .write(to: controller.migrationHandoverMarkerForTests)
    }

    func restartWindow() throws {
        window?.close()
        makeController()
        let phase = try XCTUnwrap(controller.pendingMigrationHandoverPhase)
        window = OnboardingWindowController(controller: controller, resumeMigrationHandoverPhase: phase, onComplete: { _ in })
    }

    static func makeSession() throws -> SourceServerImportSession {
        let owner = try SourceImportDirectoryOwner.create(in: HubTestScratch.directory)
        let passwordFile = owner.directory.appendingPathComponent("postgres-password")
        try Data("fictional-db-password-91c2".utf8).write(to: passwordFile)
        return SourceServerImportSession(source: "postgresql://reader@127.0.0.1:55439/sourcedb",
                                         carID: "1", passwordFile: passwordFile, sourceVersion: "4.4.0",
                                         tunnel: Process(), temporaryDirectory: owner.directory,
                                         directoryOwner: owner)
    }

    static func status(origin: String?, nextRefreshAt: Int64? = nil, progress: String = "none_sent") -> String {
        var legacy: [String: Any] = ["present": origin != nil, "storeState": "ok"]
        if let origin { legacy["origin"] = origin; legacy["refreshOwner"] = "hub"; legacy["refreshProgress"] = progress }
        else { legacy["refreshProgress"] = NSNull() }
        if let nextRefreshAt { legacy["nextRefreshAt"] = nextRefreshAt; legacy["expiresAt"] = nextRefreshAt + 28_800 }
        let root: [String: Any] = [
            "status": "ok", "version": HubRelease.bundledVersion, "ready": false,
            "database": ["path": "/tmp/hub/catalogue.sqlite3", "bytes": 1_048_576],
            "vehicles": [["vehicleId": "7a5d69ab-8ea8-4056-8b2f-42c41c28ae36", "displayName": "Test Vehicle"]],
            "credentials": ["present": origin != nil],
            "legacyCredentials": legacy
        ]
        let data = try! JSONSerialization.data(withJSONObject: root, options: [.sortedKeys])
        return String(decoding: data, as: UTF8.self)
    }

    func tearDown() {
        window?.close()
        try? FileManager.default.removeItem(at: home)
    }

    func waitUntil(timeout: TimeInterval = 5, _ condition: () -> Bool,
                   file: StaticString = #filePath, line: UInt = #line) throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() {
            guard Date() < deadline else {
                XCTFail("condition not reached; events \(events.all)", file: file, line: line)
                throw HubActionError.commandTimedOut
            }
            RunLoop.main.run(until: Date().addingTimeInterval(0.01))
        }
    }

    func fillConnectionForm(password: Bool = false) throws {
        let popup = try XCTUnwrap(view(identifier: "onboarding.migration-authentication") as? NSPopUpButton)
        XCTAssertFalse(popup.isHiddenOrHasHiddenAncestor)
        if popup.indexOfSelectedItem != (password ? 1 : 0) {
            popup.selectItem(at: password ? 1 : 0)
            XCTAssertTrue(popup.sendAction(popup.action, to: popup.target))
        }
        for (identifier, value) in [("onboarding.source-server", "server.local"),
                                     ("onboarding.source-user", "user"),
                                     ("onboarding.source-port", "22")] {
            let field = try XCTUnwrap(view(identifier: identifier) as? NSTextField)
            XCTAssertFalse(field.isHiddenOrHasHiddenAncestor)
            XCTAssertNotNil(field.window)
            field.stringValue = value
            window.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: field))
        }
        if password {
            let field = try XCTUnwrap(view(identifier: "onboarding.source-password") as? NSSecureTextField)
            XCTAssertFalse(field.isHiddenOrHasHiddenAncestor)
            XCTAssertEqual(field.stringValue, "", "every reconnect must ask for a fresh password")
            XCTAssertFalse(window.continueEnabledForTests)
            field.stringValue = Self.sshPassword
            window.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: field))
        }
    }

    func clickContinue() throws {
        let button = try XCTUnwrap(view(identifier: "onboarding.continue") as? NSButton)
        XCTAssertFalse(button.isHiddenOrHasHiddenAncestor)
        XCTAssertTrue(button.isEnabled)
        button.performClick(nil)
    }

    func connectAndAcknowledge() throws {
        window.connectToSourceForTests(server: "server.local")
        try waitUntil { window.hasMigrationSessionForTests && !window.isBusyForTests }
        window.acknowledgeSourceReleaseForTests()
        try waitUntil { !window.isBusyForTests && events.all.contains("stop-check") }
    }

    func refreshedAccount() throws -> String {
        var account: String?
        controller.refresh { account = $0.account }
        try waitUntil { account != nil }
        return account!
    }

    func anyFileUnderHomeContains(_ needle: String) throws -> Bool {
        let bytes = Data(needle.utf8)
        guard let walker = FileManager.default.enumerator(at: home, includingPropertiesForKeys: [.isRegularFileKey]) else {
            return false
        }
        for case let url as URL in walker {
            guard (try? url.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile) == true,
                  let data = try? Data(contentsOf: url), data.count < 4_000_000 else { continue }
            if data.range(of: bytes) != nil { return true }
        }
        return false
    }

    func view(identifier: String) -> NSView? { PageReader(window).view(identifier: identifier) }
    func labels() -> [String] { PageReader(window).labels() }
    func buttonTitles() -> [String] { PageReader(window).buttonTitles() }
    func acknowledgements() -> [String] { PageReader(window).acknowledgements() }
}

/// Reads what the onboarding window currently shows.
private struct PageReader {
    let window: OnboardingWindowController
    init(_ window: OnboardingWindowController) { self.window = window }

    private func views(_ root: NSView?) -> [NSView] {
        guard let root else { return [] }
        return [root] + root.subviews.flatMap { views($0) }
    }

    private var root: NSView? { window.window?.contentView }

    func view(identifier: String) -> NSView? {
        views(root).first { $0.identifier?.rawValue == identifier }
    }

    func labels() -> [String] {
        views(root).compactMap { $0 as? NSTextField }.filter { !$0.isEditable }.map(\.stringValue)
    }

    func buttonTitles() -> [String] {
        views(root).compactMap { $0 as? NSButton }.map(\.title)
    }

    func acknowledgements() -> [String] {
        views(root).compactMap { $0 as? NSButton }
            .filter { $0.action == NSSelectorFromString("handoverChanged:") }
            .compactMap { $0.accessibilityLabel() }
    }

    func checkbox(label: String) -> NSButton? {
        views(root).compactMap { $0 as? NSButton }.first { $0.accessibilityLabel() == label }
    }
}

// MARK: - The stop-check script against a fake Docker

private enum StopCheckScriptHarness {
    struct Observation {
        let status: Int32
        let output: String
        let calls: [[String]]
    }

    static func run(running: String, withSource: Bool = true, discovery: Bool = false,
                    duplicateSource: Bool = false) throws -> Observation {
        let root = HubTestScratch.fixtureRoot("source-stop-check")
        let bin = root.appendingPathComponent("bin")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: root) }
        let scenario = root.appendingPathComponent("scenario.json")
        let calls = root.appendingPathComponent("calls.jsonl")
        var containers: [String: Any] = [
            "db-1": ["project": "fictional-project", "running": "true",
                     "env": ["POSTGRES_PASSWORD": "fictional-env-secret"]],
            "source-1": ["project": "fictional-project", "running": running,
                         "env": withSource
                            ? ["DATABASE_USER": "u", "DATABASE_PASS": "fictional-env-secret",
                               "DATABASE_NAME": "n", "ENCRYPTION_KEY": "fictional-env-secret"]
                            : ["UNRELATED": "fictional-env-secret"]],
            "dashboards-1": ["project": "fictional-project", "running": "true",
                             "env": ["DATABASE_USER": "u", "DATABASE_PASS": "fictional-env-secret"]]
        ]
        if duplicateSource { containers["source-2"] = containers["source-1"] }
        try JSONSerialization.data(withJSONObject: ["databases": ["db-1"], "containers": containers])
            .write(to: scenario)
        try Data().write(to: calls)
        for name in ["docker", "id"] {
            let command = bin.appendingPathComponent(name)
            try Data(fakeCommand.utf8).write(to: command)
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: command.path)
        }
        let done = DispatchSemaphore(value: 0)
        var outcome: Result<String, Error>?
        HubProcessExecutor.run(executable: SourceScriptShell.executable,
                               arguments: SourceScriptShell.arguments(scriptFile: root.appendingPathComponent("script.sh")),
                               stdin: discovery ? SourceDiscovery.script : SourceStopCheck.script,
                               environment: [
                                   "PATH": bin.path + ":/usr/bin:/bin",
                                   "SCENARIO": scenario.path, "CALLS": calls.path,
                                   "TESATLAS_USE_SUDO": "0"
                               ],
                               maximumOutputBytes: 64 * 1024, timeout: 30) { result in
            outcome = result
            done.signal()
        }
        XCTAssertEqual(done.wait(timeout: .now() + 35), .success)
        let status: Int32, output: String
        switch outcome {
        case let .success(text)?: status = 0; output = text
        case let .failure(HubActionError.commandExited(code, text))?: status = code; output = text
        case let .failure(error)?: throw error
        case nil: throw HubActionError.commandTimedOut
        }
        let log = try String(contentsOf: calls, encoding: .utf8).split(whereSeparator: \.isNewline).map {
            try JSONSerialization.jsonObject(with: Data($0.utf8)) as! [String]
        }
        return Observation(status: status, output: output, calls: log)
    }

    static let fakeCommand = #"""
#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
s=json.loads(Path(os.environ['SCENARIO']).read_text());name=Path(sys.argv[0]).name;a=sys.argv[1:]
with Path(os.environ['CALLS']).open('a') as f:f.write(json.dumps([name]+a)+'\n')
def out(value):print(value);raise SystemExit(0)
def reject():print('Unexpected fake command',file=sys.stderr);raise SystemExit(99)
if name=='id':
 if a==['-u']:out('501')
 reject()
if name!='docker':reject()
c=s['containers']
if a==['ps','--filter','label=com.docker.compose.service=database','--format','{{.ID}}']:out('\n'.join(s['databases']))
if a and a[0]=='ps':
 filters=[a[i+1] for i,v in enumerate(a[:-1]) if v=='--filter']
 project=next((f.split('=',2)[2] for f in filters if f.startswith('label=com.docker.compose.project=')),None)
 if project is None:reject()
 if 'label=com.docker.compose.service=database' in filters:out('\n'.join(s['databases']))
 out('\n'.join(k for k,v in sorted(c.items()) if v['project']==project and ('-a' in a or v['running']=='true')))
if a and a[0]=='exec':out('42')
if len(a)==4 and a[:2]==['inspect','-f'] and a[3] in c:
 t=a[2];v=c[a[3]]
 if t=='{{index .Config.Labels "com.docker.compose.project"}}':out(v['project'])
 if t=='{{range .Config.Env}}{{index (split . "=") 0}}{{println}}{{end}}':out('\n'.join(v['env']))
 if t=='{{.State.Running}}':out(v['running'])
 if t.startswith('{{range .Config.Env}}{{if eq '):out('\n'.join(k+'='+val for k,val in v['env'].items() if k in ['DATABASE_USER','DATABASE_PASS','DATABASE_NAME']))
 if t=='{{index .Config.Labels "org.opencontainers.image.version"}}':out('4.4.0')
 if t=='{{range $name, $value := .NetworkSettings.Networks}}{{println $name}}{{end}}':out('fake-network')
 if t=='{{with index .NetworkSettings.Networks "fake-network"}}{{.IPAddress}}{{end}}':out('172.20.0.9')
reject()
"""#
}

/// Delivers a synthetic HTTP response only after the production endpoint and
/// certificate reader have run. No socket, TLS handshake or Keychain call occurs.
private final class SignInHealthURLProtocol: URLProtocol {
    static var handler: ((URLRequest) -> Int)?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        guard let status = Self.handler?(request), let url = request.url else {
            client?.urlProtocol(self, didFailWithError: URLError(.unsupportedURL)); return
        }
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: url, statusCode: status,
            httpVersion: "HTTP/1.1", headerFields: nil)!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data("ok".utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}
