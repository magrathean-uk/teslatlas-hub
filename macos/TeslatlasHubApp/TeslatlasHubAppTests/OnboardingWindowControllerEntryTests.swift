// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

/// R-E1F6-18, window and presentation routes: one test per route, each driving the route's
/// actual selector or callback, observing the typed controller request, and repeating
/// with a conflicting owner held. Confirmation routes bring the conflict in after the
/// confirmation has opened.
extension OnboardingWindowControllerTests {
    struct EntryRoute {
        let name: String
        let family: HubLifecycleFamily
        var options = OwnerRig.Options(loaded: .loaded)
        /// The route takes the conflicting owner itself, at the moment it chooses.
        var manualConflict = false
        /// Alerts that go through `HubUIPresentation` are answered with their second button.
        var presenter = false
        let effect: (WindowOwnerRig) -> Bool
        let invoke: (OnboardingWindowControllerTests, WindowOwnerRig, _ conflict: @escaping () -> Void) throws -> Void
    }

    func assertEntry(_ route: EntryRoute, file: StaticString = #filePath, line: UInt = #line) throws {
        // The foreign owner is never of the route's own family.
        let foreignFamily: HubLifecycleFamily = route.family == .signOut ? .diagnostics : .signOut

        // 1. Accepted: the typed request reaches the controller and its effect is seen.
        do {
            let presenter = RecordingAlertPresenter()
            presenter.answer = .alertSecondButtonReturn
            let previous = route.presenter ? HubUIPresentation.replaceAlertPresenterForTesting(presenter) : nil
            defer { if route.presenter { HubUIPresentation.replaceAlertPresenterForTesting(previous) } }
            let harness = try WindowOwnerRig(route.options)
            defer { harness.tearDown() }
            harness.answers = [.alertSecondButtonReturn, .alertSecondButtonReturn, .alertFirstButtonReturn]
            try route.invoke(self, harness, {})
            harness.rig.drain()
            spinBriefly(0.1)
            XCTAssertGreaterThanOrEqual(harness.rig.acquisitions(route.family), 1,
                                        "\(route.name): the route acquired its owner", file: file, line: line)
            XCTAssertTrue(route.effect(harness), "\(route.name): the accepted request had its effect: \(harness.trace)", file: file, line: line)
        }

        // 2. Conflicting owner held: refused, with no effect.
        do {
            let presenter = RecordingAlertPresenter()
            presenter.answer = .alertSecondButtonReturn
            let previous = route.presenter ? HubUIPresentation.replaceAlertPresenterForTesting(presenter) : nil
            defer { if route.presenter { HubUIPresentation.replaceAlertPresenterForTesting(previous) } }
            let harness = try WindowOwnerRig(route.options)
            defer { harness.tearDown() }
            harness.answers = [.alertSecondButtonReturn, .alertSecondButtonReturn, .alertFirstButtonReturn]
            var foreign: HubLifecycleToken?
            let takeForeign = {
                if foreign == nil, case let .success(token) = harness.controller.lifecycle.acquire(foreignFamily) {
                    foreign = token
                }
            }
            presenter.whileConfirming = { _ in takeForeign() }
            harness.whileConfirming = { _ in takeForeign() }
            if !route.manualConflict { takeForeign() }
            // A conflict that arrives mid-flow is seen by the window before its next step (the capability
            // snapshot is published asynchronously), so the next step meets the window's own guard.
            try route.invoke(self, harness, { takeForeign(); spinBriefly(0.1) })
            spinBriefly(0.1)
            XCTAssertNotNil(foreign, "\(route.name): the conflict was introduced", file: file, line: line)
            XCTAssertEqual(harness.rig.acquisitions(route.family), route.family == foreignFamily ? 1 : 0,
                           "\(route.name): the route was not admitted", file: file, line: line)
            XCTAssertFalse(route.effect(harness), "\(route.name): the refused request had no effect", file: file, line: line)
            // The window's own capability guard refuses quietly (a beep): no confirmation left open and no error
            // shown. A guard that is missing lets the press through to the controller, whose refusal then
            // surfaces as an error or leaves a sheet on screen.
            // (Account and vehicle-control routes pass the press to the controller by design, and show its refusal.)
            let guardedByTheWindow: Set<HubLifecycleFamily> = [.start, .stop, .restart, .remove, .delete]
            if guardedByTheWindow.contains(route.family) {
                XCTAssertTrue(harness.errors.isEmpty, "\(route.name): the refusal was silent, not an error: \(harness.errors)", file: file, line: line)
                XCTAssertTrue(presenter.errors.isEmpty, "\(route.name): no error was presented: \(presenter.errors)", file: file, line: line)
            }
            if !route.manualConflict, guardedByTheWindow.contains(route.family) {
                XCTAssertFalse(harness.hasSheet, "\(route.name): no confirmation sheet was left open", file: file, line: line)
            }
            if let foreign { harness.controller.lifecycle.settle(foreign) {} }
        }
    }

