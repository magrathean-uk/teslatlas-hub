import AppKit
import XCTest
@testable import Teslatlas_Hub

final class OnboardingWindowControllerTests: XCTestCase {
    func testDashboardReportsInitialRefreshWithoutLeavingLaunchBlank() {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let refreshed = expectation(description: "initial dashboard refresh delivered")
        var delivered: HubSnapshot?
        let dashboard = MainWindowController(controller: controller) {
            delivered = $0
            refreshed.fulfill()
        }
        XCTAssertNotNil(dashboard.window)
        wait(for: [refreshed], timeout: 1)

        XCTAssertEqual(delivered?.account, "Connected")
    }

    func testClosingLastWindowTerminatesOnlyTheGUI() {
        XCTAssertTrue(AppDelegate().applicationShouldTerminateAfterLastWindowClosed(NSApp))
    }

    func testStateBranchesByPathAndProviderAndBackTracks() {
        var state = HubOnboardingState()
        XCTAssertEqual(state.route, .welcome)
        XCTAssertEqual(state.step, 1)

        state.advance()
        XCTAssertEqual(state.route, .choose)
        XCTAssertEqual(state.step, 2)
        state.advance()
        XCTAssertEqual(state.route, .provider)
        XCTAssertEqual(state.step, 3)

        state.provider = .legacy
        state.advance()
        XCTAssertEqual(state.route, .legacy)
        state.back()
        XCTAssertEqual(state.route, .provider)

        state.provider = .fleet
        state.advance()
        XCTAssertEqual(state.route, .fleet)
        state.back()
        XCTAssertEqual(state.route, .provider)
        state.back()
        XCTAssertEqual(state.route, .choose)
        state.back()
        XCTAssertEqual(state.route, .welcome)
    }

    func testMigrationBranchUsesStepThreeAndBackReturnsToChoice() {
        var state = HubOnboardingState(route: .choose, path: .migration, provider: .fleet)
        state.advance()
        XCTAssertEqual(state.route, .migration)
        XCTAssertEqual(state.step, 3)
        state.back()
        XCTAssertEqual(state.route, .choose)
    }

    func testInterruptedMigrationReturnsToImportInsteadOfVerification() {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                     resumeMigrationHandoverPhase: .importing,
                                                     onComplete: {})

        XCTAssertTrue(buttons(in: onboarding.window?.contentView)
            .contains { $0.title == "Check TeslaMate 4.1.1" })
        XCTAssertTrue(labels(in: onboarding.window?.contentView)
            .contains { $0.stringValue.contains("previous import did not finish") })
        XCTAssertFalse(labels(in: onboarding.window?.contentView)
            .contains { $0.stringValue == "Checking your setup…" })
    }

    func testChoosePageShowsMigrationCopyAndIconWhenSelected() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller, onComplete: {})
        let continueButton = try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "Continue" })
        continueButton.performClick(nil)

        let migration = try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "Migrate from TeslaMate" })
        XCTAssertFalse(migration.isHidden)
        XCTAssertEqual(migration.toolTip, "Import TeslaMate 4.1.1. Your source stays unchanged.")
        XCTAssertTrue(labels(in: onboarding.window?.contentView)
            .contains { $0.stringValue == "Exact TeslaMate 4.1.1 compatibility is checked before any data is copied." })

        migration.performClick(nil)
        let back = try XCTUnwrap(buttons(in: onboarding.window?.contentView)
            .first { $0.title == "Back" })
        back.performClick(nil)
        XCTAssertTrue(labels(in: onboarding.window?.contentView)
            .contains { $0.stringValue == "Your Tesla history, privately collected." })
    }

    func testSelectedDesignsRenderAtNativeSize() throws {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let dashboard = MainWindowController(controller: controller)
        let onboarding = OnboardingWindowController(controller: controller,
                                                     previewRoute: "choose-migration",
                                                     onComplete: {})
        XCTAssertEqual(dashboard.window?.contentView?.bounds.size, NSSize(width: 900, height: 630))
        XCTAssertEqual(onboarding.window?.contentView?.bounds.size, NSSize(width: 900, height: 630))

        guard let folder = ProcessInfo.processInfo.environment["TESLATLAS_HUB_SNAPSHOT_DIR"] else {
            return
        }
        let destination = URL(fileURLWithPath: folder, isDirectory: true)
        try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
        try render(dashboard.window, to: destination.appendingPathComponent("dashboard.png"))
        try render(onboarding.window, to: destination.appendingPathComponent("onboarding-choice.png"))
    }

    private func render(_ window: NSWindow?, to destination: URL) throws {
        let view = try XCTUnwrap(window?.contentView?.superview ?? window?.contentView)
        view.layoutSubtreeIfNeeded()
        view.displayIfNeeded()
        let representation = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: representation)
        let png = try XCTUnwrap(representation.representation(using: .png, properties: [:]))
        try png.write(to: destination, options: .atomic)
    }

    private func buttons(in view: NSView?) -> [NSButton] {
        guard let view else { return [] }
        return (view as? NSButton).map { [$0] } ?? view.subviews.flatMap { buttons(in: $0) }
    }

    private func labels(in view: NSView?) -> [NSTextField] {
        guard let view else { return [] }
        return (view as? NSTextField).map { [$0] } ?? view.subviews.flatMap { labels(in: $0) }
    }
}
