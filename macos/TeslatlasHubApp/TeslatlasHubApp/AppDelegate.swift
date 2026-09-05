// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

/// Dock Quit and the application menu both enter NSApplication.terminate(_:).
/// Handle sheets here, before AppKit can silently defer termination for them.
final class HubApplication: NSApplication {
    override func terminate(_ sender: Any?) {
        guard AppDelegate.finishSheetsBeforeQuit(in: windows) else {
            activate(ignoringOtherApps: true)
            let alert = NSAlert()
            alert.messageText = "Hub is finishing an operation"
            alert.informativeText = "Wait for the current setup or import operation to finish, then quit. Quitting the app does not stop the background Hub service."
            alert.addButton(withTitle: "OK")
            _ = HubUIPresentation.response(to: alert)
            return
        }
        super.terminate(sender)
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation {
    private var mainWindowController: MainWindowController?
    private var hubController: HubController!
    private let keyWindow: () -> NSWindow?

    init(keyWindow: @escaping () -> NSWindow? = { NSApp.keyWindow }) {
        self.keyWindow = keyWindow
        super.init()
    }

    func applicationWillFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = Self.makeMainMenu(actionTarget: self)
        NSApp.windowsMenu = NSApp.mainMenu?.items.compactMap(\.submenu).first { $0.title == "Window" }
    }

    static func makeMainMenu(actionTarget: AnyObject? = nil) -> NSMenu {
        let mainMenu = NSMenu()

        let applicationItem = NSMenuItem()
        let applicationMenu = NSMenu()
        applicationItem.submenu = applicationMenu
        mainMenu.addItem(applicationItem)
        applicationMenu.addItem(withTitle: "About Teslatlas Hub",
                                action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)),
                                keyEquivalent: "")
        let legal = applicationMenu.addItem(
            withTitle: "Legal & Licence…",
            action: #selector(showLegalNotice(_:)),
            keyEquivalent: ""
        )
        legal.target = actionTarget
        let source = applicationMenu.addItem(
            withTitle: "Corresponding Source for v\(HubRelease.bundledVersion)…",
            action: #selector(openCorrespondingSource(_:)),
            keyEquivalent: ""
        )
        source.target = actionTarget
        applicationMenu.addItem(.separator())
        applicationMenu.addItem(withTitle: "Hide Teslatlas Hub",
                                action: #selector(NSApplication.hide(_:)),
                                keyEquivalent: "h")
        let hideOthers = applicationMenu.addItem(withTitle: "Hide Others",
                                                 action: #selector(NSApplication.hideOtherApplications(_:)),
                                                 keyEquivalent: "h")
        hideOthers.keyEquivalentModifierMask = [.command, .option]
        applicationMenu.addItem(withTitle: "Show All",
                                action: #selector(NSApplication.unhideAllApplications(_:)),
                                keyEquivalent: "")
        applicationMenu.addItem(.separator())
        let quit = applicationMenu.addItem(withTitle: "Quit Teslatlas Hub",
                                           action: #selector(quitApplication(_:)),
                                           keyEquivalent: "q")
        quit.target = actionTarget

