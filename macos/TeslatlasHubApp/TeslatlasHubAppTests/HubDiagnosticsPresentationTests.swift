// SPDX-License-Identifier: AGPL-3.0-only

import XCTest
@testable import Teslatlas_Hub

final class HubDiagnosticsPresentationTests: XCTestCase {
    func testParsesRealReportSectionsWithoutInventingChecks() {
        let report = """
        == doctor — Hub database, tokens, TLS, collector ==
        Duration: 20 ms
        {"status":"ok"}

        == preflight — selected provider credentials (failed) ==
        Duration: 9 ms
        missing credentials

        == future check ==
        useful detail
        """

        let rows = HubDiagnosticsPresentation.rows(from: report)

        XCTAssertEqual(rows.map(\.title), ["Environment doctor", "Preflight", "Future check"])
        XCTAssertEqual(rows.map(\.outcome), [.passed, .failed, .passed])
        XCTAssertEqual(rows[0].detail, "Status: ok")
        XCTAssertEqual(rows[1].detail, "missing credentials")
    }

    func testPrettyPrintedJSONUsesBoundedExistingStatusFieldsInsteadOfOpeningBrace() {
        let report = """
        == doctor — Hub database, tokens, TLS, collector ==
        Duration: 20 ms
        {
          "status": "ok",
          "provider": "fleet",
          "ready": true,
          "catalogue": {"journalMode": "wal"}
        }
        """

        XCTAssertEqual(HubDiagnosticsPresentation.rows(from: report), [
            HubDiagnosticRow(title: "Environment doctor",
                             detail: "Status: ok · Provider: fleet · Ready: yes",
                             outcome: .passed)
        ])
    }

    func testEmptyReportProducesNoSyntheticRows() {
        XCTAssertEqual(HubDiagnosticsPresentation.rows(from: ""), [])
    }

    func testIgnoresMalformedSectionsAndUsesFirstNonDurationDetail() {
        let report = """
        Current Hub summary
        no heading here

        ==  service   resume   ==
        Duration: 12 ms
        Hub collection resumed.

        == malformed (failed) == trailing text
        useful detail
        """

        XCTAssertEqual(HubDiagnosticsPresentation.rows(from: report), [
            HubDiagnosticRow(title: "Service resume", detail: "Hub collection resumed.", outcome: .passed)
        ])
    }

    func testRecentLogsSkipsItsReadDurationBeforeSelectingDetail() {
        let report = """
        == recent logs ==
        Read duration: 14 ms
        [INFO] Hub collection resumed
        """

        XCTAssertEqual(HubDiagnosticsPresentation.rows(from: report), [
            HubDiagnosticRow(title: "Recent logs", detail: "[INFO] Hub collection resumed", outcome: .passed)
        ])
    }

    private func localizedBundle(_ strings: [String: String]) throws -> (Bundle, URL) {
        let folder = HubTestScratch.directory
            .appendingPathComponent("hub-diagnostics-l10n-\(UUID().uuidString)", isDirectory: true)
        let english = folder.appendingPathComponent("en.lproj", isDirectory: true)
        try FileManager.default.createDirectory(at: english, withIntermediateDirectories: true)
        let text = strings.map { "\"\($0.key)\" = \"\($0.value)\";" }.joined(separator: "\n") + "\n"
        try text.write(to: english.appendingPathComponent("Localizable.strings"), atomically: true, encoding: .utf8)
        return (try XCTUnwrap(Bundle(url: folder)), folder)
    }

    func testServiceRecoveryLabelIsLocalisedAndTheReportTokenIsKept() throws {
        let (bundle, folder) = try localizedBundle(["hub.HubDiagnosticsPresentation.service_recovery": "Reprise du service"])
        defer { try? FileManager.default.removeItem(at: folder) }
        let report = "== service recovery (failed) ==\nThe restart could not be confirmed.\n\n== service recovery ==\nThe Hub was started again."
        let rows = HubDiagnosticsPresentation.rows(from: report, bundle: bundle)
        XCTAssertEqual(rows.map(\.title), ["Reprise du service", "Reprise du service"])
        XCTAssertEqual(rows.map(\.outcome), [.failed, .passed], "the report tokens still parse")
        XCTAssertEqual(HubDiagnosticsPresentation.rows(from: report).map(\.title), ["Service recovery", "Service recovery"],
                       "without a translation the English label is shown")
    }

    func testServiceDiagnosticLinePrefixIsLocalised() throws {
        let (bundle, folder) = try localizedBundle(["hub.service.diagnostic_line": "Service : %1$@"])
        defer { try? FileManager.default.removeItem(at: folder) }
        XCTAssertEqual(HubController.serviceDiagnosticLine("Move the app", bundle: bundle), "Service : Move the app")
        XCTAssertEqual(HubController.serviceDiagnosticLine("Move the app"), "Service: Move the app")
    }
}

