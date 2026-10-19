// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

/// The forbidden-pair matrix of the exclusivity inventory (R-E1F6-2): 13 independent
/// owner families, 91 unordered pairs including the diagonal, 169 ordered cases. Each
/// ordered case holds the first operation at a boundary through its real controller API,
/// asks for the second through its real API, and requires no effect from the second.
extension HubControllerTests {
    private final class Box {
        var completions = 0
        var signIn: HubAccountSignIn?
    }

    private struct Held {
        let family: HubLifecycleFamily
        let name: String
        var options = OwnerRig.Options(loaded: .loaded)
        let begin: (OwnerRig, Box) -> Void
        var finish: ((OwnerRig, Box) -> Void)?
    }

    private struct Ask {
        let family: HubLifecycleFamily
        let name: String
        var deferred = false
        /// Calls the real API. The closure is told whether the answer was a refusal.
        let invoke: (OwnerRig, @escaping (Bool) -> Void) -> Void
    }

    private static let localSource = "postgresql://reader@127.0.0.1:55438/teslamate_snapshot"
    private static let remoteSource = "postgresql://reader@teslamate.local/teslamate"

    private func credentials() -> HubFleetSetupCredentials {
        HubFleetSetupCredentials(accessToken: "a", refreshToken: "r", clientID: "c",
                                 region: "europe_middle_east_and_africa", expiresInSeconds: 3600)
    }

