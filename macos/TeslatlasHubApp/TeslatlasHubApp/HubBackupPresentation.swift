// SPDX-License-Identifier: AGPL-3.0-only

import Foundation

/// The `[backup]` section of the Hub configuration (M06-T03), as the app reads and
/// writes it. The recovery password is never part of it: `password_file` names an
/// owner-only file that the service reads at each run. A change applies when the
/// service restarts. The words on these screens are English only (M06-T04).
struct HubBackupSchedule: Equatable {
    var enabled = false
    var destination: String?
    var passwordFile: String?
    var intervalHours = 24
    var keep = 7

    /// The service's own limits (`backup.interval_hours`, `backup.keep`).
    static let intervalRange = 1...8784
    static let keepRange = 1...1000
    /// The intervals the app offers; a configured value outside them is kept and shown.
    static let intervalChoices = [1, 6, 12, 24, 48, 168]
    /// The service refuses a shorter password.
    static let minimumPasswordLength = 12

    init(enabled: Bool = false, destination: String? = nil, passwordFile: String? = nil,
         intervalHours: Int = 24, keep: Int = 7) {
        self.enabled = enabled
        self.destination = destination
        self.passwordFile = passwordFile
        self.intervalHours = intervalHours
        self.keep = keep
    }

    /// Reads the `[backup]` table of `config`. A missing table or key keeps the
    /// service's default; a value the app cannot read is left at its default too.
    init(config: String) {
        self.init()
        for (key, value) in Self.tableEntries(in: config) {
            switch key {
            case "enabled": if let flag = Bool(value) { enabled = flag }
            case "interval_hours": if let hours = Int(value) { intervalHours = hours }
            case "keep": if let count = Int(value) { keep = count }
            case "destination": destination = Self.tomlString(value)
            case "password_file": passwordFile = Self.tomlString(value)
            default: break
            }
        }
    }

    /// Why the service would refuse these settings, or nil. `dataDirectory` is the
    /// Hub's data folder: a destination inside it is refused. `newPassword` is one
    /// about to be saved with them.
    func problem(dataDirectory: URL?, newPassword: String? = nil) -> String? {
        if let newPassword, newPassword.count < Self.minimumPasswordLength {
            return HubL10n.format("hub.backup.problem.passwordShort", fallback: "The recovery password needs at least %1$@ characters.", arguments: [String(describing: Self.minimumPasswordLength)])
        }
        guard Self.intervalRange.contains(intervalHours) else {
            return HubL10n.text("hub.backup.problem.interval", fallback: "The interval must be between 1 and 8784 hours.")
        }
        guard Self.keepRange.contains(keep) else {
            return HubL10n.text("hub.backup.problem.keep", fallback: "Keep must be between 1 and 1000 backups.")
        }
        if let destination {
            guard destination.hasPrefix("/") else { return HubL10n.text("hub.backup.problem.chooseFolder", fallback: "Choose a backup folder.") }
            if let dataDirectory, HubBackupSchedule.isInside(destination, folder: dataDirectory.path) {
                return HubL10n.text("hub.backup.problem.folderInside", fallback: "Choose a backup folder outside the Hub data folder.")
            }
        }
        if enabled && (destination == nil || (passwordFile == nil && newPassword == nil)) {
            return HubL10n.text("hub.backup.problem.needsBoth", fallback: "Scheduled backups need a backup folder and a recovery password.")
        }
        return nil
    }

    static func isInside(_ path: String, folder: String) -> Bool {
        let candidate = path.split(separator: "/").map(String.init)
        let base = folder.split(separator: "/").map(String.init)
        return !base.isEmpty && candidate.count >= base.count && Array(candidate.prefix(base.count)) == base
    }

    /// `config` with its `[backup]` table set to these values. Other tables, other
    /// keys and comments stay as they are; a missing table is appended.
    func applied(to config: String) -> String {
        var lines = config.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        var values: [(String, String?)] = [
            ("enabled", enabled ? "true" : "false"),
            ("destination", destination.map(HubController.tomlBasicString)),
            ("password_file", passwordFile.map(HubController.tomlBasicString)),
            ("interval_hours", String(intervalHours)),
            ("keep", String(keep))
        ]
        if let table = lines.firstIndex(where: { Self.uncommented($0) == "[backup]" }) {
            var end = lines[(table + 1)...].firstIndex(where: Self.isTableHeader) ?? lines.endIndex
            var index = table + 1
            while index < end {
                if let key = Self.key(of: lines[index]), let slot = values.firstIndex(where: { $0.0 == key }) {
                    if let value = values[slot].1 {
                        let indentation = String(lines[index].prefix { $0 == " " || $0 == "\t" })
                        lines[index] = "\(indentation)\(key) = \(value)"
                        index += 1
                    } else {
                        lines.remove(at: index)
                        end -= 1
                    }
                    values.remove(at: slot)
                } else {
                    index += 1
                }
            }
            var insertion = table + 1
            for (key, value) in values {
                guard let value else { continue }
                lines.insert("\(key) = \(value)", at: insertion)
                insertion += 1
            }
        } else {
            while let last = lines.last, last.isEmpty { lines.removeLast() }
            if !lines.isEmpty { lines.append("") }
            lines.append("[backup]")
            for (key, value) in values {
                guard let value else { continue }
                lines.append("\(key) = \(value)")
            }
            lines.append("")
            return lines.joined(separator: "\n")
        }
        if config.hasSuffix("\n") && lines.last != "" { lines.append("") }
        return lines.joined(separator: "\n")
    }

