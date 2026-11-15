// SPDX-License-Identifier: AGPL-3.0-only

import Darwin
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
    /// Writes the `[backup]` section. With a password, first saves it in the Keychain item
    /// and only then names `password_source = "keychain"` (M06-T12, M06-T17); without one,
    /// the configured source and path stay as they are. The service applies the schedule
    /// when it restarts and reads the password at each run.
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
    /// The service did not save or confirm the password; its own words follow.
    case passwordNotSaved(String)
    case passwordSourceUnknown(String)
    case passwordFileRefused
    case passwordFileConflict
    case passwordFileChanged
    case passwordFileNotRemoved

    var errorDescription: String? {
        switch self {
        case .unreadableReport: return HubL10n.text("hub.backup.maint.unreadable", fallback: "The Hub's answer could not be read.")
        case .passwordTooShort:
            return HubL10n.format("hub.backup.problem.passwordShort", fallback: "The recovery password needs at least %1$@ characters.", arguments: [String(describing: HubBackupSchedule.minimumPasswordLength)])
        case .passwordFileUnwritable: return HubL10n.text("hub.backup.maint.unwritable", fallback: "The recovery password could not be saved.")
        case .configurationMissing: return HubL10n.text("hub.backup.maint.configMissing", fallback: "Set up the Hub before changing backups.")
        case let .passwordNotSaved(reason):
            return HubL10n.format("hub.backup.maint.notSaved", fallback: "The recovery password could not be saved in the Keychain: %1$@", arguments: [reason])
        case let .passwordSourceUnknown(value):
            return HubL10n.format("hub.backup.maint.sourceUnknown", fallback: "The Hub configuration names a password source the app does not know (%1$@). Set backup.password_source to \"keychain\" or \"file\"; nothing was changed.", arguments: [value])
        case .passwordFileRefused:
            return HubL10n.text("hub.backup.maint.fileRefused", fallback: "An earlier backup password file (backup-password beside the Hub configuration) was left in place: only a file you alone can open (mode 0600, no access list allowing others), holding at most 1 KiB of text, is moved into the Keychain.")
        case .passwordFileConflict:
            return HubL10n.text("hub.backup.maint.fileConflict", fallback: "An earlier backup password file (backup-password beside the Hub configuration) differs from the password in the Keychain. The Keychain password is used; delete the file once you no longer need it.")
        case .passwordFileChanged:
            return HubL10n.text("hub.backup.maint.fileChanged", fallback: "The earlier backup password file changed while it was being moved, so it was left in place. Open Backups again to try once more.")
        case .passwordFileNotRemoved:
            return HubL10n.text("hub.backup.maint.fileNotRemoved", fallback: "The password is in the Keychain, but the earlier backup password file could not be removed. Delete backup-password beside the Hub configuration yourself.")
        }
    }
}

/// What the Keychain item holds compared with a password the app offers.
enum HubBackupPasswordComparison: Equatable {
    case missing
    case equal
    case different
}

/// Where the unattended backup password is kept (M06-T12, M06-T17). The app's own is
/// `HubServiceBackupPasswordStore`; tests supply a fake and never touch a keychain.
protocol HubBackupPasswordStore: AnyObject {
    /// Saves `password` as the item for `account` and reads it back through the service's
    /// own access; throws unless the service can read exactly these bytes.
    func store(_ password: [UInt8], account: String) throws
    /// Compares `password` with the item for `account` through the service's access.
    func compare(_ password: [UInt8], account: String) throws -> HubBackupPasswordComparison
    /// Whether the item for `account` holds a password the service can use.
    func hasPassword(account: String) throws -> Bool
}

/// The bundled service binary keeps the item (DEC-123, option A): the app runs
/// `teslatlas-hub backup-password store|compare|status --account <account>` and writes the
/// password as one line on that process's standard input, never an argument, environment
/// variable or file. The item is created by the service binary, so the LaunchAgent (the
/// same binary) reads it without a prompt and the app itself cannot read it. The item is a
/// generic password, service `uk.magrathean.teslatlas.hub.backup`, account the configured
/// path or `unattended-backup`. The password is never logged.
final class HubServiceBackupPasswordStore: HubBackupPasswordStore {
    typealias Runner = (_ arguments: [String], _ standardInput: String?) -> Result<String, Error>

