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
///    running) before the earlier property list is removed.
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
    /// arguments and returns its exit status and standard output.
    static func handOver(run: ([String]) -> (Int32, Data),
                         installAgent: () throws -> Void,
                         prepareLogs: () throws -> Void) -> Outcome {
        let (status, output) = run([verb])
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
        let (completed, completion) = run([verb, "--complete"] + (report.start ? ["--start"] : []))
        guard completed == 0 else {
            return .retry(failureDetail(try? JSONDecoder().decode(StepReport.self, from: completion)))
        }
        return .proceed
    }

    private static var deferredMessage: String {
        HubL10n.text("hub.rename.deferred",
                     fallback: "The Hub's data is still in use by its earlier service. Wait a moment, then try again.")
    }

    private static func failureDetail(_ report: StepReport?) -> String {
        report?.detail ?? HubL10n.text("hub.rename.failed",
                                       fallback: "The Hub could not move to its new name. Nothing was lost.")
    }

    // MARK: - Start-up

    /// The product's name as this build spells it; the scripted rename changes it.
    static let productName = "Teslatlas Hub"

    /// Whether this build carries the current spelling. Before the swap it is
    /// false and the start-up step changes nothing: no environment variable,
    /// no settings and no helper run.
    static var isRenamedBuild: Bool { earlier(productName) != nil }

    /// Whether this process is the installed app (not a test host, preview or source run).
    static func appliesTo(environment: [String: String]) -> Bool {
        !HubTestHost.isXCTest(environment: environment)
            && NSClassFromString("XCTestCase") == nil
            && environment[DevelopmentHubConfiguration.enableVariable] != "1"
            && environment["TESLATLAS_HUB_UI_PREVIEW"] != "1"
            && environment["TESLATLAS_HUB_PREVIEW_SCENE"] == nil
            && environment["TESLATLAS_HUB_TEST_MODE"] != "1"
    }

    /// The start-up's effects, injected by tests.
    struct StartUp {
        var environment: () -> [String: String]
        var setEnvironment: (String, String) -> Void
        var adoptPreferences: () -> Void
        var handOver: () -> Void

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
    static func prepareBeforeLaunch(renamed: Bool = isRenamedBuild, steps: StartUp = .live) {
        guard renamed else { return }
        for (name, value) in environmentAdditions(steps.environment()) {
            steps.setEnvironment(name, value)
        }
        guard appliesTo(environment: steps.environment()) else { return }
        steps.adoptPreferences()
        steps.handOver()
    }

    private static func handOverUntilDone() {
        guard let binary = Bundle.main.url(forResource: "teslatlas-hub", withExtension: nil) else { return }
        let home = HubTestHost.defaultRealHome()
        let writer = HubLaunchAgentWriter(configuration: {
            try HubLaunchAgentConfiguration.production(bundle: .main, home: home)
        })
        while true {
            let outcome = handOver(run: { runBinary(binary, $0) },
                                   installAgent: { _ = try writer.ensureInstalled(requireConfig: false) },
                                   prepareLogs: { try writer.prepareLogsForBootstrap() })
            guard case let .retry(detail) = outcome else { return }
            HubAppLog.shared.record("rename.handover.retry", category: "app", fields: ["detail": detail])
            guard offerRetry(detail) else { exit(0) }
        }
    }

    private static func runBinary(_ binary: URL, _ arguments: [String]) -> (Int32, Data) {
        let process = Process()
        process.executableURL = binary
        process.arguments = arguments
        let output = Pipe()
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
        } catch {
            return (1, Data())
        }
        let data = output.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return (process.terminationStatus, data)
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
