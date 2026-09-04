// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var mainWindowController: MainWindowController?
    private var hubController: HubController!

    func applicationWillFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = Self.makeMainMenu(actionTarget: self)
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
        applicationMenu.addItem(withTitle: "Quit Teslatlas Hub",
                                action: #selector(NSApplication.terminate(_:)),
                                keyEquivalent: "q")

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

        return mainMenu
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
