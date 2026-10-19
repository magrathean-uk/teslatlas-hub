// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import Foundation
import XCTest
@testable import Teslatlas_Hub

/// A token from a private authority: tests cannot forge one, they obtain one the way the
/// controller does. Each call returns the token of a distinct, independent owner.
func makeLifecycleToken(_ family: HubLifecycleFamily = .diagnostics) -> HubLifecycleToken {
    switch HubLifecycleAuthority().acquire(family) {
    case let .success(token): return token
    case let .failure(refusal): preconditionFailure("a fresh authority refused: \(refusal)")
    }
}

/// The main window's native frame. Its content is `HubMetrics.windowSize`; the frame adds the title bar (32 pt)
/// and, once AppKit has laid the unified toolbar out, the toolbar (52 pt). Whether that layout has happened
/// before the window is first shown depends on the display configuration of the machine (an inactive accessory
/// test host, R-E1F6-21), so exactly these two frames are accepted: the width is always the content width.
func assertNativeMainWindowFrame(_ size: NSSize?, file: StaticString = #filePath, line: UInt = #line) {
    guard let size else { return XCTFail("the window has no frame", file: file, line: line) }
    XCTAssertEqual(size.width, HubMetrics.windowSize.width, file: file, line: line)
    XCTAssertTrue([HubMetrics.windowSize.height + 32, HubMetrics.windowSize.height + 84].contains(size.height),
                  "frame height \(size.height) is neither the bare (+32) nor the toolbar (+84) layout", file: file, line: line)
}
