// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import CoreFoundation
import Darwin
import Foundation

// Delete Hub and Data, the filesystem half. Every deletion target is computed from the
// injected home and the bundle identifier only; nothing is taken from the
// configuration, the environment or user input. Deletion never uses a path API:
// it is descriptor-relative post-order `unlinkat` on the opened directories.

/// Marker table, one place (clean-room unit C10 and the backup archive of G2-8 update it
/// in the commit that renames or adds a format). Sources:
/// - `src/storage/data_recovery.rs`: `MANIFEST_NAME = backup-v4.json`,
///   `LEGACY_MANIFEST_NAME = backup-v3.json`, `COMPLETION_NAME = BACKUP_COMPLETE`.
/// - `src/auth/credential_recovery.rs`: `FILE_MAGIC` (37 bytes including the zero byte),
///   minimum export 84 bytes, `MAX_EXPORT_BYTES = 16,532`.
enum HubDataRemovalMarkers {
    static let backupNames: Set<String> = ["backup-v4.json", "backup-v3.json", "BACKUP_COMPLETE"]
    static let exportedDriveSuffix = ".gpx"
    static let recoveryMagic: [UInt8] = Array("TESLATLAS-HUB-CREDENTIAL-RECOVERY-V1".utf8) + [0]
    static let recoveryMinimumBytes = 84
    static let recoveryMaximumBytes = 16_532
    static let maximumDepth = 128
}

enum HubDataRemovalReason: Equatable {
    case link, otherDisk, otherOwner, deviceFile, notFolder, shared
    case backup, recoveryExport, exportedDrive, tooDeep, changedSinceCheck
    /// Content or metadata that could not be inspected: never treated as ordinary data.
    case unchecked
    /// The deletion could not start, so the target was left exactly as it was.
    case notDeleted
    /// The preferences domain could not be cleared.
    case notCleared

    var text: String {
        switch self {
        case .link: return HubL10n.text("hub.dataRemoval.reason.link", fallback: "is a link to another location")
        case .otherDisk: return HubL10n.text("hub.dataRemoval.reason.disk", fallback: "is on another disk")
        case .otherOwner: return HubL10n.text("hub.dataRemoval.reason.owner", fallback: "belongs to another user")
        case .deviceFile: return HubL10n.text("hub.dataRemoval.reason.device", fallback: "is a device file")
        case .notFolder: return HubL10n.text("hub.dataRemoval.reason.not_folder", fallback: "is not a folder")
        case .shared: return HubL10n.text("hub.dataRemoval.reason.shared", fallback: "is shared with other users")
        case .backup:
            return HubL10n.text("hub.dataRemoval.reason.backup",
                                fallback: "looks like a Hub backup. Move it out of the Hub folder first")
        case .recoveryExport:
            return HubL10n.text("hub.dataRemoval.reason.recovery",
                                fallback: "looks like an exported recovery file. Move it out of the Hub folder first")
        case .exportedDrive:
            return HubL10n.text("hub.dataRemoval.reason.drive",
                                fallback: "looks like an exported drive. Move it out of the Hub folder first")
        case .tooDeep: return HubL10n.text("hub.dataRemoval.reason.depth", fallback: "is nested too deeply to check")
        case .changedSinceCheck:
            return HubL10n.text("hub.dataRemoval.reason.changed", fallback: "changed while it was being deleted")
        case .unchecked:
            return HubL10n.text("hub.dataRemoval.reason.unchecked", fallback: "could not be checked")
        case .notDeleted:
            return HubL10n.text("hub.dataRemoval.reason.not_deleted",
                                fallback: "was not deleted because the deletion could not start")
        case .notCleared:
            return HubL10n.text("hub.dataRemoval.reason.not_cleared", fallback: "could not be cleared")
        }
    }
}

enum HubDataRemovalVerdict: Equatable {
    case admit
    case refuse(HubDataRemovalReason)
}

/// A refusal, carrying the exact user-facing text (R2 to R7 of the specification).
struct HubDataRemovalRefusal: LocalizedError, Equatable {
    let message: String
    /// For an item refusal, "<path> <reason>": the part R11 reuses after "because".
    let clause: String?
    var errorDescription: String? { message }

    init(message: String, clause: String? = nil) {
        self.message = message
        self.clause = clause
    }

    static func bundleIdentifier(_ identifier: String) -> Self {
        Self(message: HubL10n.format(
            "hub.dataRemoval.refusal.bundle",
            fallback: "This copy of the app cannot delete Hub data, because it is not the Teslatlas Hub app (%1$@). Nothing was changed.",
            arguments: [identifier]))
    }

    static func testHost() -> Self {
        Self(message: HubL10n.text(
            "hub.dataRemoval.refusal.test_host",
            fallback: "Hub data is not deleted by a test host in the real home folder."))
    }

    static func dataDirOutside(_ path: String) -> Self {
        Self(message: HubL10n.format(
            "hub.dataRemoval.refusal.outside",
            fallback: "The Hub configuration keeps its data outside the Hub folder, at %1$@. Delete Hub and Data deletes only the Hub's own folder, so nothing was changed. Delete that data yourself if you no longer need it. Remove Background Service stops the Hub without deleting anything.",
            arguments: [path]))
    }

    static func dataDirUnreadable() -> Self {
        Self(message: HubL10n.text(
            "hub.dataRemoval.refusal.unreadable",
            fallback: "The app cannot tell from the Hub configuration where the Hub keeps its data. Nothing was changed."))
    }

    static func item(_ path: String, _ reason: HubDataRemovalReason) -> Self {
        Self(message: HubL10n.format(
            "hub.dataRemoval.refusal.item",
            fallback: "Nothing was changed, because %1$@ %2$@.",
            arguments: [path, reason.text]),
             clause: "\(path) \(reason.text)")
    }
}

struct HubDataRemovalTargets: Equatable {
    static let bundleIdentifier = "eu.teslatlas.hub.app"

