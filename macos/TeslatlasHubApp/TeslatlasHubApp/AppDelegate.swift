import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var mainWindowController: MainWindowController?
    private var onboardingWindowController: OnboardingWindowController?
    private var hubController: HubController!

    func applicationDidFinishLaunching(_ notification: Notification) {
        hubController = HubController()
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
