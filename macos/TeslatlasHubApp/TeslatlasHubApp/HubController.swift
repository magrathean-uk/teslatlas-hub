// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import Darwin
import Foundation

enum HubRelease {
    static let fallbackVersion = "2026.36.2"
    static let sourceRepository = "https://github.com/magrathean-uk/teslatlas-hub"
    static let licenceExpression = "AGPL-3.0-only"
    static var bundledVersion: String {
        guard let value = Bundle.main.object(forInfoDictionaryKey: "TeslatlasHubVersion") as? String,
              !value.isEmpty,
              !value.contains("$(") else { return fallbackVersion }
        return value
    }
    static var bundledSourceCommit: String? {
        guard let value = Bundle.main.object(forInfoDictionaryKey: "TeslatlasHubSourceCommit") as? String,
              isExactSourceCommit(value) else { return nil }
        return value
    }

    static func isExactSourceCommit(_ value: String) -> Bool {
        value.range(
            of: #"^[0-9a-f]{40}$"#,
            options: .regularExpression
        ) != nil
    }

    static func correspondingSourceURL(for commit: String? = bundledSourceCommit) -> URL? {
        guard let commit, isExactSourceCommit(commit) else { return nil }
        return URL(string: "\(sourceRepository)/tree/\(commit)")
    }

    static func licenceURL(for version: String = bundledVersion) -> URL? {
        guard version.range(
            of: #"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$"#,
            options: .regularExpression
        ) != nil else { return nil }
        return URL(string: "\(sourceRepository)/blob/v\(version)/LICENSE")
    }
}