    private func heldScenarios() -> [Held] {
        let creds = credentials()
        let tokens = TeslaAuthTokens(accessToken: "a", refreshToken: "r")
        return [
            Held(family: .start, name: "service start") { rig, box in
                rig.service.hold("start"); rig.controller.startHub { _ in box.completions += 1 }
            },
            Held(family: .start, name: "history-only policy", options: .init(loaded: .loaded, historyOnlyConfig: true)) { rig, box in
                rig.runner.hold("preflight:history"); rig.controller.startHub { _ in box.completions += 1 }
            },
            Held(family: .stop, name: "service stop") { rig, box in
                rig.service.hold("stop"); rig.controller.stopHub { _ in box.completions += 1 }
            },
            Held(family: .restart, name: "service restart") { rig, box in
                rig.service.hold("restart"); rig.controller.restartHub { _ in box.completions += 1 }
            },
            Held(family: .restart, name: "history-only policy", options: .init(loaded: .loaded, historyOnlyConfig: true)) { rig, box in
                rig.runner.hold("preflight:history"); rig.controller.restartHub { _ in box.completions += 1 }
            },
            Held(family: .remove, name: "service remove") { rig, box in
                rig.service.hold("remove"); rig.controller.removeBackgroundService { _ in box.completions += 1 }
            },
            Held(family: .delete, name: "data step") { rig, box in
                rig.service.hold("delete")
                rig.controller.deleteHubAndData { _ in box.completions += 1 }
                spinUntil { rig.service.heldKeys.contains("delete") }
            },
            Held(family: .reconcile, name: "conditional restart", options: .init(loaded: .loaded, change: .replaced)) { rig, box in
                rig.service.hold("reconcile"); rig.controller.reconcileLaunchAgent { box.completions += 1 }
            },
            Held(family: .account, name: "fleet pause") { rig, box in
                rig.service.hold("pause"); rig.controller.configureFleetAccount(credentials: creds) { _ in box.completions += 1 }
            },
            Held(family: .account, name: "fleet setup") { rig, box in
                rig.runner.hold("setup-fleet"); rig.controller.configureFleetAccount(credentials: creds) { _ in box.completions += 1 }
            },
            Held(family: .account, name: "fleet start") { rig, box in
                rig.service.hold("start"); rig.controller.configureFleetAccount(credentials: creds) { _ in box.completions += 1 }
            },
            Held(family: .account, name: "fleet first run setup", options: .init(installed: false)) { rig, box in
                rig.runner.hold("setup-fleet"); rig.controller.configureFleetAccount(credentials: creds) { _ in box.completions += 1 }
            },
            Held(family: .account, name: "legacy pause") { rig, box in
                rig.service.hold("pause"); rig.controller.configureTeslaAccount(tokens: tokens) { _ in box.completions += 1 }
            },
            Held(family: .account, name: "legacy first run setup", options: .init(installed: false)) { rig, box in
                rig.runner.hold("setup"); rig.controller.configureTeslaAccount(tokens: tokens) { _ in box.completions += 1 }
            },
            Held(family: .account, name: "sign-in context", begin: { rig, box in
                if case let .success(signIn) = rig.controller.beginLegacySignIn() { box.signIn = signIn }
            }, finish: { _, box in
                box.signIn?.cancel { box.completions += 1 }
            }),
            Held(family: .diagnostics, name: "pause") { rig, box in
                rig.service.hold("pause"); rig.controller.runFullDiagnostics { _ in box.completions += 1 }
            },
            Held(family: .diagnostics, name: "checks") { rig, box in
                rig.runner.hold("doctor"); rig.controller.runFullDiagnostics { _ in box.completions += 1 }
            },
            Held(family: .diagnostics, name: "resume") { rig, box in
                rig.service.hold("start"); rig.controller.runFullDiagnostics { _ in box.completions += 1 }
            },
            Held(family: .diagnostics, name: "unloaded checks", options: .init(loaded: .unloaded)) { rig, box in
                rig.runner.hold("doctor"); rig.controller.runFullDiagnostics { _ in box.completions += 1 }
            },
            Held(family: .signOut, name: "stop") { rig, box in
                rig.service.hold("stop"); rig.controller.signOutTeslaAccount { _ in box.completions += 1 }
            },
            Held(family: .signOut, name: "control") { rig, box in
                rig.runner.hold("control"); rig.controller.signOutTeslaAccount { _ in box.completions += 1 }
            },
            Held(family: .signOut, name: "final refresh") { rig, box in
                rig.runner.hold("status"); rig.controller.signOutTeslaAccount { _ in box.completions += 1 }
                spinUntil(3) { rig.runner.heldKeys.contains("status") }
            },
            Held(family: .migration, name: "local compatibility", options: .init(installed: false)) { rig, box in
                rig.runner.hold("teslamate-check:history")
                rig.controller.checkTeslaMateCompatibility(source: Self.localSource, carID: "1", passwordFile: "/p",
                                                           acknowledgeV42CompatibleSchema: true, historyOnly: true) { _ in box.completions += 1 }
            },
            Held(family: .migration, name: "connected compatibility", options: .init(installed: false)) { rig, box in
                rig.runner.hold("teslamate-check")
                rig.controller.checkTeslaMateCompatibility(source: Self.remoteSource, carID: "1", passwordFile: "/p",
                                                           acknowledgeV42CompatibleSchema: true) { _ in box.completions += 1 }
            },
            Held(family: .migration, name: "history-only import stop", options: .init(installed: false)) { rig, box in
                rig.service.hold("stop")
                rig.controller.importTeslaMateHistoryOnlyOnline(source: Self.localSource, carID: "1", passwordFile: "/p",
                                                                acknowledgeV42CompatibleSchema: true) { _ in box.completions += 1 }
            },
            Held(family: .migration, name: "online import", options: .init(installed: false)) { rig, box in
                rig.runner.hold("migrate")
                rig.controller.importTeslaMateOnline(source: Self.remoteSource, carID: "1", passwordFile: "/p", encryptionKeyFile: "/k",
                                                     acknowledgeV42CompatibleSchema: true) { _ in box.completions += 1 }
            },
            Held(family: .migration, name: "ssh preparation") { rig, box in
                rig.connector.hold()
                _ = rig.controller.connectMigrationServer(host: "teslamate.local", user: "user", port: 22,
                                                          authentication: .key(identityFile: nil),
                                                          usePasswordlessSudo: false) { _ in box.completions += 1 }
            },
            Held(family: .verification, name: "stop", options: .init(loaded: .loaded, marker: "awaiting_verification", markerInterval: 60)) { rig, box in
                rig.service.hold("stop"); rig.controller.runOnboardingChecks(expectRunning: false) { _ in box.completions += 1 }
            },
            Held(family: .verification, name: "doctor", options: .init(loaded: .loaded, marker: "awaiting_verification", markerInterval: 60)) { rig, box in
                rig.runner.hold("doctor"); rig.controller.runOnboardingChecks(expectRunning: false) { _ in box.completions += 1 }
            },
            Held(family: .verification, name: "history-only preflight",
                 options: .init(loaded: .loaded, marker: "awaiting_verification", markerHistoryOnly: true)) { rig, box in
                rig.runner.hold("preflight:history"); rig.controller.runOnboardingChecks(expectRunning: false) { _ in box.completions += 1 }
            },
            Held(family: .verification, name: "live status") { rig, box in
                rig.runner.hold("status"); rig.controller.runOnboardingChecks(expectRunning: true) { _ in box.completions += 1 }
                spinUntil(3) { rig.runner.heldKeys.contains("status") }
            },
            Held(family: .handover, name: "start", options: .init(loaded: .loaded, marker: "awaiting_handover", markerInterval: 0)) { rig, box in
                rig.service.hold("start"); rig.controller.acknowledgeMigrationHandoverAndStart { _ in box.completions += 1 }
            },
            Held(family: .handover, name: "readiness", options: .init(loaded: .loaded, marker: "awaiting_handover", markerInterval: 0)) { rig, box in
                rig.runner.hold("status")
                rig.controller.acknowledgeMigrationHandoverAndStart { _ in box.completions += 1 }
                rig.runner.hold("status")
                rig.runner.release("status")
            },
            Held(family: .handover, name: "rollback", options: .init(loaded: .loaded, marker: "awaiting_handover", markerInterval: 0)) { rig, box in
                rig.runner.results["status"] = .failure(HubActionError.commandFailed("not ready"))
                rig.service.hold("stop")
                rig.controller.acknowledgeMigrationHandoverAndStart { _ in box.completions += 1 }
            },
            Held(family: .vehicleControl, name: "control command", options: .init(loaded: .loaded, snapshot: .previewRunning)) { rig, box in
                rig.runner.hold("control"); rig.controller.performVehicleControl(.lock) { _ in box.completions += 1 }
            }
        ]
    }

