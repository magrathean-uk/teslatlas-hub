// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

/// Quit and the lifecycle owner (R-E1F6-16, with R-E1F6-7 and R-E1F6-12): every route to
/// termination asks the application's one controller, an accepted Quit seals admission
/// first, and the reply is sent once, on the main queue, after the delegate has returned.
extension HubServiceLifecycleTests {
    private final class Replies {
        private let lock = NSLock()
        private(set) var values: [(accepted: Bool, onMain: Bool, delegateReturned: Bool)] = []
        var delegateReturned = false
        func record(_ accepted: Bool) {
            lock.lock()
            values.append((accepted, Thread.isMainThread, delegateReturned))
            lock.unlock()
        }
    }

    private func delegate(for rig: OwnerRig, replies: Replies = Replies(),
                          terminated: @escaping (Any?) -> Void = { _ in }) -> AppDelegate {
        let delegate = AppDelegate(keyWindow: { nil },
                                   replyToTerminate: { replies.record($0) },
                                   terminateApplication: terminated,
                                   applicationWindows: { _ in [] })
        delegate.hubController = rig.controller
        return delegate
    }

    func testEveryTerminationRouteUsesTheControllerOwner() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        var terminated = 0
        let appDelegate = delegate(for: rig, terminated: { _ in terminated += 1 })

        // AppMenuQuit forwards to the shared terminate.
        appDelegate.quitApplication(nil)
        XCTAssertEqual(terminated, 1, "AppMenuQuit enters the shared terminate")

        // Idle: every route allows it.
        XCTAssertTrue(HubApplication.mayTerminate(windows: [], controller: rig.controller))
        XCTAssertTrue(appDelegate.applicationShouldTerminateAfterLastWindowClosed(NSApp))

        // A sign-out, with no window anywhere, is a foreground owner.
        rig.runner.hold("control")
        rig.controller.signOutTeslaAccount { _ in }
        spinUntil(3) { rig.runner.heldKeys.contains("control") }
        XCTAssertEqual(rig.capabilities.owner, .signOut)
        XCTAssertFalse(HubApplication.mayTerminate(windows: [], controller: rig.controller),
                       "DockQuit and SharedTerminate consult the controller")
        XCTAssertEqual(appDelegate.applicationShouldTerminate(NSApp), .terminateCancel, "DelegateTerminate")
        XCTAssertFalse(appDelegate.applicationShouldTerminateAfterLastWindowClosed(NSApp), "LastWindowTerminate")
        XCTAssertEqual(rig.capabilities.state, .open, "a refused Quit does not close the lifecycle")
        rig.drain()

