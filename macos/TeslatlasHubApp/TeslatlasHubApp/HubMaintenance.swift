// SPDX-License-Identifier: AGPL-3.0-only

import Foundation
import Security

/// What the Backups and Paired devices sections of Service details need (M06-T04).
/// The app's own is `HubControllerMaintenance`; tests supply fakes. Every completion
/// runs on the main queue.
protocol HubMaintenanceBackend: AnyObject {
    /// The Hub data folder (a backup folder inside it is refused).
    var dataDirectory: URL? { get }
    /// Car names by Hub vehicle id, for the device grants.
    var vehicleNames: [UUID: String] { get }
    func loadBackupSchedule() -> Result<HubBackupSchedule, Error>
    /// Writes the `[backup]` section with `password_source = "keychain"` (M06-T13); with a
    /// password, first replaces the Keychain item the section names (M06-T12). The service
    /// applies the schedule when it restarts and reads the password at each run.
    func saveBackupSchedule(_ schedule: HubBackupSchedule, password: String?) -> Result<HubBackupSchedule, Error>
    func readBackupStatus(completion: @escaping (Result<HubBackupStatus, Error>) -> Void)
    func backUpNow(completion: @escaping (HubBackupRunState) -> Void)
    func listPairedDevices(completion: @escaping (Result<[HubPairedDevice], Error>) -> Void)
    func revokeDevice(_ id: UUID, completion: @escaping (Result<Void, Error>) -> Void)
}

enum HubMaintenanceError: LocalizedError, Equatable {
    case unreadableReport
    case passwordTooShort
    case passwordFileUnwritable
    case configurationMissing

    var errorDescription: String? {
        switch self {
        case .unreadableReport: return HubL10n.text("hub.backup.maint.unreadable", fallback: "The Hub's answer could not be read.")
        case .passwordTooShort:
            return HubL10n.format("hub.backup.problem.passwordShort", fallback: "The recovery password needs at least %1$@ characters.", arguments: [String(describing: HubBackupSchedule.minimumPasswordLength)])
        case .passwordFileUnwritable: return HubL10n.text("hub.backup.maint.unwritable", fallback: "The recovery password could not be saved.")
        case .configurationMissing: return HubL10n.text("hub.backup.maint.configMissing", fallback: "Set up the Hub before changing backups.")
        }
    }
}

/// Where the app keeps the unattended backup password (M06-T12). The app's own is
/// `HubKeychainBackupPasswordStore`; tests supply a fake and never touch the login keychain.
protocol HubBackupPasswordStore: AnyObject {
    /// Replaces the item for `account` (the configured `password_file` path) with `password`.
    func store(_ password: [UInt8], account: String) throws
}

/// The owner's login keychain. The item is a generic password, service
/// `uk.magrathean.teslatlas.hub.backup`, account the configured path (no file is written
/// there). Its access list trusts exactly this app and its bundled service, so the
/// service reads it without a prompt and any other process meets the Keychain's own
/// confirmation. Saving deletes and re-adds the item, which renews the trusted code
/// after an update. The password is never logged.
final class HubKeychainBackupPasswordStore: HubBackupPasswordStore {
    static let service = "uk.magrathean.teslatlas.hub.backup"
    static let label = "Teslatlas Hub backup password"

    private let keychain: SecKeychain?
    private let serviceBinary: () -> URL?

    /// `keychain` nil is the owner's default (login) keychain; tests pass a disposable one.
    init(keychain: SecKeychain? = nil,
         serviceBinary: @escaping () -> URL? = { Bundle.main.url(forResource: "teslatlas-hub", withExtension: nil) }) {
        self.keychain = keychain
        self.serviceBinary = serviceBinary
    }