    private let run: Runner

    init(run: @escaping Runner = HubServiceBackupPasswordStore.embedded) {
        self.run = run
    }

    func store(_ password: [UInt8], account: String) throws {
        guard try outcome("store", account: account, password: password) == "stored" else {
            throw HubMaintenanceError.unreadableReport
        }
    }

    func compare(_ password: [UInt8], account: String) throws -> HubBackupPasswordComparison {
        switch try outcome("compare", account: account, password: password) {
        case "missing": return .missing
        case "equal": return .equal
        case "different": return .different
        default: throw HubMaintenanceError.unreadableReport
        }
    }

    func hasPassword(account: String) throws -> Bool {
        switch try outcome("status", account: account, password: nil) {
        case "present": return true
        case "missing": return false
        default: throw HubMaintenanceError.unreadableReport
        }
    }

    private func outcome(_ action: String, account: String, password: [UInt8]?) throws -> String {
        var input: String?
        if let password {
            guard let text = String(bytes: password, encoding: .utf8) else { throw HubMaintenanceError.passwordFileRefused }
            input = text + "\n"
        }
        switch run(["backup-password", action, "--account", account], input) {
        case let .success(output):
            let line = output.split(separator: "\n").last.map(String.init) ?? ""
            guard let object = try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any],
                  let value = object["backupPassword"] as? String else {
                throw HubMaintenanceError.unreadableReport
            }
            return value
        case let .failure(error):
            let text = error.localizedDescription
            throw HubMaintenanceError.passwordNotSaved(HubServiceRefusal.parse(text)?.message ?? text)
        }
    }

    /// The bundled `teslatlas-hub`, the same file the LaunchAgent runs.
    static func embedded(arguments: [String], standardInput: String?) -> Result<String, Error> {
        guard let executable = Bundle.main.url(forResource: "teslatlas-hub", withExtension: nil) else {
            return .failure(HubActionError.missingResource("teslatlas-hub"))
        }
        final class Outcome: @unchecked Sendable {
            var value: Result<String, Error> = .failure(HubMaintenanceError.unreadableReport)
        }
        let outcome = Outcome()
        let done = DispatchSemaphore(value: 0)
        HubProcessExecutor.run(executable: executable, arguments: arguments, stdin: standardInput,
                               timeout: 30) { result in
            outcome.value = result
            done.signal()
        }
        done.wait()
        return outcome.value
    }
}

/// An earlier plaintext password file, held open from its checks to its removal (M06-T17).
/// The directory and the file stay open, so the bytes moved are the bytes checked and only
/// that same file is removed: a file grown, rewritten or replaced meanwhile is refused.
final class HubHeldPasswordFile {
    private let directory: Int32
    private let descriptor: Int32
    private let name: String
    private let checked: stat
    private(set) var bytes: [UInt8]

