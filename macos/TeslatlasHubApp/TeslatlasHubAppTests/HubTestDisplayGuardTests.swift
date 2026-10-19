// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

/// R-E1F6-21: a test run must not open windows on the owner's external display or take
/// focus. These tests only ever show windows on the built-in display (or off-screen).
final class HubTestDisplayGuardTests: XCTestCase {
    private let builtIn = HubTestScreen(frame: CGRect(x: 0, y: 0, width: 1512, height: 982),
                                        visibleFrame: CGRect(x: 0, y: 0, width: 1512, height: 944),
                                        isBuiltIn: true)
    private let external = HubTestScreen(frame: CGRect(x: 1512, y: -300, width: 3840, height: 2160),
                                         visibleFrame: CGRect(x: 1512, y: -300, width: 3840, height: 2136),
                                         isBuiltIn: false)

    func testWindowOnTheExternalDisplayIsCentredOnTheBuiltInDisplay() {
        let frame = CGRect(x: 2000, y: 200, width: 400, height: 300)
        let origin = HubTestDisplayGuard.relocatedOrigin(for: frame, screens: [builtIn, external])
        XCTAssertEqual(origin, CGPoint(x: 556, y: 322))
    }

    func testWindowOnTheBuiltInDisplayStays() {
        let frame = CGRect(x: 100, y: 100, width: 400, height: 300)
        XCTAssertNil(HubTestDisplayGuard.relocatedOrigin(for: frame, screens: [builtIn, external]))
    }

    func testWindowSpanningBothDisplaysMoves() {
        let frame = CGRect(x: 1400, y: 100, width: 400, height: 300)
        XCTAssertNotNil(HubTestDisplayGuard.relocatedOrigin(for: frame, screens: [builtIn, external]))
    }

    func testWindowTouchingTheExternalDisplayOnlyAtAnEdgeStays() {
        let frame = CGRect(x: 1112, y: 100, width: 400, height: 300)
        XCTAssertNil(HubTestDisplayGuard.relocatedOrigin(for: frame, screens: [builtIn, external]))
    }

    func testWindowLargerThanTheBuiltInDisplayIsTopLeftAligned() {
        let frame = CGRect(x: 2000, y: 0, width: 2000, height: 1200)
        let origin = HubTestDisplayGuard.relocatedOrigin(for: frame, screens: [builtIn, external])
        XCTAssertEqual(origin, CGPoint(x: 0, y: 944 - 1200))
    }

    func testBuiltInDisplayListedAfterTheExternalOneIsStillChosen() {
        let frame = CGRect(x: 2000, y: 200, width: 400, height: 300)
        XCTAssertEqual(HubTestDisplayGuard.relocatedOrigin(for: frame, screens: [external, builtIn]),
                       CGPoint(x: 556, y: 322))
    }

    func testWithoutABuiltInDisplayAWindowOnAnyScreenGoesOutsideEveryScreen() {
        let other = HubTestScreen(frame: CGRect(x: 0, y: 0, width: 2560, height: 1440),
                                  visibleFrame: CGRect(x: 0, y: 0, width: 2560, height: 1415),
                                  isBuiltIn: false)
        let screens = [other, external]
        for frame in [CGRect(x: 100, y: 100, width: 400, height: 300),
                      CGRect(x: 2000, y: 200, width: 400, height: 300)] {
            let origin = HubTestDisplayGuard.relocatedOrigin(for: frame, screens: screens)
            let moved = CGRect(origin: origin ?? .zero, size: frame.size)
            XCTAssertNotNil(origin)
            for screen in screens {
                XCTAssertFalse(moved.intersects(screen.visibleFrame), "\(moved) touches \(screen)")
            }
        }
    }

    func testWithoutABuiltInDisplayAnAlreadyOffScreenWindowStays() {
        let frame = CGRect(x: 90000, y: 0, width: 400, height: 300)
        XCTAssertNil(HubTestDisplayGuard.relocatedOrigin(for: frame, screens: [external]))
    }

    func testNoScreensOrAnEmptyFrameLeavesTheWindowAlone() {
        XCTAssertNil(HubTestDisplayGuard.relocatedOrigin(for: CGRect(x: 5, y: 5, width: 40, height: 40),
                                                         screens: []))
        XCTAssertNil(HubTestDisplayGuard.relocatedOrigin(for: .zero, screens: [builtIn, external]))
    }

