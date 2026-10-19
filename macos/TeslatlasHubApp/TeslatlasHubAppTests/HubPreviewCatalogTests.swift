// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

final class HubPreviewCatalogTests: XCTestCase {
    func testCatalogContainsEveryPhotographedStateExactlyOnce() {
        let expected = [
            "r01-welcome", "r02-choose", "r03-migration",
            "r04-migration-connected", "r05-verify", "r06-finish-migration",
            "r07-dashboard", "r08-vehicles", "r09-diagnostics", "r10-logs",
            "r11-service-details", "r12-manage-menu", "r13-provider", "r14-fleet",
            "r15-legacy", "r16-activity", "r17-settings", "r18-finish", "r19-importing"
        ]
        let actual = HubPreviewScene.allCases.map(\.rawValue)

        XCTAssertEqual(actual, expected)
        XCTAssertEqual(actual.count, expected.count)
        XCTAssertEqual(Set(actual).count, expected.count)
        for state in expected {
            XCTAssertEqual(actual.filter { $0 == state }.count, 1, state)
        }
    }

    func testEverySceneUsesAnInertFixtureAndEntersWithoutOperationalCalls() {
        for scene in HubPreviewScene.allCases {
            let operations = HubPreviewOperationRecorder()
            let controller = HubController(environment: [
                "TESLATLAS_HUB_PREVIEW_SCENE": scene.rawValue
            ],
            commandRunner: operations,
            serviceRunner: operations)

            XCTAssertTrue(controller.previewMode, scene.rawValue)
            XCTAssertEqual(controller.previewScene, scene, scene.rawValue)
            XCTAssertEqual(controller.onboardingPreviewRoute, scene.onboardingRoute, scene.rawValue)
            assertInertFixture(controller.snapshot, for: scene)

            var refreshedSnapshot: HubSnapshot?
            controller.refresh { refreshedSnapshot = $0 }
            XCTAssertEqual(refreshedSnapshot?.health, controller.snapshot.health, scene.rawValue)
            XCTAssertTrue(operations.calls.isEmpty,
                          "\(scene.rawValue) invoked \(operations.calls) while entering its preview fixture")
        }
    }

    func testLegacyOnboardingPreviewVariableIsIgnoredWithoutPreviewGate() {
        let controller = HubController(environment: [
            "TESLATLAS_HUB_ONBOARDING_PREVIEW": "welcome"
        ])
        XCTAssertFalse(controller.previewMode)
        XCTAssertNil(controller.previewScene)
        XCTAssertNil(controller.onboardingPreviewRoute)
    }

    func testPreviewControllerRejectsOperationalServiceMutation() {
        let removal = expectation(description: "preview rejected removal")
        let deletion = expectation(description: "preview rejected deletion")
        let controller = HubController(environment: [
            "TESLATLAS_HUB_PREVIEW_SCENE": HubPreviewScene.dashboard.rawValue
        ])

        controller.removeBackgroundService { result in
            guard case let .failure(error) = result else {
                XCTFail("preview unexpectedly allowed removal")
                removal.fulfill()
                return
            }
            XCTAssertEqual(error.localizedDescription,
                           HubActionError.preview.localizedDescription)
            removal.fulfill()
        }
        controller.deleteHubAndData { result in
            guard case let .failure(error) = result else {
                XCTFail("preview unexpectedly allowed deletion")
                deletion.fulfill()
                return
            }
            XCTAssertEqual(error.localizedDescription,
                           HubActionError.preview.localizedDescription)
            deletion.fulfill()
        }
        wait(for: [removal, deletion], timeout: 1)
    }

    func testPreviewRecorderRefusesRecoveryAndDelete() {
        let recorder = HubPreviewOperationRecorder()
        var recovery: Result<String, Error>?
        var deletion: Result<HubDataRemovalReport, Error>?
        let owner = makeLifecycleToken()
        recorder.recoverAfterPause(owner: owner) { recovery = $0 }
        recorder.releaseRestartRecord(owner: owner)
        recorder.deleteHubAndData(owner: owner) { deletion = $0 }
        guard case let .failure(recoveryError)? = recovery,
              case let .failure(deletionError)? = deletion else {
            XCTFail("the preview recorder must refuse recovery and deletion")
            return
        }
        XCTAssertEqual(recoveryError.localizedDescription, HubActionError.preview.localizedDescription)
        XCTAssertEqual(deletionError.localizedDescription, HubActionError.preview.localizedDescription)
        XCTAssertEqual(recorder.calls, [.recoverAfterPause, .releaseRestartRecord, .deleteHubAndData])
        XCTAssertFalse(recorder.restartDutyOutstanding)
        var settled = false
        recorder.whenRestartDutySettled { settled = true }
        XCTAssertTrue(settled, "the preview settles immediately")
    }