enum HubShareRedactor {
    private static let replacements: [(pattern: String, template: String)] = [
        ("\u{001B}\\[[0-?]*[ -/]*[@-~]", ""),
        ("[\u{0000}-\u{0008}\u{000B}\u{000C}\u{000E}-\u{001F}\u{007F}]", ""),
        (#"(?i)(authorization\s*[:=]\s*(?:bearer|basic)\s+)[^\s,;]+"#, "$1[redacted]"),
        (#"(?i)(\b(?:access_?token|refresh_?token|ingest_?token|pairing_?token|device_?token|token|client_?secret|api_?key|private_?key|encryption_?key|password|authorization_?code|oauth_?code)\b\s*[\"']?\s*[:=]\s*[\"']?)[^\"'\s,&;}]+"#, "$1[redacted]"),
        (#"(?i)([?&](?:access_token|refresh_token|client_secret|code|state|nonce)=)[^&#\s]+"#, "$1[redacted]"),
        (#"(?i)((?:postgres(?:ql)?|https?)://[^/\s:@]+:)[^@/\s]+(@)"#, "$1[redacted]$2"),
        (#"(?i)(\"(?:display_?name|vehicle_?name)\"\s*:\s*)\"[^\"]*\""#, "$1\"[redacted-name]\""),
        (#"(?i)(\b(?:display_?name|vehicle_?name)\b\s*=\s*)(?:\"[^\"]*\"|[^\s,;]+)"#, "$1[redacted-name]"),
        (#"(?i)(\"(?:location_?name|geofence_?name|geofence)\"\s*:\s*)\"(?:\\.|[^\"\\])*\""#, "$1\"[redacted-location]\""),
        (#"(?im)(\b(?:location_?name|geofence_?name|geofence)\b\s*=\s*)(?:\"(?:\\.|[^\"\\])*\"|'(?:\\.|[^'\\])*'|.*?)(?=\s+(?:[a-z_][a-z0-9_]*\s*=)|\s*[,;]\s*(?:[a-z_][a-z0-9_]*\s*=)|[\r\n]|$)"#, "$1[redacted-location]"),
        (#"(?i)(\b(?:vehicle_?id|source_?car_?id|selected_?car_?id|car_?id|tesla_?eid)\b\s*[\"']?\s*[:=]\s*[\"']?)-?\d+(?![a-z0-9-])"#, "$1[redacted-id]"),
        (#"(?i)(\b(?:latitude|longitude|lat|lon)\b\s*[\"']?\s*[:=]\s*[\"']?)-?\d+(?:\.\d+)?"#, "$1[redacted-location]"),
        (#"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b"#, "[redacted-jwt]"),
        (#"(?i)\b[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}\b"#, "[redacted-id]"),
        (#"(?i)\b[0-9A-HJ-NPR-Z]{17}\b"#, "[redacted-vin]"),
        (#"(?i)\b[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,63}\b"#, "[redacted-email]"),
        (#"\b(?:10(?:\.\d{1,3}){3}|192\.168(?:\.\d{1,3}){2}|172\.(?:1[6-9]|2\d|3[01])(?:\.\d{1,3}){2})\b"#, "[redacted-private-ip]"),
        (#"(?<!\d)(?!127\.)(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)(?!\d)"#, "[redacted-ip]"),
        (#"(?i)\[(?:(?:f[cd][0-9a-f]{2}|fe[89ab][0-9a-f]):[0-9a-f:.]+|::1)(?:%[a-z0-9_.-]+)?\](?::\d{1,5})?"#, "[redacted-private-ip]"),
        (#"(?i)\[[0-9a-f]*:[0-9a-f:.]+(?:%[a-z0-9_.-]+)?\](?::\d{1,5})?"#, "[redacted-ip]"),
        (#"(?i)(?<![0-9a-f:])(?:(?:f[cd][0-9a-f]{2}|fe[89ab][0-9a-f]):[0-9a-f:.]+|::1)(?:%[a-z0-9_.-]+)?(?![0-9a-f:])"#, "[redacted-private-ip]")
    ]
    private static let compiledReplacements: [(expression: NSRegularExpression, template: String)] =
        replacements.compactMap { replacement in
            guard let expression = try? NSRegularExpression(pattern: replacement.pattern) else {
                return nil
            }
            return (expression: expression, template: replacement.template)
        }

    static func redact(_ text: String, homeDirectory: String = NSHomeDirectory()) -> String {
        var redacted = homeDirectory.isEmpty
            ? text
            : text.replacingOccurrences(of: homeDirectory, with: "~")
        for replacement in compiledReplacements {
            let range = NSRange(redacted.startIndex..<redacted.endIndex, in: redacted)
            redacted = replacement.expression.stringByReplacingMatches(
                in: redacted,
                range: range,
                withTemplate: replacement.template
            )
        }
        return redacted
    }
}

enum HubHealth: Equatable {
    case running
    case stopped
    case needsInstall
    case degraded

    var title: String {
        switch self {
        case .running: return HubL10n.text("hub.HubController.100.306", fallback: "Hub is running")
        case .stopped: return HubL10n.text("hub.HubController.101.307", fallback: "Hub is stopped")
        case .needsInstall: return HubL10n.text("hub.HubController.102.308", fallback: "Setup required")
        case .degraded: return HubL10n.text("hub.HubController.103.309", fallback: "Attention needed")
        }
    }

    var color: NSColor {
        switch self {
        case .running: return .systemGreen
        case .stopped: return .systemOrange
        case .needsInstall: return .systemBlue
        case .degraded: return .systemRed
        }
    }
}

struct HubActivity {
    let message: String
    let age: String
    let color: NSColor
}

struct HubSetupInvocation: Equatable {
    let arguments: [String]
    let standardInput: String
}

struct HubFleetSetupCredentials: Equatable {
    let accessToken: String
    let refreshToken: String
    let clientID: String
    let region: String
    let expiresInSeconds: Int64
}

struct HubTeslaMateCompatibility: Equatable {
    let compatible: Bool
    let message: String
    let reasonCode: String
    let requiredVersion: String
}

struct HubMigrationProgress: Equatable {
    let completedRows: UInt64
    let totalRows: UInt64
    let phase: String?
}

private struct HubMigrationProgressPayload: Decodable {
    let event: String
    let completedRows: UInt64
    let totalRows: UInt64
    let phase: String?
}

private final class HubMigrationProgressDiagnostics {
    private let processStartedAt = Date()
    private let lock = NSLock()
    private var activePhase: String?
    private var phaseStartedAt: Date?
    private var finished = false

    func received(_ progress: HubMigrationProgress) {
        guard let nextPhase = progress.phase else { return }
        let now = Date()
        var completedPhase: (name: String, duration: TimeInterval)?
        var startedPhase: String?
        lock.lock()
        if !finished, nextPhase != activePhase {
            if let activePhase, let phaseStartedAt {
                completedPhase = (activePhase, now.timeIntervalSince(phaseStartedAt))
            }
            activePhase = nextPhase
            phaseStartedAt = now
            startedPhase = nextPhase
        }
        lock.unlock()
        if let completedPhase {
            Self.logPhaseCompleted(completedPhase.name, duration: completedPhase.duration)
        }
        if let startedPhase {
            HubAppLog.shared.record("import.phase.started", category: "teslamate_import",
                                    fields: ["phase": startedPhase])
        }
    }

    func finish() -> [String: String] {
        let now = Date()
        var completedPhase: (name: String, duration: TimeInterval)?
        lock.lock()
        if !finished {
            finished = true
            if let activePhase, let phaseStartedAt {
                completedPhase = (activePhase, now.timeIntervalSince(phaseStartedAt))
            }
            activePhase = nil
            phaseStartedAt = nil
        }
        lock.unlock()
        if let completedPhase {
            Self.logPhaseCompleted(completedPhase.name, duration: completedPhase.duration)
        }
        return ["duration_ms": String(Int(now.timeIntervalSince(processStartedAt) * 1_000))]
    }

    private static func logPhaseCompleted(_ phase: String, duration: TimeInterval) {
        HubAppLog.shared.record("import.phase.completed", category: "teslamate_import", fields: [
            "phase": phase,
            "duration_ms": String(Int(duration * 1_000))
        ])
    }
}

private final class HubMigrationProgressDelivery {
    private let lock = NSLock()
    private var active = true

    func cancel() {
        lock.lock()
        active = false
        lock.unlock()
    }

    func deliver(_ progress: HubMigrationProgress,
                 to callback: @escaping (HubMigrationProgress) -> Void) {
        lock.lock()
        guard active else {
            lock.unlock()
            return
        }
        DispatchQueue.main.async {
            callback(progress)
        }
        lock.unlock()
    }
}

struct HubOnboardingCheck: Equatable {
    let title: String
    let detail: String
    let passed: Bool
}

enum HubAccountProvider: String, Equatable {
    case legacy
    case fleet

    var displayName: String {
        switch self {
        case .legacy: return HubL10n.text("hub.HubController.250.318", fallback: "Legacy token")
        case .fleet: return HubL10n.text("hub.HubController.251.319", fallback: "Fleet API")
        }
    }
}

enum HubMigrationHandoverPhase: String, Codable, Equatable {
    case importing
    case awaitingVerification = "awaiting_verification"
    case awaitingHandover = "awaiting_handover"
}

private struct HubMigrationHandoverState: Codable {
    var phase: HubMigrationHandoverPhase
    let previousIntervalSeconds: Int
    let previousProvider: String?
    let historyOnly: Bool?

    init(phase: HubMigrationHandoverPhase,
         previousIntervalSeconds: Int,
         previousProvider: String? = nil,
         historyOnly: Bool = false) {
        self.phase = phase
        self.previousIntervalSeconds = previousIntervalSeconds
        self.previousProvider = previousProvider
        self.historyOnly = historyOnly
    }
}

private struct HubCollectorLeaseWitness: Decodable {
    let instanceId: UUID
    let startedAtMs: Int64
    let heartbeatAtMs: Int64
    let leaseUntilMs: Int64

    var isCoherent: Bool {
        startedAtMs >= 0
            && heartbeatAtMs >= startedAtMs
            && leaseUntilMs > heartbeatAtMs
    }
}

private struct HubStartupStatusDocument: Decodable {
    let status: String
    let ready: Bool
    let collector: HubCollectorLeaseWitness?
}

private enum HubCollectorLeaseBaseline {
    case observed(UUID?)
    case unavailable
}

enum HubVehicleControl: String, CaseIterable, Equatable {
    case wake
    case climateStart = "climate-start"
    case climateStop = "climate-stop"
    case lock
    case unlock
    case flashLights = "flash-lights"
    case honkHorn = "honk-horn"

    var title: String {
        switch self {
        case .wake: return HubL10n.text("hub.HubController.314.326", fallback: "Wake Vehicle")
        case .climateStart: return HubL10n.text("hub.HubController.315.327", fallback: "Start Climate")
        case .climateStop: return HubL10n.text("hub.HubController.316.328", fallback: "Stop Climate")
        case .lock: return HubL10n.text("hub.HubController.317.329", fallback: "Lock Doors")
        case .unlock: return HubL10n.text("hub.HubController.318.330", fallback: "Unlock Doors")
        case .flashLights: return HubL10n.text("hub.HubController.319.331", fallback: "Flash Lights")
        case .honkHorn: return HubL10n.text("hub.HubController.320.332", fallback: "Honk Horn")
        }
    }

    var acceptedMessage: String {
        switch self {
        case .wake: return HubL10n.text("hub.HubController.326.333", fallback: "Check the vehicle to confirm it woke.")
        case .climateStart, .climateStop: return HubL10n.text("hub.HubController.327.334", fallback: "Check the vehicle to confirm the climate changed.")
        case .lock, .unlock: return HubL10n.text("hub.HubController.328.335", fallback: "Check the vehicle to confirm the doors changed.")
        case .flashLights: return HubL10n.text("hub.HubController.329.336", fallback: "The flash-lights command was accepted.")
        case .honkHorn: return HubL10n.text("hub.HubController.330.337", fallback: "The honk-horn command was accepted.")
        }
    }
}

enum HubVehicleTelemetryStatus: String, Equatable {
    case current
    case cached
    case unknown

    var displayName: String {
        switch self {
        case .current: return HubL10n.text("hub.HubController.342.338", fallback: "Current")
        case .cached: return HubL10n.text("hub.HubController.343.339", fallback: "Cached")
        case .unknown: return HubL10n.text("hub.HubController.344.340", fallback: "Unknown")
        }
    }
}

struct HubControlVehicle: Equatable {
    let id: UUID
    let displayName: String
    let status: String
    let activityState: String?
    let batteryLevel: Int?
    let locationName: String?
    let telemetryStatus: HubVehicleTelemetryStatus

    init(id: UUID, displayName: String, status: String,
         activityState: String? = nil, batteryLevel: Int? = nil,
         locationName: String? = nil,
         telemetryStatus: HubVehicleTelemetryStatus = .unknown) {
        self.id = id
        self.displayName = displayName
        self.status = status
        self.activityState = activityState
        self.batteryLevel = batteryLevel
        self.locationName = locationName
        self.telemetryStatus = telemetryStatus
    }
}

struct HubSnapshot {
    enum AccountState {
        case connected, notConfigured, historyOnly, unknown
    }

    enum DatabaseState {
        case healthy, awaitingSetup, unknown
    }

    var health: HubHealth
    var service: String
    var account: String
    var accountState: AccountState
    var provider: HubAccountProvider?
    var vehicleName: String
    var vehicle: String
    var hasConfiguredVehicle: Bool
    var controlVehicleID: UUID?
    var controlVehicles: [HubControlVehicle]
    var database: String
    var databaseState: DatabaseState
    var activity: [HubActivity]
    var version: String
    var dataDirectory: URL?
    var diagnosticLines: [String]
    var checkingStatus = false

    var accountDisplay: String {
        guard accountState == .connected, let provider else { return account }
        return HubL10n.format("hub.HubController.389.342", fallback: "%1$@ · %2$@", arguments: [String(describing: account), String(describing: provider.displayName)])
    }

    var statusUnavailable: Bool {
        checkingStatus || (accountState == .unknown && databaseState == .unknown && health != .needsInstall)
    }

    var shouldOfferTeslaMateImport: Bool {
        health != .running && accountState == .notConfigured
            && databaseState == .awaitingSetup
    }

    static let previewRunning = HubSnapshot(
        health: .running,
        service: "Active",
        account: "Connected",
        accountState: .connected,
        provider: .fleet,
        vehicleName: "Aurora",
        vehicle: "Model 3 Long Range · Parked · 78% · Home",
        hasConfiguredVehicle: true,
        controlVehicleID: UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5"),
        controlVehicles: [
            HubControlVehicle(
                id: UUID(uuidString: "B4C070D1-4C7C-4E01-BD5D-AC56F42A77B5")!,
                displayName: "Aurora",
                status: "Model 3 Long Range · Parked · 78% · Home",
                activityState: "parked",
                batteryLevel: 78,
                locationName: "Home",
                telemetryStatus: .cached
            ),
            HubControlVehicle(
                id: UUID(uuidString: "FB25AA4A-A719-4575-8BB1-02D4524F2571")!,
                displayName: "Comet",
                status: "Model Y Performance · Asleep · 54%",
                activityState: "asleep",
                batteryLevel: 54,
                telemetryStatus: .cached
            )
        ],
        database: "teslatlas.sqlite · 3.51 GB (imported)",
        databaseState: .healthy,
        activity: [
            HubActivity(message: "Imported TeslaMate history", age: "just now", color: .systemGreen)
        ],
        version: HubRelease.fallbackVersion,
        dataDirectory: URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/Application Support/Teslatlas Hub"),
        diagnosticLines: [
            "Preview mode: no process or launchctl mutation",
            "Service: Installed and running",
            "Account: Connected",
            "Vehicle: Model 3 · offline",
            "Database: Healthy · 18,426 records"
        ]
    )

    static let firstRun = HubSnapshot(
        health: .needsInstall,
        service: HubL10n.text("hub.HubController.441.368", fallback: "Not installed"),
        account: HubL10n.text("hub.HubController.393.343", fallback: "Not configured"),
        accountState: .notConfigured,
        provider: nil,
        vehicleName: HubL10n.text("hub.HubController.1163.481", fallback: "Vehicle"),
        vehicle: HubL10n.text("hub.HubController.445.371", fallback: "No configured vehicle"),
        hasConfiguredVehicle: false,
        controlVehicleID: nil,
        controlVehicles: [],
        database: HubL10n.text("hub.HubController.394.344", fallback: "Waiting for setup or import"),
        databaseState: .awaitingSetup,
        activity: [],
        version: HubRelease.fallbackVersion,
        dataDirectory: URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/Application Support/Teslatlas Hub/data"),
        diagnosticLines: [HubL10n.text("hub.HubController.452.374", fallback: "Hub has not been configured or installed.")]
    )
}

enum HubActionError: LocalizedError {
    case preview
    case missingResource(String)
    case commandFailed(String)
    case commandExited(Int32, String)
    case commandTimedOut
    case lifecycleRefused(HubLifecycleRefusal)
    /// The command failed (the first value) and its child could not be shown to be gone: it may
    /// still run or write. The owner of the work stays held until the quiescence says otherwise.
    case commandNotQuiescent(Error, HubChildQuiescence)

    /// The failure the process executor itself reports for a command: a timeout, an exit status, the output
    /// that did not close, or a system error from starting the process or feeding it.
    static func isExecutorFailure(_ error: Error) -> Bool {
        switch error {
        case HubActionError.commandTimedOut, HubActionError.commandExited: return true
        case let HubActionError.commandFailed(message): return message == outputDidNotClose.errorDescription
        case is HubActionError: return false
        default: return true
        }
    }

    static var outputDidNotClose: HubActionError {
        .commandFailed(HubL10n.text("hub.HubController.775.379", fallback: "Hub command output did not close."))
    }

    var errorDescription: String? {
        switch self {
        case let .commandNotQuiescent(inner, _):
            let sentence = HubL10n.text("hub.HubController.command_not_quiescent",
                                        fallback: "A Hub command could not be stopped and may still be running. Wait a moment before trying again.")
            // F8-1: a failure that a layer above the executor has already put into words (the Delete refusal,
            // the Start preflight message) keeps its words; the sentence follows. The executor's own failure
            // says nothing more than the sentence.
            return Self.isExecutorFailure(inner) ? sentence : inner.localizedDescription + " " + sentence
        case let .lifecycleRefused(refusal):
            return refusal.userMessage
        case .preview:
            return HubL10n.text("hub.HubController.467.375", fallback: "Preview mode is read-only. No process, installer, or launchctl action was run.")
        case let .missingResource(name):
            return HubL10n.format("hub.HubController.469.376", fallback: "Embedded resource is missing: %1$@", arguments: [String(describing: name)])
        case let .commandFailed(message):
            return message
        case let .commandExited(_, message):
            return message
        case .commandTimedOut:
            return HubL10n.text("hub.HubController.477.378", fallback: "Hub command timed out.")
        }
    }
}

/// Whether a child process can still write (F6-3). A child can write until its process has been
/// reaped. The executor feeds this from the termination handler; an owner of the lifecycle reads it
/// to decide when it may release admission protection. (Only the child the app started is covered:
/// a descendant that outlives it is not observable from here.)
final class HubChildQuiescence {
    private let lock = NSLock()
    private var childOutstanding = false
    private var waiters: [() -> Void] = []

    init() {}

    /// The process is about to be launched: it may write until it is reaped.
    func childMayRun() { lock.lock(); childOutstanding = true; lock.unlock() }

    /// The process was reaped, or it never started.
    func childReaped() {
        lock.lock()
        childOutstanding = false
        let due = waiters
        waiters = []
        lock.unlock()
        due.forEach { $0() }
    }

    var isQuiescent: Bool {
        lock.lock(); defer { lock.unlock() }
        return !childOutstanding
    }

    /// F8-4: one quiescence that is reached when every one of `parts` that is still pending has been reached.
    /// Nil when none is pending; the single pending part itself when only one is.
    static func allOf(_ parts: [HubChildQuiescence]) -> HubChildQuiescence? {
        let pending = parts.filter { !$0.isQuiescent }
        guard pending.count > 1 else { return pending.first }
        let combined = HubChildQuiescence()
        combined.childMayRun()
        let lock = NSLock()
        var remaining = pending.count
        for part in pending {
            part.whenQuiescent {
                lock.lock()
                remaining -= 1
                let last = remaining == 0
                lock.unlock()
                if last { combined.childReaped() }
            }
        }
        return combined
    }

    /// Calls back once, immediately when already quiescent.
    func whenQuiescent(_ block: @escaping () -> Void) {
        lock.lock()
        if !childOutstanding { lock.unlock(); block(); return }
        waiters.append(block)
        lock.unlock()
    }
}

/// A result that says its child may still be running (`pendingQuiescence`), so that the lifecycle
/// owner keeps its place until the child has gone.
protocol HubQuiescenceReporting {
    var pendingQuiescence: HubChildQuiescence? { get }
}

extension HubActionError {
    /// The quiescence of a child that cannot yet be shown gone, carried by `error` directly or by the
    /// restart report that wraps it. Nil for every other error and for a child that is already reaped.
    static func pendingQuiescence(of error: Error) -> HubChildQuiescence? {
        if case let HubActionError.commandNotQuiescent(_, quiescence) = error {
            return quiescence.isQuiescent ? nil : quiescence
        }
        if let report = error as? HubServiceRestartReport {
            // F8-4: the original failure and the recovery step can each have a child that may still run; the
            // owner is held until both are gone.
            return HubChildQuiescence.allOf([pendingQuiescence(of: report.original),
                                            report.recoveryFailure.flatMap { pendingQuiescence(of: $0) }].compactMap { $0 })
        }
        return nil
    }

    /// F7-2: a layer that turns a failure into another error (a preflight message, a stop failure, an
    /// SSH diagnostic) must not drop the fact that the child behind it may still run. When `source`
    /// reports a child that is not yet quiescent, `replacement` is returned as
    /// `commandNotQuiescent(replacement, q)` with the same quiescence; otherwise it is returned as is.
    static func keepingQuiescence(of source: Error, in replacement: Error) -> Error {
        if case HubActionError.commandNotQuiescent = replacement { return replacement }
        guard let quiescence = pendingQuiescence(of: source) else { return replacement }
        return HubActionError.commandNotQuiescent(replacement, quiescence)
    }

    /// The error a caller shows or classifies: a not-quiescent wrapper is transparent to it.
    static func settingQuiescenceAside(_ error: Error) -> Error {
        if case let HubActionError.commandNotQuiescent(inner, _) = error { return settingQuiescenceAside(inner) }
        return error
    }
}

extension Result: HubQuiescenceReporting where Failure == Error {
    var pendingQuiescence: HubChildQuiescence? {
        guard case let .failure(error) = self else { return nil }
        return HubActionError.pendingQuiescence(of: error)
    }
}

protocol HubCommandRunning {
    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void)
    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void)
    func run(arguments: [String],
             onOutputLine: @escaping (String) -> Void,
             completion: @escaping (Result<String, Error>) -> Void)
}

extension HubCommandRunning {
    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, completion: completion)
    }

    func run(arguments: [String],
             onOutputLine: @escaping (String) -> Void,
             completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, completion: completion)
    }
}

final class BoundedProcessOutput {
    private let maximumBytes: Int
    private var retained = Data()
    private let lock = NSLock()

    init(maximumBytes: Int) {
        self.maximumBytes = maximumBytes
    }

    func append(_ chunk: Data) {
        guard maximumBytes > 0, !chunk.isEmpty else { return }
        lock.lock()
        defer { lock.unlock() }
        if chunk.count >= maximumBytes {
            retained = Data(chunk.suffix(maximumBytes))
            return
        }
        let overflow = retained.count + chunk.count - maximumBytes
        if overflow > 0 {
            retained.removeFirst(overflow)
        }
        retained.append(chunk)
    }

    func snapshot() -> Data {
        lock.lock()
        defer { lock.unlock() }
        return retained
    }
}

private final class ProcessOutputLineCallbackGate {
    private let lock = NSLock()
    private var callback: ((String) -> Void)?

    init(callback: ((String) -> Void)?) {
        self.callback = callback
    }

    var isActive: Bool {
        lock.lock()
        defer { lock.unlock() }
        return callback != nil
    }

    func emit(_ line: String) {
        lock.lock()
        let callback = callback
        lock.unlock()
        callback?(line)
    }

    func cancel() {
        lock.lock()
        callback = nil
        lock.unlock()
    }
}

private final class ProcessOutputLineDecoder {
    private static let maximumLineBytes = 64 * 1024

    private var bytes: [UInt8] = []
    private var discardingOversizedLine = false
    private let callbackGate: ProcessOutputLineCallbackGate

    init(callbackGate: ProcessOutputLineCallbackGate) {
        self.callbackGate = callbackGate
        bytes.reserveCapacity(1_024)
    }

    func append(_ chunk: Data) {
        guard callbackGate.isActive else { return }
        for byte in chunk {
            if discardingOversizedLine {
                if byte == 0x0A { discardingOversizedLine = false }
                continue
            }
            if byte == 0x0A {
                emitBufferedLine()
            } else if bytes.count < Self.maximumLineBytes {
                bytes.append(byte)
            } else {
                bytes.removeAll(keepingCapacity: true)
                discardingOversizedLine = true
            }
        }
    }

    func finish() {
        guard !discardingOversizedLine, !bytes.isEmpty else { return }
        emitBufferedLine()
    }

    private func emitBufferedLine() {
        if bytes.last == 0x0D { bytes.removeLast() }
        callbackGate.emit(String(decoding: bytes, as: UTF8.self))
        bytes.removeAll(keepingCapacity: true)
    }
}

final class ProcessOutputPipeReader {
    enum Outcome: Equatable {
        case pending
        case endOfFile
        case cancelled
        case failed(Int32)
    }

    private static let pollIntervalMilliseconds: Int32 = 20
    private static let chunkSize = 16 * 1024

    private let fileHandle: FileHandle
    private let onChunk: (Data) -> Void
    private let lock = NSLock()
    private var cancellationRequested = false
    private var storedOutcome = Outcome.pending

    init(fileHandle: FileHandle, onChunk: @escaping (Data) -> Void) {
        self.fileHandle = fileHandle
        self.onChunk = onChunk
    }

    var outcome: Outcome {
        lock.lock()
        defer { lock.unlock() }
        return storedOutcome
    }

    func cancel() {
        lock.lock()
        cancellationRequested = true
        lock.unlock()
    }

    func run() {
        let fileDescriptor = fileHandle.fileDescriptor
        var pollDescriptor = pollfd(
            fd: fileDescriptor,
            events: Int16(POLLIN | POLLHUP | POLLERR),
            revents: 0
        )
        var buffer = [UInt8](repeating: 0, count: Self.chunkSize)
        defer { try? fileHandle.close() }

        while true {
            if isCancellationRequested {
                finish(with: .cancelled)
                return
            }
            pollDescriptor.revents = 0
            let pollResult = Darwin.poll(
                &pollDescriptor,
                1,
                Self.pollIntervalMilliseconds
            )
            if pollResult == 0 { continue }
            if pollResult < 0 {
                let pollError = errno
                if pollError == EINTR { continue }
                finish(with: .failed(pollError))
                return
            }
            if pollDescriptor.revents & Int16(POLLNVAL) != 0 {
                finish(with: .failed(EBADF))
                return
            }
            guard pollDescriptor.revents & Int16(POLLIN | POLLHUP | POLLERR) != 0 else {
                continue
            }
            let byteCount = buffer.withUnsafeMutableBytes { storage in
                Darwin.read(fileDescriptor, storage.baseAddress, storage.count)
            }
            if byteCount > 0 {
                onChunk(Data(buffer.prefix(byteCount)))
                continue
            }
            if byteCount == 0 {
                finish(with: .endOfFile)
                return
            }
            let readError = errno
            if readError == EINTR || readError == EAGAIN || readError == EWOULDBLOCK {
                continue
            }
            finish(with: .failed(readError))
            return
        }
    }

    private var isCancellationRequested: Bool {
        lock.lock()
        defer { lock.unlock() }
        return cancellationRequested
    }

    private func finish(with outcome: Outcome) {
        lock.lock()
        storedOutcome = outcome
        lock.unlock()
    }
}

/// Only foreground catalogue status readers belong to this lifecycle. Launchd
/// services and mutation commands retain their existing ownership and quit guards.
final class HubStatusProcessLifecycle {
    static let shared = HubStatusProcessLifecycle()
    private let lock = NSLock()
    private var stopping = false
    private var paused = false
    private var processes: [ObjectIdentifier: Process] = [:]

    static func isStatusRead(_ arguments: [String]) -> Bool {
        arguments.count == 3 && arguments[0] == "--config" && arguments[2] == "status"
    }

    func launch(_ process: Process) throws {
        lock.lock(); defer { lock.unlock() }
        guard !stopping else { throw HubActionError.commandFailed("Hub status read cancelled during application shutdown.") }
        guard !paused else { throw HubActionError.commandFailed("Hub status reads are paused.") }
        let identity = ObjectIdentifier(process)
        processes[identity] = process
        do { try process.run() }
        catch { processes.removeValue(forKey: identity); throw error }
    }

    func finished(_ process: Process) {
        lock.lock(); processes.removeValue(forKey: ObjectIdentifier(process)); lock.unlock()
    }

    func shutdown() {
        lock.lock()
        stopping = true
        let owned = Array(processes.values)
        processes.removeAll()
        lock.unlock()
        for process in owned where process.isRunning { process.terminate() }
    }

    /// Refuses new status reads, terminates the running ones and returns true only once
    /// each has exited, within `waitingUpTo` seconds. Distinct from `shutdown()`: a pause
    /// can be undone with `resume()`.
    func pause(waitingUpTo seconds: TimeInterval) -> Bool {
        lock.lock()
        paused = true
        let owned = Array(processes.values)
        lock.unlock()
        for process in owned where process.isRunning { process.terminate() }
        let deadline = DispatchTime.now() + max(0, seconds)
        while true {
            lock.lock()
            let remaining = processes.values.filter { $0.isRunning }.count
            lock.unlock()
            if remaining == 0 { return true }
            guard DispatchTime.now() < deadline else { return false }
            Thread.sleep(forTimeInterval: 0.01)
        }
    }

    func resume() {
        lock.lock(); paused = false; lock.unlock()
    }

    var isPaused: Bool {
        lock.lock(); defer { lock.unlock() }; return paused
    }

    var activeCount: Int {
        lock.lock(); defer { lock.unlock() }; return processes.count
    }
}

/// The clock and signal primitives of `HubProcessExecutor`. Production uses the system
/// uptime clock, real semaphore waits and real signals; a test injects a manual clock so
/// the run limit is enforced on injected time and the TERM, KILL and reap steps are
/// observed as events instead of being timed on a loaded host.
struct HubProcessPrimitives {
    /// System-uptime nanoseconds.
    var now: () -> UInt64
    /// Waits for the semaphore until the clock reaches the absolute deadline.
    var wait: (DispatchSemaphore, UInt64) -> DispatchTimeoutResult
    var terminate: (Process) -> Void
    var kill: (pid_t) -> Void

    static let live = HubProcessPrimitives(
        now: { DispatchTime.now().uptimeNanoseconds },
        wait: { $0.wait(timeout: DispatchTime(uptimeNanoseconds: $1)) },
        terminate: { $0.terminate() },
        kill: { _ = Darwin.kill($0, SIGKILL) }
    )
}

enum HubProcessExecutor {
    static let defaultMaximumOutputBytes = 256 * 1024
    static let maximumStandardInputBytes = 8 * 1024 * 1024
    static let defaultTimeout: TimeInterval = 5 * 60
    static let defaultTerminationGrace: TimeInterval = 2
    static let defaultOutputDrainTimeout: TimeInterval = 2

    static func run(executable: URL,
                    arguments: [String],
                    stdin: String? = nil,
                    environment: [String: String]? = nil,
                    maximumOutputBytes: Int = defaultMaximumOutputBytes,
                    timeout: TimeInterval = defaultTimeout,
                    waitDeadline: DispatchTime? = nil,
                    finalDeadline: DispatchTime? = nil,
                    terminationGrace: TimeInterval = defaultTerminationGrace,
                    outputDrainTimeout: TimeInterval = defaultOutputDrainTimeout,
                    statusLifecycle: HubStatusProcessLifecycle = .shared,
                    processFactory: @escaping () -> Process = Process.init,
                    primitives: HubProcessPrimitives = .live,
                    onOutputLine: ((String) -> Void)? = nil,
                    completion: @escaping (Result<String, Error>) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            guard (stdin?.utf8.prefix(maximumStandardInputBytes + 1).count ?? 0)
                    <= maximumStandardInputBytes else {
                completion(.failure(HubActionError.commandFailed("Hub command input exceeds its size limit.")))
                return
            }
            let standardInput = stdin.map { Data($0.utf8) }
            let process = processFactory()
            let output = Pipe()
            let retained = BoundedProcessOutput(maximumBytes: maximumOutputBytes)
            let callbackGate = ProcessOutputLineCallbackGate(callback: onOutputLine)
            let lineDecoder = ProcessOutputLineDecoder(callbackGate: callbackGate)
            let pipeReader = ProcessOutputPipeReader(
                fileHandle: output.fileHandleForReading
            ) { chunk in
                retained.append(chunk)
                lineDecoder.append(chunk)
            }
            // Signalled once when the output reader has finished; `readerStarted` says whether
            // there is a reader to wait for at all (a spawn refused early has none).
            let readerDone = DispatchSemaphore(value: 0)
            var readerStarted = false
            let terminated = DispatchSemaphore(value: 0)
            // F6-3: whether the child can still write. Fed by the termination handler (reaped); a failure
            // is only final once the child is gone.
            let quiescence = HubChildQuiescence()
            var launchAttempted = false
            var launched = false
            process.executableURL = executable
            process.arguments = arguments
            if let environment {
                process.environment = ProcessInfo.processInfo.environment.merging(environment) { _, new in new }
            }
            process.standardOutput = output
            process.standardError = output
            if stdin != nil { process.standardInput = Pipe() }
            process.terminationHandler = { [weak process] _ in
                if let process { statusLifecycle.finished(process) }
                quiescence.childReaped()
                terminated.signal()
            }
            defer {
                if let input = process.standardInput as? Pipe {
                    try? input.fileHandleForWriting.close()
                }
            }
            // Cleanup waits end at the wanted interval from now, cut to what remains of the
            // caller's whole budget, so a late start or a slow kill never renews it.
            func cleanupDeadline(_ interval: TimeInterval) -> UInt64 {
                let now = primitives.now()
                let wanted = UInt64(max(0.001, interval) * 1_000_000_000)
                guard let finalDeadline = finalDeadline?.uptimeNanoseconds else { return now &+ wanted }
                guard finalDeadline > now else { return now &+ 1_000_000 }
                return now &+ max(1_000_000, min(wanted, finalDeadline - now))
            }
            do {
                let deadline = waitDeadline?.uptimeNanoseconds
                    ?? primitives.now() &+ UInt64(max(0.001, timeout) * 1_000_000_000)
                // Queue delay or preparation may already have spent the caller's budget:
                // a child is never started after its deadline.
                if waitDeadline != nil, primitives.now() >= deadline {
                    throw HubActionError.commandTimedOut
                }
                launchAttempted = true
                quiescence.childMayRun()
                if HubStatusProcessLifecycle.isStatusRead(arguments) { try statusLifecycle.launch(process) }
                else { try process.run() }
                launched = true
                readerStarted = true
                DispatchQueue.global(qos: .userInitiated).async {
                    pipeReader.run()
                    lineDecoder.finish()
                    readerDone.signal()
                }
                if let standardInput, let input = process.standardInput as? Pipe {
                    try writeStandardInput(standardInput, to: input.fileHandleForWriting,
                                           deadline: deadline, now: primitives.now)
                    try input.fileHandleForWriting.close()
                }
                if primitives.wait(terminated, deadline) == .timedOut {
                    throw HubActionError.commandTimedOut
                }
                guard primitives.wait(readerDone, cleanupDeadline(outputDrainTimeout)) == .success else {
                    callbackGate.cancel()
                    pipeReader.cancel()
                    _ = primitives.wait(readerDone, cleanupDeadline(outputDrainTimeout))
                    let unclosed = HubActionError.outputDidNotClose
                    completion(.failure(quiescence.isQuiescent ? unclosed : HubActionError.commandNotQuiescent(unclosed, quiescence)))
                    return
                }
                callbackGate.cancel()
                guard pipeReader.outcome == .endOfFile else {
                    completion(.failure(HubActionError.commandFailed(
                        HubL10n.text("hub.HubController.781.380", fallback: "Hub command output could not be read.")
                    )))
                    return
                }
                let text = String(decoding: retained.snapshot(), as: UTF8.self)
                if process.terminationStatus == 0 {
                    completion(.success(text))
                } else {
                    completion(.failure(HubActionError.commandExited(
                        process.terminationStatus,
                        text.isEmpty ? HubL10n.text("hub.HubController.791.381", fallback: "Hub command failed.") : text
                    )))
                }
            } catch {
                callbackGate.cancel()
                if let input = process.standardInput as? Pipe {
                    try? input.fileHandleForWriting.close()
                }
                if process.isRunning {
                    let pid = process.processIdentifier
                    primitives.terminate(process)
                    if primitives.wait(terminated, cleanupDeadline(terminationGrace)) == .timedOut {
                        if process.isRunning { primitives.kill(pid) }
                        _ = primitives.wait(terminated, cleanupDeadline(terminationGrace))
                    }
                }
                if readerStarted,
                   primitives.wait(readerDone, cleanupDeadline(outputDrainTimeout)) == .timedOut {
                    pipeReader.cancel()
                    _ = primitives.wait(readerDone, cleanupDeadline(outputDrainTimeout))
                }
                pipeReader.cancel()
                try? output.fileHandleForReading.close()
                // A spawn that never started has no child. Otherwise the failure is only final once the
                // child has been reaped; until then it is reported as "not quiescent" and the owner of
                // the work keeps its place.
                if launchAttempted && !launched { quiescence.childReaped() }
                completion(.failure(quiescence.isQuiescent ? error : HubActionError.commandNotQuiescent(error, quiescence)))
            }
        }
    }

    private static func writeStandardInput(_ data: Data, to handle: FileHandle,
                                           deadline: UInt64, now clock: () -> UInt64) throws {
        let descriptor = handle.fileDescriptor
        let flags = fcntl(descriptor, F_GETFL)
        guard flags >= 0, fcntl(descriptor, F_SETFL, flags | O_NONBLOCK) == 0,
              fcntl(descriptor, F_SETNOSIGPIPE, 1) == 0 else {
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
        }
        try data.withUnsafeBytes { storage in
            var offset = 0
            while offset < storage.count {
                guard clock() < deadline else { throw HubActionError.commandTimedOut }
                let written = Darwin.write(descriptor, storage.baseAddress!.advanced(by: offset),
                                           min(16 * 1024, storage.count - offset))
                if written > 0 { offset += written; continue }
                let writeError = errno
                if written < 0 && writeError == EINTR { continue }
                guard written < 0 && (writeError == EAGAIN || writeError == EWOULDBLOCK) else {
                    throw NSError(domain: NSPOSIXErrorDomain, code: Int(written == 0 ? EIO : writeError))
                }
                var readiness = pollfd(fd: descriptor, events: Int16(POLLOUT), revents: 0)
                let now = clock()
                guard now < deadline else { throw HubActionError.commandTimedOut }
                let remaining = deadline - now
                let milliseconds = Int32(min(20, max(1, remaining / 1_000_000)))
                if Darwin.poll(&readiness, 1, milliseconds) < 0 && errno != EINTR {
                    throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
                }
            }
        }
    }
}

final class EmbeddedHubCommandRunner: HubCommandRunning {
    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        runProcess(arguments: arguments, stdin: nil, onOutputLine: nil, completion: completion)
    }

    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        runProcess(arguments: arguments, stdin: stdin, onOutputLine: nil, completion: completion)
    }

    func run(arguments: [String],
             onOutputLine: @escaping (String) -> Void,
             completion: @escaping (Result<String, Error>) -> Void) {
        runProcess(arguments: arguments, stdin: nil, onOutputLine: onOutputLine, completion: completion)
    }

    private func runProcess(arguments: [String],
                            stdin: String?,
                            onOutputLine: ((String) -> Void)?,
                            completion: @escaping (Result<String, Error>) -> Void) {
        guard let executable = Bundle.main.url(forResource: "teslatlas-hub", withExtension: nil) else {
            completion(.failure(HubActionError.missingResource("teslatlas-hub")))
            return
        }
        HubProcessExecutor.run(executable: executable,
                               arguments: arguments,
                               stdin: stdin,
                               maximumOutputBytes: Self.maximumOutputBytes(for: arguments),
                               timeout: Self.timeout(for: arguments),
                               onOutputLine: onOutputLine,
                               completion: completion)
    }

    static func timeout(for arguments: [String]) -> TimeInterval {
        if arguments.contains("migrate") { return 24 * 60 * 60 }
        if arguments.contains("doctor") { return 15 * 60 }
        if arguments.contains("teslamate-check") { return 5 * 60 }
        if arguments.contains("setup") { return 5 * 60 }
        if arguments.contains("control") { return 45 }
        if arguments.contains("status") || arguments.contains("preflight") { return 30 }
        return HubProcessExecutor.defaultTimeout
    }

    private static func maximumOutputBytes(for arguments: [String]) -> Int {
        if arguments.contains("doctor") { return 1024 * 1024 }
        return HubProcessExecutor.defaultMaximumOutputBytes
    }
}

// MARK: - launchctl runner

/// The one child of the lifecycle controller: `/bin/launchctl` with unchanged argv.
protocol HubLaunchctlRunning {
    func run(arguments: [String], timeout: TimeInterval,
             completion: @escaping (Result<String, Error>) -> Void)
}

/// Everything the spawn seam needs. The monotonic deadline is absolute, so queue delay
/// and preparation are charged to the caller's budget and never renew it.
struct HubLaunchctlInvocation: Equatable {
    let executable: URL
    let arguments: [String]
    /// Ordinary wait, already net of the cleanup reserve.
    let waitLimit: TimeInterval
    let terminationGrace: TimeInterval
    let outputDrainTimeout: TimeInterval
    /// System-uptime nanoseconds at which the ordinary wait ends.
    let waitDeadlineUptimeNanoseconds: UInt64
    /// System-uptime nanoseconds at which the caller's whole budget (wait plus cleanup) ends.
    let finalDeadlineUptimeNanoseconds: UInt64
}

typealias HubLaunchctlSpawn = (_ invocation: HubLaunchctlInvocation,
                               _ completion: @escaping (Result<String, Error>) -> Void) -> Void

final class HubLaunchctlRunner: HubLaunchctlRunning {
    static let executable = URL(fileURLWithPath: "/bin/launchctl")
    static let defaultTerminationGrace: TimeInterval = 0.25
    static let defaultOutputDrainTimeout: TimeInterval = 0.25

    private let spawn: HubLaunchctlSpawn
    private let terminationGrace: TimeInterval
    private let outputDrainTimeout: TimeInterval
    private let environment: [String: String]
    private let uptime: () -> UInt64

    init(spawn: @escaping HubLaunchctlSpawn = HubLaunchctlRunner.processSpawn(),
         terminationGrace: TimeInterval = HubLaunchctlRunner.defaultTerminationGrace,
         outputDrainTimeout: TimeInterval = HubLaunchctlRunner.defaultOutputDrainTimeout,
         environment: [String: String] = ProcessInfo.processInfo.environment,
         uptime: @escaping () -> UInt64 = { DispatchTime.now().uptimeNanoseconds }) {
        self.spawn = spawn
        self.terminationGrace = terminationGrace
        self.outputDrainTimeout = outputDrainTimeout
        self.environment = environment
        self.uptime = uptime
    }

    /// Termination takes SIGTERM, a grace, SIGKILL, a grace, then the output drain
    /// twice: this much of the caller's deadline is spent after the ordinary wait.
    var cleanupReserve: TimeInterval { 2 * terminationGrace + 2 * outputDrainTimeout }

    static let refusal = HubActionError.commandFailed(
        "Hub service commands are not run by a test host."
    )

    func run(arguments: [String], timeout: TimeInterval,
             completion: @escaping (Result<String, Error>) -> Void) {
        let entry = uptime()
        guard !HubTestHost.isXCTest(environment: environment) else {
            completion(.failure(Self.refusal))
            return
        }
        guard timeout > cleanupReserve else {
            completion(.failure(HubActionError.commandTimedOut))
            return
        }
        let wait = timeout - cleanupReserve
        let invocation = HubLaunchctlInvocation(
            executable: Self.executable,
            arguments: arguments,
            waitLimit: wait,
            terminationGrace: terminationGrace,
            outputDrainTimeout: outputDrainTimeout,
            waitDeadlineUptimeNanoseconds: entry &+ UInt64(wait * 1_000_000_000),
            finalDeadlineUptimeNanoseconds: entry &+ UInt64(timeout * 1_000_000_000)
        )
        let once = HubCompletionOnce(completion)
        spawn(invocation) { once.complete($0) }
    }

    /// Production spawn: `HubProcessExecutor`, with the absolute wait deadline.
    /// `executable` and `processFactory` exist for harmless process tests only.
    static func processSpawn(executable: URL? = nil,
                             processFactory: @escaping () -> Process = Process.init,
                             primitives: HubProcessPrimitives = .live) -> HubLaunchctlSpawn {
        { invocation, completion in
            HubProcessExecutor.run(
                executable: executable ?? invocation.executable,
                arguments: invocation.arguments,
                timeout: invocation.waitLimit,
                waitDeadline: DispatchTime(uptimeNanoseconds: invocation.waitDeadlineUptimeNanoseconds),
                finalDeadline: DispatchTime(uptimeNanoseconds: invocation.finalDeadlineUptimeNanoseconds),
                terminationGrace: invocation.terminationGrace,
                outputDrainTimeout: invocation.outputDrainTimeout,
                processFactory: processFactory,
                primitives: primitives,
                completion: completion
            )
        }
    }
}

/// Delivers a completion at most once.
final class HubCompletionOnce<Value> {
    private let lock = NSLock()
    private var handler: ((Value) -> Void)?

    init(_ handler: @escaping (Value) -> Void) { self.handler = handler }

    func complete(_ value: Value) {
        lock.lock()
        let pending = handler
        handler = nil
        lock.unlock()
        pending?(value)
    }
}

// MARK: - Lifecycle ownership (R-E1F6-1)

/// The independent owners of the Hub's lifecycle (the inventory's S, T, R, U, D, L, A, G,
/// O, I, V, H and C). At most one is held at a time; every pair, including two of the
/// same family, is forbidden to overlap.
enum HubLifecycleFamily: String, CaseIterable, Equatable {
    case start, stop, restart, remove, delete, reconcile, account, diagnostics, signOut
    case migration, verification, handover, vehicleControl

    var letter: String {
        switch self {
        case .start: return "S"
        case .stop: return "T"
        case .restart: return "R"
        case .remove: return "U"
        case .delete: return "D"
        case .reconcile: return "L"
        case .account: return "A"
        case .diagnostics: return "G"
        case .signOut: return "O"
        case .migration: return "I"
        case .verification: return "V"
        case .handover: return "H"
        case .vehicleControl: return "C"
        }
    }

    /// Launch reconciliation is the only background owner: Quit waits for it.
    var isBackground: Bool { self == .reconcile }

    /// Admitting one of these records an intentional change of the service that a
    /// deferred launch-time reconciliation must not undo (R-E1F6-12).
    var recordsIntentionalChange: Bool { self == .stop || self == .remove || self == .delete }

    var activityName: String {
        switch self {
        case .start: return "starting the Hub"
        case .stop: return "stopping the Hub"
        case .restart: return "restarting the Hub"
        case .remove: return "removing the background service"
        case .delete: return "deleting the Hub and its data"
        case .reconcile: return "checking the background service"
        case .account: return "setting up the Tesla account"
        case .diagnostics: return "running diagnostics"
        case .signOut: return "disconnecting Tesla"
        case .migration: return "preparing or importing TeslaMate data"
        case .verification: return "checking the Hub"
        case .handover: return "starting the Hub after migration"
        case .vehicleControl: return "sending a vehicle command"
        }
    }
}

enum HubLifecycleState: Equatable {
    case open, deleting, deleted, closing, closed
}

/// Why an operation was not admitted. Nothing ran.
enum HubLifecycleRefusal: Error, Equatable {
    case busy(HubLifecycleFamily)
    case deleting
    case deleted
    case closing
    case closed
}

extension HubLifecycleRefusal {
    var userMessage: String {
        switch self {
        case let .busy(family):
            return HubL10n.format("hub.lifecycle.busy",
                                  fallback: "The Hub is busy with another operation (%1$@). Nothing was changed. Try again when it has finished.",
                                  arguments: [family.activityName])
        case .deleting, .deleted:
            return HubL10n.text("hub.dataRemoval.already_started",
                                fallback: "Delete Hub and Data has already started. Nothing was changed.")
        case .closing, .closed:
            return HubL10n.text("hub.lifecycle.closing",
                                fallback: "Teslatlas Hub is quitting. Nothing was changed.")
        }
    }
}

/// The identity of one admitted operation. It can only be made by the authority.
struct HubLifecycleToken: Equatable, Hashable {
    let id: UUID
    /// Monotonically advancing across the authority's acquisitions.
    let generation: UInt64
    let family: HubLifecycleFamily

    fileprivate init(id: UUID, generation: UInt64, family: HubLifecycleFamily) {
        self.id = id
        self.generation = generation
        self.family = family
    }
}

/// An immutable view of the lifecycle that the interface and the termination decision read.
struct HubLifecycleCapabilities: Equatable {
    /// Advances with every change; a consumer ignores a view older than one it has seen.
    let publication: UInt64
    let state: HubLifecycleState
    let owner: HubLifecycleFamily?
    let phase: String?
    /// A child of the owner failed but cannot be shown gone: the owner stays held until it is reaped.
    var childHeld = false

    static let idle = HubLifecycleCapabilities(publication: 0, state: .open, owner: nil, phase: nil)

    /// Whether any service, account, migration or control operation could be admitted now.
    var isIdle: Bool { state == .open && owner == nil }
    var servicesMutable: Bool { isIdle }
    var accountsMutable: Bool { isIdle }
    var diagnosticsHeld: Bool { owner == .diagnostics }
    /// A foreground owner, or a Delete before its data step, refuses Quit.
    var foregroundOwner: HubLifecycleFamily? { owner.flatMap { $0.isBackground ? nil : $0 } }
    /// F8-2: so does any owner holding a child that cannot be shown gone, background owners included.
    var preventsQuit: Bool { foregroundOwner != nil || state == .deleting || childHeld }
    var observersAdmitted: Bool { state == .open || state == .deleting }
    var preferenceWritesAdmitted: Bool { state == .open }
}

/// The one synchronisation domain for admission, phase changes, child registration,
/// terminal transition and release. Held by the application's one `HubController`.
final class HubLifecycleAuthority {
    enum Terminal: Equatable { case open, deleted }

    enum Event: Equatable {
        case acquired(HubLifecycleFamily, generation: UInt64)
        case refused(HubLifecycleFamily, HubLifecycleRefusal)
        case released(HubLifecycleFamily, generation: UInt64, Terminal)
        case staleIgnored(HubLifecycleFamily, generation: UInt64)
    }

    private final class Entry {
        let token: HubLifecycleToken
        var phase: String
        var children = 0
        /// Children that failed but cannot be shown gone (F6-3); counted in `children` too.
        var held = 0
        var childWaiters: [(atMost: Int, block: () -> Void)] = []
        var settling = false
        var terminal = Terminal.open
        var deliver: (() -> Void)?
        init(token: HubLifecycleToken, phase: String) { self.token = token; self.phase = phase }
    }

    private let lock = NSLock()
    private var state = HubLifecycleState.open
    private var entry: Entry?
    private var lastGeneration: UInt64 = 0
    private var publicationCounter: UInt64 = 0
    private var intent: UInt64 = 0
    private var deletionRan = false
    private var observers: [UUID: (HubLifecycleCapabilities) -> Void] = [:]
    private var idleWaiters: [() -> Void] = []
    private var journalStorage: [Event] = []
    /// Called, outside the lock, after every release (the controller drains deferred work).
    var onIdle: (() -> Void)?

    // MARK: Reading

    var capabilities: HubLifecycleCapabilities {
        lock.lock(); defer { lock.unlock() }
        return capturedLocked()
    }

    var journal: [Event] {
        lock.lock(); defer { lock.unlock() }
        return journalStorage
    }

    /// Advances with every admitted Stop, Remove, Delete and with termination.
    var intentGeneration: UInt64 {
        lock.lock(); defer { lock.unlock() }
        return intent
    }

    /// True once a Delete has run its data step; stays true through closing.
    var deletionCompleted: Bool {
        lock.lock(); defer { lock.unlock() }
        return deletionRan
    }

    /// The owner's token, for diagnostics and tests. Settling it still goes through the
    /// operation that holds the work; nothing is released by merely reading it.
    var currentToken: HubLifecycleToken? {
        lock.lock(); defer { lock.unlock() }
        return entry?.token
    }

    func isCurrent(_ token: HubLifecycleToken) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return entry?.token == token && entry?.settling == false
    }

    private func capturedLocked() -> HubLifecycleCapabilities {
        HubLifecycleCapabilities(publication: publicationCounter, state: state,
                                 owner: entry?.token.family, phase: entry?.phase,
                                 childHeld: (entry?.held ?? 0) > 0)
    }

    private func record(_ event: Event) {
        journalStorage.append(event)
        if journalStorage.count > 512 { journalStorage.removeFirst(journalStorage.count - 512) }
    }

    // MARK: Admission

    /// Atomic: the answer and the state change happen under one lock. `barrier` runs under
    /// the lock for a Delete, so a preference write that asks next sees the closed state.
    func acquire(_ family: HubLifecycleFamily,
                 phase: String = "admitted",
                 installingBarrier barrier: (() -> Void)? = nil) -> Result<HubLifecycleToken, HubLifecycleRefusal> {
        lock.lock()
        let refusal: HubLifecycleRefusal?
        switch state {
        case .closed: refusal = .closed
        case .closing: refusal = .closing
        case .deleted: refusal = .deleted
        case .deleting: refusal = .deleting
        case .open: refusal = entry.map { .busy($0.token.family) }
        }
        if let refusal {
            record(.refused(family, refusal))
            lock.unlock()
            return .failure(refusal)
        }
        lastGeneration += 1
        let token = HubLifecycleToken(id: UUID(), generation: lastGeneration, family: family)
        entry = Entry(token: token, phase: phase)
        if family == .delete {
            state = .deleting
            barrier?()
        }
        if family.recordsIntentionalChange { intent += 1 }
        record(.acquired(family, generation: token.generation))
        publicationCounter += 1
        let snapshot = capturedLocked()
        lock.unlock()
        publish(snapshot)
        return .success(token)
    }

    /// Moves the current owner to a named phase. False for a stale or settled token.
    @discardableResult
    func advance(_ token: HubLifecycleToken, to phase: String) -> Bool {
        lock.lock()
        guard let entry, entry.token == token, !entry.settling else {
            record(.staleIgnored(token.family, generation: token.generation))
            lock.unlock()
            return false
        }
        entry.phase = phase
        publicationCounter += 1
        let snapshot = capturedLocked()
        lock.unlock()
        publish(snapshot)
        return true
    }

    // MARK: Children

    /// Registers an owned child before it is launched. False (nothing may be launched)
    /// for a stale or settled token.
    func beginChild(_ token: HubLifecycleToken) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard let entry, entry.token == token, !entry.settling else {
            record(.staleIgnored(token.family, generation: token.generation))
            return false
        }
        entry.children += 1
        return true
    }

    /// The child has finished and can no longer write or schedule another phase.
    func endChild(_ token: HubLifecycleToken) {
        endChild(token, wasHeld: false)
    }

    /// F7-4: a child failed but cannot be shown gone. It stays registered (see `endHeldChild`); the
    /// interface learns of the state so that Quit can say why it is refused.
    func holdChild(_ token: HubLifecycleToken) {
        lock.lock()
        guard let entry, entry.token == token, entry.children > 0 else { lock.unlock(); return }
        entry.held += 1
        publicationCounter += 1
        let snapshot = capturedLocked()
        lock.unlock()
        publish(snapshot)
    }

    /// The held child has been reaped.
    func endHeldChild(_ token: HubLifecycleToken) {
        endChild(token, wasHeld: true)
    }

    private func endChild(_ token: HubLifecycleToken, wasHeld: Bool) {
        lock.lock()
        guard let entry, entry.token == token, entry.children > 0 else { lock.unlock(); return }
        entry.children -= 1
        if wasHeld, entry.held > 0 { entry.held -= 1 }
        let due = entry.childWaiters.filter { $0.atMost >= entry.children }
        entry.childWaiters.removeAll { $0.atMost >= entry.children }
        guard entry.settling, entry.children == 0 else {
            var snapshot: HubLifecycleCapabilities?
            if wasHeld { publicationCounter += 1; snapshot = capturedLocked() }
            lock.unlock()
            if let snapshot { publish(snapshot) }
            due.forEach { $0.block() }
            return
        }
        let release = releaseLocked()
        lock.unlock()
        due.forEach { $0.block() }
        finish(release)
    }

    /// F7-3: calls back once the owner has no registered child beyond the `own` that the caller itself
    /// registered (immediately when that is so, or when the token is no longer the owner's). A caller that
    /// starts the Hub again (the owed recovery, the doctor's resume Start) waits through this, so the Hub is
    /// not started while a child that may still write is outstanding. Other phases of the owner still run.
    func whenChildrenQuiescent(_ token: HubLifecycleToken, besides own: Int = 0, _ block: @escaping () -> Void) {
        lock.lock()
        guard let entry, entry.token == token, entry.children > own else { lock.unlock(); block(); return }
        entry.childWaiters.append((atMost: own, block: block))
        lock.unlock()
    }

    func childCount(_ token: HubLifecycleToken) -> Int {
        lock.lock(); defer { lock.unlock() }
        return entry?.token == token ? entry?.children ?? 0 : 0
    }

    // MARK: Settlement

    /// The one terminal transition. The first call for the current token is accepted; the
    /// release waits for every child, then `deliver` runs once after the release. A
    /// stale or duplicate call returns false and runs nothing.
    @discardableResult
    func settle(_ token: HubLifecycleToken,
                terminal: Terminal = .open,
                then deliver: @escaping () -> Void) -> Bool {
        lock.lock()
        guard let entry, entry.token == token, !entry.settling else {
            record(.staleIgnored(token.family, generation: token.generation))
            lock.unlock()
            return false
        }
        entry.settling = true
        entry.terminal = terminal
        entry.deliver = deliver
        guard entry.children == 0 else { lock.unlock(); return true }
        let release = releaseLocked()
        lock.unlock()
        finish(release)
        return true
    }

    private struct Release {
        let snapshot: HubLifecycleCapabilities
        let deliver: (() -> Void)?
        let waiters: [() -> Void]
    }

    private func releaseLocked() -> Release {
        let released = entry!
        entry = nil
        switch (state, released.terminal) {
        case (.deleting, .deleted): state = .deleted; deletionRan = true
        case (.deleting, .open): state = .open
        default: break
        }
        record(.released(released.token.family, generation: released.token.generation, released.terminal))
        publicationCounter += 1
        let waiters = idleWaiters
        idleWaiters = []
        return Release(snapshot: capturedLocked(), deliver: released.deliver, waiters: waiters)
    }

    private func finish(_ release: Release) {
        publish(release.snapshot)
        release.deliver?()
        release.waiters.forEach { $0() }
        onIdle?()
    }

    /// Calls back once, immediately when nothing is owned.
    func whenIdle(_ callback: @escaping () -> Void) {
        lock.lock()
        if entry == nil { lock.unlock(); callback(); return }
        idleWaiters.append(callback)
        lock.unlock()
    }

    // MARK: Closing

    /// What `beginClosing` decided, under the one lock that also made the transition.
    enum ClosingEntry: Equatable {
        /// This call closed the lifecycle: nothing is admitted after it. `ownerHeld` says whether a
        /// (background) owner still holds it, which Quit must wait for.
        case entered(ownerHeld: Bool)
        /// Already closing or closed (a second Quit while the first waits): nothing changed.
        case alreadyClosing
        /// A foreground owner holds the lifecycle, a Delete is before its data step, or an owner holds a child that
        /// cannot be shown gone: it stays open.
        case refused
    }

    /// Accepted termination: no later admission, and queued reconciliation is invalidated.
    @discardableResult
    func beginClosing() -> ClosingEntry {
        lock.lock()
        switch state {
        case .closing, .closed: lock.unlock(); return .alreadyClosing
        case .deleting: lock.unlock(); return .refused
        case .open, .deleted:
            // The decision and the transition share this lock: a foreground owner admitted a moment
            // ago refuses the Quit here, whatever an earlier snapshot of the lifecycle said.
            // F8-2: so does an owner of any family that holds a child which cannot be shown gone.
            if let entry, !entry.token.family.isBackground || entry.held > 0 { lock.unlock(); return .refused }
        }
        state = .closing
        intent += 1
        publicationCounter += 1
        let snapshot = capturedLocked()
        let ownerHeld = entry != nil
        lock.unlock()
        publish(snapshot)
        return .entered(ownerHeld: ownerHeld)
    }

    func markClosed() {
        lock.lock()
        guard state == .closing else { lock.unlock(); return }
        state = .closed
        publicationCounter += 1
        let snapshot = capturedLocked()
        lock.unlock()
        publish(snapshot)
    }

    // MARK: Preference writes

    /// Runs `body` only while the lifecycle is open, serialised with the Delete barrier.
    func performPreferenceWrite(_ body: () -> Void) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard state == .open else { return false }
        body()
        return true
    }

    // MARK: Publication

    final class Observation {
        private weak var authority: HubLifecycleAuthority?
        private let identifier: UUID
        fileprivate init(authority: HubLifecycleAuthority, identifier: UUID) {
            self.authority = authority
            self.identifier = identifier
        }
        func invalidate() { authority?.removeObserver(identifier) }
        deinit { invalidate() }
    }

    func observe(_ block: @escaping (HubLifecycleCapabilities) -> Void) -> Observation {
        let identifier = UUID()
        lock.lock(); observers[identifier] = block; lock.unlock()
        return Observation(authority: self, identifier: identifier)
    }

    fileprivate func removeObserver(_ identifier: UUID) {
        lock.lock(); observers.removeValue(forKey: identifier); lock.unlock()
    }

    /// On the main queue: immediately when already there, so the interface follows an
    /// acquisition made by a user action without a refresh.
    private func publish(_ snapshot: HubLifecycleCapabilities) {
        lock.lock()
        let blocks = Array(observers.values)
        lock.unlock()
        guard !blocks.isEmpty else { return }
        let deliver = { blocks.forEach { $0(snapshot) } }
        if Thread.isMainThread { deliver() } else { DispatchQueue.main.async(execute: deliver) }
    }
}

/// One admitted operation's context. It outlives the window that started it; every
/// child it launches is registered before launch, and its settlement waits for them.
final class HubLifecycleOperation {
    let token: HubLifecycleToken
    private let authority: HubLifecycleAuthority
    private let commandRunner: HubCommandRunning
    private let serviceRunner: HubServiceControlling

    init(token: HubLifecycleToken,
         authority: HubLifecycleAuthority,
         commandRunner: HubCommandRunning,
         serviceRunner: HubServiceControlling) {
        self.token = token
        self.authority = authority
        self.commandRunner = commandRunner
        self.serviceRunner = serviceRunner
    }

    private let settlementLock = NSLock()
    private var settlementClaimed = false

    var isLive: Bool { authority.isCurrent(token) }

    /// The first caller wins; a duplicate or late settlement is ignored.
    private func claimSettlement() -> Bool {
        settlementLock.lock(); defer { settlementLock.unlock() }
        guard !settlementClaimed else { return false }
        settlementClaimed = true
        return true
    }

    @discardableResult
    func phase(_ name: String) -> Bool { authority.advance(token, to: name) }

    private func child<Value>(_ launch: (@escaping (Value) -> Void) -> Void,
                              stale: Value,
                              completion: @escaping (Value) -> Void) {
        guard authority.beginChild(token) else { completion(stale); return }
        let once = HubCompletionOnce<Value> { [authority, token] value in
            // F6-3: a child that cannot be shown to be gone may still write. The failure is reported,
            // but the child stays registered, so the owner is not released and nothing else is admitted,
            // until the executor's quiescence says it can no longer write.
            if let pending = (value as? HubQuiescenceReporting)?.pendingQuiescence {
                authority.holdChild(token)
                completion(value)
                pending.whenQuiescent {
                    // F7-1: the termination handler that reaps the child runs on a Foundation queue. The
                    // release, and every window completion it delivers, belongs on the main queue.
                    let release = { authority.endHeldChild(token) }
                    if Thread.isMainThread { release() } else { DispatchQueue.main.async(execute: release) }
                }
                return
            }
            authority.endChild(token)
            completion(value)
        }
        launch { once.complete($0) }
    }

    private static var staleFailure: Result<String, Error> {
        .failure(HubActionError.commandFailed(
            HubL10n.text("hub.lifecycle.stale", fallback: "That Hub operation had already finished. Nothing was changed.")))
    }

    func run(_ arguments: [String],
             completion: @escaping (Result<String, Error>) -> Void) {
        child({ done in commandRunner.run(arguments: arguments, completion: done) },
              stale: Self.staleFailure, completion: completion)
    }

    func run(_ arguments: [String], stdin: String,
             completion: @escaping (Result<String, Error>) -> Void) {
        child({ done in commandRunner.run(arguments: arguments, stdin: stdin, completion: done) },
              stale: Self.staleFailure, completion: completion)
    }

    func run(_ arguments: [String],
             onOutputLine: @escaping (String) -> Void,
             completion: @escaping (Result<String, Error>) -> Void) {
        child({ done in commandRunner.run(arguments: arguments, onOutputLine: onOutputLine, completion: done) },
              stale: Self.staleFailure, completion: completion)
    }

    func service(_ arguments: [String],
                 completion: @escaping (Result<String, Error>) -> Void) {
        child({ [token] done in serviceRunner.run(arguments: arguments, owner: token, completion: done) },
              stale: Self.staleFailure, completion: completion)
    }

    /// F8-3: a service command that waits until the owner's other children are gone, as the owed start does:
    /// the doctor's resume Start does not run while the doctor child may still write.
    func serviceAfterOtherChildren(_ arguments: [String],
                                   completion: @escaping (Result<String, Error>) -> Void) {
        child({ [token, authority, serviceRunner] done in
            authority.whenChildrenQuiescent(token, besides: 1) {
                serviceRunner.run(arguments: arguments, owner: token, completion: done)
            }
        }, stale: Self.staleFailure, completion: completion)
    }

    func recoverAfterPause(completion: @escaping (Result<String, Error>) -> Void) {
        // F7-3: the owed start waits until the owner's other children are gone: while one of them may
        // still write, the Hub is not started again.
        child({ [token, authority, serviceRunner] done in
            authority.whenChildrenQuiescent(token, besides: 1) {
                serviceRunner.recoverAfterPause(owner: token, completion: done)
            }
        }, stale: Self.staleFailure, completion: completion)
    }

    func releaseRestartRecord() {
        guard isLive else { return }
        serviceRunner.releaseRestartRecord(owner: token)
    }

    func deleteHubAndData(completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        child({ [token] done in serviceRunner.deleteHubAndData(owner: token, completion: done) },
              stale: .failure(HubActionError.commandFailed(
                HubL10n.text("hub.lifecycle.stale", fallback: "That Hub operation had already finished. Nothing was changed."))),
              completion: completion)
    }

    /// Registers asynchronous work that is not a runner command (the SSH connection).
    func track<Value>(_ launch: (@escaping (Value) -> Void) -> Void,
                      stale: Value,
                      completion: @escaping (Value) -> Void) {
        child(launch, stale: stale, completion: completion)
    }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        child({ done in serviceRunner.loadedState(completion: done) },
              stale: .unknown(HubActionError.commandFailed("That Hub operation had already finished.")),
              completion: completion)
    }

    /// Releases the owner exactly once, after its children have gone, then delivers.
    /// A restart still owed to this owner is settled first: cancelling never abandons it.
    func settle(_ terminal: HubLifecycleAuthority.Terminal = .open, then deliver: @escaping () -> Void) {
        guard isLive, claimSettlement() else { return }
        if serviceRunner.restartRecordHeld(by: token) {
            recoverAfterPause { [authority, token] _ in
                authority.settle(token, terminal: terminal, then: deliver)
            }
            return
        }
        authority.settle(token, terminal: terminal, then: deliver)
    }

    func finish<Value>(_ result: Result<Value, Error>,
                       terminal: HubLifecycleAuthority.Terminal = .open,
                       completion: @escaping (Result<Value, Error>) -> Void) {
        settle(terminal) { completion(result) }
    }
}


/// The deletion barrier the controller installs itself (R-E1F6-13): restoration and
/// preference writes stop when a Delete is admitted, whichever surface asked for it.
protocol HubRestorationBarrier {
    /// Flips the barrier. Cheap and lock-free of AppKit: it runs inside the admission.
    func install()
    /// Makes current windows non-restorable. Main queue.
    func applyToExistingWindows()
    /// A Delete that was refused before its data step lifts the barrier again.
    func remove()
}

struct HubWindowRestorationBarrier: HubRestorationBarrier {
    func install() { HubWindowRestoration.setSuppressed(true) }
    func applyToExistingWindows() { HubWindowRestoration.applyToCurrentWindows() }
    func remove() { HubWindowRestoration.endSuppressing() }
}

/// The SSH connection of migration preparation, injectable so tests never open a tunnel.
typealias HubMigrationConnector = (
    _ host: String,
    _ user: String,
    _ port: Int,
    _ authentication: TeslaMateSSHAuthentication,
    _ usePasswordlessSudo: Bool,
    _ completion: @escaping (Result<TeslaMateServerImportSession, Error>) -> Void
) -> Void

/// A window's handle on an active SSH preparation. Cancelling it never abandons the
/// preparation: the controller closes whatever the connection produces.
final class HubMigrationPreparation {
    private let lock = NSLock()
    private var cancelled = false
    var isCancelled: Bool { lock.lock(); defer { lock.unlock() }; return cancelled }
    func cancel() { lock.lock(); cancelled = true; lock.unlock() }
}

/// The controller-owned account context of a Tesla sign-in (R-E1F6-8): held from before
/// the authentication window opens until setup finishes or the sign-in is cancelled.
final class HubAccountSignIn {
    fileprivate let operation: HubLifecycleOperation?
    private unowned let controller: HubController
    private let lock = NSLock()
    private var consumed = false

    fileprivate init(operation: HubLifecycleOperation?, controller: HubController) {
        self.operation = operation
        self.controller = controller
    }

    var isActive: Bool { operation?.isLive ?? false }

    private func consume() -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard !consumed else { return false }
        consumed = true
        return true
    }

    /// Completes the sign-in under its own context. A late callback after cancellation, or
    /// after the lifecycle ended, configures nothing.
    func configure(tokens: TeslaAuthTokens, vehicleID: Int64? = nil,
                   completion: @escaping (Result<Void, Error>) -> Void) {
        guard let operation else { completion(.failure(HubActionError.preview)); return }
        guard operation.isLive, consume() else {
            completion(.failure(HubActionError.commandFailed(
                HubL10n.text("hub.lifecycle.stale", fallback: "That Hub operation had already finished. Nothing was changed."))))
            return
        }
        controller.performLegacyConfiguration(operation, tokens: tokens, vehicleID: vehicleID) { result in
            operation.finish(result, completion: completion)
        }
    }

    /// Authentication was cancelled or failed: the context is settled, nothing changed.
    func cancel(completion: @escaping () -> Void = {}) {
        guard let operation, consume() else { completion(); return }
        operation.settle { completion() }
    }
}

// MARK: - Service lifecycle

enum HubServiceAction: Equatable {
    case start
    case stop
    case pause
    case restart
    case remove
    case delete
}

enum HubServiceLoadState {
    case loaded
    case unloaded
    case unknown(Error)
}

extension HubServiceLoadState: HubQuiescenceReporting {
    var pendingQuiescence: HubChildQuiescence? {
        guard case let .unknown(error) = self else { return nil }
        return HubActionError.pendingQuiescence(of: error)
    }
}

/// The outcome of a restart duty that followed a failed action (section 2.3.11.1).
struct HubServiceRestartReport: LocalizedError {
    enum Outcome: Equatable {
        case restarted(appliesNewDefinition: Bool)
        case unconfirmed
    }

    let original: Error
    let outcome: Outcome
    /// The error of the recovery step that could not complete, when its command may still be running (F7-2).
    var recoveryFailure: Error?

    var errorDescription: String? {
        let first = original.localizedDescription
        switch outcome {
        case let .restarted(appliesNewDefinition):
            var text = first + " " + HubL10n.text("hub.service.restarted_after_failure",
                                                    fallback: "The Hub was started again.")
            if appliesNewDefinition {
                text += " " + HubL10n.text("hub.service.restart_applies_new_definition",
                                           fallback: "Restart Hub applies the new definition.")
            }
            return text
        case .unconfirmed:
            return first + " " + HubL10n.text("hub.service.restart_unconfirmed",
                                              fallback: "The restart could not be confirmed.")
        }
    }

    var isUnconfirmed: Bool { outcome == .unconfirmed }
}

enum HubReconcileOutcome {
    case completed
    case writerFailed(Error)
    case restartFailed(Error)
    /// The loaded-state query failed with a process that may still run (F8-4); nothing was changed.
    case stateCheckFailed(Error)
}

extension HubReconcileOutcome: HubQuiescenceReporting {
    var pendingQuiescence: HubChildQuiescence? {
        switch self {
        case let .restartFailed(error), let .stateCheckFailed(error): return HubActionError.pendingQuiescence(of: error)
        case .completed, .writerFailed: return nil
        }
    }
}

/// Every call carries the owner it acts for (R-E1F6-3): the held restart record belongs
/// to the owner that made it, and no other owner can consume or clear it.
protocol HubServiceControlling {
    func loadedState(completion: @escaping (HubServiceLoadState) -> Void)
    func run(arguments: [String], owner: HubLifecycleToken,
             completion: @escaping (Result<String, Error>) -> Void)
    /// The typed recovery of a pause site (section 2.3.11.1 step 3): one attempt for the
    /// owner's held record, then the record is cleared. With none, one showing the job
    /// unloaded, or one that belongs to another owner, nothing is issued and the result
    /// is an empty success.
    func recoverAfterPause(owner: HubLifecycleToken,
                           completion: @escaping (Result<String, Error>) -> Void)
    /// Clears the owner's held record without a command (the custody site's refused start).
    func releaseRestartRecord(owner: HubLifecycleToken)
    /// Whether the held record belongs to this owner.
    func restartRecordHeld(by owner: HubLifecycleToken) -> Bool
    func deleteHubAndData(owner: HubLifecycleToken,
                          completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void)
    /// Launch-time reconciliation of the service file (D-E7) as one complete lifecycle
    /// operation: the writer step, and a restart only of a changed, loaded job. It adds
    /// no work once Delete Hub and Data owns the lifecycle.
    func reconcile(owner: HubLifecycleToken,
                   ensure: @escaping () throws -> HubLaunchAgentChange,
                   completion: @escaping (HubReconcileOutcome) -> Void)
    /// A held record showing the job loaded, or an action still owing its restart.
    var restartDutyOutstanding: Bool { get }
    /// Calls back once, immediately when nothing is outstanding.
    func whenRestartDutySettled(_ callback: @escaping () -> Void)
}

/// A wall-clock share on the controller's injected monotonic clock. Synchronous
/// writer and log work is excluded: it extends the share by the time it took.
private final class HubServiceShare {
    private(set) var deadline: TimeInterval

    init(deadline: TimeInterval) { self.deadline = deadline }

    func excludeSynchronousWork<T>(clock: () -> TimeInterval, _ work: () throws -> T) rethrows -> T {
        let started = clock()
        defer { deadline += max(0, clock() - started) }
        return try work()
    }
}

/// The action-start share begins when the first `bootstrap` or `kickstart` is about to
/// run, and the final `print` belongs to the same share.
private final class HubServiceStartShare {
    private var share: HubServiceShare?

    func current(clock: () -> TimeInterval) -> HubServiceShare {
        if let share { return share }
        let created = HubServiceShare(deadline: clock() + HubLaunchAgentContract.replaceActionStartShare)
        share = created
        return created
    }
}

/// The unload poll reached its deadline (or the end of its share).
private struct HubServiceStopDeadline: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

final class LaunchctlServiceController: HubServiceControlling {
    private let agent: HubLaunchAgentManaging
    private let launchctl: HubLaunchctlRunning
    private let preflight: (@escaping (Result<String, Error>) -> Void) -> Void
    private let dataRemover: HubDataRemoving?
    private let clock: () -> TimeInterval
    private let schedule: (TimeInterval, @escaping () -> Void) -> Void
    private let execute: (@escaping () -> Void) -> Void
    private let domain: String
    private let service: String
    private let plistPath: String

    private let stateLock = NSLock()
    /// The restart record: nil none; true the job was loaded before the first stop.
    private var heldRecord: Bool?
    /// The owner that made the held record. Only that owner may consume or clear it.
    private var heldOwner: HubLifecycleToken?
    private var actionsOwingRestart = 0
    private var settledCallbacks: [() -> Void] = []
    /// Complete lifecycle operations run one at a time, in submission order: no
    /// operation's callbacks interleave with another's.
    private var queuedOperations: [(@escaping () -> Void) -> Void] = []
    private var operationActive = false
    /// Delete Hub and Data owns the lifecycle from its start until it is refused; after
    /// it completes the lifecycle stays its own.
    private var deletionOwned = false

    init(agent: HubLaunchAgentManaging,
         launchctl: HubLaunchctlRunning = HubLaunchctlRunner(),
         preflight: @escaping (@escaping (Result<String, Error>) -> Void) -> Void = { completion in
             completion(.failure(HubActionError.commandFailed("The Hub checks are not configured.")))
         },
         dataRemover: HubDataRemoving? = nil,
         uid: uid_t = getuid(),
         plist: URL = URL(fileURLWithPath: NSHomeDirectory())
             .appendingPathComponent("Library/LaunchAgents/\(HubLaunchAgentContract.plistName)"),
         clock: @escaping () -> TimeInterval = { ProcessInfo.processInfo.systemUptime },
         schedule: @escaping (TimeInterval, @escaping () -> Void) -> Void = { delay, action in
             DispatchQueue.global(qos: .userInitiated).asyncAfter(deadline: .now() + delay, execute: action)
         },
         queue: DispatchQueue = DispatchQueue(label: "eu.teslatlas.hub.service-lifecycle"),
         executor: ((@escaping () -> Void) -> Void)? = nil) {
        self.agent = agent
        self.launchctl = launchctl
        self.preflight = preflight
        self.dataRemover = dataRemover
        self.clock = clock
        self.schedule = schedule
        self.execute = executor ?? { queue.async(execute: $0) }
        domain = "gui/\(uid)"
        service = "gui/\(uid)/\(HubLaunchAgentContract.label)"
        plistPath = plist.path
    }

    // MARK: Plan table

    /// The launchctl commands of one row of the plan (the writer call and the unload poll
    /// are the controller's own steps and are not listed).
    static func commandPlan(action: HubServiceAction,
                            loaded: Bool,
                            plistChanged: Bool,
                            domain: String,
                            service: String,
                            plist: String) -> [[String]] {
        switch action {
        case .start:
            if !loaded { return [["enable", service], ["bootstrap", domain, plist]] }
            return plistChanged
                ? [["enable", service], ["bootout", service], ["bootstrap", domain, plist]]
                // RunAtLoad + KeepAlive starts an unloaded job during bootstrap; a loaded,
                // unchanged job is only kicked.
                : [["enable", service], ["kickstart", service]]
        case .restart:
            return loaded
                ? [["enable", service], ["bootout", service], ["bootstrap", domain, plist]]
                : [["enable", service], ["bootstrap", domain, plist]]
        case .stop:
            return loaded ? [["disable", service], ["bootout", service]] : [["disable", service]]
        case .pause:
            return loaded ? [["bootout", service]] : []
        case .remove:
            return loaded ? [["bootout", service], ["enable", service]] : [["enable", service]]
        case .delete:
            return loaded
                ? [["disable", service], ["bootout", service], ["enable", service]]
                : [["disable", service], ["enable", service]]
        }
    }

    static func isKnownUnloadedPrintFailure(status: Int32, output: String, service: String) -> Bool {
        guard status == 113, let label = service.split(separator: "/").last else { return false }
        let expected = "Could not find service \"\(label)\" in domain for user gui: \(getuid())"
        return output.split(whereSeparator: \Character.isNewline).contains {
            $0.trimmingCharacters(in: .whitespacesAndNewlines) == expected
        }
    }

    // MARK: HubCommandRunning

    func run(arguments: [String], owner: HubLifecycleToken,
             completion: @escaping (Result<String, Error>) -> Void) {
        let action: HubServiceAction
        switch arguments.last {
        case "start": action = .start
        case "stop": action = .stop
        case "pause": action = .pause
        case "restart": action = .restart
        case "remove": action = .remove
        default:
            completion(.failure(HubActionError.commandFailed(
                HubL10n.text("hub.DevelopmentHubRuntime.472.142", fallback: "Unknown service action."))))
            return
        }
        let once = HubCompletionOnce(completion)
        serialized { [self] finished in
            let done: (Result<String, Error>) -> Void = { once.complete($0); finished() }
            // Another owner's restart record is never touched: nothing is issued.
            guard recordAllows(owner) else { done(.failure(Self.recordHeldElsewhere)); return }
            switch action {
            case .start, .restart: runReplace(action, owner: owner, completion: done)
            case .stop: runStop(owner: owner, completion: done)
            case .pause: runPause(owner: owner, completion: done)
            case .remove: runRemove(owner: owner, completion: done)
            case .delete: finished()
            }
        }
    }

    private static var recordHeldElsewhere: Error {
        HubActionError.commandFailed(
            HubL10n.text("hub.lifecycle.record_held_elsewhere",
                         fallback: "Another Hub operation is holding the service. Nothing was changed."))
    }

    /// A record held by another owner stays that owner's.
    private func recordAllows(_ owner: HubLifecycleToken) -> Bool {
        stateLock.lock(); defer { stateLock.unlock() }
        return heldRecord == nil || heldOwner == owner
    }

    func restartRecordHeld(by owner: HubLifecycleToken) -> Bool {
        stateLock.lock(); defer { stateLock.unlock() }
        return heldRecord != nil && heldOwner == owner
    }

    // MARK: Operation gate

    /// Queues one complete lifecycle operation. It starts, on the lifecycle queue, only
    /// when every earlier operation has called `finished`; `finished` runs after the
    /// operation's own completion has been delivered.
    private func serialized(_ operation: @escaping (_ finished: @escaping () -> Void) -> Void) {
        stateLock.lock()
        queuedOperations.append(operation)
        let startNow = !operationActive
        if startNow { operationActive = true }
        stateLock.unlock()
        if startNow { advanceOperations() }
    }

    private func advanceOperations() {
        stateLock.lock()
        guard !queuedOperations.isEmpty else {
            operationActive = false
            stateLock.unlock()
            return
        }
        let next = queuedOperations.removeFirst()
        stateLock.unlock()
        let gate = HubCompletionOnce<Void> { [self] _ in advanceOperations() }
        execute { next { gate.complete(()) } }
    }

    private var isDeletionOwned: Bool {
        stateLock.lock(); defer { stateLock.unlock() }
        return deletionOwned
    }

    private func setDeletionOwned(_ owned: Bool) {
        stateLock.lock(); deletionOwned = owned; stateLock.unlock()
    }

    func reconcile(owner: HubLifecycleToken,
                   ensure: @escaping () throws -> HubLaunchAgentChange,
                   completion: @escaping (HubReconcileOutcome) -> Void) {
        let once = HubCompletionOnce(completion)
        serialized { [self] finished in
            let done: (HubReconcileOutcome) -> Void = { once.complete($0); finished() }
            guard !isDeletionOwned, recordAllows(owner) else { done(.completed); return }
            let change: HubLaunchAgentChange
            do { change = try ensure() } catch { done(.writerFailed(error)); return }
            guard change != .unchanged else { done(.completed); return }
            queryLoaded(HubServiceShare(deadline: clock() + HubLaunchAgentContract.launchctlRunLimit)) { [self] queried in
                // An unloaded job is never started, and an unknown state changes nothing. A query whose process
                // may still run is reported, so that the reconciliation is not complete before it is gone.
                guard case .success(true) = queried else {
                    if case let .failure(error) = queried, HubActionError.pendingQuiescence(of: error) != nil {
                        done(.stateCheckFailed(error))
                    } else {
                        done(.completed)
                    }
                    return
                }
                runReplace(.restart, owner: owner) { result in
                    if case let .failure(error) = result { done(.restartFailed(error)); return }
                    done(.completed)
                }
            }
        }
    }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        execute { [self] in
            queryLoaded(HubServiceShare(deadline: clock() + HubLaunchAgentContract.launchctlRunLimit)) { result in
                switch result {
                case .success(true): completion(.loaded)
                case .success(false): completion(.unloaded)
                case let .failure(error): completion(.unknown(error))
                }
            }
        }
    }

    // MARK: Restart duty bookkeeping

    var restartDutyOutstanding: Bool {
        stateLock.lock(); defer { stateLock.unlock() }
        return heldRecord == true || actionsOwingRestart > 0
    }

    func whenRestartDutySettled(_ callback: @escaping () -> Void) {
        stateLock.lock()
        let outstanding = heldRecord == true || actionsOwingRestart > 0
        if outstanding { settledCallbacks.append(callback) }
        stateLock.unlock()
        if !outstanding { callback() }
    }

    func releaseRestartRecord(owner: HubLifecycleToken) {
        stateLock.lock()
        let mine = heldRecord == nil || heldOwner == owner
        stateLock.unlock()
        guard mine else { return }
        setHeldRecord(nil, owner: nil)
    }

    private func setHeldRecord(_ record: Bool?, owner: HubLifecycleToken?) {
        stateLock.lock()
        heldRecord = record
        heldOwner = record == nil ? nil : owner
        stateLock.unlock()
        notifyIfSettled()
    }

    private var currentRecord: Bool? {
        stateLock.lock(); defer { stateLock.unlock() }
        return heldRecord
    }

    private func beginOwingRestart() {
        stateLock.lock(); actionsOwingRestart += 1; stateLock.unlock()
    }

    private func endOwingRestart() {
        stateLock.lock(); actionsOwingRestart -= 1; stateLock.unlock()
        notifyIfSettled()
    }

    private func notifyIfSettled() {
        stateLock.lock()
        let outstanding = heldRecord == true || actionsOwingRestart > 0
        let callbacks = outstanding ? [] : settledCallbacks
        if !outstanding { settledCallbacks.removeAll() }
        stateLock.unlock()
        callbacks.forEach { $0() }
    }

    // MARK: launchctl steps

    private func command(_ arguments: [String], share: HubServiceShare,
                         completion: @escaping (Result<String, Error>) -> Void) {
        let remaining = share.deadline - clock()
        guard remaining > 0 else {
            completion(.failure(HubActionError.commandTimedOut))
            return
        }
        launchctl.run(arguments: arguments,
                      timeout: min(HubLaunchAgentContract.launchctlRunLimit, remaining)) { [execute] result in
            execute { completion(result) }
        }
    }

    private func queryLoaded(_ share: HubServiceShare,
                             completion: @escaping (Result<Bool, Error>) -> Void) {
        command(["print", service], share: share) { [service] result in
            switch result {
            case .success: completion(.success(true))
            case let .failure(error):
                if case let HubActionError.commandExited(status, output) = error,
                   Self.isKnownUnloadedPrintFailure(status: status, output: output, service: service) {
                    completion(.success(false))
                } else {
                    completion(.failure(error))
                }
            }
        }
    }

    private func stopMessage(persistent: Bool) -> Error {
        var text = HubL10n.text("hub.HubController.1151.476", fallback: "Hub service did not finish stopping.")
        if persistent {
            text += " " + HubL10n.text("hub.service.stop_disabled",
                                       fallback: "The Hub will not start at login.")
        }
        return HubServiceStopDeadline(message: text)
    }

    /// Polls `print` until the service is unloaded: every 0.1 s for the first 10 s, then
    /// every 1 s, ending at `unloadPollDeadline` and never beyond the stop share.
    private func pollUntilUnloaded(share: HubServiceShare,
                                   persistent: Bool,
                                   completion: @escaping (Result<Void, Error>) -> Void) {
        let started = clock()
        let deadline = min(started + HubLaunchAgentContract.unloadPollDeadline, share.deadline)
        func tick() {
            let now = clock()
            guard now < deadline else {
                completion(.failure(stopMessage(persistent: persistent)))
                return
            }
            queryLoaded(HubServiceShare(deadline: min(deadline, share.deadline))) { result in
                switch result {
                case .success(false): completion(.success(()))
                case .success(true):
                    let elapsed = self.clock() - started
                    let spacing = elapsed < 10 ? 0.1 : 1.0
                    self.schedule(min(spacing, max(0.001, deadline - self.clock()))) {
                        self.execute { tick() }
                    }
                case let .failure(error): completion(.failure(error))
                }
            }
        }
        tick()
    }

    // MARK: Stop, pause, remove

    private func runStop(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) {
        setHeldRecord(nil, owner: nil)
        let share = HubServiceShare(deadline: clock() + HubLaunchAgentContract.replaceStopShare)
        queryLoaded(share) { [self] result in
            switch result {
            case let .failure(error): completion(.failure(error))
            case let .success(loaded):
                command(["disable", service], share: share) { [self] disabled in
                    if case let .failure(error) = disabled { completion(.failure(error)); return }
                    guard loaded else { completion(.success("")); return }
                    command(["bootout", service], share: share) { [self] result in
                        if case let .failure(error) = result { completion(.failure(error)); return }
                        pollUntilUnloaded(share: share, persistent: true) { polled in
                            completion(polled.map { "" })
                        }
                    }
                }
            }
        }
    }

    private func runPause(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) {
        let share = HubServiceShare(deadline: clock() + HubLaunchAgentContract.replaceStopShare)
        queryLoaded(share) { [self] result in
            switch result {
            case let .failure(error): completion(.failure(error))
            case let .success(loaded):
                if currentRecord == nil { setHeldRecord(loaded, owner: owner) }
                guard loaded else { completion(.success("")); return }
                command(["bootout", service], share: share) { [self] result in
                    if case let .failure(error) = result { completion(.failure(error)); return }
                    pollUntilUnloaded(share: share, persistent: false) { polled in
                        completion(polled.map { "" })
                    }
                }
            }
        }
    }

    private func runRemove(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) {
        setHeldRecord(nil, owner: nil)
        let share = HubServiceShare(deadline: clock() + HubLaunchAgentContract.replaceStopShare)
        let finish: () -> Void = { [self] in
            command(["enable", service], share: share) { [self] result in
                if case let .failure(error) = result { completion(.failure(error)); return }
                do {
                    try agent.removeInstalled()
                    completion(.success(""))
                } catch {
                    completion(.failure(error))
                }
            }
        }
        queryLoaded(share) { [self] result in
            switch result {
            case let .failure(error): completion(.failure(error))
            case let .success(loaded):
                guard loaded else { finish(); return }
                command(["bootout", service], share: share) { [self] result in
                    if case let .failure(error) = result { completion(.failure(error)); return }
                    pollUntilUnloaded(share: share, persistent: false) { polled in
                        switch polled {
                        case .success: finish()
                        case let .failure(error): completion(.failure(error))
                        }
                    }
                }
            }
        }
    }

    // MARK: Start and restart (RD1)

    private func runReplace(_ action: HubServiceAction,
                            owner: HubLifecycleToken,
                            completion: @escaping (Result<String, Error>) -> Void) {
        let heldAtStart = currentRecord
        // Own duty: this action found the job loaded and plans to stop it.
        var owing = false
        let finish: (Result<String, Error>) -> Void = { [self] result in
            let record = heldAtStart ?? (owing ? true : nil)
            let settle: (Result<String, Error>) -> Void = { [self] final in
                setHeldRecord(nil, owner: nil)
                if owing { endOwingRestart(); owing = false }
                completion(final)
            }
            guard case let .failure(original) = result, record == true else {
                settle(result)
                return
            }
            // Every return path of a recorded-loaded action runs one recovery first.
            runRecovery(original: original) { settle(.failure($0)) }
        }

        let change: HubLaunchAgentChange
        do {
            change = try agent.ensureInstalled(requireConfig: true)
        } catch {
            finish(.failure(error))
            return
        }
        preflight { [self] preflightResult in
            execute { [self] in
                if case let .failure(error) = preflightResult {
                    finish(.failure(Self.preflightFailure(error)))
                    return
                }
                let stopShare = HubServiceShare(deadline: clock() + HubLaunchAgentContract.replaceStopShare)
                queryLoaded(stopShare) { [self] queried in
                    switch queried {
                    case let .failure(error): finish(.failure(error))
                    case let .success(loaded):
                        // A held pause record already stopped the job: never a second bootout.
                        let plan = Self.commandPlan(action: action, loaded: loaded,
                                                    plistChanged: change != .unchanged && heldAtStart == nil,
                                                    domain: domain, service: service, plist: plistPath)
                        if loaded, heldAtStart == nil, plan.contains(where: { $0.first == "bootout" }) {
                            owing = true
                            beginOwingRestart()
                        }
                        let startShare = HubServiceStartShare()
                        executeReplacePlan(plan, index: 0, stopShare: stopShare, startShare: startShare) { [self] result in
                            switch result {
                            case .success:
                                finalPrint(startShare.current(clock: clock)) { finish($0.map { "" }) }
                            case let .failure(error): finish(.failure(error))
                            }
                        }
                    }
                }
            }
        }
    }

    private func finalPrint(_ share: HubServiceShare, completion: @escaping (Result<Void, Error>) -> Void) {
        queryLoaded(share) { result in
            switch result {
            case .success(true): completion(.success(()))
            case .success(false):
                completion(.failure(HubActionError.commandFailed(
                    HubL10n.text("hub.service.not_loaded_after_start",
                                 fallback: "The Hub service did not load."))))
            case let .failure(error): completion(.failure(error))
            }
        }
    }

    /// `bootstrap` and `kickstart` belong to the action-start share; everything before them
    /// (including the unload poll) to the stop share. `prepareLogsForBootstrap` runs
    /// immediately before each `bootstrap`.
    private func executeReplacePlan(_ plan: [[String]], index: Int,
                                    stopShare: HubServiceShare, startShare: HubServiceStartShare,
                                    completion: @escaping (Result<Void, Error>) -> Void) {
        guard index < plan.count else { completion(.success(())); return }
        let step = plan[index]
        let isStartPhase = step.first == "bootstrap" || step.first == "kickstart"
        if step.first == "bootstrap" {
            // Synchronous log work comes before the start share begins.
            do { try agent.prepareLogsForBootstrap() } catch {
                completion(.failure(error))
                return
            }
        }
        let share = isStartPhase ? startShare.current(clock: clock) : stopShare
        command(step, share: share) { [self] result in
            if case let .failure(error) = result { completion(.failure(error)); return }
            let next = { [self] in
                executeReplacePlan(plan, index: index + 1, stopShare: stopShare,
                                   startShare: startShare, completion: completion)
            }
            guard step.first == "bootout" else { next(); return }
            pollUntilUnloaded(share: stopShare, persistent: false) { polled in
                switch polled {
                case .success: next()
                case let .failure(error): completion(.failure(error))
                }
            }
        }
    }

    private static func preflightFailure(_ error: Error) -> Error {
        // F7-2: the preflight is a Hub CLI child; the message must not hide that it may still run.
        HubActionError.keepingQuiescence(of: error, in: HubActionError.commandFailed(
            HubL10n.format("hub.service.preflight_failed",
                           fallback: "The Hub did not start because its checks failed: %1$@",
                           arguments: [String(describing: HubActionError.settingQuiescenceAside(error).localizedDescription)])
        ))
    }

    // MARK: Recovery (section 2.3.11.1 step 3)

    /// One recovery attempt in its own share: never `ensureInstalled`, never the preflight.
    /// `print`; loaded: `kickstart -k`; unloaded: `enable`, logs, `bootstrap`; then one `print`.
    private func runRecovery(original: Error, completion: @escaping (Error) -> Void) {
        let share = HubServiceShare(deadline: clock() + HubLaunchAgentContract.replaceRecoveryShare)
        let settle: (Result<Bool, Error>) -> Void = { result in
            switch result {
            case let .success(usedKickstart):
                completion(HubServiceRestartReport(original: original,
                                                   outcome: .restarted(appliesNewDefinition: usedKickstart)))
            case let .failure(error):
                completion(HubServiceRestartReport(original: original, outcome: .unconfirmed, recoveryFailure: error))
            }
        }
        queryLoaded(share) { [self] queried in
            switch queried {
            case let .failure(error): settle(.failure(error))
            case .success(true):
                command(["kickstart", "-k", service], share: share) { [self] result in
                    if case let .failure(error) = result { settle(.failure(error)); return }
                    finalPrint(share) { settle($0.map { true }) }
                }
            case .success(false):
                command(["enable", service], share: share) { [self] result in
                    if case let .failure(error) = result { settle(.failure(error)); return }
                    do {
                        try share.excludeSynchronousWork(clock: clock) { try agent.prepareLogsForBootstrap() }
                    } catch { settle(.failure(error)); return }
                    command(["bootstrap", domain, plistPath], share: share) { [self] result in
                        if case let .failure(error) = result { settle(.failure(error)); return }
                        finalPrint(share) { settle($0.map { false }) }
                    }
                }
            }
        }
    }

    func recoverAfterPause(owner: HubLifecycleToken,
                           completion: @escaping (Result<String, Error>) -> Void) {
        let once = HubCompletionOnce(completion)
        serialized { [self] finished in
            let done: (Result<String, Error>) -> Void = { once.complete($0); finished() }
            // Another owner's record is neither consumed nor cleared.
            guard recordAllows(owner) else { done(.success("")); return }
            guard currentRecord == true else {
                setHeldRecord(nil, owner: nil)
                done(.success(""))
                return
            }
            beginOwingRestart()
            runRecovery(original: HubActionError.commandFailed("")) { [self] outcome in
                setHeldRecord(nil, owner: nil)
                endOwingRestart()
                guard let report = outcome as? HubServiceRestartReport else {
                    done(.failure(outcome))
                    return
                }
                switch report.outcome {
                case .restarted:
                    done(.success(HubL10n.text("hub.service.restarted_after_failure",
                                               fallback: "The Hub was started again.")))
                case .unconfirmed:
                    done(.failure(report))
                }
            }
        }
    }

    // MARK: Delete Hub and Data (section 2.3.10.4)

    func deleteHubAndData(owner: HubLifecycleToken,
                          completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        let once = HubCompletionOnce(completion)
        serialized { [self] finished in
            // From here no reconciliation adds work and no restart duty is held.
            setDeletionOwned(true)
            runDelete { [self] result in
                if case .failure = result { setDeletionOwned(false) }
                once.complete(result)
                finished()
            }
        }
    }

    private func runDelete(completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        guard let dataRemover else {
            completion(.failure(HubActionError.preview))
            return
        }
        setHeldRecord(nil, owner: nil)
        // 1. Read-only checks: no launchctl command, no file change.
        do {
            _ = try dataRemover.check()
            try agent.validateRemovable()
        } catch {
            completion(.failure(error))
            return
        }
        let share = HubServiceShare(deadline: clock() + HubLaunchAgentContract.replaceStopShare)
        let stopFailed: (Error) -> Error = { error in
            HubActionError.keepingQuiescence(of: error, in: HubActionError.commandFailed(
                HubL10n.format("hub.dataRemoval.stop_failed",
                               fallback: "The background service could not be stopped (%1$@). Nothing was deleted.",
                               arguments: [String(describing: HubActionError.settingQuiescenceAside(error).localizedDescription)])))
        }
        // 2. Persistent stop: disable, print, conditional bootout, bounded poll.
        command(["disable", service], share: share) { [self] disabled in
            if case let .failure(error) = disabled { completion(.failure(stopFailed(error))); return }
            queryLoaded(share) { [self] queried in
                switch queried {
                case let .failure(error): completion(.failure(stopFailed(error)))
                case let .success(loaded):
                    let afterStop: () -> Void = { [self] in removePlistThenData(share: share, completion: completion) }
                    guard loaded else { afterStop(); return }
                    command(["bootout", service], share: share) { [self] result in
                        if case let .failure(error) = result { completion(.failure(stopFailed(error))); return }
                        pollUntilUnloaded(share: share, persistent: false) { polled in
                            switch polled {
                            case .success: afterStop()
                            case let .failure(error) where error is HubServiceStopDeadline:
                                completion(.failure(HubActionError.commandFailed(
                                    error.localizedDescription + " " + HubL10n.text(
                                        "hub.dataRemoval.deadline_nothing_deleted",
                                        fallback: "Nothing was deleted. The Hub will not start at login. Try again in a few minutes."))))
                            case let .failure(error):
                                completion(.failure(stopFailed(error)))
                            }
                        }
                    }
                }
            }
        }
    }

    private func removePlistThenData(share: HubServiceShare,
                                     completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        guard let dataRemover else { completion(.failure(HubActionError.preview)); return }
        // 3. The plist goes before any data.
        do { try agent.removeInstalled() } catch {
            completion(.failure(HubActionError.commandFailed(
                HubL10n.format("hub.dataRemoval.plist_failed",
                               fallback: "The Hub is stopped and will not start at login, but its background service file could not be removed (%1$@). Nothing was deleted.",
                               arguments: [String(describing: error.localizedDescription)]))))
            return
        }
        // 4. Second check, now that nothing writes into the folders.
        let plan: HubDataRemovalPlan
        do { plan = try dataRemover.check() } catch {
            let reason = (error as? HubDataRemovalRefusal)?.clause ?? error.localizedDescription
            completion(.failure(HubActionError.commandFailed(
                HubL10n.format("hub.dataRemoval.second_check",
                               fallback: "The background service was removed, but no data was deleted, because %1$@. Start Hub sets the service up again.",
                               arguments: [String(describing: reason)]))))
            return
        }
        // 5. The data step.
        var report = dataRemover.delete(plan)
        // 6. Final enable, after plist removal; failure is a warning.
        command(["enable", service], share: HubServiceShare(deadline: clock() + HubLaunchAgentContract.launchctlRunLimit)) { result in
            if case .failure = result {
                report.warnings.append(HubDataRemovalReport.finalEnableFailedWarning)
            }
            completion(.success(report))
        }
    }
}

final class HubController {
    /// The one lifecycle owner of the application (R-E1F6-1): every surface asks it, and
    /// it alone decides which operation may run. The application's one controller is
    /// authoritative; surfaces that share an operation share this controller.
    let lifecycle = HubLifecycleAuthority()

    var lifecycleCapabilities: HubLifecycleCapabilities { lifecycle.capabilities }

    /// Called on the main queue at acquisition, at phase changes and at settlement.
    func observeLifecycle(_ block: @escaping (HubLifecycleCapabilities) -> Void)
        -> HubLifecycleAuthority.Observation {
        lifecycle.observe(block)
    }

    /// Custody survives an entry-point window closing, until the service resume
    /// callback settles. Every application termination path asks the same controller.
    var diagnosticsPreventQuit: Bool { lifecycle.capabilities.diagnosticsHeld }

    static let previewOnboardingChecks = [
        HubOnboardingCheck(title: HubL10n.text("hub.HubController.1161.477", fallback: "Service"), detail: HubL10n.text("hub.HubController.1161.478", fallback: "Launch agent installed and responsive"), passed: true),
        HubOnboardingCheck(title: HubL10n.text("hub.HubController.1162.479", fallback: "Tesla account"), detail: HubL10n.text("hub.HubController.1162.480", fallback: "Credentials stored and valid"), passed: true),
        HubOnboardingCheck(title: HubL10n.text("hub.HubController.1163.481", fallback: "Vehicle"), detail: HubL10n.text("hub.HubController.1163.482", fallback: "1 vehicle reachable"), passed: true),
        HubOnboardingCheck(title: HubL10n.text("hub.DiagnosticsWindowController.374.248", fallback: "Database"), detail: HubL10n.text("hub.HubController.1164.484", fallback: "SQLite schema at revision 42"), passed: true),
        HubOnboardingCheck(title: HubL10n.text("hub.AppDelegate.123.39", fallback: "Diagnostics"), detail: HubL10n.text("hub.HubController.1165.486", fallback: "Preflight checks passed"), passed: true),
        HubOnboardingCheck(title: HubL10n.text("hub.HubController.1166.487", fallback: "Logs"), detail: HubL10n.text("hub.DiagnosticsWindowController.377.255", fallback: "No errors in the last hour"), passed: true)
    ]

    let previewMode: Bool
    let previewScene: HubPreviewScene?
    let onboardingPreviewRoute: String?
    let developmentConfiguration: DevelopmentHubConfiguration?
    /// Remove Background Service and Delete Hub and Data act on the production service
    /// only, never in preview or against a source-run development Hub.
    var allowsServiceRemoval: Bool { developmentConfiguration == nil && !previewMode }
    private let commandRunner: HubCommandRunning
    private let serviceRunner: HubServiceControlling
    let launchAgent: HubLaunchAgentManaging?
    let dataRemover: HubDataRemoving?
    private let isTestHost: Bool
    private let statusLifecycle: HubStatusProcessLifecycle
    private let statusPauseBound: TimeInterval
    private let restorationBarrier: HubRestorationBarrier
    private let staleImportCleaner: () -> TeslaMateServerImporter.StaleTemporaryCleanup
    private let migrationConnector: HubMigrationConnector
    /// Background reconciliation requested while another owner held the lifecycle.
    private let deferredLock = NSLock()
    private var deferredReconciles: [(completion: (() -> Void)?, intent: UInt64)] = []
    /// Prepared (idle) migration sessions: their tunnel and directory lease belong here.
    private let sessionLock = NSLock()
    private var registeredSessions: [ObjectIdentifier: TeslaMateServerImportSession] = [:]
    private var serviceDiagnostic: String?
    private let homeDirectory: URL
    private let configuredConfigPath: URL?
    private let configuredDataDirectory: URL?
    private let configuredLogDirectory: URL?
    private let serviceInstalledOverride: Bool?
    private let migrationStartupReadinessPollInterval: TimeInterval
    private let migrationStartupReadinessTimeout: TimeInterval
    private let migrationStartupReadinessMaxAttempts: Int
    private let migrationStartupReadinessNow: () -> Date
    private let migrationStartupReadinessSchedule: (
        _ delay: TimeInterval,
        _ action: @escaping () -> Void
    ) -> Void
    private(set) var snapshot: HubSnapshot
    private var lastStatusFailureCode: String?
    private let refreshRequestLock = NSLock()
    private var newestRefreshRequest: UInt64 = 0

    /// Stands for "build the production writer for `homeDirectory`" in the initializer
    /// defaults, which cannot read another argument; an explicit nil means none.
    static let automaticLaunchAgent: HubLaunchAgentManaging = HubAutomaticLaunchAgent()
    static let automaticDataRemover: HubDataRemoving = HubAutomaticDataRemover()

    init(environment: [String: String] = ProcessInfo.processInfo.environment,
         commandRunner: HubCommandRunning = EmbeddedHubCommandRunner(),
         serviceRunner: HubServiceControlling? = nil,
         launchAgent: HubLaunchAgentManaging? = HubController.automaticLaunchAgent,
         dataRemover: HubDataRemoving? = HubController.automaticDataRemover,
         homeDirectory: URL = URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true),
         serviceInstalledOverride: Bool? = nil,
         initialSnapshot: HubSnapshot? = nil,
         migrationStartupReadinessPollInterval: TimeInterval = 0.5,
         migrationStartupReadinessTimeout: TimeInterval = 60,
         migrationStartupReadinessMaxAttempts: Int = 121,
         migrationStartupReadinessNow: @escaping () -> Date = Date.init,
         migrationStartupReadinessSchedule: @escaping (
             _ delay: TimeInterval,
             _ action: @escaping () -> Void
         ) -> Void = { delay, action in
             DispatchQueue.global(qos: .userInitiated).asyncAfter(
                 deadline: .now() + delay,
                 execute: action
             )
         },
         developmentConfiguration: DevelopmentHubConfiguration? = nil,
         statusLifecycle: HubStatusProcessLifecycle = .shared,
         statusPauseBound: TimeInterval = 10,
         restorationBarrier: HubRestorationBarrier = HubWindowRestorationBarrier(),
         staleImportCleaner: @escaping () -> TeslaMateServerImporter.StaleTemporaryCleanup = {
             TeslaMateServerImporter.cleanupStaleTemporaryDirectoriesReporting()
         },
         migrationConnector: @escaping HubMigrationConnector = { host, user, port, authentication, sudo, completion in
             TeslaMateServerImporter.connect(host: host, user: user, port: port,
                                             authentication: authentication,
                                             usePasswordlessSudo: sudo, completion: completion)
         }) {
        self.statusLifecycle = statusLifecycle
        self.statusPauseBound = statusPauseBound
        self.restorationBarrier = restorationBarrier
        self.staleImportCleaner = staleImportCleaner
        self.migrationConnector = migrationConnector
        let scene = HubPreviewScene(environmentValue: environment["TESLATLAS_HUB_PREVIEW_SCENE"])
        let isPreview = environment["TESLATLAS_HUB_UI_PREVIEW"] == "1" || scene != nil
        let previewRoute = isPreview
            ? (scene?.onboardingRoute ?? environment["TESLATLAS_HUB_ONBOARDING_PREVIEW"])
            : nil
        previewMode = isPreview
        previewScene = scene
        onboardingPreviewRoute = previewRoute
        self.developmentConfiguration = developmentConfiguration
        self.commandRunner = commandRunner
        isTestHost = HubTestHost.isXCTest(environment: environment)
        let production = developmentConfiguration == nil && !isPreview
        let resolvedAgent: HubLaunchAgentManaging?
        if launchAgent === Self.automaticLaunchAgent {
            resolvedAgent = production
                ? HubLaunchAgentWriter(configuration: {
                    try HubLaunchAgentConfiguration.production(bundle: .main, home: homeDirectory)
                })
                : nil
        } else {
            resolvedAgent = launchAgent
        }
        let resolvedRemover: HubDataRemoving?
        if dataRemover === Self.automaticDataRemover {
            resolvedRemover = production ? HubDataRemover.production(home: homeDirectory) : nil
        } else {
            resolvedRemover = dataRemover
        }
        self.launchAgent = resolvedAgent
        self.dataRemover = resolvedRemover
        if let serviceRunner {
            self.serviceRunner = serviceRunner
        } else {
            self.serviceRunner = LaunchctlServiceController(
                agent: resolvedAgent ?? HubInertLaunchAgent(),
                preflight: { [commandRunner, homeDirectory] completion in
                    commandRunner.run(
                        arguments: ["--config",
                                    homeDirectory.appendingPathComponent("Library/Application Support/Teslatlas Hub/config.toml").path,
                                    "preflight"],
                        completion: completion)
                },
                dataRemover: resolvedRemover,
                plist: homeDirectory.appendingPathComponent("Library/LaunchAgents/\(HubLaunchAgentContract.plistName)")
            )
        }
        self.homeDirectory = homeDirectory
        configuredConfigPath = developmentConfiguration?.config
        configuredDataDirectory = developmentConfiguration?.stateDirectory
        configuredLogDirectory = developmentConfiguration?.logDirectory
        self.serviceInstalledOverride = serviceInstalledOverride
        self.migrationStartupReadinessPollInterval = max(0, migrationStartupReadinessPollInterval)
        self.migrationStartupReadinessTimeout = max(0, migrationStartupReadinessTimeout)
        self.migrationStartupReadinessMaxAttempts = max(1, migrationStartupReadinessMaxAttempts)
        self.migrationStartupReadinessNow = migrationStartupReadinessNow
        self.migrationStartupReadinessSchedule = migrationStartupReadinessSchedule
        let firstRunPreviewRoutes = [
            "welcome", "choose", "choose-migration", "provider", "fleet", "legacy",
            "migration", "migration-connected", "importing", "verify", "finish", "finish-migration"
        ]
        let previewSnapshot: HubSnapshot = firstRunPreviewRoutes.contains(previewRoute ?? "")
            ? .firstRun : .previewRunning
        snapshot = initialSnapshot ?? (isPreview ? previewSnapshot : .firstRun)
        // Deferred reconciliation is drained after the interface has had its completion.
        lifecycle.onIdle = { [weak self] in
            DispatchQueue.main.async { self?.drainDeferredReconciles() }
        }
    }

    static func applicationController(
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) throws -> HubController {
        guard let development = try DevelopmentHubConfiguration.from(environment: environment) else {
            return HubController(environment: environment)
        }
        let runner = DevelopmentHubCommandRunner(configuration: development)
        return HubController(
            environment: environment,
            commandRunner: runner,
            serviceRunner: DevelopmentLaunchctlServiceController(configuration: development),
            launchAgent: nil,
            dataRemover: nil,
            serviceInstalledOverride: true,
            developmentConfiguration: development
        )
    }

    var hasPendingMigrationHandover: Bool {
        !previewMode && FileManager.default.fileExists(atPath: migrationHandoverMarker.path)
    }

    var pendingMigrationHandoverPhase: HubMigrationHandoverPhase? {
        previewMode ? nil : migrationHandoverState?.phase
    }

    var pendingMigrationHandoverIsHistoryOnly: Bool {
        !previewMode && migrationHandoverState?.historyOnly == true
    }

    var historyOnlyControlActive: Bool {
        guard !previewMode else { return false }
        if migrationHandoverState?.historyOnly == true { return true }
        return (try? HistoryOnlyControl.isSelected(for: configPath)) ?? true
    }

    /// Reasons that are safe to decide synchronously before the first status refresh.
    /// Keeping the main window hidden for these paths prevents ready-app chrome from
    /// flashing before onboarding takes over.
    var shouldShowOnboardingBeforeInitialRefresh: Bool {
        if developmentConfiguration != nil {
            return onboardingPreviewRoute != nil || hasPendingMigrationHandover
        }
        return onboardingPreviewRoute != nil
            || hasPendingMigrationHandover
            || !FileManager.default.fileExists(atPath: configPath.path)
    }

    func shouldShowOnboarding(for snapshot: HubSnapshot) -> Bool {
        if developmentConfiguration != nil {
            return onboardingPreviewRoute != nil || hasPendingMigrationHandover
        }
        return onboardingPreviewRoute != nil
            || hasPendingMigrationHandover
            || !FileManager.default.fileExists(atPath: configPath.path)
            || snapshot.health == .needsInstall
            || snapshot.accountState == .notConfigured
    }

    func refresh(completion: @escaping (HubSnapshot) -> Void) {
        guard !previewMode else {
            completion(snapshot)
            return
        }
        // Observation overlaps ordinary lifecycle work (R-E1F6-14). It stops once a
        // Delete's data step has run and once Quit has been accepted.
        guard lifecycle.capabilities.observersAdmitted else {
            completion(snapshot)
            return
        }
        let request = beginRefreshRequest()
        serviceRunner.loadedState { [weak self] loaded in
            guard let self else { return }
            let installed = self.isServiceInstalled
            self.commandRunner.run(arguments: ["--config", self.configPath.path, "status"]) { result in
                let settle: (Bool) -> Void = { liveReady in
                    DispatchQueue.main.async {
                        guard self.isNewestRefreshRequest(request) else {
                            completion(self.snapshot)
                            return
                        }
                        switch result {
                        case let .success(output):
                            if let status = self.parseStatus(output) {
                                if let previousCode = self.lastStatusFailureCode {
                                    HubAppLog.shared.record("status.recovered", category: "dashboard",
                                                            fields: ["previous_error_code": previousCode])
                                }
                                self.lastStatusFailureCode = nil
                                self.snapshot = self.statusSnapshot(status, installed: installed,
                                                                    loaded: loaded)
                                if !liveReady, case .loaded = loaded {
                                    self.snapshot.health = .degraded
                                    self.snapshot.service = "Development Hub · HTTPS readiness unavailable"
                                    self.snapshot.controlVehicleID = nil
                                    self.snapshot.diagnosticLines.insert("The selected process has not returned a trusted HTTPS readiness response. Stored data remains available.", at: 0)
                                }
                            } else {
                                self.recordStatusFailure("invalid_status_output", installed: installed)
                                self.snapshot = self.fallbackSnapshot(installed: installed, loaded: loaded)
                            }
                        case let .failure(error):
                            self.recordStatusFailure(HubAppLog.errorCode(error), installed: installed)
                            self.snapshot = self.fallbackSnapshot(installed: installed, loaded: loaded)
                        }
                        completion(self.snapshot)
                    }
                }
                if case .loaded = loaded, let live = self.serviceRunner as? HubLiveReadinessChecking {
                    live.checkLiveReadiness(completion: settle)
                } else { settle(true) }
            }
        }
    }

    private func beginRefreshRequest() -> UInt64 {
        refreshRequestLock.lock()
        defer { refreshRequestLock.unlock() }
        newestRefreshRequest &+= 1
        return newestRefreshRequest
    }

    private func isNewestRefreshRequest(_ request: UInt64) -> Bool {
        refreshRequestLock.lock()
        defer { refreshRequestLock.unlock() }
        return request == newestRefreshRequest
    }

    private func recordStatusFailure(_ code: String, installed: Bool) {
        guard lastStatusFailureCode != code else { return }
        lastStatusFailureCode = code
        HubAppLog.shared.record("status.failed", category: "dashboard", level: "WARN",
                                fields: [
                                    "error_code": code,
                                    "service_installed": installed ? "true" : "false"
                                ])
    }

    /// Stops the Hub, turns it off for login, and removes its LaunchAgent file. Data,
    /// configuration and logs stay.
    func removeBackgroundService(completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        guard developmentConfiguration == nil else {
            completion(.failure(HubActionError.commandFailed(
                HubL10n.text("hub.service.remove_development_refused",
                             fallback: "A source-run development Hub cannot remove the production service.")
            )))
            return
        }
        guard let op = admit(.remove, completion: completion) else { return }
        let started = Date()
        HubAppLog.shared.record("remove.requested", category: "service")
        runServiceCommand(op, ["service", "remove"]) { [self] result in
            switch result {
            case .success:
                HubAppLog.shared.record("remove.completed", category: "service", fields: [
                    "duration_ms": String(Int(Date().timeIntervalSince(started) * 1000))
                ])
                refresh { _ in op.finish(.success(()), completion: completion) }
            case let .failure(error):
                op.finish(.failure(error), completion: completion)
            }
        }
    }

    /// True once Delete Hub and Data has started: windows stay non-restorable.
    var hubDeletionStarted: Bool {
        let state = lifecycle.capabilities.state
        return state == .deleting || lifecycle.deletionCompleted
    }

    /// True once the data step has run: the transition may be left by Quit alone.
    var hubDeletionCompleted: Bool { lifecycle.deletionCompleted }


    /// Delete Hub and Data (section 2.3.10.4): pause status reads, then the service
    /// controller's ordered stop, plist removal and data step.
    func deleteHubAndData(completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        guard developmentConfiguration == nil else {
            completion(.failure(HubActionError.commandFailed(
                HubL10n.text("hub.service.delete_development_refused",
                             fallback: "A source-run development Hub cannot delete the production service or its data.")
            )))
            return
        }
        // D is acquired before the status pause. The barrier is installed under the same
        // lock, so a preference write racing the Delete is refused or finishes first.
        guard let op = admit(.delete, phase: "admitted",
                             barrier: { [restorationBarrier] in restorationBarrier.install() },
                             refused: { refusal in
            HubAppLog.shared.record("delete.refused", category: "service", level: "WARN",
                                    fields: ["error_code": Self.refusalCode(refusal)])
            let message: String
            switch refusal {
            case .busy(.diagnostics):
                message = HubL10n.text("hub.dataRemoval.diagnostics_running",
                                       fallback: "The Hub's diagnostics are running. Nothing was changed. Try again when they have finished.")
            case .deleting, .deleted, .busy(.delete):
                message = HubL10n.text("hub.dataRemoval.already_started",
                                       fallback: "Delete Hub and Data has already started. Nothing was changed.")
            default:
                message = refusal.userMessage
            }
            completion(.failure(HubActionError.commandFailed(message)))
        }) else { return }
        restorationBarrier.applyToExistingWindows()
        HubAppLog.shared.record("delete.requested", category: "service")
        let started = Date()
        // A refusal before the data step: reads resume, the barrier lifts, the status is
        // refreshed, and only then does the lifecycle open again.
        let refuse: (Error) -> Void = { [self] error in
            statusLifecycle.resume()
            restorationBarrier.remove()
            HubAppLog.shared.record("delete.refused", category: "service", level: "WARN",
                                    fields: ["error_code": HubAppLog.errorCode(error)])
            refresh { _ in op.finish(.failure(error), completion: completion) }
        }
        DispatchQueue.global(qos: .userInitiated).async { [self] in
            op.phase("status-pause")
            guard statusLifecycle.pause(waitingUpTo: statusPauseBound) else {
                DispatchQueue.main.async {
                    refuse(HubActionError.commandFailed(
                        HubL10n.text("hub.dataRemoval.reads_busy",
                                     fallback: "The app is still reading the Hub's status. Nothing was changed. Try again in a moment.")))
                }
                return
            }
            op.phase("service")
            op.deleteHubAndData { result in
                DispatchQueue.main.async {
                    switch result {
                    case let .success(report):
                        // The data step has run: no refresh, no resumed reads, no later owner.
                        HubAppLog.shared.record("delete.completed", category: "service", fields: [
                            "duration_ms": String(Int(Date().timeIntervalSince(started) * 1000))
                        ])
                        op.finish(.success(report), terminal: .deleted, completion: completion)
                    case let .failure(error):
                        refuse(error)
                    }
                }
            }
        }
    }

    /// Launch-time reconciliation of the LaunchAgent file with the bundled program
    /// (D-E7). A no-op under a test host, in preview, in development, and before the
    /// file exists. A changed file on a loaded job restarts it; an unloaded job is
    /// never started. Failures are logged and shown as a status diagnostic, never as a
    /// modal.
    func reconcileLaunchAgent(completion: (() -> Void)? = nil) {
        guard !isTestHost, !previewMode, developmentConfiguration == nil,
              let launchAgent, launchAgent.isInstalled else {
            completion?()
            return
        }
        // L is acquired before the writer step. While another owner holds the lifecycle the
        // request is deferred (coalesced) and re-evaluated when it is idle again.
        switch lifecycle.acquire(.reconcile, phase: "writer") {
        case let .failure(refusal):
            guard case .busy = refusal else { completion?(); return }
            deferReconcile(completion)
        case let .success(token):
            let op = makeOperation(token)
            // The writer step and any restart run as one operation of the service
            // controller, serialised with every other lifecycle operation.
            // F7-2: it is a registered child, so a restart whose preflight cannot be shown gone keeps the owner.
            op.track({ (done: @escaping (HubReconcileOutcome) -> Void) in
                serviceRunner.reconcile(owner: token, ensure: { try launchAgent.ensureInstalled(requireConfig: false) },
                                        completion: done)
            }, stale: .completed) { [self] outcome in
                switch outcome {
                case .completed: break
                case let .writerFailed(error): recordServiceDiagnostic(error, event: "reconcile.failed")
                case let .restartFailed(error): recordServiceDiagnostic(error, event: "reconcile.restart_failed")
                case let .stateCheckFailed(error): recordServiceDiagnostic(error, event: "reconcile.state_check_failed")
                }
                op.settle { completion?() }
            }
        }
    }

    private func deferReconcile(_ completion: (() -> Void)?) {
        deferredLock.lock()
        deferredReconciles.append((completion, lifecycle.intentGeneration))
        deferredLock.unlock()
    }

    /// A deferred request runs only if no Stop, Remove, Delete or termination was admitted
    /// since it was made; it then reacquires and re-evaluates installation and load state.
    private func drainDeferredReconciles() {
        deferredLock.lock()
        let entries = deferredReconciles
        deferredReconciles.removeAll()
        deferredLock.unlock()
        guard !entries.isEmpty else { return }
        let current = lifecycle.intentGeneration
        let live = entries.filter { $0.intent == current }
        entries.filter { $0.intent != current }.forEach { $0.completion?() }
        guard !live.isEmpty else { return }
        reconcileLaunchAgent { live.forEach { $0.completion?() } }
    }

    private func discardDeferredReconciles() {
        deferredLock.lock()
        let entries = deferredReconciles
        deferredReconciles.removeAll()
        deferredLock.unlock()
        entries.forEach { $0.completion?() }
    }

    private func recordServiceDiagnostic(_ error: Error, event: String) {
        HubAppLog.shared.record(event, category: "service", level: "WARN",
                                fields: ["error_code": HubAppLog.errorCode(error)])
        DispatchQueue.main.async { self.serviceDiagnostic = error.localizedDescription }
    }

    /// A restart duty is outstanding while a record shows the job loaded or an action
    /// still owes its restart; Quit waits for it.
    var restartDutyOutstanding: Bool { serviceRunner.restartDutyOutstanding }

    func whenRestartDutySettled(_ callback: @escaping () -> Void) {
        serviceRunner.whenRestartDutySettled(callback)
    }

    func signOutTeslaAccount(completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        guard !hasPendingMigrationHandover else {
            completion(.failure(HubActionError.commandFailed(
                HubL10n.text("hub.HubController.1486.555", fallback: "Finish or cancel the TeslaMate migration handover before signing out.")
            )))
            return
        }
        // O is acquired before the persistent Stop (or, with no installed service, before
        // the control command) and held through the final refresh.
        guard let op = admit(.signOut, phase: "stop", completion: completion) else { return }
        HubAppLog.shared.record("sign_out.requested", category: "account")
        let signOut = { [self] in
            op.phase("control")
            op.run(["--config", configPath.path, "control", "sign-out"]) { [self] result in
                DispatchQueue.main.async {
                    switch result {
                    case .success:
                        HubAppLog.shared.record("sign_out.completed", category: "account")
                        self.refresh { _ in op.finish(.success(()), completion: completion) }
                    case let .failure(error):
                        HubAppLog.shared.record("sign_out.failed", category: "account", level: "ERROR",
                                                fields: ["error_code": HubAppLog.errorCode(error)])
                        self.refresh { _ in
                            op.finish(.failure(HubActionError.commandFailed(
                                HubL10n.format("hub.HubController.1504.567", fallback: "Disconnect did not finish cleanly. Hub status was refreshed; check whether the account is still connected. %1$@", arguments: [String(describing: error.localizedDescription)])
                            )), completion: completion)
                        }
                    }
                }
            }
        }
        // A signed-out Hub must not relaunch and fail its preflight at every login:
        // a persistent stop first, and the sign-out is abandoned when it fails.
        guard isServiceInstalled else { signOut(); return }
        op.service(["service", "stop"]) { [self] stopResult in
            switch stopResult {
            case .success:
                signOut()
            case let .failure(error):
                HubAppLog.shared.record("sign_out.stop_failed", category: "account", level: "ERROR",
                                        fields: ["error_code": HubAppLog.errorCode(error)])
                DispatchQueue.main.async {
                    self.refresh { _ in
                        op.finish(.failure(HubActionError.commandFailed(
                            HubL10n.format("hub.service.sign_out_stop_failed",
                                           fallback: "The Hub could not be stopped, so the account was not disconnected: %1$@",
                                           arguments: [String(describing: error.localizedDescription)])
                        )), completion: completion)
                    }
                }
            }
        }
    }

    func checkTeslaMateCompatibility(source: String,
                                     carID: String,
                                     passwordFile: String,
                                     acknowledgeV42CompatibleSchema: Bool,
                                     historyOnly: Bool = false,
                                     completion: @escaping (Result<HubTeslaMateCompatibility, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        // Pure validation first: it writes nothing and launches nothing.
        let arguments: [String]
        do {
            arguments = try compatibilityArguments(source: source, carID: carID,
                                                   passwordFile: passwordFile,
                                                   acknowledgeV42CompatibleSchema: acknowledgeV42CompatibleSchema,
                                                   historyOnly: historyOnly)
        } catch {
            completion(.failure(error))
            return
        }
        // Compatibility-only work owns the lifecycle but adds no Stop.
        guard let op = admit(.migration, phase: "compatibility", completion: completion) else { return }
        runCompatibilityCheck(op, arguments: arguments, historyOnly: historyOnly) { result in
            op.finish(result, completion: completion)
        }
    }

    private func compatibilityArguments(source: String,
                                        carID: String,
                                        passwordFile: String,
                                        acknowledgeV42CompatibleSchema: Bool,
                                        historyOnly: Bool) throws -> [String] {
        guard acknowledgeV42CompatibleSchema else {
            throw HubActionError.commandFailed(
                historyOnly
                    ? HubL10n.text("hub.HubController.1522.568", fallback: "Acknowledge the reviewed TeslaMate history schema before checking compatibility.")
                    : HubL10n.text("hub.HubController.1523.569", fallback: "Confirm TeslaMate 4.2.0 or newer before checking compatibility.")
            )
        }
        try Self.validateMigrationSource(source)
        if historyOnly {
            let host = URLComponents(string: source)?.host
            guard host == "127.0.0.1" || host == "::1" else {
                throw HubActionError.commandFailed(HubL10n.text("hub.HubController.1532.570", fallback: "Choose a local PostgreSQL source on 127.0.0.1 or ::1."))
            }
        }
        guard let selectedCarID = Int64(carID), selectedCarID > 0 else {
            throw HubActionError.commandFailed(HubL10n.text("hub.HubController.1536.571", fallback: "TeslaMate car ID must be a positive number."))
        }
        guard !passwordFile.isEmpty else {
            throw HubActionError.commandFailed(HubL10n.text("hub.HubController.1539.572", fallback: "Choose the protected PostgreSQL password file."))
        }
        return ["--config", configPath.path, "teslamate-check",
                "--source", source, "--car-id", String(selectedCarID),
                "--postgres-password-file", passwordFile,
                "--acknowledge-v4-2-compatible-schema"]
            + (historyOnly ? ["--history-only"] : [])
    }

    private func runCompatibilityCheck(_ op: HubLifecycleOperation,
                                       arguments: [String],
                                       historyOnly: Bool,
                                       completion: @escaping (Result<HubTeslaMateCompatibility, Error>) -> Void) {
        op.run(arguments) { result in
            DispatchQueue.main.async {
                switch result {
                case let .success(output):
                    guard let report = Self.parseTeslaMateCompatibility(output, historyOnly: historyOnly) else {
                        completion(.failure(HubActionError.commandFailed(
                            HubL10n.text("hub.HubController.1552.580", fallback: "TeslaMate compatibility check returned no valid report.")
                        )))
                        return
                    }
                    completion(.success(report))
                case let .failure(error):
                    if let report = Self.parseTeslaMateCompatibility(error.localizedDescription, historyOnly: historyOnly) {
                        completion(.success(report))
                    } else {
                        completion(.failure(HubActionError.commandFailed(
                            HubL10n.text("hub.HubController.1562.581", fallback: "Could not verify TeslaMate. Check the server connection and try again.")
                        )))
                    }
                }
            }
        }
    }

    func importTeslaMateOnline(source: String,
                               carID: String,
                               passwordFile: String,
                               encryptionKeyFile: String,
                               acknowledgeV42CompatibleSchema: Bool,
                               progress: @escaping (HubMigrationProgress) -> Void = { _ in },
                               completion: @escaping (Result<Void, Error>) -> Void) {
        HubAppLog.shared.record("import.requested", category: "teslamate_import",
                                fields: ["capture_mode": "online_snapshot"])
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        guard acknowledgeV42CompatibleSchema else {
            completion(.failure(HubActionError.commandFailed(
                HubL10n.text("hub.HubController.1585.586", fallback: "Confirm TeslaMate 4.2.0 or newer before importing.")
            )))
            return
        }
        guard !encryptionKeyFile.isEmpty else {
            completion(.failure(HubActionError.commandFailed(
                HubL10n.text("hub.HubController.1591.587", fallback: "Choose the TeslaMate ENCRYPTION_KEY file.")
            )))
            return
        }
        let checkArguments: [String]
        do {
            checkArguments = try compatibilityArguments(source: source, carID: carID,
                                                        passwordFile: passwordFile,
                                                        acknowledgeV42CompatibleSchema: acknowledgeV42CompatibleSchema,
                                                        historyOnly: false)
        } catch {
            HubAppLog.shared.record("import.preflight.failed", category: "teslamate_import",
                                    level: "ERROR",
                                    fields: ["error_code": HubAppLog.errorCode(error)])
            completion(.failure(error))
            return
        }
        // I is acquired once, before the compatibility check, and held through the import.
        guard let op = admit(.migration, phase: "compatibility", completion: completion) else { return }
        runCompatibilityCheck(op, arguments: checkArguments, historyOnly: false) { [self] checkResult in
            switch checkResult {
            case let .failure(error):
                HubAppLog.shared.record("import.preflight.failed", category: "teslamate_import",
                                        level: "ERROR",
                                        fields: ["error_code": HubAppLog.errorCode(error)])
                op.finish(.failure(error), completion: completion)
            case let .success(report):
                guard report.compatible else {
                    HubAppLog.shared.record("import.preflight.rejected", category: "teslamate_import",
                                            level: "WARN", fields: ["reason": report.reasonCode])
                    op.finish(.failure(HubActionError.commandFailed(report.message)), completion: completion)
                    return
                }
                HubAppLog.shared.record("import.preflight.completed", category: "teslamate_import")
                op.phase("import")
                prepareOnlineMigration(op,
                                       source: source,
                                       carID: carID,
                                       passwordFile: passwordFile,
                                       encryptionKeyFile: encryptionKeyFile,
                                       historyOnly: false,
                                       progress: progress,
                                       completion: { op.finish($0, completion: completion) })
            }
        }
    }

    func importTeslaMateHistoryOnlyOnline(source: String,
                                          carID: String,
                                          passwordFile: String,
                                          acknowledgeV42CompatibleSchema: Bool,
                                          progress: @escaping (HubMigrationProgress) -> Void = { _ in },
                                          completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        let checkArguments: [String]
        do {
            checkArguments = try compatibilityArguments(source: source, carID: carID,
                                                        passwordFile: passwordFile,
                                                        acknowledgeV42CompatibleSchema: acknowledgeV42CompatibleSchema,
                                                        historyOnly: true)
        } catch {
            completion(.failure(error))
            return
        }
        guard let op = admit(.migration, phase: "compatibility", completion: completion) else { return }
        runCompatibilityCheck(op, arguments: checkArguments, historyOnly: true) { [self] result in
            switch result {
            case let .success(report) where report.compatible:
                op.phase("import")
                prepareOnlineMigration(op, source: source, carID: carID,
                                       passwordFile: passwordFile,
                                       encryptionKeyFile: nil, historyOnly: true,
                                       progress: progress,
                                       completion: { op.finish($0, completion: completion) })
            case let .success(report):
                op.finish(.failure(HubActionError.commandFailed(report.message)), completion: completion)
            case let .failure(error):
                op.finish(.failure(error), completion: completion)
            }
        }
    }

    /// SSH preparation (R-E1F6-9): admitted before any tunnel or temporary directory
    /// exists. The connection is a registered child; whatever it produces is accounted
    /// for here even if the window that asked has gone. The prepared session is then an
    /// idle, registered session with its own directory lease, not an active owner.
    func connectMigrationServer(host: String,
                                user: String,
                                port: Int,
                                authentication: TeslaMateSSHAuthentication,
                                usePasswordlessSudo: Bool,
                                completion: @escaping (Result<TeslaMateServerImportSession, Error>) -> Void)
        -> HubMigrationPreparation? {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return nil }
        guard let op = admit(.migration, phase: "ssh-preparation", completion: completion) else { return nil }
        let preparation = HubMigrationPreparation()
        let connector = migrationConnector
        op.track({ (done: @escaping (Result<TeslaMateServerImportSession, Error>) -> Void) in
            connector(host, user, port, authentication, usePasswordlessSudo, done)
        }, stale: .failure(HubActionError.commandFailed(
            HubL10n.text("hub.lifecycle.stale", fallback: "That Hub operation had already finished. Nothing was changed."))),
        completion: { [self] result in
            switch result {
            case let .success(session):
                guard !preparation.isCancelled else {
                    // The window that asked is gone: nothing is left running or on disk.
                    session.close()
                    op.settle {}
                    return
                }
                registerMigrationSession(session)
                op.finish(.success(session), completion: completion)
            case let .failure(error):
                op.finish(.failure(error)) { (outcome: Result<TeslaMateServerImportSession, Error>) in
                    if !preparation.isCancelled { completion(outcome) }
                }
            }
        })
        return preparation
    }

    private func prepareOnlineMigration(_ op: HubLifecycleOperation,
                                        source: String,
                                        carID: String,
                                        passwordFile: String,
                                        encryptionKeyFile: String?,
                                        historyOnly: Bool,
                                        progress: @escaping (HubMigrationProgress) -> Void,
                                        completion: @escaping (Result<Void, Error>) -> Void) {
        let previousInterval = historyOnly ? 0 : (migrationHandoverState?.previousIntervalSeconds
            ?? configuredCollectorIntervalSeconds())
        let previousProvider = migrationHandoverState?.previousProvider
            ?? configuredCollectorProvider()
        let originalConfig: String?
        do {
            originalConfig = try readConfigIfPresent()
        } catch {
            completion(.failure(error))
            return
        }
        do {
            try writeMigrationHandoverMarker(
                HubMigrationHandoverState(phase: .importing,
                                          previousIntervalSeconds: previousInterval,
                                          previousProvider: previousProvider,
                                          historyOnly: historyOnly)
            )
            if !historyOnly { try ensureConfig(collectorIntervalSeconds: 0) }
        } catch {
            completion(.failure(recoverFailedImport(error, originalConfig: originalConfig)))
            return
        }
        let arguments = ["--config", configPath.path, "migrate",
                         "--source", source, "--car-id", carID,
                         "--postgres-password-file", passwordFile,
                         "--online-snapshot",
                         "--acknowledge-v4-2-compatible-schema"]
            + (historyOnly ? ["--history-only"] : ["--encryption-key-file", encryptionKeyFile ?? "",
                                                  "--preserve-existing-credentials"])
        let finish: (Result<Void, Error>) -> Void = { result in
            DispatchQueue.main.async { completion(result) }
        }
        let abortBeforeImport: (Error) -> Void = { [self] error in
            finish(.failure(recoverFailedImport(error, originalConfig: originalConfig)))
        }
        let failStartedImport: (Error) -> Void = { error in
            finish(.failure(Self.migrationStoppedError(error)))
        }
        let runImport = { [self] in
            op.phase("migrate")
            let diagnostics = HubMigrationProgressDiagnostics()
            let progressDelivery = HubMigrationProgressDelivery()
            HubAppLog.shared.record("import.process.started", category: "teslamate_import",
                                    fields: ["capture_mode": "online_snapshot"])
            op.run(arguments, onOutputLine: { line in
                guard let update = Self.parseMigrationProgress(line) else { return }
                diagnostics.received(update)
                progressDelivery.deliver(update, to: progress)
            }) { [self] result in
                progressDelivery.cancel()
                let durationFields = diagnostics.finish()
                switch result {
                case let .success(output):
                    guard Self.containsOnlineMigrationReport(output) else {
                        HubAppLog.shared.record("import.process.invalid_report",
                                                category: "teslamate_import", level: "ERROR",
                                                fields: durationFields)
                        failStartedImport(HubActionError.commandFailed(
                            HubL10n.text("hub.HubController.1718.615", fallback: "TeslaMate import returned no valid completion report. Hub remains stopped.")
                        ))
                        return
                    }
                    do {
                        if historyOnly { try writeHistoryOnlyControl() }
                        try writeMigrationHandoverMarker(
                            HubMigrationHandoverState(phase: .awaitingVerification,
                                                      previousIntervalSeconds: previousInterval,
                                                      previousProvider: previousProvider,
                                                      historyOnly: historyOnly)
                        )
                        HubAppLog.shared.record("import.completed", category: "teslamate_import",
                                                fields: durationFields.merging(
                                                    ["handover": "awaiting_verification"]
                                                ) { _, new in new })
                        finish(.success(()))
                    } catch {
                        HubAppLog.shared.record("import.handover.failed", category: "teslamate_import",
                                                level: "ERROR",
                                                fields: durationFields.merging(
                                                    ["error_code": HubAppLog.errorCode(error)]
                                                ) { _, new in new })
                        finish(.failure(HubActionError.commandFailed(
                            HubL10n.format("hub.HubController.1742.624", fallback: "Import completed, but Hub could not record the safe handover gate: %1$@. Hub remains stopped.", arguments: [String(describing: error.localizedDescription)])
                        )))
                    }
                case let .failure(error):
                    HubAppLog.shared.record("import.process.failed", category: "teslamate_import",
                                            level: "ERROR",
                                            fields: durationFields.merging(
                                                [
                                                    "error_code": HubAppLog.errorCode(error),
                                                    "reason_code": Self.migrationFailureReasonCode(error)
                                                ]
                                            ) { _, new in new })
                    failStartedImport(error)
                }
            }
        }
        // This controls Teslatlas Hub only. TeslaMate is never stopped or changed.
        if historyOnly {
            op.phase("stop")
            op.service(["service", "stop"]) { result in
                switch result {
                case .success: runImport()
                case let .failure(error): abortBeforeImport(error)
                }
            }
            return
        }
        guard isServiceInstalled else {
            runImport()
            return
        }
        op.phase("stop")
        op.service(["service", "stop"]) { stopResult in
            switch stopResult {
            case .success:
                HubAppLog.shared.record("local_hub.stopped", category: "teslamate_import")
                runImport()
            case let .failure(error):
                HubAppLog.shared.record("local_hub.stop_failed", category: "teslamate_import",
                                        level: "ERROR",
                                        fields: ["error_code": HubAppLog.errorCode(error)])
                abortBeforeImport(error)
            }
        }
    }

    static func parseTeslaMateCompatibility(_ output: String,
                                            historyOnly: Bool = false) -> HubTeslaMateCompatibility? {
        for line in output.split(whereSeparator: { $0.isNewline }) {
            guard let data = String(line).data(using: .utf8),
                  let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let status = root["status"] as? String,
                  let reason = root["reasonCode"] as? String,
                  let required = root["requiredVersion"] as? String,
                  let guidance = root["guidance"] as? String else { continue }
            if historyOnly && (root["credentialScope"] as? String != "history-only"
                               || root["applicationVersionStatus"] as? String != "unknown") {
                continue
            }
            return HubTeslaMateCompatibility(
                compatible: status == "compatible"
                    && reason == "v4_2_compatible_schema"
                    && required == "4.2.0",
                message: guidance,
                reasonCode: reason,
                requiredVersion: required
            )
        }
        return nil
    }

    static func parseMigrationProgress(_ line: String) -> HubMigrationProgress? {
        guard let data = line.data(using: .utf8),
              let payload = try? JSONDecoder().decode(HubMigrationProgressPayload.self, from: data),
              payload.event == "migration_progress",
              payload.totalRows > 0 else { return nil }
        let safePhase = payload.phase.flatMap { phase in
            phase.range(of: #"^[a-z0-9_-]{1,64}$"#, options: .regularExpression) == nil
                ? nil
                : phase
        }
        return HubMigrationProgress(completedRows: min(payload.completedRows, payload.totalRows),
                                    totalRows: payload.totalRows,
                                    phase: safePhase)
    }

    static func containsOnlineMigrationReport(_ output: String) -> Bool {
        output.split(whereSeparator: { $0.isNewline }).contains { line in
            guard let data = String(line).data(using: .utf8),
                  let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                return false
            }
            return root["status"] as? String == "imported"
                && root["captureMode"] as? String == "online-snapshot"
        }
    }

    func importTeslaMate(source: String,
                         carID: String,
                         passwordFile: String,
                         encryptionKeyFile: String,
                         acknowledgeV42CompatibleSchema: Bool,
                         completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        guard acknowledgeV42CompatibleSchema else {
            completion(.failure(HubActionError.commandFailed(
                HubL10n.text("hub.HubController.1846.655", fallback: "Confirm TeslaMate 4.2.0 or newer before importing from a direct PostgreSQL source.")
            )))
            return
        }
        importTeslaMateOnline(source: source,
                              carID: carID,
                              passwordFile: passwordFile,
                              encryptionKeyFile: encryptionKeyFile,
                              acknowledgeV42CompatibleSchema: acknowledgeV42CompatibleSchema,
                              completion: completion)
    }

    static func validateMigrationSource(_ source: String) throws {
        guard let components = URLComponents(string: source),
              let scheme = components.scheme?.lowercased(),
              scheme == "postgres" || scheme == "postgresql",
              let host = components.host, !host.isEmpty,
              components.path.count > 1 else {
            throw HubActionError.commandFailed(
                HubL10n.text("hub.HubController.1865.658", fallback: "PostgreSQL source must include a postgres or postgresql scheme, host, and database name.")
            )
        }
        guard components.password == nil else {
            throw HubActionError.commandFailed(HubL10n.text("hub.HubController.1869.659", fallback: "PostgreSQL source must not contain a password. Use the password file field."))
        }
    }

    func configureFleetAccount(credentials: HubFleetSetupCredentials,
                               completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        // A is acquired before configuration or data are prepared and held through the
        // Start (or its recovery) and the final result.
        guard let op = admit(.account, phase: "validate", completion: completion) else { return }
        performFleetConfiguration(op, credentials: credentials) { op.finish($0, completion: completion) }
    }

    private func performFleetConfiguration(_ op: HubLifecycleOperation,
                                           credentials: HubFleetSetupCredentials,
                                           completion: @escaping (Result<Void, Error>) -> Void) {
        let installed = isServiceInstalled
        let setupStarted = Date()
        HubAppLog.shared.record("setup.requested", category: "account", fields: [
            "provider": "fleet",
            "service_installed": installed ? "true" : "false"
        ])
        let invocation: HubSetupInvocation
        let originalConfig: String?
        do {
            invocation = try Self.fleetSetupInvocation(configPath: configPath,
                                                       credentials: credentials)
            originalConfig = try readConfigIfPresent()
        } catch {
            HubAppLog.shared.record("setup.rejected", category: "account", level: "WARN",
                                    fields: [
                                        "provider": "fleet",
                                        "error_code": HubAppLog.errorCode(error)
                                    ])
            completion(.failure(error))
            return
        }
        let finish: (Result<Void, Error>) -> Void = { [self] result in
            DispatchQueue.main.async {
                switch result {
                case .success:
                    HubAppLog.shared.record("setup.completed", category: "account", fields: [
                        "provider": "fleet",
                        "duration_ms": String(Int(Date().timeIntervalSince(setupStarted) * 1000))
                    ])
                    self.refresh { _ in completion(.success(())) }
                case let .failure(error):
                    HubAppLog.shared.record("setup.failed", category: "account", level: "ERROR",
                                            fields: [
                                                "provider": "fleet",
                                                "duration_ms": String(Int(Date().timeIntervalSince(setupStarted) * 1000)),
                                                "error_code": HubAppLog.errorCode(error)
                                            ])
                    completion(result)
                }
            }
        }
        // The ordinary Start: it writes the LaunchAgent file, runs the Hub preflight and
        // starts; a failure here is followed by the site's typed recovery.
        let startService = { [self] in
            op.phase("start")
            op.service(["service", "start"]) { [self] result in
                switch result {
                case .success: finish(.success(()))
                case let .failure(error): recoverThenFail(op, error, finish: finish)
                }
            }
        }
        guard installed else {
            do {
                op.phase("configuration")
                try ensureConfig(provider: "fleet")
            } catch {
                finish(.failure(error))
                return
            }
            op.phase("setup")
            op.run(invocation.arguments, stdin: invocation.standardInput) { setupResult in
                switch setupResult {
                case .success:
                    op.phase("start")
                    op.service(["service", "start"]) { startResult in
                        finish(startResult.map { _ in () })
                    }
                case let .failure(error): finish(.failure(error))
                }
            }
            return
        }

        // Installed: pause, ensure the configuration, set up with the embedded program,
        // then the ordinary Start. The service controller holds whether the job was
        // loaded before the pause and restarts it on every failure path (RD1).
        let restoreThenRecover = { [self] (setupError: Error) in
            do {
                try restoreConfig(originalConfig)
                recoverThenFail(op, setupError, finish: finish)
            } catch let recoveryError {
                recoverThenFail(op, HubActionError.commandFailed(
                    HubL10n.format("hub.HubController.1966.689", fallback: "Fleet setup failed: %1$@ Hub configuration recovery also failed: %2$@", arguments: [String(describing: setupError.localizedDescription), String(describing: recoveryError.localizedDescription)])
                ), finish: finish)
            }
        }
        op.phase("pause")
        op.service(["service", "pause"]) { [self] pauseResult in
            if case let .failure(error) = pauseResult {
                recoverThenFail(op, error, finish: finish)
                return
            }
            do {
                op.phase("configuration")
                try ensureConfig(provider: "fleet")
            } catch {
                restoreThenRecover(error)
                return
            }
            // No working old provider is restarted after setup: it may have migrated data.
            op.phase("setup")
            op.run(invocation.arguments, stdin: invocation.standardInput) { [self] setupResult in
                switch setupResult {
                case .success: startService()
                case let .failure(error):
                    // The configuration may already be in an unknown state: it is not
                    // restored, and the Hub is restarted only by the typed recovery.
                    recoverThenFail(op, Self.providerSwitchUnverifiedError(error), finish: finish)
                }
            }
        }
    }

    static func fleetSetupInvocation(configPath: URL,
                                     credentials: HubFleetSetupCredentials) throws -> HubSetupInvocation {
        guard !credentials.accessToken.isEmpty,
              !credentials.refreshToken.isEmpty,
              !credentials.clientID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              ["europe_middle_east_and_africa", "north_america_and_asia_pacific", "china"]
                .contains(credentials.region),
              credentials.expiresInSeconds > 0 else {
            throw HubActionError.commandFailed(HubL10n.text("hub.HubController.2096.708", fallback: "Fleet credentials are incomplete or invalid."))
        }
        let payload: [String: Any] = [
            "accessToken": credentials.accessToken,
            "refreshToken": credentials.refreshToken,
            "clientId": credentials.clientID,
            "region": credentials.region,
            "expiresInSeconds": credentials.expiresInSeconds
        ]
        let data = try JSONSerialization.data(withJSONObject: payload, options: [])
        guard let input = String(data: data, encoding: .utf8) else {
            throw HubActionError.commandFailed(HubL10n.text("hub.HubController.2107.714", fallback: "Could not encode Fleet credentials."))
        }
        return HubSetupInvocation(
            arguments: ["--config", configPath.path, "setup-fleet", "--all-vehicles"],
            standardInput: input
        )
    }

    func configureTeslaAccount(tokens: TeslaAuthTokens,
                               vehicleID: Int64? = nil,
                               completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        guard let op = admit(.account, phase: "validate", completion: completion) else { return }
        performLegacyConfiguration(op, tokens: tokens, vehicleID: vehicleID) { op.finish($0, completion: completion) }
    }

    /// The Tesla sign-in's account context (R-E1F6-8): held before the authentication
    /// window opens, so its completion continues under the same owner.
    func beginLegacySignIn() -> Result<HubAccountSignIn, Error> {
        guard !previewMode else { return .success(HubAccountSignIn(operation: nil, controller: self)) }
        var failure: Error?
        guard let op = admit(.account, phase: "authentication",
                             refused: { failure = HubActionError.lifecycleRefused($0) }) else {
            return .failure(failure ?? HubActionError.preview)
        }
        return .success(HubAccountSignIn(operation: op, controller: self))
    }

    fileprivate func performLegacyConfiguration(_ op: HubLifecycleOperation,
                                                tokens: TeslaAuthTokens,
                                                vehicleID: Int64?,
                                                completion: @escaping (Result<Void, Error>) -> Void) {
        let installed = isServiceInstalled
        let setupStarted = Date()
        HubAppLog.shared.record("setup.requested", category: "account", fields: [
            "provider": "legacy",
            "service_installed": installed ? "true" : "false"
        ])
        let invocation: HubSetupInvocation
        let installedInvocation: HubSetupInvocation
        let originalConfig: String?
        do {
            invocation = try Self.setupInvocation(configPath: configPath,
                                                  tokens: tokens,
                                                  vehicleID: vehicleID)
            installedInvocation = Self.oldCompatibleSetupInvocation(invocation)
            originalConfig = installed ? try readConfigIfPresent() : nil
        } catch {
            HubAppLog.shared.record("setup.rejected", category: "account", level: "WARN",
                                    fields: [
                                        "provider": "legacy",
                                        "error_code": HubAppLog.errorCode(error)
                                    ])
            completion(.failure(error))
            return
        }
        let finish: (Result<Void, Error>) -> Void = { [self] result in
            DispatchQueue.main.async {
                switch result {
                case .success:
                    HubAppLog.shared.record("setup.completed", category: "account", fields: [
                        "provider": "legacy",
                        "duration_ms": String(Int(Date().timeIntervalSince(setupStarted) * 1000))
                    ])
                    self.refresh { _ in completion(.success(())) }
                case let .failure(error):
                    HubAppLog.shared.record("setup.failed", category: "account", level: "ERROR",
                                            fields: [
                                                "provider": "legacy",
                                                "duration_ms": String(Int(Date().timeIntervalSince(setupStarted) * 1000)),
                                                "error_code": HubAppLog.errorCode(error)
                                            ])
                    completion(result)
                }
            }
        }
        let startService = { [self] in
            op.phase("start")
            op.service(["service", "start"]) { [self] result in
                switch result {
                case .success: finish(.success(()))
                case let .failure(error): recoverThenFail(op, error, finish: finish)
                }
            }
        }
        guard installed else {
            do {
                op.phase("configuration")
                try ensureConfig(provider: "legacy")
            } catch {
                finish(.failure(error))
                return
            }
            op.phase("setup")
            op.run(invocation.arguments, stdin: invocation.standardInput) { setupResult in
                switch setupResult {
                case .success:
                    op.phase("start")
                    op.service(["service", "start"]) { startResult in
                        finish(startResult.map { _ in () })
                    }
                case let .failure(error):
                    finish(.failure(error))
                }
            }
            return
        }

        let restoreThenRecover = { [self] (action: String, actionError: Error) in
            do {
                try restoreConfig(originalConfig)
                recoverThenFail(op, actionError, finish: finish)
            } catch let recoveryError {
                recoverThenFail(op, HubActionError.commandFailed(
                    HubL10n.format("hub.HubController.2268.756", fallback: "%1$@: %2$@ Hub configuration recovery also failed: %3$@", arguments: [String(describing: action), String(describing: actionError.localizedDescription), String(describing: recoveryError.localizedDescription)])
                ), finish: finish)
            }
        }
        op.phase("pause")
        op.service(["service", "pause"]) { [self] pauseResult in
            if case let .failure(error) = pauseResult {
                recoverThenFail(op, error, finish: finish)
                return
            }
            do {
                op.phase("configuration")
                try ensureConfig(provider: "legacy")
            } catch {
                restoreThenRecover(HubL10n.text("hub.HubController.2292.760", fallback: "Tesla setup failed"), error)
                return
            }
            // Configure with the embedded current binary; an older program that does not
            // know --all-vehicles gets the compatible invocation (retained, C9).
            op.phase("setup")
            op.run(invocation.arguments, stdin: invocation.standardInput) { [self] setupResult in
                let handleSetupResult: (Result<String, Error>) -> Void = { [self] result in
                    switch result {
                    case .success: startService()
                    case let .failure(error):
                        recoverThenFail(op, Self.providerSwitchUnverifiedError(error), finish: finish)
                    }
                }
                if case let .failure(error) = setupResult, Self.isAllVehiclesUnsupported(error) {
                    op.run(installedInvocation.arguments, stdin: installedInvocation.standardInput,
                           completion: handleSetupResult)
                } else {
                    handleSetupResult(setupResult)
                }
            }
        }
    }

    static func setupInvocation(configPath: URL,
                                tokens: TeslaAuthTokens,
                                vehicleID: Int64?) throws -> HubSetupInvocation {
        let payload = try JSONSerialization.data(withJSONObject: [
            "accessToken": tokens.accessToken,
            "refreshToken": tokens.refreshToken
        ], options: [])
        guard let input = String(data: payload, encoding: .utf8) else {
            throw HubActionError.commandFailed(HubL10n.text("hub.HubController.2341.768", fallback: "Could not encode Tesla login credentials."))
        }
        var arguments = ["--config", configPath.path, "setup", "--tokens-stdin"]
        if let vehicleID {
            guard vehicleID > 0 else {
                throw HubActionError.commandFailed(HubL10n.text("hub.HubController.2346.772", fallback: "Tesla vehicle ID must be positive."))
            }
            arguments += ["--vehicle-id", String(vehicleID)]
        } else {
            arguments.append("--all-vehicles")
        }
        return HubSetupInvocation(arguments: arguments, standardInput: input)
    }

    static func oldCompatibleSetupInvocation(_ invocation: HubSetupInvocation) -> HubSetupInvocation {
        HubSetupInvocation(arguments: invocation.arguments.filter { $0 != "--all-vehicles" },
                           standardInput: invocation.standardInput)
    }

    static func isProviderSwitchOutcomeAmbiguous(_ error: Error) -> Bool {
        error.localizedDescription.contains("TESLATLAS_PROVIDER_SWITCH_OUTCOME_AMBIGUOUS")
    }

    static func isMigrationOutcomeAmbiguous(_ error: Error) -> Bool {
        error.localizedDescription.contains("TESLATLAS_MIGRATION_OUTCOME_AMBIGUOUS")
    }

    /// The provider switch's outcome cannot be trusted. No claim is made about whether
    /// the Hub is stopped: the recovery outcome and the refreshed status say that.
    static func providerSwitchUnverifiedError(_ error: Error) -> Error {
        HubActionError.commandFailed(
            HubL10n.format("hub.HubController.provider_switch_unverified",
                           fallback: "Tesla provider switch outcome needs verification. Run diagnostics before retrying. %1$@",
                           arguments: [String(describing: error.localizedDescription)])
        )
    }

    /// A pause site failed (the pause itself, or the work after it). Runs the single typed
    /// recovery for the held record and reports the original error first and the
    /// recovery second; an unconfirmed recovery refreshes status and shows the stopped
    /// text only when status shows the service stopped.
    private func recoverThenFail(_ op: HubLifecycleOperation,
                                 _ original: Error,
                                 finish: @escaping (Result<Void, Error>) -> Void) {
        op.recoverAfterPause { [self] recovery in
            switch recovery {
            case let .success(note):
                // The original can itself be an unconfirmed restart report (the ordinary
                // Start and its own recovery failed): that refreshes and may add the
                // stopped text.
                reportServiceFailure(original) { reported in
                    finish(.failure(note.isEmpty
                        ? reported
                        : HubActionError.commandFailed(reported.localizedDescription + " " + note)))
                }
            case .failure:
                // An original that is itself a restart report is not described twice.
                let base = (original as? HubServiceRestartReport)?.original ?? original
                reportUnconfirmedRestart(base) { finish(.failure($0)) }
            }
        }
    }

    /// The restart could not be confirmed: refresh, and say the Hub did not restart only
    /// when the refreshed status shows it stopped. A job still running is left running.
    private func reportUnconfirmedRestart(_ original: Error, completion: @escaping (Error) -> Void) {
        unconfirmedRestartText(after: original) { completion(HubActionError.commandFailed($0)) }
    }

    private func unconfirmedRestartText(after original: Error?, completion: @escaping (String) -> Void) {
        var text = (original.map { $0.localizedDescription + " " } ?? "")
            + HubL10n.text("hub.service.restart_unconfirmed", fallback: "The restart could not be confirmed.")
        refresh { snapshot in
            if snapshot.health == .stopped {
                text += " " + HubL10n.text("hub.service.did_not_restart",
                                           fallback: "The Hub did not restart. Choose Start Hub.")
            }
            completion(text)
        }
    }

    /// A service command failed. A restart report that could not confirm the restart
    /// refreshes status first.
    private func reportServiceFailure(_ error: Error, completion: @escaping (Error) -> Void) {
        guard let report = error as? HubServiceRestartReport, report.isUnconfirmed else {
            completion(error)
            return
        }
        reportUnconfirmedRestart(report.original, completion: completion)
    }

    static func migrationStoppedError(_ error: Error) -> Error {
        let diagnostic = error.localizedDescription.lowercased()
        if diagnostic.contains("legacy refresh outcome is unresolved")
            || (diagnostic.contains("legacy")
                && diagnostic.contains("credential")
                && diagnostic.contains("consum")) {
            return HubActionError.commandFailed(
                HubL10n.text("hub.HubController.2385.784", fallback: "Sign in to Tesla again in TeslaMate, then retry the import.")
            )
        }
        return HubActionError.commandFailed(
            HubL10n.text("hub.HubController.2389.785", fallback: "TeslaMate import failed; retry the import.")
        )
    }

    static func migrationFailureReasonCode(_ failure: Error) -> String {
        // F7-6: a command that could not be stopped is classified by the failure underneath.
        let error = HubActionError.settingQuiescenceAside(failure)
        if HubAppLog.errorCode(error) == "command_timed_out" { return "timeout" }

        let diagnostic = error.localizedDescription.lowercased()
        if diagnostic.contains("timed out")
            || diagnostic.contains("timeout")
            || diagnostic.contains("deadline exceeded") {
            return "timeout"
        }
        if diagnostic.contains("no space left on device")
            || diagnostic.contains("database or disk is full")
            || diagnostic.contains("disk full")
            || diagnostic.contains("sqlite_full")
            || diagnostic.contains("storage full") {
            return "disk_space"
        }
        if diagnostic.contains("schema mismatch")
            || diagnostic.contains("schema version")
            || diagnostic.contains("incompatible schema")
            || diagnostic.contains("unsupported schema")
            || diagnostic.contains("migration version")
            || diagnostic.contains("teslamate version")
            || diagnostic.contains("requires teslamate")
            || diagnostic.contains("update teslamate") {
            return "schema_version"
        }
        if diagnostic.contains("ssh ")
            || diagnostic.contains("ssh:")
            || diagnostic.contains("ssh_")
            || diagnostic.contains("tunnel")
            || diagnostic.contains("port forwarding")
            || diagnostic.contains("could not resolve hostname") {
            return "ssh_tunnel"
        }
        if diagnostic.contains("legacy refresh")
            || diagnostic.contains("token pair")
            || diagnostic.contains("refresh token")
            || diagnostic.contains("credential")
            || diagnostic.contains("re-login") {
            return "credentials"
        }
        if diagnostic.contains("sqlite")
            || diagnostic.contains("database")
            || diagnostic.contains("postgres")
            || diagnostic.contains("catalogue")
            || diagnostic.contains("sqlx") {
            return "sqlite_database"
        }
        return "generic"
    }

    static func isAllVehiclesUnsupported(_ error: Error) -> Bool {
        let message = error.localizedDescription.lowercased()
        return message.contains("--all-vehicles")
            && (message.contains("unexpected argument")
                || message.contains("unknown option")
                || message.contains("unrecognized option"))
    }

    func startHub(completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        // S is acquired before the history-only policy check: the check is part of Start.
        guard let op = admit(.start, phase: "policy", completion: completion) else { return }
        guard !hasPendingMigrationHandover else {
            HubAppLog.shared.record("service.rejected", category: "service", level: "WARN",
                                    fields: ["action": "start", "reason": "handover_pending"])
            op.finish(.failure(HubActionError.commandFailed(
                HubL10n.text("hub.HubController.2457.838", fallback: "Finish the TeslaMate handover before starting Hub.")
            )), completion: completion)
            return
        }
        verifyHistoryOnlyStartPolicy(op) { [self] result in
            switch result {
            case .success:
                runServiceCommand(op, ["service", "start"]) { op.finish($0, completion: completion) }
            case let .failure(error):
                op.finish(.failure(error), completion: completion)
            }
        }
    }

    func stopHub(completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        guard let op = admit(.stop, phase: "stop", completion: completion) else { return }
        runServiceCommand(op, ["service", "stop"]) { op.finish($0, completion: completion) }
    }

    func restartHub(completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        guard let op = admit(.restart, phase: "policy", completion: completion) else { return }
        guard !hasPendingMigrationHandover else {
            HubAppLog.shared.record("service.rejected", category: "service", level: "WARN",
                                    fields: ["action": "restart", "reason": "handover_pending"])
            op.finish(.failure(HubActionError.commandFailed(
                HubL10n.text("hub.HubController.2479.850", fallback: "Finish the TeslaMate handover before restarting Hub.")
            )), completion: completion)
            return
        }
        verifyHistoryOnlyStartPolicy(op) { [self] result in
            switch result {
            case .success:
                runServiceCommand(op, ["service", "restart"]) { op.finish($0, completion: completion) }
            case let .failure(error):
                op.finish(.failure(error), completion: completion)
            }
        }
    }

    func acknowledgeMigrationHandoverAndStart(completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else {
            HubAppLog.shared.record("handover_start.rejected", category: "teslamate_import",
                                    level: "WARN", fields: ["reason": "preview_read_only"])
            completion(.failure(HubActionError.preview))
            return
        }
        // H is acquired before the configuration changes or the readiness baseline is read.
        guard let op = admit(.handover, phase: "validate", refused: { refusal in
            HubAppLog.shared.record("handover_start.rejected", category: "teslamate_import",
                                    level: "WARN", fields: ["reason": "already_in_progress"])
            if case .busy(.handover) = refusal {
                completion(.failure(HubActionError.commandFailed(
                    HubL10n.text("hub.HubController.2503.863", fallback: "Hub startup is already in progress.")
                )))
            } else {
                completion(.failure(HubActionError.lifecycleRefused(refusal)))
            }
        }) else { return }
        // Released only when the Start, the readiness wait and any cleanup or rollback
        // have settled; the interface hears of the outcome afterwards, on the main queue.
        let finish: (Result<Void, Error>) -> Void = { result in
            op.settle { DispatchQueue.main.async { completion(result) } }
        }
        guard let handover = migrationHandoverState,
              handover.phase == .awaitingHandover else {
            HubAppLog.shared.record("handover_start.rejected", category: "teslamate_import",
                                    level: "WARN", fields: ["reason": "checks_incomplete"])
            finish(.failure(HubActionError.commandFailed(
                HubL10n.text("hub.HubController.2519.869", fallback: "Finish the migration checks before starting Hub.")
            )))
            return
        }
        let started = Date()
        HubAppLog.shared.record("handover_start.requested", category: "teslamate_import")
        if handover.historyOnly == true {
            op.phase("policy")
            verifyHistoryOnlyStartPolicy(op) { [self] policyResult in
                switch policyResult {
                case let .failure(error): finish(.failure(error))
                case .success:
                    op.phase("start")
                    op.service(["service", "start"]) { [self] result in
                        switch result {
                        case .success:
                            completeMigrationHandoverStart(op, handover: handover,
                                                           started: started, completion: finish)
                        case let .failure(error):
                            op.service(["service", "stop"]) { _ in
                                finish(.failure(error))
                            }
                        }
                    }
                }
            }
            return
        }
        op.phase("baseline")
        captureMigrationCollectorLeaseBaseline(op) { [self] baseline in
            do {
                try ensureConfig(provider: handover.previousProvider,
                                 collectorIntervalSeconds: handover.previousIntervalSeconds)
            } catch {
                HubAppLog.shared.record("handover_start.failed", category: "teslamate_import",
                                        level: "ERROR", fields: [
                                        "error_code": HubAppLog.errorCode(error),
                                        "reason": "config_restore_failed"
                                    ])
                finish(.failure(error))
                return
            }
            let startRequestedAtMs = migrationStartupReadinessNowMilliseconds()
            let readinessDeadlineAtMs = startRequestedAtMs + Int64(
                migrationStartupReadinessTimeout * 1_000
            )
            op.phase("start")
            op.service(["service", "start"]) { [self] result in
                switch result {
                case .success:
                    op.phase("readiness")
                    waitForMigrationStartupReadiness(
                        op,
                        baseline: baseline,
                        startRequestedAtMs: startRequestedAtMs,
                        deadlineAtMs: readinessDeadlineAtMs,
                        collectorRequired: handover.previousIntervalSeconds > 0,
                        attemptsRemaining: migrationStartupReadinessMaxAttempts
                    ) { [self] readinessResult in
                        switch readinessResult {
                        case .success:
                            completeMigrationHandoverStart(
                                op,
                                handover: handover,
                                started: started,
                                completion: finish
                            )
                        case let .failure(readinessError):
                            rollbackUnreadyMigrationHandoverStart(
                                op,
                                handover: handover,
                                readinessError: readinessError,
                                completion: finish
                            )
                        }
                    }
                case let .failure(startError):
                    HubAppLog.shared.record("handover_start.failed", category: "teslamate_import",
                                            level: "ERROR", fields: [
                                                "error_code": HubAppLog.errorCode(startError),
                                                "reason": "service_start_failed"
                                            ])
                    do {
                        try ensureConfig(provider: handover.previousProvider,
                                         collectorIntervalSeconds: 0)
                        try writeMigrationHandoverMarker(handover)
                        finish(.failure(startError))
                    } catch {
                        HubAppLog.shared.record("handover_recovery.failed",
                                                category: "teslamate_import", level: "ERROR",
                                                fields: [
                                                    "error_code": HubAppLog.errorCode(error),
                                                    "reason": "start_failure_rollback"
                                                ])
                        finish(.failure(HubActionError.commandFailed(
                            HubL10n.format("hub.HubController.2610.896", fallback: "Hub did not start: %1$@. The collector pause could not be restored: %2$@", arguments: [String(describing: startError.localizedDescription), String(describing: error.localizedDescription)])
                        )))
                    }
                }
            }
        }
    }



    private func captureMigrationCollectorLeaseBaseline(
        _ op: HubLifecycleOperation,
        completion: @escaping (HubCollectorLeaseBaseline) -> Void
    ) {
        op.run(["--config", configPath.path, "status"]) { result in
            guard case let .success(output) = result,
                  let data = output.data(using: .utf8),
                  let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  root.keys.contains("collector"),
                  let document = try? JSONDecoder().decode(HubStartupStatusDocument.self,
                                                            from: data),
                  document.status == "ok" else {
                completion(.unavailable)
                return
            }
            guard !(root["collector"] is NSNull) else {
                completion(.observed(nil))
                return
            }
            guard let collector = document.collector, collector.isCoherent else {
                completion(.unavailable)
                return
            }
            completion(.observed(collector.instanceId))
        }
    }

    private func waitForMigrationStartupReadiness(
        _ op: HubLifecycleOperation,
        baseline: HubCollectorLeaseBaseline,
        startRequestedAtMs: Int64,
        deadlineAtMs: Int64,
        collectorRequired: Bool,
        attemptsRemaining: Int,
        completion: @escaping (Result<Void, Error>) -> Void
    ) {
        guard migrationStartupReadinessNowMilliseconds() < deadlineAtMs else {
            completion(.failure(HubActionError.commandFailed(
                HubL10n.text("hub.HubController.2678.902", fallback: "Hub did not become ready before the startup deadline.")
            )))
            return
        }
        op.run(["--config", configPath.path, "status"]) { [self] result in
            let document: HubStartupStatusDocument
            switch result {
            case .failure:
                completion(.failure(HubActionError.commandFailed(
                    HubL10n.text("hub.HubController.2689.905", fallback: "Hub readiness status could not be read.")
                )))
                return
            case let .success(output):
                guard let data = output.data(using: .utf8),
                      let parsed = try? JSONDecoder().decode(HubStartupStatusDocument.self,
                                                             from: data),
                      parsed.status == "ok" else {
                    completion(.failure(HubActionError.commandFailed(
                        HubL10n.text("hub.HubController.2698.907", fallback: "Hub readiness status was invalid.")
                    )))
                    return
                }
                document = parsed
            }
            if document.ready,
               isFreshMigrationStartupStatus(
                document,
                baseline: baseline,
                startRequestedAtMs: startRequestedAtMs,
                collectorRequired: collectorRequired
               ) {
                completion(.success(()))
                return
            }
            if document.ready, collectorRequired,
               (document.collector == nil || document.collector?.isCoherent != true) {
                completion(.failure(HubActionError.commandFailed(
                    HubL10n.text("hub.HubController.2717.908", fallback: "Hub readiness status did not include a valid collector lease.")
                )))
                return
            }
            guard attemptsRemaining > 1,
                  migrationStartupReadinessNowMilliseconds() < deadlineAtMs else {
                completion(.failure(HubActionError.commandFailed(
                    HubL10n.text("hub.HubController.2678.902", fallback: "Hub did not become ready before the startup deadline.")
                )))
                return
            }
            migrationStartupReadinessSchedule(migrationStartupReadinessPollInterval) { [self] in
                // A callback after the owner has settled launches nothing.
                guard op.isLive else { return }
                waitForMigrationStartupReadiness(
                    op,
                    baseline: baseline,
                    startRequestedAtMs: startRequestedAtMs,
                    deadlineAtMs: deadlineAtMs,
                    collectorRequired: collectorRequired,
                    attemptsRemaining: attemptsRemaining - 1,
                    completion: completion
                )
            }
        }
    }

    private func migrationStartupReadinessNowMilliseconds() -> Int64 {
        Int64(migrationStartupReadinessNow().timeIntervalSince1970 * 1_000)
    }

    private func isFreshMigrationStartupStatus(
        _ document: HubStartupStatusDocument,
        baseline: HubCollectorLeaseBaseline,
        startRequestedAtMs: Int64,
        collectorRequired: Bool
    ) -> Bool {
        guard collectorRequired else { return true }
        guard let collector = document.collector, collector.isCoherent else { return false }
        switch baseline {
        case let .observed(previousInstance):
            return previousInstance != collector.instanceId
        case .unavailable:
            return collector.startedAtMs >= startRequestedAtMs
        }
    }

    private func completeMigrationHandoverStart(
        _ op: HubLifecycleOperation,
        handover: HubMigrationHandoverState,
        started: Date,
        completion: @escaping (Result<Void, Error>) -> Void
    ) {
        op.phase("marker")
        do {
            try FileManager.default.removeItem(at: migrationHandoverMarker)
            HubAppLog.shared.record("handover_start.completed",
                                    category: "teslamate_import", fields: [
                                        "duration_ms": String(Int(Date().timeIntervalSince(started) * 1000))
                                    ])
            completion(.success(()))
        } catch {
            let cleanupError = error
            HubAppLog.shared.record("handover_start.failed",
                                    category: "teslamate_import", level: "ERROR",
                                    fields: [
                                        "error_code": HubAppLog.errorCode(error),
                                        "reason": "handover_gate_cleanup_failed"
                                    ])
            op.phase("rollback")
            op.service(["service", "stop"]) { [self] _ in
                do {
                    if handover.historyOnly != true {
                        try ensureConfig(provider: handover.previousProvider,
                                         collectorIntervalSeconds: 0)
                    }
                    try writeMigrationHandoverMarker(handover)
                } catch {
                    HubAppLog.shared.record("handover_recovery.failed",
                                            category: "teslamate_import", level: "ERROR",
                                            fields: [
                                                "error_code": HubAppLog.errorCode(error),
                                                "reason": "cleanup_failure_rollback"
                                            ])
                }
                completion(.failure(HubActionError.commandFailed(
                    HubL10n.format("hub.HubController.2798.927", fallback: "Hub was stopped because the migration handover gate could not be cleared: %1$@", arguments: [String(describing: cleanupError.localizedDescription)])
                )))
            }
        }
    }

    private func rollbackUnreadyMigrationHandoverStart(
        _ op: HubLifecycleOperation,
        handover: HubMigrationHandoverState,
        readinessError: Error,
        completion: @escaping (Result<Void, Error>) -> Void
    ) {
        HubAppLog.shared.record("handover_start.failed", category: "teslamate_import",
                                level: "ERROR", fields: [
                                    "error_code": HubAppLog.errorCode(readinessError),
                                    "reason": "startup_readiness_failed"
                                ])
        op.phase("rollback")
        op.service(["service", "stop"]) { [self] stopResult in
            var pauseRestored = true
            do {
                try ensureConfig(provider: handover.previousProvider,
                                 collectorIntervalSeconds: 0)
                try writeMigrationHandoverMarker(handover)
            } catch {
                pauseRestored = false
                HubAppLog.shared.record("handover_recovery.failed",
                                        category: "teslamate_import", level: "ERROR",
                                        fields: [
                                            "error_code": HubAppLog.errorCode(error),
                                            "reason": "readiness_failure_rollback"
                                        ])
            }
            let message: String
            switch (stopResult, pauseRestored) {
            case (.success, true):
                message = HubL10n.text("hub.HubController.2832.942", fallback: "Hub did not become ready and was stopped. The migration handover remains pending.")
            case (.success, false):
                message = HubL10n.text("hub.HubController.2834.943", fallback: "Hub was stopped after startup failed, but the collector pause could not be restored. Keep TeslaMate disabled and try again.")
            case (.failure, _):
                message = HubL10n.text("hub.HubController.2836.944", fallback: "Hub did not become ready and could not be confirmed stopped. Keep TeslaMate disabled, stop Hub, and try again.")
            }
            completion(.failure(HubActionError.commandFailed(message)))
        }
    }

    func runOnboardingChecks(expectRunning: Bool,
                             completion: @escaping (Result<[HubOnboardingCheck], Error>) -> Void) {
        HubAppLog.shared.record("verification.started", category: "onboarding",
                                fields: ["expect_running": expectRunning ? "true" : "false"])
        if previewMode {
            DispatchQueue.main.async { completion(.success(Self.previewOnboardingChecks)) }
            return
        }
        // V is acquired for each active verification transaction (its Stop, checks and
        // marker advancement), automatic or a retry.
        guard let op = admit(.verification, phase: "verify", completion: completion) else { return }
        let finish: (Result<[HubOnboardingCheck], Error>) -> Void = { result in
            op.finish(result) { outcome in DispatchQueue.main.async { completion(outcome) } }
        }
        if !expectRunning, migrationHandoverState?.historyOnly == true {
            op.phase("stop")
            op.service(["service", "stop"]) { [self] result in
                switch result {
                case .success:
                    performOnboardingChecks(op, expectRunning: false, completion: finish)
                case let .failure(error):
                    finish(.failure(error))
                }
            }
            return
        }
        if !expectRunning, migrationHandoverState != nil {
            op.phase("stop")
            op.service(["service", "stop"]) { [self] stopResult in
                switch stopResult {
                case .success:
                    performOnboardingChecks(op, expectRunning: false, completion: finish)
                case let .failure(error):
                    finish(.failure(error))
                }
            }
            return
        }
        performOnboardingChecks(op, expectRunning: expectRunning, completion: finish)
    }

    private func performOnboardingChecks(_ op: HubLifecycleOperation,
                                         expectRunning: Bool,
                                         completion: @escaping (Result<[HubOnboardingCheck], Error>) -> Void) {
        op.phase("checks")
        if !expectRunning, migrationHandoverState?.historyOnly == true {
            op.run(["--config", configPath.path, "preflight", "--history-only"]) { [self] result in
                let ready: Bool
                switch result {
                case let .success(output):
                    ready = Self.historyOnlyPreflightReady(output)
                case .failure:
                    ready = false
                }
                let checks = [
                    HubOnboardingCheck(title: HubL10n.text("hub.HubController.2905.957", fallback: "Hub"), detail: HubL10n.text("hub.HubController.2905.958", fallback: "Stopped for history verification"), passed: true),
                    HubOnboardingCheck(title: HubL10n.text("hub.HubController.2906.959", fallback: "Signed history"), detail: ready ? HubL10n.text("hub.HubController.2906.960", fallback: "Published and ready") : HubL10n.text("hub.HubController.2906.961", fallback: "Not ready"), passed: ready),
                    HubOnboardingCheck(title: HubL10n.text("hub.HubController.2907.962", fallback: "Collection"), detail: ready ? HubL10n.text("hub.HubController.2907.963", fallback: "Disabled") : HubL10n.text("hub.HubController.2907.964", fallback: "Check the local Hub configuration"), passed: ready),
                    HubOnboardingCheck(title: HubL10n.text("hub.HubController.1162.479", fallback: "Tesla account"), detail: HubL10n.text("hub.HubController.2908.966", fallback: "Not connected; history is available offline"), passed: true)
                ]
                if ready, var handover = migrationHandoverState {
                    handover.phase = .awaitingHandover
                    do { try writeMigrationHandoverMarker(handover) }
                    catch {
                        completion(.failure(error))
                        return
                    }
                }
                completion(.success(checks))
            }
            return
        }
        let finish: (Result<String, Error>) -> Void = { [self] doctorResult in
            logs { [self] logText in
                // Read readiness last, so the wizard and dashboard use the same current state.
                refresh { [self] snapshot in
                    let servicePassed = expectRunning
                        ? snapshot.health == .running
                        : snapshot.health == .stopped
                    let serviceDetail = expectRunning
                        ? snapshot.service
                        : (servicePassed ? HubL10n.text("hub.HubController.2932.967", fallback: "Installed and safely stopped") : snapshot.service)
                    let vehiclePassed = snapshot.hasConfiguredVehicle
                    let doctorPassed: Bool
                    let doctorDetail: String
                    switch doctorResult {
                    case .success:
                        doctorPassed = !expectRunning || servicePassed
                        doctorDetail = expectRunning
                            ? (servicePassed ? HubL10n.text("hub.HubController.2943.972", fallback: "Live readiness passed; full integrity check available in Diagnostics")
                               : HubL10n.text("hub.HubController.2944.973", fallback: "Hub is not ready yet. Wait a moment, then run again."))
                            : HubL10n.text("hub.OnboardingWindowController.1165.2025", fallback: "Passed")
                    case let .failure(error):
                        doctorPassed = false
                        doctorDetail = Self.conciseDiagnostic(error.localizedDescription)
                    }
                    let noLogsYet = logText.hasPrefix("No Hub logs are available yet.")
                    let logsPassed = expectRunning ? !noLogsYet && !logText.isEmpty : true
                    let logsDetail = noLogsYet
                        ? (expectRunning ? HubL10n.text("hub.HubController.2953.976", fallback: "No logs yet") : HubL10n.text("hub.HubController.2953.977", fallback: "No service logs while stopped"))
                        : (logText.isEmpty ? HubL10n.text("hub.HubController.2954.978", fallback: "Unavailable") : HubL10n.text("hub.HubController.2954.979", fallback: "Readable"))
                    let checks = [
                        HubOnboardingCheck(title: HubL10n.text("hub.HubController.1161.477", fallback: "Service"), detail: serviceDetail, passed: servicePassed),
                        HubOnboardingCheck(title: HubL10n.text("hub.HubController.1162.479", fallback: "Tesla account"), detail: snapshot.account,
                                           passed: snapshot.accountState == .connected),
                        HubOnboardingCheck(title: HubL10n.text("hub.HubController.1163.481", fallback: "Vehicle"), detail: snapshot.vehicleName,
                                           passed: vehiclePassed),
                        HubOnboardingCheck(title: HubL10n.text("hub.DiagnosticsWindowController.374.248", fallback: "Database"), detail: snapshot.database,
                                           passed: snapshot.databaseState == .healthy),
                        HubOnboardingCheck(title: HubL10n.text("hub.AppDelegate.123.39", fallback: "Diagnostics"), detail: doctorDetail,
                                           passed: doctorPassed),
                        HubOnboardingCheck(title: HubL10n.text("hub.HubController.1166.487", fallback: "Logs"),
                                           detail: logsDetail,
                                           passed: logsPassed)
                    ]
                    guard !expectRunning,
                          checks.allSatisfy(\.passed),
                          var handover = migrationHandoverState else {
                        HubAppLog.shared.record(
                            "verification.completed",
                            category: "onboarding",
                            fields: [
                                "passed": checks.allSatisfy(\.passed) ? "true" : "false",
                                "failed_checks": checks.filter { !$0.passed }.map(\.title).joined(separator: ",")
                            ]
                        )
                        completion(.success(checks))
                        return
                    }
                    handover.phase = .awaitingHandover
                    do {
                        try writeMigrationHandoverMarker(handover)
                        HubAppLog.shared.record("verification.completed", category: "onboarding",
                                                fields: ["handover": "awaiting_user", "passed": "true"])
                        completion(.success(checks))
                    } catch {
                        HubAppLog.shared.record("verification.failed", category: "onboarding",
                                                level: "ERROR",
                                                fields: ["error_code": HubAppLog.errorCode(error)])
                        completion(.failure(error))
                    }
                }
            }
        }
        if expectRunning {
            // Immutable doctor requires a stopped writer. Status opens the live database
            // read-only and checks readiness without pausing collection or calling Tesla.
            finish(.success(""))
        } else {
            op.run(["--config", configPath.path, "doctor"], completion: finish)
        }
    }

    private static func conciseDiagnostic(_ message: String) -> String {
        let line = message.split(whereSeparator: { $0.isNewline }).first.map(String.init)
            ?? HubL10n.text("hub.HubController.3010.1006", fallback: "Failed")
        return String(line.prefix(160))
    }

    func performVehicleControl(_ action: HubVehicleControl,
                               vehicleID requestedVehicleID: UUID? = nil,
                               completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else {
            HubAppLog.shared.record("command.rejected", category: "vehicle_control", level: "WARN",
                                    fields: ["action": action.rawValue, "reason": "preview_read_only"])
            completion(.failure(HubActionError.preview))
            return
        }
        // C is held from the accepted-snapshot validation through the known or unknown
        // outcome. Ownership is not authority to add or send any other command.
        guard let op = admit(.vehicleControl, phase: "validate", completion: completion) else { return }
        let reject: (String, String) -> Void = { reason, message in
            HubAppLog.shared.record("command.rejected", category: "vehicle_control", level: "WARN",
                                    fields: ["action": action.rawValue, "reason": reason])
            op.finish(.failure(HubActionError.commandFailed(message)), completion: completion)
        }
        guard snapshot.health == .running else {
            reject("service_not_running", HubL10n.text("hub.HubController.3026.1019", fallback: "Hub must be running before sending a vehicle command."))
            return
        }
        guard snapshot.accountState == .connected else {
            reject("account_disconnected", HubL10n.text("hub.HubController.3032.1027", fallback: "Connect Tesla before sending a vehicle command."))
            return
        }
        guard snapshot.provider == .fleet else {
            reject("provider_does_not_support_commands", HubL10n.text("hub.HubController.3040.1034", fallback: "Vehicle controls require Tesla Fleet API."))
            return
        }
        guard let vehicleID = requestedVehicleID ?? snapshot.controlVehicleID else {
            reject("vehicle_not_selected", HubL10n.text("hub.HubController.3047.1041", fallback: "Choose a vehicle before sending a command."))
            return
        }
        guard snapshot.controlVehicles.contains(where: { $0.id == vehicleID }) else {
            reject("vehicle_stale", HubL10n.text("hub.HubController.3053.1048", fallback: "The selected vehicle is no longer configured."))
            return
        }
        let started = Date()
        HubAppLog.shared.record("command.requested", category: "vehicle_control",
                                fields: ["action": action.rawValue])
        let arguments = ["--config", configPath.path, "control", "--vehicle-id",
                         vehicleID.uuidString.lowercased(), action.rawValue, "--confirm"]
        op.phase("control")
        op.run(arguments) { result in
            DispatchQueue.main.async {
                switch result {
                case .success:
                    HubAppLog.shared.record("command.accepted", category: "vehicle_control",
                                            fields: [
                                                "action": action.rawValue,
                                                "duration_ms": String(Int(Date().timeIntervalSince(started) * 1000))
                                            ])
                    op.finish(.success(()), completion: completion)
                case let .failure(error):
                    HubAppLog.shared.record("command.failed", category: "vehicle_control", level: "ERROR",
                                            fields: [
                                                "action": action.rawValue,
                                                "duration_ms": String(Int(Date().timeIntervalSince(started) * 1000)),
                                                "error_code": HubAppLog.errorCode(error)
                                            ])
                    op.finish(.failure(error), completion: completion)
                }
            }
        }
    }

    func logs(maximumBytes: Int = 128 * 1024, completion: @escaping (String) -> Void) {
        if previewMode {
            completion("Preview mode\n\n[INFO] Teslatlas Hub is running in the background.\n[INFO] Vehicle went offline\n[INFO] Position stored\n")
            return
        }
        DispatchQueue.global(qos: .utility).async {
            let folder = self.configuredLogDirectory
                ?? self.homeDirectory.appendingPathComponent("Library/Logs/Teslatlas Hub", isDirectory: true)
            let files = [
                ("hub.out.log", folder.appendingPathComponent("hub.out.log")),
                ("hub.err.log", folder.appendingPathComponent("hub.err.log"))
            ]
            let contents = files.compactMap { name, url in
                Self.logTail(of: url, maximumBytes: maximumBytes).map { tail in
                    let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
                    let modified = attributes?[.modificationDate] as? Date
                    let timestamp = modified.map { ISO8601DateFormatter().string(from: $0) } ?? "unknown"
                    return "File last written: \(timestamp). This file may contain historical errors, not current service status.\n== \(name) ==\n\(tail)"
                }
            }
            let text = contents.isEmpty ? "No Hub logs are available yet.\n" : contents.joined(separator: "\n")
            DispatchQueue.main.async { completion(text) }
        }
    }

    func diagnostics() -> [String] {
        snapshot.diagnosticLines + (serviceDiagnostic.map { [Self.serviceDiagnosticLine($0)] } ?? [])
    }

    static func serviceDiagnosticLine(_ message: String, bundle: Bundle = .main) -> String {
        HubL10n.format("hub.service.diagnostic_line", fallback: "Service: %1$@",
                       arguments: [message], bundle: bundle)
    }

    func runFullDiagnostics(completion: @escaping (String) -> Void) {
        if previewMode {
            completion("Preview mode\n\n" + supportMetadata() + "\n\n"
                + snapshot.diagnosticLines.joined(separator: "\n"))
            return
        }
        // G is acquired before the initial loaded-state query and held through the
        // doctor, preflight, status and log work, the policy check, the ordinary resume or
        // recovery, the duty release and the final report, whichever surface asked.
        guard let op = admit(.diagnostics, phase: "state", refused: { refusal in
            switch refusal {
            case .deleting, .deleted, .busy(.delete):
                completion(HubL10n.text("hub.diagnostics.delete_in_progress",
                                        fallback: "Diagnostics were not run because Delete Hub and Data is in progress."))
            default:
                completion(HubL10n.text("hub.diagnostics.not_run_busy",
                                        fallback: "Diagnostics were not run because another Hub operation is in progress. Nothing was changed."))
            }
        }) else { return }
        // F8-6: a recovery finishes on the service controller's queue, and so can the release of a held child;
        // the report goes to window code, which is main-queue only.
        let finish: (String) -> Void = { report in
            op.settle {
                if Thread.isMainThread { completion(report) } else { DispatchQueue.main.async { completion(report) } }
            }
        }
        // The pre-pause query only decides whether to pause; the controller holds the
        // record that decides the restart (RD1).
        op.loadedState { [self] state in
            switch state {
            case .loaded:
                op.phase("pause")
                op.service(["service", "pause"]) { [self] pauseResult in
                    switch pauseResult {
                    case .success:
                        op.phase("checks")
                        runFullDiagnosticsWhileStopped(op) { [self] report in
                            op.phase("policy")
                            verifyHistoryOnlyStartPolicy(op) { [self] policyResult in
                                switch policyResult {
                                case .success:
                                    op.phase("resume")
                                    op.serviceAfterOtherChildren(["service", "start"]) { [self] startResult in
                                        switch startResult {
                                        case .success:
                                            finish(report + "\n\n" + Self.serviceResumeSection(startResult))
                                        case let .failure(startError):
                                            // The recovery's own result is reported too, and a restart
                                            // that could not be confirmed is judged by refreshed status.
                                            recoverThenFail(op, startError) { outcome in
                                                finish(report + "\n\n"
                                                       + Self.serviceResumeSection(outcome.map { _ in "" }))
                                            }
                                        }
                                    }
                                case let .failure(error):
                                    // History-only start policy refused: the Hub is deliberately
                                    // left stopped, so no start is owed any more.
                                    op.releaseRestartRecord()
                                    finish(report + "\n\n" + Self.serviceResumeSection(.failure(error)))
                                }
                            }
                        }
                    case let .failure(error):
                        op.phase("checks")
                        runFullDiagnosticsWhileStopped(op) { [self] report in
                            op.phase("recovery")
                            op.recoverAfterPause { [self] recovery in
                                serviceRecoverySection(recovery) { section in
                                    finish(Self.servicePauseFailureSection(error) + "\n\n" + report + section)
                                }
                            }
                        }
                    }
                }
            case .unloaded:
                op.phase("checks")
                runFullDiagnosticsWhileStopped(op, completion: finish)
            case let .unknown(error):
                op.phase("checks")
                runFullDiagnosticsWhileStopped(op) { report in
                    finish(Self.serviceStateFailureSection(error) + "\n\n" + report)
                }
            }
        }
    }

    /// The recovery section of a failed pause. A recovery that could not be confirmed
    /// refreshes status and says the Hub did not restart only when status shows it stopped.
    private func serviceRecoverySection(_ result: Result<String, Error>,
                                        completion: @escaping (String) -> Void) {
        switch result {
        case let .success(note):
            completion(note.isEmpty ? "" : "\n\n== service recovery ==\n\(note)")
        case let .failure(error):
            guard (error as? HubServiceRestartReport)?.isUnconfirmed == true else {
                completion("\n\n== service recovery (failed) ==\n\(error.localizedDescription)")
                return
            }
            unconfirmedRestartText(after: nil) { text in
                completion("\n\n== service recovery (failed) ==\n\(text)")
            }
        }
    }

    private static func servicePauseFailureSection(_ error: Error) -> String {
        "== service pause (failed) ==\n\(error.localizedDescription)"
    }

    private static func serviceStateFailureSection(_ error: Error) -> String {
        "== service state check (failed) ==\n\(error.localizedDescription)"
    }

    private static func serviceResumeSection(_ result: Result<String, Error>) -> String {
        switch result {
        case .success:
            return "== service resume ==\nHub collection resumed."
        case let .failure(error):
            return "== service resume (failed) ==\n\(error.localizedDescription)"
        }
    }

    private func runFullDiagnosticsWhileStopped(_ op: HubLifecycleOperation,
                                                completion: @escaping (String) -> Void) {
        let diagnosticsStarted = Date()
        HubAppLog.shared.record("checks.started", category: "diagnostics")
        let config = ["--config", configPath.path]
        func section(_ title: String,
                     _ result: Result<String, Error>,
                     durationMilliseconds: Int) -> String {
            switch result {
            case let .success(output):
                return "== \(title) ==\nDuration: \(durationMilliseconds) ms\n"
                    + "\(output.trimmingCharacters(in: .whitespacesAndNewlines))\n"
            case let .failure(error):
                return "== \(title) (failed) ==\nDuration: \(durationMilliseconds) ms\n"
                    + "\(error.localizedDescription)\n"
            }
        }
        let doctorStarted = Date()
        op.run(config + ["doctor"]) { [self] doctor in
            let doctorMilliseconds = Int(Date().timeIntervalSince(doctorStarted) * 1000)
            let preflightStarted = Date()
            op.run(config + ["preflight"]) { [self] preflight in
                let preflightMilliseconds = Int(Date().timeIntervalSince(preflightStarted) * 1000)
                let statusStarted = Date()
                op.run(config + ["status"]) { [self] status in
                    let statusMilliseconds = Int(Date().timeIntervalSince(statusStarted) * 1000)
                    let logsStarted = Date()
                    logs(maximumBytes: 512 * 1024) { [self] logText in
                        let logsMilliseconds = Int(Date().timeIntervalSince(logsStarted) * 1000)
                        let failedSections = [doctor, preflight, status].reduce(into: 0) { count, result in
                            if case .failure = result { count += 1 }
                        }
                        let report = [
                            "Teslatlas Hub diagnostics",
                            "TeslaMate is not written. Stored Owner and Fleet tokens are not deleted.",
                            "",
                            supportMetadata(),
                            "",
                            section("doctor — Hub database, tokens, TLS, collector", doctor,
                                    durationMilliseconds: doctorMilliseconds),
                            section("preflight — selected provider credentials", preflight,
                                    durationMilliseconds: preflightMilliseconds),
                            section("status — vehicles and credential presence", status,
                                    durationMilliseconds: statusMilliseconds),
                            "== recent logs ==",
                            "Read duration: \(logsMilliseconds) ms",
                            logText.trimmingCharacters(in: .whitespacesAndNewlines)
                        ].joined(separator: "\n")
                        HubAppLog.shared.record(
                            "checks.completed",
                            category: "diagnostics",
                            level: failedSections == 0 ? "INFO" : "WARN",
                            fields: [
                                "duration_ms": String(Int(Date().timeIntervalSince(diagnosticsStarted) * 1000)),
                                "failed_sections": String(failedSections),
                                "doctor_ms": String(doctorMilliseconds),
                                "logs_ms": String(logsMilliseconds),
                                "preflight_ms": String(preflightMilliseconds),
                                "status_ms": String(statusMilliseconds)
                            ]
                        )
                        completion(report)
                    }
                }
            }
        }
    }

    private func supportMetadata() -> String {
        let bundle = Bundle.main
        let appVersion = bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
            ?? "development"
        let appBuild = bundle.object(forInfoDictionaryKey: "CFBundleVersion") as? String
            ?? "development"
        let provider = snapshot.provider?.displayName ?? "Not configured"
        let serviceState: String
        switch snapshot.health {
        case .running: serviceState = "running"
        case .stopped: serviceState = "stopped"
        case .needsInstall: serviceState = "not installed"
        case .degraded: serviceState = "needs attention"
        }
#if arch(arm64)
        let architecture = "arm64"
#elseif arch(x86_64)
        let architecture = "x86_64"
#else
        let architecture = "unknown"
#endif
        let availableStorage: String
        if let bytes = try? homeDirectory.resourceValues(
            forKeys: [.volumeAvailableCapacityForImportantUsageKey]
        ).volumeAvailableCapacityForImportantUsage {
            availableStorage = ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
        } else {
            availableStorage = "Unavailable"
        }
        var metadata = [
            "== support metadata ==",
            "Generated: \(ISO8601DateFormatter().string(from: Date()))",
            "App: \(appVersion) (\(appBuild))",
            "Expected Hub: \(HubRelease.bundledVersion)",
            "Observed Hub: \(snapshot.version)",
            "Service: \(serviceState)",
            "Provider: \(provider)",
            "macOS: \(ProcessInfo.processInfo.operatingSystemVersionString)",
            "Architecture: \(architecture)",
            "Available storage: \(availableStorage)"
        ]
        if let developmentConfiguration {
            metadata.insert("Runtime: explicit local development LaunchAgent", at: 5)
            metadata.insert("Mode: \(developmentConfiguration.mode.rawValue)", at: 6)
            metadata.insert("Binary: \(developmentConfiguration.binary.path)", at: 7)
            metadata.insert("Configuration: \(developmentConfiguration.config.path)", at: 8)
            metadata.insert("State: \(developmentConfiguration.stateDirectory.path)", at: 9)
            metadata.insert("Logs: \(developmentConfiguration.logDirectory.path)", at: 10)
        }
        return metadata.joined(separator: "\n")
    }

    func showDataFolder() {
        guard !previewMode, let dataDirectory = snapshot.dataDirectory else { return }
        NSWorkspace.shared.open(dataDirectory)
    }

    private static func historyOnlyPreflightReady(_ output: String) -> Bool {
        guard let data = output.data(using: .utf8),
              let report = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return false
        }
        return report["status"] as? String == "ready"
            && report["credentialScope"] as? String == "history-only"
            && report["collectorEnabled"] as? Bool == false
            && (report["publishedVehicles"] as? Int ?? 0) > 0
    }

    private func verifyHistoryOnlyStartPolicy(_ op: HubLifecycleOperation,
                                              completion: @escaping (Result<Void, Error>) -> Void) {
        let selected: Bool
        do {
            selected = try HistoryOnlyControl.isSelected(for: configPath)
                || migrationHandoverState?.historyOnly == true
        } catch {
            completion(.failure(error))
            return
        }
        guard selected else { completion(.success(())); return }
        op.run(["--config", configPath.path, "preflight", "--history-only"]) { result in
            DispatchQueue.main.async {
                guard case let .success(output) = result,
                      Self.historyOnlyPreflightReady(output) else {
                    completion(.failure(HubActionError.commandFailed(
                        HubL10n.text("hub.HubController.3323.1158", fallback: "History-only Hub configuration or published history is not ready. Keep collection disabled and run verification again.")
                    )))
                    return
                }
                completion(.success(()))
            }
        }
    }

    /// One service action as a child of the owner: it neither admits nor releases.
    private func runServiceCommand(_ op: HubLifecycleOperation, _ arguments: [String],
                                   completion: @escaping (Result<Void, Error>) -> Void) {
        let action = arguments.last ?? "unknown"
        HubAppLog.shared.record("service.requested", category: "service",
                                fields: ["action": action])
        op.service(arguments) { [self] result in
            DispatchQueue.main.async {
                switch result {
                case .success:
                    HubAppLog.shared.record("service.completed", category: "service",
                                            fields: ["action": action])
                    completion(.success(()))
                case let .failure(error):
                    HubAppLog.shared.record("service.failed", category: "service", level: "ERROR",
                                            fields: [
                                                "action": action,
                                                "error_code": HubAppLog.errorCode(error)
                                            ])
                    self.reportServiceFailure(error) { completion(.failure($0)) }
                }
            }
        }
    }

    // MARK: Lifecycle admission (R-E1F6-1)

    private func makeOperation(_ token: HubLifecycleToken) -> HubLifecycleOperation {
        HubLifecycleOperation(token: token, authority: lifecycle,
                              commandRunner: commandRunner, serviceRunner: serviceRunner)
    }

    static func refusalCode(_ refusal: HubLifecycleRefusal) -> String {
        switch refusal {
        case let .busy(family): return "busy_" + family.letter
        case .deleting: return "deleting"
        case .deleted: return "deleted"
        case .closing: return "closing"
        case .closed: return "closed"
        }
    }

    /// Admission before the first operational effect: acquires the owner or reports the
    /// refusal. A refused foreground request is not queued.
    private func admit(_ family: HubLifecycleFamily,
                       phase: String = "admitted",
                       barrier: (() -> Void)? = nil,
                       refused: (HubLifecycleRefusal) -> Void) -> HubLifecycleOperation? {
        switch lifecycle.acquire(family, phase: phase, installingBarrier: barrier) {
        case let .success(token):
            return makeOperation(token)
        case let .failure(refusal):
            HubAppLog.shared.record("lifecycle.refused", category: "service", level: "WARN",
                                    fields: ["family": family.letter, "reason": Self.refusalCode(refusal)])
            refused(refusal)
            return nil
        }
    }

    private func admit<Value>(_ family: HubLifecycleFamily,
                              phase: String = "admitted",
                              completion: (Result<Value, Error>) -> Void) -> HubLifecycleOperation? {
        admit(family, phase: phase, refused: { completion(.failure(HubActionError.lifecycleRefused($0))) })
    }

    /// Writes a preference only while the lifecycle is open (appearance, frame autosave).
    func performPreferenceWrite(_ body: () -> Void) -> Bool {
        lifecycle.performPreferenceWrite(body)
    }

    /// Controller-managed startup coordination: stale import directories are removed
    /// before the interface is offered, under the existing per-directory ownership.
    @discardableResult
    func runStartupCleanup() -> Int {
        guard !previewMode, lifecycle.capabilities.state == .open else { return 0 }
        let result = staleImportCleaner()
        if result.removed > 0 {
            HubAppLog.shared.record("temporary_files.removed", category: "teslamate_import",
                                    fields: ["directories": String(result.removed)])
        }
        return result.removed
    }

    /// Accepted termination: no later admission, queued reconciliation dropped, prepared
    /// migration sessions closed.
    @discardableResult
    func beginClosing() -> HubLifecycleAuthority.ClosingEntry {
        let entry = lifecycle.beginClosing()
        guard case .entered = entry else { return entry }
        discardDeferredReconciles()
        closeRegisteredMigrationSessions()
        return entry
    }

    /// Termination is happening: observers shut down and the closed state is final.
    func completeClosing() {
        lifecycle.markClosed()
        statusLifecycle.shutdown()
        closeRegisteredMigrationSessions()
    }

    /// Calls back once, when no owner is held and no restart is owed.
    func whenLifecycleSettled(_ callback: @escaping () -> Void) {
        lifecycle.whenIdle { [self] in serviceRunner.whenRestartDutySettled(callback) }
    }

    // MARK: Prepared migration sessions

    func closeMigrationSession(_ session: TeslaMateServerImportSession) {
        sessionLock.lock()
        registeredSessions.removeValue(forKey: ObjectIdentifier(session))
        sessionLock.unlock()
        session.close()
    }

    var registeredMigrationSessionCount: Int {
        sessionLock.lock(); defer { sessionLock.unlock() }
        return registeredSessions.count
    }

    private func registerMigrationSession(_ session: TeslaMateServerImportSession) {
        sessionLock.lock()
        registeredSessions[ObjectIdentifier(session)] = session
        sessionLock.unlock()
    }

    private func closeRegisteredMigrationSessions() {
        sessionLock.lock()
        let sessions = Array(registeredSessions.values)
        registeredSessions.removeAll()
        sessionLock.unlock()
        sessions.forEach { $0.close() }
    }

    private var isServiceInstalled: Bool {
        serviceInstalledOverride ?? launchAgent?.isInstalled ?? false
    }

    private var configPath: URL {
        if let configuredConfigPath { return configuredConfigPath }
        return homeDirectory
            .appendingPathComponent("Library/Application Support/Teslatlas Hub", isDirectory: true)
            .appendingPathComponent("config.toml")
    }

    private var dataDirectory: URL {
        configuredDataDirectory
            ?? configPath.deletingLastPathComponent().appendingPathComponent("data", isDirectory: true)
    }

    private var migrationHandoverMarker: URL {
        configPath.deletingLastPathComponent().appendingPathComponent(".teslamate-handover-pending")
    }

    private func writeHistoryOnlyControl() throws {
        try HistoryOnlyControl.write(for: configPath)
    }

    private var migrationHandoverState: HubMigrationHandoverState? {
        guard FileManager.default.fileExists(atPath: migrationHandoverMarker.path) else {
            return nil
        }
        guard let values = try? migrationHandoverMarker.resourceValues(
            forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey]
        ), values.isRegularFile == true, values.isSymbolicLink != true,
              let size = values.fileSize, size <= 4_096,
              let data = HubAppLog.regularFileData(of: migrationHandoverMarker,
                                                   maximumBytes: 4_096),
              let state = try? JSONDecoder().decode(HubMigrationHandoverState.self, from: data) else {
            // An unreadable marker remains a safe gate and resumes at verification.
            return HubMigrationHandoverState(phase: .awaitingVerification,
                                              previousIntervalSeconds: 60,
                                              previousProvider: nil)
        }
        return state
    }

    private func writeMigrationHandoverMarker(_ state: HubMigrationHandoverState) throws {
        let folder = configPath.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let data = try JSONEncoder().encode(state)
        try data.write(to: migrationHandoverMarker, options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o600)],
                                              ofItemAtPath: migrationHandoverMarker.path)
    }

