// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

/// Spins the main run loop until the condition holds (a real-time watchdog only).
func spinUntil(_ seconds: TimeInterval = 5, _ condition: () -> Bool) {
    let limit = Date().addingTimeInterval(seconds)
    while !condition(), Date() < limit { RunLoop.current.run(until: Date().addingTimeInterval(0.005)) }
}

func spinBriefly(_ seconds: TimeInterval = 0.05) {
    RunLoop.current.run(until: Date().addingTimeInterval(seconds))
}

/// A service double that records every call with its owner and can hold the completion of
/// one named action at a time until the test releases it. Holds are one-shot.
final class OwnerService: HubServiceControlling {
    private let lock = NSLock()
    private var callLog: [String] = []
    private var ownerLog: [HubLifecycleToken] = []
    private var heldThunks: [(key: String, release: () -> Void)] = []
    private var holdKeys: Set<String> = []
    private var recordOwner: HubLifecycleToken?

    var loaded: HubServiceLoadState = .unloaded
    var results: [String: Result<String, Error>] = [:]
    var deleteResult: Result<HubDataRemovalReport, Error> = .success(HubDataRemovalReport(deleted: ["D1"], kept: [], warnings: []))
    var onCall: ((String) -> Void)?
    /// The `ensure` step of a reconcile runs when it is asked to; the change it reports
    /// decides whether the double restarts a loaded job.
    var restartDuty = false
    /// F8-6: the queue the real service controller completes on (its own serial queue). When set, the
    /// completion of a `run` and of a recovery is delivered there, off the main thread.
    var completionQueue: DispatchQueue?

    private func deliver(_ block: @escaping () -> Void) {
        if let completionQueue { completionQueue.async(execute: block) } else { block() }
    }

    var calls: [String] { lock.lock(); defer { lock.unlock() }; return callLog }
    var owners: [HubLifecycleToken] { lock.lock(); defer { lock.unlock() }; return ownerLog }
    func count(_ call: String) -> Int { calls.filter { $0 == call }.count }

    func hold(_ keys: String...) { lock.lock(); holdKeys.formUnion(keys); lock.unlock() }

    var heldKeys: [String] { lock.lock(); defer { lock.unlock() }; return heldThunks.map(\.key) }

    @discardableResult
    func release(_ key: String) -> Bool {
        lock.lock()
        guard let index = heldThunks.firstIndex(where: { $0.key == key }) else { lock.unlock(); return false }
        let thunk = heldThunks.remove(at: index)
        lock.unlock()
        thunk.release()
        return true
    }

    @discardableResult
    func releaseAll() -> Bool {
        var any = false
        while true {
            lock.lock()
            guard !heldThunks.isEmpty else { lock.unlock(); return any }
            let thunk = heldThunks.removeFirst()
            lock.unlock()
            thunk.release()
            any = true
        }
    }

    private func record(_ call: String, _ owner: HubLifecycleToken?) {
        lock.lock()
        callLog.append(call)
        if let owner { ownerLog.append(owner) }
        lock.unlock()
        onCall?(call)
    }

    private func holdIfAsked(_ key: String, _ thunk: @escaping () -> Void) -> Bool {
        lock.lock()
        guard holdKeys.remove(key) != nil else { lock.unlock(); return false }
        heldThunks.append((key, thunk))
        lock.unlock()
        return true
    }

    // MARK: HubServiceControlling

