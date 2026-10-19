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