    private func assertInertFixture(_ snapshot: HubSnapshot,
                                    for scene: HubPreviewScene,
                                    file: StaticString = #filePath,
                                    line: UInt = #line) {
        if scene.isOnboarding {
            XCTAssertEqual(snapshot.health, .needsInstall, scene.rawValue, file: file, line: line)
            XCTAssertEqual(snapshot.service, "Not installed", scene.rawValue, file: file, line: line)
            XCTAssertEqual(snapshot.account, "Not configured", scene.rawValue, file: file, line: line)
            XCTAssertNil(snapshot.controlVehicleID, scene.rawValue, file: file, line: line)
            XCTAssertEqual(snapshot.controlVehicles, [], scene.rawValue, file: file, line: line)
            XCTAssertEqual(snapshot.diagnosticLines,
                           ["Hub has not been configured or installed."],
                           scene.rawValue,
                           file: file,
                           line: line)
        } else {
            XCTAssertEqual(snapshot.health, .running, scene.rawValue, file: file, line: line)
            XCTAssertEqual(snapshot.service, "Active", scene.rawValue, file: file, line: line)
            XCTAssertEqual(snapshot.account, "Connected", scene.rawValue, file: file, line: line)
            XCTAssertEqual(snapshot.controlVehicleID,
                           UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5"),
                           scene.rawValue,
                           file: file,
                           line: line)
            XCTAssertEqual(snapshot.controlVehicles.map(\.displayName),
                           ["Aurora", "Comet"],
                           scene.rawValue,
                           file: file,
                           line: line)
            XCTAssertEqual(snapshot.diagnosticLines.first,
                           "Preview mode: no process or launchctl mutation",
                           scene.rawValue,
                           file: file,
                           line: line)
        }
    }
}

final class HubPreviewOperationRecorder: HubServiceControlling, HubCommandRunning {
    enum Call: Equatable {
        case command
        case serviceLoadState
        case recoverAfterPause
        case releaseRestartRecord
        case deleteHubAndData
    }

    private(set) var calls: [Call] = []

    /// Preview has no outstanding duty and settles at once.
    var restartDutyOutstanding: Bool { false }

    func whenRestartDutySettled(_ callback: @escaping () -> Void) { callback() }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        calls.append(.command)
        completion(.failure(HubActionError.preview))
    }
    func run(arguments: [String], owner: HubLifecycleToken,
             completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, completion: completion)
    }
    func restartRecordHeld(by owner: HubLifecycleToken) -> Bool { false }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        calls.append(.serviceLoadState)
        completion(.unloaded)
    }

    func recoverAfterPause(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) {
        calls.append(.recoverAfterPause)
        completion(.failure(HubActionError.preview))
    }

    func releaseRestartRecord(owner: HubLifecycleToken) {
        calls.append(.releaseRestartRecord)
    }


    func reconcile(owner: HubLifecycleToken, ensure: @escaping () throws -> HubLaunchAgentChange,
                   completion: @escaping (HubReconcileOutcome) -> Void) { completion(.completed) }
    func deleteHubAndData(owner: HubLifecycleToken, completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        calls.append(.deleteHubAndData)
        completion(.failure(HubActionError.preview))
    }
}

final class HubPreviewLogRecorder: HubAppLogging {
    enum Call: Equatable {
        case record
        case recentText
    }

    private(set) var calls: [Call] = []

    func record(_ name: String,
                category: String,
                level: String = "INFO",
                fields: [String: String] = [:]) {
        calls.append(.record)
    }

    func recentText(maximumBytes: Int = 256 * 1024) -> String {
        calls.append(.recentText)
        return ""
    }
}

final class HubPreviewAlertRecorder: HubAlertPresenting {
    private(set) var errorPresentations = 0
    private(set) var informationPresentations = 0
    private(set) var responseRequests = 0

    var totalInvocations: Int {
        errorPresentations + informationPresentations + responseRequests
    }

    func present(error: Error) {
        errorPresentations += 1
    }

    func present(information alert: NSAlert) {
        informationPresentations += 1
    }

    func response(to alert: NSAlert,
                  silentResponse: NSApplication.ModalResponse) -> NSApplication.ModalResponse {
        responseRequests += 1
        return silentResponse
    }
}

final class HubPreviewPanelRecorder {
    private(set) var savePanelRequests = 0

    func present(_ panel: NSSavePanel,
                 for window: NSWindow,
                 completion: @escaping (NSApplication.ModalResponse) -> Void) {
        savePanelRequests += 1
    }
}