    func testPlaceMovesAnUnshownWindowOffTheExternalDisplay() {
        let window = NSWindow(contentRect: NSRect(x: 2000, y: 200, width: 400, height: 300),
                              styleMask: [.titled], backing: .buffered, defer: true)
        window.isReleasedWhenClosed = false
        // AppKit moves a new window onto a real screen when it is created off every one of them; the real
        // arrangement differs from machine to machine, so the origin is set explicitly (never constrained).
        window.setFrameOrigin(NSPoint(x: 2000, y: 200))
        XCTAssertEqual(window.frame.origin, NSPoint(x: 2000, y: 200))
        XCTAssertFalse(window.isVisible)
        XCTAssertTrue(HubTestDisplayGuard.place(window, screens: [builtIn, external]), "frame \(window.frame) of \(NSScreen.screens.map(\.frame))")
        XCTAssertNil(HubTestDisplayGuard.relocatedOrigin(for: window.frame, screens: [builtIn, external]))
        XCTAssertFalse(HubTestDisplayGuard.place(window, screens: [builtIn, external]),
                       "a window already on the built-in display is not moved again")
    }

    func testEveryWindowTheGuardSeesIsTransparentAndIgnoresTheMouse() {
        let window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 400, height: 300),
                              styleMask: [.titled], backing: .buffered, defer: true)
        window.isReleasedWhenClosed = false
        XCTAssertEqual(window.alphaValue, 1)
        XCTAssertFalse(window.ignoresMouseEvents)
        XCTAssertFalse(HubTestDisplayGuard.place(window, screens: [builtIn, external]),
                       "the window is already on the built-in display")
        XCTAssertEqual(window.alphaValue, 0)
        XCTAssertTrue(window.ignoresMouseEvents)
    }

    func testPlaceLeavesASheetOnItsParent() throws {
        let parent = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 400, height: 300),
                              styleMask: [.titled], backing: .buffered, defer: true)
        parent.isReleasedWhenClosed = false
        let sheet = NSWindow(contentRect: NSRect(x: 2000, y: 200, width: 300, height: 100),
                             styleMask: [.titled], backing: .buffered, defer: true)
        sheet.isReleasedWhenClosed = false
        sheet.setFrameOrigin(NSPoint(x: 2000, y: 200))
        XCTAssertEqual(sheet.frame.origin, NSPoint(x: 2000, y: 200))
        // A sheet reports isSheet only while attached; an unattached window is placed.
        XCTAssertFalse(sheet.isSheet)
        XCTAssertTrue(HubTestDisplayGuard.place(sheet, screens: [builtIn, external]), "frame \(sheet.frame) of \(NSScreen.screens.map(\.frame))")
    }

    func testTheGuardIsInstalledByTheTestBundle() {
        XCTAssertTrue(HubTestDisplayGuard.isInstalled,
                      "the test bundle's principal class must install the display guard")
        let bundle = Bundle(for: HubTestBundleMain.self)
        XCTAssertEqual(bundle.object(forInfoDictionaryKey: "NSPrincipalClass") as? String, "HubTestBundleMain")
    }

    func testEveryOrderingEntryPointIsGuardedBeforeTheWindowReachesTheScreen() throws {
        try XCTSkipUnless(HubTestDisplayGuard.isInstalled, "guard not installed: showing a window would be unsafe")
        let screens = HubTestDisplayGuard.currentScreens()
        let start = screens.first(where: { !$0.isBuiltIn })?.frame.origin
            .applying(CGAffineTransform(translationX: 60, y: 60)) ?? .zero
        var inspected = 0
        // The guard must look at a window before the ordering call puts it on screen: the window
        // notifications and the run-loop sweep are only the backstop that runs after it is visible.
        var tracked: NSWindow?
        var visibleAtFirstInspection: Bool?
        HubTestDisplayGuard.inspectionObserver = { window in
            inspected += 1
            if window === tracked, visibleAtFirstInspection == nil { visibleAtFirstInspection = window.isVisible }
        }
        defer { HubTestDisplayGuard.inspectionObserver = nil }
        let entries: [(String, (NSWindow) -> Void)] = [
            ("makeKeyAndOrderFront", { $0.makeKeyAndOrderFront(nil) }),
            ("orderFront", { $0.orderFront(nil) }),
            ("orderFrontRegardless", { $0.orderFrontRegardless() }),
            ("orderWindow", { $0.order(.above, relativeTo: 0) })
        ]
        for (name, show) in entries {
            let window = NSWindow(contentRect: NSRect(origin: start, size: CGSize(width: 240, height: 120)),
                                  styleMask: [.titled], backing: .buffered, defer: true)
            window.isReleasedWhenClosed = false
            // Invisible even where it lands: a guard that fails must not litter a display.
            window.alphaValue = 0
            window.isOpaque = false
            window.backgroundColor = .clear
            window.hasShadow = false
            window.ignoresMouseEvents = true
            let before = inspected
            tracked = window
            visibleAtFirstInspection = nil
            show(window)
            XCTAssertGreaterThan(inspected, before, "\(name) bypassed the display guard")
            XCTAssertEqual(visibleAtFirstInspection, false, "\(name) reached the screen before the display guard saw it")
            XCTAssertNil(HubTestDisplayGuard.relocatedOrigin(for: window.frame, screens: screens),
                         "\(name) left the window on a non-built-in display: \(window.frame)")
            // orderOut, not close: closing the last window makes the host application quit
            // (applicationShouldTerminateAfterLastWindowClosed), so these tests never close windows.
            window.orderOut(nil)
        }
    }

    func testTheClassifierFollowsTheSystemsAnswerForEachDisplay() {
        let query: (UInt32) -> Bool = { $0 == 1 }
        XCTAssertTrue(HubTestDisplayGuard.isBuiltIn(displayNumber: 1, query: query))
        XCTAssertFalse(HubTestDisplayGuard.isBuiltIn(displayNumber: 2, query: query),
                       "an external display is never taken for the built-in one")
        XCTAssertFalse(HubTestDisplayGuard.isBuiltIn(displayNumber: nil, query: query))
    }

    func testTheOrderingEntryPointsAreImplementedByTheGuardNotByAppKit() throws {
        try XCTSkipUnless(HubTestDisplayGuard.isInstalled, "guard not installed")
        let ours = Bundle(for: HubTestBundleMain.self).bundlePath
        let selectors: [Selector] = [#selector(NSWindow.makeKeyAndOrderFront(_:)), #selector(NSWindow.orderFront(_:)),
                                     #selector(NSWindow.orderFrontRegardless), #selector(NSWindow.order(_:relativeTo:)),
                                     #selector(NSWindow.constrainFrameRect(_:to:))]
        for selector in selectors {
            let implementation = unsafeBitCast(class_getMethodImplementation(NSWindow.self, selector), to: UnsafeRawPointer.self)
            var info = Dl_info()
            XCTAssertNotEqual(dladdr(implementation, &info), 0, "\(selector)")
            let image = info.dli_fname.map { String(cString: $0) } ?? ""
            XCTAssertTrue(image.hasPrefix(ours), "\(selector) is implemented by \(image), not by the display guard in \(ours)")
        }
    }

    func testEveryScreenIsClassifiedByTheSystemsBuiltInFlag() throws {
        let key = NSDeviceDescriptionKey("NSScreenNumber")
        let classified = HubTestDisplayGuard.currentScreens()
        XCTAssertEqual(classified.count, NSScreen.screens.count)
        for (screen, entry) in zip(NSScreen.screens, classified) {
            let number = try XCTUnwrap((screen.deviceDescription[key] as? NSNumber)?.uint32Value)
            XCTAssertEqual(entry.isBuiltIn, CGDisplayIsBuiltin(number) != 0,
                           "display \(number) is classified differently from CGDisplayIsBuiltin")
        }
    }

    func testTheRunLoopSweepMovesAWindowThatLandedOnAnotherDisplay() throws {
        try XCTSkipUnless(HubTestDisplayGuard.isInstalled, "guard not installed")
        let provider = HubTestDisplayGuard.screensProvider
        defer { HubTestDisplayGuard.screensProvider = provider }
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 240, height: 120),
                              styleMask: [.titled], backing: .buffered, defer: true)
        window.isReleasedWhenClosed = false
        // Fake layout whose "external" display is the window's own current rectangle
        // and whose built-in display is the real one, so a move is observable and safe.
        let real = HubTestDisplayGuard.currentScreens()
        try XCTSkipUnless(real.contains(where: { $0.isBuiltIn }), "no built-in display online")
        let fakeExternal = HubTestScreen(frame: window.frame.insetBy(dx: -1, dy: -1),
                                         visibleFrame: window.frame, isBuiltIn: false)
        let realBuiltIn = real.first(where: { $0.isBuiltIn })!
        HubTestDisplayGuard.screensProvider = { [realBuiltIn, fakeExternal] in [realBuiltIn, fakeExternal] }
        let before = window.frame.origin
        // The sweep is a run-loop observer (beforeWaiting/afterWaiting): turn the run loop until its effect is
        // observable, with the same 5 s bound as before, instead of guessing which turn runs it (a block
        // on the main queue can finish before the observer's next pass when the machine is loaded).
        let deadline = Date().addingTimeInterval(5)
        while window.frame.origin == before && Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.02))
        }
        XCTAssertNotEqual(window.frame.origin, before, "the run-loop sweep did not move the window")
        XCTAssertNil(HubTestDisplayGuard.relocatedOrigin(for: window.frame, screens: [realBuiltIn, fakeExternal]))
    }
}