        // Every family: foreground owners refuse, the reconciliation waits, Delete before its
        // data step refuses, and after it Quit is allowed.
        for family in HubLifecycleFamily.allCases {
            let capabilities = HubLifecycleCapabilities(publication: 1, state: family == .delete ? .deleting : .open,
                                                        owner: family, phase: nil)
            let reply = AppDelegate.terminationReply(windows: [], lifecycle: capabilities, restartDutyOutstanding: false)
            XCTAssertEqual(reply, family == .reconcile ? .terminateLater : .terminateCancel, family.rawValue)
        }
        let deleted = HubLifecycleCapabilities(publication: 1, state: .deleted, owner: nil, phase: nil)
        XCTAssertEqual(AppDelegate.terminationReply(windows: [], lifecycle: deleted, restartDutyOutstanding: false),
                       .terminateNow, "the completed Delete's narrow exemption")
    }

    func testQuitIsRefusedDuringSignOutWithoutAVisibleBusyWindow() throws {
        for boundary in ["stop", "control", "refresh"] {
            let rig = try OwnerRig(.init(loaded: .loaded))
            defer { rig.tearDown() }
            switch boundary {
            case "stop": rig.service.hold("stop")
            case "control": rig.runner.hold("control")
            default: rig.runner.hold("status")
            }
            rig.controller.signOutTeslaAccount { _ in }
            spinUntil(3) { !(rig.service.heldKeys + rig.runner.heldKeys).isEmpty }
            let appDelegate = delegate(for: rig)
            XCTAssertEqual(appDelegate.applicationShouldTerminate(NSApp), .terminateCancel, boundary)
            XCTAssertEqual(rig.capabilities.state, .open, "\(boundary): refused, not closing")
            rig.drain()
            XCTAssertEqual(appDelegate.applicationShouldTerminate(NSApp), .terminateNow, "\(boundary): allowed once it has settled")
        }
    }

    func testQuitWaitsForReconcileBeforeRestartDutyExists() throws {
        let rig = try OwnerRig(.init(loaded: .loaded, change: .replaced))
        defer { rig.tearDown() }
        let replies = Replies()
        rig.service.hold("reconcile")
        var reconciled = 0
        rig.controller.reconcileLaunchAgent { reconciled += 1 }
        XCTAssertEqual(rig.capabilities.owner, .reconcile)
        XCTAssertFalse(rig.service.restartDutyOutstanding, "no restart record exists yet")
        XCTAssertFalse(rig.controller.restartDutyOutstanding)
        let appDelegate = delegate(for: rig, replies: replies)
        let reply = appDelegate.applicationShouldTerminate(NSApp)
        replies.delegateReturned = true
        XCTAssertEqual(reply, .terminateLater, "the background reconciliation is awaited")
        XCTAssertTrue(replies.values.isEmpty, "no reply before it has settled")
        rig.drain()
        spinUntil(3) { !replies.values.isEmpty }
        XCTAssertEqual(replies.values.count, 1)
        XCTAssertEqual(reconciled, 1)
    }

    func testAcceptedQuitBlocksNewAdmissionsAndDrainsOwnedCleanup() throws {
        let rig = try OwnerRig(.init(installed: false, loaded: .loaded))
        defer { rig.tearDown() }
        // A prepared (idle) migration session, a deferred reconcile and a background owner.
        var connected: Result<TeslaMateServerImportSession, Error>?
        _ = rig.controller.connectMigrationServer(host: "teslamate.local", user: "user", port: 22,
                                                  authentication: .key(identityFile: nil),
                                                  usePasswordlessSudo: false) { connected = $0 }
        spinUntil { connected != nil }
        XCTAssertEqual(rig.controller.registeredMigrationSessionCount, 1)
        let directory = try XCTUnwrap(rig.connector.directories.first)
        XCTAssertTrue(FileManager.default.fileExists(atPath: directory.path))
        guard case let .success(background) = rig.controller.lifecycle.acquire(.reconcile) else { return XCTFail("no owner") }
        var queuedReconcile = 0
        rig.agent.installed = true
        rig.controller.reconcileLaunchAgent { queuedReconcile += 1 }

        let replies = Replies()
        let appDelegate = delegate(for: rig, replies: replies)
        let reply = appDelegate.applicationShouldTerminate(NSApp)
        replies.delegateReturned = true
        XCTAssertEqual(reply, .terminateLater)

        // Sealed at once, before anything has settled.
        XCTAssertEqual(rig.capabilities.state, .closing)
        let before = rig.signature()
        var outcomes = 0
        rig.controller.startHub { if case .failure = $0 { outcomes += 1 } }
        rig.controller.signOutTeslaAccount { if case .failure = $0 { outcomes += 1 } }
        rig.controller.runFullDiagnostics { if $0.contains("not run") { outcomes += 1 } }
        XCTAssertEqual(outcomes, 3, "new admissions are refused while closing")
        XCTAssertEqual(rig.signature(), before)
        var refreshed = 0
        rig.controller.refresh { _ in refreshed += 1 }
        XCTAssertEqual(refreshed, 1)
        XCTAssertEqual(rig.runner.count("status"), 0, "no observer subprocess once closing")
        XCTAssertFalse(rig.controller.performPreferenceWrite {})
        XCTAssertEqual(queuedReconcile, 1, "the queued reconciliation was dropped and answered")
        XCTAssertFalse(rig.events.all.contains("ensure"))
        XCTAssertEqual(rig.controller.registeredMigrationSessionCount, 0, "prepared sessions were closed")
        XCTAssertFalse(FileManager.default.fileExists(atPath: directory.path), "and their directory removed")
        XCTAssertTrue(replies.values.isEmpty)

        // The already-authorised background owner settles, then the one reply is sent.
        XCTAssertTrue(rig.controller.lifecycle.settle(background) {})
        spinUntil(3) { !replies.values.isEmpty }
        XCTAssertEqual(replies.values.count, 1)

        // Termination: the closed state is final.
        appDelegate.applicationWillTerminate(Notification(name: NSApplication.willTerminateNotification))
        XCTAssertEqual(rig.capabilities.state, .closed)
        outcomes = 0
        rig.controller.startHub { if case .failure = $0 { outcomes += 1 } }
        XCTAssertEqual(outcomes, 1)
    }

    func testTerminationReplyIsOnceOnMainAfterDelegateReturn() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        guard case let .success(background) = rig.controller.lifecycle.acquire(.reconcile) else { return XCTFail("no owner") }
        let replies = Replies()
        let appDelegate = delegate(for: rig, replies: replies)
        let reply = appDelegate.applicationShouldTerminate(NSApp)
        replies.delegateReturned = true
        XCTAssertEqual(reply, .terminateLater)
        // Settled from another thread, and the settlement announced twice.
        let settled = expectation(description: "settled off the main thread")
        DispatchQueue.global().async {
            rig.controller.lifecycle.settle(background) {}
            rig.controller.whenLifecycleSettled {}
            settled.fulfill()
        }
        wait(for: [settled], timeout: 3)
        spinUntil(3) { !replies.values.isEmpty }
        spinBriefly(0.1)
        XCTAssertEqual(replies.values.count, 1, "exactly one reply")
        XCTAssertEqual(replies.values.first?.accepted, true)
        XCTAssertEqual(replies.values.first?.onMain, true, "on the main queue")
        XCTAssertEqual(replies.values.first?.delegateReturned, true, "after applicationShouldTerminate returned")
    }
}

// MARK: - R-E1F6-18: termination routes, one test each

extension HubServiceLifecycleTests {
    private func heldSignOut(_ rig: OwnerRig) {
        rig.runner.hold("control")
        rig.controller.signOutTeslaAccount { _ in }
        spinUntil(3) { rig.runner.heldKeys.contains("control") }
    }

