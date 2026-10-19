// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

/// Dock Quit and the application menu both enter NSApplication.terminate(_:).
/// Handle sheets here, before AppKit can silently defer termination for them.
final class HubApplication: NSApplication {
    /// The decision shared by the application menu's Quit and the Dock's: windows and the
    /// application controller's lifecycle owner, in that one place (R-E1F6-16).
    static func mayTerminate(windows: [NSWindow], controller: HubController?) -> Bool {
        AppDelegate.finishSheetsBeforeQuit(in: windows,
                                           lifecycle: controller?.lifecycleCapabilities ?? .idle)
    }

    /// What the alert of a refused Quit says. A child process that failed but cannot be shown gone
    /// holds the owner (and so refuses Quit, and a logout or restart of the Mac) until it exits: that
    /// state has its own words (F7-4).
    static func quitRefusalText(for lifecycle: HubLifecycleCapabilities) -> (title: String, detail: String) {
        if lifecycle.childHeld {
            return (HubL10n.text("hub.lifecycle.quit_held_title", fallback: "Hub is waiting for a helper process"),
                    HubL10n.text("hub.lifecycle.quit_held_detail", fallback: "Hub is waiting for a stuck helper process to exit, so it cannot quit yet. Wait a moment, then try again. Quitting the app does not stop the background Hub service."))
        }
        return (HubL10n.text("hub.AppDelegate.12.1", fallback: "Hub is finishing an operation"),
         HubL10n.text("hub.AppDelegate.13.2", fallback: "Wait for the current setup, service, vehicle, or diagnostics operation to finish, then quit. Quitting the app does not stop the background Hub service."))
    }

