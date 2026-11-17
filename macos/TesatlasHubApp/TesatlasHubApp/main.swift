// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

let application = HubApplication.shared
let applicationDelegate = AppDelegate()
application.delegate = applicationDelegate
application.setActivationPolicy(HubApplicationActivation.launchPolicy())
HubApplicationActivation.installFocusYield()
applicationDelegate.launchGate.hold()
let handOverNeededTryAgain = HubRenameMigration.prepareBeforeLaunch()
applicationDelegate.launchGate.release(afterRetry: handOverNeededTryAgain) {
    applicationDelegate.presentLaunchWindow()
}
application.run()