    // MARK: TOML lines

    private static func uncommented(_ line: String) -> String {
        // A `#` inside a quoted value is not a comment; the keys read here never hold one
        // before their value, so only the header and key checks use this.
        String(line.split(separator: "#", maxSplits: 1, omittingEmptySubsequences: false)[0])
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func isTableHeader(_ line: String) -> Bool {
        let value = uncommented(line)
        return value.hasPrefix("[") && value.contains("]")
    }

    private static func key(of line: String) -> String? {
        let value = uncommented(line)
        guard let equals = value.firstIndex(of: "=") else { return nil }
        let key = value[..<equals].trimmingCharacters(in: .whitespaces)
        return key.isEmpty ? nil : key
    }

    /// The `key = raw value` pairs of the `[backup]` table, values untrimmed of quotes.
    static func tableEntries(in config: String) -> [(String, String)] {
        let lines = config.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        guard let table = lines.firstIndex(where: { uncommented($0) == "[backup]" }) else { return [] }
        let end = lines[(table + 1)...].firstIndex(where: isTableHeader) ?? lines.endIndex
        return lines[(table + 1)..<end].compactMap { line in
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard !trimmed.hasPrefix("#"), let equals = trimmed.firstIndex(of: "=") else { return nil }
            let key = trimmed[..<equals].trimmingCharacters(in: .whitespaces)
            var value = trimmed[trimmed.index(after: equals)...].trimmingCharacters(in: .whitespaces)
            if !value.hasPrefix("\""), !value.hasPrefix("'") {
                value = uncommented(value)
            }
            return (key, value)
        }
    }

    /// A TOML basic or literal string on one line, or nil.
    static func tomlString(_ raw: String) -> String? {
        if raw.hasPrefix("'") {
            let body = raw.dropFirst()
            guard let closing = body.firstIndex(of: "'") else { return nil }
            return String(body[..<closing])
        }
        guard raw.hasPrefix("\"") else { return nil }
        var result = String.UnicodeScalarView()
        var iterator = raw.unicodeScalars.dropFirst().makeIterator()
        while let scalar = iterator.next() {
            switch scalar {
            case "\"": return String(result)
            case "\\":
                guard let escape = iterator.next() else { return nil }
                switch escape {
                case "b": result.append("\u{08}")
                case "t": result.append("\t")
                case "n": result.append("\n")
                case "f": result.append("\u{0C}")
                case "r": result.append("\r")
                case "\"": result.append("\"")
                case "\\": result.append("\\")
                case "u", "U":
                    var digits = ""
                    for _ in 0..<(escape == "u" ? 4 : 8) {
                        guard let digit = iterator.next() else { return nil }
                        digits.unicodeScalars.append(digit)
                    }
                    guard let code = UInt32(digits, radix: 16), let decoded = Unicode.Scalar(code) else { return nil }
                    result.append(decoded)
                default: return nil
                }
            default: result.append(scalar)
            }
        }
        return nil
    }
}

/// A refusal the service gave (`code: message` on the command's error output).
struct HubServiceRefusal: Equatable {
    let code: String
    let message: String

    /// The last `code: message` line of `text`, where the code is a snake_case word.
    static func parse(_ text: String) -> HubServiceRefusal? {
        for rawLine in text.split(whereSeparator: \.isNewline).reversed() {
            var line = rawLine.trimmingCharacters(in: .whitespaces)
            for prefix in ["Error: ", "error: "] where line.hasPrefix(prefix) {
                line = String(line.dropFirst(prefix.count))
            }
            guard let colon = line.range(of: ": ") else { continue }
            let code = String(line[..<colon.lowerBound])
            guard !code.isEmpty, code.contains("_"),
                  code.unicodeScalars.allSatisfy({ CharacterSet.lowercaseLetters.contains($0)
                      || CharacterSet.decimalDigits.contains($0) || $0 == "_" }) else { continue }
            return HubServiceRefusal(code: code, message: String(line[colon.upperBound...]))
        }
        return nil
    }
}

/// What `backup-status` reports (M06-T04): read-only, with no path and no password.
struct HubBackupStatus: Equatable {
    var enabled = false
    var configured = false
    var running = false
    var paused = false
    var lastAttempt: Date?
    var lastSuccess: Date?
    var lastGeneration: String?
    var lastFailureCode: String?
    var lastFailureMessage: String?
    var blocked = false
    var nextRun: Date?