    func store(_ password: [UInt8], account: String) throws {
        guard let binary = serviceBinary() else { throw HubMaintenanceError.passwordFileUnwritable }
        let access = try Self.access(trusting: binary)
        let match: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.service,
            kSecAttrAccount: account
        ]
        var delete = match
        if let keychain { delete[kSecMatchSearchList] = [keychain] }
        let deleted = SecItemDelete(delete as CFDictionary)
        guard deleted == errSecSuccess || deleted == errSecItemNotFound else {
            throw HubMaintenanceError.passwordFileUnwritable
        }
        var secret = Data(password)
        defer { secret.resetBytes(in: 0..<secret.count) }
        var add = match
        add[kSecAttrLabel] = Self.label
        add[kSecAttrAccess] = access
        add[kSecValueData] = secret
        if let keychain { add[kSecUseKeychain] = keychain }
        guard SecItemAdd(add as CFDictionary, nil) == errSecSuccess else {
            throw HubMaintenanceError.passwordFileUnwritable
        }
    }

    /// An access list whose trusted applications are this app and the service binary.
    private static func access(trusting binary: URL) throws -> SecAccess {
        var app: SecTrustedApplication?
        var service: SecTrustedApplication?
        var access: SecAccess?
        guard SecTrustedApplicationCreateFromPath(nil, &app) == errSecSuccess, let app,
              SecTrustedApplicationCreateFromPath(binary.path, &service) == errSecSuccess, let service,
              SecAccessCreate(label as CFString, [app, service] as CFArray, &access) == errSecSuccess,
              let access else {
            throw HubMaintenanceError.passwordFileUnwritable
        }
        return access
    }
}

/// The app's backend: the Hub's configuration file and its commands.
final class HubControllerMaintenance: HubMaintenanceBackend {
    /// The name the configuration gives the password, next to `config.toml`. On the Mac
    /// it names the Keychain item; a file of that name is an earlier plaintext copy,
    /// moved into the Keychain and removed (M06-T12).
    static let passwordFileName = "backup-password"

    private let controller: HubController
    private let fileManager: FileManager
    private let passwordStore: HubBackupPasswordStore

    init(controller: HubController, fileManager: FileManager = .default,
         passwordStore: HubBackupPasswordStore = HubKeychainBackupPasswordStore()) {
        self.controller = controller
        self.fileManager = fileManager
        self.passwordStore = passwordStore
    }

    var dataDirectory: URL? { controller.maintenanceDataDirectory }

    var vehicleNames: [UUID: String] {
        Dictionary(controller.snapshot.controlVehicles.map { ($0.id, $0.displayName) },
                   uniquingKeysWith: { first, _ in first })
    }

    private var configPath: URL { controller.maintenanceConfigPath }

    /// The app's own password name beside `config.toml`.
    private var passwordName: URL {
        configPath.deletingLastPathComponent().appendingPathComponent(Self.passwordFileName)
    }

    func loadBackupSchedule() -> Result<HubBackupSchedule, Error> {
        guard fileManager.fileExists(atPath: configPath.path) else { return .success(HubBackupSchedule()) }
        do {
            let schedule = HubBackupSchedule(config: try String(contentsOf: configPath, encoding: .utf8))
            // A failed move keeps the file; the service then says it is waiting to move.
            try? migratePasswordFile(named: schedule.passwordFile)
            return .success(schedule)
        } catch {
            return .failure(error)
        }
    }

    /// Moves an earlier plaintext password file into the Keychain and removes it. Only the
    /// app's own file is moved, only when the configuration names it, and only when it
    /// passes the service's own checks (a regular file the owner alone can read, at most
    /// 1 KiB). With no file there is nothing to do; on any failure the file stays.
    func migratePasswordFile(named configured: String?) throws {
        let file = passwordName
        guard configured == file.path else { return }
        var info = stat()
        guard lstat(file.path, &info) == 0 else { return }
        var bytes = try Self.readPasswordFile(at: file)
        defer { for index in bytes.indices { bytes[index] = 0 } }
        try passwordStore.store(bytes, account: file.path)
        guard unlink(file.path) == 0 else { throw HubMaintenanceError.passwordFileUnwritable }
    }