    var restartDutyOutstanding: Bool { lock.lock(); defer { lock.unlock() }; return restartDuty }
    private var dutyCallbacks: [() -> Void] = []
    func whenRestartDutySettled(_ callback: @escaping () -> Void) {
        lock.lock()
        if restartDuty { dutyCallbacks.append(callback); lock.unlock(); return }
        lock.unlock()
        callback()
    }
    /// The owed restart has been performed: waiting callers hear of it.
    func settleRestartDuty() {
        lock.lock(); restartDuty = false; let callbacks = dutyCallbacks; dutyCallbacks = []; lock.unlock()
        callbacks.forEach { $0() }
    }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        record("loaded", nil)
        completion(loaded)
    }

    func run(arguments: [String], owner: HubLifecycleToken,
             completion: @escaping (Result<String, Error>) -> Void) {
        let action = arguments.last ?? ""
        record("run:" + action, owner)
        let result = results[action] ?? .success("")
        let finish = { [self] in
            lock.lock()
            if action == "pause", case .success = result { recordOwner = owner }
            else if ["start", "restart", "stop", "remove"].contains(action), recordOwner == owner { recordOwner = nil }
            lock.unlock()
            deliver { completion(result) }
        }
        if !holdIfAsked(action, finish) { finish() }
    }

    func recoverAfterPause(owner: HubLifecycleToken,
                           completion: @escaping (Result<String, Error>) -> Void) {
        record("recover", owner)
        lock.lock(); if recordOwner == owner { recordOwner = nil }; lock.unlock()
        deliver { completion(.success("")) }
    }

    func releaseRestartRecord(owner: HubLifecycleToken) {
        record("release", owner)
        lock.lock(); if recordOwner == owner { recordOwner = nil }; lock.unlock()
    }

    func restartRecordHeld(by owner: HubLifecycleToken) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return recordOwner == owner
    }

    func deleteHubAndData(owner: HubLifecycleToken,
                          completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        record("delete", owner)
        let result = deleteResult
        if !holdIfAsked("delete", { completion(result) }) { completion(result) }
    }

    func reconcile(owner: HubLifecycleToken,
                   ensure: @escaping () throws -> HubLaunchAgentChange,
                   completion: @escaping (HubReconcileOutcome) -> Void) {
        record("reconcile", owner)
        let change: HubLaunchAgentChange
        do { change = try ensure() } catch { completion(.writerFailed(error)); return }
        let finish: () -> Void = { [self] in
            guard change != .unchanged, case .loaded = loaded else { completion(.completed); return }
            run(arguments: ["service", "restart"], owner: owner) { result in
                if case let .failure(error) = result { completion(.restartFailed(error)) } else { completion(.completed) }
            }
        }
        if !holdIfAsked("reconcile", finish) { finish() }
    }
}

/// A command runner double: records every command and holds a named one on request.
final class OwnerRunner: HubCommandRunning {
    static let compatible = #"{"status":"compatible","reasonCode":"v4_2_compatible_schema","requiredVersion":"4.2.0","guidance":"Ready."}"#
    static let historyCompatible = #"{"status":"compatible","reasonCode":"v4_2_compatible_schema","requiredVersion":"4.2.0","guidance":"Ready.","credentialScope":"history-only","applicationVersionStatus":"unknown"}"#
    static let historyReady = #"{"status":"ready","credentialScope":"history-only","publishedVehicles":1,"collectorEnabled":false}"#
    static let imported = #"{"status":"imported","captureMode":"online-snapshot"}"#
    static let ready = #"{"status":"ok","ready":true}"#
    static var runningFleetStatus: String {
        #"{"status":"ok","version":""# + HubRelease.bundledVersion
            + #"","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1048576},"ready":true,"provider":"fleet","vehicles":[{"vehicleId":"B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5","displayName":"Aurora"}],"credentials":{"present":true}}"#
    }

    private let lock = NSLock()
    private var commandLog: [[String]] = []
    private var heldThunks: [(key: String, release: () -> Void)] = []
    private var holdKeys: Set<String> = []
    var results: [String: Result<String, Error>] = [:]
    /// Commands whose completion is delivered twice (a duplicate or stale callback).
    var duplicates: Set<String> = []
    var onCommand: ((String) -> Void)?

    var commands: [[String]] { lock.lock(); defer { lock.unlock() }; return commandLog }
    var names: [String] { commands.map(Self.key) }
    func count(_ key: String) -> Int { names.filter { $0 == key }.count }

    static func key(_ arguments: [String]) -> String {
        let name = arguments.first { !$0.hasPrefix("-") && !$0.hasSuffix("config.toml") } ?? ""
        return name + (arguments.contains("--history-only") ? ":history" : "")
    }

    func hold(_ keys: String...) { lock.lock(); holdKeys.formUnion(keys); lock.unlock() }
    var heldKeys: [String] { lock.lock(); defer { lock.unlock() }; return heldThunks.map(\.key) }