    private func asks() -> [Ask] {
        let creds = credentials()
        let tokens = TeslaAuthTokens(accessToken: "a", refreshToken: "r")
        func outcome(_ done: @escaping (Bool) -> Void) -> (Result<Void, Error>) -> Void {
            { if case .failure = $0 { done(true) } else { done(false) } }
        }
        return [
            Ask(family: .start, name: "Start") { rig, done in rig.controller.startHub(completion: outcome(done)) },
            Ask(family: .stop, name: "Stop") { rig, done in rig.controller.stopHub(completion: outcome(done)) },
            Ask(family: .restart, name: "Restart") { rig, done in rig.controller.restartHub(completion: outcome(done)) },
            Ask(family: .remove, name: "Remove") { rig, done in rig.controller.removeBackgroundService(completion: outcome(done)) },
            Ask(family: .delete, name: "Delete") { rig, done in rig.controller.deleteHubAndData { if case .failure = $0 { done(true) } else { done(false) } } },
            Ask(family: .reconcile, name: "Reconcile", deferred: true) { rig, done in rig.controller.reconcileLaunchAgent { done(false) } },
            Ask(family: .account, name: "Fleet") { rig, done in rig.controller.configureFleetAccount(credentials: creds, completion: outcome(done)) },
            Ask(family: .account, name: "Legacy token") { rig, done in rig.controller.configureTeslaAccount(tokens: tokens, completion: outcome(done)) },
            Ask(family: .account, name: "Sign-in context") { rig, done in
                switch rig.controller.beginLegacySignIn() {
                case .failure: done(true)
                case let .success(signIn): done(false); signIn.cancel()
                }
            },
            Ask(family: .diagnostics, name: "Diagnostics") { rig, done in rig.controller.runFullDiagnostics { done($0.contains("not run")) } },
            Ask(family: .signOut, name: "Sign-out") { rig, done in rig.controller.signOutTeslaAccount(completion: outcome(done)) },
            Ask(family: .migration, name: "Local compatibility") { rig, done in
                rig.controller.checkTeslaMateCompatibility(source: Self.localSource, carID: "1", passwordFile: "/p",
                                                           acknowledgeV42CompatibleSchema: true, historyOnly: true) {
                    if case .failure = $0 { done(true) } else { done(false) }
                }
            },
            Ask(family: .migration, name: "History-only import") { rig, done in
                rig.controller.importTeslaMateHistoryOnlyOnline(source: Self.localSource, carID: "1", passwordFile: "/p",
                                                                acknowledgeV42CompatibleSchema: true, completion: outcome(done))
            },
            Ask(family: .migration, name: "Online import") { rig, done in
                rig.controller.importTeslaMateOnline(source: Self.remoteSource, carID: "1", passwordFile: "/p", encryptionKeyFile: "/k",
                                                     acknowledgeV42CompatibleSchema: true, completion: outcome(done))
            },
            Ask(family: .migration, name: "SSH preparation") { rig, done in
                _ = rig.controller.connectMigrationServer(host: "teslamate.local", user: "user", port: 22,
                                                          authentication: .key(identityFile: nil),
                                                          usePasswordlessSudo: false) { if case .failure = $0 { done(true) } else { done(false) } }
            },
            Ask(family: .verification, name: "Stopped verification") { rig, done in
                rig.controller.runOnboardingChecks(expectRunning: false) { if case .failure = $0 { done(true) } else { done(false) } }
            },
            Ask(family: .verification, name: "Live verification") { rig, done in
                rig.controller.runOnboardingChecks(expectRunning: true) { if case .failure = $0 { done(true) } else { done(false) } }
            },
            Ask(family: .handover, name: "Handover") { rig, done in rig.controller.acknowledgeMigrationHandoverAndStart(completion: outcome(done)) },
            Ask(family: .vehicleControl, name: "Vehicle control") { rig, done in rig.controller.performVehicleControl(.unlock, completion: outcome(done)) }
        ]
    }

