// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

final class HubVisualSnapshotTests: XCTestCase {
    private var baselineWindowCount = 0
    private var baselineAlertCount = 0
    private var alertRecorder = HubPreviewAlertRecorder()
    private var panelRecorder = HubPreviewPanelRecorder()
    private var baselinePanelCount = 0
    private var previousAlertPresenter: HubAlertPresenting?

    override func setUpWithError() throws {
        try super.setUpWithError()
        baselineWindowCount = appWindows.count
        alertRecorder = HubPreviewAlertRecorder()
        panelRecorder = HubPreviewPanelRecorder()
        previousAlertPresenter = HubUIPresentation.replaceAlertPresenterForTesting(alertRecorder)
        baselineAlertCount = alertRecorder.totalInvocations
        baselinePanelCount = panelRecorder.savePanelRequests
    }

    override func tearDownWithError() throws {
        defer {
            _ = HubUIPresentation.replaceAlertPresenterForTesting(previousAlertPresenter)
            try? super.tearDownWithError()
        }
        let remainingWindows = appWindows.map { "\(type(of: $0)): \($0.title), visible=\($0.isVisible)" }
        XCTAssertEqual(appWindows.count, baselineWindowCount,
                       "preview rendering must return AppKit windows to their baseline: \(remainingWindows)")
        XCTAssertEqual(alertRecorder.totalInvocations, baselineAlertCount,
                       "preview rendering must not invoke an alert presenter")
        XCTAssertEqual(panelRecorder.savePanelRequests, baselinePanelCount,
                       "preview rendering must not invoke a save-panel presenter")
    }

    private var appWindows: [NSWindow] {
        // macOS lazily retains an invisible Text Input UI service window after
        // rendering native titlebars. It is not an app window or leaked sheet.
        NSApp.windows.filter { !(String(describing: type(of: $0)) == "TUINSWindow" && !$0.isVisible) }
    }

    func testEveryPreviewSceneRendersANonEmptyNativeSurface() throws {
        for scene in HubPreviewScene.allCases where scene != .manageMenu {
            let png = try autoreleasepool { try render(scene) }
            XCTAssertGreaterThan(png.count, 1_000, scene.rawValue)
            let attachment = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
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
                try png.write(to: destination, options: .atomic)
            }
        }
    }

    func testStateAndAppearanceVariations() throws {
        var stopped = HubSnapshot.previewRunning
        stopped.health = .stopped
        stopped.service = "Stopped"
        var degraded = HubSnapshot.previewRunning
        degraded.health = .degraded
        degraded.service = "Unavailable"
        var empty = HubSnapshot.previewRunning
        empty.controlVehicles = []
        empty.controlVehicleID = nil
        empty.activity = []
        let variants: [(String, HubPreviewScene, HubSnapshot?, String?, Bool)] = [
            ("V01-stopped", .dashboard, stopped, nil, false),
            ("V02-degraded", .dashboard, degraded, nil, false),
            ("V03-empty-vehicles", .vehicles, empty, nil, false),
            ("V04-empty-activity", .activity, empty, nil, false),
            ("V05-migration-error", .migration, nil, "migration-error", false),
            ("V06-verify-loading", .verify, nil, "verify-loading", false),
            ("V07-light-overview", .dashboard, nil, nil, true),
            ("V08-light-settings", .settings, nil, nil, true),
            ("V09-light-onboarding", .choose, nil, nil, true)
        ]
        for (name, scene, snapshot, route, light) in variants {
            let png = try autoreleasepool {
                try render(scene, snapshot: snapshot, route: route, light: light)
            }
            let attachment = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
            attachment.name = name + ".png"
            attachment.lifetime = .keepAlways
            add(attachment)
        }
    }

    func testMinimumWindowSurfaces() throws {
        for scene: HubPreviewScene in [.dashboard, .vehicles, .settings, .diagnostics, .choose, .fleet, .migrationConnected] {
            let png = try autoreleasepool { try render(scene, minimumSize: true) }
            let attachment = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
            attachment.name = "minimum-" + scene.rawValue + ".png"
            attachment.lifetime = .keepAlways
            add(attachment)
        }
    }

    private func render(_ scene: HubPreviewScene, snapshot: HubSnapshot? = nil,
                        route: String? = nil, light: Bool = false, minimumSize: Bool = false) throws -> Data {
        let operations = HubPreviewOperationRecorder()
        let appLog = HubPreviewLogRecorder()
        let controller = HubController(environment: [
            "TESLATLAS_HUB_PREVIEW_SCENE": scene.rawValue,
            "TESLATLAS_HUB_TEST_MODE": "1"
        ],
        commandRunner: operations,
        installedCommandRunner: operations,
        installer: operations,
        serviceRunner: operations,
        initialSnapshot: snapshot)
        let owner: NSWindowController
        switch scene {
        case .welcome, .choose, .provider, .fleet, .legacy, .migration,
             .migrationConnected, .importing, .verify, .finish, .finishMigration:
            owner = OnboardingWindowController(
                controller: controller,
                previewRoute: route ?? scene.onboardingRoute,
                dismissalPolicy: .firstRun,
                onComplete: { _ in }
            )
        case .diagnostics, .logs, .serviceDetails:
            let main = MainWindowController(controller: controller)
            main.configurePreviewScene(scene)
            owner = main
        case .dashboard, .vehicles, .activity, .settings, .manageMenu:
            let main = MainWindowController(controller: controller)
            switch scene {
            case .vehicles, .manageMenu: main.selectMainSection(.vehicles)
            case .activity: main.selectMainSection(.activity)
            case .settings: main.selectMainSection(.settings)
            default: main.selectMainSection(.dashboard)
            }
            owner = main
        }
        defer { owner.close() }

        if minimumSize { owner.window?.setContentSize(NSSize(width: 820, height: 640)) }
        owner.window?.appearance = NSAppearance(named: light ? .aqua : .darkAqua)
        let view = try XCTUnwrap(owner.window?.contentView?.superview, scene.rawValue)
        owner.window?.layoutIfNeeded()
        owner.window?.contentView?.layoutSubtreeIfNeeded()
        view.layoutSubtreeIfNeeded()
        let representation = try XCTUnwrap(
            NSBitmapImageRep(bitmapDataPlanes: nil,
                             pixelsWide: Int(view.bounds.width * 2),
                             pixelsHigh: Int(view.bounds.height * 2),
                             bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
                             isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
            scene.rawValue
        )
        representation.size = view.bounds.size
        view.cacheDisplay(in: view.bounds, to: representation)
        let png = try XCTUnwrap(representation.representation(using: .png, properties: [:]),
                                scene.rawValue)
        XCTAssertTrue(operations.calls.isEmpty,
                      "\(scene.rawValue) invoked \(operations.calls) while rendering its fixture")
        XCTAssertTrue(appLog.calls.isEmpty,
                      "\(scene.rawValue) invoked \(appLog.calls) while rendering its fixture")
        return png
    }

    private func outputName(for scene: HubPreviewScene) -> String {
        let value = scene.rawValue
        return value.prefix(3).uppercased() + value.dropFirst(3) + ".png"
    }
}
