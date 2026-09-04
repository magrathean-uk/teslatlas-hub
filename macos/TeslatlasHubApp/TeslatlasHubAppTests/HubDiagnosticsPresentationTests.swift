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
        XCTAssertEqual(rows[1].detail, "missing credentials")
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
}