    @discardableResult
    func release(_ key: String) -> Bool {
        lock.lock()
        guard let index = heldThunks.firstIndex(where: { $0.key == key }) else { lock.unlock(); return false }
        let thunk = heldThunks.remove(at: index)
        lock.unlock()
        thunk.release()
        return true
    }

    @discardableResult
    func releaseAll() -> Bool {
        var any = false
        while true {
            lock.lock()
            guard !heldThunks.isEmpty else { lock.unlock(); return any }
            let thunk = heldThunks.removeFirst()
            lock.unlock()
            thunk.release()
            any = true
        }
    }

    private func defaultResult(_ key: String) -> Result<String, Error> {
        switch key {
        case "teslamate-check": return .success(Self.compatible)
        case "teslamate-check:history": return .success(Self.historyCompatible)
        case "preflight:history": return .success(Self.historyReady)
        case "migrate", "migrate:history": return .success(Self.imported)
        case "status": return .success(Self.ready)
        default: return .success("{}")
        }
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        let key = Self.key(arguments)
        lock.lock(); commandLog.append(arguments); lock.unlock()
        onCommand?(key)
        let result = results[key] ?? defaultResult(key)
        let finish = { [self] in
            completion(result)
            if duplicates.contains(key) { completion(result) }
        }
        lock.lock()
        let held = holdKeys.remove(key) != nil
        if held { heldThunks.append((key, finish)) }
        lock.unlock()
        if !held { finish() }
    }
}

final class RecordingBarrier: HubRestorationBarrier {
    private let lock = NSLock()
    private var log: [String] = []
    var events: [String] { lock.lock(); defer { lock.unlock() }; return log }
    func install() { lock.lock(); log.append("install"); lock.unlock() }
    func applyToExistingWindows() { lock.lock(); log.append("apply"); lock.unlock() }
    func remove() { lock.lock(); log.append("remove"); lock.unlock() }
}

/// The SSH connection double: it creates real import sessions (a directory with its owner
/// lock, a process that is never launched) so cleanup is observable on disk.
final class OwnerConnector {
    let root: URL
    private let lock = NSLock()
    private var heldThunks: [() -> Void] = []
    private var shouldHold = false
    private(set) var callCount = 0
    private(set) var created: [TeslaMateServerImportSession] = []
    private(set) var directories: [URL] = []
    var failure: Error?

    init() throws {
        root = HubTestScratch.directory
            .appendingPathComponent("hub-owner-connector-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
    }

    func hold() { lock.lock(); shouldHold = true; lock.unlock() }
    var isHolding: Bool { lock.lock(); defer { lock.unlock() }; return !heldThunks.isEmpty }

    func releaseAll() {
        lock.lock(); let thunks = heldThunks; heldThunks = []; lock.unlock()
        thunks.forEach { $0() }
    }

    func tearDown() {
        created.forEach { $0.close() }
        try? FileManager.default.removeItem(at: root)
    }

    var connector: HubMigrationConnector {
        { [unowned self] _, _, _, _, _, completion in
            lock.lock(); callCount += 1; lock.unlock()
            let finish: () -> Void = { [self] in
                if let failure { completion(.failure(failure)); return }
                do {
                    let owner = try TeslaMateImportDirectoryOwner.create(in: root)
                    let session = TeslaMateServerImportSession(
                        source: "postgresql://reader@127.0.0.1:5432/teslamate",
                        carID: "1",
                        passwordFile: owner.directory.appendingPathComponent("password"),
                        encryptionKeyFile: owner.directory.appendingPathComponent("key"),
                        teslaMateVersion: "4.2.0",
                        tunnel: Process(),
                        temporaryDirectory: owner.directory,
                        directoryOwner: owner)
                    lock.lock(); created.append(session); directories.append(owner.directory); lock.unlock()
                    completion(.success(session))
                } catch {
                    completion(.failure(error))
                }
            }
            lock.lock()
            let held = shouldHold
            if held { shouldHold = false; heldThunks.append(finish) }
            lock.unlock()
            if !held { finish() }
        }
    }
}

/// A production-mode controller over the doubles, in a throwaway home. Nothing here can
/// reach the real launchd, home or preferences.
final class OwnerRig {
    let home: URL
    let service = OwnerService()
    let runner = OwnerRunner()
    let events = LifecycleEvents()
    let agent: FakeLaunchAgent
    let barrier = RecordingBarrier()
    let connector: OwnerConnector
    let status = HubStatusProcessLifecycle()
    private(set) var controller: HubController!
    private(set) var cleanerCalls = 0

