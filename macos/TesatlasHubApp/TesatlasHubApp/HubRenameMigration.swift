// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import Foundation

/// The rename hand-over at start-up (DEC-160), before the app reads any Hub path.
///
/// 1. Environment variables still exported under the earlier spelling are also
///    set under the current one (a set current name always wins).
/// 2. The app's settings are copied once from the earlier bundle identifier's
///    domain; the earlier domain is kept for this release.
/// 3. The bundled service binary runs `migrate-names`: it stops the earlier
///    LaunchAgent, moves the Hub's folders and points the configuration at
///    them. The app then writes the current LaunchAgent with its own writer,
///    and `migrate-names --complete` starts it (when the earlier one was
///    running) before the earlier property list is removed. A current service
///    already serving is not restarted, and one that was not loaded is loaded
///    (it runs at load) without a restart after. The start counts once the
///    binary's `serving-check` confirms the service serves its configuration
///    (between collection windows included); the binary stops waiting and
///    refuses the step after one minute. This runs before the app finishes
///    launching, so the owner sees no window until it ends. The app waits for
///    each run at most `StepRunner.limit`; a run still going then is left alone
///    and the owner is offered Try Again or Quit, and a Try Again while it still
///    runs starts nothing and says the move is still in progress.
/// 4. The app's launch work waits for the hand-over (`LaunchGate`): the retry
///    alert can let AppKit finish launching before `application.run()`. After
///    a hand-over that needed Try Again, the app shows the window a normal
///    launch shows and comes to the front.
///
/// Every step is idempotent. A step that is deferred or fails changes nothing
/// the next attempt cannot finish: the owner is offered Try Again or Quit, and
/// the app never opens on an empty Hub while the earlier data waits. Before the
/// swap (`isRenamedBuild` is false) none of these steps runs at all.
enum HubRenameMigration {
    /// The current and earlier spellings in their three written forms.
    static let forms: [(current: String, earlier: String)] = [
        ("tesatlas", "teslatlas"), // rename:keep
        ("Tesatlas", "Teslatlas"), // rename:keep
        ("TESATLAS", "TESLATLAS"), // rename:keep
    ]

    static let verb = "migrate-names"
    static let deferredExitStatus: Int32 = 75

    /// `name` in the earlier spelling, or nil when it does not carry the current one.
    static func earlier(_ name: String) -> String? {
        swap(name) { ($0.current, $0.earlier) }
    }

    /// `name` in the current spelling, or nil when it does not carry the earlier one.
    static func current(_ name: String) -> String? {
        swap(name) { ($0.earlier, $0.current) }
    }

    private static func swap(_ name: String, _ direction: ((current: String, earlier: String)) -> (String, String)) -> String? {
        var result = name
        var changed = false
        for form in forms {
            let (from, to) = direction(form)
            if result.contains(from) {
                result = result.replacingOccurrences(of: from, with: to)
                changed = true
            }
        }
        return changed ? result : nil
    }

    // MARK: - Environment

    /// The variables to set: each earlier-spelled name whose current name is unset.
    static func environmentAdditions(_ environment: [String: String]) -> [String: String] {
        var additions: [String: String] = [:]
        for (key, value) in environment {
            guard let renamed = current(key), renamed != key, environment[renamed] == nil else { continue }
            additions[renamed] = value
        }
        return additions
    }

    // MARK: - Settings

    /// The key that records, inside the current domain and in the same write
    /// as the adopted values, that the earlier settings were adopted once.
    static let adoptedMarker = "HubRenameSettingsAdopted"

    /// Settings in the earlier bundle identifier's domain, with each key in the
    /// current spelling, merged under the current domain's own values (which
    /// win). `nil` when there is nothing to adopt, or they were adopted before.
    static func preferencesToAdopt(earlier: [String: Any]?, current: [String: Any]?) -> [String: Any]? {
        guard let earlier, !earlier.isEmpty, current?[adoptedMarker] == nil else { return nil }
        var merged = current ?? [:]
        for (key, value) in earlier {
            let renamed = Self.current(key) ?? key
            if merged[renamed] == nil { merged[renamed] = value }
        }
        merged[adoptedMarker] = true
        return merged
    }

    /// Copies the settings once. The earlier domain is kept for this release:
    /// nothing is deleted on a read-back that cannot prove the copy is durable,
    /// and an interrupted copy (no marker yet) is simply made again.
    static func adoptEarlierPreferences(currentDomain: String,
                                        read: (String) -> [String: Any]?,
                                        write: ([String: Any], String) -> Void) {
        guard let earlierDomain = earlier(currentDomain),
              let merged = preferencesToAdopt(earlier: read(earlierDomain), current: read(currentDomain)) else { return }
        write(merged, currentDomain)
    }

    // MARK: - Service hand-over

