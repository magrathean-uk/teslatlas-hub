// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

final class HubDesignSystemTests: XCTestCase {
    func testAppearanceStartsAtSystemThenPersistsExplicitToggle() throws {
        let suiteName = "HubDesignSystemTests-\(UUID())"
        let suite = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { suite.removePersistentDomain(forName: suiteName) }
        var preference = HubAppearancePreference(defaults: suite, key: "appearance")

        XCTAssertEqual(preference.mode, .system)
        XCTAssertEqual(preference.toggle(currentIsDark: false), .dark)
        XCTAssertEqual(HubAppearancePreference(defaults: suite, key: "appearance").mode, .dark)
        XCTAssertEqual(preference.toggle(currentIsDark: true), .light)
    }

    func testSheetWindowUsesApprovedContentGeometry() {
        let window = HubSheetStyle.makeWindow(contentSize: NSSize(width: 560, height: 500))
        XCTAssertEqual(window.contentView?.bounds.size, NSSize(width: 560, height: 500))
        XCTAssertFalse(window.styleMask.contains(.resizable))
        XCTAssertEqual(window.backgroundColor, .clear)
    }

    func testCardUsesContinuousRoundedBorderedSurface() {
        let card = HubCardView()
        XCTAssertEqual(card.layer?.cornerRadius, HubMetrics.cardRadius)
        XCTAssertEqual(card.layer?.cornerCurve, .continuous)
        XCTAssertTrue(card.wantsLayer)
    }

    func testSharedButtonsKeepUniformHeightAndEnoughWidthForTheirText() {
        let titles = ["Dashboard", "Connect Tesla", "Restart Hub", "Run Diagnostics"]
        for title in titles {
            let button = HubActionButton(title: title, target: nil, action: nil)
            button.hubStyle = title == "Connect Tesla" ? .primary : .neutral
            button.hubFont = .systemFont(ofSize: 12, weight: .medium)
            let textWidth = ceil(button.attributedTitle.size().width)

            XCTAssertEqual(button.intrinsicContentSize.height,
                           HubMetrics.compactControlHeight,
                           title)
            XCTAssertGreaterThanOrEqual(button.intrinsicContentSize.width,
                                        textWidth + 20,
                                        title)
        }
    }

    func testDynamicLayerColorsRefreshWhenContainingWindowAppearanceChanges() throws {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 200, height: 100),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: .aqua)
        let root = NSView(frame: window.contentView?.bounds ?? .zero)
        window.contentView = root

        let card = HubCardView(frame: NSRect(x: 10, y: 10, width: 80, height: 40))
        let button = HubActionButton(title: "Action", target: nil, action: nil)
        button.hubStyle = .neutral
        root.addSubview(card)
        root.addSubview(button)

        try assertLayerColor(card.layer?.backgroundColor, equalsHex: 0xFFFFFF, in: window.appearance!)
        try assertLayerColor(button.layer?.backgroundColor, equalsHex: 0xFFFFFF, in: window.appearance!)

        window.appearance = NSAppearance(named: .darkAqua)

        try assertLayerColor(card.layer?.backgroundColor, equalsHex: 0x262628, in: window.appearance!)
        try assertLayerColor(button.layer?.backgroundColor, equalsHex: 0x262628, in: window.appearance!)
    }

    func testSharedPaletteResolvesToApprovedLightAndDarkTokens() throws {
        let light = try XCTUnwrap(NSAppearance(named: .aqua))
        let dark = try XCTUnwrap(NSAppearance(named: .darkAqua))

        try assertColor(HubPalette.card, equalsHex: 0xFFFFFF, alpha: 1, in: light)
        try assertColor(HubPalette.elevated, equalsHex: 0xF5F5F7, alpha: 1, in: light)
        try assertColor(HubPalette.accent, equalsHex: 0x007AFF, alpha: 1, in: light)
        try assertColor(HubStatusTone.success.color, equalsHex: 0x34C759, alpha: 1, in: light)
        try assertColor(HubPalette.danger, equalsHex: 0xFF3B30, alpha: 1, in: light)
        try assertColor(HubStatusTone.warning.color, equalsHex: 0xFF9500, alpha: 1, in: light)
        try assertColor(HubPalette.hairline, equalsHex: 0x000000, alpha: 0.08, in: light)

        try assertColor(HubPalette.card, equalsHex: 0x262628, alpha: 1, in: dark)
        try assertColor(HubPalette.elevated, equalsHex: 0x2C2C2E, alpha: 1, in: dark)
        try assertColor(HubPalette.accent, equalsHex: 0x0A84FF, alpha: 1, in: dark)
        try assertColor(HubStatusTone.success.color, equalsHex: 0x30D158, alpha: 1, in: dark)
        try assertColor(HubPalette.danger, equalsHex: 0xFF453A, alpha: 1, in: dark)
        try assertColor(HubStatusTone.warning.color, equalsHex: 0xFF9F0A, alpha: 1, in: dark)
        try assertColor(HubPalette.hairline, equalsHex: 0xFFFFFF, alpha: 0.09, in: dark)
    }

    private func assertColor(_ color: NSColor,
                             equalsHex hex: UInt32,
                             alpha: CGFloat,
                             in appearance: NSAppearance,
                             file: StaticString = #filePath,
                             line: UInt = #line) throws {
        var resolved: NSColor?
        appearance.performAsCurrentDrawingAppearance {
            resolved = color.usingColorSpace(.sRGB)
        }
        let rgba = try XCTUnwrap(resolved, file: file, line: line)
        XCTAssertEqual(rgba.redComponent,
                       CGFloat((hex >> 16) & 0xFF) / 255,
                       accuracy: 0.001,
                       file: file,
                       line: line)
        XCTAssertEqual(rgba.greenComponent,
                       CGFloat((hex >> 8) & 0xFF) / 255,
                       accuracy: 0.001,
                       file: file,
                       line: line)
        XCTAssertEqual(rgba.blueComponent,
                       CGFloat(hex & 0xFF) / 255,
                       accuracy: 0.001,
                       file: file,
                       line: line)
        XCTAssertEqual(rgba.alphaComponent, alpha, accuracy: 0.001, file: file, line: line)
    }

    private func assertLayerColor(_ color: CGColor?,
                                  equalsHex hex: UInt32,
                                  in appearance: NSAppearance,
                                  file: StaticString = #filePath,
                                  line: UInt = #line) throws {
        let color = try XCTUnwrap(color, file: file, line: line)
        let appKitColor = try XCTUnwrap(NSColor(cgColor: color), file: file, line: line)
        try assertColor(appKitColor, equalsHex: hex, alpha: 1, in: appearance,
                        file: file, line: line)
    }
}