    var configPath: URL { home.appendingPathComponent("Library/Application Support/Teslatlas Hub/config.toml") }
    var markerPath: URL { configPath.deletingLastPathComponent().appendingPathComponent(".teslamate-handover-pending") }

    struct Options {
        var installed = true
        var loaded: HubServiceLoadState = .unloaded
        var config = true
        var historyOnlyConfig = false
        var marker: String?                 // phase raw value
        var markerHistoryOnly = false
        var markerInterval = 0
        var snapshot: HubSnapshot?
        var change: HubLaunchAgentChange = .unchanged
        /// Use the production restoration barrier (it flips the process-wide suppression).
        var realBarrier = false
        /// A running Fleet Hub with one vehicle answers `status` (otherwise it fails).
        var runningStatus = false
    }

    init(_ options: Options = Options()) throws {
        home = HubTestScratch.directory
            .appendingPathComponent("hub-owner-rig-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        connector = try OwnerConnector()
        agent = FakeLaunchAgent(events: events)
        agent.installed = true      // the service file; `serviceInstalledOverride` carries "installed"
        agent.change = options.change
        service.loaded = options.loaded
        let folder = configPath.deletingLastPathComponent()
        if options.config || options.marker != nil {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        }
        if options.config {
            try "data_dir = \"/tmp/hub\"\n\n[collector]\nprovider = \"legacy\"\ninterval_seconds = 60\n"
                .write(to: configPath, atomically: true, encoding: .utf8)
        }
        if options.historyOnlyConfig { try HistoryOnlyControl.write(for: configPath) }
        if let phase = options.marker {
            try """
            {"phase":"\(phase)","previousIntervalSeconds":\(options.markerInterval),"previousProvider":"legacy","historyOnly":\(options.markerHistoryOnly)}
            """.write(to: markerPath, atomically: true, encoding: .utf8)
            if options.markerHistoryOnly { try HistoryOnlyControl.write(for: configPath) }
        }
        if options.runningStatus { runner.results["status"] = .success(OwnerRunner.runningFleetStatus) }
        controller = HubController(
            environment: [:],
            commandRunner: runner,
            serviceRunner: service,
            launchAgent: agent,
            dataRemover: nil,
            homeDirectory: home,
            serviceInstalledOverride: options.installed,
            initialSnapshot: options.snapshot,
            migrationStartupReadinessPollInterval: 0.001,
            migrationStartupReadinessTimeout: 5,
            migrationStartupReadinessMaxAttempts: 3,
            statusLifecycle: status,
            restorationBarrier: options.realBarrier ? HubWindowRestorationBarrier() : barrier,
            staleImportCleaner: { [unowned self] in cleanerCalls += 1; return .init() },
            migrationConnector: connector.connector)
    }

    func tearDown() {
        drain()
        connector.tearDown()
        try? FileManager.default.removeItem(at: home)
        HubWindowRestoration.endSuppressing()
    }

    var configText: String { (try? String(contentsOf: configPath, encoding: .utf8)) ?? "<none>" }
    var markerExists: Bool { FileManager.default.fileExists(atPath: markerPath.path) }

    /// Everything an operation could change, in one comparable value.
    func signature() -> [String] {
        service.calls.map { "service:" + $0 }
            + runner.names.map { "command:" + $0 }
            + events.all.map { "agent:" + $0 }
            + barrier.events.map { "barrier:" + $0 }
            + ["config:" + configText, "marker:\(markerExists)", "connect:\(connector.callCount)",
               "cleaner:\(cleanerCalls)", "statusPaused:\(status.isPaused)"]
    }

    var capabilities: HubLifecycleCapabilities { controller.lifecycleCapabilities }

    /// Releases every held step until the lifecycle is idle (or terminally deleted).
    func drain(_ seconds: TimeInterval = 8) {
        let limit = Date().addingTimeInterval(seconds)
        var quiet = 0
        while Date() < limit {
            let moved = service.releaseAll() || runner.releaseAll() || connector.isHolding
            connector.releaseAll()
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
            let state = capabilities
            let settled = (state.owner == nil)
                && service.heldKeys.isEmpty && runner.heldKeys.isEmpty && !connector.isHolding
            if settled && !moved { quiet += 1 } else { quiet = 0 }
            if quiet >= 4 { return }
        }
    }

    var journal: [HubLifecycleAuthority.Event] { controller.lifecycle.journal }
    func acquisitions(_ family: HubLifecycleFamily) -> Int {
        journal.filter { if case .acquired(family, _) = $0 { return true } else { return false } }.count
    }
    func releases(_ family: HubLifecycleFamily) -> Int {
        journal.filter { if case .released(family, _, _) = $0 { return true } else { return false } }.count
    }
}

func isLifecycleRefusal(_ error: Error) -> Bool {
    if case HubActionError.lifecycleRefused = error { return true }
    let text = error.localizedDescription
    return text.contains("Nothing was changed") && (text.contains("busy") || text.contains("already started")
        || text.contains("diagnostics are running") || text.contains("quitting"))
        || text.contains("already in progress")
}


/// A main window over an `OwnerRig`: every confirmation, error, reveal and termination is a
/// recording seam. Nothing here reaches Finder, the real preferences or the process.
final class WindowOwnerRig {
    let rig: OwnerRig
    private(set) var window: MainWindowController!
    private(set) var alerts: [NSAlert] = []
    private(set) var errors: [Error] = []
    private(set) var revealed: [URL] = []
    private(set) var terminateCalls = 0
    private(set) var appearanceWrites: [String] = []
    var answers: [NSApplication.ModalResponse] = []
    /// Called as each confirmation is shown: a test can change the world while it is open.
    var whileConfirming: ((NSAlert) -> Void)?
    private var refreshed = false
    var staleRefusals = 0