    static let blockedCode = "backup_blocked_by_refused_credentials"
    static let runningCode = "backup_running"

    init() {}

    /// Reads the report defensively; nil when it is not a JSON object.
    init?(report: String) {
        guard let root = Self.jsonObject(report) else { return nil }
        enabled = root["enabled"] as? Bool ?? false
        configured = root["configured"] as? Bool ?? false
        running = root["running"] as? Bool ?? false
        paused = root["paused"] as? Bool ?? false
        lastAttempt = HubCollectionStatus.date(root["lastAttemptMs"])
        lastSuccess = HubCollectionStatus.date(root["lastSuccessMs"])
        lastGeneration = root["lastGeneration"] as? String
        lastFailureCode = root["lastFailureCode"] as? String
        lastFailureMessage = root["lastFailureMessage"] as? String
        blocked = root["blocked"] as? Bool ?? (lastFailureCode == Self.blockedCode)
        nextRun = HubCollectionStatus.date(root["nextRunMs"])
    }

    static func jsonObject(_ output: String) -> [String: Any]? {
        if let data = output.data(using: .utf8),
           let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            return root
        }
        for line in output.split(whereSeparator: { $0.isNewline }).reversed() {
            if let data = String(line).data(using: .utf8),
               let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                return root
            }
        }
        return nil
    }

    // MARK: Words

    func lastGoodText(date: (Date) -> String = HubCollectionStatus.defaultDateText) -> String {
        switch (lastSuccess, lastGeneration) {
        case let (success?, generation?): return "\(date(success)) (\(generation))"
        case let (success?, nil): return date(success)
        case let (nil, generation?): return generation
        case (nil, nil): return HubL10n.text("hub.backup.lastGoodNone", fallback: "None yet")
        }
    }

    func nextRunText(date: (Date) -> String = HubCollectionStatus.defaultDateText) -> String {
        if !enabled { return HubL10n.text("hub.backup.nextOff", fallback: "Scheduled backups are off") }
        if !configured { return HubL10n.text("hub.backup.nextNotScheduled", fallback: "Not scheduled: choose a folder and a password") }
        if paused { return HubL10n.text("hub.backup.nextPaused", fallback: "Waiting for credential recovery") }
        return nextRun.map(date) ?? HubL10n.text("hub.collection.notReported", fallback: "Not reported")
    }

    /// The service's words when refused credentials block backups. The app offers no
    /// purge action: the message names the command to run.
    var blockedText: String? {
        guard blocked else { return nil }
        if let lastFailureMessage, !lastFailureMessage.isEmpty { return lastFailureMessage }
        return HubL10n.text("hub.backup.blocked", fallback: "Refused credentials block backups. Run purge-refused-credentials to clear them.")
    }

    func lastResultText() -> String? {
        guard let code = lastFailureCode, !blocked else { return nil }
        if let lastFailureMessage, !lastFailureMessage.isEmpty { return HubL10n.format("hub.backup.lastFailedMessage", fallback: "Last attempt failed: %1$@", arguments: [lastFailureMessage]) }
        return HubL10n.format("hub.backup.lastFailedCode", fallback: "Last attempt failed (%1$@)", arguments: [code])
    }

    func rows(date: (Date) -> String = HubCollectionStatus.defaultDateText) -> [(label: String, value: String)] {
        var rows: [(label: String, value: String)] = [
            (HubL10n.text("hub.backup.row.lastGood", fallback: "Last good backup"), lastGoodText(date: date)),
            (HubL10n.text("hub.backup.row.next", fallback: "Next backup"), running ? HubL10n.text("hub.backup.row.running", fallback: "A backup is running now") : nextRunText(date: date))
        ]
        if let text = lastResultText() { rows.append((HubL10n.text("hub.backup.row.lastAttempt", fallback: "Last attempt"), text)) }
        return rows
    }
}

/// The Back up now progress as the Backups section shows it.
enum HubBackupRunState: Equatable {
    case idle
    /// The app's own request is in flight.
    case running
    /// The request ended before the run (the command's wait is shorter than a large
    /// backup), or a scheduled run holds the slot; the service is still working.
    case stillRunning
    case finished(generation: String?)
    case blocked(message: String)
    case failed(message: String)

    var isBusy: Bool { self == .running || self == .stillRunning }

