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
        for scene in HubPreviewScene.allCases {
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

    private func render(_ scene: HubPreviewScene) throws -> Data {
        let operations = HubPreviewOperationRecorder()
        let appLog = HubPreviewLogRecorder()
        let controller = HubController(environment: [
            "TESLATLAS_HUB_PREVIEW_SCENE": scene.rawValue,
            "TESLATLAS_HUB_TEST_MODE": "1"
        ],
        commandRunner: operations,
        installedCommandRunner: operations,
        installer: operations,
        serviceRunner: operations)
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
            owner = LogsWindowController(controller: controller,
                                         appLog: appLog,
                                         savePanelPresenter: panelRecorder.present)
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
        defer { owner.close() }

        owner.window?.appearance = NSAppearance(named: .aqua)
        let view = try XCTUnwrap(owner.window?.contentView?.superview, scene.rawValue)
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