    init(_ options: OwnerRig.Options = .init(loaded: .loaded)) throws {
        rig = try OwnerRig(options)
        weak var weakSelf: WindowOwnerRig?
        window = MainWindowController(
            controller: rig.controller,
            serviceTransitionTimeout: 5,
            serviceTransitionPollInterval: 0.05,
            errorPresenter: { weakSelf?.errors.append($0) },
            confirmationPresenter: { alert, silent in
                guard let owner = weakSelf else { return silent }
                owner.alerts.append(alert)
                owner.whileConfirming?(alert)
                return owner.answers.isEmpty ? .alertFirstButtonReturn : owner.answers.removeFirst()
            },
            revealApp: { weakSelf?.revealed.append($0) },
            terminateApp: { weakSelf?.terminateCalls += 1 },
            olderInstallerPresent: { false },
            appearancePreference: HubAppearancePreference(stored: nil, write: { weakSelf?.appearanceWrites.append($0) }),
            onInitialRefresh: { _ in weakSelf?.refreshed = true })
        weakSelf = self
        spinUntil(5) { refreshed }
    }

    func tearDown() {
        window.close()
        rig.tearDown()
    }

    var service: OwnerService { rig.service }
    var runner: OwnerRunner { rig.runner }
    var controller: HubController { rig.controller }

    func allViews(_ view: NSView?) -> [NSView] {
        guard let view else { return [] }
        return [view] + view.subviews.flatMap { allViews($0) }
    }

    func buttons(in view: NSView?) -> [NSButton] {
        var result = allViews(view).compactMap { $0 as? NSButton }
        if view === view?.window?.contentView {
            result += (view?.window?.toolbar?.items.compactMap(\.view) ?? []).flatMap { allViews($0).compactMap { $0 as? NSButton } }
        }
        return result
    }

    func dashboardButton(_ title: String) -> NSButton? {
        buttons(in: window.dashboardView).first { $0.title == title }
    }

    func detailsButton(_ identifier: String, in root: NSView? = nil) -> NSButton? {
        buttons(in: root ?? window.window?.contentView).first { $0.identifier?.rawValue == identifier }
    }

    /// Sends the real action of a private `@objc` selector of the window.
    func send(_ selector: String) {
        _ = window.perform(NSSelectorFromString(selector))
    }