    var text: String? {
        switch self {
        case .idle: return nil
        case .running: return HubL10n.text("hub.backup.run.running", fallback: "Backing up…")
        case .stillRunning: return HubL10n.text("hub.backup.run.stillRunning", fallback: "The backup is still running in the Hub service.")
        case let .finished(generation?): return HubL10n.format("hub.backup.run.finishedGeneration", fallback: "Backup finished: %1$@", arguments: [generation])
        case .finished(nil): return HubL10n.text("hub.backup.run.finished", fallback: "Backup finished")
        case let .blocked(message): return message
        case let .failed(message): return HubL10n.format("hub.backup.run.failed", fallback: "Backup failed: %1$@", arguments: [message])
        }
    }

    /// The outcome of `backup-now`. A command that gave up waiting while the service may
    /// still run the backup is not a failure: the caller asks `backup-status` next.
    static func outcome(of result: Result<String, Error>) -> HubBackupRunState {
        switch result {
        case let .success(output):
            let root = HubBackupStatus.jsonObject(output)
            return .finished(generation: root?["generation"] as? String)
        case let .failure(error):
            let text = HubActionError.settingQuiescenceAside(error).localizedDescription
            if case HubActionError.commandTimedOut = HubActionError.settingQuiescenceAside(error) {
                return .stillRunning
            }
            guard let refusal = HubServiceRefusal.parse(text) else { return .failed(message: text) }
            switch refusal.code {
            case HubBackupStatus.blockedCode: return .blocked(message: refusal.message)
            case HubBackupStatus.runningCode: return .stillRunning
            // The command's wait ended; the run continues in the service.
            case "control_channel_failed" where refusal.message.contains("answer did not arrive"):
                return .stillRunning
            default: return .failed(message: refusal.message.isEmpty ? refusal.code : refusal.message)
            }
        }
    }
}

/// One paired device from `control paired-devices`, with the cars it may read. The
/// listing carries no bearer material.
struct HubPairedDevice: Equatable {
    let id: UUID
    let name: String
    let created: Date?
    let expires: Date?
    let revoked: Date?
    let lastSeen: Date?
    let vehicles: [UUID]

    var isActive: Bool { revoked == nil && (expires.map { $0 > Date() } ?? true) }

    /// Reads the listing (snake_case, as the service writes it; camelCase accepted).
    static func list(from output: String) -> [HubPairedDevice]? {
        let text = output.trimmingCharacters(in: .whitespacesAndNewlines)
        let candidates = [text] + text.split(whereSeparator: \.isNewline).reversed().map(String.init)
        for candidate in candidates {
            guard let data = candidate.data(using: .utf8),
                  let array = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] else { continue }
            return array.compactMap(device)
        }
        return nil
    }

    private static func device(_ object: [String: Any]) -> HubPairedDevice? {
        func value(_ snake: String, _ camel: String) -> Any? { object[snake] ?? object[camel] }
        guard let raw = value("device_id", "deviceId") as? String, let id = UUID(uuidString: raw) else { return nil }
        let vehicles = (object["vehicles"] as? [String] ?? []).compactMap(UUID.init(uuidString:))
        return HubPairedDevice(
            id: id,
            name: (value("display_name", "displayName") as? String).flatMap { $0.isEmpty ? nil : $0 } ?? HubL10n.text("hub.devices.unnamed", fallback: "Unnamed device"),
            created: HubCollectionStatus.date(value("created_at_ms", "createdAtMs")),
            expires: HubCollectionStatus.date(value("expires_at_ms", "expiresAtMs")),
            revoked: HubCollectionStatus.date(value("revoked_at_ms", "revokedAtMs")),
            lastSeen: HubCollectionStatus.date(value("last_authenticated_at_ms", "lastAuthenticatedAtMs")),
            vehicles: vehicles
        )
    }

    /// The cars this device may read, by name where the Hub knows it.
    func grantText(vehicleNames: [UUID: String]) -> String {
        guard !vehicles.isEmpty else { return HubL10n.text("hub.devices.noCars", fallback: "No cars") }
        return vehicles.map { vehicleNames[$0] ?? HubL10n.format("hub.devices.carFallback", fallback: "Car %1$@", arguments: [String($0.uuidString.prefix(8).lowercased())]) }
            .joined(separator: ", ")
    }

    func stateText(date: (Date) -> String = HubCollectionStatus.defaultDateText) -> String {
        if let revoked { return HubL10n.format("hub.devices.stateRevoked", fallback: "Revoked %1$@", arguments: [date(revoked)]) }
        if let expires, expires <= Date() { return HubL10n.format("hub.devices.stateExpired", fallback: "Expired %1$@", arguments: [date(expires)]) }
        if let lastSeen { return HubL10n.format("hub.devices.stateLastSeen", fallback: "Last seen %1$@", arguments: [date(lastSeen)]) }
        return HubL10n.text("hub.devices.stateNeverSeen", fallback: "Not seen yet")
    }
}