    func runForbiddenPairMatrix() throws {
        var covered = Set<String>()
        let held = heldScenarios()
        let asked = asks()
        XCTAssertEqual(Set(held.map(\.family)).count, 13)
        XCTAssertEqual(Set(asked.map(\.family)).count, 13)
        var scenarios = 0
        for first in held {
            for second in asked {
                let label = "\(first.family.letter)(\(first.name)) then \(second.family.letter)(\(second.name))"
                let rig = try OwnerRig(first.options)
                defer { rig.tearDown() }
                let box = Box()

                // 1. The first operation is held at its boundary, through its real API.
                first.begin(rig, box)
                XCTAssertEqual(rig.capabilities.owner, first.family, "\(label): the first operation holds the lifecycle")
                XCTAssertEqual(box.completions, 0, label)

                // 2. The second is asked for; nothing it could do happens.
                let before = rig.signature()
                var answered: Bool?
                second.invoke(rig) { answered = $0 }
                if second.deferred {
                    if first.family == .delete {
                        XCTAssertEqual(answered, false, "\(label): reconciliation adds no work once Delete owns the lifecycle")
                    } else {
                        XCTAssertNil(answered, "\(label): a deferred request waits")
                    }
                } else {
                    XCTAssertEqual(answered, true, "\(label): refused at once")
                }
                XCTAssertEqual(rig.signature(), before, "\(label): the second operation had an effect")
                XCTAssertEqual(rig.capabilities.owner, first.family, "\(label): the owner is unchanged")
                if first.family == .delete { XCTAssertEqual(rig.capabilities.state, .deleting, label) }

                // 3. The first operation settles: one completion, one release.
                first.finish?(rig, box)
                rig.drain()
                spinUntil(2) { box.completions >= 1 }
                XCTAssertEqual(box.completions, 1, "\(label): the first operation completes once")
                XCTAssertEqual(rig.releases(first.family), rig.acquisitions(first.family),
                               "\(label): every acquisition was released exactly once")
                XCTAssertNil(rig.capabilities.owner, "\(label): no owner remains")
                XCTAssertTrue(rig.service.heldKeys.isEmpty && rig.runner.heldKeys.isEmpty, label)

                // 4. What the second asked for.
                if second.deferred {
                    spinUntil(2) { answered != nil }
                    XCTAssertEqual(answered, false, "\(label): the deferred request hears exactly one completion")
                    let ensured = rig.events.all.contains("ensure")
                    if first.family == .delete {
                        XCTAssertFalse(ensured, "\(label): nothing is written once the data step has run")
                    } else if first.family != .remove && first.family != .stop {
                        XCTAssertTrue(ensured, "\(label): the deferred request ran once the lifecycle was free")
                    }
                    if first.family == .reconcile {
                        XCTAssertEqual(rig.acquisitions(.reconcile), 2, label)
                    }
                } else if first.family == .delete {
                    XCTAssertEqual(rig.capabilities.state, .deleted, label)
                    var again: Bool?
                    let afterDelete = rig.signature()
                    second.invoke(rig) { again = $0 }
                    XCTAssertEqual(again, true, "\(label): nothing is admitted after a completed Delete")
                    XCTAssertEqual(rig.signature(), afterDelete, label)
                } else {
                    let admittedBefore = rig.acquisitions(second.family)
                    // Sign-out is also refused, before admission, while a migration handover is
                    // pending: that is its own reviewed rule, not the lifecycle's.
                    let handoverRule = second.family == .signOut && rig.markerExists
                    var retried: Bool?
                    second.invoke(rig) { retried = $0 }
                    if !handoverRule {
                        XCTAssertEqual(rig.acquisitions(second.family), admittedBefore + 1,
                                       "\(label): the refused request is admitted once the first has settled")
                    }
                    _ = retried
                    rig.drain()
                    XCTAssertNil(rig.capabilities.owner, "\(label): the retry also settled")
                }
                covered.insert("\(first.family.rawValue)>\(second.family.rawValue)")
                scenarios += 1
            }
        }
        XCTAssertEqual(covered.count, 169, "every ordered pair of the 13 families is exercised")
        XCTAssertGreaterThanOrEqual(scenarios, 169)
    }
}