    /// Opens `url`; nil when there is no such file. Refuses a link, a non-regular file, a
    /// file of another owner, any group or other mode bit, an ACL `allow` entry (the
    /// service's own rule), an empty file, one over 1 KiB, text that is not UTF-8, and a
    /// file that changes while it is read. One trailing line end is dropped.
    static func open(_ url: URL) throws -> HubHeldPasswordFile? {
        let directory = Darwin.open(url.deletingLastPathComponent().path, O_RDONLY | O_DIRECTORY | O_CLOEXEC)
        guard directory >= 0 else { throw HubMaintenanceError.passwordFileRefused }
        let name = url.lastPathComponent
        let descriptor = openat(directory, name, O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
        guard descriptor >= 0 else {
            let missing = errno == ENOENT
            close(directory)
            if missing { return nil }
            throw HubMaintenanceError.passwordFileRefused
        }
        do {
            var info = stat()
            guard fstat(descriptor, &info) == 0, (info.st_mode & S_IFMT) == S_IFREG,
                  info.st_uid == getuid(), (info.st_mode & 0o077) == 0,
                  info.st_size > 0, info.st_size <= 1024,
                  try !aclGrantsAccess(descriptor) else {
                throw HubMaintenanceError.passwordFileRefused
            }
            var bytes = [UInt8](repeating: 0, count: 1025)
            var filled = 0
            while filled < bytes.count {
                let count = bytes.withUnsafeMutableBytes { read(descriptor, $0.baseAddress! + filled, $0.count - filled) }
                if count < 0 && errno == EINTR { continue }
                guard count >= 0 else { throw HubMaintenanceError.passwordFileRefused }
                if count == 0 { break }
                filled += count
            }
            bytes.removeSubrange(filled...)
            var after = stat()
            guard fstat(descriptor, &after) == 0, Self.same(info, after), filled == Int(info.st_size) else {
                for index in bytes.indices { bytes[index] = 0 }
                throw HubMaintenanceError.passwordFileChanged
            }
            if bytes.last == UInt8(ascii: "\n") {
                bytes.removeLast()
                if bytes.last == UInt8(ascii: "\r") { bytes.removeLast() }
            }
            guard !bytes.isEmpty, String(bytes: bytes, encoding: .utf8) != nil else {
                for index in bytes.indices { bytes[index] = 0 }
                throw HubMaintenanceError.passwordFileRefused
            }
            return HubHeldPasswordFile(directory: directory, descriptor: descriptor, name: name, checked: info, bytes: bytes)
        } catch {
            close(descriptor)
            close(directory)
            throw error
        }
    }

    private init(directory: Int32, descriptor: Int32, name: String, checked: stat, bytes: [UInt8]) {
        self.directory = directory
        self.descriptor = descriptor
        self.name = name
        self.checked = checked
        self.bytes = bytes
    }

    deinit {
        for index in bytes.indices { bytes[index] = 0 }
        close(descriptor)
        close(directory)
    }

    /// Removes the name only while it still refers to the held, unchanged file. macOS has
    /// no unlink-by-descriptor, so the name is first renamed aside in the same folder and
    /// removed only when the entry moved is the held file; anything else is put back and
    /// refused. A file changed since its checks is refused and left; a removal the system
    /// refuses is reported.
    func removeIfUnchanged() throws {
        var now = stat()
        guard fstat(descriptor, &now) == 0, Self.same(checked, now) else {
            throw HubMaintenanceError.passwordFileChanged
        }
        let aside = ".\(name).removing-\(UUID().uuidString)"
        guard renameatx_np(directory, name, directory, aside, UInt32(RENAME_EXCL)) == 0 else {
            throw errno == ENOENT ? HubMaintenanceError.passwordFileChanged : HubMaintenanceError.passwordFileNotRemoved
        }
        var moved = stat()
        guard fstatat(directory, aside, &moved, AT_SYMLINK_NOFOLLOW) == 0,
              moved.st_dev == now.st_dev, moved.st_ino == now.st_ino else {
            _ = renameatx_np(directory, aside, directory, name, UInt32(RENAME_EXCL))
            throw HubMaintenanceError.passwordFileChanged
        }
        guard unlinkat(directory, aside, 0) == 0 else {
            _ = renameatx_np(directory, aside, directory, name, UInt32(RENAME_EXCL))
            throw HubMaintenanceError.passwordFileNotRemoved
        }
    }

    private static func same(_ first: stat, _ second: stat) -> Bool {
        first.st_dev == second.st_dev && first.st_ino == second.st_ino && first.st_size == second.st_size
            && first.st_mtimespec.tv_sec == second.st_mtimespec.tv_sec
            && first.st_mtimespec.tv_nsec == second.st_mtimespec.tv_nsec
            && first.st_nlink == second.st_nlink
    }

    /// Whether the open file's extended ACL holds an `allow` entry (the service's rule:
    /// a deny-only list grants nothing). Fails closed.
    static func aclGrantsAccess(_ descriptor: Int32) throws -> Bool {
        guard let acl = acl_get_fd_np(descriptor, ACL_TYPE_EXTENDED) else {
            if errno == ENOENT { return false }
            throw HubMaintenanceError.passwordFileRefused
        }
        defer { acl_free(UnsafeMutableRawPointer(acl)) }
        var entry: acl_entry_t?
        var which = ACL_FIRST_ENTRY.rawValue
        while acl_get_entry(acl, which, &entry) == 0, let current = entry {
            var tag = ACL_UNDEFINED_TAG
            guard acl_get_tag_type(current, &tag) == 0 else { return true }
            if tag == ACL_EXTENDED_ALLOW { return true }
            which = ACL_NEXT_ENTRY.rawValue
        }
        return false
    }
}

/// The app's backend: the Hub's configuration file and its commands.
final class HubControllerMaintenance: HubMaintenanceBackend {
    /// The name the configuration gives the password, next to `config.toml`. On the Mac
    /// it names the Keychain item; a file of that name is an earlier plaintext copy,
    /// moved into the Keychain and removed (M06-T12, M06-T17).
    static let passwordFileName = "backup-password"