        let editItem = NSMenuItem()
        let editMenu = NSMenu(title: "Edit")
        editItem.submenu = editMenu
        mainMenu.addItem(editItem)
        editMenu.addItem(withTitle: "Undo", action: Selector(("undo:")), keyEquivalent: "z")
        let redo = editMenu.addItem(withTitle: "Redo", action: Selector(("redo:")), keyEquivalent: "Z")
        redo.keyEquivalentModifierMask = [.command, .shift]
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        editMenu.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        editMenu.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        editMenu.addItem(withTitle: "Delete", action: #selector(NSText.delete(_:)), keyEquivalent: "")
        editMenu.addItem(.separator())
        editMenu.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")

        let viewItem = NSMenuItem()
        let viewMenu = NSMenu(title: "View")
        viewItem.submenu = viewMenu
        mainMenu.addItem(viewItem)
        let logs = viewMenu.addItem(withTitle: "Hub Logs",
                                    action: #selector(showLogs(_:)),
                                    keyEquivalent: "l")
        logs.target = actionTarget

        let windowItem = NSMenuItem()
        let windowMenu = NSMenu(title: "Window")
        windowItem.submenu = windowMenu
        mainMenu.addItem(windowItem)
        // AppKit disables performClose: for attached sheets before asking the
        // window to validate it. Route Close explicitly to the key window so an
        // idle account sheet can use its guarded Cancel action instead.
        let close = windowMenu.addItem(withTitle: "Close", action: #selector(closeKeyWindow(_:)), keyEquivalent: "w")
        close.target = actionTarget
        windowMenu.addItem(withTitle: "Minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
        windowMenu.addItem(withTitle: "Zoom", action: #selector(NSWindow.performZoom(_:)), keyEquivalent: "")

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
        guard menuItem.action == #selector(closeKeyWindow(_:)) else { return true }
        guard let window = keyWindow() else { return false }
        if let onboarding = window.windowController as? OnboardingWindowController {
            return onboarding.canCancel
        }
        return window.styleMask.contains(.closable) && window.attachedSheet == nil
    }

    @objc func quitApplication(_ sender: Any?) {
        NSApp.terminate(sender)
    }

    /// NSApplication's standard termination can be suppressed by an attached
    /// setup sheet. End idle sheets explicitly; don't interrupt an active import.
    static func finishSheetsBeforeQuit(in windows: [NSWindow]) -> Bool {
        guard !windows.contains(where: {
            ($0.delegate as? OnboardingWindowController)?.operationPreventsQuit == true
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
        guard !HubUIPresentation.isSilentTestHost else { return }
        hubController = HubController()
        if !hubController.previewMode {
            let staleImports = TeslaMateServerImporter.cleanupStaleTemporaryDirectories()
            if staleImports > 0 {
                HubAppLog.shared.record("temporary_files.removed", category: "teslamate_import",
                                        fields: ["directories": String(staleImports)])
            }
        }
        let appVersion = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString")
            as? String ?? "development"
        let appBuild = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion")
            as? String ?? "development"
        HubAppLog.shared.record("launch.completed", category: "app", fields: [
            "app_build": appBuild,
            "app_version": appVersion
        ])
        showDashboard { [weak self] snapshot in
            guard let self else { return }
            if let scene = self.hubController.previewScene {
                self.mainWindowController?.configurePreviewScene(scene)
            } else if self.hubController.shouldShowOnboarding(for: snapshot) {
                self.mainWindowController?.showFirstRunOnboarding()
            }
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    @objc func showLogs(_ sender: Any?) {
        guard let hubController else { return }
        HubAppLog.shared.record("window.opened", category: "logs", fields: ["source": "keyboard_or_menu"])
        showDashboard()
        _ = mainWindowController?.showLogs()
    }

    @objc func openCorrespondingSource(_ sender: Any?) {
        guard let url = HubRelease.correspondingSourceURL() else { return }
        NSWorkspace.shared.open(url)
    }

    @objc func showLegalNotice(_ sender: Any?) {
        let alert = NSAlert()
        alert.messageText = "Teslatlas Hub v\(HubRelease.bundledVersion)"
        alert.informativeText = Self.legalNoticeText
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "View Licence")
        alert.addButton(withTitle: "Corresponding Source")
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

    static let legalNoticeText = """
    Licence: AGPL-3.0-only
    Copyright © 2026 György Bolyki, MAGRATHEAN UK LTD, and identified contributors, each for material they own.
    Teslatlas Hub — originally authored by György Bolyki and published by MAGRATHEAN UK LTD. Source: https://github.com/magrathean-uk/teslatlas-hub
    Unofficial; not affiliated with Tesla or TeslaMate; no warranty.
    """

    private func showDashboard(onInitialRefresh: ((HubSnapshot) -> Void)? = nil) {
        if mainWindowController == nil {
            mainWindowController = MainWindowController(controller: hubController,
                                                        onInitialRefresh: onInitialRefresh)
        }
        mainWindowController?.showWindow(nil)
        mainWindowController?.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }
}