final class HubTestDisplayGuardConstraintTests: XCTestCase {
    func testTheHostKeepsOneInvisibleWindowSoReleasingTestWindowsNeverEndsTheRun() throws {
        try XCTSkipUnless(HubTestDisplayGuard.isInstalled, "guard not installed")
        let keeper = try XCTUnwrap(HubTestDisplayGuard.hostKeeper)
        XCTAssertTrue(NSApp.windows.contains(keeper))
        XCTAssertTrue(keeper.isVisible)
        XCTAssertEqual(keeper.alphaValue, 0)
        XCTAssertTrue(keeper.ignoresMouseEvents)
        // The ordinary shape of a test window: ordered in, hidden again, released.
        autoreleasepool {
            let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 200, height: 100),
                                  styleMask: [.titled], backing: .buffered, defer: true)
            window.makeKeyAndOrderFront(nil)
            window.orderOut(nil)
        }
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        XCTAssertTrue(keeper.isVisible, "the keeper survived")
    }

    func testAWindowKeepsItsSizeOnTheSmallBuiltInDisplay() throws {
        let screen = try XCTUnwrap(NSScreen.screens.first)
        let window = NSWindow(contentRect: .zero, styleMask: [.titled, .closable, .resizable],
                              backing: .buffered, defer: true)
        window.alphaValue = 0
        let tall = NSRect(x: 40, y: 40, width: 600, height: screen.visibleFrame.height + 500)
        XCTAssertEqual(window.constrainFrameRect(tall, to: screen), tall,
                       "the display guard must leave window geometry as the code set it")
    }
}

