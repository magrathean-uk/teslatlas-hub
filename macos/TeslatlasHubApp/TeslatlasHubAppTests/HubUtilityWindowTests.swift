// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

final class HubUtilityWindowTests: XCTestCase {
    func testQuitEndsIdleFirstRunSheetButDoesNotInterruptBusySetup() throws {
        for busy in [false, true] {
            let main = MainWindowController(controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]))
            let setup = try XCTUnwrap(main.showFirstRunOnboarding())
            let sheet = try XCTUnwrap(setup.window)
            defer { setup.close(); main.close() }
            setup.setBusy(busy)
            XCTAssertNotNil(sheet.sheetParent)
            XCTAssertEqual(AppDelegate.finishSheetsBeforeQuit(in: [sheet]), !busy)
            XCTAssertEqual(sheet.sheetParent != nil, busy)
            if !busy { XCTAssertNil(main.activeModalKind) }
        }
        let delegate = AppDelegate()
        let quit = try XCTUnwrap(AppDelegate.makeMainMenu(actionTarget: delegate)
            .items.first?.submenu?.item(withTitle: "Quit Teslatlas Hub"))
        XCTAssertTrue(quit.target === delegate)
        XCTAssertEqual(quit.action, #selector(AppDelegate.quitApplication(_:)))
        XCTAssertEqual(quit.keyEquivalent, "q")
        XCTAssertEqual(quit.keyEquivalentModifierMask, .command)
    }

    func testVehiclesDoesNotRetainHiddenDashboardDefaultButton() throws {
        let main = MainWindowController(controller: HubController(
            environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"], initialSnapshot: .firstRun))
        defer { main.close() }
        XCTAssertNotNil(main.window?.defaultButtonCell)
        main.selectMainSection(.vehicles)
        XCTAssertNil(main.window?.defaultButtonCell)
        main.selectMainSection(.dashboard)
        XCTAssertNotNil(main.window?.defaultButtonCell)
    }

    func testWindowMenuClosesKeyUtilityThroughResponderChain() throws {
        let main = MainWindowController(controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]))
        defer { main.close() }
        let utility = try XCTUnwrap(main.showLogs())
        let window = try XCTUnwrap(utility.window)
        window.makeKeyAndOrderFront(nil)
        let delegate = AppDelegate(keyWindow: { window })
        let menu = AppDelegate.makeMainMenu(actionTarget: delegate)
        let windowMenu = try XCTUnwrap(menu.items.compactMap(\.submenu).first { $0.title == "Window" })
        let close = try XCTUnwrap(windowMenu.item(withTitle: "Close"))
        XCTAssertEqual(close.keyEquivalent, "w")
        XCTAssertEqual(close.keyEquivalentModifierMask, .command)
        XCTAssertTrue(close.target === delegate)
        XCTAssertTrue(windowMenu.performKeyEquivalent(with: key("w", code: 13, modifiers: .command)))
        RunLoop.main.run(until: Date().addingTimeInterval(0.1))
        XCTAssertNil(main.activeModalKind)
        XCTAssertNotNil(windowMenu.item(withTitle: "Minimize"))
    }

    func testReturnAndKeypadEnterAdvanceWelcome() throws {
        for (character, code): (String, UInt16) in [("\r", 36), ("\u{3}", 76)] {
            let owner = OnboardingWindowController(
                controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
                previewRoute: "welcome", dismissalPolicy: .firstRun, onComplete: { _ in })
            let window = try XCTUnwrap(owner.window)
            defer { owner.close() }
            window.makeKeyAndOrderFront(nil)
            XCTAssertTrue(window.performKeyEquivalent(with: key(character, code: code)))
            XCTAssertEqual(owner.currentRoute, .choose)
        }
    }

    func testEscapeCancelsIdleAccountSetupButNotFirstRunOrBusySetup() throws {
        for (policy, busy, shouldClose): (HubOnboardingDismissalPolicy, Bool, Bool) in [
            (.accountManagement, false, true), (.firstRun, false, false), (.accountManagement, true, false)
        ] {
            let owner = OnboardingWindowController(
                controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
                initialRoute: .provider, dismissalPolicy: policy, onComplete: { _ in })
            let window = try XCTUnwrap(owner.window)
            defer { owner.close() }
            window.makeKeyAndOrderFront(nil)
            owner.setBusy(busy)
            _ = window.performKeyEquivalent(with: key("\u{1b}", code: 53))
            XCTAssertEqual(window.isVisible, !shouldClose)
        }
    }

    func testCommandCloseUsesSheetCancellationPolicy() throws {
        for (policy, busy): (HubOnboardingDismissalPolicy, Bool) in [
            (.accountManagement, false), (.accountManagement, true), (.firstRun, false)
        ] {
            let parent = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 630),
                                  styleMask: [.titled, .closable], backing: .buffered, defer: false)
            parent.isReleasedWhenClosed = false
            let owner = OnboardingWindowController(
                controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
                initialRoute: .provider, dismissalPolicy: policy, onComplete: { _ in })
            let window = try XCTUnwrap(owner.window)
            defer { parent.endSheet(window); owner.close(); parent.close() }
            owner.setBusy(busy)
            parent.makeKeyAndOrderFront(nil)
            parent.beginSheet(window)
            window.makeKeyAndOrderFront(nil)
            // The active app can change during a hosted test run. Keep the
            // menu/action path real, but supply its selected window explicitly.
            let delegate = AppDelegate(keyWindow: { window })
            let menu = AppDelegate.makeMainMenu(actionTarget: delegate)
            let windowMenu = try XCTUnwrap(menu.items.compactMap(\.submenu).first { $0.title == "Window" })
            windowMenu.update()
            XCTAssertEqual(windowMenu.item(withTitle: "Close")?.isEnabled, policy == .accountManagement && !busy)
            _ = windowMenu.performKeyEquivalent(with: key("w", code: 13, modifiers: .command))
            RunLoop.main.run(until: Date().addingTimeInterval(0.1))
            XCTAssertEqual(window.isVisible, policy == .firstRun || busy)
        }
    }

    func testReturnFromFieldEditorHonoursDisabledAndEnabledDefaultButton() throws {
        let owner = OnboardingWindowController(
            controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
            initialRoute: .migration, dismissalPolicy: .accountManagement, onComplete: { _ in })
        let window = try XCTUnwrap(owner.window)
        defer { owner.close() }
        let field = try XCTUnwrap(descendants(window.contentView).compactMap { $0 as? NSTextField }
            .first { $0.placeholderString == "teslamate.local" })
        let button = try XCTUnwrap(descendants(window.contentView).compactMap { $0 as? NSButton }
            .first { $0.title == "Connect to Server" })
        let recorder = KeyboardActionRecorder()
        button.target = recorder
        button.action = #selector(KeyboardActionRecorder.invoke(_:))
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(field)
        XCTAssertFalse(button.isEnabled)
        _ = window.performKeyEquivalent(with: key("\r", code: 36))
        XCTAssertEqual(recorder.count, 0)
        field.stringValue = "teslamate.local"
        owner.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: field))
        XCTAssertTrue(button.isEnabled)
        for (character, code): (String, UInt16) in [("\r", 36), ("\u{3}", 76)] {
            XCTAssertTrue(window.performKeyEquivalent(with: key(character, code: code)))
        }
        XCTAssertEqual(recorder.count, 2)
    }

    private func key(_ character: String, code: UInt16,
                     modifiers: NSEvent.ModifierFlags = []) -> NSEvent {
        NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: modifiers,
                        timestamp: 0, windowNumber: 0, context: nil,
                        characters: character, charactersIgnoringModifiers: character,
                        isARepeat: false, keyCode: code)!
    }

    func testUtilitiesHaveMovableResizableNativeWindowsAndCloseThroughCoordinator() throws {
        let main = MainWindowController(controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]))
        defer { main.close() }
        main.selectMainSection(.vehicles)
        for kind in [HubModalKind.logs, .diagnostics, .serviceDetails] {
            let owner: NSWindowController?
            switch kind {
            case .logs: owner = main.showLogs()
            case .diagnostics: owner = main.showDiagnostics()
            default: owner = main.showServiceDetails()
            }
            let window = try XCTUnwrap(owner?.window)
            XCTAssertNil(window.sheetParent)
            XCTAssertTrue(window.styleMask.contains([.titled, .closable, .resizable]))
            XCTAssertTrue(window.isMovable)
            let close = try XCTUnwrap(window.standardWindowButton(.closeButton))
            XCTAssertFalse(close.isHidden)
            XCTAssertTrue(close.isEnabled)
            let origin = window.frame.origin
            window.setFrameOrigin(NSPoint(x: origin.x + 30, y: origin.y + 20))
            XCTAssertEqual(window.frame.origin.x, origin.x + 30, accuracy: 0.5)
            XCTAssertFalse(descendants(window.contentView).contains { $0.identifier?.rawValue == "hub.modal.close" })
            window.performClose(nil)
            XCTAssertNil(main.activeModalKind)
            XCTAssertEqual(main.selectedSection, .vehicles)
        }
    }

    func testLongLogsReachTheirFinalLineAndReflowOnResize() throws {
        let logs = LogsWindowController(controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]))
        defer { logs.close() }
        let window = try XCTUnwrap(logs.window)
        let text = (1...500).map { "Record \($0): a deterministic line of diagnostic output for scroll verification." }.joined(separator: "\n")
        logs.renderLogs(text)
        window.contentView?.layoutSubtreeIfNeeded()
        let scroll = logs.scrollViewForTesting
        let document = logs.textViewForTesting
        XCTAssertTrue(scroll.hasVerticalScroller)
        XCTAssertFalse(scroll.hasHorizontalScroller)
        XCTAssertGreaterThan(document.frame.height, scroll.contentSize.height * 2)
        let initial = scroll.contentView.bounds.origin.y
        document.scrollToEndOfDocument(nil)
        XCTAssertGreaterThan(scroll.contentView.bounds.origin.y, initial)
        XCTAssertGreaterThanOrEqual(scroll.documentVisibleRect.maxY, document.bounds.maxY - 2)
        let oldWidth = document.frame.width
        window.setContentSize(NSSize(width: 780, height: 480))
        window.contentView?.layoutSubtreeIfNeeded()
        (document as? HubReportTextView)?.fitDocument()
        XCTAssertGreaterThan(document.frame.width, oldWidth)
        XCTAssertEqual(document.frame.width, scroll.contentSize.width, accuracy: 1)
        XCTAssertTrue(document.string.contains("Record 500:"))
    }

    private func descendants(_ view: NSView?) -> [NSView] {
        guard let view else { return [] }
        return [view] + view.subviews.flatMap { descendants($0) }
    }
}

private final class KeyboardActionRecorder: NSObject {
    var count = 0
    @objc func invoke(_ sender: Any?) { count += 1 }
}
