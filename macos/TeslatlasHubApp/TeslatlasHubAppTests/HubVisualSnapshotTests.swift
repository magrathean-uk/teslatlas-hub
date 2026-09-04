// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

final class HubVisualSnapshotTests: XCTestCase {
    func testEveryPreviewSceneRendersANonEmptyNativeSurface() throws {
        for scene in HubPreviewScene.allCases {
            let rendered = try render(scene)
            XCTAssertGreaterThan(rendered.png.count, 1_000, scene.rawValue)
            let attachment = XCTAttachment(data: rendered.png, uniformTypeIdentifier: "public.png")
            attachment.name = outputName(for: scene)
            attachment.lifetime = .keepAlways
            add(attachment)
            if let directory = ProcessInfo.processInfo.environment["TESLATLAS_HUB_SNAPSHOT_DIR"] {
                let destination = URL(fileURLWithPath: directory, isDirectory: true)
                    .appendingPathComponent(outputName(for: scene))
                try FileManager.default.createDirectory(
                    at: destination.deletingLastPathComponent(),
                    withIntermediateDirectories: true
                )
                try rendered.png.write(to: destination, options: .atomic)
            }
            withExtendedLifetime(rendered.owner) {}
        }
    }

    private func render(_ scene: HubPreviewScene) throws -> (png: Data, owner: AnyObject) {
        let controller = HubController(environment: [
            "TESLATLAS_HUB_PREVIEW_SCENE": scene.rawValue,
            "TESLATLAS_HUB_TEST_MODE": "1"
        ])
        let owner: NSWindowController
        switch scene {
        case .welcome, .choose, .migration, .migrationConnected, .verify, .finishMigration:
            owner = OnboardingWindowController(
                controller: controller,
                previewRoute: scene.onboardingRoute,
                onComplete: { _ in }
            )
        case .diagnostics:
            owner = DiagnosticsWindowController(controller: controller)
        case .logs:
            owner = LogsWindowController(controller: controller)
        case .serviceDetails:
            owner = ServiceDetailsWindowController(
                snapshot: controller.snapshot,
                controller: controller,
                onChanged: {}
            )
        case .dashboard, .vehicles, .manageMenu:
            let main = MainWindowController(controller: controller)
            if scene != .dashboard { main.selectMainSection(.vehicles) }
            owner = main
        }

        let view = try XCTUnwrap(owner.window?.contentView, scene.rawValue)
        view.layoutSubtreeIfNeeded()
        let representation = try XCTUnwrap(
            view.bitmapImageRepForCachingDisplay(in: view.bounds),
            scene.rawValue
        )
        view.cacheDisplay(in: view.bounds, to: representation)
        let png = try XCTUnwrap(representation.representation(using: .png, properties: [:]),
                                scene.rawValue)
        return (png, owner)
    }

    private func outputName(for scene: HubPreviewScene) -> String {
        let value = scene.rawValue
        return value.prefix(3).uppercased() + value.dropFirst(3) + ".png"
    }
}
