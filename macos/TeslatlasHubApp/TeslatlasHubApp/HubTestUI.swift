// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

enum HubUIPresentation {
    static var isSilentTestHost: Bool {
        let environment = ProcessInfo.processInfo.environment
        return environment["TESLATLAS_HUB_TEST_MODE"] == "1"
            || environment["XCTestConfigurationFilePath"] != nil
            || NSClassFromString("XCTestCase") != nil
    }

    static func presentError(_ error: Error) {
        guard !isSilentTestHost else { return }
        _ = NSAlert(error: error).runModal()
    }

    static func presentInformation(_ alert: NSAlert) {
        guard !isSilentTestHost else { return }
        _ = alert.runModal()
    }

    static func response(to alert: NSAlert,
                         silentResponse: NSApplication.ModalResponse = .alertFirstButtonReturn)
        -> NSApplication.ModalResponse {
        guard !isSilentTestHost else { return silentResponse }
        return alert.runModal()
    }
}
