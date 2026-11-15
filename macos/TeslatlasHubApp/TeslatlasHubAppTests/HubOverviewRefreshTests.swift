// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

/// The Overview refreshes while its window is shown, whether or not the app is active
/// (M02-T27). Notification centre, window visibility and the settle delay are doubles: no
/// window is shown or activated.
final class HubOverviewRefreshTests: XCTestCase {
    private final class Shown { var value = true }

    private func makeWindow(_ rig: OwnerRig, centre: NotificationCenter, shown: Shown,
                            settleDelay: TimeInterval = 600) -> MainWindowController {
        var delivered = false
        let window = MainWindowController(
            controller: rig.controller, serviceTransitionTimeout: 5, serviceTransitionPollInterval: 0.05,
            errorPresenter: { _ in }, onInitialRefresh: { _ in delivered = true },
            refreshNotificationCenter: centre, settleRefreshDelay: settleDelay,
            windowShownOverride: { shown.value })
        spinUntil(5) { delivered }
        XCTAssertTrue(delivered)
        return window
    }

    func testPolicyKeepsFifteenSecondCadenceAndStopsOnlyWhenHiddenOrMinimised() {
        XCTAssertEqual(HubOverviewRefreshPolicy.interval, 15)
        XCTAssertTrue(HubOverviewRefreshPolicy.isShown(visible: true, miniaturized: false))
        XCTAssertFalse(HubOverviewRefreshPolicy.isShown(visible: false, miniaturized: false))
        XCTAssertFalse(HubOverviewRefreshPolicy.isShown(visible: true, miniaturized: true))
    }

    func testBecomingActiveRefreshesAtOnceWhenShown() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        let centre = NotificationCenter()
        let window = makeWindow(rig, centre: centre, shown: Shown())
        defer { window.close() }
        let before = rig.runner.count("status")
        centre.post(name: NSApplication.didBecomeActiveNotification, object: nil)
        spinUntil(5) { rig.runner.count("status") > before }
        XCTAssertGreaterThan(rig.runner.count("status"), before)
    }

    func testWindowBecomingKeyRefreshesAtOnce() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        let centre = NotificationCenter()
        let window = makeWindow(rig, centre: centre, shown: Shown())
        defer { window.close() }
        let before = rig.runner.count("status")
        centre.post(name: NSWindow.didBecomeKeyNotification, object: window.window)
        spinUntil(5) { rig.runner.count("status") > before }
        XCTAssertGreaterThan(rig.runner.count("status"), before)
        // Another window becoming key is not this window's refresh.
        let after = rig.runner.count("status")
        centre.post(name: NSWindow.didBecomeKeyNotification, object: NSWindow())
        spinBriefly(0.4)
        XCTAssertEqual(rig.runner.count("status"), after)
    }

    func testHiddenOrMinimisedWindowDoesNotRefresh() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        let centre = NotificationCenter()
        let shown = Shown()
        let window = makeWindow(rig, centre: centre, shown: shown, settleDelay: 0.2)
        defer { window.close() }
        shown.value = false
        let before = rig.runner.count("status")
        centre.post(name: NSApplication.didBecomeActiveNotification, object: nil)
        centre.post(name: NSWindow.didBecomeKeyNotification, object: window.window)
        spinBriefly(0.6)
        XCTAssertEqual(rig.runner.count("status"), before, "no refresh while hidden, even after the settle delay")
    }

    func testRefreshRunsOnceMoreAfterLaunchRestartSettles() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        let before = rig.runner.count("status")
        let window = makeWindow(rig, centre: NotificationCenter(), shown: Shown(), settleDelay: 0.3)
        defer { window.close() }
        let afterInitial = rig.runner.count("status")
        XCTAssertGreaterThan(afterInitial, before)
        spinUntil(5) { rig.runner.count("status") > afterInitial }
        XCTAssertGreaterThan(rig.runner.count("status"), afterInitial, "the settle refresh ran without the app being active")
    }
}