    override func terminate(_ sender: Any?) {
        guard Self.mayTerminate(windows: windows, controller: (delegate as? AppDelegate)?.hubController) else {
            HubApplicationActivation.activate()
            let alert = NSAlert()
            let text = Self.quitRefusalText(for: (delegate as? AppDelegate)?.hubController?.lifecycleCapabilities ?? .idle)
            alert.messageText = text.title
            alert.informativeText = text.detail
            alert.addButton(withTitle: HubL10n.text("hub.AppDelegate.14.3", fallback: "OK"))
            _ = HubUIPresentation.response(to: alert)
            return
        }
        super.terminate(sender)
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation {
    private static let windowMenuTag = 104
    private var mainWindowController: MainWindowController?
    /// The application controller. Tests may set it to reach `applicationShouldTerminate`
    /// without replacing global application state.
    var hubController: HubController!
    private let keyWindow: () -> NSWindow?
    private let replyToTerminate: (Bool) -> Void
    private let terminateApplication: (Any?) -> Void
    private let applicationWindows: ((NSApplication) -> [NSWindow])?

    init(keyWindow: @escaping () -> NSWindow? = { NSApp.keyWindow },
         replyToTerminate: @escaping (Bool) -> Void = HubProductionEffects.terminationReply(),
         terminateApplication: @escaping (Any?) -> Void = { NSApp.terminate($0) },
         applicationWindows: ((NSApplication) -> [NSWindow])? = nil) {
        self.keyWindow = keyWindow
        self.replyToTerminate = replyToTerminate
        self.terminateApplication = terminateApplication
        self.applicationWindows = applicationWindows
        super.init()
    }

    func applicationWillTerminate(_ notification: Notification) {
        if let hubController {
            hubController.completeClosing()
        } else {
            HubStatusProcessLifecycle.shared.shutdown()
        }
        DevelopmentEventLog.drainAll()
    }

    func applicationWillFinishLaunching(_ notification: Notification) {
        HubApplicationActivation.yieldFocus()
        NSApp.mainMenu = Self.makeMainMenu(actionTarget: self)
        NSApp.windowsMenu = NSApp.mainMenu?.item(withTag: Self.windowMenuTag)?.submenu
    }

    static func makeMainMenu(actionTarget: AnyObject? = nil) -> NSMenu {
        let mainMenu = NSMenu()

        let applicationItem = NSMenuItem()
        let applicationMenu = NSMenu()
        applicationItem.submenu = applicationMenu
        mainMenu.addItem(applicationItem)
        applicationMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.44.5", fallback: "About Teslatlas Hub"),
                                action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)),
                                keyEquivalent: "")
        let legal = applicationMenu.addItem(
            withTitle: HubL10n.text("hub.AppDelegate.48.6", fallback: "Legal & Licence…"),
            action: #selector(showLegalNotice(_:)),
            keyEquivalent: ""
        )
        legal.target = actionTarget
        if HubRelease.bundledSourceCommit != nil {
            let source = applicationMenu.addItem(
                withTitle: HubL10n.text("hub.AppDelegate.55.7", fallback: "Corresponding Source…"),
                action: #selector(openCorrespondingSource(_:)),
                keyEquivalent: ""
            )
            source.target = actionTarget
        }
        applicationMenu.addItem(.separator())
        applicationMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.62.8", fallback: "Hide Teslatlas Hub"),
                                action: #selector(NSApplication.hide(_:)),
                                keyEquivalent: "h")
        let hideOthers = applicationMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.65.10", fallback: "Hide Others"),
                                                 action: #selector(NSApplication.hideOtherApplications(_:)),
                                                 keyEquivalent: "h")
        hideOthers.keyEquivalentModifierMask = [.command, .option]
        applicationMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.69.12", fallback: "Show All"),
                                action: #selector(NSApplication.unhideAllApplications(_:)),
                                keyEquivalent: "")
        applicationMenu.addItem(.separator())
        let quit = applicationMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.73.13", fallback: "Quit Teslatlas Hub"),
                                           action: #selector(quitApplication(_:)),
                                           keyEquivalent: "q")
        quit.target = actionTarget

        let editItem = NSMenuItem()
        let editMenu = NSMenu(title: HubL10n.text("hub.AppDelegate.79.15", fallback: "Edit"))
        editItem.submenu = editMenu
        mainMenu.addItem(editItem)
        editMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.82.16", fallback: "Undo"), action: Selector(("undo:")), keyEquivalent: "z")
        let redo = editMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.83.19", fallback: "Redo"), action: Selector(("redo:")), keyEquivalent: "Z")
        redo.keyEquivalentModifierMask = [.command, .shift]
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.86.22", fallback: "Cut"), action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        editMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.87.24", fallback: "Copy"), action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        editMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.88.26", fallback: "Paste"), action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        editMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.89.28", fallback: "Delete"), action: #selector(NSText.delete(_:)), keyEquivalent: "")
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.91.29", fallback: "Select All"), action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")

        let fileItem = NSMenuItem()
        let fileMenu = NSMenu(title: HubL10n.text("hub.AppDelegate.94.31", fallback: "File"))
        fileItem.submenu = fileMenu
        mainMenu.addItem(fileItem)
        let importData = fileMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.97.32", fallback: "Import TeslaMate Data…"),
                                          action: #selector(importData(_:)),
                                          keyEquivalent: "i")
        importData.target = actionTarget

        let viewItem = NSMenuItem()
        let viewMenu = NSMenu(title: HubL10n.text("hub.AppDelegate.103.34", fallback: "View"))
        viewItem.submenu = viewMenu
        mainMenu.addItem(viewItem)
        let overview = viewMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.106.35", fallback: "Overview"),
                                        action: #selector(showOverview(_:)),
                                        keyEquivalent: "1")
        overview.target = actionTarget
        let vehicles = viewMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.110.36", fallback: "Vehicles"),
                                        action: #selector(showVehicles(_:)),
                                        keyEquivalent: "2")
        vehicles.target = actionTarget
        let activity = viewMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.114.37", fallback: "Activity"),
                                        action: #selector(showActivity(_:)),
                                        keyEquivalent: "3")
        activity.target = actionTarget
        let settings = viewMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.118.38", fallback: "Settings"),
                                        action: #selector(showSettings(_:)),
                                        keyEquivalent: "4")
        settings.target = actionTarget
        viewMenu.addItem(.separator())
        let diagnostics = viewMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.123.39", fallback: "Diagnostics"),
                                           action: #selector(showDiagnostics(_:)),
                                           keyEquivalent: "d")
        diagnostics.keyEquivalentModifierMask = [.command, .shift]
        diagnostics.target = actionTarget
        let logs = viewMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.128.41", fallback: "Activity & Logs"),
                                    action: #selector(showLogs(_:)),
                                    keyEquivalent: "l")
        logs.target = actionTarget
        let service = viewMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.132.43", fallback: "Service Details"),
                                       action: #selector(showServiceDetails(_:)),
                                       keyEquivalent: "i")
        service.keyEquivalentModifierMask = [.command, .option]
        service.target = actionTarget

        let windowItem = NSMenuItem()
        windowItem.tag = windowMenuTag
        let windowMenu = NSMenu(title: HubL10n.text("hub.AppDelegate.139.45", fallback: "Window"))
        windowItem.submenu = windowMenu
        mainMenu.addItem(windowItem)
        // AppKit disables performClose: for attached sheets before asking the
        // window to validate it. Route Close explicitly to the key window so an
        // idle account sheet can use its guarded Cancel action instead.
        let close = windowMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.145.46", fallback: "Close"), action: #selector(closeKeyWindow(_:)), keyEquivalent: "w")
        close.target = actionTarget
        windowMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.147.48", fallback: "Minimize"), action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
        windowMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.148.50", fallback: "Zoom"), action: #selector(NSWindow.performZoom(_:)), keyEquivalent: "")

        let helpItem = NSMenuItem()
        let helpMenu = NSMenu(title: HubL10n.text("hub.AppDelegate.151.51", fallback: "Help"))
        helpItem.submenu = helpMenu
        mainMenu.addItem(helpItem)
        let help = helpMenu.addItem(withTitle: HubL10n.text("hub.AppDelegate.154.52", fallback: "Teslatlas Hub Help"),
                                    action: #selector(showHelp(_:)),
                                    keyEquivalent: "?")
        help.target = actionTarget

        return mainMenu
    }

    @objc func closeKeyWindow(_ sender: Any?) {
        guard let window = keyWindow() else { return }
        if let onboarding = window.windowController as? OnboardingWindowController {
            onboarding.cancelOperation(sender)
        } else {
            window.performClose(sender)
        }
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(importData(_:)) {
            return mainWindowController?.canImportTeslaMate == true
        }
        let navigationActions: [Selector] = [#selector(showOverview(_:)), #selector(showVehicles(_:)),
            #selector(showActivity(_:)), #selector(showSettings(_:)), #selector(showDiagnostics(_:)),
            #selector(showLogs(_:)), #selector(showServiceDetails(_:))]
        if let action = menuItem.action, navigationActions.contains(action) {
            return mainWindowController?.navigationAvailable == true
        }
        guard menuItem.action == #selector(closeKeyWindow(_:)) else { return true }
        guard let window = keyWindow() else { return false }
        if let onboarding = window.windowController as? OnboardingWindowController {
            return onboarding.canCancel
        }
        if let main = window.windowController as? MainWindowController,
           main.operationPreventsQuit {
            return false
        }
        return window.styleMask.contains(.closable) && window.attachedSheet == nil
    }

    @objc func quitApplication(_ sender: Any?) {
        terminateApplication(sender)
    }

    /// NSApplication's standard termination can be suppressed by an attached
    /// sheet. End idle sheets explicitly; don't interrupt an owned operation.
    static func finishSheetsBeforeQuit(in windows: [NSWindow],
                                       lifecycle: HubLifecycleCapabilities) -> Bool {
        // The lifecycle owner first: a foreground owner refuses whether or not a window
        // shows it (sign-out, diagnostics from a closed Logs page, account work...).
        guard !lifecycle.preventsQuit else { return false }
        guard !windows.contains(where: {
            ($0.delegate as? OnboardingWindowController)?.operationPreventsQuit == true
                || ($0.windowController as? MainWindowController)?.operationPreventsQuit == true
                || ($0.windowController as? DiagnosticsWindowController)?.operationInProgress == true
        }) else { return false }
        func depth(_ window: NSWindow) -> Int {
            var count = 0
            var parent = window.sheetParent
            while let current = parent { count += 1; parent = current.sheetParent }
            return count
        }
        let sheets = windows.filter { $0.sheetParent != nil }.sorted { depth($0) > depth($1) }
        for sheet in sheets {
            sheet.sheetParent?.endSheet(sheet, returnCode: .cancel)
            sheet.close()
        }
        return true
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        HubApplicationActivation.yieldFocus()
        guard !HubUIPresentation.isSilentTestHost else { return }
        do {
            hubController = try HubController.applicationController()
        } catch {
            let alert = NSAlert()
            alert.alertStyle = .critical
            alert.messageText = HubL10n.text("hub.AppDelegate.227.53", fallback: "Local Hub development configuration was rejected")
            alert.informativeText = error.localizedDescription
            alert.addButton(withTitle: HubL10n.text("hub.AppDelegate.229.54", fallback: "Quit"))
            _ = HubUIPresentation.response(to: alert)
            NSApp.terminate(nil)
            return
        }
        hubController.runStartupCleanup()
        let appVersion = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString")
            as? String ?? "development"
        let appBuild = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion")
            as? String ?? "development"
        HubAppLog.shared.record("launch.completed", category: "app", fields: [
            "app_build": appBuild,
            "app_version": appVersion
        ])
        hubController.reconcileLaunchAgent()
        let showOnboardingImmediately = hubController.shouldShowOnboardingBeforeInitialRefresh
        showDashboard(makeVisible: !showOnboardingImmediately) { [weak self] snapshot in
            guard let self else { return }
            if let scene = self.hubController.previewScene {
                self.mainWindowController?.configurePreviewScene(scene)
                if self.mainWindowController?.activeModalKind != .onboarding {
                    self.showDashboard()
                }
            } else if self.hubController.shouldShowOnboarding(for: snapshot) {
                if self.mainWindowController?.activeModalKind != .onboarding {
                    self.mainWindowController?.showFirstRunOnboarding()
                }
            } else if self.mainWindowController?.activeModalKind != .onboarding {
                self.showDashboard()
            }
        }
        if showOnboardingImmediately {
            mainWindowController?.showFirstRunOnboarding()
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        !(hubController?.lifecycleCapabilities.preventsQuit ?? false)
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        let lifecycle = hubController?.lifecycleCapabilities ?? .idle
        var reply = Self.terminationReply(windows: applicationWindows?(sender) ?? sender.windows, lifecycle: lifecycle,
                                          restartDutyOutstanding: hubController?.restartDutyOutstanding ?? false)
        guard reply != .terminateCancel else { return reply }
        // Accepted: the lifecycle closes to new admissions before anything is settled. The decision to
        // close is taken by the authority, under its own lock, because the snapshot above may be stale:
        // a foreground owner admitted since refuses the Quit, a background owner admitted since is
        // awaited.
        switch hubController?.beginClosing() {
        case .refused?:
            return .terminateCancel
        case .entered(ownerHeld: true)?:
            reply = .terminateLater
        default:
            break
        }
        if reply == .terminateLater {
            // One reply, on the main queue after this method has returned `.terminateLater`,
            // once background reconciliation and any restart owed have been settled.
            let once = HubCompletionOnce<Void> { [replyToTerminate] _ in
                DispatchQueue.main.async { replyToTerminate(true) }
            }
            hubController?.whenLifecycleSettled { once.complete(()) }
        }
        return reply
    }

    /// The lifecycle owner and the existing operation and sheet guards first (a pending
    /// operation cancels the quit); then background work or an outstanding restart duty
    /// defers it; otherwise it proceeds.
    static func terminationReply(windows: [NSWindow],
                                 lifecycle: HubLifecycleCapabilities,
                                 restartDutyOutstanding: Bool) -> NSApplication.TerminateReply {
        guard finishSheetsBeforeQuit(in: windows, lifecycle: lifecycle) else { return .terminateCancel }
        let backgroundOwned = lifecycle.owner?.isBackground == true
        return (restartDutyOutstanding || backgroundOwned) ? .terminateLater : .terminateNow
    }

    @objc func showLogs(_ sender: Any?) {
        guard hubController != nil else { return }
        HubAppLog.shared.record("window.opened", category: "logs", fields: ["source": "keyboard_or_menu"])
        showDashboard()
        mainWindowController?.showEmbeddedLogs()
    }

    @objc func showOverview(_ sender: Any?) {
        guard hubController != nil else { return }
        showDashboard()
        mainWindowController?.selectMainSection(.dashboard)
    }

    @objc func showVehicles(_ sender: Any?) {
        guard hubController != nil else { return }
        showDashboard()
        mainWindowController?.selectMainSection(.vehicles)
    }

    @objc func showActivity(_ sender: Any?) {
        guard hubController != nil else { return }
        showDashboard()
        mainWindowController?.selectMainSection(.activity)
    }

    @objc func showSettings(_ sender: Any?) {
        guard hubController != nil else { return }
        showDashboard()
        mainWindowController?.selectMainSection(.settings)
    }

    @objc func showDiagnostics(_ sender: Any?) {
        guard hubController != nil else { return }
        showDashboard()
        mainWindowController?.showEmbeddedDiagnostics()
    }

    @objc func showServiceDetails(_ sender: Any?) {
        guard hubController != nil else { return }
        showDashboard()
        mainWindowController?.showEmbeddedServiceDetails()
    }

    @objc func importData(_ sender: Any?) {
        guard hubController != nil else { return }
        showDashboard()
        mainWindowController?.showImport()
    }

    @objc func toggleSidebar(_ sender: Any?) {
        guard hubController != nil else { return }
        showDashboard()
        mainWindowController?.toggleSidebar()
    }

    @objc func showHelp(_ sender: Any?) {
        let alert = NSAlert()
        alert.messageText = HubL10n.text("hub.AppDelegate.154.52", fallback: "Teslatlas Hub Help")
        alert.informativeText = HubL10n.text("hub.AppDelegate.332.71", fallback: "Overview shows Hub health at a glance. Vehicles contains status and commands. Activity keeps recent events together, and Settings contains connections, diagnostics, imports and service details.")
        alert.addButton(withTitle: HubL10n.text("hub.AppDelegate.14.3", fallback: "OK"))
        HubUIPresentation.presentInformation(alert)
    }

    @objc func openCorrespondingSource(_ sender: Any?) {
        guard let url = HubRelease.correspondingSourceURL() else { return }
        NSWorkspace.shared.open(url)
    }

    @objc func showLegalNotice(_ sender: Any?) {
        let alert = NSAlert()
        alert.messageText = HubL10n.format("hub.AppDelegate.344.73", fallback: "Teslatlas Hub v%1$@", arguments: [String(describing: HubRelease.bundledVersion)])
        alert.informativeText = Self.legalNoticeText
        alert.addButton(withTitle: HubL10n.text("hub.AppDelegate.14.3", fallback: "OK"))
        alert.addButton(withTitle: HubL10n.text("hub.AppDelegate.347.75", fallback: "View Licence"))
        if HubRelease.bundledSourceCommit != nil {
            alert.addButton(withTitle: HubL10n.text("hub.AppDelegate.349.76", fallback: "Corresponding Source"))
        }
        switch HubUIPresentation.response(to: alert) {
        case .alertSecondButtonReturn:
            if let bundled = Bundle.main.url(forResource: "LICENSE", withExtension: nil) {
                NSWorkspace.shared.open(bundled)
            } else if let remote = HubRelease.licenceURL() {
                NSWorkspace.shared.open(remote)
            }
        case .alertThirdButtonReturn:
            openCorrespondingSource(sender)
        default:
            break
        }
    }

    static var legalNoticeText: String {
        let source: String
        if let url = HubRelease.correspondingSourceURL() {
            source = "Corresponding Source: \(url.absoluteString)"
        } else {
            source = "UNBOUND DEVELOPMENT BUILD; non-distributable until rebuilt with an exact pushed source commit."
        }
        return """
        Licence: AGPL-3.0-only
        Copyright © 2026 MAGRATHEAN UK LTD.
        Teslatlas Hub — originally authored by György Bolyki and published by MAGRATHEAN UK LTD. Source: https://github.com/magrathean-uk/teslatlas-hub
        \(source)
        Unofficial; not affiliated with Tesla or TeslaMate; no warranty.
        """
    }

    private func showDashboard(makeVisible: Bool = true,
                               onInitialRefresh: ((HubSnapshot) -> Void)? = nil) {
        if mainWindowController == nil {
            mainWindowController = MainWindowController(controller: hubController,
                                                        onInitialRefresh: onInitialRefresh)
        }
        guard makeVisible else { return }
        mainWindowController?.showWindow(nil)
        mainWindowController?.window?.makeKeyAndOrderFront(nil)
        HubApplicationActivation.activate()
    }
}
