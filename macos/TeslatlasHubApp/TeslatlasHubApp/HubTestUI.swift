// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

protocol HubAlertPresenting: AnyObject {
    func present(error: Error)
    func present(information alert: NSAlert)
    func response(to alert: NSAlert,
                  silentResponse: NSApplication.ModalResponse) -> NSApplication.ModalResponse
}

enum HubUIPresentation {
    private static var alertPresenterForTesting: HubAlertPresenting?

    @discardableResult
    static func replaceAlertPresenterForTesting(_ presenter: HubAlertPresenting?) -> HubAlertPresenting? {
        let previous = alertPresenterForTesting
        alertPresenterForTesting = presenter
        return previous
    }

    static var isSilentTestHost: Bool {
        let environment = ProcessInfo.processInfo.environment
        return environment["TESLATLAS_HUB_TEST_MODE"] == "1"
            || environment["XCTestConfigurationFilePath"] != nil
            || NSClassFromString("XCTestCase") != nil
    }

    static func presentError(_ error: Error) {
        if let alertPresenterForTesting {
            alertPresenterForTesting.present(error: error)
            return
        }
        guard !isSilentTestHost else { return }
        _ = NSAlert(error: error).runModal()
    }

    static func presentInformation(_ alert: NSAlert) {
        if let alertPresenterForTesting {
            alertPresenterForTesting.present(information: alert)
            return
        }
        guard !isSilentTestHost else { return }
        _ = alert.runModal()
    }

    static func response(to alert: NSAlert,
                         silentResponse: NSApplication.ModalResponse = .alertFirstButtonReturn)
        -> NSApplication.ModalResponse {
        if let alertPresenterForTesting {
            return alertPresenterForTesting.response(to: alert, silentResponse: silentResponse)
        }
        guard !isSilentTestHost else { return silentResponse }
        return alert.runModal()
    }
}