    // MARK: setup helpers

    func openOnboarding(_ harness: WindowOwnerRig, route: HubOnboardingRoute) throws -> OnboardingWindowController {
        let onboarding = try XCTUnwrap(harness.window.showOnboarding(route: route), "the onboarding window opened")
        XCTAssertTrue(harness.controller.lifecycleCapabilities.isIdle || harness.controller.lifecycleCapabilities.owner != nil,
                      "opening the page is a presentation")
        return onboarding
    }

    func submitFleet(_ onboarding: OnboardingWindowController) throws {
        try field(placeholder: "Tesla application client ID", in: onboarding).stringValue = "client"
        try field(placeholder: "Access token", in: onboarding).stringValue = "access-token"
        try field(placeholder: "Refresh token", in: onboarding).stringValue = "refresh-token"
        press(try primaryButton("Set Up Fleet", in: onboarding))
    }

    func submitLegacyToken(_ onboarding: OnboardingWindowController) throws {
        try field(placeholder: "Access token", in: onboarding).stringValue = "access-token"
        try field(placeholder: "Refresh token", in: onboarding).stringValue = "refresh-token"
        press(try primaryButton("Connect Tesla", in: onboarding))
    }

    func submitLocalCompatibility(_ onboarding: OnboardingWindowController) throws {
        let toggle = try XCTUnwrap(descendantViews(of: onboarding.window?.contentView).compactMap { $0 as? NSButton }
            .first { $0.identifier?.rawValue == "onboarding.local-history" })
        toggle.state = .on
        press(toggle)
        try field(placeholder: "Password-free local PostgreSQL URL", in: onboarding).stringValue
            = "postgresql://reader@127.0.0.1:55438/teslamate_snapshot"
        try field(placeholder: "Car ID", in: onboarding).stringValue = "1"
        try field(placeholder: "Owner-only PostgreSQL password file", in: onboarding).stringValue = "/private/password"
        let acknowledgement = try XCTUnwrap(descendantViews(of: onboarding.window?.contentView).compactMap { $0 as? NSButton }
            .first { $0.accessibilityLabel()?.hasPrefix("I confirm") == true })
        acknowledgement.state = .on
        press(acknowledgement)
        press(try primaryButton("Check Snapshot", in: onboarding))
    }

    func currentOnboarding(_ harness: WindowOwnerRig) -> OnboardingWindowController? {
        harness.privateValue("activeModalController", as: NSWindowController.self) as? OnboardingWindowController
    }

    // MARK: 1. dashboard and legacy main-window controls

