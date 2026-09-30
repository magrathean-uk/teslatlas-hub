// SPDX-License-Identifier: AGPL-3.0-only
import Darwin
import XCTest
@testable import Teslatlas_Hub

final class DevelopmentEventLogTests: XCTestCase {
    func testDirectoryWalkStaysAnchoredDuringAncestorSwap() throws {
        let root = try canonicalTemporaryDirectory().appendingPathComponent(UUID().uuidString, isDirectory: true)
        let ancestor = root.appendingPathComponent("ancestor", isDirectory: true)
        let held = root.appendingPathComponent("held", isDirectory: true)
        let redirected = root.appendingPathComponent("redirected", isDirectory: true)
        let logs = ancestor.appendingPathComponent("logs", isDirectory: true)
        for path in [logs, redirected.appendingPathComponent("logs", isDirectory: true)] {
            try FileManager.default.createDirectory(at: path, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        }
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.setAttributes([.posixPermissions: 0o775], ofItemAtPath: ancestor.path)
        var swapError: Error?
        let descriptor = try DevelopmentEventLog.validateDirectoryReadOnly(logs, afterOpeningComponent: { opened in
            if opened == ancestor.path {
                do {
                    try FileManager.default.moveItem(at: ancestor, to: held)
                    try FileManager.default.createSymbolicLink(at: ancestor, withDestinationURL: redirected)
                } catch { swapError = error }
            }
        })
        defer { close(descriptor) }
        XCTAssertNil(swapError)
        let marker = openat(descriptor, "safe-marker", O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, mode_t(0o600))
        XCTAssertGreaterThanOrEqual(marker, 0)
        if marker >= 0 { close(marker) }
        XCTAssertTrue(FileManager.default.fileExists(atPath: held.appendingPathComponent("logs/safe-marker").path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: redirected.appendingPathComponent("logs/safe-marker").path))
        XCTAssertThrowsError(try DevelopmentEventLog.validateDirectoryReadOnly(logs))
    }
    func testDirectoryWalkRejectsUnopenedChildSwap() throws {
        let root = try canonicalTemporaryDirectory().appendingPathComponent(UUID().uuidString, isDirectory: true)
        let logs = root.appendingPathComponent("logs", isDirectory: true)
        let held = root.appendingPathComponent("held", isDirectory: true)
        try FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: root) }
        var swapError: Error?
        XCTAssertThrowsError(try DevelopmentEventLog.validateDirectoryReadOnly(logs, afterOpeningComponent: { opened in
            if opened == root.path {
                do {
                    try FileManager.default.moveItem(at: logs, to: held)
                    try FileManager.default.createSymbolicLink(at: logs, withDestinationURL: held)
                } catch { swapError = error }
            }
        }))
        XCTAssertNil(swapError)
    }
    private func canonicalTemporaryDirectory() throws -> URL {
        guard let lab = ProcessInfo.processInfo.environment["TESLATLAS_LAB"] else { throw XCTSkip("external lab is required for the local source-run journal gate") }
        let temporary = URL(fileURLWithPath: lab, isDirectory: true).appendingPathComponent("tmp", isDirectory: true)
        guard let path = realpath(temporary.path, nil) else { throw POSIXError(.EIO) }
        defer { free(path) }
        return URL(fileURLWithPath: String(cString: path), isDirectory: true)
    }
    func testJournalPrivacyOutcomesAndRotation() throws {
        let directory = try canonicalTemporaryDirectory().appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: directory) }
        let journal = DevelopmentEventLog(directory: directory, limit: 1100)
        for _ in 0..<20 {
            journal.completion(kind: .command, action: .migrate, operation: UUID(), started: DispatchTime.now().uptimeNanoseconds,
                               result: .failure(HubActionError.commandExited(7, "token=SECRET location=SECRET source=SECRET")))
        }
        journal.completion(kind: .control, action: .start, operation: UUID(), started: DispatchTime.now().uptimeNanoseconds,
                           result: .failure(HubActionError.commandTimedOut))
        XCTAssertTrue(journal.drain(timeout: 5))
        let files = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil).filter { $0.pathExtension == "jsonl" }
        XCTAssertFalse(files.isEmpty); XCTAssertLessThanOrEqual(files.count, 5)
        var outcomes = Set<String>()
        for file in files {
            XCTAssertEqual((try FileManager.default.attributesOfItem(atPath: file.path)[.posixPermissions] as? NSNumber)?.intValue, 0o600)
            let content = try String(contentsOf: file, encoding: .utf8)
            XCTAssertFalse(content.contains("SECRET")); XCTAssertFalse(content.contains("location")); XCTAssertFalse(content.contains("source"))
            for line in content.split(separator: "\n") {
                let frame = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any])
                outcomes.insert(try XCTUnwrap(frame["outcome"] as? String))
                XCTAssertNil(frame["error"]); XCTAssertNil(frame["arguments"])
            }
        }
        XCTAssertTrue(outcomes.contains("failed")); XCTAssertTrue(outcomes.contains("timeout"))
    }
    func testJournalRejectsSymlinkDestination() throws {
        let root = try canonicalTemporaryDirectory().appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: root) }
        let target = root.appendingPathComponent("preserved.txt")
        try Data("preserved".utf8).write(to: target)
        try FileManager.default.createSymbolicLink(at: root.appendingPathComponent("appkit-events.0.jsonl"), withDestinationURL: target)
        let journal = DevelopmentEventLog(directory: root)
        journal.record(kind: .control, action: .stop, outcome: .complete, operation: UUID())
        XCTAssertTrue(journal.drain(timeout: 5))
        XCTAssertEqual(try String(contentsOf: target, encoding: .utf8), "preserved")
    }
    func testJournalRejectsSymlinkedAncestorBeforeWriting() throws {
        let root = try canonicalTemporaryDirectory().appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: root) }
        let real = root.appendingPathComponent("real", isDirectory: true)
        try FileManager.default.createDirectory(at: real, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let alias = root.appendingPathComponent("alias", isDirectory: true)
        try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: real)
        let journal = DevelopmentEventLog(directory: alias)
        journal.record(kind: .command, action: .status, outcome: .start, operation: UUID())
        XCTAssertTrue(journal.drain(timeout: 5))
        XCTAssertTrue(try FileManager.default.contentsOfDirectory(atPath: real.path).isEmpty)
    }

    func testExistingOwnerDirectoryPolicyReadOnly() throws {
        let path = try XCTUnwrap(ProcessInfo.processInfo.environment["TESLATLAS_HUB_LOG_POLICY_DIRECTORY"])
        let descriptor = try DevelopmentEventLog.validateDirectoryReadOnly(URL(fileURLWithPath: path))
        XCTAssertGreaterThanOrEqual(descriptor, 0)
        close(descriptor)
    }

    func testJournalRejectsWorldWritableAncestor() throws {
        let root = try canonicalTemporaryDirectory().appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: root) }
        let leaf = root.appendingPathComponent("logs", isDirectory: true)
        try FileManager.default.createDirectory(at: leaf, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try FileManager.default.setAttributes([.posixPermissions: 0o777], ofItemAtPath: root.path)
        XCTAssertThrowsError(try DevelopmentEventLog.validateDirectoryReadOnly(leaf))
    }

}
