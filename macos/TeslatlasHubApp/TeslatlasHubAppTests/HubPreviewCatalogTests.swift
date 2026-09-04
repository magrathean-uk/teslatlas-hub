// SPDX-License-Identifier: AGPL-3.0-only

import XCTest
@testable import Teslatlas_Hub

final class HubPreviewCatalogTests: XCTestCase {
    func testCatalogContainsEveryPhotographedStateExactlyOnce() {
        XCTAssertEqual(HubPreviewScene.allCases.map(\.rawValue), [
            "r01-welcome", "r02-choose", "r03-migration",
            "r04-migration-connected", "r05-verify", "r06-finish-migration",
            "r07-dashboard", "r08-vehicles", "r09-diagnostics", "r10-logs",
            "r11-service-details", "r12-manage-menu"
        ])
    }

    func testSceneEnvironmentEnablesReadOnlyPreviewAndSelectsCorrectSnapshotFamily() {
        for scene in HubPreviewScene.allCases {
            let controller = HubController(environment: [
                "TESLATLAS_HUB_PREVIEW_SCENE": scene.rawValue
            ])
            XCTAssertTrue(controller.previewMode, scene.rawValue)
            XCTAssertEqual(controller.previewScene, scene, scene.rawValue)
            XCTAssertEqual(controller.onboardingPreviewRoute, scene.onboardingRoute, scene.rawValue)
            XCTAssertEqual(controller.snapshot.health,
                           scene.isOnboarding ? .needsInstall : .running,
                           scene.rawValue)
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
        let expectation = expectation(description: "preview rejected mutation")
        let controller = HubController(environment: [
            "TESLATLAS_HUB_PREVIEW_SCENE": HubPreviewScene.dashboard.rawValue
        ])

        controller.installService { result in
            guard case let .failure(error) = result else {
                XCTFail("preview unexpectedly allowed installation")
                expectation.fulfill()
                return
            }
            XCTAssertEqual(error.localizedDescription,
                           HubActionError.preview.localizedDescription)
            expectation.fulfill()
        }
        wait(for: [expectation], timeout: 1)
    }
}
