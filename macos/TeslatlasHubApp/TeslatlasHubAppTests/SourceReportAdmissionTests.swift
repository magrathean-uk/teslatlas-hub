// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

private func sourceReportFixture(_ name: String) throws -> String {
    let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        .appendingPathComponent("Fixtures/" + name + ".json")
    return try String(contentsOf: url, encoding: .utf8)
}

private final class SourceReportRunner: HubCommandRunning {
    let result: Result<String, Error>
    private(set) var calls: [[String]] = []
    init(_ result: Result<String, Error>) { self.result = result }
    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        calls.append(arguments)
        completion(result)
    }
}

extension HubControllerTests {
    func testSourceCheckFailureCallbackPreservesHistoryOnlyRefusal() throws {
        let fixture = try sourceReportFixture("source-check-not-recognised")
        let runner = SourceReportRunner(.failure(HubActionError.commandExited(1, fixture)))
        let home = HubTestScratch.fixtureRoot("source-report")
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: runner, homeDirectory: home,
                                       serviceInstalledOverride: false)
        let done = expectation(description: "refusal remains a report")
        controller.checkSourceCompatibility(source: "postgresql://reader@127.0.0.1/sourcedb", carID: "42",
            passwordFile: home.appendingPathComponent("password").path, acceptSchemaEvidence: true,
            historyOnly: true) { result in
                guard case let .success(report) = result else { return XCTFail("Refusal was discarded") }
                XCTAssertEqual(report.schemaRecognised, false)
                XCTAssertNil(report.schemaSetLabel)
                XCTAssertFalse(report.mayImport)
                XCTAssertEqual(report.recognitionKey, "hub.import.notRecognised")
                XCTAssertTrue(fixture.contains(report.message))
                done.fulfill()
            }
        wait(for: [done], timeout: 2)
        XCTAssertEqual(runner.calls.count, 1)
        XCTAssertTrue(runner.calls[0].contains("source-check"))
        XCTAssertTrue(runner.calls[0].contains("--accept-schema-evidence"))
        XCTAssertFalse(runner.calls[0].contains("migrate"))
    }

    func testFailedSourceCheckCannotAdmitSuccessShapedReport() throws {
        let fixture = try sourceReportFixture("source-check-recognised")
        let runner = SourceReportRunner(.failure(HubActionError.commandExited(1, fixture)))
        let home = HubTestScratch.fixtureRoot("failed-source-report")
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: runner, homeDirectory: home, serviceInstalledOverride: false)
        let done = expectation(description: "failed process never admits")
        controller.checkSourceCompatibility(source: "postgresql://reader@127.0.0.1/sourcedb", carID: "42",
            passwordFile: "/fictional/password", acceptSchemaEvidence: true, historyOnly: true) { result in
                guard case let .success(report) = result else { return XCTFail("Diagnostic report was discarded") }
                XCTAssertFalse(report.compatible)
                XCTAssertFalse(report.mayImport)
                done.fulfill()
            }
        wait(for: [done], timeout: 2)
    }
}

extension OnboardingWindowControllerTests {
    func testLocalHistoryFailureReportShowsNotRecognisedAndBlocksImport() throws {
        let fixture = try sourceReportFixture("source-check-not-recognised")
        let runner = SourceReportRunner(.failure(HubActionError.commandExited(1, fixture)))
        let home = HubTestScratch.fixtureRoot("source-report")
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = HubController(commandRunner: runner, homeDirectory: home, serviceInstalledOverride: false)
        let onboarding = OnboardingWindowController(controller: controller, previewRoute: "migration", onComplete: { _ in })
        onboarding.configureLocalHistoryForTests(source: "postgresql://reader@127.0.0.1/sourcedb", carID: "42",
            passwordFile: home.appendingPathComponent("password").path)
        func localFiles() -> [String: Data] {
            let urls = FileManager.default.enumerator(at: home, includingPropertiesForKeys: [.isRegularFileKey])
            var result: [String: Data] = [:]
            while let url = urls?.nextObject() as? URL {
                if let bytes = try? Data(contentsOf: url) { result[url.path] = bytes }
            }
            return result
        }
        let before = localFiles()
        let done = expectation(description: "actual callback rendered")
        onboarding.checkLocalHistoryForTests()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
            XCTAssertEqual(onboarding.localHistoryReportForTests?.schemaRecognised, false)
            XCTAssertFalse(onboarding.localHistoryReportForTests?.mayImport ?? true)
            func descendants(_ view: NSView) -> [NSView] { [view] + view.subviews.flatMap(descendants) }
            let views = onboarding.window?.contentView.map(descendants) ?? []
            XCTAssertTrue(views.contains { $0.identifier?.rawValue == "hub.import.notRecognised" })
            let texts = views.compactMap { ($0 as? NSTextField)?.stringValue }
            XCTAssertTrue(texts.contains(onboarding.localHistoryReportForTests?.message ?? "missing"))
            XCTAssertTrue(views.contains { ($0 as? NSButton)?.title == "Check Snapshot" && ($0 as? NSButton)?.isEnabled == true })
            onboarding.importLocalHistoryForTests()
            XCTAssertEqual(localFiles(), before)
            XCTAssertFalse(localFiles().keys.contains { $0.contains("handover") })
            onboarding.showSourceConnectionFormForTests()
            let otherViews = onboarding.window?.contentView.map(descendants) ?? []
            XCTAssertTrue(otherViews.contains { $0.identifier?.rawValue == "hub.import.notRecognised" })
            XCTAssertTrue(otherViews.compactMap { ($0 as? NSTextField)?.stringValue }.contains(onboarding.localHistoryReportForTests?.message ?? "missing"))
            XCTAssertEqual(runner.calls.count, 1)
            XCTAssertFalse(runner.calls.flatMap { $0 }.contains("migrate"))
            done.fulfill()
        }
        wait(for: [done], timeout: 2)
    }
}