    private func configuredCollectorIntervalSeconds() -> Int {
        guard let content = try? readConfigIfPresent(),
              let configured = Self.collectorIntervalSeconds(in: content),
              configured > 0 else { return 60 }
        return configured
    }

    private func configuredCollectorProvider() -> String? {
        guard let content = try? readConfigIfPresent() else {
            return nil
        }
        return Self.collectorProvider(in: content)
    }

    private func readConfigIfPresent() throws -> String? {
        let descriptor = Darwin.open(configPath.path,
                                     O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK)
        guard descriptor >= 0 else {
            if errno == ENOENT { return nil }
            throw HubActionError.commandFailed(HubL10n.text("hub.HubController.3430.1177", fallback: "Hub configuration is not a regular file."))
        }
        defer { Darwin.close(descriptor) }

        var information = stat()
        guard fstat(descriptor, &information) == 0,
              information.st_mode & S_IFMT == S_IFREG else {
            throw HubActionError.commandFailed(HubL10n.text("hub.HubController.3430.1177", fallback: "Hub configuration is not a regular file."))
        }
        let maximumBytes = 1024 * 1024
        guard information.st_size >= 0, information.st_size <= off_t(maximumBytes) else {
            throw HubActionError.commandFailed(HubL10n.text("hub.HubController.3441.1179", fallback: "Hub configuration is too large."))
        }
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 16 * 1024)
        while true {
            let count = buffer.withUnsafeMutableBytes { bytes in
                Darwin.read(descriptor, bytes.baseAddress, bytes.count)
            }
            if count == 0 { break }
            if count < 0 {
                if errno == EINTR { continue }
                throw HubActionError.commandFailed(HubL10n.text("hub.HubController.3452.1180", fallback: "Hub configuration could not be read."))
            }
            guard data.count + count <= maximumBytes else {
                throw HubActionError.commandFailed(HubL10n.text("hub.HubController.3441.1179", fallback: "Hub configuration is too large."))
            }
            data.append(contentsOf: buffer.prefix(count))
        }
        guard let content = String(data: data, encoding: .utf8) else {
            throw HubActionError.commandFailed(HubL10n.text("hub.HubController.3460.1182", fallback: "Hub configuration must use UTF-8 text."))
        }
        return content
    }

    private func ensureConfig(provider: String? = nil,
                              collectorIntervalSeconds: Int? = nil) throws {
        let manager = FileManager.default
        let configFolder = configPath.deletingLastPathComponent()
        try manager.createDirectory(at: configFolder, withIntermediateDirectories: true)
        try manager.setAttributes([.posixPermissions: NSNumber(value: 0o700)], ofItemAtPath: configFolder.path)
        try manager.createDirectory(at: dataDirectory, withIntermediateDirectories: true)
        try manager.setAttributes([.posixPermissions: NSNumber(value: 0o700)], ofItemAtPath: dataDirectory.path)
        if let original = try readConfigIfPresent() {
            var updated = Self.addOfflineDefaults(to: original)
            if let provider {
                updated = Self.settingCollectorProvider(provider, in: updated)
            }
            if let collectorIntervalSeconds {
                updated = Self.settingCollectorInterval(collectorIntervalSeconds, in: updated)
            }
            if updated != original {
                try Data(updated.utf8).write(to: configPath, options: .atomic)
                try manager.setAttributes([.posixPermissions: NSNumber(value: 0o600)], ofItemAtPath: configPath.path)
            }
            return
        }
        var collectorBlock = ""
        if provider != nil || collectorIntervalSeconds != nil {
            collectorBlock = "\n[collector]\n"
            if let provider { collectorBlock += "provider = \"\(provider)\"\n" }
            if let collectorIntervalSeconds {
                collectorBlock += "interval_seconds = \(collectorIntervalSeconds)\n"
            }
        }
        let content = """
        data_dir = \(Self.tomlBasicString(dataDirectory.path))
        bind = "127.0.0.1:8080"
        \(collectorBlock)

        [geocoder]
        enabled = false

        [terrain]
        enabled = false
        """ + "\n"
        let temporary = configFolder.appendingPathComponent(".config.\(UUID().uuidString).tmp")
        try Data(content.utf8).write(to: temporary)
        try manager.setAttributes([.posixPermissions: NSNumber(value: 0o600)], ofItemAtPath: temporary.path)
        do { try manager.moveItem(at: temporary, to: configPath) }
        catch { try? manager.removeItem(at: temporary); throw error }
    }

    private func restoreConfig(_ content: String?) throws {
        guard let content else { return }
        try Data(content.utf8).write(to: configPath, options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o600)],
                                              ofItemAtPath: configPath.path)
    }

    private func unwindFailedImport(originalConfig: String?) throws {
        if let originalConfig {
            try restoreConfig(originalConfig)
        } else if FileManager.default.fileExists(atPath: configPath.path) {
            let values = try configPath.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey])
            guard values.isRegularFile == true, values.isSymbolicLink != true else {
                throw HubActionError.commandFailed(
                    HubL10n.text("hub.HubController.3527.1187", fallback: "Hub configuration recovery refused to remove a replaced configuration path.")
                )
            }
            try FileManager.default.removeItem(at: configPath)
        }
        if FileManager.default.fileExists(atPath: migrationHandoverMarker.path) {
            try FileManager.default.removeItem(at: migrationHandoverMarker)
        }
    }

    private func recoverFailedImport(_ importError: Error, originalConfig: String?) -> Error {
        do {
            try unwindFailedImport(originalConfig: originalConfig)
            return importError
        } catch let recoveryError {
            let safetyState = hasPendingMigrationHandover
                ? HubL10n.text("hub.HubController.3543.1188", fallback: "The migration safety marker remains and Hub must stay stopped.")
                : HubL10n.text("hub.HubController.3544.1189", fallback: "Hub must stay stopped until its configuration is repaired.")
            return HubActionError.commandFailed(
                HubL10n.format("hub.HubController.3546.1190", fallback: "TeslaMate import failed: %1$@ Configuration recovery also failed: %2$@ %3$@", arguments: [String(describing: importError.localizedDescription), String(describing: recoveryError.localizedDescription), String(describing: safetyState)])
            )
        }
    }

    static func settingCollectorProvider(_ provider: String, in content: String) -> String {
        precondition(provider == "legacy" || provider == "fleet")
        var lines = content.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)

        func uncommented(_ line: String) -> String {
            String(line.split(separator: "#", maxSplits: 1, omittingEmptySubsequences: false)[0])
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }
        func isTableHeader(_ line: String) -> Bool {
            let value = uncommented(line)
            return value.hasPrefix("[") && value.contains("]")
        }

        if let table = lines.firstIndex(where: { uncommented($0) == "[collector]" }) {
            let end = lines[(table + 1)...].firstIndex(where: isTableHeader) ?? lines.endIndex
            if let setting = lines[(table + 1)..<end].firstIndex(where: { line in
                let value = uncommented(line)
                guard let equals = value.firstIndex(of: "=") else { return false }
                return value[..<equals].trimmingCharacters(in: .whitespaces) == "provider"
            }) {
                let indentation = String(lines[setting].prefix { $0 == " " || $0 == "\t" })
                lines[setting] = "\(indentation)provider = \"\(provider)\""
            } else {
                lines.insert("provider = \"\(provider)\"", at: table + 1)
            }
        } else {
            if !lines.isEmpty && lines.last != "" { lines.append("") }
            lines.append("[collector]")
            lines.append("provider = \"\(provider)\"")
        }
        if content.hasSuffix("\n") && lines.last != "" { lines.append("") }
        return lines.joined(separator: "\n")
    }

    static func collectorProvider(in content: String) -> String? {
        let lines = content.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        func uncommented(_ line: String) -> String {
            String(line.split(separator: "#", maxSplits: 1, omittingEmptySubsequences: false)[0])
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }
        func isTableHeader(_ line: String) -> Bool {
            let value = uncommented(line)
            return value.hasPrefix("[") && value.contains("]")
        }
        guard let table = lines.firstIndex(where: { uncommented($0) == "[collector]" }) else {
            return nil
        }
        let end = lines[(table + 1)...].firstIndex(where: isTableHeader) ?? lines.endIndex
        for line in lines[(table + 1)..<end] {
            let value = uncommented(line)
            guard let equals = value.firstIndex(of: "="),
                  value[..<equals].trimmingCharacters(in: .whitespaces) == "provider" else {
                continue
            }
            let raw = value[value.index(after: equals)...]
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .trimmingCharacters(in: CharacterSet(charactersIn: "\""))
            if raw == "legacy" || raw == "fleet" {
                return raw
            }
            return nil
        }
        return nil
    }

    static func collectorIntervalSeconds(in content: String) -> Int? {
        let lines = content.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        func uncommented(_ line: String) -> String {
            String(line.split(separator: "#", maxSplits: 1, omittingEmptySubsequences: false)[0])
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }
        func isTableHeader(_ line: String) -> Bool {
            let value = uncommented(line)
            return value.hasPrefix("[") && value.contains("]")
        }
        guard let table = lines.firstIndex(where: { uncommented($0) == "[collector]" }) else {
            return nil
        }
        let end = lines[(table + 1)...].firstIndex(where: isTableHeader) ?? lines.endIndex
        for line in lines[(table + 1)..<end] {
            let value = uncommented(line)
            guard let equals = value.firstIndex(of: "="),
                  value[..<equals].trimmingCharacters(in: .whitespaces) == "interval_seconds" else {
                continue
            }
            return Int(value[value.index(after: equals)...].trimmingCharacters(in: .whitespaces))
        }
        return nil
    }

    static func settingCollectorInterval(_ seconds: Int, in content: String) -> String {
        precondition(seconds >= 0)
        var lines = content.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        func uncommented(_ line: String) -> String {
            String(line.split(separator: "#", maxSplits: 1, omittingEmptySubsequences: false)[0])
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }
        func isTableHeader(_ line: String) -> Bool {
            let value = uncommented(line)
            return value.hasPrefix("[") && value.contains("]")
        }
        if let table = lines.firstIndex(where: { uncommented($0) == "[collector]" }) {
            let end = lines[(table + 1)...].firstIndex(where: isTableHeader) ?? lines.endIndex
            if let setting = lines[(table + 1)..<end].firstIndex(where: { line in
                let value = uncommented(line)
                guard let equals = value.firstIndex(of: "=") else { return false }
                return value[..<equals].trimmingCharacters(in: .whitespaces) == "interval_seconds"
            }) {
                let indentation = String(lines[setting].prefix { $0 == " " || $0 == "\t" })
                lines[setting] = "\(indentation)interval_seconds = \(seconds)"
            } else {
                lines.insert("interval_seconds = \(seconds)", at: table + 1)
            }
        } else {
            if !lines.isEmpty && lines.last != "" { lines.append("") }
            lines.append("[collector]")
            lines.append("interval_seconds = \(seconds)")
        }
        if content.hasSuffix("\n") && lines.last != "" { lines.append("") }
        return lines.joined(separator: "\n")
    }

    static func addOfflineDefaults(to content: String) -> String {
        var lines = content.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)

        func uncommented(_ line: String) -> String {
            String(line.split(separator: "#", maxSplits: 1, omittingEmptySubsequences: false)[0])
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }
        func isTableHeader(_ line: String) -> Bool {
            let value = uncommented(line)
            return value.hasPrefix("[") && value.contains("]")
        }
        func hasEnabled(from start: Int, to end: Int) -> Bool {
            guard start < end else { return false }
            return lines[start..<end].contains { line in
                let value = uncommented(line)
                guard value.hasPrefix("enabled") else { return false }
                return value.dropFirst("enabled".count)
                    .trimmingCharacters(in: .whitespaces).hasPrefix("=")
            }
        }

        var changed = false
        for name in ["geocoder", "terrain"] {
            if let table = lines.firstIndex(where: { uncommented($0) == "[\(name)]" }) {
                let end = lines[(table + 1)...].firstIndex(where: isTableHeader) ?? lines.endIndex
                if !hasEnabled(from: table + 1, to: end) {
                    lines.insert("enabled = false", at: table + 1)
                    changed = true
                }
            } else {
                if !lines.isEmpty && lines.last != "" { lines.append("") }
                lines.append("[\(name)]")
                lines.append("enabled = false")
                changed = true
            }
        }
        guard changed else { return content }
        if content.hasSuffix("\n") && lines.last != "" { lines.append("") }
        return lines.joined(separator: "\n")
    }

    static func tomlBasicString(_ value: String) -> String {
        var encoded = "\""
        for scalar in value.unicodeScalars {
            switch scalar.value {
            case 0x08: encoded += "\\b"
            case 0x09: encoded += "\\t"
            case 0x0A: encoded += "\\n"
            case 0x0C: encoded += "\\f"
            case 0x0D: encoded += "\\r"
            case 0x22: encoded += "\\\""
            case 0x5C: encoded += "\\\\"
            case 0x00...0x1F, 0x7F:
                encoded += String(format: "\\u%04X", scalar.value)
            default:
                encoded.unicodeScalars.append(scalar)
            }
        }
        encoded += "\""
        return encoded
    }

    static func logTail(of url: URL, maximumBytes: Int) -> String? {
        HubAppLog.regularFileTail(of: url, maximumBytes: maximumBytes)
    }

    /// Plain words for the registry of collection attention the Hub reports in
    /// `status` as `collectionAttention`. An unknown word, from a newer Hub, gets
    /// the general line. These are diagnostic lines like the others, not catalog
    /// strings, and the stable code stays the only thing parsed.
    static func collectionAttentionLabel(_ code: String) -> String {
        switch code {
        case "collector_api_refused": return "Owner API has refused collection"
        case "collector_provider_refused": return "The provider has refused collection"
        case "collector_fleet_access_refused": return "Fleet access has been refused"
        case "collector_billing_suspended": return "Fleet billing is suspended"
        case "collector_cap_reached": return "The local monthly cap has been reached"
        case "collector_config_error": return "Fleet configuration needs attention"
        case "collector_account_locked": return "The account is locked"
        case "collector_stream_audit_failed": return "Stream receipts cannot be written"
        case "collector_auth_waiting": return "Collection is waiting for credentials"
        case "collector_auth_configuration_error": return "Sign-in configuration needs attention"
        case "collector_auth_sign_in_required":
            return "Sign in again; collection can continue until the token expires"
        case "collector_cadence_invalid":
            return "Collection cadence needs attention; correct the setting and restart"
        case "collector_fleet_refresh_outcome_unknown":
            return "Reconnect Fleet credentials; the last refresh outcome is unknown"
        default: return "Collection needs attention"
        }
    }

    private func parseStatus(_ output: String) -> HubSnapshot? {
        guard let data = output.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let database = root["database"] as? [String: Any]
        let vehicles = root["vehicles"] as? [[String: Any]] ?? []
        let vehicle = root["vehicle"] as? [String: Any] ?? vehicles.first
        let credentials = root["credentials"] as? [String: Any]
        let ready = root["ready"] as? Bool ?? false
        let vehicleName = vehicles.count > 1
            ? HubL10n.plural("hub.HubController.3748.1243", count: vehicles.count,
                             one: "%d vehicle", other: "%d vehicles")
            : vehicle?["displayName"] as? String ?? HubL10n.text("hub.HubController.445.371", fallback: "No configured vehicle")
        let vehicleSummary: String
        if let observed = vehicle?["latestObservedAtMs"] as? NSNumber {
            vehicleSummary = HubL10n.format("hub.HubController.3752.1247", fallback: "Last seen %1$@", arguments: [String(describing: relativeAge(milliseconds: observed.int64Value))])
        } else {
            vehicleSummary = vehicle == nil ? HubL10n.text("hub.HubController.445.371", fallback: "No configured vehicle") : HubL10n.text("hub.HubController.3754.1249", fallback: "No observations yet")
        }
        let controlVehicleID = vehicles.count == 1
            ? (vehicles[0]["vehicleId"] as? String).flatMap(UUID.init(uuidString:))
            : nil
        let controlVehicles = vehicles.compactMap { vehicle -> HubControlVehicle? in
            guard let idValue = vehicle["vehicleId"] as? String,
                  let id = UUID(uuidString: idValue) else { return nil }
            let name = (vehicle["displayName"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
            let displayName = name.flatMap { $0.isEmpty ? nil : $0 } ?? HubL10n.text("hub.HubController.1163.481", fallback: "Vehicle")
            let status: String
            if let observed = vehicle["latestObservedAtMs"] as? NSNumber {
                status = HubL10n.format("hub.HubController.3752.1247", fallback: "Last seen %1$@", arguments: [String(describing: relativeAge(milliseconds: observed.int64Value))])
            } else {
                status = HubL10n.text("hub.HubController.3754.1249", fallback: "No observations yet")
            }
            let state = (vehicle["activityState"] as? String)?
                .trimmingCharacters(in: .whitespacesAndNewlines)
            let location = (vehicle["locationName"] as? String)?
                .trimmingCharacters(in: .whitespacesAndNewlines)
            let battery = (vehicle["batteryLevel"] as? NSNumber)?.intValue
            return HubControlVehicle(
                id: id,
                displayName: displayName,
                status: status,
                activityState: state.flatMap { $0.isEmpty ? nil : $0 },
                batteryLevel: battery.flatMap { (0...100).contains($0) ? $0 : nil },
                locationName: location.flatMap { $0.isEmpty ? nil : $0 },
                telemetryStatus: (vehicle["telemetryStatus"] as? String)
                    .flatMap(HubVehicleTelemetryStatus.init(rawValue:)) ?? .unknown
            )
        }
        let dbBytes = database?["bytes"] as? NSNumber
        let dbText = dbBytes.map { HubL10n.format("hub.HubController.3787.1262", fallback: "Healthy · %1$@ MB", arguments: [String(describing: $0.int64Value / 1_048_576)]) } ?? HubL10n.text("hub.HubController.394.344", fallback: "Waiting for setup or import")
        let dataDirectory = (database?["path"] as? String).map { URL(fileURLWithPath: $0).deletingLastPathComponent() }
        let service = ready ? HubL10n.text("hub.HubController.3789.1265", fallback: "Installed and running") : HubL10n.text("hub.HubController.3789.1266", fallback: "Installed · needs attention")
        let providerValue = root["provider"] as? String
        let configuredProvider = providerValue.flatMap(HubAccountProvider.init(rawValue:))
        let legacy = root["legacyCredentials"] as? [String: Any]
        let fleet = root["fleetCredentials"] as? [String: Any]
        let selectedPresent = credentials?["present"] as? Bool == true
        let legacyPresent = legacy?["present"] as? Bool == true
        let fleetPresent = fleet?["present"] as? Bool == true
        let provider: HubAccountProvider?
        if selectedPresent {
            provider = configuredProvider
        } else if legacyPresent != fleetPresent {
            provider = legacyPresent ? .legacy : .fleet
        } else {
            provider = configuredProvider
        }
        let account = (selectedPresent || legacyPresent || fleetPresent)
            ? HubL10n.text("hub.HubController.connected_state", fallback: "Connected")
            : (vehicles.isEmpty ? HubL10n.text("hub.HubController.393.343", fallback: "Not configured") : HubL10n.text("hub.HubController.3807.1275", fallback: "History available · Tesla not connected"))
        let fleetScope = fleet?["scopeStatus"] as? String
        var diagnosticLines = [
            "Service: \(service)",
            "Account: \(account)",
            "Vehicle: \(vehicleSummary)",
            "Database: \(dbText)",
            "Readiness: \(root["readinessReason"] as? String ?? "ready")",
            "Provider: \(providerValue ?? "unknown")",
            "Owner tokens: \(legacyPresent ? "present" : "absent")",
            "Fleet tokens: \(fleetPresent ? "present" : "absent")"
        ]
        if let fleetScope {
            diagnosticLines.append("Fleet scopes: \(fleetScope)")
        }
        // The one condition that needs the owner while the service stays up. It is
        // shown beside the readiness line and never changes `health`: only `ready`
        // does (B2: service readiness and collection attention are separate).
        if let attention = root["collectionAttention"] as? String {
            let readinessIndex = diagnosticLines.firstIndex { $0.hasPrefix("Readiness:") }
                ?? diagnosticLines.endIndex - 1
            diagnosticLines.insert("Collection attention: \(Self.collectionAttentionLabel(attention))",
                                   at: readinessIndex + 1)
        }
        return HubSnapshot(health: ready ? .running : .degraded,
                           service: service,
                           account: account,
                           accountState: selectedPresent || legacyPresent || fleetPresent
                               ? .connected : (vehicles.isEmpty ? .notConfigured : .historyOnly),
                           provider: provider,
                           vehicleName: vehicleName,
                           vehicle: vehicleSummary,
                           hasConfiguredVehicle: vehicle != nil,
                           controlVehicleID: controlVehicleID,
                           controlVehicles: controlVehicles,
                           database: dbText,
                           databaseState: dbBytes == nil ? .awaitingSetup : .healthy,
                           activity: [],
                           version: root["version"] as? String ?? HubRelease.fallbackVersion,
                           dataDirectory: dataDirectory,
                           diagnosticLines: diagnosticLines)
    }

    private func statusSnapshot(_ status: HubSnapshot, installed: Bool, loaded: HubServiceLoadState) -> HubSnapshot {
        guard installed else { return HubSnapshot(health: .needsInstall, service: HubL10n.text("hub.HubController.441.368", fallback: "Not installed"), account: status.account, accountState: status.accountState, provider: status.provider, vehicleName: status.vehicleName, vehicle: status.vehicle, hasConfiguredVehicle: status.hasConfiguredVehicle, controlVehicleID: nil, controlVehicles: status.controlVehicles, database: status.database, databaseState: status.databaseState, activity: status.activity, version: status.version, dataDirectory: status.dataDirectory ?? dataDirectory, diagnosticLines: status.diagnosticLines) }
        var result = status
        var descriptionState: ServiceDescriptionState
        switch loaded {
        case .loaded:
            result.health = status.health == .running ? .running : .degraded
            descriptionState = status.health == .running ? .running : .attention
            result.service = status.health == .running ? HubL10n.text("hub.HubController.3789.1265", fallback: "Installed and running") : HubL10n.text("hub.HubController.3789.1266", fallback: "Installed · needs attention")
        case .unloaded:
            result.health = .stopped
            descriptionState = .stopped
            result.service = HubL10n.text("hub.HubController.3846.1290", fallback: "Installed but stopped")
        case .unknown:
            result.health = .degraded
            descriptionState = .serviceStateUnavailable
            result.service = HubL10n.text("hub.HubController.3849.1291", fallback: "Installed · service state unavailable")
        }
        if status.version != HubRelease.bundledVersion {
            result.diagnosticLines.insert(
                "Version mismatch: service \(status.version), app \(HubRelease.bundledVersion)",
                at: 0
            )
            if case .loaded = loaded {
                result.health = .degraded
                descriptionState = .versionMismatch
                result.service = HubL10n.text("hub.HubController.3858.1293", fallback: "Installed · version mismatch")
            }
        }
        if let developmentConfiguration {
            result.service = Self.developmentServiceDescription(descriptionState)
            result.diagnosticLines.insert(
                "Development runtime: \(developmentConfiguration.serviceLabel)", at: 0
            )
            result.diagnosticLines.insert("Mode: \(developmentConfiguration.mode.rawValue)", at: 1)
            result.diagnosticLines.insert("Binary: \(developmentConfiguration.binary.path)", at: 2)
            result.diagnosticLines.insert("Logs: \(developmentConfiguration.logDirectory.path)", at: 3)
            result.diagnosticLines.insert("State: \(developmentConfiguration.stateDirectory.path)", at: 4)
            result.diagnosticLines.insert("Configuration: \(developmentConfiguration.config.path)", at: 5)
        }
        return result
    }

    private enum ServiceDescriptionState {
        case running, stopped, attention, serviceStateUnavailable, versionMismatch, statusUnavailable
    }

    private static func developmentServiceDescription(_ state: ServiceDescriptionState) -> String {
        switch state {
        case .running: return HubL10n.text("hub.HubController.3877.1301", fallback: "Development Hub running")
        case .stopped: return HubL10n.text("hub.HubController.3878.1303", fallback: "Development Hub stopped")
        case .attention: return HubL10n.text("hub.HubController.3879.1305", fallback: "Development Hub · needs attention")
        case .serviceStateUnavailable: return HubL10n.text("hub.HubController.3880.1307", fallback: "Development Hub · service state unavailable")
        case .versionMismatch: return HubL10n.text("hub.HubController.3881.1309", fallback: "Development Hub · version mismatch")
        case .statusUnavailable: return HubL10n.text("hub.HubController.3882.1311", fallback: "Development Hub · status unavailable")
        }
    }

    private func fallbackSnapshot(installed: Bool, loaded: HubServiceLoadState) -> HubSnapshot {
        guard installed else { return .firstRun }
        let health: HubHealth
        let service: String
        switch loaded {
        case .loaded: health = .degraded; service = HubL10n.text("hub.HubController.3882.1310", fallback: "Installed · status unavailable")
        case .unloaded: health = .stopped; service = HubL10n.text("hub.HubController.3846.1290", fallback: "Installed but stopped")
        case .unknown: health = .degraded; service = HubL10n.text("hub.HubController.3849.1291", fallback: "Installed · service state unavailable")
        }
        if let developmentConfiguration {
            let descriptionState: ServiceDescriptionState
            switch loaded {
            case .loaded: descriptionState = .statusUnavailable
            case .unloaded: descriptionState = .stopped
            case .unknown: descriptionState = .serviceStateUnavailable
            }
            let developmentService = Self.developmentServiceDescription(descriptionState)
            var retained = snapshot
            retained.health = health
            retained.service = developmentService
            retained.accountState = .unknown
            retained.account = HubL10n.text("hub.HubController.344.340", fallback: "Unknown")
            retained.controlVehicleID = nil
            retained.databaseState = .unknown
            retained.database = snapshot.databaseState == .awaitingSetup ? "Unknown"
                : snapshot.database == "Unknown" || snapshot.database.hasPrefix("Last known: ")
                    ? snapshot.database : "Last known: \(snapshot.database)"
            retained.dataDirectory = snapshot.dataDirectory ?? dataDirectory
            retained.controlVehicles = snapshot.controlVehicles.map {
                HubControlVehicle(id: $0.id, displayName: $0.displayName, status: $0.status,
                                  activityState: $0.activityState, batteryLevel: $0.batteryLevel,
                                  locationName: $0.locationName,
                                  telemetryStatus: $0.telemetryStatus == .current ? .cached : $0.telemetryStatus)
            }
            retained.diagnosticLines = [developmentService, "Development runtime: \(developmentConfiguration.serviceLabel)", "Mode: \(developmentConfiguration.mode.rawValue)", "Binary: \(developmentConfiguration.binary.path)", "Configuration: \(developmentConfiguration.config.path)", "State: \(developmentConfiguration.stateDirectory.path)", "Logs: \(developmentConfiguration.logDirectory.path)", "Hub status is unavailable. Vehicle and database summaries are last known values, not a fresh report."]
            return retained
        }
        return HubSnapshot(health: health, service: service, account: HubL10n.text("hub.HubController.344.340", fallback: "Unknown"), accountState: .unknown, provider: nil, vehicleName: HubL10n.text("hub.HubController.1163.481", fallback: "Vehicle"), vehicle: HubL10n.text("hub.HubController.344.340", fallback: "Unknown"), hasConfiguredVehicle: false, controlVehicleID: nil, controlVehicles: [], database: HubL10n.text("hub.HubController.344.340", fallback: "Unknown"), databaseState: .unknown, activity: [], version: HubRelease.fallbackVersion, dataDirectory: dataDirectory, diagnosticLines: [service, "Hub status command did not return a valid report."])
    }

    private func relativeAge(milliseconds: Int64) -> String {
        let seconds = max(0, Int(Date().timeIntervalSince1970 - Double(milliseconds) / 1_000))
        if seconds < 60 { return HubL10n.text("hub.HubController.3905.1332", fallback: "just now") }
        if seconds < 3_600 { return HubL10n.plural("hub.HubController.3906.1333", count: Int(seconds / 60), one: "%d minute ago", other: "%d minutes ago") }
        if seconds < 86_400 { return HubL10n.plural("hub.HubController.3907.1334", count: Int(seconds / 3_600), one: "%d hour ago", other: "%d hours ago") }
        return HubL10n.plural("hub.HubController.3908.1335", count: Int(seconds / 86_400), one: "%d day ago", other: "%d days ago")
    }
}