    struct StepReport: Decodable, Equatable {
        let status: String
        let install: Bool
        let start: Bool
        let detail: String?
    }

    enum Outcome: Equatable {
        case proceed
        case retry(String)
    }

    /// One attempt of step 3. `run` runs the bundled binary with the given
    /// arguments and returns its exit status and standard output, or nil when
    /// that run (or an earlier one) is still going.
    static func handOver(run: ([String]) -> (Int32, Data)?,
                         installAgent: () throws -> Void,
                         prepareLogs: () throws -> Void) -> Outcome {
        guard let prepared = run([verb]) else { return .retry(inProgressMessage) }
        let (status, output) = prepared
        let report = try? JSONDecoder().decode(StepReport.self, from: output)
        guard status == 0, let report else {
            return .retry(status == deferredExitStatus ? (report?.detail ?? deferredMessage) : failureDetail(report))
        }
        guard report.install else { return .proceed }
        do {
            try installAgent()
            if report.start { try prepareLogs() }
        } catch {
            return .retry(error.localizedDescription)
        }
        guard let completing = run([verb, "--complete"] + (report.start ? ["--start"] : [])) else {
            return .retry(inProgressMessage)
        }
        let (completed, completion) = completing
        guard completed == 0 else {
            return .retry(failureDetail(try? JSONDecoder().decode(StepReport.self, from: completion)))
        }
        return .proceed
    }

    private static var deferredMessage: String {
        HubL10n.text("hub.rename.deferred",
                     fallback: "The Hub's data is still in use by its earlier service. Wait a moment, then try again.")
    }

    static var inProgressMessage: String {
        HubL10n.text("hub.rename.inProgress",
                     fallback: "The Hub's move to its new name is still in progress. Wait a moment, then try again.")
    }

    private static func failureDetail(_ report: StepReport?) -> String {
        report?.detail ?? HubL10n.text("hub.rename.failed",
                                       fallback: "The Hub could not move to its new name. Nothing was lost.")
    }

    // MARK: - Start-up

    /// The product's name as this build spells it; the scripted rename changes it.
    static let productName = "Tesatlas Hub"

    /// Whether this build carries the current spelling. Before the swap it is
    /// false and the start-up step changes nothing: no environment variable,
    /// no settings and no helper run.
    static var isRenamedBuild: Bool { earlier(productName) != nil }

    /// Whether this process is the installed app (not a test host, preview or source run).
    static func appliesTo(environment: [String: String]) -> Bool {
        !HubTestHost.isXCTest(environment: environment)
            && NSClassFromString("XCTestCase") == nil
            && environment[DevelopmentHubConfiguration.enableVariable] != "1"
            && environment["TESATLAS_HUB_UI_PREVIEW"] != "1"
            && environment["TESATLAS_HUB_PREVIEW_SCENE"] == nil
            && environment["TESATLAS_HUB_TEST_MODE"] != "1"
    }

    /// The start-up's effects, injected by tests.
    struct StartUp {
        var environment: () -> [String: String]
        var setEnvironment: (String, String) -> Void
        var adoptPreferences: () -> Void
        /// True when the hand-over completed after a Try Again.
        var handOver: () -> Bool

        static var live: StartUp { StartUp(
            environment: { ProcessInfo.processInfo.environment },
            setEnvironment: { setenv($0, $1, 0) },
            adoptPreferences: {
                guard let domain = Bundle.main.bundleIdentifier else { return }
                let defaults = UserDefaults.standard
                HubRenameMigration.adoptEarlierPreferences(currentDomain: domain,
                                                           read: { defaults.persistentDomain(forName: $0) },
                                                           write: { defaults.setPersistentDomain($0, forName: $1) })
            },
            handOver: { HubRenameMigration.handOverUntilDone() }) }
    }

    /// Runs the hand-over in the installed app; called once from `main.swift`.
    /// True when it completed after a Try Again.
    @discardableResult
    static func prepareBeforeLaunch(renamed: Bool = isRenamedBuild, steps: StartUp = .live) -> Bool {
        guard renamed else { return false }
        for (name, value) in environmentAdditions(steps.environment()) {
            steps.setEnvironment(name, value)
        }
        guard appliesTo(environment: steps.environment()) else { return false }
        steps.adoptPreferences()
        return steps.handOver()
    }

    private static func handOverUntilDone() -> Bool {
        guard let binary = Bundle.main.url(forResource: "tesatlas-hub", withExtension: nil) else { return false }
        let home = HubTestHost.defaultRealHome()
        let writer = HubLaunchAgentWriter(configuration: {
            try HubLaunchAgentConfiguration.production(bundle: .main, home: home)
        })
        let runner = StepRunner(executable: binary)
        let retried = attemptUntilDone({
            handOver(run: { runner.run($0) },
                     installAgent: { _ = try writer.ensureInstalled(requireConfig: false) },
                     prepareLogs: { try writer.prepareLogsForBootstrap() })
        }, offerRetry: { detail in
            HubAppLog.shared.record("rename.handover.retry", category: "app", fields: ["detail": detail])
            return offerRetry(detail)
        })
        guard let retried else { exit(0) }
        return retried
    }