    struct Root: Equatable {
        enum Kind: Equatable { case folder, file }
        let id: String
        /// Folder under `Library` that holds the target.
        let parent: String
        let name: String
        let kind: Kind
    }

    let home: URL
    let roots: [Root]

    init(home: URL, bundleIdentifier: String) throws {
        guard bundleIdentifier == Self.bundleIdentifier else {
            throw HubDataRemovalRefusal.bundleIdentifier(bundleIdentifier)
        }
        self.home = home
        let identifier = Self.bundleIdentifier
        roots = [
            Root(id: "D1", parent: "Application Support", name: "Teslatlas Hub", kind: .folder),
            Root(id: "D2", parent: "Logs", name: "Teslatlas Hub", kind: .folder),
            Root(id: "D3", parent: "Caches", name: identifier, kind: .folder),
            Root(id: "D4", parent: "HTTPStorages", name: identifier, kind: .folder),
            Root(id: "D4", parent: "HTTPStorages", name: identifier + ".binarycookies", kind: .file),
            Root(id: "D5", parent: "WebKit", name: identifier, kind: .folder),
            Root(id: "D6", parent: "Saved Application State", name: identifier + ".savedState", kind: .folder)
        ]
    }

    var library: URL { home.appendingPathComponent("Library", isDirectory: true) }
    var hubFolder: URL {
        library.appendingPathComponent("Application Support/Teslatlas Hub", isDirectory: true)
    }
    var preferencesDomain: String { Self.bundleIdentifier }
    var preferencesFile: URL {
        library.appendingPathComponent("Preferences/\(Self.bundleIdentifier).plist")
    }
}

struct HubDataRemovalPlan {
    struct Entry {
        let root: HubDataRemovalTargets.Root
        let device: dev_t
        let inode: ino_t
        /// The identity of the parent folder the root was found in. The deletion opens the
        /// parent again and must find this folder, not another of the same name.
        let parentDevice: dev_t
        let parentInode: ino_t
    }
    let entries: [Entry]
    let preferencesPresent: Bool
}

struct HubDataRemovalKept: Equatable {
    let path: String
    let reason: String
    /// The app's saved settings (the preferences file). Never an item the user is asked
    /// to delete by hand: the file belongs to the preferences service.
    var settings = false
}

struct HubDataRemovalReport: Equatable {
    var deleted: [String] = []
    var kept: [HubDataRemovalKept] = []
    var warnings: [String] = []

    /// A warning, not a failure: macOS still lists the service as turned off.
    static let finalEnableFailedWarning = "final-enable-failed"
}

protocol HubDataRemoving: AnyObject {
    /// Read-only: refuses with the exact specification text.
    func check() throws -> HubDataRemovalPlan
    func delete(_ plan: HubDataRemovalPlan) -> HubDataRemovalReport
}

/// Pure admission rule for one entry; tested with fabricated `stat` values.
enum HubDataRemovalPolicy {
    static func admit(name: String, entry: stat, rootDevice: dev_t, owner: uid_t) -> HubDataRemovalVerdict {
        let type = entry.st_mode & S_IFMT
        if type == S_IFLNK { return .refuse(.link) }
        if entry.st_dev != rootDevice { return .refuse(.otherDisk) }
        if entry.st_uid != owner { return .refuse(.otherOwner) }
        if type == S_IFBLK || type == S_IFCHR { return .refuse(.deviceFile) }
        if type == S_IFREG, name.lowercased().hasSuffix(HubDataRemovalMarkers.exportedDriveSuffix) {
            return .refuse(.exportedDrive)
        }
        return .admit
    }
}

/// Effects of the production composition, each refusing under an XCTest host (D-E16):
/// the test host is the production app, so its preferences domain, temporary
/// import folders, Finder and termination are the owner's real ones.
enum HubProductionEffects {
    /// With no `effect`, the clearer runs `clearPreferencesDomain` on the given preferences
    /// seams (the real ones by default), so the synchronise and the re-read decide success.
    static func preferencesClearer(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        effect: ((String) -> Bool)? = nil,
        remove: @escaping (String) -> Void = HubProductionEffects.removePreferencesDomain,
        synchronize: @escaping (String) -> Bool = HubProductionEffects.synchronizePreferencesDomain,
        keysLeft: @escaping (String) -> [String]? = HubProductionEffects.preferencesKeysLeft
    ) -> (String) -> Bool {
        { domain in
            guard !HubTestHost.isXCTest(environment: environment) else { return true }
            if let effect { return effect(domain) }
            return clearPreferencesDomain(domain, remove: remove, synchronize: synchronize, keysLeft: keysLeft)
        }
    }

    static func removePreferencesDomain(_ domain: String) {
        UserDefaults.standard.removePersistentDomain(forName: domain)
    }

    static func synchronizePreferencesDomain(_ domain: String) -> Bool {
        CFPreferencesAppSynchronize(domain as CFString)
    }

    static func preferencesKeysLeft(_ domain: String) -> [String]? {
        CFPreferencesCopyKeyList(domain as CFString, kCFPreferencesCurrentUser, kCFPreferencesAnyHost) as? [String]
    }

    /// Clears the app's preferences domain. Success needs the removal, a successful
    /// synchronise and a re-read that finds no keys left in the domain.
    static func clearPreferencesDomain(
        _ domain: String,
        remove: (String) -> Void = HubProductionEffects.removePreferencesDomain,
        synchronize: (String) -> Bool = HubProductionEffects.synchronizePreferencesDomain,
        keysLeft: (String) -> [String]? = HubProductionEffects.preferencesKeysLeft
    ) -> Bool {
        remove(domain)
        guard synchronize(domain) else { return false }
        return (keysLeft(domain) ?? []).isEmpty
    }

    static func staleImportCleaner(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        effect: @escaping () -> TeslaMateServerImporter.StaleTemporaryCleanup = {
            TeslaMateServerImporter.cleanupStaleTemporaryDirectoriesReporting()
        }
    ) -> () -> TeslaMateServerImporter.StaleTemporaryCleanup {
        {
            guard !HubTestHost.isXCTest(environment: environment) else { return .init() }
            return effect()
        }
    }