    func testEntryAppMenuQuitUsesLifecycleOwner() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        var terminated = 0
        let appDelegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { _ in },
                                      terminateApplication: { _ in terminated += 1 },
                                      applicationWindows: { _ in [] })
        appDelegate.hubController = rig.controller
        // The application menu's own Quit item, built for this delegate.
        let quit = try XCTUnwrap(AppDelegate.makeMainMenu(actionTarget: appDelegate).items.first?.submenu?
            .item(withTitle: "Quit Teslatlas Hub"))
        XCTAssertTrue(NSApp.sendAction(try XCTUnwrap(quit.action), to: quit.target, from: quit))
        XCTAssertEqual(terminated, 1, "the menu item enters the shared terminate")
        // The shared terminate then asks the controller.
        heldSignOut(rig)
        XCTAssertFalse(HubApplication.mayTerminate(windows: [], controller: appDelegate.hubController))
        rig.drain()
    }

    func testEntryDockQuitUsesLifecycleOwner() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        XCTAssertTrue(HubApplication.mayTerminate(windows: [], controller: rig.controller))
        rig.service.hold("pause")
        rig.controller.runFullDiagnostics { _ in }
        XCTAssertFalse(HubApplication.mayTerminate(windows: [], controller: rig.controller),
                       "the Dock's Quit enters the same terminate and asks the controller")
        rig.drain()
        XCTAssertTrue(HubApplication.mayTerminate(windows: [], controller: rig.controller))
    }

    func testEntrySharedTerminateUsesLifecycleOwner() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        let appDelegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { _ in },
                                      applicationWindows: { _ in [] })
        appDelegate.hubController = rig.controller
        // HubApplication.terminate reads the delegate's controller: the application's one.
        XCTAssertTrue(appDelegate.hubController === rig.controller)
        for family in HubLifecycleFamily.allCases where family != .delete && family != .reconcile {
            guard case let .success(token) = rig.controller.lifecycle.acquire(family) else { return XCTFail("\(family)") }
            XCTAssertFalse(HubApplication.mayTerminate(windows: [], controller: appDelegate.hubController), family.rawValue)
            rig.controller.lifecycle.settle(token) {}
        }
        guard case let .success(background) = rig.controller.lifecycle.acquire(.reconcile) else { return XCTFail("reconcile") }
        XCTAssertTrue(HubApplication.mayTerminate(windows: [], controller: rig.controller),
                      "background reconciliation does not refuse; the delegate waits for it")
        rig.controller.lifecycle.settle(background) {}
    }

    func testQuitAcceptedFromAnIdleSnapshotIsRefusedWhenAForegroundOwnerArrivesBeforeClosing() throws {
        // F6-4: accepting Quit and entering closing are one step. The decision reads the lifecycle,
        // then the windows; a foreground owner admitted between the two must still refuse the Quit.
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        var token: HubLifecycleToken?
        let replies = Replies()
        let appDelegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { replies.record($0) },
                                      applicationWindows: { _ in
            if token == nil, case let .success(admitted) = rig.controller.lifecycle.acquire(.signOut) { token = admitted }
            return []
        })
        appDelegate.hubController = rig.controller
        XCTAssertEqual(appDelegate.applicationShouldTerminate(NSApp), .terminateCancel,
                       "a sign-out admitted after the snapshot refuses the Quit")
        XCTAssertNotNil(token, "the owner was admitted inside the decision")
        XCTAssertEqual(rig.capabilities.state, .open, "the lifecycle did not close over the owner")
        XCTAssertEqual(rig.capabilities.owner, .signOut)
        XCTAssertTrue(replies.values.isEmpty)
        if let token { rig.controller.lifecycle.settle(token) {} }
        XCTAssertEqual(appDelegate.applicationShouldTerminate(NSApp), .terminateNow)
        XCTAssertEqual(rig.capabilities.state, .closing)
    }

    func testARefusedClosingLeavesPreparedSessionsAndQueuedReconcilesAlone() throws {
        // Only an entered closing seals the controller: a refused one (a foreground owner holds the
        // lifecycle) closes no prepared migration session and drops no queued work.
        let rig = try OwnerRig(.init(installed: false, loaded: .loaded, change: .replaced))
        defer { rig.tearDown() }
        var connected: Result<TeslaMateServerImportSession, Error>?
        _ = rig.controller.connectMigrationServer(host: "teslamate.local", user: "user", port: 22,
                                                  authentication: .key(identityFile: nil),
                                                  usePasswordlessSudo: false) { connected = $0 }
        spinUntil { connected != nil }
        XCTAssertEqual(rig.controller.registeredMigrationSessionCount, 1)
        guard case let .success(foreground) = rig.controller.lifecycle.acquire(.signOut) else { return XCTFail("not admitted") }
        // A reconcile requested while the foreground owner holds the lifecycle is queued (F7-5).
        rig.agent.installed = true
        var queuedReconcile = 0
        rig.controller.reconcileLaunchAgent { queuedReconcile += 1 }
        XCTAssertEqual(queuedReconcile, 0, "queued, not answered")
        XCTAssertEqual(rig.service.count("reconcile"), 0)

        XCTAssertEqual(rig.controller.beginClosing(), .refused)
        XCTAssertEqual(rig.controller.registeredMigrationSessionCount, 1, "the prepared session stays open")
        XCTAssertEqual(rig.capabilities.state, .open)
        XCTAssertEqual(queuedReconcile, 0, "a refused closing does not discard the queued reconcile")

        // It is still queued: when the foreground owner is gone it runs, once.
        rig.controller.lifecycle.settle(foreground) {}
        spinUntil(5) { queuedReconcile >= 1 }
        spinBriefly(0.1)
        XCTAssertEqual(queuedReconcile, 1)
        XCTAssertEqual(rig.service.count("reconcile"), 1, "the kept reconcile was carried out")
        rig.drain()

        // An entered closing discards a queued reconcile without running it.
        guard case let .success(background) = rig.controller.lifecycle.acquire(.reconcile) else { return XCTFail("not admitted") }
        var discarded = 0
        rig.controller.reconcileLaunchAgent { discarded += 1 }
        XCTAssertEqual(discarded, 0)
        XCTAssertEqual(rig.controller.beginClosing(), .entered(ownerHeld: true))
        XCTAssertEqual(discarded, 1, "an entered closing answers the queued reconcile at once")
        XCTAssertEqual(rig.service.count("reconcile"), 1, "and does not run it")
        XCTAssertEqual(rig.controller.registeredMigrationSessionCount, 0, "an entered closing closes the prepared session")
        rig.controller.lifecycle.settle(background) {}
        spinBriefly(0.1)
        XCTAssertEqual(rig.service.count("reconcile"), 1)
        XCTAssertEqual(rig.controller.beginClosing(), .alreadyClosing)
    }

    func testQuitAcceptedFromAnIdleSnapshotWaitsForABackgroundOwnerThatArrivesBeforeClosing() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        var token: HubLifecycleToken?
        let replies = Replies()
        let appDelegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { replies.record($0) },
                                      applicationWindows: { _ in
            if token == nil, case let .success(admitted) = rig.controller.lifecycle.acquire(.reconcile) { token = admitted }
            return []
        })
        appDelegate.hubController = rig.controller
        let reply = appDelegate.applicationShouldTerminate(NSApp)
        replies.delegateReturned = true
        XCTAssertEqual(reply, .terminateLater, "the reconciliation admitted after the snapshot is awaited")
        XCTAssertEqual(rig.capabilities.state, .closing)
        spinBriefly(0.2)
        XCTAssertTrue(replies.values.isEmpty, "no reply before the owner has settled")
        if let token { rig.controller.lifecycle.settle(token) {} }
        spinUntil(3) { !replies.values.isEmpty }
        XCTAssertEqual(replies.values.count, 1)
        XCTAssertEqual(replies.values.first?.accepted, true)
    }

    func testEntryDelegateTerminateUsesLifecycleOwner() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        let appDelegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { _ in },
                                      applicationWindows: { _ in [] })
        appDelegate.hubController = rig.controller
        heldSignOut(rig)
        XCTAssertEqual(appDelegate.applicationShouldTerminate(NSApp), .terminateCancel)
        XCTAssertEqual(rig.capabilities.state, .open)
        rig.drain()
        XCTAssertEqual(appDelegate.applicationShouldTerminate(NSApp), .terminateNow)
        XCTAssertEqual(rig.capabilities.state, .closing, "an accepted Quit closed the lifecycle")
    }

    func testEntryDelayedTerminateReplyUsesLifecycleOwner() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        let replies = Replies2()
        let appDelegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { replies.record($0) },
                                      applicationWindows: { _ in [] })
        appDelegate.hubController = rig.controller
        rig.service.restartDuty = true      // a restart is owed (the held record shows the job loaded)
        XCTAssertTrue(rig.controller.restartDutyOutstanding)
        let reply = appDelegate.applicationShouldTerminate(NSApp)
        XCTAssertEqual(reply, .terminateLater)
        spinBriefly(0.2)   // a reply sent too early is queued on the main queue and would arrive here
        XCTAssertEqual(replies.count, 0, "no reply while the duty stands")
        rig.service.settleRestartDuty()
        spinUntil(3) { replies.count == 1 }
        XCTAssertEqual(replies.count, 1)
        XCTAssertTrue(replies.onMain, "the reply is sent on the main queue")
    }

    func testEntryLastWindowTerminateUsesLifecycleOwner() throws {
        let rig = try OwnerRig(.init(loaded: .loaded))
        defer { rig.tearDown() }
        let appDelegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { _ in },
                                      applicationWindows: { _ in [] })
        appDelegate.hubController = rig.controller
        XCTAssertTrue(appDelegate.applicationShouldTerminateAfterLastWindowClosed(NSApp))
        heldSignOut(rig)
        XCTAssertFalse(appDelegate.applicationShouldTerminateAfterLastWindowClosed(NSApp),
                       "closing the last window does not end a sign-out")
        rig.drain()
        XCTAssertTrue(appDelegate.applicationShouldTerminateAfterLastWindowClosed(NSApp))
    }

    // MARK: F7-4

    func testARefusedQuitNamesAStuckHelperProcessThroughTheApplicationsTerminate() throws {
        let appDelegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { _ in },
                                      terminateApplication: { _ in }, applicationWindows: { _ in [] })
        let previousDelegate = NSApp.delegate
        let presenter = RecordingAlertPresenter()
        let previousPresenter = HubUIPresentation.replaceAlertPresenterForTesting(presenter)
        defer {
            NSApp.delegate = previousDelegate
            HubUIPresentation.replaceAlertPresenterForTesting(previousPresenter)
        }
        NSApp.delegate = appDelegate

        // An ordinary foreground owner: the generic text.
        let ordinary = try OwnerRig(.init(loaded: .loaded))
        defer { ordinary.tearDown() }
        appDelegate.hubController = ordinary.controller
        ordinary.runner.hold("control")
        ordinary.controller.signOutTeslaAccount { _ in }
        spinUntil(3) { ordinary.runner.heldKeys.contains("control") }
        XCTAssertFalse(ordinary.capabilities.childHeld)
        guard NSApp.delegate === appDelegate, !HubApplication.mayTerminate(windows: NSApp.windows, controller: ordinary.controller) else {
            return XCTFail("a sign-out must refuse Quit (and the delegate must be the test's)")
        }
        NSApp.terminate(nil)
        XCTAssertEqual(presenter.confirmations.count, 1)
        XCTAssertEqual(presenter.confirmations.last?.messageText, "Hub is finishing an operation")
        ordinary.drain()

        // A child that failed but cannot be shown gone: its own words.
        let stuck = try OwnerRig(.init(loaded: .loaded))
        defer { stuck.tearDown() }
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        stuck.runner.results["control"] = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        appDelegate.hubController = stuck.controller
        stuck.controller.signOutTeslaAccount { _ in }
        spinUntil(5) { stuck.capabilities.childHeld }
        XCTAssertTrue(stuck.capabilities.childHeld)
        guard NSApp.delegate === appDelegate, !HubApplication.mayTerminate(windows: NSApp.windows, controller: stuck.controller) else {
            return XCTFail("a held child must refuse Quit (and the delegate must be the test's)")
        }
        NSApp.terminate(nil)
        XCTAssertEqual(presenter.confirmations.count, 2)
        XCTAssertEqual(presenter.confirmations.last?.messageText, "Hub is waiting for a helper process")
        XCTAssertTrue(presenter.confirmations.last?.informativeText.contains("stuck helper process") ?? false)
        quiescence.childReaped()
        stuck.drain()
    }

    // MARK: F7-2

    func testAPreflightThatCannotBeStoppedKeepsItsQuiescenceThroughTheStartAndRestartFailure() {
        for action in ["start", "restart"] {
            let rig = LifecycleRig(loaded: action == "restart")
            let quiescence = HubChildQuiescence()
            quiescence.childMayRun()
            rig.preflightResult = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
            guard case let .failure(error)? = rig.run(action) else { return XCTFail("\(action) did not fail") }
            guard case let HubActionError.commandNotQuiescent(inner, held) = error else {
                return XCTFail("\(action): the quiescence was dropped by the preflight wrapper: \(error)")
            }
            XCTAssertTrue(held === quiescence, action)
            XCTAssertTrue(inner.localizedDescription.contains("checks failed"), "\(action): \(inner.localizedDescription)")
            quiescence.childReaped()
        }
        // A preflight whose process is already gone is the plain failure.
        let rig = LifecycleRig()
        let reaped = HubChildQuiescence()
        rig.preflightResult = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, reaped))
        guard case let .failure(error)? = rig.run("start"), case HubActionError.commandFailed = error else {
            return XCTFail("a reaped preflight child is an ordinary failure")
        }
    }

    func testAStartRecoveryReportKeepsTheQuiescenceOfAPreflightThatCannotBeStopped() {
        // The Hub was paused (a restart is owed); the Start's preflight then fails and a recovery follows. The
        // report that wraps the failure must still say the preflight child may be running.
        let rig = LifecycleRig(loaded: true)
        _ = rig.run("pause")
        XCTAssertTrue(rig.controller.restartRecordHeld(by: rig.owner))
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        rig.preflightResult = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        guard case let .failure(error)? = rig.run("start") else { return XCTFail("the start did not fail") }
        XCTAssertTrue(error is HubServiceRestartReport, "consumers still receive the restart report: \(error)")
        XCTAssertTrue(HubActionError.pendingQuiescence(of: error) === quiescence)
        let result: Result<String, Error> = .failure(error)
        XCTAssertTrue(result.pendingQuiescence === quiescence, "so the lifecycle operation holds its owner")
        quiescence.childReaped()
        XCTAssertNil(result.pendingQuiescence)
    }

    func testARecoveryStepThatCannotBeStoppedIsKeptByTheRestartReport() {
        let rig = LifecycleRig(loaded: true)
        _ = rig.run("pause")
        rig.preflightResult = .failure(HubActionError.commandFailed("checks"))
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        defer { quiescence.childReaped() }
        rig.launchctl.failNext("enable", with: HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        guard case let .failure(error)? = rig.run("start") else { return XCTFail("the start did not fail") }
        guard let report = error as? HubServiceRestartReport, report.isUnconfirmed else {
            return XCTFail("an unconfirmed restart report is expected: \(error)")
        }
        XCTAssertTrue(HubActionError.pendingQuiescence(of: report) === quiescence,
                      "the recovery command that may still run is not forgotten")
    }

    func testDeleteStopFailuresKeepTheQuiescenceOfTheCommandThatCannotBeStopped() {
        for command in ["disable", "bootout"] {
            let rig = LifecycleRig(loaded: true)
            let quiescence = HubChildQuiescence()
            quiescence.childMayRun()
            rig.launchctl.failNext(command, with: HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
            guard case let .failure(error)? = rig.deleteHubAndData() else { return XCTFail("\(command): Delete did not fail") }
            guard case let HubActionError.commandNotQuiescent(inner, held) = error else {
                return XCTFail("\(command): the quiescence was dropped by the stop wrapper: \(error)")
            }
            XCTAssertTrue(held === quiescence, command)
            XCTAssertTrue(inner.localizedDescription.contains("Nothing was deleted"), inner.localizedDescription)
            quiescence.childReaped()
        }
    }

    func testReconcileRestartThatCannotStopItsPreflightKeepsItsOwnerHeld() throws {
        let rig = try OwnerRig(.init(loaded: .loaded, change: .replaced))
        defer { rig.tearDown() }
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        rig.service.results["restart"] = .failure(HubActionError.commandNotQuiescent(HubActionError.commandTimedOut, quiescence))
        var reconciled = 0
        rig.controller.reconcileLaunchAgent { reconciled += 1 }
        spinUntil(5) { rig.service.count("reconcile") == 1 }
        spinBriefly(0.2)
        XCTAssertEqual(reconciled, 0, "the reconciliation is not complete while its preflight child may still run")
        XCTAssertEqual(rig.capabilities.owner, .reconcile)
        guard case .failure(.busy(.reconcile)) = rig.controller.lifecycle.acquire(.delete) else {
            return XCTFail("Delete was admitted while the reconcile child may still run")
        }
        quiescence.childReaped()
        spinUntil(5) { reconciled == 1 }
        XCTAssertEqual(reconciled, 1)
        XCTAssertTrue(rig.capabilities.isIdle)
    }

    // MARK: F8-1

    private static let notStoppedSentence =
        "A Hub command could not be stopped and may still be running. Wait a moment before trying again."

    private static func stuckCommand(_ quiescence: HubChildQuiescence, _ underlying: Error = HubActionError.commandTimedOut) -> Error {
        HubActionError.commandNotQuiescent(underlying, quiescence)
    }

    private static func timesSaid(_ text: String) -> Int {
        text.components(separatedBy: "could not be stopped and may still be running").count - 1
    }

    func testAStuckChildKeepsTheMessageOfTheOperationThatFailedAndSaysItMayStillRun() {
        let sentence = Self.notStoppedSentence
        // Delete: the refusal still says that nothing was deleted (the spec's R9 text), then that the command may run.
        for command in ["disable", "bootout"] {
            let rig = LifecycleRig(loaded: true)
            let quiescence = HubChildQuiescence()
            quiescence.childMayRun()
            defer { quiescence.childReaped() }
            rig.launchctl.failNext(command, with: Self.stuckCommand(quiescence))
            guard case let .failure(error)? = rig.deleteHubAndData() else { return XCTFail("\(command): Delete did not fail") }
            let text = error.localizedDescription
            XCTAssertTrue(text.hasPrefix("The background service could not be stopped ("), "\(command): \(text)")
            XCTAssertTrue(text.contains("Hub command timed out.). Nothing was deleted."), "\(command): \(text)")
            XCTAssertTrue(text.hasSuffix(" " + sentence), "\(command): \(text)")
            XCTAssertEqual(Self.timesSaid(text), 1, "\(command): the sentence is not repeated inside the brackets: \(text)")
        }
        // Start and Restart: the preflight message is still shown, then the sentence.
        for action in ["start", "restart"] {
            let rig = LifecycleRig(loaded: action == "restart")
            let quiescence = HubChildQuiescence()
            quiescence.childMayRun()
            defer { quiescence.childReaped() }
            rig.preflightResult = .failure(Self.stuckCommand(quiescence))
            guard case let .failure(error)? = rig.run(action) else { return XCTFail("\(action) did not fail") }
            let text = error.localizedDescription
            XCTAssertTrue(text.hasPrefix("The Hub did not start because its checks failed: Hub command timed out."), "\(action): \(text)")
            XCTAssertTrue(text.hasSuffix(" " + sentence), "\(action): \(text)")
            XCTAssertEqual(Self.timesSaid(text), 1, "\(action): \(text)")
        }
        // The restart report that a Start after a pause returns keeps the same words for the window.
        let paused = LifecycleRig(loaded: true)
        _ = paused.run("pause")
        let held = HubChildQuiescence()
        held.childMayRun()
        defer { held.childReaped() }
        paused.preflightResult = .failure(Self.stuckCommand(held))
        guard case let .failure(report)? = paused.run("start") else { return XCTFail("the start did not fail") }
        XCTAssertTrue(report is HubServiceRestartReport, "\(report)")
        XCTAssertTrue(report.localizedDescription.contains("The Hub did not start because its checks failed:"), report.localizedDescription)
        XCTAssertTrue(report.localizedDescription.contains("may still be running"), report.localizedDescription)
        // A bare executor failure keeps the plain sentence: the command itself says nothing more.
        let bare = HubChildQuiescence()
        bare.childMayRun()
        defer { bare.childReaped() }
        XCTAssertEqual(Self.stuckCommand(bare).localizedDescription, sentence)
        XCTAssertEqual(Self.stuckCommand(bare, HubActionError.commandExited(7, "boom")).localizedDescription, sentence)
        XCTAssertEqual(Self.stuckCommand(bare, NSError(domain: NSPOSIXErrorDomain, code: 5)).localizedDescription, sentence)
        XCTAssertEqual(Self.stuckCommand(bare, HubActionError.commandFailed("Hub command output did not close.")).localizedDescription,
                       sentence)
        // Any other worded failure of a layer above the executor keeps its words too.
        for worded in [HubActionError.preview, HubActionError.missingResource("hub")] {
            let text = Self.stuckCommand(bare, worded).localizedDescription
            XCTAssertEqual(text, worded.localizedDescription + " " + sentence)
        }
    }

    // MARK: F8-2

    func testAHeldChildOfABackgroundOwnerRefusesQuitToo() throws {
        let rig = try OwnerRig(.init(loaded: .loaded, change: .replaced))
        defer { rig.tearDown() }
        let replies = Replies()
        let appDelegate = delegate(for: rig, replies: replies)
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        rig.service.results["restart"] = .failure(Self.stuckCommand(quiescence))
        rig.controller.reconcileLaunchAgent {}
        spinUntil(5) { rig.capabilities.childHeld }
        XCTAssertEqual(rig.capabilities.owner, .reconcile)
        XCTAssertTrue(rig.capabilities.childHeld)
        XCTAssertTrue(rig.capabilities.preventsQuit, "a held child refuses Quit whatever the owner")
        XCTAssertFalse(HubApplication.mayTerminate(windows: [], controller: rig.controller))
        XCTAssertEqual(appDelegate.applicationShouldTerminate(NSApp), .terminateCancel)
        XCTAssertEqual(rig.capabilities.state, .open, "a refused Quit does not close the lifecycle")
        XCTAssertTrue(replies.values.isEmpty, "no termination reply is owed")
        XCTAssertEqual(HubApplication.quitRefusalText(for: rig.capabilities).title, "Hub is waiting for a helper process")

        quiescence.childReaped()
        spinUntil(5) { rig.capabilities.isIdle }
        XCTAssertTrue(rig.capabilities.isIdle)
        XCTAssertFalse(rig.capabilities.preventsQuit)
        XCTAssertEqual(appDelegate.applicationShouldTerminate(NSApp), .terminateNow)
    }

    func testTheAuthorityRefusesToCloseWhileAChildIsHeldWhateverAnEarlierSnapshotSaid() throws {
        let authority = HubLifecycleAuthority()
        guard case let .success(token) = authority.acquire(.reconcile) else { return XCTFail("not admitted") }
        let runner = OwnerRunner()
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        runner.results["status"] = .failure(Self.stuckCommand(quiescence))
        let operation = HubLifecycleOperation(token: token, authority: authority,
                                              commandRunner: runner, serviceRunner: OwnerService())
        operation.run(["--config", "/x/config.toml", "status"]) { _ in }
        operation.settle {}
        XCTAssertTrue(authority.capabilities.childHeld)
        XCTAssertEqual(authority.beginClosing(), .refused, "decided under the authority's own lock")
        XCTAssertEqual(authority.capabilities.state, .open)
        quiescence.childReaped()
        XCTAssertTrue(authority.capabilities.isIdle)
        XCTAssertEqual(authority.beginClosing(), .entered(ownerHeld: false))
    }

    // MARK: F8-4

    private final class QuietRunner: HubCommandRunning {
        func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) { completion(.success("{}")) }
    }

    func testAReconcileWhoseStateQueryCannotBeStoppedReportsItAndKeepsItsChildHeld() {
        let rig = LifecycleRig(loaded: true)
        rig.agent.change = .replaced
        rig.agent.installed = true
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        defer { quiescence.childReaped() }
        rig.launchctl.failNext("print", with: Self.stuckCommand(quiescence))
        var outcome: HubReconcileOutcome?
        rig.controller.reconcile(owner: rig.owner, ensure: { try rig.agent.ensureInstalled(requireConfig: false) }) { outcome = $0 }
        rig.scheduler.advance(by: 0)
        guard let outcome else { return XCTFail("the reconcile did not answer") }
        XCTAssertTrue(outcome.pendingQuiescence === quiescence, "the query's process may still run: \(outcome)")
        XCTAssertEqual(rig.launchctlEvents, ["print"], "an unknown state changes nothing")

        // An ordinary failure of the query still changes nothing and holds nothing.
        let plain = LifecycleRig(loaded: true)
        plain.agent.change = .replaced
        plain.agent.installed = true
        plain.launchctl.failNext("print", with: HubActionError.commandTimedOut)
        var second: HubReconcileOutcome?
        plain.controller.reconcile(owner: plain.owner, ensure: { try plain.agent.ensureInstalled(requireConfig: false) }) { second = $0 }
        plain.scheduler.advance(by: 0)
        guard case .completed? = second else { return XCTFail("an ordinary query failure completes: \(String(describing: second))") }
        XCTAssertNil(second?.pendingQuiescence)
    }

    func testAReconcileWhoseStateQueryCannotBeStoppedKeepsTheOwnerOfTheControllerHeld() {
        let rig = LifecycleRig(loaded: true)
        rig.agent.change = .replaced
        rig.agent.installed = true
        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        rig.launchctl.failNext("print", with: Self.stuckCommand(quiescence))
        let controller = HubController(environment: [:], commandRunner: QuietRunner(), serviceRunner: rig.controller,
                                       launchAgent: rig.agent, dataRemover: nil, homeDirectory: HubTestScratch.directory,
                                       serviceInstalledOverride: true)
        var reconciled = 0
        controller.reconcileLaunchAgent { reconciled += 1 }
        spinUntil(5) { controller.lifecycleCapabilities.childHeld }
        XCTAssertEqual(controller.lifecycleCapabilities.owner, .reconcile)
        XCTAssertTrue(controller.lifecycleCapabilities.childHeld)
        spinBriefly(0.2)
        XCTAssertEqual(reconciled, 0, "the reconciliation is not complete while its query child may still run")
        guard case .failure(.busy(.reconcile)) = controller.lifecycle.acquire(.delete) else {
            return XCTFail("Delete was admitted while the query child may still run")
        }
        quiescence.childReaped()
        spinUntil(5) { reconciled == 1 }
        XCTAssertEqual(reconciled, 1)
        XCTAssertTrue(controller.lifecycleCapabilities.isIdle)
    }

    func testARestartReportHoldsUntilEveryChildItReportsIsGone() {
        func stuck() -> HubChildQuiescence { let q = HubChildQuiescence(); q.childMayRun(); return q }
        for firstReapedFirst in [true, false] {
            let original = stuck(), recovery = stuck()
            let report = HubServiceRestartReport(original: Self.stuckCommand(original), outcome: .unconfirmed,
                                                 recoveryFailure: Self.stuckCommand(recovery))
            guard let held = HubActionError.pendingQuiescence(of: report) else { return XCTFail("a report with two live children is pending") }
            var released = 0
            held.whenQuiescent { released += 1 }
            (firstReapedFirst ? original : recovery).childReaped()
            XCTAssertFalse(held.isQuiescent, "one of the two children is still running (\(firstReapedFirst))")
            XCTAssertEqual(released, 0)
            (firstReapedFirst ? recovery : original).childReaped()
            XCTAssertTrue(held.isQuiescent)
            XCTAssertEqual(released, 1, "released exactly once, when the last one is gone")
            XCTAssertNil(HubActionError.pendingQuiescence(of: report))
        }
        // One live child is reported as itself, whichever of the two it is.
        let only = stuck(), gone = HubChildQuiescence()
        let first = HubServiceRestartReport(original: Self.stuckCommand(only), outcome: .unconfirmed,
                                            recoveryFailure: Self.stuckCommand(gone))
        XCTAssertTrue(HubActionError.pendingQuiescence(of: first) === only)
        let second = HubServiceRestartReport(original: Self.stuckCommand(gone), outcome: .unconfirmed,
                                             recoveryFailure: Self.stuckCommand(only))
        XCTAssertTrue(HubActionError.pendingQuiescence(of: second) === only)
        only.childReaped()
    }

    func testTheCombinedQuiescenceIgnoresPartsThatAreAlreadyGone() {
        func stuck() -> HubChildQuiescence { let q = HubChildQuiescence(); q.childMayRun(); return q }
        let gone = HubChildQuiescence()
        XCTAssertNil(HubChildQuiescence.allOf([]))
        XCTAssertNil(HubChildQuiescence.allOf([gone]), "nothing pending")
        let only = stuck()
        XCTAssertTrue(HubChildQuiescence.allOf([only, gone]) === only, "one pending part is reported as itself")
        XCTAssertTrue(HubChildQuiescence.allOf([gone, only]) === only)
        only.childReaped()
        XCTAssertNil(HubChildQuiescence.allOf([only]), "a part reaped since is no longer pending")
    }

    func testAStartAndItsRecoveryThatBothCannotBeStoppedHoldTheOwnerUntilBothAreGone() {
        let rig = LifecycleRig(loaded: true)
        _ = rig.run("pause")
        let preflight = HubChildQuiescence()
        preflight.childMayRun()
        let enable = HubChildQuiescence()
        enable.childMayRun()
        rig.preflightResult = .failure(Self.stuckCommand(preflight))
        rig.launchctl.failNext("enable", with: Self.stuckCommand(enable))
        guard case let .failure(error)? = rig.run("start") else { return XCTFail("the start did not fail") }
        let result: Result<String, Error> = .failure(error)
        guard let held = result.pendingQuiescence else { return XCTFail("the report holds its owner") }
        preflight.childReaped()
        XCTAssertFalse(held.isQuiescent, "the recovery command may still run")
        XCTAssertNotNil(result.pendingQuiescence)
        enable.childReaped()
        XCTAssertTrue(held.isQuiescent)
        XCTAssertNil(result.pendingQuiescence)
    }

    private final class Replies2 {
        private let lock = NSLock()
        private var calls = 0
        private(set) var onMain = true
        var count: Int { lock.lock(); defer { lock.unlock() }; return calls }
        func record(_ accepted: Bool) {
            lock.lock(); calls += 1; onMain = onMain && Thread.isMainThread; lock.unlock()
        }
    }
}