/// M04-T03: Status shows the Hub's own login, its credential state (including re-login
/// needed and refused) and the collection window with its expiry, read from the running
/// service. No test opens, activates or orders front a window: the model and the Service
/// details rows are values, and the controller tests drive command doubles only.
final class CollectionStatusTests: XCTestCase {
    private static let expiry: Int64 = 1_800_000_000
    private static let start: Int64 = 1_799_990_000
    private static func text(_ date: Date) -> String { "t\(Int64(date.timeIntervalSince1970))" }

    private static func status(present: Bool = true, storeState: String = "ok",
                               expiresAt: String = "1800003600", exposure: Bool = false,
                               attention: String? = nil, ready: Bool = true,
                               reason: String? = nil, version: String = HubRelease.bundledVersion) -> String {
        let attentionJSON = attention.map { "\"\($0)\"" } ?? "null"
        let reasonJSON = reason.map { "\"\($0)\"" } ?? "null"
        return """
        {"status":"ok","version":"\(version)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1048576},"ready":\(ready),"readinessReason":\(reasonJSON),"collectionAttention":\(attentionJSON),"provider":"legacy","vehicles":[],"credentials":{"present":\(present)},"legacyCredentials":{"present":\(present),"expiresAt":\(present ? expiresAt : "null"),"nextRefreshAt":null,"origin":null,"refreshOwner":null,"storeState":"\(storeState)"},"fleetCredentials":{"present":false,"storeState":"ok"},"credentialStore":{"formatState":"complete","keyring":"ok","exposureSuspected":\(exposure)}}
        """
    }

    private static func window(_ phase: String, reason: String? = nil, trusted: Bool = true,
                               faulted: Bool = false) -> String {
        let reasonJSON = reason.map { "\"\($0)\"" } ?? "null"
        return """
        {"phase":"\(phase)","trusted":\(trusted),"faulted":\(faulted),"closeReason":\(reasonJSON),"seq":4,"grantDigest":"abc","startsAt":\(start),"deadline":\(expiry),"openedAt":\(start),"closingAt":null,"lastHandback":null}
        """
    }

    private func model(_ status: String?, _ window: HubCollectionStatus.WindowRead,
                       signIn: String? = nil) -> HubCollectionStatus {
        HubCollectionStatus(statusOutput: status, window: window, signInRefusal: signIn)
    }

    private func rows(_ value: HubCollectionStatus) -> [String: String] {
        Dictionary(uniqueKeysWithValues: value.rows(date: Self.text).map { ($0.label, $0.value) })
    }

    // Acceptance: own login, credential state and the open window with its expiry.

    func testOwnLoginValidCredentialAndOpenWindowShowTheirExpiry() {
        let value = model(Self.status(), .report(Self.window("open")))
        XCTAssertEqual(value.login, .own)
        XCTAssertEqual(value.credential, .valid(expiresAt: Date(timeIntervalSince1970: 1_800_003_600)))
        XCTAssertEqual(value.window, .open(expiresAt: Date(timeIntervalSince1970: TimeInterval(Self.expiry))))
        XCTAssertEqual(value.rows(date: Self.text).map(\.label),
                       ["Tesla login", "Credential state", "Collection window", "Fleet"])
        XCTAssertEqual(rows(value)["Tesla login"], "Signed in with the Hub's own login")
        XCTAssertEqual(rows(value)["Credential state"], "Valid until t1800003600")
        XCTAssertEqual(rows(value)["Collection window"], "Open until t\(Self.expiry)")
    }

    func testPendingAndClosingWindowsShowTheirTimes() {
        XCTAssertEqual(rows(model(Self.status(), .report(Self.window("pending"))))["Collection window"],
                       "Pending from t\(Self.start) until t\(Self.expiry)")
        XCTAssertEqual(rows(model(Self.status(), .report(Self.window("closing"))))["Collection window"],
                       "Closing (expiry t\(Self.expiry))")
    }

    func testClosedWindowShowsItsReasonInPlainWords() {
        let cases: [(String?, String)] = [
            ("expired", "Closed (expired)"), ("operator_close", "Closed (closed by the operator)"),
            ("clock_invalid", "Closed (clock check failed)"), ("reboot", "Closed (the Mac restarted)"),
            ("state_unverifiable", "Closed (state could not be verified)"),
            ("a_newer_reason", "Closed (a_newer_reason)"), (nil, "Closed")
        ]
        for (reason, expected) in cases {
            XCTAssertEqual(rows(model(Self.status(), .report(Self.window("closed", reason: reason))))["Collection window"],
                           expected, reason ?? "nil")
        }
        XCTAssertEqual(rows(model(Self.status(), .report(Self.window("closed", trusted: false))))["Collection window"],
                       "Closed (no trusted issuer)")
        XCTAssertEqual(model(Self.status(), .report(Self.window("closed", faulted: true))).window, .faulted)
    }