final class HubApplicationActivationTests: XCTestCase {
    func testTheLaunchPolicyIsAccessoryUnderEveryXCTestSignalAndRegularOtherwise() {
        for key in ["XCTestConfigurationFilePath", "XCTestBundlePath", "XCInjectBundleInto"] {
            XCTAssertEqual(HubApplicationActivation.launchPolicy(environment: [key: "/x"]), .accessory, key)
        }
        XCTAssertEqual(HubApplicationActivation.launchPolicy(environment: [:]), .regular)
        XCTAssertEqual(HubApplicationActivation.launchPolicy(environment: ["TESLATLAS_HUB_TEST_MODE": "1"]),
                       .regular, "the policy follows the XCTest seam only")
    }

    func testActivationIsANoOpUnderXCTestAndActivatesOtherwise() {
        var calls: [Bool] = []
        HubApplicationActivation.activate(environment: ["XCTestConfigurationFilePath": "/x"],
                                          effect: { calls.append($0) })
        XCTAssertTrue(calls.isEmpty, "a test host must never activate itself")
        HubApplicationActivation.activate(environment: [:], effect: { calls.append($0) })
        XCTAssertEqual(calls, [true], "outside a test host the application activates, ignoring others")
    }

    func testFocusIsYieldedUnderXCTestOnlyAndNeverReenters() {
        var yields = 0
        HubApplicationActivation.yieldFocus(environment: ["XCTestConfigurationFilePath": "/x"],
                                            effect: { yields += 1 })
        XCTAssertEqual(yields, 1)
        HubApplicationActivation.yieldFocus(environment: [:], effect: { yields += 1 })
        XCTAssertEqual(yields, 1, "outside a test host the application keeps the focus it is given")
        HubApplicationActivation.yieldFocus(environment: ["XCTestConfigurationFilePath": "/x"], effect: {
            yields += 1
            HubApplicationActivation.yieldFocus(environment: ["XCTestConfigurationFilePath": "/x"],
                                                effect: { yields += 100 })
        })
        XCTAssertEqual(yields, 2)
    }