    static func appReveal(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        effect: @escaping (URL) -> Void = { NSWorkspace.shared.activateFileViewerSelecting([$0]) }
    ) -> (URL) -> Void {
        { url in
            guard !HubTestHost.isXCTest(environment: environment) else { return }
            effect(url)
        }
    }

    static func terminator(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        effect: @escaping () -> Void = { NSApp.terminate(nil) }
    ) -> () -> Void {
        {
            guard !HubTestHost.isXCTest(environment: environment) else { return }
            effect()
        }
    }

    static func terminationReply(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        effect: @escaping (Bool) -> Void = { NSApp.reply(toApplicationShouldTerminate: $0) }
    ) -> (Bool) -> Void {
        { shouldTerminate in
            guard !HubTestHost.isXCTest(environment: environment) else { return }
            effect(shouldTerminate)
        }
    }
}

/// The three preferences primitives the clearing composes: remove the domain, synchronise
/// it, and re-read what keys it still holds.
struct HubPreferencesPrimitives {
    var remove: (String) -> Void
    var synchronize: (String) -> Bool
    var keysLeft: (String) -> [String]?

    static let production = HubPreferencesPrimitives(
        remove: HubProductionEffects.removePreferencesDomain,
        synchronize: HubProductionEffects.synchronizePreferencesDomain,
        keysLeft: HubProductionEffects.preferencesKeysLeft)
}

final class HubDataRemover: HubDataRemoving {
    private let home: URL
    private let bundleIdentifier: String
    private let suspendAppLog: () -> Void
    private let cleanStaleImports: () -> TeslaMateServerImporter.StaleTemporaryCleanup
    private let clearPreferences: (String) -> Bool
    private let logWarning: (String) -> Void
    private let environment: [String: String]
    private let realHome: URL
    private let afterOpeningComponent: ((String) -> Void)?
    private let beforeRemovingDirectory: ((String) -> Void)?
    private let owner: uid_t

    init(home: URL,
         bundleIdentifier: String,
         suspendAppLog: @escaping () -> Void,
         cleanStaleImports: @escaping () -> TeslaMateServerImporter.StaleTemporaryCleanup,
         clearPreferences: @escaping (String) -> Bool,
         logWarning: @escaping (String) -> Void = { _ in },
         environment: [String: String] = ProcessInfo.processInfo.environment,
         realHome: URL = HubTestHost.defaultRealHome(),
         owner: uid_t = getuid(),
         afterOpeningComponent: ((String) -> Void)? = nil,
         beforeRemovingDirectory: ((String) -> Void)? = nil) {
        self.home = home
        self.bundleIdentifier = bundleIdentifier
        self.suspendAppLog = suspendAppLog
        self.cleanStaleImports = cleanStaleImports
        self.clearPreferences = clearPreferences
        self.logWarning = logWarning
        self.environment = environment
        self.realHome = realHome
        self.owner = owner
        self.afterOpeningComponent = afterOpeningComponent
        self.beforeRemovingDirectory = beforeRemovingDirectory
    }

    /// The production remover for this account and app: effects composed behind the
    /// XCTest guards of `HubProductionEffects`.
    ///
    /// The effects environment and the preferences primitives are injectable only so a test
    /// can reach this composition with recording primitives: by default they are the
    /// process's and the real ones.
    static func production(home: URL,
                           bundleIdentifier: String? = Bundle.main.bundleIdentifier,
                           effectsEnvironment: [String: String] = ProcessInfo.processInfo.environment,
                           preferences: HubPreferencesPrimitives = .production,
                           staleImportEffect: (() -> TeslaMateServerImporter.StaleTemporaryCleanup)? = nil,
                           suspendAppLog: @escaping () -> Void = { HubAppLog.shared.suspend() }) -> HubDataRemover {
        HubDataRemover(home: home,
                       bundleIdentifier: bundleIdentifier ?? "",
                       suspendAppLog: suspendAppLog,
                       cleanStaleImports: staleImportEffect.map {
                           HubProductionEffects.staleImportCleaner(environment: effectsEnvironment, effect: $0)
                       } ?? HubProductionEffects.staleImportCleaner(environment: effectsEnvironment),
                       clearPreferences: HubProductionEffects.preferencesClearer(
                           environment: effectsEnvironment,
                           remove: preferences.remove,
                           synchronize: preferences.synchronize,
                           keysLeft: preferences.keysLeft),
                       logWarning: { detail in
                           HubAppLog.shared.record("data_removal.warning", category: "service", level: "WARN",
                                                   fields: ["detail": detail])
                       })
    }

    // MARK: - Check

    func check() throws -> HubDataRemovalPlan {
        try guardRealHome()
        let targets = try HubDataRemovalTargets(home: home, bundleIdentifier: bundleIdentifier)
        guard let library = try openLibrary(targets) else {
            return HubDataRemovalPlan(entries: [], preferencesPresent: false)
        }
        defer { close(library.descriptor) }

        var entries: [HubDataRemovalPlan.Entry] = []
        var parents: [String: Int32] = [:]
        defer { parents.values.forEach { close($0) } }

        for root in targets.roots {
            guard let parent = try openParent(root.parent, library: library.descriptor, cache: &parents,
                                              targets: targets) else { continue }
            var entry = stat()
            guard fstatat(parent.descriptor, root.name, &entry, AT_SYMLINK_NOFOLLOW) == 0 else {
                if errno == ENOENT { continue }
                // Nothing has been deleted yet: the item could not be inspected.
                throw HubDataRemovalRefusal.item(display(targets, root), .unchecked)
            }
            try validateRoot(entry, root: root, parentDevice: parent.device, targets: targets)
            entries.append(.init(root: root, device: entry.st_dev, inode: entry.st_ino,
                                 parentDevice: parent.device, parentInode: parent.inode))

            guard root.kind == .folder else { continue }
            let descriptor: Int32
            switch openDirectoryChecked(parent.descriptor, root.name, expected: entry) {
            case let .opened(opened): descriptor = opened
            case .changed: throw HubDataRemovalRefusal.item(display(targets, root), .changedSinceCheck)
            case .unreadable: throw HubDataRemovalRefusal.item(display(targets, root), .unchecked)
            }
            defer { close(descriptor) }
            afterOpeningComponent?(targets.library.appendingPathComponent(root.parent).appendingPathComponent(root.name).path)
            if root.id == "D1" { try checkConfiguration(hubFolder: descriptor, targets: targets) }
            if let refusal = scan(descriptor, rootDevice: entry.st_dev,
                                  path: display(targets, root), depth: 0) {
                throw refusal
            }
        }
        return HubDataRemovalPlan(entries: entries,
                                  preferencesPresent: preferencesFileExists(library: library.descriptor))
    }