    // Acceptance: re-login needed and refused.

    func testReloginNeededFromTheWindowCloseAndFromTheService() {
        let closed = model(Self.status(present: false), .report(Self.window("closed", reason: "relogin_needed")))
        XCTAssertEqual(closed.login, .signedOut)
        XCTAssertEqual(closed.credential, .reloginNeeded)
        XCTAssertEqual(rows(closed)["Credential state"], "Re-login needed: sign in again in the next open window")
        XCTAssertEqual(rows(closed)["Collection window"], "Closed (re-login needed)")
        XCTAssertEqual(model(Self.status(attention: "collector_auth_sign_in_required"),
                             .report(Self.window("open"))).credential, .reloginNeeded)
        XCTAssertEqual(model(Self.status(storeState: "fresh_sign_in_required"),
                             .report(Self.window("closed"))).credential, .reloginNeeded)
    }

    func testOwnerAPIRefusedIsItsOwnState() {
        for value in [
            model(Self.status(), .report(Self.window("closed", reason: "owner_api_refused"))),
            model(Self.status(attention: "collector_api_refused"), .report(Self.window("closed")))
        ] {
            XCTAssertEqual(value.credential, .ownerAPIRefused)
            XCTAssertEqual(rows(value)["Credential state"], "Owner API refused: Tesla refused this login")
        }
    }

    func testAStoredLoginThatIsNotTheHubsOwnIsRefused() {
        let value = model(Self.status(storeState: "source_credential_disabled"), .report(Self.window("closed")))
        XCTAssertEqual(value.login, .notOwn(code: "source_credential_disabled"))
        XCTAssertEqual(value.credential, .refused(code: "source_credential_disabled"))
        XCTAssertEqual(rows(value)["Tesla login"], "Stored login is not the Hub's own")
        XCTAssertEqual(rows(value)["Credential state"], "Refused (source_credential_disabled)")
    }

    func testExposureSuspectedAndOtherStoreStates() {
        let exposed = model(Self.status(exposure: true), .report(Self.window("open")))
        XCTAssertEqual(exposed.credential, .exposureSuspected)
        XCTAssertEqual(rows(exposed)["Credential state"], "Exposure suspected: sign in again in the next open window")
        XCTAssertEqual(model(Self.status(storeState: "corrupt"), .report(Self.window("open"))).credential,
                       .needsAttention(code: "corrupt"))
        let signedOut = model(Self.status(present: false), .report(Self.window("closed", reason: "expired")))
        XCTAssertEqual(signedOut.credential, .notSignedIn)
        XCTAssertEqual(rows(signedOut)["Tesla login"], "Signed out")
    }

    // The service's refusals and the parked Fleet path.

    func testServiceNotRunningIsShownAndNotAsked() {
        XCTAssertEqual(model(Self.status(), .serviceNotLoaded).window, .serviceNotRunning)
        XCTAssertEqual(model(Self.status(), .failed(HubActionError.commandExited(
            1, "service_not_running: the Hub service is not running"))).window, .serviceNotRunning)
        XCTAssertEqual(rows(model(Self.status(), .serviceNotLoaded))["Collection window"],
                       "Unknown: Hub is not running")
        XCTAssertEqual(model(Self.status(), .failed(HubActionError.commandFailed("timeout"))).window, .unknown)
    }

    // M02-T10 round 2: the service's recovery state and its authenticated control channel.