    func testEveryBecameActiveNotificationYieldsUnderXCTestAndNothingOutsideIt() {
        let center = NotificationCenter()
        var yields = 0
        let token = HubApplicationActivation.installFocusYield(
            environment: ["XCTestConfigurationFilePath": "/x"], center: center, effect: { yields += 1 })
        XCTAssertNotNil(token)
        center.post(name: NSApplication.didBecomeActiveNotification, object: nil)
        center.post(name: NSApplication.didBecomeActiveNotification, object: nil)
        XCTAssertEqual(yields, 2)
        if let token { center.removeObserver(token) }

        let plain = NotificationCenter()
        var plainYields = 0
        let plainToken = HubApplicationActivation.installFocusYield(
            environment: [:], center: plain, effect: { plainYields += 1 })
        plain.post(name: NSApplication.didBecomeActiveNotification, object: nil)
        XCTAssertNil(plainToken)
        XCTAssertEqual(plainYields, 0)
    }

    func testTheRunningTestHostIsNotTheActiveApplication() {
        // Run the main loop long enough for the launch activation and its yield to settle.
        RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        XCTAssertFalse(NSApp.isActive, "the test host holds the keyboard focus")
        XCTAssertFalse(NSRunningApplication.current.isActive)
    }

    func testTheLaunchPathsCallTheYield() throws {
        let tests = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let sources = tests.deletingLastPathComponent().appendingPathComponent("TeslatlasHubApp")
        let main = try String(contentsOf: sources.appendingPathComponent("main.swift"), encoding: .utf8)
        XCTAssertTrue(main.contains("HubApplicationActivation.installFocusYield()"))
        let delegate = try String(contentsOf: sources.appendingPathComponent("AppDelegate.swift"), encoding: .utf8)
        for launch in ["func applicationWillFinishLaunching(_ notification: Notification) {\n        HubApplicationActivation.yieldFocus()",
                       "func applicationDidFinishLaunching(_ notification: Notification) {\n        HubApplicationActivation.yieldFocus()"] {
            XCTAssertTrue(delegate.contains(launch), launch)
        }
    }

    func testTheRunningTestHostIsAnAccessoryApplicationFromLaunch() {
        XCTAssertTrue(HubTestHost.isXCTest())
        XCTAssertEqual(NSApp.activationPolicy(), .accessory)
    }

    func testNoSourceActivatesTheApplicationExceptTheSeam() throws {
        let tests = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let sources = tests.deletingLastPathComponent().appendingPathComponent("TeslatlasHubApp")
        let files = try FileManager.default.contentsOfDirectory(at: sources, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "swift" }
        XCTAssertGreaterThan(files.count, 20, "the app sources were not found at \(sources.path)")
        var offenders: [String] = []
        for file in files where file.lastPathComponent != "HubApplicationActivation.swift" {
            let text = try String(contentsOf: file, encoding: .utf8)
            for needle in ["activate(ignoringOtherApps", "activateIgnoringOtherApps",
                           "NSRunningApplication.current.activate", "setActivationPolicy(.regular)"]
            where text.contains(needle) {
                offenders.append("\(file.lastPathComponent): \(needle)")
            }
        }
        XCTAssertEqual(offenders, [])
        let main = try String(contentsOf: sources.appendingPathComponent("main.swift"), encoding: .utf8)
        XCTAssertTrue(main.contains("HubApplicationActivation.launchPolicy()"))
    }
}