    /// Read-only presence of the preferences file; never opens its contents.
    private func preferencesFileExists(library: Int32) -> Bool {
        let directory = openat(library, "Preferences", O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW)
        guard directory >= 0 else { return false }
        defer { close(directory) }
        var entry = stat()
        return fstatat(directory, "\(HubDataRemovalTargets.bundleIdentifier).plist", &entry, AT_SYMLINK_NOFOLLOW) == 0
    }

    private func validateRoot(_ entry: stat, root: HubDataRemovalTargets.Root,
                              parentDevice: dev_t, targets: HubDataRemovalTargets) throws {
        if let reason = rootRefusal(entry, root: root, parentDevice: parentDevice) {
            throw HubDataRemovalRefusal.item(display(targets, root), reason)
        }
    }

    /// The admission rule for a target root (ownership, type, shared permissions of D1 and
    /// D2, same disk as its parent). Applied at the check and again before each root is
    /// acted on during deletion.
    private func rootRefusal(_ entry: stat, root: HubDataRemovalTargets.Root,
                             parentDevice: dev_t) -> HubDataRemovalReason? {
        let type = entry.st_mode & S_IFMT
        if type == S_IFLNK { return .link }
        switch root.kind {
        case .folder: guard type == S_IFDIR else { return .notFolder }
        case .file: guard type == S_IFREG else { return .notFolder }
        }
        guard entry.st_uid == owner else { return .otherOwner }
        if root.id == "D1" || root.id == "D2", entry.st_mode & 0o022 != 0 { return .shared }
        guard entry.st_dev == parentDevice else { return .otherDisk }
        return nil
    }

    // MARK: - Configuration (the data_dir comparison)