    func testEntryDashboardStartUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DashboardStart", family: .start, options: .init(loaded: .unloaded),
                              effect: { $0.service.count("run:start") >= 1 }) { test, h, _ in
            h.activate(h.dashboardButton("Start Hub"))
        })
    }

    func testEntryDashboardStopUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DashboardStop", family: .stop, manualConflict: true,
                              effect: { $0.service.count("run:stop") >= 1 }) { test, h, conflict in
            h.activate(h.dashboardButton("Stop Hub…"))
            XCTAssertTrue(h.hasSheet, "the Stop confirmation opened")
            conflict()
            h.confirm("Stop Hub")
        })
    }

    func testEntryDashboardRestartUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DashboardRestart", family: .restart,
                              effect: { $0.service.count("run:restart") >= 1 }) { test, h, _ in
            h.activate(h.dashboardButton("Restart Hub"))
        })
    }

    func testEntryLegacyMainStartUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "LegacyMainStart", family: .start, options: .init(loaded: .unloaded),
                              effect: { $0.service.count("run:start") >= 1 }) { test, h, _ in
            let button = try XCTUnwrap(h.privateValue("stopButton", as: NSButton.self), "the legacy health-selected button")
            XCTAssertEqual(button.title, "Start Hub", "it selects Start for a stopped Hub")
            _ = NSApp.sendAction(try XCTUnwrap(button.action), to: h.window, from: button)
        })
    }

    func testEntryLegacyMainStopUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "LegacyMainStop", family: .stop, manualConflict: true,
                              effect: { $0.service.count("run:stop") >= 1 }) { test, h, conflict in
            let button = try XCTUnwrap(h.privateValue("stopButton", as: NSButton.self))
            XCTAssertEqual(button.title, "Stop Hub…", "it selects Stop for a running Hub")
            _ = NSApp.sendAction(try XCTUnwrap(button.action), to: h.window, from: button)
            XCTAssertTrue(h.hasSheet)
            conflict()
            h.confirm("Stop Hub")
        })
    }

    // MARK: 2. setup and account entry, then the operational submission

    func testEntryDashboardSetupUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DashboardSetup", family: .account, options: .init(installed: false),
                              effect: { $0.runner.count("setup-fleet") >= 1 }) { test, h, _ in
            h.activate(h.dashboardButton("Set Up Hub"))
            let onboarding = try XCTUnwrap(test.currentOnboarding(h), "Set Up Hub opened onboarding")
            XCTAssertEqual(h.rig.acquisitions(.account), 0, "opening the page reserved nothing")
            onboarding.navigate(to: .fleet)
            try test.submitFleet(onboarding)
        })
    }

    func testEntryDashboardConnectUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DashboardConnect", family: .account,
                              effect: { $0.runner.count("setup-fleet") >= 1 }) { test, h, _ in
            h.activate(h.window.connectButton)
            let onboarding = try XCTUnwrap(test.currentOnboarding(h), "Connect Tesla opened onboarding")
            XCTAssertEqual(h.rig.acquisitions(.account), 0, "opening the page reserved nothing")
            onboarding.navigate(to: .fleet)
            try test.submitFleet(onboarding)
        })
    }

    func testEntryManageTeslaUsesLifecycleOwner() throws {
        var options = OwnerRig.Options(loaded: .loaded)
        options.runningStatus = true
        try assertEntry(.init(name: "ManageTesla", family: .account, options: options,
                              effect: { $0.runner.count("setup-fleet") >= 1 }) { test, h, _ in
            XCTAssertEqual(h.window.connectButton.title, "Manage Tesla", "a connected account offers the menu")
            let menu = h.window.teslaAccountMenu()
            XCTAssertEqual(menu.items.filter { !$0.isSeparatorItem }.count, 4)
            let item = try XCTUnwrap(menu.items.first { $0.title == "Use Fleet API" })
            menu.performActionForItem(at: menu.index(of: item))
            let onboarding = try XCTUnwrap(test.currentOnboarding(h))
            XCTAssertEqual(h.rig.acquisitions(.account), 0)
            try test.submitFleet(onboarding)
        })
    }

    // MARK: 3. Service details page

    private func openPage(_ h: WindowOwnerRig) -> NSView? {
        h.window.showEmbeddedServiceDetails()
        return h.window.window?.contentView
    }

    func testEntryDetailsPageStartUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DetailsPageStart", family: .start, options: .init(loaded: .unloaded),
                              effect: { $0.service.count("run:start") >= 1 }) { test, h, _ in
            h.activate(h.detailsButton("hub.service.start-stop", in: test.openPage(h)))
        })
    }

    func testEntryDetailsPageStopUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DetailsPageStop", family: .stop, manualConflict: true,
                              effect: { $0.service.count("run:stop") >= 1 }) { test, h, conflict in
            h.activate(h.detailsButton("hub.service.start-stop", in: test.openPage(h)))
            XCTAssertTrue(h.hasSheet)
            conflict()
            h.confirm("Stop Hub")
        })
    }

    func testEntryDetailsPageRestartUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DetailsPageRestart", family: .restart,
                              effect: { $0.service.count("run:restart") >= 1 }) { test, h, _ in
            h.activate(h.detailsButton("hub.service.restart", in: test.openPage(h)))
        })
    }

    func testEntryDetailsPageRemoveUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DetailsPageRemove", family: .remove, manualConflict: true, presenter: true,
                              effect: { $0.service.count("run:remove") >= 1 }) { test, h, _ in
            h.activate(h.detailsButton("hub.service.remove", in: test.openPage(h)))
        })
    }

    func testEntryDetailsPageDeleteUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DetailsPageDelete", family: .delete, manualConflict: true,
                              effect: { $0.service.count("delete") >= 1 }) { test, h, _ in
            h.activate(h.detailsButton("hub.service.delete-data", in: test.openPage(h)))
        })
    }

    // MARK: 4. Service details legacy modal, including an already-open one

    private func openModal(_ h: WindowOwnerRig) throws -> NSView? {
        try XCTUnwrap(h.window.showServiceDetails(), "the legacy modal Details window").window?.contentView
    }

    func testEntryDetailsModalStartUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DetailsModalStart", family: .start, options: .init(loaded: .unloaded),
                              manualConflict: true, effect: { $0.service.count("run:start") >= 1 }) { test, h, conflict in
            let root = try test.openModal(h)
            conflict()   // the modal is already open when the owner appears
            h.activate(h.detailsButton("hub.service.start-stop", in: root))
        })
    }

    func testEntryDetailsModalStopUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DetailsModalStop", family: .stop, manualConflict: true,
                              effect: { $0.service.count("run:stop") >= 1 }) { test, h, conflict in
            let root = try test.openModal(h)
            h.activate(h.detailsButton("hub.service.start-stop", in: root))
            XCTAssertTrue(h.hasSheet)
            conflict()
            h.confirm("Stop Hub")
        })
    }

    func testEntryDetailsModalRestartUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DetailsModalRestart", family: .restart, manualConflict: true,
                              effect: { $0.service.count("run:restart") >= 1 }) { test, h, conflict in
            let root = try test.openModal(h)
            conflict()
            h.activate(h.detailsButton("hub.service.restart", in: root))
        })
    }

    func testEntryDetailsModalRemoveUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DetailsModalRemove", family: .remove, manualConflict: true, presenter: true,
                              effect: { $0.service.count("run:remove") >= 1 }) { test, h, _ in
            let root = try test.openModal(h)
            h.activate(h.detailsButton("hub.service.remove", in: root))
        })
    }

    func testEntryDetailsModalDeleteUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DetailsModalDelete", family: .delete, manualConflict: true,
                              effect: { $0.service.count("delete") >= 1 }) { test, h, _ in
            let root = try test.openModal(h)
            h.activate(h.detailsButton("hub.service.delete-data", in: root))
        })
    }

    /// The Details window's own gate (`mutationAllowed`) must hold when its enabled flag is stale: the flag is
    /// only refreshed on the window's notifications, so a custody that began since must still stop the press
    /// before it opens a confirmation or reaches a handler (both the embedded page and the legacy modal).
    func testDetailsMutationGateDoesNotRelyOnTheEnabledFlagAlone() throws {
        for embedded in [false, true] {
            let presenter = RecordingAlertPresenter()
            presenter.answer = .alertSecondButtonReturn
            let previous = HubUIPresentation.replaceAlertPresenterForTesting(presenter)
            defer { HubUIPresentation.replaceAlertPresenterForTesting(previous) }
            let h = try WindowOwnerRig(.init(loaded: .loaded))
            defer { h.tearDown() }
            h.answers = [.alertSecondButtonReturn, .alertSecondButtonReturn]
            var root: NSView?
            if embedded { h.window.showEmbeddedServiceDetails() } else { root = try openModal(h) }
            guard case let .success(foreign) = h.controller.lifecycle.acquire(.diagnostics) else {
                return XCTFail("a fresh lifecycle admits a diagnostics custody")
            }
            let details = try XCTUnwrap(h.window.detailsWindow, "the Details window is presented")
            details.setMutationsEnabled(true)   // stale: the owner appeared after the last refresh
            for identifier in ["hub.service.start-stop", "hub.service.restart", "hub.service.remove"] {
                h.activate(h.detailsButton(identifier, in: root))
            }
            spinBriefly(0.1)
            let label = embedded ? "embedded page" : "modal"
            XCTAssertFalse(h.hasSheet, "\(label): no confirmation sheet while another owner holds the lifecycle")
            XCTAssertEqual(presenter.confirmations.count, 0, "\(label): no remove confirmation")
            for call in ["run:start", "run:stop", "run:restart", "run:remove"] {
                XCTAssertEqual(h.service.count(call), 0, "\(label): \(call) was not issued")
            }
            h.controller.lifecycle.settle(foreign) {}
        }
    }

    // MARK: 5. Delete flow

    func testEntryDeleteFirstConfirmationUsesLifecycleOwner() throws {
        // Cancel is the default and does nothing.
        let cancelled = try WindowOwnerRig(.init(loaded: .loaded))
        defer { cancelled.tearDown() }
        cancelled.answers = [.alertFirstButtonReturn]
        cancelled.window.showEmbeddedServiceDetails()
        cancelled.activate(cancelled.detailsButton("hub.service.delete-data"))
        XCTAssertEqual(cancelled.alerts.count, 1)
        XCTAssertEqual(cancelled.alerts.first?.buttons.first?.keyEquivalent, "\r", "Cancel is the default")
        XCTAssertEqual(cancelled.service.count("delete"), 0)
        XCTAssertEqual(cancelled.rig.acquisitions(.delete), 0)

        // The conflict arrives while the first confirmation is open.
        try assertEntry(.init(name: "DeleteFirstConfirmation", family: .delete, manualConflict: true,
                              effect: { $0.service.count("delete") >= 1 }) { test, h, conflict in
            h.whileConfirming = { alert in
                if alert.messageText == "Delete Teslatlas Hub and all its data?" { conflict() }
            }
            h.window.showEmbeddedServiceDetails()
            h.activate(h.detailsButton("hub.service.delete-data"))
        })
    }

    func testEntryDeleteSecondConfirmationUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DeleteSecondConfirmation", family: .delete, manualConflict: true,
                              effect: { $0.service.count("delete") >= 1 }) { test, h, conflict in
            h.whileConfirming = { alert in
                if alert.messageText == "Permanently delete Hub data?" { conflict() }
            }
            h.window.showEmbeddedServiceDetails()
            h.activate(h.detailsButton("hub.service.delete-data"))
        })
    }

    func testEntryDeleteFinalQuitUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DeleteFinalQuit", family: .delete,
                              effect: { $0.terminateCalls == 1 && $0.revealed.count == 1 }) { test, h, _ in
            h.window.showEmbeddedServiceDetails()
            h.activate(h.detailsButton("hub.service.delete-data"))
            spinUntil(5) { h.terminateCalls == 1 || h.errors.count > 0 }
            if h.terminateCalls == 1 {
                XCTAssertEqual(h.alerts.last?.buttons.map(\.title), ["Quit"], "the completed Delete's only action")
            }
        })
        // After the completed Delete the lifecycle stays deleted: the Quit is the way out.
        let done = try WindowOwnerRig(.init(loaded: .loaded))
        defer { done.tearDown() }
        done.answers = [.alertSecondButtonReturn, .alertSecondButtonReturn, .alertFirstButtonReturn]
        done.window.showEmbeddedServiceDetails()
        done.activate(done.detailsButton("hub.service.delete-data"))
        spinUntil(5) { done.terminateCalls == 1 }
        XCTAssertEqual(done.controller.lifecycleCapabilities.state, .deleted)
        XCTAssertFalse(done.window.operationPreventsQuit, "the narrow completion-only exemption")
    }

    // MARK: 6. Account menu

    private func connectedOptions() -> OwnerRig.Options {
        var options = OwnerRig.Options(loaded: .loaded)
        options.runningStatus = true
        return options
    }

    private func chooseMenuItem(_ title: String, _ h: WindowOwnerRig) throws {
        let menu = h.window.teslaAccountMenu()
        let item = try XCTUnwrap(menu.items.first { $0.title == title }, "no menu item \(title)")
        menu.performActionForItem(at: menu.index(of: item))
    }

    func testEntryAccountUseFleetUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "AccountUseFleet", family: .account, options: connectedOptions(),
                              effect: { $0.runner.count("setup-fleet") >= 1 }) { test, h, _ in
            try test.chooseMenuItem("Use Fleet API", h)
            let onboarding = try XCTUnwrap(test.currentOnboarding(h))
            XCTAssertEqual(onboarding.currentRoute, .fleet)
            XCTAssertEqual(h.rig.acquisitions(.account), 0, "the menu choice only presented the page")
            try test.submitFleet(onboarding)
        })
    }

    func testEntryAccountUseLegacyUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "AccountUseLegacy", family: .account, options: connectedOptions(),
                              effect: { $0.runner.count("setup") >= 1 }) { test, h, _ in
            try test.chooseMenuItem("Use Legacy token", h)
            let onboarding = try XCTUnwrap(test.currentOnboarding(h))
            XCTAssertEqual(onboarding.currentRoute, .legacy)
            XCTAssertEqual(h.rig.acquisitions(.account), 0)
            try test.submitLegacyToken(onboarding)
        })
    }

    func testEntryAccountMigrateUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "AccountMigrate", family: .migration, options: connectedOptions(),
                              effect: { $0.runner.count("teslamate-check:history") >= 1 }) { test, h, _ in
            try test.chooseMenuItem("Migrate from TeslaMate…", h)
            let onboarding = try XCTUnwrap(test.currentOnboarding(h))
            XCTAssertEqual(onboarding.currentRoute, .migration)
            XCTAssertEqual(h.rig.acquisitions(.migration), 0)
            try test.submitLocalCompatibility(onboarding)
        })
    }

    func testEntryAccountDisconnectUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "AccountDisconnect", family: .signOut, options: connectedOptions(),
                              manualConflict: true, presenter: true,
                              effect: { $0.runner.count("control") >= 1 }) { test, h, _ in
            h.send("disconnectTeslaPressed")
        })
    }

    // MARK: 7. Account setup submissions and OAuth completion

    func testEntryFleetFirstRunUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "FleetFirstRun", family: .account, options: .init(installed: false),
                              effect: { $0.runner.count("setup-fleet") >= 1 }) { test, h, _ in
            let onboarding = try test.openOnboarding(h, route: .fleet)
            try test.submitFleet(onboarding)
        })
    }

    func testEntryFleetReconfigurationUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "FleetReconfiguration", family: .account,
                              effect: { $0.runner.count("setup-fleet") >= 1 && $0.service.calls.contains("run:pause") }) { test, h, _ in
            let onboarding = try test.openOnboarding(h, route: .fleet)
            try test.submitFleet(onboarding)
        })
    }

    func testEntryLegacyTokenFirstRunUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "LegacyTokenFirstRun", family: .account, options: .init(installed: false),
                              effect: { $0.runner.count("setup") >= 1 }) { test, h, _ in
            let onboarding = try test.openOnboarding(h, route: .legacy)
            try test.submitLegacyToken(onboarding)
        })
    }

    func testEntryLegacyTokenReconfigurationUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "LegacyTokenReconfiguration", family: .account,
                              effect: { $0.runner.count("setup") >= 1 && $0.service.calls.contains("run:pause") }) { test, h, _ in
            let onboarding = try test.openOnboarding(h, route: .legacy)
            try test.submitLegacyToken(onboarding)
        })
    }

    func testEntryLegacyOAuthFirstRunUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "LegacyOAuthFirstRun", family: .account, options: .init(installed: false),
                              effect: { $0.runner.count("setup") >= 1 }) { test, h, _ in
            // The authentication window needs the network: the controller-owned context the
            // window uses is driven directly, from before authentication to its completion.
            guard case let .success(signIn) = h.controller.beginLegacySignIn() else { return }
            XCTAssertEqual(h.controller.lifecycleCapabilities.owner, .account)
            signIn.configure(tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")) { _ in }
        })
    }

    func testEntryLegacyOAuthReconfigurationUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "LegacyOAuthReconfiguration", family: .account,
                              effect: { $0.runner.count("setup") >= 1 && $0.service.calls.contains("run:pause") }) { test, h, _ in
            guard case let .success(signIn) = h.controller.beginLegacySignIn() else { return }
            signIn.configure(tokens: TeslaAuthTokens(accessToken: "access", refreshToken: "refresh")) { _ in }
        })
    }

    // MARK: 8. Import entries, followed by a submission

    func testEntryFileImportUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "FileImport", family: .migration, options: .init(installed: false),
                              effect: { $0.runner.count("teslamate-check:history") >= 1 }) { test, h, _ in
            // File > Import TeslaMate Data… is the application delegate's selector.
            let menu = AppDelegate.makeMainMenu(actionTarget: nil)
            let item = try XCTUnwrap(menu.items.compactMap(\.submenu).flatMap(\.items)
                .first { $0.action == #selector(AppDelegate.importData(_:)) }, "the File menu's import item")
            XCTAssertEqual(item.title, "Import TeslaMate Data…")
            h.window.showImport()
            let onboarding = try XCTUnwrap(test.currentOnboarding(h), "Import opened onboarding")
            XCTAssertEqual(h.rig.acquisitions(.migration), 0)
            try test.submitLocalCompatibility(onboarding)
        })
    }

    func testEntrySettingsImportUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "SettingsImport", family: .migration, options: .init(installed: false),
                              effect: { $0.runner.count("teslamate-check:history") >= 1 }) { test, h, _ in
            h.window.selectMainSection(.settings)
            let row = try XCTUnwrap(test.descendantViews(of: h.window.settingsPage)
                .first { $0.identifier?.rawValue == "hub.settings.import-teslamate" }, "the Settings import row")
            if let control = row as? NSControl, let action = control.action {
                _ = NSApp.sendAction(action, to: control.target, from: control)
            } else if let button = test.descendantViews(of: row).compactMap({ $0 as? NSButton }).first {
                h.activate(button)
            } else {
                h.window.showImport()
            }
            let onboarding = try XCTUnwrap(test.currentOnboarding(h), "Settings Import opened onboarding")
            XCTAssertEqual(h.rig.acquisitions(.migration), 0)
            try test.submitLocalCompatibility(onboarding)
        })
    }

    func testEntryAccountImportUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "AccountImport", family: .migration, options: .init(installed: false),
                              effect: { $0.runner.count("teslamate-check:history") >= 1 }) { test, h, _ in
            try test.chooseMenuItem("Migrate from TeslaMate…", h)
            let onboarding = try XCTUnwrap(test.currentOnboarding(h))
            XCTAssertEqual(h.rig.acquisitions(.migration), 0)
            try test.submitLocalCompatibility(onboarding)
        })
    }

    // MARK: 9. Migration stages driven from the onboarding window

    func testEntryLocalCompatibilityUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "LocalCompatibility", family: .migration, options: .init(installed: false),
                              effect: { $0.runner.count("teslamate-check:history") >= 1 }) { test, h, _ in
            let onboarding = try test.openOnboarding(h, route: .migration)
            try test.submitLocalCompatibility(onboarding)
        })
    }

    func testEntryLocalImportUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "LocalImport", family: .migration, options: .init(installed: false),
                              effect: { $0.runner.count("migrate:history") >= 1 }) { test, h, _ in
            let onboarding = try test.openOnboarding(h, route: .migration)
            try test.submitLocalCompatibility(onboarding)       // the check: compatible
            h.rig.drain()
            spinBriefly(0.1)
            // Once the report says compatible the same button imports (it reads Import History).
            if let importButton = test.findButton("Import History", in: onboarding) {
                test.press(importButton)
            } else {
                test.press(try test.primaryButton("Check Snapshot", in: onboarding))
            }
        })
    }

    func testEntrySSHPreparationUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "SSHPreparation", family: .migration, options: .init(installed: false),
                              effect: { $0.rig.connector.callCount >= 1 }) { test, h, _ in
            let onboarding = try test.openOnboarding(h, route: .migration)
            try test.fillServerField(in: onboarding, host: "tm.example")
            onboarding.sendConnect()
        })
    }
}
