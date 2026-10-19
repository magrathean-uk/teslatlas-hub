// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

/// The one place the application activates itself, and the launch activation policy.
/// A test host (the production app launched by XCTest) never activates itself and
/// never takes keyboard focus: it is an accessory application from launch, and every
/// self-activation is a no-op there. Outside a test host the behaviour is unchanged
/// (R-E1F6-21).
enum HubApplicationActivation {
    /// `.accessory` under an XCTest host, `.regular` otherwise. Applied before the
    /// first window exists.
    static func launchPolicy(
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> NSApplication.ActivationPolicy {
        HubTestHost.isXCTest(environment: environment) ? .accessory : .regular
    }

    /// Activates the application, ignoring other applications, unless this process
    /// is an XCTest host.
    static func activate(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        effect: (Bool) -> Void = { NSApp.activate(ignoringOtherApps: $0) }
    ) {
        guard !HubTestHost.isXCTest(environment: environment) else { return }
        effect(true)
    }

    /// Under an XCTest host only: hands activation straight back to the application the
    /// owner was using (hide, then unhide without activating, so the windows stay ordered
    /// in for the tests). LaunchServices activates a test host at every launch before any
    /// application code runs, so this runs at launch and on every became-active
    /// notification (R-E1F6-21b). Outside a test host it does nothing.
    static func yieldFocus(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        effect: () -> Void = {
            NSApp.hide(nil)
            NSApp.unhideWithoutActivation()
        }
    ) {
        guard HubTestHost.isXCTest(environment: environment) else { return }
        guard !isYielding else { return }
        isYielding = true
        defer { isYielding = false }
        effect()
    }

    private static var isYielding = false

    /// Registers the became-active observer that yields focus at once. A no-op outside an
    /// XCTest host; returns the observer token for tests.
    @discardableResult
    static func installFocusYield(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        center: NotificationCenter = .default,
        effect: @escaping () -> Void = {
            NSApp.hide(nil)
            NSApp.unhideWithoutActivation()
        }
    ) -> NSObjectProtocol? {
        guard HubTestHost.isXCTest(environment: environment) else { return nil }
        return center.addObserver(forName: NSApplication.didBecomeActiveNotification,
                                  object: nil, queue: nil) { _ in
            yieldFocus(environment: environment, effect: effect)
        }
    }
}