    /// The lifecycle commands the window has issued to the service double.
    var lifecycleCalls: [String] { service.calls.filter { $0.hasPrefix("run:") || $0 == "delete" || $0 == "reconcile" } }
}

/// Answers the alerts that go through `HubUIPresentation` (the account menu's Disconnect
/// confirmation, error alerts).
final class RecordingAlertPresenter: HubAlertPresenting {
    var answer: NSApplication.ModalResponse = .alertSecondButtonReturn
    private(set) var errors: [Error] = []
    private(set) var informations: [NSAlert] = []
    private(set) var confirmations: [NSAlert] = []
    var whileConfirming: ((NSAlert) -> Void)?
    func present(error: Error) { errors.append(error) }
    func present(information alert: NSAlert) { informations.append(alert) }
    func response(to alert: NSAlert, silentResponse: NSApplication.ModalResponse) -> NSApplication.ModalResponse {
        confirmations.append(alert)
        whileConfirming?(alert)
        return answer
    }
}

// MARK: - Route driving helpers (R-E1F6-18)

extension WindowOwnerRig {
    func button(titled title: String, in view: NSView?) -> NSButton? {
        buttons(in: view).first { $0.title == title }
    }

    /// Sends a control's own action to its target, whether or not it looks enabled: the
    /// way a stale or keyboard-driven activation reaches the handler.
    func activate(_ button: NSButton?, file: StaticString = #filePath, line: UInt = #line) {
        guard let button else { return XCTFail("the control was not found", file: file, line: line) }
        if let action = button.action {
            _ = NSApp.sendAction(action, to: button.target ?? window, from: button)
        }
    }

    /// Chooses a button of the confirmation sheet attached to the main window.
    func confirm(_ title: String, file: StaticString = #filePath, line: UInt = #line) {
        guard let sheet = window.window?.attachedSheet else {
            return XCTFail("no confirmation sheet is open", file: file, line: line)
        }
        guard let button = buttons(in: sheet.contentView).first(where: { $0.title == title }) else {
            return XCTFail("the sheet has no \(title) button", file: file, line: line)
        }
        button.performClick(nil)
    }

    var hasSheet: Bool { window.window?.attachedSheet != nil }

    /// A private stored property of the main window (the legacy controls have no accessor).
    func privateValue<T>(_ name: String, as type: T.Type = T.self) -> T? {
        Mirror(reflecting: window!).descendant(name) as? T
    }

    /// What the route left behind, for a failure message.
    var trace: String {
        "service=\(service.calls) commands=\(runner.names) errors=\(errors.map(\.localizedDescription)) "
            + "alerts=\(alerts.map { $0.messageText }) sheet=\(hasSheet)"
    }
}

extension OnboardingWindowControllerTests {
    func descendantViews(of view: NSView?) -> [NSView] {
        guard let view else { return [] }
        return [view] + view.subviews.flatMap { descendantViews(of: $0) }
    }

    func field(placeholder: String, in onboarding: OnboardingWindowController?) throws -> NSTextField {
        try XCTUnwrap(descendantViews(of: onboarding?.window?.contentView).compactMap { $0 as? NSTextField }
            .first { $0.placeholderString == placeholder }, "no field with placeholder \(placeholder)")
    }

    func allButtons(in onboarding: OnboardingWindowController?) -> [NSButton] {
        let root = onboarding?.window?.contentView
        let inContent = descendantViews(of: root).compactMap { $0 as? NSButton }
        let inToolbar = (onboarding?.window?.toolbar?.items.compactMap(\.view) ?? []).flatMap {
            descendantViews(of: $0).compactMap { $0 as? NSButton }
        }
        return inContent + inToolbar
    }

    /// Without failing the test when it is absent.
    func findButton(_ title: String, in onboarding: OnboardingWindowController?) -> NSButton? {
        allButtons(in: onboarding).first { $0.title == title }
    }

    func primaryButton(_ title: String, in onboarding: OnboardingWindowController?) throws -> NSButton {
        try XCTUnwrap(findButton(title, in: onboarding),
                      "no \(title) button; there are \(allButtons(in: onboarding).map(\.title))")
    }

    func press(_ button: NSButton) {
        if let action = button.action { _ = NSApp.sendAction(action, to: button.target, from: button) }
    }
}
