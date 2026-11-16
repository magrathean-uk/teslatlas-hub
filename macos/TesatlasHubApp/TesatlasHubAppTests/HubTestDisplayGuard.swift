// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import CoreGraphics
import ObjectiveC

/// Test-bundle support (R-E1F6-21): every window the app or a test shows during a test
/// run is placed on the built-in display, never on an external one. The test bundle's
/// principal class installs it before the first test. If no built-in display is online,
/// windows are placed outside every screen. The host process never activates itself
/// (see `HubApplicationActivation`), so it never takes keyboard focus either.
struct HubTestScreen: Equatable {
    var frame: CGRect
    var visibleFrame: CGRect
    var isBuiltIn: Bool
}

enum HubTestDisplayGuard {
    /// Where `frame` must be moved to, or nil when it may stay.
    /// Built-in display online: a frame that touches any non-built-in screen is centred
    /// inside the built-in display's visible frame (top-left aligned when larger).
    /// No built-in display online: a frame that touches any screen is moved right of all
    /// screens, so it is outside every screen's visible frame.
    static func relocatedOrigin(for frame: CGRect, screens: [HubTestScreen]) -> CGPoint? {
        guard !frame.isEmpty, !frame.isNull, !screens.isEmpty else { return nil }
        if let builtIn = screens.first(where: { $0.isBuiltIn }) {
            let others = screens.filter { !$0.isBuiltIn }
            guard others.contains(where: { overlaps(frame, $0.frame) }) else { return nil }
            let visible = builtIn.visibleFrame
            let x = frame.width >= visible.width
                ? visible.minX
                : visible.minX + (visible.width - frame.width) / 2
            let y = frame.height >= visible.height
                ? visible.maxY - frame.height
                : visible.minY + (visible.height - frame.height) / 2
            return CGPoint(x: x, y: y)
        }
        guard screens.contains(where: { overlaps(frame, $0.frame) }) else { return nil }
        let farRight = screens.map { $0.frame.maxX }.max() ?? 0
        return CGPoint(x: farRight + 200, y: frame.origin.y)
    }

    private static func overlaps(_ a: CGRect, _ b: CGRect) -> Bool {
        let i = a.intersection(b)
        return !i.isNull && i.width > 0 && i.height > 0
    }

    static func currentScreens() -> [HubTestScreen] {
        let key = NSDeviceDescriptionKey("NSScreenNumber")
        return NSScreen.screens.map { screen in
            let number = (screen.deviceDescription[key] as? NSNumber)?.uint32Value
            return HubTestScreen(frame: screen.frame, visibleFrame: screen.visibleFrame,
                                 isBuiltIn: isBuiltIn(displayNumber: number))
        }
    }

    /// The system's own answer for one display; a screen without a display number is not built-in.
    static func isBuiltIn(displayNumber: UInt32?, query: (UInt32) -> Bool = { CGDisplayIsBuiltin($0) != 0 }) -> Bool {
        displayNumber.map(query) ?? false
    }

    /// The screens consulted by placement. Tests replace it; production of the guard
    /// reads the live displays.
    static var screensProvider: () -> [HubTestScreen] = currentScreens
    /// Called for every window the guard looked at (ordering entry or sweep). Tests only.
    static var inspectionObserver: ((NSWindow) -> Void)?
    private(set) static var isInstalled = false
    private(set) static var placementCount = 0
    private static var isPlacing = false

    /// Moves `window` onto the built-in display when it touches another one. Sheets
    /// travel with their parent and are left alone.
    @discardableResult
    static func place(_ window: NSWindow, screens: [HubTestScreen]? = nil) -> Bool {
        inspectionObserver?(window)
        makeInvisible(window)
        guard !window.isSheet, window.sheetParent == nil else { return false }
        guard let origin = relocatedOrigin(for: window.frame,
                                           screens: screens ?? screensProvider()) else {
            return false
        }
        guard !isPlacing else { return false }
        isPlacing = true
        defer { isPlacing = false }
        window.setFrameOrigin(origin)
        placementCount += 1
        return true
    }

    /// A window shown during a test is transparent and ignores the mouse: nobody can
    /// see it, click it or be interrupted by it, wherever it lands.
    static func makeInvisible(_ window: NSWindow) {
        if window.alphaValue != 0 { window.alphaValue = 0 }
        if !window.ignoresMouseEvents { window.ignoresMouseEvents = true }
    }

