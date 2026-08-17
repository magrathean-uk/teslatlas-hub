import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var mainWindowController: MainWindowController!
    private var hubController: HubController!

    func applicationDidFinishLaunching(_ notification: Notification) {
        hubController = HubController()
        mainWindowController = MainWindowController(controller: hubController)
        mainWindowController.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}
