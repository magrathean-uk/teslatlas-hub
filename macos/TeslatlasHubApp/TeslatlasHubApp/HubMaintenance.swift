// SPDX-License-Identifier: AGPL-3.0-only

import Foundation

/// What the Backups and Paired devices sections of Service details need (M06-T04).
/// The app's own is `HubControllerMaintenance`; tests supply fakes. Every completion
/// runs on the main queue.
protocol HubMaintenanceBackend: AnyObject {
    /// The Hub data folder (a backup folder inside it is refused).
    var dataDirectory: URL? { get }
    /// Car names by Hub vehicle id, for the device grants.
    var vehicleNames: [UUID: String] { get }
    func loadBackupSchedule() -> Result<HubBackupSchedule, Error>
    /// Writes the `[backup]` section; with a password, first replaces the owner-only
    /// password file the section names. The service applies it when it restarts.
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
        case .unreadableReport: return "The Hub's answer could not be read."
        case .passwordTooShort:
            return "The recovery password needs at least \(HubBackupSchedule.minimumPasswordLength) characters."
        case .passwordFileUnwritable: return "The recovery password could not be saved."
        case .configurationMissing: return "Set up the Hub before changing backups."
        }
    }
}

/// The app's backend: the Hub's configuration file and its commands.
final class HubControllerMaintenance: HubMaintenanceBackend {
    /// The password file's name, next to `config.toml` in the owner-only Hub folder.
    static let passwordFileName = "backup-password"

    private let controller: HubController
    private let fileManager: FileManager

    init(controller: HubController, fileManager: FileManager = .default) {
        self.controller = controller
        self.fileManager = fileManager
    }

    var dataDirectory: URL? { controller.maintenanceDataDirectory }

    var vehicleNames: [UUID: String] {
        Dictionary(controller.snapshot.controlVehicles.map { ($0.id, $0.displayName) },
                   uniquingKeysWith: { first, _ in first })
    }

    private var configPath: URL { controller.maintenanceConfigPath }

    func loadBackupSchedule() -> Result<HubBackupSchedule, Error> {
        guard fileManager.fileExists(atPath: configPath.path) else { return .success(HubBackupSchedule()) }
        do {
            return .success(HubBackupSchedule(config: try String(contentsOf: configPath, encoding: .utf8)))
        } catch {
            return .failure(error)
        }
    }

    func saveBackupSchedule(_ schedule: HubBackupSchedule, password: String?) -> Result<HubBackupSchedule, Error> {
        guard fileManager.fileExists(atPath: configPath.path) else { return .failure(HubMaintenanceError.configurationMissing) }
        do {
            var saved = schedule
            if let password {
                let file = configPath.deletingLastPathComponent().appendingPathComponent(Self.passwordFileName)
                try Self.writePasswordFile(password, to: file)
                saved.passwordFile = file.path
            }
            let original = try String(contentsOf: configPath, encoding: .utf8)
            let updated = saved.applied(to: original)
            if updated != original {
                try Data(updated.utf8).write(to: configPath, options: .atomic)
                try fileManager.setAttributes([.posixPermissions: NSNumber(value: 0o600)], ofItemAtPath: configPath.path)
            }
            return .success(saved)
        } catch {
            return .failure(error)
        }
    }

    /// Writes the password to a new mode-0600 file beside `url` and renames it into
    /// place, so the service never reads a partial or group-readable file. The bytes
    /// are wiped from the app's buffer afterwards; nothing is logged.
    static func writePasswordFile(_ password: String, to url: URL) throws {
        guard password.count >= HubBackupSchedule.minimumPasswordLength else {
            throw HubMaintenanceError.passwordTooShort
        }
        var bytes = Array(password.utf8)
        defer { for index in bytes.indices { bytes[index] = 0 } }
        let temporary = url.deletingLastPathComponent()
            .appendingPathComponent(".\(url.lastPathComponent).\(UUID().uuidString).tmp")
        let descriptor = open(temporary.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard descriptor >= 0 else { throw HubMaintenanceError.passwordFileUnwritable }
        var written = 0
        let ok: Bool = bytes.withUnsafeBufferPointer { buffer in
            while written < buffer.count {
                let count = write(descriptor, buffer.baseAddress! + written, buffer.count - written)
                if count <= 0 { return false }
                written += count
            }
            return fchmod(descriptor, 0o600) == 0 && fsync(descriptor) == 0
        }
        close(descriptor)
        guard ok, rename(temporary.path, url.path) == 0 else {
            unlink(temporary.path)
            throw HubMaintenanceError.passwordFileUnwritable
        }
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
