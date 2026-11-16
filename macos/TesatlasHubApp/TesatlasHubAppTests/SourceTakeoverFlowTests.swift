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
        XCTAssertEqual(order.filter { $0 == "stop-check" }.count, 5, "\(order)")
        XCTAssertLessThan(try XCTUnwrap(order.lastIndex(of: "stop-check")), migrate, "\(order)")
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
        fixture.window.enterSourceKeyForTests(Self.key)
        XCTAssertFalse(fixture.window.continueEnabledForTests)
        XCTAssertTrue(fixture.labels().contains { $0.contains("could not read whether the source application is stopped") })

        fixture.stops.results = [.failure(HubActionError.commandExited(255, "Control socket connect failed"))]
        fixture.window.continueForTests()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
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
        XCTAssertNotNil(fixture.controller.sourceTakeoverRecordedAt)
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
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(input.utf8)) as? [String: String])
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
        XCTAssertEqual(fixture.controller.pendingMigrationHandoverPhase, .awaitingVerification)
        XCTAssertNil(fixture.controller.sourceTakeoverRecordedAt)
        XCTAssertTrue(fixture.window.hasMigrationSessionForTests)
        XCTAssertEqual(fixture.service.calls, [])

        // A report that is not the contract's is a failure too, and writes nothing.
        fixture.window.enterSourceKeyForTests(Self.key)
        XCTAssertTrue(fixture.window.continueEnabledForTests)
        fixture.window.continueForTests()
        try fixture.waitUntil { !fixture.window.isBusyForTests }
        XCTAssertEqual(fixture.window.takeoverFailureCodeForTests, "takeover_failed")
        XCTAssertNil(fixture.controller.sourceTakeoverRecordedAt)

        // Retry runs the takeover alone: no second import.
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.currentRoute == .verify }
        XCTAssertEqual(fixture.events.all.filter { $0 == "migrate" }.count, 1)
        XCTAssertEqual(fixture.events.all.filter { $0 == "takeover" }.count, 3)
        XCTAssertNotNil(fixture.controller.sourceTakeoverRecordedAt)
        XCTAssertEqual(fixture.controller.registeredMigrationSessionCount, 0)
    }

    func testSignInInsteadKeepsTheImportAndLeadsToTheOwnSignIn() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        fixture.runner.takeoverResults = [
            .failure(HubActionError.commandExited(1, #"{"error":"source_still_running"}"#))
        ]
        try fixture.connectAndAcknowledge()
        fixture.window.enterSourceKeyForTests(Self.key)
        fixture.window.continueForTests()
        try fixture.waitUntil { fixture.window.takeoverFailureCodeForTests != nil }
        XCTAssertEqual(fixture.window.takeoverFailureCodeForTests, "source_still_running")
        XCTAssertTrue(fixture.labels().contains { $0.contains("still connected to its database") })

        fixture.window.declineTakeoverForTests()
        XCTAssertEqual(fixture.window.currentRoute, .verify)
        XCTAssertTrue(fixture.window.takeoverDeclinedForTests)
        XCTAssertFalse(fixture.window.hasMigrationSessionForTests)
        XCTAssertEqual(fixture.controller.registeredMigrationSessionCount, 0)
        XCTAssertEqual(fixture.controller.pendingMigrationHandoverPhase, .awaitingVerification)
        XCTAssertNil(fixture.controller.sourceTakeoverRecordedAt)
        XCTAssertEqual(fixture.events.all.filter { $0 == "takeover" }.count, 1)

        // The finish page says the Tesla sign-in comes next and keeps the earlier acknowledgement.
        fixture.window.showFinishForTests()
        let text = fixture.labels()
        XCTAssertTrue(text.contains("After Hub starts, sign in with Tesla on the next page so Hub has its own login."))
        XCTAssertFalse(text.contains { $0.hasPrefix("Hub now holds your Tesla login") })
        XCTAssertTrue(fixture.acknowledgements().contains(
            "I have disabled Tesla access in the source application to avoid duplicate requests"))
    }

    // MARK: Finish copy

    func testFinishPageAfterTakeoverCarriesTheGoingBackCopyAndNewAcknowledgement() throws {
        let (rig, window) = try Self.resumedFinish(takenOverAt: 1_800_000_000)
        defer { window.close(); rig.tearDown() }
        let page = PageReader(window)
        XCTAssertEqual(window.currentRoute, .finish)

        let text = page.labels()
        XCTAssertTrue(text.contains("Hub now holds your Tesla login. Do not start the source application again: after Hub's first refresh its stored login no longer works, and running both on one login can lock you out."))
        XCTAssertTrue(text.contains("To go back before Hub's first refresh: sign out in Hub, then start the source application. Its stored login still works."))
        XCTAssertTrue(text.contains("To go back after the first refresh: sign out in Hub, start the source application and sign it in again with its own sign-in page. Hub never writes to the source database and never exports a login."))
        XCTAssertEqual(page.acknowledgements(), ["I will keep the source application stopped"])
        XCTAssertFalse(text.contains("I have disabled Tesla access in the source application to avoid duplicate requests"))

        // Start Hub waits for the acknowledgement.
        XCTAssertEqual(window.continueTitleForTests, "Start Hub")
        XCTAssertFalse(window.continueEnabledForTests)
        let checkbox = try XCTUnwrap(page.checkbox(label: "I will keep the source application stopped"))
        checkbox.performClick(nil)
        XCTAssertTrue(window.continueEnabledForTests)
    }

    func testFinishPageWithoutATakeoverKeepsTheEarlierAcknowledgement() throws {
        let (rig, window) = try Self.resumedFinish(takenOverAt: nil)
        defer { window.close(); rig.tearDown() }
        let page = PageReader(window)
        XCTAssertEqual(page.acknowledgements(),
                       ["I have disabled Tesla access in the source application to avoid duplicate requests"])
        XCTAssertFalse(page.labels().contains { $0.hasPrefix("Hub now holds your Tesla login") })
    }

    /// A Hub resumed at its finish page (the shared rig writes the handover gate), with or
    /// without this app's takeover record.
    private static func resumedFinish(takenOverAt: Int64?) throws -> (OwnerRig, OnboardingWindowController) {
        let rig = try OwnerRig(OwnerRig.Options(installed: false, marker: "awaiting_handover", markerInterval: 60))
        if let takenOverAt {
            try #"{"takenOverAt":\#(takenOverAt)}"#.write(
                to: rig.configPath.deletingLastPathComponent().appendingPathComponent(".source-takeover-first-refresh"),
                atomically: true, encoding: .utf8)
        }
        let window = OnboardingWindowController(controller: rig.controller,
                                                resumeMigrationHandoverPhase: .awaitingHandover,
                                                onComplete: { _ in })
        return (rig, window)
    }

    // MARK: Status until the first refresh

    func testStatusShowsTakenOverUntilTheFirstRefresh() throws {
        let takenOverAt: Int64 = 1_800_000_000
        let fixture = try Fixture(takenOverAt: takenOverAt, migrationWindow: false)
        defer { fixture.tearDown() }
        fixture.runner.statusOutputs = [
            Fixture.status(origin: "source_token", nextRefreshAt: takenOverAt - 5),
            Fixture.status(origin: "source_token", nextRefreshAt: takenOverAt + 25_000),
            Fixture.status(origin: "source_token", nextRefreshAt: takenOverAt - 5)
        ]
        XCTAssertEqual(try fixture.refreshedAccount(), "Taken over from the source application; first refresh at the next window")
        XCTAssertNotNil(fixture.controller.sourceTakeoverRecordedAt)

        // The first refresh moved the schedule on: the ordinary line, and the record retires.
        XCTAssertEqual(try fixture.refreshedAccount(), "Connected")
        XCTAssertNil(fixture.controller.sourceTakeoverRecordedAt)
        XCTAssertEqual(try fixture.refreshedAccount(), "Connected")
    }

    func testStatusLineNeedsTheTakenOverOriginAndAHealthyPair() throws {
        let takenOverAt: Int64 = 1_800_000_000
        let due = [String: Any]()
        XCTAssertTrue(HubSourceTakeoverStatus.awaitingFirstRefresh(
            legacy: ["present": true, "origin": "source_token", "storeState": "ok", "nextRefreshAt": takenOverAt],
            takenOverAt: takenOverAt))
        for legacy: [String: Any]? in [
            nil, due,
            ["present": false, "origin": "source_token", "nextRefreshAt": takenOverAt],
            ["present": true, "origin": "hub_login", "storeState": "ok", "nextRefreshAt": takenOverAt],
            ["present": true, "origin": "source_token", "storeState": "refresh_token_spent", "nextRefreshAt": takenOverAt],
            ["present": true, "origin": "source_token", "storeState": "ok"],
            ["present": true, "origin": "source_token", "storeState": "ok", "nextRefreshAt": takenOverAt + 61]
        ] {
            XCTAssertFalse(HubSourceTakeoverStatus.awaitingFirstRefresh(legacy: legacy, takenOverAt: takenOverAt),
                           String(describing: legacy))
        }

        // A signed-out Hub retires the record as well.
        let fixture = try Fixture(takenOverAt: takenOverAt, migrationWindow: false)
        defer { fixture.tearDown() }
        fixture.runner.statusOutputs = [Fixture.status(origin: nil, nextRefreshAt: nil)]
        _ = try fixture.refreshedAccount()
        XCTAssertNil(fixture.controller.sourceTakeoverRecordedAt)
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

    init(events: Events) { self.events = events }

    var calls: [Call] { lock.lock(); defer { lock.unlock() }; return recorded }

    private func answer(_ arguments: [String], stdin: String?,
                        completion: @escaping (Result<String, Error>) -> Void) {
        lock.lock(); recorded.append(Call(arguments: arguments, stdin: stdin)); lock.unlock()
        if arguments.contains("take-over-source-token") {
            events.append("takeover")
            completion(takeoverResults.isEmpty ? .success(Self.report) : takeoverResults.removeFirst())
        } else if arguments.contains("source-check") {
            events.append("check")
            completion(.success(Self.compatible))
        } else if arguments.contains("migrate") {
            events.append("migrate")
            completion(.success(#"{"status":"imported","captureMode":"online-snapshot"}"#))
        } else if arguments.contains("status"), !statusOutputs.isEmpty {
            completion(.success(statusOutputs.removeFirst()))
        } else {
            completion(.failure(HubActionError.commandFailed("unexpected command")))
        }
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        answer(arguments, stdin: nil, completion: completion)
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
    let controller: HubController
    var window: OnboardingWindowController!

    var folder: URL { home.appendingPathComponent("Library/Application Support/Tesatlas Hub", isDirectory: true) }

    init(takenOverAt: Int64? = nil, migrationWindow: Bool = true) throws {
        home = HubTestScratch.directory
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
        controller = HubController(commandRunner: runner,
                                   serviceRunner: service,
                                   homeDirectory: home,
                                   serviceInstalledOverride: false,
                                   migrationConnector: { _, _, _, _, _, completion in
                                       do { completion(.success(try Fixture.makeSession())) }
                                       catch { completion(.failure(error)) }
                                   },
                                   sourceStopChecker: stops.checker)
        if migrationWindow {
            window = OnboardingWindowController(controller: controller, initialRoute: .migration,
                                                onComplete: { _ in })
        }
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

    static func status(origin: String?, nextRefreshAt: Int64?) -> String {
        var legacy: [String: Any] = ["present": origin != nil, "storeState": "ok"]
        if let origin { legacy["origin"] = origin; legacy["refreshOwner"] = "hub" }
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

    static func run(running: String, withSource: Bool = true) throws -> Observation {
        let root = HubTestScratch.fixtureRoot("source-stop-check")
        let bin = root.appendingPathComponent("bin")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: root) }
        let scenario = root.appendingPathComponent("scenario.json")
        let calls = root.appendingPathComponent("calls.jsonl")
        let containers: [String: Any] = [
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
        HubProcessExecutor.run(executable: URL(fileURLWithPath: "/bin/sh"), arguments: ["-s"],
                               stdin: SourceStopCheck.script,
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
if len(a)==6 and a[:3]==['ps','-a','--filter'] and a[3].startswith('label=com.docker.compose.project=') and a[4:]==['--format','{{.ID}}']:
 project=a[3].split('=',2)[2];out('\n'.join(k for k,v in sorted(c.items()) if v['project']==project))
if len(a)==4 and a[:2]==['inspect','-f'] and a[3] in c:
 t=a[2];v=c[a[3]]
 if t=='{{index .Config.Labels "com.docker.compose.project"}}':out(v['project'])
 if t=='{{range .Config.Env}}{{index (split . "=") 0}}{{println}}{{end}}':out('\n'.join(v['env']))
 if t=='{{.State.Running}}':out(v['running'])
reject()
"""#
}
