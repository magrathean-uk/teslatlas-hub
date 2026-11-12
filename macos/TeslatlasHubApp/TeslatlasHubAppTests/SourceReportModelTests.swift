// SPDX-License-Identifier: AGPL-3.0-only

import Foundation
import XCTest
@testable import Teslatlas_Hub

final class SourceReportModelTests: XCTestCase {
    private func fixture(_ name: String) throws -> [String: Any] {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("Fixtures/" + name + ".json")
        return try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
    }

    private func output(_ value: [String: Any]) throws -> String {
        String(decoding: try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }

    func testSourceCheckReportRecognisedFixtureAdmits() throws {
        let report = try XCTUnwrap(HubSourceCheckResult.parse(output(fixture("source-check-recognised")), historyOnly: true))
        XCTAssertEqual(report.schemaRecognised, true)
        XCTAssertEqual(report.schemaSetLabel, "c497f904ef74")
        XCTAssertEqual(report.recognitionKey, "hub.import.recognised")
        XCTAssertTrue(report.compatible)
        XCTAssertTrue(report.mayImport)
    }

    func testSourceCheckReportNotRecognisedFixtureRefuses() throws {
        let value = try fixture("source-check-not-recognised")
        XCTAssertNil(value["credentialScope"])
        XCTAssertNil(value["applicationVersionStatus"])
        let report = try XCTUnwrap(HubSourceCheckResult.fromCommandResult(
            .failure(HubActionError.commandExited(1, try output(value))), historyOnly: true))
        XCTAssertEqual(report.schemaRecognised, false)
        XCTAssertNil(report.schemaSetLabel)
        XCTAssertEqual(report.recognitionKey, "hub.import.notRecognised")
        XCTAssertEqual(report.message, value["guidance"] as? String)
        XCTAssertFalse(report.mayImport)
    }

    func testSourceCheckReportWithoutRecognitionNeverAdmits() throws {
        let original = try fixture("source-check-recognised")
        for invalid in [NSNull(), false, 0, 1, "true", [:] as [String: String], [] as [String]] as [Any] {
            var value = original
            value["sourceSchemaRecognised"] = invalid
            let report = try XCTUnwrap(HubSourceCheckResult.parse(output(value), historyOnly: true))
            XCTAssertFalse(report.compatible)
            XCTAssertFalse(report.mayImport)
        }
        var missing = original
        missing.removeValue(forKey: "sourceSchemaRecognised")
        XCTAssertFalse(try XCTUnwrap(HubSourceCheckResult.parse(output(missing), historyOnly: true)).mayImport)
    }

    func testSourceCheckReportWithInvalidLabelNeverAdmits() throws {
        let original = try fixture("source-check-recognised")
        for invalid in [NSNull(), "c497f904", "C497F904EF74", "c497f904ef74\n", 123, "not-a-label"] as [Any] {
            var value = original
            value["schemaSetLabel"] = invalid
            XCTAssertFalse(try XCTUnwrap(HubSourceCheckResult.parse(output(value), historyOnly: true)).mayImport)
        }
        var missing = original
        missing.removeValue(forKey: "schemaSetLabel")
        XCTAssertFalse(try XCTUnwrap(HubSourceCheckResult.parse(output(missing), historyOnly: true)).mayImport)
    }

    func testSourceCheckHistoryOnlySuccessRequiresScopeMetadata() throws {
        let original = try fixture("source-check-recognised")
        for (key, invalid) in [("credentialScope", "all"), ("applicationVersionStatus", "known")] {
            var value = original
            value[key] = invalid
            XCTAssertNil(try HubSourceCheckResult.parse(output(value), historyOnly: true))
            value.removeValue(forKey: key)
            XCTAssertNil(try HubSourceCheckResult.parse(output(value), historyOnly: true))
        }
    }

    func testSourceCheckFailedCommandNeverAdmitsCompatibleOutput() throws {
        let value = try fixture("source-check-recognised")
        let report = try XCTUnwrap(HubSourceCheckResult.fromCommandResult(
            .failure(HubActionError.commandExited(1, try output(value))), historyOnly: true))
        XCTAssertEqual(report.schemaRecognised, true)
        XCTAssertFalse(report.compatible)
        XCTAssertFalse(report.mayImport)
    }

    func testSourceCheckConfirmationRemainsRecognisedWithoutAdmission() throws {
        var value = try fixture("source-check-recognised")
        value["status"] = "confirmation_required"
        let report = try XCTUnwrap(HubSourceCheckResult.parse(output(value), historyOnly: true))
        XCTAssertEqual(report.schemaRecognised, true)
        XCTAssertFalse(report.mayImport)
    }

    func testSourceCheckUnknownComparisonRemainsUnknown() throws {
        var value = try fixture("source-check-not-recognised")
        value["status"] = "unavailable"
        value["sourceSchemaRecognised"] = NSNull()
        let report = try XCTUnwrap(HubSourceCheckResult.parse(output(value), historyOnly: true))
        XCTAssertNil(report.schemaRecognised)
        XCTAssertNil(report.recognitionKey)
        XCTAssertFalse(report.mayImport)
    }
    /// Review REV-M03-T08 finding 3: only a report for the current reviewed
    /// set (release 4.4.0) enables import; a report naming the earlier set
    /// does not, and a refusal of an earlier-set source keeps the producer's
    /// guidance to update the source application to 4.4.0 first.
    func testOnlyTheCurrentReviewedSetEnablesImport() throws {
        let current = try fixture("source-check-recognised")
        XCTAssertEqual(current["reasonCode"] as? String, HubSourceCheckResult.compatibleReasonCode)
        XCTAssertEqual(current["requiredVersion"] as? String, HubSourceCheckResult.requiredSourceRelease)
        XCTAssertEqual(HubSourceCheckResult.requiredSourceRelease, "4.4.0")
        XCTAssertTrue(try XCTUnwrap(HubSourceCheckResult.parse(output(current), historyOnly: true)).mayImport)

        var earlier = current
        earlier["reasonCode"] = "v4_2_compatible_schema"
        earlier["requiredVersion"] = "4.2.0"
        XCTAssertFalse(try XCTUnwrap(HubSourceCheckResult.parse(output(earlier), historyOnly: true)).mayImport)
        var staleRelease = current
        staleRelease["requiredVersion"] = "4.2.0"
        XCTAssertFalse(try XCTUnwrap(HubSourceCheckResult.parse(output(staleRelease), historyOnly: true)).mayImport)

        var refused = try fixture("source-check-not-recognised")
        let guidance = "This source database has the earlier reviewed schema. Back up the source application, update it to release 4.4.0 or newer, start it once and let its migrations finish, then retry."
        refused["reasonCode"] = "older_than_v4_4_compatible_schema"
        refused["observedMigrationVersion"] = 20260808090000
        refused["guidance"] = guidance
        let report = try XCTUnwrap(HubSourceCheckResult.fromCommandResult(
            .failure(HubActionError.commandExited(1, try output(refused))), historyOnly: true))
        XCTAssertFalse(report.mayImport)
        XCTAssertEqual(report.schemaRecognised, false)
        XCTAssertEqual(report.message, guidance)
        XCTAssertEqual(report.requiredVersion, "4.4.0")
    }

    func testRecognisedReportShowsTheSchemaSetLabel() throws {
        let value = try fixture("source-check-recognised")
        let report = try XCTUnwrap(HubSourceCheckResult.parse(output(value), historyOnly: true))
        XCTAssertTrue(report.recognitionTitle.contains("c497f904ef74"))
        XCTAssertFalse(report.recognitionTitle.contains("%1$@"))
    }

}
