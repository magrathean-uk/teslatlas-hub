import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var mainWindowController: MainWindowController?
    private var onboardingWindowController: OnboardingWindowController?
    private var logsWindowController: LogsWindowController?
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
        let staleImports = TeslaMateServerImporter.cleanupStaleTemporaryDirectories()
        if staleImports > 0 {
            HubAppLog.shared.record("temporary_files.removed", category: "teslamate_import",
                                    fields: ["directories": String(staleImports)])
        }
        hubController = HubController()
        HubAppLog.shared.record("launch.completed", category: "app")
        showDashboard { [weak self] snapshot in
            guard let self else { return }
            if self.hubController.shouldShowOnboarding(for: snapshot) {
                let dashboard = self.mainWindowController
                self.mainWindowController = nil
                self.showOnboarding()
                dashboard?.close()
            }
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    @objc func showLogs(_ sender: Any?) {
        guard let hubController else { return }
        HubAppLog.shared.record("window.opened", category: "logs", fields: ["source": "keyboard_or_menu"])
        if let logsWindowController {
            logsWindowController.refresh()
            logsWindowController.showWindow(nil)
            logsWindowController.window?.makeKeyAndOrderFront(nil)
            return
        }
        logsWindowController = LogsWindowController(controller: hubController)
        logsWindowController?.showWindow(nil)
        logsWindowController?.window?.makeKeyAndOrderFront(nil)
    }

    private func showOnboarding() {
        let onboarding = OnboardingWindowController(
            controller: hubController,
            resumeMigrationHandoverPhase: hubController.pendingMigrationHandoverPhase,
            previewRoute: hubController.onboardingPreviewRoute
        ) { [weak self] in
            guard let self else { return }
            let finishedOnboarding = self.onboardingWindowController
            self.onboardingWindowController = nil
            self.showDashboard()
            finishedOnboarding?.close()
        }
        onboardingWindowController = onboarding
        onboarding.showWindow(nil)
        onboarding.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

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