    /// Attempts until one proceeds. True when a Try Again was needed, false
    /// when the first attempt proceeded, nil when the owner chose Quit.
    static func attemptUntilDone(_ attempt: () -> Outcome, offerRetry: (String) -> Bool) -> Bool? {
        var retried = false
        while true {
            guard case let .retry(detail) = attempt() else { return retried }
            guard offerRetry(detail) else { return nil }
            retried = true
        }
    }

    /// Holds the app's launch work while the hand-over runs before launch.
    /// The retry alert's modal loop can let AppKit finish launching before
    /// `application.run()`; that launch work is held until the hand-over has
    /// ended, so the app reads no Hub path while the data moves. After a
    /// hand-over that needed Try Again, `present` shows the window a normal
    /// launch shows (the main window, or the welcome for a Hub with no
    /// account) and brings the app to the front, once the launch work has run.
    /// Used from the main thread only.
    final class LaunchGate {
        private var holding = false
        private var held: (() -> Void)?
        private var presentAfterLaunch: (() -> Void)?

        /// From `main.swift`, before the hand-over.
        func hold() { holding = true }

        /// From `applicationDidFinishLaunching`: runs `work` now, or once the
        /// hand-over has ended.
        func launch(_ work: @escaping () -> Void) {
            guard !holding else { held = work; return }
            work()
            if let present = presentAfterLaunch {
                presentAfterLaunch = nil
                present()
            }
        }

        /// From `main.swift`, after the hand-over. Launch work held meanwhile
        /// runs through `schedule` (the main queue, once the application runs),
        /// followed by `present` after a Try Again; launch work still to come
        /// is followed by `present` then.
        func release(afterRetry: Bool,
                     present: @escaping () -> Void,
                     schedule: (@escaping () -> Void) -> Void = { DispatchQueue.main.async(execute: $0) }) {
            holding = false
            guard let work = held else {
                if afterRetry { presentAfterLaunch = present }
                return
            }
            held = nil
            schedule {
                work()
                if afterRetry { present() }
            }
        }
    }

    /// Runs the bundled binary's steps one at a time and waits for each at most
    /// `limit`. A step still running then is left alone (never terminated or
    /// signalled) and the run answers nil; while it still runs, a further run
    /// starts nothing and answers nil too. Used from the main thread only.
    final class StepRunner {
        /// Longer than the binary's own one-minute bound on its confirmation.
        static let limit: TimeInterval = 75

        private let executable: URL
        private let leadingArguments: [String]
        private let limit: TimeInterval
        private var running: Process?

        init(executable: URL, leadingArguments: [String] = [], limit: TimeInterval = StepRunner.limit) {
            self.executable = executable
            self.leadingArguments = leadingArguments
            self.limit = limit
        }

        func run(_ arguments: [String]) -> (Int32, Data)? {
            if let running, running.isRunning { return nil }
            running = nil
            let process = Process()
            process.executableURL = executable
            process.arguments = leadingArguments + arguments
            let output = Pipe()
            process.standardOutput = output
            process.standardError = FileHandle.nullDevice
            let ended = DispatchSemaphore(value: 0)
            process.terminationHandler = { _ in ended.signal() }
            let collected = Collected()
            let drained = DispatchSemaphore(value: 0)
            do {
                try process.run()
            } catch {
                return (1, Data())
            }
            running = process
            let reader = output.fileHandleForReading
            DispatchQueue.global(qos: .userInitiated).async {
                collected.data = reader.readDataToEndOfFile()
                drained.signal()
            }
            let deadline = DispatchTime.now() + limit
            guard ended.wait(timeout: deadline) == .success else { return nil }
            running = nil
            guard drained.wait(timeout: max(deadline, .now() + 1)) == .success else {
                return (process.terminationStatus, Data())
            }
            return (process.terminationStatus, collected.data)
        }

        /// The output a reader thread hands back once it has all of it.
        private final class Collected: @unchecked Sendable {
            var data = Data()
        }
    }

    private static func offerRetry(_ detail: String) -> Bool {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = HubL10n.text("hub.rename.title", fallback: "The Hub is moving to its new name")
        alert.informativeText = detail
        alert.addButton(withTitle: HubL10n.text("hub.rename.retry", fallback: "Try Again"))
        alert.addButton(withTitle: HubL10n.text("hub.AppDelegate.229.54", fallback: "Quit"))
        return alert.runModal() == .alertFirstButtonReturn
    }
}