    func testCredentialRecoveryPendingIsShownInsteadOfAPlainClosedWindow() {
        let pending = #"{"phase":"closed","trusted":true,"faulted":false,"closeReason":null,"seq":null,"grantDigest":null,"startsAt":null,"deadline":null,"openedAt":null,"closingAt":null,"lastHandback":null,"credentialRecoveryPending":true}"#
        let value = model(Self.status(), .report(pending))
        XCTAssertEqual(value.window, .recoveryPending)
        XCTAssertEqual(rows(value)["Collection window"],
                       "Closed (waiting for credential recovery: run restore-recovery-credentials)")
        let restored = pending.replacingOccurrences(of: #""credentialRecoveryPending":true"#,
                                                    with: #""credentialRecoveryPending":false"#)
        XCTAssertEqual(model(Self.status(), .report(restored)).window, .closed(reason: nil))
    }

    func testAnUnauthenticatedControlChannelIsNotTakenForTheHub() {
        let value = model(Self.status(), .failed(HubActionError.commandExited(
            1, "service_unauthenticated: the process on the control channel is not this Hub; nothing was sent")))
        XCTAssertEqual(value.window, .serviceUnauthenticated)
        XCTAssertEqual(rows(value)["Collection window"], "Unknown: the control channel is not this Hub")
    }

    func testSignInUnavailableIsShownAndOtherRefusalsAreNotKept() {
        let value = model(Self.status(present: false), .report(Self.window("open")), signIn: "sign_in_unavailable")
        XCTAssertEqual(rows(value)["Sign-in"], "Sign-in unavailable on this Hub")
        XCTAssertEqual(value.rows(date: Self.text).map(\.label),
                       ["Tesla login", "Credential state", "Sign-in", "Collection window", "Fleet"])
        XCTAssertNil(model(Self.status(), .report(Self.window("open")), signIn: "window_closed").signInRefusal)
    }

    func testFleetIsParkedWithNoAction() {
        let value = model(Self.status(), .report(Self.window("open")))
        XCTAssertEqual(rows(value)["Fleet"], "Parked: no Fleet action is available")
        let snapshot = { () -> HubSnapshot in var s = HubSnapshot.previewRunning; s.collection = value; return s }()
        let details = ServiceDetailsWindowController.collectionDetails(for: snapshot, date: Self.text)
        XCTAssertEqual(details.last, HubServiceDetail(label: "Fleet", value: "Parked: no Fleet action is available"))
    }

    // Fields are read defensively: nothing is invented for a missing or renamed field.

    func testMissingOrRenamedFieldsAreNotGuessed() {
        let bare = model(#"{"status":"ok","ready":true}"#, .report(#"{"state":"open"}"#))
        XCTAssertEqual(bare.login, .unknown)
        XCTAssertEqual(bare.credential, .unknown)
        XCTAssertEqual(bare.window, .unknown)
        XCTAssertEqual(rows(bare)["Credential state"], "Not reported")
        XCTAssertEqual(model(nil, .report("not json")).window, .unknown)
        // snake_case spellings and a millisecond expiry are still read.
        let snake = model(Self.status(expiresAt: "1800003600000"),
                          .report(#"{"phase":"open","expires_at":1800000000}"#))
        XCTAssertEqual(snake.window, .open(expiresAt: Date(timeIntervalSince1970: 1_800_000_000)))
        XCTAssertEqual(snake.credential, .valid(expiresAt: Date(timeIntervalSince1970: 1_800_003_600)))
        XCTAssertEqual(model(Self.status(), .report(#"{"phase":"closed","close_reason":"relogin_needed"}"#)).credential,
                       .reloginNeeded)
        // A boolean is not a time.
        XCTAssertNil(HubCollectionStatus.date(true))
    }

    // The controller: Status reads `window status` only, and only when it observes it.

    private final class Runner: HubCommandRunning {
        private let lock = NSLock()
        private var log: [[String]] = []
        var status: Result<String, Error>
        var window: Result<String, Error>
        var setup: Result<String, Error> = .success("{}")
        init(status: String, window: Result<String, Error>) {
            self.status = .success(status)
            self.window = window
        }
        var commands: [[String]] { lock.lock(); defer { lock.unlock() }; return log }
        var names: [String] {
            commands.map { args in args.filter { !$0.hasPrefix("--") && !$0.hasSuffix("config.toml") }.joined(separator: " ") }
        }
        func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
            lock.lock(); log.append(arguments); lock.unlock()
            if arguments.contains("window") { completion(window) }
            else if arguments.contains("setup") { completion(setup) }
            else { completion(status) }
        }
    }

    private func controller(_ runner: HubCommandRunning, loaded: HubServiceLoadState = .loaded,
                            observes: Bool = true) -> HubController {
        let service = OwnerService()
        service.loaded = loaded
        return HubController(commandRunner: runner, serviceRunner: service,
                             homeDirectory: HubTestScratch.directory, serviceInstalledOverride: true,
                             initialSnapshot: .previewRunning, observesCollectionWindow: observes)
    }

    private func refreshed(_ controller: HubController) -> HubSnapshot? {
        let finished = expectation(description: "refreshed")
        var value: HubSnapshot?
        controller.refresh { value = $0; finished.fulfill() }
        wait(for: [finished], timeout: 5)
        return value
    }

    func testStatusShowsLoginCredentialAndOpenWindowFromTheRunningService() throws {
        let runner = Runner(status: Self.status(), window: .success(Self.window("open")))
        let snapshot = try XCTUnwrap(refreshed(controller(runner)))
        XCTAssertEqual(runner.names, ["status", "window status"], "read-only: nothing opened, closed or signed in")
        let collection = try XCTUnwrap(snapshot.collection)
        XCTAssertEqual(collection.window, .open(expiresAt: Date(timeIntervalSince1970: TimeInterval(Self.expiry))))
        let details = ServiceDetailsWindowController.details(for: snapshot)
        XCTAssertEqual(details.first { $0.label == "Tesla login" }?.value, "Signed in with the Hub's own login")
        XCTAssertEqual(details.first { $0.label == "Collection window" }?.value,
                       "Open until \(HubCollectionStatus.defaultDateText(Date(timeIntervalSince1970: TimeInterval(Self.expiry))))")
        XCTAssertTrue(snapshot.diagnosticLines.contains("Fleet: Parked: no Fleet action is available"))
    }

    func testStatusShowsServiceNotRunningWithoutAskingTheWindow() throws {
        let runner = Runner(status: Self.status(), window: .success(Self.window("open")))
        let snapshot = try XCTUnwrap(refreshed(controller(runner, loaded: .unloaded)))
        XCTAssertEqual(runner.names, ["status"])
        XCTAssertEqual(snapshot.collection?.window, .serviceNotRunning)
        let refused = Runner(status: Self.status(), window: .failure(HubActionError.commandExited(
            1, "service_not_running: the Hub service is not running")))
        XCTAssertEqual(refreshed(controller(refused))?.collection?.window, .serviceNotRunning)
    }

    // M02-T26: between windows the Hub is idle, not "Attention needed".

    private func idleSnapshot(status: String = CollectionStatusTests.status(ready: false, reason: "collector_absent"),
                              window: String, loaded: HubServiceLoadState = .loaded) throws -> HubSnapshot {
        try XCTUnwrap(refreshed(controller(Runner(status: status, window: .success(window)), loaded: loaded)))
    }

    func testClosedWindowWithAGoodLoginIsIdleNotAttention() throws {
        for reason in [nil, "expired", "operator_close", "reboot"] as [String?] {
            let snapshot = try idleSnapshot(window: Self.window("closed", reason: reason))
            XCTAssertNotNil(snapshot.betweenWindows, "closed \(reason ?? "nil")")
            XCTAssertNil(snapshot.betweenWindows?.startsAt)
            XCTAssertEqual(snapshot.health, .running)
            XCTAssertEqual(snapshot.statusTitle, "Waiting for the next window")
            XCTAssertNotEqual(snapshot.statusTitle, HubHealth.degraded.title)
            XCTAssertNotEqual(snapshot.statusColor, NSColor.systemRed)
            XCTAssertEqual(snapshot.service, "Installed · waiting for the next window")
            XCTAssertEqual(snapshot.betweenWindows?.subtitle(), "Hub is waiting for its next collection window.")
        }
    }

    func testPendingWindowIsIdleAndSaysWhenItStarts() throws {
        let snapshot = try idleSnapshot(window: Self.window("pending"))
        let start = Date(timeIntervalSince1970: TimeInterval(Self.start))
        XCTAssertEqual(snapshot.betweenWindows, HubBetweenWindows(startsAt: start))
        XCTAssertEqual(snapshot.betweenWindows?.subtitle(date: Self.text),
                       "Hub is waiting for its next collection window, which starts t\(Self.start).")
        XCTAssertEqual(snapshot.health, .running)
    }

    func testIdleKeepsTheReadinessLineInDiagnostics() throws {
        let snapshot = try idleSnapshot(window: Self.window("closed", reason: "expired"))
        XCTAssertTrue(snapshot.diagnosticLines.contains("Readiness: collector_absent"))
    }

    func testARealFaultStillShowsAttentionNeeded() throws {
        let absent = Self.status(ready: false, reason: "collector_absent")
        struct Fault { let name: String; let status: String; let window: String; let loaded: HubServiceLoadState }
        let faults = [
            Fault(name: "open window, collector absent", status: absent,
                  window: Self.window("open"), loaded: .loaded),
            Fault(name: "closing window, collector absent", status: absent,
                  window: Self.window("closing"), loaded: .loaded),
            Fault(name: "faulted window", status: absent,
                  window: Self.window("closed", reason: "expired", faulted: true), loaded: .loaded),
            Fault(name: "untrusted window", status: absent,
                  window: Self.window("closed", reason: "expired", trusted: false), loaded: .loaded),
            Fault(name: "recovery pending", status: absent,
                  window: #"{"phase":"closed","trusted":true,"credentialRecoveryPending":true}"#, loaded: .loaded),
            Fault(name: "clock check failed", status: absent,
                  window: Self.window("closed", reason: "clock_invalid"), loaded: .loaded),
            Fault(name: "state unverifiable", status: absent,
                  window: Self.window("closed", reason: "state_unverifiable"), loaded: .loaded),
            Fault(name: "claim lost", status: absent,
                  window: Self.window("closed", reason: "claim_lost"), loaded: .loaded),
            Fault(name: "unknown close reason", status: absent,
                  window: Self.window("closed", reason: "a_newer_reason"), loaded: .loaded),
            Fault(name: "window unreadable", status: absent, window: "not json", loaded: .loaded),
            Fault(name: "re-login needed", status: Self.status(storeState: "fresh_sign_in_required", ready: false, reason: "collector_absent"),
                  window: Self.window("closed", reason: "expired"), loaded: .loaded),
            Fault(name: "owner API refused", status: Self.status(ready: false, reason: "collector_absent"),
                  window: Self.window("closed", reason: "owner_api_refused"), loaded: .loaded),
            Fault(name: "login absent", status: Self.status(present: false, ready: false, reason: "collector_absent"),
                  window: Self.window("closed", reason: "expired"), loaded: .loaded),
            Fault(name: "credential store refuses the login", status: Self.status(storeState: "unknown_origin", ready: false, reason: "collector_absent"),
                  window: Self.window("closed", reason: "expired"), loaded: .loaded),
            Fault(name: "credential exposure suspected", status: Self.status(exposure: true, ready: false, reason: "collector_absent"),
                  window: Self.window("closed", reason: "expired"), loaded: .loaded),
            Fault(name: "collection attention", status: Self.status(attention: "collector_stalled", ready: false, reason: "collector_absent"),
                  window: Self.window("closed", reason: "expired"), loaded: .loaded),
            Fault(name: "other readiness reason", status: Self.status(ready: false, reason: "database_unavailable"),
                  window: Self.window("closed", reason: "expired"), loaded: .loaded),
            Fault(name: "readiness reason absent", status: Self.status(ready: false),
                  window: Self.window("closed", reason: "expired"), loaded: .loaded),
            Fault(name: "version mismatch", status: Self.status(ready: false, reason: "collector_absent", version: "0.0.0-other"),
                  window: Self.window("closed", reason: "expired"), loaded: .loaded),
            Fault(name: "service state unknown", status: absent,
                  window: Self.window("closed", reason: "expired"), loaded: .unknown(HubActionError.commandTimedOut))
        ]
        for fault in faults {
            let snapshot = try idleSnapshot(status: fault.status, window: fault.window, loaded: fault.loaded)
            XCTAssertNil(snapshot.betweenWindows, fault.name)
            XCTAssertEqual(snapshot.health, .degraded, fault.name)
            XCTAssertEqual(snapshot.statusTitle, "Attention needed", fault.name)
            XCTAssertEqual(snapshot.statusColor, NSColor.systemRed, fault.name)
        }
    }

    func testAStoppedServiceIsStoppedNotIdle() throws {
        let snapshot = try idleSnapshot(window: Self.window("closed", reason: "expired"), loaded: .unloaded)
        XCTAssertNil(snapshot.betweenWindows)
        XCTAssertEqual(snapshot.health, .stopped)
    }

    func testServiceDetailsSaysWaitingBetweenWindowsNotThatTheCollectorIsRunning() throws {
        let running = "The background collector is running on this Mac."
        XCTAssertEqual(ServiceDetailsWindowController.runningDetail(idle: nil), running)
        let closed = try idleSnapshot(window: Self.window("closed", reason: "expired"))
        XCTAssertEqual(ServiceDetailsWindowController.runningDetail(idle: closed.betweenWindows),
                       "Hub is waiting for its next collection window.")
        let pending = try idleSnapshot(window: Self.window("pending"))
        XCTAssertEqual(ServiceDetailsWindowController.runningDetail(idle: pending.betweenWindows),
                       pending.betweenWindows?.subtitle())
        XCTAssertTrue(ServiceDetailsWindowController.runningDetail(idle: pending.betweenWindows)
            .contains("which starts"))
    }

    func testAReadyHubWithCollectionAttentionShowsAttentionNeededButStaysHealthy() throws {
        let snapshot = try idleSnapshot(
            status: Self.status(attention: "collector_cap_reached", ready: true),
            window: Self.window("closed", reason: "expired"))
        XCTAssertNil(snapshot.betweenWindows)
        XCTAssertEqual(snapshot.health, .running)
        XCTAssertTrue(snapshot.needsCollectionAttention)
        XCTAssertEqual(snapshot.statusTitle, "Attention needed")
        XCTAssertEqual(snapshot.statusColor, NSColor.systemRed)
        XCTAssertTrue(snapshot.diagnosticLines.contains(
            "Collection attention: The local monthly cap has been reached"))
        // No attention: still the plain running headline.
        let plain = try idleSnapshot(status: Self.status(), window: Self.window("open"))
        XCTAssertFalse(plain.needsCollectionAttention)
        XCTAssertEqual(plain.statusTitle, "Hub is running")
        XCTAssertEqual(plain.statusColor, NSColor.systemGreen)
    }

    func testAReadyHubIsRunningNotIdle() throws {
        let snapshot = try idleSnapshot(status: Self.status(),
                                        window: Self.window("closed", reason: "expired"))
        XCTAssertNil(snapshot.betweenWindows)
        XCTAssertEqual(snapshot.health, .running)
        XCTAssertEqual(snapshot.statusTitle, "Hub is running")
    }

    func testAHubWithoutAWindowReadIsNeverIdle() throws {
        let runner = Runner(status: Self.status(ready: false, reason: "collector_absent"),
                            window: .success(Self.window("closed", reason: "expired")))
        let snapshot = try XCTUnwrap(refreshed(controller(runner, observes: false)))
        XCTAssertNil(snapshot.betweenWindows)
        XCTAssertEqual(snapshot.health, .degraded)
    }

    func testAControllerThatDoesNotObserveTheWindowKeepsItsCommands() throws {
        let runner = Runner(status: Self.status(), window: .success(Self.window("open")))
        let snapshot = try XCTUnwrap(refreshed(controller(runner, observes: false)))
        XCTAssertEqual(runner.names, ["status"])
        XCTAssertNil(snapshot.collection)
        XCTAssertFalse(ServiceDetailsWindowController.details(for: snapshot).contains { $0.label == "Collection window" })
    }

    func testSignInUnavailableStaysOnStatusUntilTheNextSignIn() throws {
        let runner = Runner(status: Self.status(present: false), window: .success(Self.window("open")))
        runner.setup = .failure(HubActionError.commandExited(
            1, "sign_in_unavailable: The Hub could not reach Tesla's sign-in service."))
        let hub = controller(runner)
        let signedIn = expectation(description: "sign-in refused")
        hub.configureTeslaAccount(grant: TeslaAuthCodeGrant(code: "one-time-code",
                                                            codeVerifier: String(repeating: "v", count: 64),
                                                            redirectURI: "tesla://auth/callback")) { result in
            if case .success = result { XCTFail("expected sign_in_unavailable") }
            signedIn.fulfill()
        }
        wait(for: [signedIn], timeout: 5)
        let snapshot = try XCTUnwrap(refreshed(hub))
        XCTAssertEqual(snapshot.collection?.signInRefusal, "sign_in_unavailable")
        XCTAssertEqual(ServiceDetailsWindowController.details(for: snapshot).first { $0.label == "Sign-in" }?.value,
                       "Sign-in unavailable on this Hub")
        XCTAssertFalse(runner.commands.joined().contains { $0.contains("token") }, "no token is passed")
    }

    // M02-T27: the installed Hub's own `status` and `window status` output, byte for byte
    // (the data path anonymised), run through the app's real process executor and refresh.

    private static let installedStatus = #"""
{"collectionAttention":null,"collector":null,"collectorRunning":false,"credentialStore":{"backupsWaiting":false,"blockedReason":{"fleetApi":null,"ownerApi":null},"cleanupNeededBytes":null,"durability":"ok","exposureSuspected":false,"formatState":"complete","keyAgeDays":0,"keyring":"ok","legacyFences":0,"opaqueLegacyFences":0,"secretsBackupExcluded":true,"tokenEntryRequiresSignIn":false},"credentials":{"present":true},"database":{"bytes":2347008,"path":"/tmp/hub/hub.sqlite"},"fleetCredentials":{"expiresAt":null,"nextRefreshAt":null,"origin":null,"present":false,"refreshOwner":null,"scopeStatus":null,"scopes":null,"storeState":"ok"},"fleetTelemetry":{"configured":false,"deliveryPolicy":null,"enabled":false,"mode":"disabled","operationalState":"disabled","paidVehicleDataPolling":false},"legacyCredentials":{"expiresAt":1794728708,"nextRefreshAt":1794721582,"origin":"hub_login","present":true,"refreshOwner":"hub","storeState":"ok"},"provider":"legacy","readinessDetail":["collector_gap_unnotified"],"readinessReason":"collector_absent","ready":false,"status":"ok","vehicle":{"activityState":"offline","batteryLevel":42,"displayName":"Athena","latestObservationId":681,"latestObservedAtMs":1794700221057,"latestReceivedAtMs":1794700221057,"locationName":null,"sourceCarId":1493114796524256,"telemetryStatus":"cached","teslaEid":1493114796524256,"vehicleId":"b26ced23-b09d-430b-8760-b712b12d3aa8"},"vehicles":[{"activityState":"offline","batteryLevel":42,"displayName":"Athena","latestObservationId":681,"latestObservedAtMs":1794700221057,"latestReceivedAtMs":1794700221057,"locationName":null,"sourceCarId":1493114796524256,"telemetryStatus":"cached","teslaEid":1493114796524256,"vehicleId":"b26ced23-b09d-430b-8760-b712b12d3aa8"}],"version":"2026.36.2"}
"""#
    private static let installedWindow = #"""
{"closeReason":"expired","closingAt":null,"credentialRecoveryPending":false,"deadline":null,"faulted":false,"grantDigest":null,"lastHandback":{"closed_at":1794703800,"coverage_seconds":3600,"gaps":[],"grant_digest":"bc41684a9481188b18e21d46720e97dddfcfd21126b97b1ee312283d630a2b5f","high_water":{"charges":null,"drives":1073741824,"positions":1073742283},"opened_at":1794700200,"reason":"expired","seq":4},"openedAt":null,"phase":"closed","seq":null,"startsAt":null,"trusted":true}
"""#

    /// The app's own process executor, with `/bin/cat` standing in for the Hub binary.
    private final class ExecutorRunner: HubCommandRunning {
        let statusFile: URL, windowFile: URL
        init(status: URL, window: URL) { statusFile = status; windowFile = window }
        func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
            let file = arguments.contains("window") ? windowFile : statusFile
            HubProcessExecutor.run(executable: URL(fileURLWithPath: "/bin/cat"), arguments: [file.path],
                                   timeout: 10, completion: completion)
        }
    }

    func testTheInstalledHubsOwnOutputBetweenWindowsIsIdle() throws {
        let directory = HubTestScratch.directory
        let statusFile = directory.appendingPathComponent("installed-status.json")
        let windowFile = directory.appendingPathComponent("installed-window.json")
        try Self.installedStatus.write(to: statusFile, atomically: true, encoding: .utf8)
        try Self.installedWindow.write(to: windowFile, atomically: true, encoding: .utf8)
        let runners: [(String, HubCommandRunning)] = [
            ("string runner", Runner(status: Self.installedStatus, window: .success(Self.installedWindow))),
            ("process executor", ExecutorRunner(status: statusFile, window: windowFile))
        ]
        for (name, runner) in runners {
            let snapshot = try XCTUnwrap(refreshed(controller(runner)), name)
            XCTAssertEqual(snapshot.version, HubRelease.bundledVersion, name)
            XCTAssertEqual(snapshot.readinessReason, "collector_absent", name)
            XCTAssertNil(snapshot.collectionAttention, name)
            XCTAssertEqual(snapshot.collection?.login, .own, name)
            XCTAssertEqual(snapshot.collection?.window, .closed(reason: "expired"), name)
            XCTAssertNotNil(snapshot.betweenWindows, name)
            XCTAssertEqual(snapshot.health, .running, name)
            XCTAssertEqual(snapshot.statusTitle, "Waiting for the next window", name)
            XCTAssertEqual(snapshot.service, "Installed · waiting for the next window", name)
        }
    }

    func testTheMainWindowShowsTheIdleStateForTheInstalledHubsOutput() throws {
        let runner = Runner(status: Self.installedStatus, window: .success(Self.installedWindow))
        let hub = controller(runner)
        let window = MainWindowController(controller: hub)
        func labels(_ view: NSView?) -> [String] {
            guard let view else { return [] }
            return ((view as? NSTextField).map { [$0.stringValue] } ?? []) + view.subviews.flatMap { labels($0) }
        }
        let deadline = Date().addingTimeInterval(10)
        while Date() < deadline, !labels(window.window?.contentView).contains("Waiting for the next window") {
            RunLoop.current.run(until: Date().addingTimeInterval(0.05))
        }
        let shown = labels(window.window?.contentView)
        XCTAssertTrue(shown.contains("Waiting for the next window"), "\(shown)")
        XCTAssertTrue(shown.contains("Installed · waiting for the next window"), "\(shown)")
        XCTAssertFalse(shown.contains("Attention needed"), "\(shown)")
        XCTAssertFalse(shown.contains("Installed · needs attention"), "\(shown)")
        withExtendedLifetime(window) {}
    }
}
