// SPDX-License-Identifier: AGPL-3.0-only

import Foundation
import XCTest
@testable import Teslatlas_Hub

final class SourceDiscoveryHarnessTests: XCTestCase {
    private let full = ["DATABASE_USER": "chosen-user", "DATABASE_PASS": "chosen-password",
                        "DATABASE_NAME": "sourcedb", "ENCRYPTION_KEY": "chosen-key"]
    private func scenario(version: String? = "4.4.0", uid: Int = 501,
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
        let result = try AllowListedDiscoveryHarness.run(scenario: scenario, useSudo: sudo)
        XCTAssertLessThanOrEqual(result.output.utf8.count, 64 * 1024)
        for value in ["unselected-db-secret", "unselected-user-secret", "unselected-password-secret", "unselected-name-secret", "unselected-other-db-secret", "chosen-key"] {
            XCTAssertFalse(result.output.contains(value))
            XCTAssertFalse(result.output.contains(Data(value.utf8).base64EncodedString()))
        }
        // The source key is never selected, carried or returned.
        XCTAssertNil(result.values?["key"])
        for call in result.calls {
            XCTAssertFalse(call.contains("{{.Config.Image}}"))
            XCTAssertFalse(call.contains("{{range .Config.Env}}{{println .}}{{end}}"), "the whole environment is never read")
            XCTAssertFalse(call.contains { $0.contains("com.docker.compose.service") && $0 != "label=com.docker.compose.service=database" })
            if call.contains(SourceDiscovery.allowListedValuesTemplate) {
                XCTAssertEqual(call.last, "chosen-last")
            }
        }
        return result
    }

    func testDiscoverySelectsOnlyAllowListedDatabaseFields() throws {
        XCTAssertEqual(SourceDiscovery.allowListedNames, ["DATABASE_USER", "DATABASE_PASS", "DATABASE_NAME"])
        XCTAssertEqual(SourceDiscovery.allowListedValuesTemplate,
                       #"{{range .Config.Env}}{{if eq (index (split . "=") 0) "DATABASE_USER" "DATABASE_PASS" "DATABASE_NAME"}}{{println .}}{{end}}{{end}}"#)
        XCTAssertFalse(SourceDiscovery.script.contains("{{range .Config.Env}}{{println .}}{{end}}"))
        XCTAssertFalse(SourceDiscovery.script.contains("encryption_key"))
        XCTAssertFalse(SourceDiscovery.script.contains("printf 'key="))
        let result = try checked(scenario())
        XCTAssertEqual(result.status, 0)
        XCTAssertEqual(Set(result.values?.keys.map { $0 } ?? []),
                       ["user", "password", "database", "car", "address", "oci_version"])
        XCTAssertEqual(result.calls.filter { $0.contains(SourceDiscovery.allowListedValuesTemplate) }.count, 1)
        // An answer that still carried a key line is parsed with that line dropped.
        let values = try SourceDiscovery.parse(["user", "password", "database", "car", "address", "key"]
            .map { "\($0)=\(Data("x".utf8).base64EncodedString())" }.joined(separator: "\n"))
        XCTAssertNil(values["key"])
        XCTAssertEqual(Set(values.keys), ["user", "password", "database", "car", "address"])
    }

    func testDiscoveryFindsOneCandidateByEnvironmentShape() throws {
        let result = try checked(scenario())
        XCTAssertEqual(result.status, 0)
        XCTAssertEqual(result.values?["password"], "chosen-password")
        XCTAssertEqual(result.values?["car"], "42")
        XCTAssertEqual(result.classification, .supported("4.4.0"))
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
    func testEarlierReviewedSetReleaseIsRefusedBeforeImport() throws {
        let result = try checked(scenario(version: "4.2.0"))
        XCTAssertEqual(result.status, 0)
        XCTAssertEqual(result.classification, .tooOld("4.2.0"))
        XCTAssertFalse(result.versionAdmitted)
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

/// The production discovery script against a fake Docker that answers only the allow-listed
/// value template: a script that asked for the whole environment would be refused (exit 99).
private enum AllowListedDiscoveryHarness {
    static let wholeEnvironmentAnswer = #" if template=='{{range .Config.Env}}{{println .}}{{end}}':out('\n'.join(k+'='+v for k,v in c['env'].items()))"#

    static var fakeCommand: String {
        let allowed = SourceDiscovery.allowListedNames.map { "'\($0)'" }.joined(separator: ",")
        let answer = #" if template==\#(pythonLiteral(SourceDiscovery.allowListedValuesTemplate)):out('\n'.join(k+'='+v for k,v in c['env'].items() if k in (\#(allowed))))"#
        precondition(SourceDiscoveryHarness.fakeCommand.contains(wholeEnvironmentAnswer))
        return SourceDiscoveryHarness.fakeCommand.replacingOccurrences(of: wholeEnvironmentAnswer, with: answer)
    }

    private static func pythonLiteral(_ text: String) -> String {
        "'" + text.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "'", with: "\\'") + "'"
    }

    static func run(scenario: [String: Any], useSudo: Bool = false) throws -> SourceDiscoveryHarness.Observation {
        let root = HubTestScratch.fixtureRoot("source-discovery-allow-list")
        let bin = root.appendingPathComponent("bin")
        try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700])
        var retainUntilQuiescent = false
        defer { if !retainUntilQuiescent { try? FileManager.default.removeItem(at: root) } }
        let temporary = root.appendingPathComponent("tmp")
        try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700])
        let scene = root.appendingPathComponent("scenario.json")
        let calls = root.appendingPathComponent("calls.jsonl")
        try JSONSerialization.data(withJSONObject: scenario).write(to: scene)
        try Data().write(to: calls)
        for name in ["docker", "sudo", "id"] {
            let command = bin.appendingPathComponent(name)
            try Data(fakeCommand.utf8).write(to: command)
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: command.path)
        }
        let result = SourceDiscoveryHarness.execute(environment: [
            "PATH": bin.path + ":" + (ProcessInfo.processInfo.environment["PATH"] ?? "/usr/bin:/bin"),
            "SCENARIO": scene.path, "CALLS": calls.path, "TMPDIR": temporary.path,
            "TESLATLAS_USE_SUDO": useSudo ? "1" : "0"
        ])
        let status: Int32, output: String
        switch result {
        case let .success(text): status = 0; output = text
        case let .failure(HubActionError.commandExited(code, text)): status = code; output = text
        case let .failure(error):
            if case let HubActionError.commandNotQuiescent(_, pending) = error, !pending.isQuiescent {
                retainUntilQuiescent = true
                pending.whenQuiescent { try? FileManager.default.removeItem(at: root) }
            }
            throw error
        }
        let log = try String(contentsOf: calls, encoding: .utf8).split(whereSeparator: \.isNewline).map {
            try JSONSerialization.jsonObject(with: Data($0.utf8)) as! [String]
        }
        let values = try? SourceServerImporter.parseForTests(output)
        let classification = values.map { SourceServerImporter.classifyVersionForTests(ociVersion: $0["oci_version"]) }
        let admitted: Bool, acknowledgement: Bool
        if let values {
            do { acknowledgement = try SourceDiscovery.admittedVersion(ociVersion: values["oci_version"]) == nil; admitted = true }
            catch { acknowledgement = false; admitted = false }
        } else { admitted = false; acknowledgement = false }
        return SourceDiscoveryHarness.Observation(status: status, output: output, calls: log, values: values,
            classification: classification, versionAdmitted: admitted, acknowledgementRequired: acknowledgement)
    }
}
