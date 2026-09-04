// SPDX-License-Identifier: AGPL-3.0-only

import XCTest
@testable import Teslatlas_Hub

final class HubPresentationStateTests: XCTestCase {
    func testModalStateReusesSameKindAndReplacesDifferentKind() {
        var state = HubModalState()
        XCTAssertEqual(state.request(.logs), .present(.logs))
        XCTAssertEqual(state.request(.logs), .reuse(.logs))
        XCTAssertEqual(state.request(.diagnostics), .replace(old: .logs, new: .diagnostics))
        state.dismiss(.diagnostics)
        XCTAssertNil(state.active)
    }

    func testActivityIsNewestFirstBoundedAndRealEventOnly() {
        var store = HubSessionActivityStore(limit: 3, now: { Date(timeIntervalSince1970: 100) })
        store.record(.hubStarted)
        store.record(.hubStopped)
        store.record(.hubRestarted)
        store.record(.accountDisconnected)

        XCTAssertEqual(store.activities.count, 3)
        XCTAssertEqual(store.activities.map(\.message), [
            "Tesla account disconnected", "Hub service restarted", "Hub service stopped"
        ])
    }
}