    /// The bytes of an owner-only password file, one trailing line end dropped. Any other
    /// file (a link, another owner, group or world access, empty or over 1 KiB) is refused.
    static func readPasswordFile(at url: URL) throws -> [UInt8] {
        let descriptor = open(url.path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
        guard descriptor >= 0 else { throw HubMaintenanceError.passwordFileUnwritable }
        defer { close(descriptor) }
        var info = stat()
        guard fstat(descriptor, &info) == 0, (info.st_mode & S_IFMT) == S_IFREG,
              info.st_uid == getuid(), (info.st_mode & 0o077) == 0,
              info.st_size > 0, info.st_size <= 1024 else {
            throw HubMaintenanceError.passwordFileUnwritable
        }
        var bytes = [UInt8](repeating: 0, count: Int(info.st_size))
        let count = bytes.withUnsafeMutableBytes { read(descriptor, $0.baseAddress, $0.count) }
        guard count == bytes.count else {
            for index in bytes.indices { bytes[index] = 0 }
            throw HubMaintenanceError.passwordFileUnwritable
        }
        if bytes.last == UInt8(ascii: "\n") {
            bytes.removeLast()
            if bytes.last == UInt8(ascii: "\r") { bytes.removeLast() }
        }
        return bytes
    }

    func saveBackupSchedule(_ schedule: HubBackupSchedule, password: String?) -> Result<HubBackupSchedule, Error> {
        guard fileManager.fileExists(atPath: configPath.path) else { return .failure(HubMaintenanceError.configurationMissing) }
        do {
            var saved = schedule
            let file = passwordName
            if let password {
                guard password.count >= HubBackupSchedule.minimumPasswordLength else {
                    throw HubMaintenanceError.passwordTooShort
                }
                var bytes = Array(password.utf8)
                defer { for index in bytes.indices { bytes[index] = 0 } }
                try passwordStore.store(bytes, account: file.path)
                // The Keychain item replaces an earlier plaintext copy.
                if unlink(file.path) != 0 && errno != ENOENT { throw HubMaintenanceError.passwordFileUnwritable }
                saved.passwordFile = file.path
            } else {
                try? migratePasswordFile(named: saved.passwordFile)
            }
            let original = try String(contentsOf: configPath, encoding: .utf8)
            let updated = Self.namingKeychainSource(in: saved.applied(to: original))
            if updated != original {
                try Data(updated.utf8).write(to: configPath, options: .atomic)
                try fileManager.setAttributes([.posixPermissions: NSNumber(value: 0o600)], ofItemAtPath: configPath.path)
            }
            return .success(saved)
        } catch {
            return .failure(error)
        }
    }

    /// The source the app's configuration names for the unattended password (M06-T13).
    static let keychainPasswordSource = "keychain"

    /// `config` with `password_source = "keychain"` in its `[backup]` table, so the
    /// service reads the password only from the Keychain item the app writes (its account
    /// stays the `password_file` path). An existing `password_source` line is replaced;
    /// other lines, other tables and comments stay as they are. `config` already holds
    /// the table (`HubBackupSchedule.applied(to:)` adds it).
    static func namingKeychainSource(in config: String) -> String {
        var lines = config.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        func content(_ line: String) -> String {
            String(line.split(separator: "#", maxSplits: 1, omittingEmptySubsequences: false)[0])
                .trimmingCharacters(in: .whitespaces)
        }
        guard let table = lines.firstIndex(where: { content($0) == "[backup]" }) else { return config }
        let entry = "password_source = \"\(keychainPasswordSource)\""
        let end = lines[(table + 1)...].firstIndex(where: { content($0).hasPrefix("[") }) ?? lines.endIndex
        let existing = lines[(table + 1)..<end].firstIndex { line in
            let parts = content(line).split(separator: "=", maxSplits: 1)
            return parts.count == 2 && parts[0].trimmingCharacters(in: .whitespaces) == "password_source"
        }
        if let existing {
            guard content(lines[existing]) != entry else { return config }
            let indentation = String(lines[existing].prefix { $0 == " " || $0 == "\t" })
            lines[existing] = indentation + entry
        } else {
            lines.insert(entry, at: table + 1)
        }
        return lines.joined(separator: "\n")
    }

    func readBackupStatus(completion: @escaping (Result<HubBackupStatus, Error>) -> Void) {
        controller.runMaintenanceCommand(["backup-status"]) { result in
            completion(result.flatMap { output -> Result<HubBackupStatus, Error> in
                guard let status = HubBackupStatus(report: output) else {
                    return .failure(HubMaintenanceError.unreadableReport)
                }
                return .success(status)
            })
        }
    }

    func backUpNow(completion: @escaping (HubBackupRunState) -> Void) {
        controller.runMaintenanceCommand(["backup-now"]) { result in
            completion(HubBackupRunState.outcome(of: result))
        }
    }

    func listPairedDevices(completion: @escaping (Result<[HubPairedDevice], Error>) -> Void) {
        controller.runMaintenanceCommand(["control", "paired-devices"]) { result in
            completion(result.flatMap { output -> Result<[HubPairedDevice], Error> in
                guard let devices = HubPairedDevice.list(from: output) else {
                    return .failure(HubMaintenanceError.unreadableReport)
                }
                return .success(devices)
            })
        }
    }

    func revokeDevice(_ id: UUID, completion: @escaping (Result<Void, Error>) -> Void) {
        controller.runMaintenanceCommand(["control", "revoke-device", id.uuidString.lowercased()]) { result in
            completion(result.map { _ in () })
        }
    }
}