    /// One window that exists for the life of the test host: invisible, mouse-transparent,
    /// never closed. The host quits when its last window goes (the delegate's
    /// `applicationShouldTerminateAfterLastWindowClosed`), and an inactive accessory host
    /// has no other windows, so without it a test that releases its window ends the run.
    private(set) static var hostKeeper: NSWindow?

    static func keepHostAlive() {
        guard hostKeeper == nil, NSApp != nil else { return }
        let create = {
            let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1, height: 1),
                                  styleMask: [.borderless], backing: .buffered, defer: true)
            window.isReleasedWhenClosed = false
            window.isOpaque = false
            window.backgroundColor = .clear
            window.hasShadow = false
            window.collectionBehavior = [.ignoresCycle, .stationary]
            makeInvisible(window)
            hostKeeper = window
            window.orderFrontRegardless()
        }
        if Thread.isMainThread { create() } else { DispatchQueue.main.sync(execute: create) }
    }

    static func sweep() {
        guard let application = NSApp else { return }
        for window in application.windows { place(window) }
    }

    /// Idempotent. Intercepts the ordering entry points so a window is placed before
    /// it reaches the window server, and sweeps every window at each run-loop turn and
    /// on window notifications as a backstop for later moves.
    static func install() {
        guard !isInstalled else { return }
        isInstalled = true
        defer { keepHostAlive() }
        let pairs: [(Selector, Selector)] = [
            (#selector(NSWindow.makeKeyAndOrderFront(_:)), #selector(NSWindow.hubGuard_makeKeyAndOrderFront(_:))),
            (#selector(NSWindow.orderFront(_:)), #selector(NSWindow.hubGuard_orderFront(_:))),
            (#selector(NSWindow.orderFrontRegardless), #selector(NSWindow.hubGuard_orderFrontRegardless)),
            (#selector(NSWindow.order(_:relativeTo:)), #selector(NSWindow.hubGuard_orderWindow(_:relativeTo:))),
            // A window on the built-in display keeps the size the code gave it: the (small)
            // built-in screen must not shrink windows that tests measure.
            (#selector(NSWindow.constrainFrameRect(_:to:)), #selector(NSWindow.hubGuard_constrainFrameRect(_:to:)))
        ]
        for (original, replacement) in pairs {
            guard let a = class_getInstanceMethod(NSWindow.self, original),
                  let b = class_getInstanceMethod(NSWindow.self, replacement) else {
                preconditionFailure("display guard cannot intercept \(original)")
            }
            method_exchangeImplementations(a, b)
        }
        if let observer = CFRunLoopObserverCreateWithHandler(
            kCFAllocatorDefault,
            CFRunLoopActivity.beforeWaiting.rawValue | CFRunLoopActivity.afterWaiting.rawValue,
            true, 0
        ) { _, _ in sweep() } {
            CFRunLoopAddObserver(CFRunLoopGetMain(), observer, .commonModes)
        }
        for name in [NSWindow.didBecomeKeyNotification, NSWindow.didUpdateNotification,
                     NSWindow.didMoveNotification, NSWindow.didChangeScreenNotification,
                     NSWindow.didExposeNotification] {
            NotificationCenter.default.addObserver(forName: name, object: nil, queue: nil) { note in
                if let window = note.object as? NSWindow { place(window) }
            }
        }
    }
}

extension NSWindow {
    // After the exchange these names run the original implementations.
    @objc func hubGuard_makeKeyAndOrderFront(_ sender: Any?) {
        HubTestDisplayGuard.place(self)
        hubGuard_makeKeyAndOrderFront(sender)
    }
    @objc func hubGuard_orderFront(_ sender: Any?) {
        HubTestDisplayGuard.place(self)
        hubGuard_orderFront(sender)
    }
    @objc func hubGuard_orderFrontRegardless() {
        HubTestDisplayGuard.place(self)
        hubGuard_orderFrontRegardless()
    }
    @objc func hubGuard_constrainFrameRect(_ frame: NSRect, to screen: NSScreen?) -> NSRect {
        frame
    }
    @objc func hubGuard_orderWindow(_ mode: NSWindow.OrderingMode, relativeTo other: Int) {
        HubTestDisplayGuard.place(self)
        hubGuard_orderWindow(mode, relativeTo: other)
    }
}

/// The test bundle's principal class: XCTest creates it when the bundle loads.
@objc(HubTestBundleMain)
final class HubTestBundleMain: NSObject {
    override init() {
        super.init()
        HubTestDisplayGuard.install()
    }
}
