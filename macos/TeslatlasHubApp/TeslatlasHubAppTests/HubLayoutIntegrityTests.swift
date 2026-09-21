// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

final class HubLayoutIntegrityTests: XCTestCase {
    func testThreeCommandGroupsFitAlignedDesktopRows() throws {
        let model = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let card = HubVehicleCardView(actions: HubVehicleCardActions(select: { _ in }, command: { _, _ in }))
        card.apply(vehicle: model.snapshot.controlVehicles.first,
                   allVehicles: model.snapshot.controlVehicles,
                   provider: .fleet, enabled: true)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 744, height: 540),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        window.contentView = card
        card.layoutSubtreeIfNeeded()
        let commands = descendants(card).compactMap { $0 as? HubActionButton }
        XCTAssertEqual(commands.count, 3)
        for button in commands {
            button.layoutSubtreeIfNeeded()
            XCTAssertEqual(button.frame.height, 32, accuracy: 0.5)
            assertContained(button)
            XCTAssertFalse(button.hubTitleLabel.frame.intersects(button.hubImageView.frame))
        }
    }

    func testNavigationSymbolsStayInsideSelectedPills() {
        let actions = HubNavigationActions(select: { _ in }, diagnostics: {}, logs: {},
                                           serviceDetails: {}, importTeslaMate: {}, connectTesla: {},
                                           accountMenu: { NSMenu() }, appearance: {})
        let navigation = HubNavigationBar(actions: actions)
        navigation.frame = NSRect(x: 0, y: 0, width: 492, height: 36)
        navigation.layoutSubtreeIfNeeded()
        let buttons = descendants(navigation).compactMap { $0 as? HubActionButton }
        XCTAssertEqual(buttons.count, 4)
        for button in buttons {
            button.layoutSubtreeIfNeeded()
            assertContained(button)
            XCTAssertGreaterThanOrEqual(button.hubImageView.frame.minX, 16)
            XCTAssertGreaterThanOrEqual(button.bounds.maxX - button.hubTitleLabel.frame.maxX, 16)
        }
    }

    func testNavigationAndHorizontalActionsContainTheirContent() {
        for title in ["Overview", "Vehicles", "Diagnostics", "Activity & Logs", "Service Details", "Run Again", "Copy", "Save…"] {
            let button = HubActionButton(title: title, target: nil, action: nil)
            button.image = NSImage(systemSymbolName: "car", accessibilityDescription: nil)
            button.imagePosition = .imageLeading
            button.hubFont = .systemFont(ofSize: 12, weight: .medium)
            button.frame = NSRect(origin: .zero, size: button.intrinsicContentSize)
            button.layoutSubtreeIfNeeded()
            assertContained(button)
            XCTAssertGreaterThanOrEqual(button.hubImageView.frame.minX, 10)
            XCTAssertGreaterThanOrEqual(button.bounds.maxX - button.hubTitleLabel.frame.maxX, 10)
        }
    }

    func testTextOnlyButtonHasRoomForItsCellAndHitTestingUsesSuperviewCoordinates() {
        let parent = NSView(frame: NSRect(x: 0, y: 0, width: 300, height: 100))
        let button = HubActionButton(title: "Cancel", target: nil, action: nil)
        button.frame = NSRect(origin: NSPoint(x: 90, y: 25), size: button.intrinsicContentSize)
        parent.addSubview(button)
        button.layoutSubtreeIfNeeded()
        XCTAssertTrue(button.hubImageView.isHidden)
        XCTAssertGreaterThanOrEqual(button.hubTitleLabel.frame.width, button.hubTitleLabel.cell!.cellSize.width)
        XCTAssertTrue(button.hitTest(NSPoint(x: button.frame.midX, y: button.frame.midY)) === button)
        XCTAssertNil(button.hitTest(NSPoint(x: 5, y: 5)))
    }

    private func assertContained(_ button: HubActionButton, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue(button.bounds.contains(button.hubImageView.frame), button.title, file: file, line: line)
        XCTAssertTrue(button.bounds.contains(button.hubTitleLabel.frame), button.title, file: file, line: line)
        let textWidth = ceil((button.title as NSString).size(withAttributes: [.font: button.hubFont]).width)
        XCTAssertGreaterThanOrEqual(button.hubTitleLabel.frame.width, textWidth + 4, button.title, file: file, line: line)
        XCTAssertFalse(button.hubTitleLabel.frame.intersects(button.hubImageView.frame), button.title, file: file, line: line)
    }

    private func descendants(_ view: NSView) -> [NSView] {
        [view] + view.subviews.flatMap(descendants)
    }
}
