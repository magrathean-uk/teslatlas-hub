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

    func testDashboardAtActualMinimumKeepsAllVehiclesAndFooterReachableByScrolling() throws {
        var snapshot = HubSnapshot.previewRunning
        snapshot.activity = [
            HubActivity(message: "Imported TeslaMate history", age: "just now", color: .systemGreen),
            HubActivity(message: "Vehicle went offline", age: "2 min", color: .systemOrange),
            HubActivity(message: "Position stored", age: "4 min", color: .systemBlue)
        ]
        let actions = HubDashboardActions(
            start: {}, stop: {}, restart: {}, setup: {}, diagnostics: {},
            vehicle: HubVehicleCardActions(select: { _ in }, command: { _, _ in }),
            serviceDetails: {}, dataFolder: {}
        )
        let dashboard = HubDashboardView(actions: actions)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 820, height: 590),
                              styleMask: [.titled, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        window.contentView = dashboard
        dashboard.apply(snapshot: snapshot, transition: nil, activity: snapshot.activity)
        dashboard.layoutSubtreeIfNeeded()

        let scroll = try XCTUnwrap(descendants(dashboard).compactMap { $0 as? NSScrollView }.first {
            $0.identifier?.rawValue == "hub.dashboard.scroll"
        })
        let document = try XCTUnwrap(scroll.documentView)
        document.layoutSubtreeIfNeeded()
        XCTAssertEqual(dashboard.frame.size, NSSize(width: 820, height: 590))
        XCTAssertGreaterThan(document.bounds.height, scroll.contentView.bounds.height)
        let text = descendants(dashboard).compactMap { $0 as? NSTextField }
            .map(\.stringValue).joined(separator: " ")
        for expected in ["Aurora", "Comet", "Imported TeslaMate history",
                         "Vehicle went offline", "Position stored"] {
            XCTAssertTrue(text.contains(expected), "missing \(expected)")
        }

        let folder = try XCTUnwrap(descendants(dashboard).compactMap { $0 as? NSButton }
            .first { $0.title == "Data Folder" })
        let bottom = max(0, document.bounds.maxY - scroll.contentView.bounds.height)
        scroll.contentView.scroll(to: NSPoint(x: 0, y: bottom))
        scroll.reflectScrolledClipView(scroll.contentView)
        let folderFrame = folder.convert(folder.bounds, to: document)
        XCTAssertTrue(scroll.documentVisibleRect.intersects(folderFrame))
    }

    func testVehiclesAtActualMinimumKeepsFleetCardAndFooterReachableByScrolling() throws {
        let vehicles = HubVehiclesView(actions: .init(select: { _ in }, command: { _, _ in }))
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 820, height: 590),
                              styleMask: [.titled, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        window.contentView = vehicles
        vehicles.apply(snapshot: .previewRunning, enabled: true)
        vehicles.layoutSubtreeIfNeeded()

        let scroll = try XCTUnwrap(descendants(vehicles).compactMap { $0 as? NSScrollView }.first {
            $0.identifier?.rawValue == "hub.vehicles.scroll"
        })
        let document = try XCTUnwrap(scroll.documentView)
        document.layoutSubtreeIfNeeded()
        XCTAssertEqual(vehicles.frame.size, NSSize(width: 820, height: 590))
        XCTAssertGreaterThan(document.bounds.height, scroll.contentView.bounds.height)
        XCTAssertTrue(descendants(vehicles).compactMap { $0 as? NSTextField }
            .contains { $0.stringValue == "Aurora" })

        let footer = try XCTUnwrap(descendants(vehicles).compactMap { $0 as? NSTextField }.first {
            $0.stringValue == "Teslatlas Hub \(HubRelease.bundledVersion)"
        })
        let bottom = max(0, document.bounds.maxY - scroll.contentView.bounds.height)
        scroll.contentView.scroll(to: NSPoint(x: 0, y: bottom))
        scroll.reflectScrolledClipView(scroll.contentView)
        let footerFrame = footer.convert(footer.bounds, to: document)
        XCTAssertTrue(scroll.documentVisibleRect.intersects(footerFrame))
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