    private func checkConfiguration(hubFolder: Int32, targets: HubDataRemovalTargets) throws {
        let descriptor = openat(hubFolder, "config.toml", O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK)
        guard descriptor >= 0 else {
            if errno == ENOENT { return }
            throw HubDataRemovalRefusal.dataDirUnreadable()
        }
        defer { close(descriptor) }
        var entry = stat()
        guard fstat(descriptor, &entry) == 0, entry.st_mode & S_IFMT == S_IFREG,
              entry.st_size >= 0, entry.st_size <= 1 << 20 else { throw HubDataRemovalRefusal.dataDirUnreadable() }
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 16 * 1024)
        while true {
            let count = buffer.withUnsafeMutableBytes { read(descriptor, $0.baseAddress, $0.count) }
            if count == 0 { break }
            if count < 0 {
                if errno == EINTR { continue }
                throw HubDataRemovalRefusal.dataDirUnreadable()
            }
            guard data.count + count <= 1 << 20 else { throw HubDataRemovalRefusal.dataDirUnreadable() }
            data.append(contentsOf: buffer.prefix(count))
        }
        guard let content = String(data: data, encoding: .utf8) else { throw HubDataRemovalRefusal.dataDirUnreadable() }
        guard let value = Self.topLevelDataDir(in: content) else { throw HubDataRemovalRefusal.dataDirUnreadable() }
        guard value.hasPrefix("/") else { throw HubDataRemovalRefusal.dataDirUnreadable() }
        let normalised = HubTestHost.lexicalPath(value)
        let folder = HubTestHost.lexicalPath(targets.hubFolder.path)
        guard Self.isInside(normalised, folder: folder) else {
            throw HubDataRemovalRefusal.dataDirOutside(normalised)
        }
    }

    /// By path components, never by string prefix: `.../Teslatlas Hub Backup/data` is outside.
    static func isInside(_ path: String, folder: String) -> Bool {
        let candidate = path.split(separator: "/").map(String.init)
        let base = folder.split(separator: "/").map(String.init)
        return candidate.count >= base.count && Array(candidate.prefix(base.count)) == base
    }

    /// The value of a top-level `data_dir` line (before the first table header): a TOML
    /// basic string with the escapes `tomlBasicString` writes, or a literal string,
    /// optionally followed by a `#` comment. Anything else is unsupported (nil).
    static func topLevelDataDir(in content: String) -> String? {
        for rawLine in content.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("[") { return nil }
            guard line.hasPrefix("data_dir") else { continue }
            let rest = line.dropFirst("data_dir".count).drop(while: { $0 == " " || $0 == "\t" })
            guard rest.hasPrefix("=") else { continue }
            let value = rest.dropFirst().drop(while: { $0 == " " || $0 == "\t" })
            if value.hasPrefix("\"\"\"") || value.hasPrefix("'''") { return nil }
            if value.hasPrefix("'") {
                guard let closing = value.dropFirst().firstIndex(of: "'") else { return nil }
                let literal = String(value[value.index(after: value.startIndex)..<closing])
                return trailerIsValid(value[value.index(after: closing)...]) ? literal : nil
            }
            guard value.hasPrefix("\"") else { return nil }
            return decodeBasicString(value)
        }
        return nil
    }

    private static func trailerIsValid(_ trailer: Substring) -> Bool {
        let trimmed = trailer.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty || trimmed.hasPrefix("#")
    }

    private static func decodeBasicString(_ value: Substring) -> String? {
        var result = String.UnicodeScalarView()
        var iterator = value.unicodeScalars.dropFirst().makeIterator()
        var closed = false
        var trailer = ""
        while let scalar = iterator.next() {
            if closed { trailer.unicodeScalars.append(scalar); continue }
            switch scalar {
            case "\"": closed = true
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
                    let length = escape == "u" ? 4 : 8
                    var digits = ""
                    for _ in 0..<length {
                        guard let digit = iterator.next() else { return nil }
                        digits.unicodeScalars.append(digit)
                    }
                    guard let code = UInt32(digits, radix: 16), let decoded = Unicode.Scalar(code) else { return nil }
                    result.append(decoded)
                default: return nil
                }
            default:
                if scalar.value < 0x20 && scalar != "\t" { return nil }
                result.append(scalar)
            }
        }
        guard closed, trailerIsValid(Substring(trailer)) else { return nil }
        return String(result)
    }

    // MARK: - Delete

    func delete(_ plan: HubDataRemovalPlan) -> HubDataRemovalReport {
        var report = HubDataRemovalReport()
        guard (try? guardRealHome()) != nil,
              let targets = try? HubDataRemovalTargets(home: home, bundleIdentifier: bundleIdentifier) else {
            return retainingEverything(plan, because: HubDataRemovalRefusal.testHost().message)
        }
        let library: (descriptor: Int32, device: dev_t)?
        do { library = try openLibrary(targets) } catch {
            return retainingEverything(plan, because: (error as? HubDataRemovalRefusal)?.clause
                                        ?? error.localizedDescription)
        }
        defer { if let library { close(library.descriptor) } }
        // `Library` was there at the check and is not now (renamed or removed meanwhile):
        // nothing can be deleted, and the planned targets must not read as removed.
        if library == nil, !plan.entries.isEmpty || plan.preferencesPresent {
            return retainingEverything(
                plan,
                because: HubDataRemovalRefusal.item(display(home: targets.home, targets.library.path),
                                                    .changedSinceCheck).clause ?? "")
        }

        // Every planned root's parent folder is opened, and held, before the first unlink.
        // One that is absent or cannot be opened now fails the whole data step: no target
        // is deleted, preferences are not cleared, and every planned target is reported
        // as retained.
        var parents: [String: Int32] = [:]
        defer { parents.values.forEach { close($0) } }
        if let library {
            for planned in plan.entries {
                let opened = (try? openParent(planned.root.parent, library: library.descriptor,
                                              cache: &parents, targets: targets)) ?? nil
                // The folder must be the one that was checked, not another of the same name.
                guard let opened, opened.device == planned.parentDevice, opened.inode == planned.parentInode else {
                    let folder = targets.library.appendingPathComponent(planned.root.parent).path
                    return retainingEverything(
                        plan,
                        because: HubDataRemovalRefusal.item(display(home: targets.home, folder),
                                                            .changedSinceCheck).clause ?? "")
                }
            }
        }

        func remove(_ id: String) {
            for planned in plan.entries where planned.root.id == id {
                guard let library else { continue }
                guard let parent = (try? openParent(planned.root.parent, library: library.descriptor,
                                                    cache: &parents, targets: targets)) ?? nil else {
                    // Held since before the first unlink; kept as a defence all the same.
                    report.kept.append(.init(path: display(targets, planned.root),
                                             reason: HubDataRemovalReason.changedSinceCheck.text))
                    continue
                }
                removeRoot(planned, parent: parent, targets: targets, report: &report)
            }
        }
        remove("D1")
        suspendAppLog()
        remove("D2")
        remove("D3")
        remove("D4")
        remove("D5")
        remove("D6")
        let staleImports = cleanStaleImports()
        if staleImports.removed > 0 { report.deleted.append("D7") }
        for failure in staleImports.failures {
            report.kept.append(.init(path: failure.path, reason: failure.reason))
        }
        if let scanFailure = staleImports.scanFailure {
            // A warning: the folder that could not be listed (the system temporary folder)
            // is not something the user is asked to delete. The final message shows it, and
            // it is logged (the unified log; the app's own log file is suspended by now).
            warn(&report, HubL10n.format("hub.dataRemoval.import_scan_failed",
                                         fallback: "temporary import folders could not be checked (%1$@)",
                                         arguments: [scanFailure.reason]))
        }
        if clearPreferences(targets.preferencesDomain) {
            if plan.preferencesPresent { report.deleted.append("D8") }
        } else if plan.preferencesPresent {
            // The file is the preferences service's: kept, but never an item to delete by hand.
            report.kept.append(.init(path: display(home: targets.home, targets.preferencesFile.path),
                                     reason: HubDataRemovalReason.notCleared.text, settings: true))
        } else {
            warn(&report, Self.settingsNotClearedWarning)
        }
        return report
    }

    private static var settingsNotClearedWarning: String {
        HubL10n.text("hub.dataRemoval.settings_not_cleared", fallback: "the app's saved settings could not be cleared")
    }

    private func warn(_ report: inout HubDataRemovalReport, _ text: String) {
        report.warnings.append(text)
        logWarning(text)
    }

    /// The deletion could not start: nothing was removed, and every planned target, with the
    /// preferences, is named as retained so the report can never read as a removal.
    private func retainingEverything(_ plan: HubDataRemovalPlan, because warning: String) -> HubDataRemovalReport {
        var report = HubDataRemovalReport()
        warn(&report, warning)
        let library = home.appendingPathComponent("Library", isDirectory: true)
        for planned in plan.entries {
            let path = library.appendingPathComponent(planned.root.parent).appendingPathComponent(planned.root.name).path
            report.kept.append(.init(path: display(home: home, path), reason: HubDataRemovalReason.notDeleted.text))
        }
        if plan.preferencesPresent {
            let path = library.appendingPathComponent("Preferences/\(HubDataRemovalTargets.bundleIdentifier).plist").path
            report.kept.append(.init(path: display(home: home, path), reason: HubDataRemovalReason.notDeleted.text,
                                     settings: true))
        }
        return report
    }

    private func removeRoot(_ planned: HubDataRemovalPlan.Entry,
                            parent: OpenedFolder,
                            targets: HubDataRemovalTargets,
                            report: inout HubDataRemovalReport) {
        let path = display(targets, planned.root)
        var entry = stat()
        guard fstatat(parent.descriptor, planned.root.name, &entry, AT_SYMLINK_NOFOLLOW) == 0 else {
            // The check found it and it is not there now (moved or removed meanwhile):
            // never silently dropped, so the report cannot read as a removal.
            report.kept.append(.init(path: path, reason: HubDataRemovalReason.changedSinceCheck.text))
            return
        }
        guard entry.st_dev == planned.device, entry.st_ino == planned.inode,
              entry.st_mode & S_IFMT != S_IFLNK else {
            report.kept.append(.init(path: path, reason: HubDataRemovalReason.changedSinceCheck.text))
            return
        }
        // The same admission as at the check: ownership, type and shared permissions can
        // change without changing the inode.
        if let reason = rootRefusal(entry, root: planned.root, parentDevice: parent.device) {
            report.kept.append(.init(path: path, reason: reason.text))
            return
        }
        switch planned.root.kind {
        case .file:
            guard unlinkat(parent.descriptor, planned.root.name, 0) == 0 else {
                report.kept.append(.init(path: path, reason: Self.errnoText()))
                return
            }
            _ = fsync(parent.descriptor)
            if !report.deleted.contains(planned.root.id) { report.deleted.append(planned.root.id) }
        case .folder:
            guard let descriptor = openDirectory(parent.descriptor, planned.root.name, expected: entry) else {
                report.kept.append(.init(path: path, reason: HubDataRemovalReason.changedSinceCheck.text))
                return
            }
            defer { close(descriptor) }
            afterOpeningComponent?(targets.library.appendingPathComponent(planned.root.parent)
                .appendingPathComponent(planned.root.name).path)
            var kept: [HubDataRemovalKept] = []
            let empty = removeContents(descriptor, rootDevice: entry.st_dev, path: path, depth: 0,
                                       allowRescan: true, kept: &kept)
            report.kept.append(contentsOf: kept)
            guard empty else { return }
            beforeRemovingDirectory?(path)
            if unlinkat(parent.descriptor, planned.root.name, AT_REMOVEDIR) != 0 {
                let failure = errno
                guard failure == ENOTEMPTY else {
                    report.kept.append(.init(path: path, reason: Self.errnoText(failure)))
                    return
                }
                // A file arrived during the walk: one rescan with the same rules, then one retry.
                var rescanKept: [HubDataRemovalKept] = []
                let rescanned = removeContents(descriptor, rootDevice: entry.st_dev, path: path, depth: 0,
                                               allowRescan: false, kept: &rescanKept)
                report.kept.append(contentsOf: rescanKept)
                guard rescanned else { return }
                guard unlinkat(parent.descriptor, planned.root.name, AT_REMOVEDIR) == 0 else {
                    report.kept.append(.init(path: path, reason: Self.errnoText()))
                    return
                }
            }
            _ = fsync(parent.descriptor)
            if !report.deleted.contains(planned.root.id) { report.deleted.append(planned.root.id) }
        }
    }

    // MARK: - Walks

    /// Read-only classification of everything below a held directory: the first refusal.
    private func scan(_ directory: Int32, rootDevice: dev_t, path: String, depth: Int) -> HubDataRemovalRefusal? {
        guard depth <= HubDataRemovalMarkers.maximumDepth else { return .item(path, .tooDeep) }
        guard let names = Self.entryNames(directory) else { return .item(path, .unchecked) }
        if !HubDataRemovalMarkers.backupNames.isDisjoint(with: names) { return .item(path, .backup) }
        for name in names {
            let child = path + "/" + name
            var entry = stat()
            guard fstatat(directory, name, &entry, AT_SYMLINK_NOFOLLOW) == 0 else {
                // Only a confirmed absence is an absence; anything else could hide a child.
                if errno == ENOENT { continue }
                return .item(child, .unchecked)
            }
            if case let .refuse(reason) = classify(directory, name: name, entry: entry, rootDevice: rootDevice) {
                return .item(child, reason)
            }
            if entry.st_mode & S_IFMT == S_IFDIR {
                let descriptor: Int32
                switch openDirectoryChecked(directory, name, expected: entry) {
                case let .opened(opened): descriptor = opened
                case .changed: return .item(child, .changedSinceCheck)
                case .unreadable: return .item(child, .unchecked)
                }
                defer { close(descriptor) }
                afterOpeningComponent?(absolute(child))
                if let refusal = scan(descriptor, rootDevice: rootDevice, path: child, depth: depth + 1) {
                    return refusal
                }
            }
        }
        return nil
    }

    /// Policy plus the content checks (recovery export magic). A file that cannot be read
    /// to check is refused, never classified as ordinary data.
    private func classify(_ directory: Int32, name: String, entry: stat, rootDevice: dev_t) -> HubDataRemovalVerdict {
        let verdict = HubDataRemovalPolicy.admit(name: name, entry: entry, rootDevice: rootDevice, owner: owner)
        guard verdict == .admit else { return verdict }
        if entry.st_mode & S_IFMT == S_IFREG,
           entry.st_size >= off_t(HubDataRemovalMarkers.recoveryMinimumBytes),
           entry.st_size <= off_t(HubDataRemovalMarkers.recoveryMaximumBytes) {
            switch recoveryMagic(directory, name: name, entry: entry) {
            case .match: return .refuse(.recoveryExport)
            case .unchecked: return .refuse(.unchecked)
            case .noMatch: break
            }
        }
        return .admit
    }

    private enum RecoveryMagic { case match, noMatch, unchecked }

    /// `noMatch` only for content that was read and differs (or that is confirmed gone);
    /// any open, identity or read failure is `unchecked`.
    private func recoveryMagic(_ directory: Int32, name: String, entry: stat) -> RecoveryMagic {
        let descriptor = openat(directory, name, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK)
        guard descriptor >= 0 else { return errno == ENOENT ? .noMatch : .unchecked }
        defer { close(descriptor) }
        var opened = stat()
        guard fstat(descriptor, &opened) == 0, opened.st_dev == entry.st_dev, opened.st_ino == entry.st_ino,
              opened.st_mode & S_IFMT == S_IFREG else { return .unchecked }
        var bytes = [UInt8](repeating: 0, count: HubDataRemovalMarkers.recoveryMagic.count)
        var offset = 0
        while offset < bytes.count {
            let count = bytes.withUnsafeMutableBytes {
                read(descriptor, $0.baseAddress!.advanced(by: offset), $0.count - offset)
            }
            if count < 0 {
                if errno == EINTR { continue }
                return .unchecked
            }
            // End of file before the marker is complete: the content was read and differs.
            if count == 0 { return .noMatch }
            offset += count
        }
        return bytes == HubDataRemovalMarkers.recoveryMagic ? .match : .noMatch
    }

    /// Post-order deletion of everything below a held directory. Returns true when the
    /// directory is empty afterwards. Entries that fail their re-check or their unlink are
    /// kept, with their parents, and the walk continues with the rest.
    private func removeContents(_ directory: Int32, rootDevice: dev_t, path: String, depth: Int,
                                allowRescan: Bool, kept: inout [HubDataRemovalKept]) -> Bool {
        guard depth <= HubDataRemovalMarkers.maximumDepth else {
            kept.append(.init(path: path, reason: HubDataRemovalReason.tooDeep.text))
            return false
        }
        guard let names = Self.entryNames(directory) else {
            kept.append(.init(path: path, reason: HubDataRemovalReason.changedSinceCheck.text))
            return false
        }
        if !HubDataRemovalMarkers.backupNames.isDisjoint(with: names) {
            kept.append(.init(path: path, reason: HubDataRemovalReason.backup.text))
            return false
        }
        var empty = true
        for name in names.sorted() {
            let child = path + "/" + name
            var entry = stat()
            guard fstatat(directory, name, &entry, AT_SYMLINK_NOFOLLOW) == 0 else {
                if errno != ENOENT {
                    kept.append(.init(path: child, reason: HubDataRemovalReason.unchecked.text))
                    empty = false
                }
                continue
            }
            if case let .refuse(reason) = classify(directory, name: name, entry: entry, rootDevice: rootDevice) {
                kept.append(.init(path: child, reason: reason.text))
                empty = false
                continue
            }
            if entry.st_mode & S_IFMT == S_IFDIR {
                guard let descriptor = openDirectory(directory, name, expected: entry) else {
                    kept.append(.init(path: child, reason: HubDataRemovalReason.changedSinceCheck.text))
                    empty = false
                    continue
                }
                afterOpeningComponent?(absolute(child))
                var childKept: [HubDataRemovalKept] = []
                var childEmpty = removeContents(descriptor, rootDevice: rootDevice, path: child, depth: depth + 1,
                                                allowRescan: true, kept: &childKept)
                var removed = false
                if childEmpty {
                    beforeRemovingDirectory?(child)
                    var failure: Int32 = 0
                    if unlinkat(directory, name, AT_REMOVEDIR) == 0 {
                        removed = true
                    } else {
                        failure = errno
                        if failure == ENOTEMPTY, allowRescan {
                            // One rescan with the same rules, then one retry.
                            childEmpty = removeContents(descriptor, rootDevice: rootDevice, path: child,
                                                        depth: depth + 1, allowRescan: false, kept: &childKept)
                            if childEmpty {
                                if unlinkat(directory, name, AT_REMOVEDIR) == 0 {
                                    removed = true
                                } else {
                                    failure = errno
                                }
                            } else {
                                // The rescan kept its own entries; no unlink ran after it.
                                failure = 0
                            }
                        }
                    }
                    if !removed, failure != 0 {
                        childKept.append(.init(path: child, reason: Self.errnoText(failure)))
                    }
                }
                close(descriptor)
                kept.append(contentsOf: childKept)
                if !removed { empty = false }
            } else {
                if unlinkat(directory, name, 0) != 0, errno != ENOENT {
                    kept.append(.init(path: child, reason: Self.errnoText()))
                    empty = false
                }
            }
        }
        return empty
    }

    // MARK: - Descriptors

    private func guardRealHome() throws {
        if HubTestHost.refusesHome(home, realHome: realHome, environment: environment) {
            throw HubDataRemovalRefusal.testHost()
        }
    }

    /// `<home>/Library` by the descriptor walk from `/`: no link, ancestors owned by
    /// root or the user and not world-writable, a leaf owned by the user and not
    /// writable by others. nil when `Library` does not exist (an empty home).
    private func openLibrary(_ targets: HubDataRemovalTargets) throws -> (descriptor: Int32, device: dev_t)? {
        var entry = stat()
        guard lstat(targets.library.path, &entry) == 0 else {
            // Library is absent only when the system says so; any other failure could hide it.
            if errno == ENOENT { return nil }
            throw HubDataRemovalRefusal.item(display(home: targets.home, targets.library.path), .unchecked)
        }
        // `Library` is there (lstat found it). What it is and who owns it are said as
        // they are: a link, something that is not a folder, a folder of another user, a
        // folder others can write to. Only a failure to open or inspect it, or one of its
        // ancestors, could not be checked.
        let shown = display(home: targets.home, targets.library.path)
        let type = entry.st_mode & S_IFMT
        if type == S_IFLNK { throw HubDataRemovalRefusal.item(shown, .link) }
        if type != S_IFDIR { throw HubDataRemovalRefusal.item(shown, .notFolder) }
        if entry.st_uid != owner { throw HubDataRemovalRefusal.item(shown, .otherOwner) }
        if entry.st_mode & 0o022 != 0 { throw HubDataRemovalRefusal.item(shown, .shared) }
        let descriptor: Int32
        do {
            descriptor = try DevelopmentEventLog.validateDirectoryReadOnly(
                targets.library,
                allowOwnedGroupWritableAncestors: true,
                policy: .launchAgentsLeaf,
                afterOpeningComponent: afterOpeningComponent)
        } catch {
            throw HubDataRemovalRefusal.item(shown, .unchecked)
        }
        var opened = stat()
        guard fstat(descriptor, &opened) == 0 else {
            close(descriptor)
            throw HubDataRemovalRefusal.item(display(home: targets.home, targets.library.path), .unchecked)
        }
        return (descriptor, opened.st_dev)
    }

    private typealias OpenedFolder = (descriptor: Int32, device: dev_t, inode: ino_t)

    /// A folder under `Library`, owned by the user or root. Absent: nil. Cached so that
    /// two roots with one parent share a descriptor; closed by the caller.
    private func openParent(_ name: String, library: Int32, cache: inout [String: Int32],
                            targets: HubDataRemovalTargets) throws -> OpenedFolder? {
        if let cached = cache[name] {
            var entry = stat()
            _ = fstat(cached, &entry)
            return (cached, entry.st_dev, entry.st_ino)
        }
        let descriptor = openat(library, name, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW)
        guard descriptor >= 0 else {
            let failure = errno
            if failure == ENOENT { return nil }
            // A link is said to be one, a plain file to be no folder; a folder that cannot be
            // opened or inspected (permissions, I/O) could not be checked.
            let reason: HubDataRemovalReason
            switch failure {
            case ELOOP, ENOTDIR:
                // macOS reports a link and a plain file alike; the name itself tells them apart.
                var named = stat()
                if fstatat(library, name, &named, AT_SYMLINK_NOFOLLOW) != 0 { reason = .unchecked }
                else if named.st_mode & S_IFMT == S_IFLNK { reason = .link }
                else { reason = .notFolder }
            default: reason = .unchecked
            }
            throw HubDataRemovalRefusal.item(display(home: targets.home, targets.library.appendingPathComponent(name).path),
                                             reason)
        }
        var entry = stat()
        guard fstat(descriptor, &entry) == 0 else {
            close(descriptor)
            throw HubDataRemovalRefusal.item(display(home: targets.home, targets.library.appendingPathComponent(name).path),
                                             .unchecked)
        }
        guard entry.st_mode & S_IFMT == S_IFDIR, entry.st_uid == owner || entry.st_uid == 0 else {
            close(descriptor)
            throw HubDataRemovalRefusal.item(display(home: targets.home, targets.library.appendingPathComponent(name).path),
                                             .otherOwner)
        }
        afterOpeningComponent?(targets.library.appendingPathComponent(name).path)
        cache[name] = descriptor
        return (descriptor, entry.st_dev, entry.st_ino)
    }

    private enum DirectoryOpen {
        case opened(Int32)
        /// The name no longer is the directory that was classified (gone, swapped or replaced).
        case changed
        /// The directory exists but cannot be opened or inspected (permissions, I/O).
        case unreadable
    }

    /// `openat(O_DIRECTORY|O_NOFOLLOW)` whose result must equal the `fstatat` that
    /// classified it (device and inode). A change of identity and an inspection failure
    /// are told apart, so the check can say "could not be checked" for the second.
    private func openDirectoryChecked(_ parent: Int32, _ name: String, expected: stat) -> DirectoryOpen {
        let descriptor = openat(parent, name, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW)
        guard descriptor >= 0 else {
            switch errno {
            case ENOENT, ELOOP, ENOTDIR: return .changed
            default: return .unreadable
            }
        }
        var opened = stat()
        guard fstat(descriptor, &opened) == 0 else {
            close(descriptor)
            return .unreadable
        }
        guard opened.st_dev == expected.st_dev, opened.st_ino == expected.st_ino else {
            close(descriptor)
            return .changed
        }
        return .opened(descriptor)
    }

    /// The same open for the deletion walk, where any failure is "changed since the check".
    private func openDirectory(_ parent: Int32, _ name: String, expected: stat) -> Int32? {
        if case let .opened(descriptor) = openDirectoryChecked(parent, name, expected: expected) { return descriptor }
        return nil
    }

    /// Entry names through `fdopendir` on a duplicate, so the held descriptor stays usable.
    private static func entryNames(_ directory: Int32) -> Set<String>? {
        let duplicate = dup(directory)
        guard duplicate >= 0, let handle = fdopendir(duplicate) else {
            if duplicate >= 0 { close(duplicate) }
            return nil
        }
        defer { closedir(handle) }
        rewinddir(handle)
        var names = Set<String>()
        while let item = readdir(handle) {
            let name = withUnsafePointer(to: &item.pointee.d_name) {
                $0.withMemoryRebound(to: CChar.self, capacity: Int(NAME_MAX) + 1) { String(cString: $0) }
            }
            if name != "." && name != ".." { names.insert(name) }
        }
        return names
    }

    private func display(_ targets: HubDataRemovalTargets, _ root: HubDataRemovalTargets.Root) -> String {
        display(home: targets.home,
                targets.library.appendingPathComponent(root.parent).appendingPathComponent(root.name).path)
    }

    /// Hooks receive absolute paths; the report and messages show `~`.
    private func absolute(_ displayed: String) -> String {
        displayed.hasPrefix("~") ? home.path + displayed.dropFirst() : displayed
    }

    private func display(home: URL, _ path: String) -> String {
        path.hasPrefix(home.path) ? "~" + path.dropFirst(home.path.count) : path
    }

    private static func errnoText(_ code: Int32 = errno) -> String {
        String(cString: strerror(code))
    }
}

/// Marker for the initializer default "build the production remover"; never called.
final class HubAutomaticDataRemover: HubDataRemoving {
    func check() throws -> HubDataRemovalPlan { throw HubActionError.preview }
    func delete(_ plan: HubDataRemovalPlan) -> HubDataRemovalReport { HubDataRemovalReport() }
}
