// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

/// R-E1F6-18, continued: diagnostics, vehicle control, preference/restoration and close
/// routes, and the residual import sheet.
extension OnboardingWindowControllerTests {
    private func runButton(in root: NSView?, _ h: WindowOwnerRig) -> NSButton? {
        h.buttons(in: root).first { $0.title == "Run Again" }
    }

    private func connected() -> OwnerRig.Options {
        var options = OwnerRig.Options(loaded: .loaded)
        options.runningStatus = true
        return options
    }

    // MARK: 12. Diagnostics entries and their Run actions

    func testEntryDashboardDiagnosticsUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DashboardDiagnostics", family: .diagnostics,
                              effect: { $0.service.count("run:pause") >= 1 }) { test, h, _ in
            h.activate(h.dashboardButton("Run Diagnostics"))
            XCTAssertEqual(h.rig.acquisitions(.diagnostics), 0, "opening the page reserved nothing")
            h.activate(test.runButton(in: h.window.window?.contentView, h))
        })
    }

    func testEntrySettingsDiagnosticsUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "SettingsDiagnostics", family: .diagnostics,
                              effect: { $0.service.count("run:pause") >= 1 }) { test, h, _ in
            h.window.selectMainSection(.settings)
            let row = try XCTUnwrap(h.buttons(in: h.window.settingsPage).first { $0.accessibilityLabel() == "Diagnostics" },
                                    "the Settings Diagnostics row")
            h.activate(row)
            XCTAssertEqual(h.rig.acquisitions(.diagnostics), 0)
            h.activate(test.runButton(in: h.window.window?.contentView, h))
        })
    }

    func testEntryMenuDiagnosticsUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "MenuDiagnostics", family: .diagnostics,
                              effect: { $0.service.count("run:pause") >= 1 }) { test, h, _ in
            // View > Diagnostics is the application delegate's selector; its own window follows.
            let delegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { _ in },
                                       terminateApplication: { _ in }, applicationWindows: { _ in [] })
            delegate.hubController = h.controller
            delegate.showDiagnostics(nil)
            let main = try XCTUnwrap(Mirror(reflecting: delegate).descendant("mainWindowController") as? MainWindowController)
            defer { main.close() }
            // The new window's first status read has to finish before it offers a page.
            spinUntil(5) { (Mirror(reflecting: main).descendant("initialRefreshPending") as? Bool) == false }
            main.showEmbeddedDiagnostics()
            XCTAssertEqual(h.rig.acquisitions(.diagnostics), 0, "the menu choice only presented the page")
            h.activate(test.runButton(in: main.window?.contentView, h))
        })
    }

    func testEntryEmbeddedDiagnosticsRunUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "EmbeddedDiagnosticsRun", family: .diagnostics,
                              effect: { $0.service.count("run:pause") >= 1 }) { test, h, _ in
            h.window.showEmbeddedDiagnostics()
            h.activate(test.runButton(in: h.window.window?.contentView, h))
        })
    }

    func testEntryModalDiagnosticsRunUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "ModalDiagnosticsRun", family: .diagnostics,
                              effect: { $0.service.count("run:pause") >= 1 }) { test, h, _ in
            let modal = try XCTUnwrap(h.window.showDiagnostics(), "the legacy modal Diagnostics window")
            h.activate(test.runButton(in: modal.window?.contentView, h))
        })
    }

    func testEntryLogsDiagnosticsRunUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "LogsDiagnosticsRun", family: .diagnostics,
                              effect: { $0.service.count("run:pause") >= 1 }) { test, h, _ in
            let logs = LogsWindowController(controller: h.controller)
            let item = try XCTUnwrap(logs.secondaryActionsMenu.items.first { $0.title == "Run Diagnostics" })
            spinUntil(3) { item.isEnabled || h.controller.lifecycleCapabilities.owner != nil }
            // The Logs window's own selector; its enabled state does not stand in for admission.
            if let action = item.action { _ = NSApp.sendAction(action, to: item.target, from: item) }
            _ = logs
        })
    }

    // MARK: 13. Vehicle control, recording effects only

    private func vehicleCard(in root: NSView?, _ test: OnboardingWindowControllerTests) throws -> HubVehicleCardView {
        try XCTUnwrap(test.descendantViews(of: root).compactMap { $0 as? HubVehicleCardView }.first, "a vehicle card")
    }

    private func commandItem(_ action: HubVehicleControl) -> NSMenuItem {
        let item = NSMenuItem(title: action.title, action: nil, keyEquivalent: "")
        item.representedObject = action.rawValue
        return item
    }

    func testEntryDashboardVehicleControlUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "DashboardVehicleControl", family: .vehicleControl, options: connected(),
                              manualConflict: true, effect: { $0.runner.count("control") >= 1 }) { test, h, conflict in
            let card = try XCTUnwrap(Mirror(reflecting: h.window.dashboardView!).descendant("vehicleCard") as? HubVehicleCardView)
            _ = card.perform(NSSelectorFromString("runCommand:"), with: test.commandItem(.lock))
            XCTAssertTrue(h.hasSheet, "the command confirmation opened")
            conflict()
            h.confirm(HubVehicleControl.lock.title)
        })
    }

    func testEntryVehiclesPageControlUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "VehiclesPageControl", family: .vehicleControl, options: connected(),
                              manualConflict: true, effect: { $0.runner.count("control") >= 1 }) { test, h, conflict in
            h.window.selectMainSection(.vehicles)
            let card = try XCTUnwrap(Mirror(reflecting: h.window.vehiclesView!).descendant("detailCard") as? HubVehicleCardView)
            _ = card.perform(NSSelectorFromString("runCommand:"), with: test.commandItem(.unlock))
            XCTAssertTrue(h.hasSheet)
            conflict()
            h.confirm(HubVehicleControl.unlock.title)
        })
    }

    func testEntryLegacyVehicleSelectorUsesLifecycleOwner() throws {
        try assertEntry(.init(name: "LegacyVehicleSelector", family: .vehicleControl, options: connected(),
                              manualConflict: true, effect: { $0.runner.count("control") >= 1 }) { test, h, conflict in
            let button = NSButton(title: "Lock", target: nil, action: nil)
            button.identifier = NSUserInterfaceItemIdentifier(HubVehicleControl.lock.rawValue)
            conflict()    // the legacy selector has no confirmation of its own to wait for
            _ = h.window.perform(NSSelectorFromString("vehicleCardButtonPressed:"), with: button)
            if h.hasSheet { h.confirm(HubVehicleControl.lock.title) }
        })
    }

    // MARK: 16. Preference and restoration barrier

    private func barrierRoute(_ name: String, probe: @escaping (WindowOwnerRig) -> Bool) throws {
        var options = OwnerRig.Options(loaded: .loaded)
        options.realBarrier = true
        let harness = try WindowOwnerRig(options)
        defer { harness.tearDown() }
        HubWindowRestoration.endSuppressing()
        XCTAssertTrue(probe(harness), "\(name): admitted when open")
        harness.service.hold("delete")
        harness.controller.deleteHubAndData { _ in }
        spinUntil { harness.service.heldKeys.contains("delete") }
        XCTAssertFalse(probe(harness), "\(name): refused during Delete")
        harness.service.release("delete")
        spinUntil { harness.controller.hubDeletionCompleted }
        XCTAssertFalse(probe(harness), "\(name): refused after Delete")
    }

    func testEntryAppearanceUsesLifecycleOwner() throws {
        try barrierRoute("Appearance") { harness in
            let before = harness.appearanceWrites.count
            harness.send("appearancePressed")
            return harness.appearanceWrites.count == before + 1
        }
    }

    func testEntryFrameAutosaveUsesLifecycleOwner() throws {
        try barrierRoute("FrameAutosave") { _ in
            let window = HubUtilityWindowStyle.makeWindow(title: "autosave \(UUID())", size: NSSize(width: 300, height: 200),
                                                          minimum: NSSize(width: 100, height: 100))
            defer { window.close() }
            // The window factory carries a frame autosave name only when writes are admitted
            // and the host is not a test host; the barrier is what the factory consults.
            return !HubWindowRestoration.isSuppressing && window.isRestorable
        }
    }

    func testEntryWindowRestorationUsesLifecycleOwner() throws {
        try barrierRoute("WindowRestoration") { harness in
            let sheet = HubOnboardingSheetStyle.makeWindow(contentSize: NSSize(width: 400, height: 300), dismissible: true)
            defer { sheet.close() }
            return sheet.isRestorable
        }
    }

    // MARK: 10. Residual import sheet

    func testEntryResidualImportSheetUsesLifecycleOwner() throws {
        try testResidualImportSheetCannotBypassLifecycleAdmission()
    }

    // MARK: 18. Close routes

    private func closeFixture() throws -> WindowOwnerRig {
        try WindowOwnerRig(.init(loaded: .loaded))
    }

    /// Disconnect Tesla and a menu-driven vehicle command are guarded by the window before any confirmation
    /// opens: with another owner already holding the lifecycle (and seen by the window) nothing opens, nothing
    /// is shown and nothing reaches the controller. The control half proves the same presses do open them.
    func testDisconnectAndMenuVehicleCommandOpenNothingWhileAnotherOwnerHoldsTheLifecycle() throws {
        let vehicleID = try XCTUnwrap(UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5"))
        for conflict in [false, true] {
            let presenter = RecordingAlertPresenter()
            presenter.answer = .alertFirstButtonReturn   // Cancel
            let previous = HubUIPresentation.replaceAlertPresenterForTesting(presenter)
            defer { HubUIPresentation.replaceAlertPresenterForTesting(previous) }
            let h = try WindowOwnerRig(connected())
            defer { h.tearDown() }
            spinUntil(5) { h.privateValue("initialRefreshPending", as: Bool.self) == false }
            var foreign: HubLifecycleToken?
            if conflict {
                guard case let .success(token) = h.controller.lifecycle.acquire(.diagnostics) else { return XCTFail("no foreign owner") }
                foreign = token
                spinBriefly(0.15)   // the window's capability snapshot catches up with the owner
            }
            h.send("disconnectTeslaPressed")
            h.window.vehicleCommand(.lock, vehicleID: vehicleID)
            spinBriefly(0.1)
            if conflict {
                XCTAssertEqual(presenter.confirmations.count, 0, "no Disconnect confirmation while another owner holds the lifecycle")
                XCTAssertFalse(h.hasSheet, "no vehicle confirmation either")
                XCTAssertTrue(h.errors.isEmpty && presenter.errors.isEmpty, "and no error: the presses were refused quietly")
                XCTAssertEqual(h.runner.count("control"), 0)
                if let foreign { h.controller.lifecycle.settle(foreign) {} }
            } else {
                XCTAssertEqual(presenter.confirmations.count, 1, "control: Disconnect asks first")
                XCTAssertTrue(h.hasSheet, "control: the vehicle command asks first")
                h.confirm("Cancel")
            }
        }
    }

    func testEntryWindowMenuCloseUsesLifecycleOwner() throws {
        let h = try closeFixture()
        defer { h.tearDown() }
        let logs = try startLogsDiagnostics(h, holding: "pause")
        defer { _ = logs }
        let mainWindow = try XCTUnwrap(h.window.window)
        let delegate = AppDelegate(keyWindow: { mainWindow })
        delegate.hubController = h.controller
        let item = NSMenuItem()
        item.action = #selector(AppDelegate.closeKeyWindow(_:))
        XCTAssertFalse(delegate.validateMenuItem(item), "Close is unavailable while a protected operation runs")
        var closed = false
        let observer = NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: mainWindow,
                                                              queue: nil) { _ in closed = true }
        defer { NotificationCenter.default.removeObserver(observer) }
        delegate.closeKeyWindow(nil)
        XCTAssertFalse(closed, "Window > Close consulted the window's close policy and the window stayed open")
        XCTAssertEqual(h.rig.releases(.diagnostics), 0, "the Window menu's Close released nothing")
        h.rig.drain()
        XCTAssertTrue(delegate.validateMenuItem(item), "and is available again afterwards")
    }

    func testEntryMainCloseUsesLifecycleOwner() throws {
        let h = try closeFixture()
        defer { h.tearDown() }
        let logs = try startLogsDiagnostics(h, holding: "pause")
        defer { _ = logs }
        let mainWindow = try XCTUnwrap(h.window.window)
        XCTAssertFalse(h.window.windowShouldClose(mainWindow), "the main window refuses while its protected operation is active")
        XCTAssertEqual(h.rig.releases(.diagnostics), 0)
        h.rig.drain()
        XCTAssertTrue(h.window.windowShouldClose(mainWindow))
    }

    func testEntryOnboardingCloseUsesLifecycleOwner() throws {
        let h = try closeFixture()
        defer { h.tearDown() }
        let onboarding = try XCTUnwrap(h.window.showOnboarding(route: .provider))
        let sheet = try XCTUnwrap(onboarding.window)
        onboarding.setBusy(true)
        XCTAssertFalse(onboarding.windowShouldClose(sheet), "busy onboarding refuses to close")
        onboarding.setBusy(false)
        h.service.hold("pause")
        h.controller.configureFleetAccount(credentials: HubFleetSetupCredentials(
            accessToken: "a", refreshToken: "r", clientID: "c",
            region: "europe_middle_east_and_africa", expiresInSeconds: 3600)) { _ in }
        XCTAssertTrue(onboarding.windowShouldClose(sheet), "an idle onboarding window closes")
        sheet.close()
        XCTAssertEqual(h.controller.lifecycleCapabilities.owner, .account, "the owner outlived the window")
        XCTAssertEqual(h.rig.releases(.account), 0)
        h.rig.drain()
        XCTAssertEqual(h.rig.releases(.account), 1)
    }

    func testEntryDiagnosticsCloseUsesLifecycleOwner() throws {
        let h = try closeFixture()
        defer { h.tearDown() }
        h.service.hold("pause")
        let diagnostics = DiagnosticsWindowController(controller: h.controller)
        let run = try XCTUnwrap(h.buttons(in: diagnostics.window?.contentView).first { $0.title == "Run Again" })
        spinUntil(2) { run.isEnabled }
        h.activate(run)
        let window = try XCTUnwrap(diagnostics.window)
        XCTAssertFalse(diagnostics.windowShouldClose(window), "the Diagnostics window refuses during its run")
        XCTAssertEqual(h.rig.releases(.diagnostics), 0)
        h.rig.drain()
        spinUntil(3) { diagnostics.windowShouldClose(window) }
        XCTAssertTrue(diagnostics.windowShouldClose(window))
        diagnostics.close()
    }

    func testEntryDetailsCloseUsesLifecycleOwner() throws {
        let h = try closeFixture()
        defer { h.tearDown() }
        let logs = try startLogsDiagnostics(h, holding: "pause")
        defer { _ = logs }
        let details = try XCTUnwrap(h.window.showServiceDetails())
        XCTAssertTrue(details.windowShouldClose(try XCTUnwrap(details.window)), "Details may close")
        details.window?.close()
        XCTAssertEqual(h.controller.lifecycleCapabilities.owner, .diagnostics, "controller-owned work continues")
        XCTAssertEqual(h.rig.releases(.diagnostics), 0)
        h.rig.drain()
        XCTAssertEqual(h.rig.releases(.diagnostics), 1)
    }

    func testEntryLogsCloseUsesLifecycleOwner() throws {
        let h = try closeFixture()
        defer { h.tearDown() }
        let logs = try startLogsDiagnostics(h, holding: "pause")
        logs.window?.close()
        XCTAssertEqual(h.controller.lifecycleCapabilities.owner, .diagnostics, "closing Logs leaves the custody held")
        XCTAssertEqual(h.rig.releases(.diagnostics), 0)
        h.rig.drain()
        XCTAssertEqual(h.rig.releases(.diagnostics), 1, "only the controller's settlement released it")
    }
}