    private let controller: HubController
    private let fileManager: FileManager
    private let passwordStore: HubBackupPasswordStore

    init(controller: HubController, fileManager: FileManager = .default,
         passwordStore: HubBackupPasswordStore = HubServiceBackupPasswordStore()) {
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

    /// The account `backup-password` uses when the configuration names no path (the
    /// service's `DEFAULT_ACCOUNT`).
    static let defaultKeychainAccount = "unattended-backup"

    /// Where the configuration says the password comes from (M06-T13), as the service reads
    /// it on the Mac: no source with a path means the Keychain, no source and no path means
    /// nothing is configured.
    enum PasswordSource: Equatable {
        case keychain(account: String)
        case file
        case unset
        case unknown(String)
    }

    static func passwordSource(of schedule: HubBackupSchedule) -> PasswordSource {
        switch schedule.passwordSource {
        case nil: return schedule.passwordFile.map { .keychain(account: $0) } ?? .unset
        case "keychain"?: return .keychain(account: schedule.passwordFile ?? defaultKeychainAccount)
        case "file"?: return .file
        case let other?: return .unknown(other)
        }
    }

    /// Reads the schedule. Only a Keychain source ever moves an earlier password file; a
    /// configured file source is left exactly as it is; an unknown source changes nothing
    /// and is shown. A move that fails or stops half way is shown, never hidden.
    func loadBackupSchedule() -> Result<HubBackupSchedule, Error> {
        guard fileManager.fileExists(atPath: configPath.path) else { return .success(HubBackupSchedule()) }
        do {
            var schedule = HubBackupSchedule(config: try String(contentsOf: configPath, encoding: .utf8))
            switch Self.passwordSource(of: schedule) {
            case let .unknown(value):
                schedule.passwordNotice = HubMaintenanceError.passwordSourceUnknown(value).localizedDescription
            case .file:
                schedule.passwordStored = schedule.passwordFile != nil
            case .unset:
                schedule.passwordStored = false
            case let .keychain(account):
                do {
                    try migratePasswordFile(account: account)
                } catch {
                    schedule.passwordNotice = error.localizedDescription
                }
                schedule.passwordStored = storedPassword(account: account, notice: &schedule.passwordNotice)
            }
            return .success(schedule)
        } catch {
            return .failure(error)
        }
    }

    /// Whether the item holds a usable password; a Keychain that cannot answer is shown and
    /// counts as holding one, so an ordinary edit never demands the password again.
    private func storedPassword(account: String, notice: inout String?) -> Bool {
        do {
            return try passwordStore.hasPassword(account: account)
        } catch {
            if notice == nil { notice = error.localizedDescription }
            return true
        }
    }

    /// Moves an earlier plaintext password file into the Keychain and removes it (M06-T12,
    /// order of M06-T17 notes section 3). Only the app's own `backup-password` beside the
    /// configuration, only when the configuration names it as the Keychain account. The
    /// file is held open throughout. An item that already holds the same password finishes
    /// an interrupted move; an item that holds another password wins and the file stays
    /// (conflict). Otherwise the service saves and reads back the password, and only then
    /// is the same unchanged file removed. With no file there is nothing to do.
    func migratePasswordFile(account: String) throws {
        let file = passwordName
        guard account == file.path, let held = try HubHeldPasswordFile.open(file) else { return }
        switch try passwordStore.compare(held.bytes, account: account) {
        case .equal: break
        case .different: throw HubMaintenanceError.passwordFileConflict
        case .missing: try passwordStore.store(held.bytes, account: account)
        }
        try held.removeIfUnchanged()
    }

    func saveBackupSchedule(_ schedule: HubBackupSchedule, password: String?) -> Result<HubBackupSchedule, Error> {
        guard fileManager.fileExists(atPath: configPath.path) else { return .failure(HubMaintenanceError.configurationMissing) }
        do {
            let original = try String(contentsOf: configPath, encoding: .utf8)
            let current = HubBackupSchedule(config: original)
            var saved = schedule
            saved.passwordNotice = nil
            var namedSource: String?
            let source = Self.passwordSource(of: current)
            if case let .unknown(value) = source { throw HubMaintenanceError.passwordSourceUnknown(value) }
            if let password {
                guard password.count >= HubBackupSchedule.minimumPasswordLength else {
                    throw HubMaintenanceError.passwordTooShort
                }
                // A configured Keychain account keeps its item; a file source or none moves
                // to the app's own account. Typing a password is the owner's explicit switch.
                let account: String
                if case let .keychain(configured) = source { account = configured } else { account = passwordName.path }
                var bytes = Array(password.utf8)
                defer { for index in bytes.indices { bytes[index] = 0 } }
                try passwordStore.store(bytes, account: account)
                saved.passwordFile = current.passwordFile == nil && account == Self.defaultKeychainAccount ? nil : account
                saved.passwordStored = true
                namedSource = Self.keychainPasswordSource
                // The Keychain item replaces an earlier plaintext copy of the app's own name.
                if account == passwordName.path {
                    do {
                        try HubHeldPasswordFile.open(passwordName)?.removeIfUnchanged()
                    } catch {
                        saved.passwordNotice = error.localizedDescription
                    }
                }
            } else {
                // The configured source and path stay; only the edited keys change.
                saved.passwordFile = current.passwordFile
                saved.passwordSource = current.passwordSource
                if case let .keychain(account) = source {
                    do {
                        try migratePasswordFile(account: account)
                    } catch {
                        saved.passwordNotice = error.localizedDescription
                    }
                    if current.passwordSource == nil { namedSource = Self.keychainPasswordSource }
                }
            }
            if let namedSource { saved.passwordSource = namedSource }
            var updated = saved.applied(to: original)
            if let namedSource { updated = Self.naming(passwordSource: namedSource, in: updated) }
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

    /// `config` with `password_source = "keychain"` in its `[backup]` table.
    static func namingKeychainSource(in config: String) -> String {
        naming(passwordSource: keychainPasswordSource, in: config)
    }

    /// `config` with exactly one `password_source` entry, `source`, in its `[backup]` table,
    /// whatever quoting the existing key used (`password_source`, `"password_source"` and
    /// `'password_source'` are one TOML key): the first is replaced and any repeat removed.
    /// Other lines, other tables and comments stay as they are.
    static func naming(passwordSource source: String, in config: String) -> String {
        var lines = config.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        guard let table = lines.firstIndex(where: HubBackupSchedule.isBackupHeader) else { return config }
        let entry = "password_source = \(HubController.tomlBasicString(source))"
        var end = lines[(table + 1)...].firstIndex(where: HubBackupSchedule.isTableHeaderLine) ?? lines.endIndex
        var found = false
        var index = table + 1
        while index < end {
            guard HubBackupSchedule.keyName(of: lines[index]) == "password_source" else { index += 1; continue }
            if found {
                lines.remove(at: index)
                end -= 1
                continue
            }
            found = true
            let indentation = String(lines[index].prefix { $0 == " " || $0 == "\t" })
            lines[index] = indentation + entry
            index += 1
        }
        if !found { lines.insert(entry, at: table + 1) }
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
