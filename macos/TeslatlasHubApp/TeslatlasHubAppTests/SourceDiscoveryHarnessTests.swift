// SPDX-License-Identifier: AGPL-3.0-only

import Foundation
import XCTest
@testable import Teslatlas_Hub

final class SourceDiscoveryHarnessTests: XCTestCase {
    private let full = ["DATABASE_USER": "chosen-user", "DATABASE_PASS": "chosen-password",
                        "DATABASE_NAME": "sourcedb", "ENCRYPTION_KEY": "chosen-key"]
    private func scenario(version: String? = "4.2.0", uid: Int = 501,
                          candidateCount: Int = 1, databases: [String] = ["database-a"]) -> [String: Any] {
        var containers: [String: Any] = [
            "database-a": ["project": "arbitrary-project", "env": ["POSTGRES_PASSWORD": "unselected-db-secret"]],
            "other-first": ["project": "arbitrary-project", "env": ["DATABASE_USER": "unselected-user-secret",
                "DATABASE_PASS": "unselected-password-secret", "DATABASE_NAME": "unselected-name-secret"]]
        ]
        for index in 0..<candidateCount {
            var candidate: [String: Any] = ["project": "arbitrary-project", "env": full, "image": "arbitrary/image:7"]
            if let version { candidate["version"] = version }
            containers[index == 0 ? "chosen-last" : "candidate-\(index)"] = candidate
        }
        for id in databases where id != "database-a" {
            containers[id] = ["project": "arbitrary-project", "env": ["POSTGRES_PASSWORD": "unselected-other-db-secret"]]
        }
        return ["uid": uid, "databases": databases, "containers": containers]
    }
    private func checked(_ scenario: [String: Any], sudo: Bool = false) throws -> SourceDiscoveryHarness.Observation {
        let result = try SourceDiscoveryHarness.run(scenario: scenario, useSudo: sudo)
        XCTAssertLessThanOrEqual(result.output.utf8.count, 64 * 1024)
        for value in ["unselected-db-secret", "unselected-user-secret", "unselected-password-secret", "unselected-name-secret", "unselected-other-db-secret"] {
            XCTAssertFalse(result.output.contains(value))
            XCTAssertFalse(result.output.contains(Data(value.utf8).base64EncodedString()))
        }
        for call in result.calls {
            XCTAssertFalse(call.contains("{{.Config.Image}}"))
            XCTAssertFalse(call.contains { $0.contains("com.docker.compose.service") && $0 != "label=com.docker.compose.service=database" })
            if call.contains("{{range .Config.Env}}{{println .}}{{end}}") {
                XCTAssertEqual(call.last, "chosen-last")
            }
        }
        return result
    }
    func testDiscoveryFindsOneCandidateByEnvironmentShape() throws {
        let result = try checked(scenario())
        XCTAssertEqual(result.status, 0)
        XCTAssertEqual(result.values?["password"], "chosen-password")
        XCTAssertEqual(result.values?["car"], "42")
        XCTAssertEqual(result.classification, .supported("4.2.0"))
        XCTAssertTrue(result.versionAdmitted)
    }
    func testDiscoveryReportsNoCandidate() throws {
        XCTAssertEqual(try checked(scenario(candidateCount: 0)).status, 20)
    }
    func testDiscoveryRefusesSeveralCandidates() throws {
        XCTAssertEqual(try checked(scenario(candidateCount: 2)).status, 27)
    }
    func testDiscoveryReadsOnlyChosenContainerValues() throws {
        let result = try checked(scenario())
        XCTAssertEqual(result.values?["user"], "chosen-user")
        XCTAssertTrue(result.calls.contains { $0.contains("{{range .Config.Env}}{{index (split . \"=\") 0}}{{println}}{{end}}") && $0.last == "other-first" })
    }
    func testDiscoveryDeduplicatesProjectsAndKeepsDatabaseMultiplicity() throws {
        let result = try checked(scenario(databases: ["database-a", "database-b"]))
        XCTAssertEqual(result.status, 28)
        XCTAssertEqual(result.calls.filter { $0.contains("ps") && $0.contains("label=com.docker.compose.project=arbitrary-project") && !$0.contains("label=com.docker.compose.service=database") }.count, 1)
    }
    func testDiscoveryUsesNonInteractiveSudoAndRootDirectly() throws {
        let user = try checked(scenario(), sudo: true)
        XCTAssertTrue(user.calls.contains { $0.first == "sudo" && $0.dropFirst().first == "-n" })
        let root = try checked(scenario(uid: 0), sudo: true)
        XCTAssertEqual(root.status, 0)
        XCTAssertFalse(root.calls.contains { $0.first == "sudo" })
    }
    func testOldIdentityWithoutShapeCannotBecomeCandidate() throws {
        var value = scenario(candidateCount: 0)
        var containers = value["containers"] as! [String: Any]
        let oldName = ["tesl", "amate"].joined()
        containers["old-identity"] = ["project": "arbitrary-project", "env": ["DATABASE_USER": "unselected-user-secret"], "image": oldName + "/" + oldName + ":4.2.0"]
        value["containers"] = containers
        XCTAssertEqual(try checked(value).status, 20)
    }
    func testTooOldOCITravelsThroughParserAndProductionAdmission() throws {
        let result = try checked(scenario(version: "4.1.1"))
        XCTAssertEqual(result.status, 0)
        XCTAssertEqual(result.classification, .tooOld("4.1.1"))
        XCTAssertFalse(result.versionAdmitted)
    }
    func testMissingOCIRequiresAcknowledgement() throws {
        let result = try checked(scenario(version: nil))
        XCTAssertEqual(result.classification, .unknown)
        XCTAssertTrue(result.acknowledgementRequired)
    }
    func testMalformedDiscoveryUsesLocalisedCommandFailure() {
        XCTAssertEqual(
            SourceServerImporter.discoveryFailureReason(NSError(domain: "discovery", code: 1)),
            "process_start_failed"
        )
        XCTAssertThrowsError(try SourceDiscovery.parse("user=!!!!")) { error in
            guard case let HubActionError.commandFailed(message) = error else { return XCTFail("Expected commandFailed") }
            XCTAssertEqual(message, HubL10n.text("hub.SourceServerImporter.428.2461", fallback: "Source server returned incomplete setup details."))
            XCTAssertEqual(SourceServerImporter.discoveryFailureReason(error), "process_failed")
        }
    }

}
