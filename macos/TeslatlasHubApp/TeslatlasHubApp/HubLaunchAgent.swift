// SPDX-License-Identifier: AGPL-3.0-only

import Darwin
import Foundation

/// The per-user LaunchAgent of the Hub: one label, one property list, written by
/// the app and never by an installer. Constants derive from `exitTimeOut` (D-E14);
/// a changed timeout needs a reviewed amendment, not a silent edit.
enum HubLaunchAgentContract {
    static let label = "com.teslatlas.hub"        // must equal LABEL in macos_launch_agent.rs
    static let plistName = "com.teslatlas.hub.plist"
    static let identityVariable = "TESLATLAS_HUB_EXECUTABLE_IDENTITY"
    static let throttleInterval = 30, exitTimeOut = 270, umask = 63
    static let unloadPollDeadline: TimeInterval = TimeInterval(exitTimeOut + 30)
    static let compactAbove = 1 << 20, compactKeep = 512 << 10
    static let launchctlRunLimit: TimeInterval = 30
    /// The initial `print`, `enable`, `bootout` and the full unload poll.
    static let replaceStopShare: TimeInterval = unloadPollDeadline + 3 * launchctlRunLimit
    /// At most four launchctl runs plus the existing 30-second preflight.
    static let replaceActionStartShare: TimeInterval = 5 * launchctlRunLimit
    /// `print`, `enable`, `bootstrap`, final `print` on the unloaded branch.
    static let replaceRecoveryShare: TimeInterval = 4 * launchctlRunLimit
    static let replaceStartShare: TimeInterval = replaceActionStartShare + replaceRecoveryShare
    static let replaceServiceBudget: TimeInterval = replaceStopShare + replaceStartShare
}

struct HubLaunchAgentConfiguration: Equatable {
    let home: URL
    let binary: URL
    let config: URL
    let ownerUID: uid_t
    /// `<CFBundleShortVersionString>+<CFBundleVersion>`.
    let bundleVersion: String

    var launchAgentsDirectory: URL {
        home.appendingPathComponent("Library/LaunchAgents", isDirectory: true)
    }
    var plist: URL { launchAgentsDirectory.appendingPathComponent(HubLaunchAgentContract.plistName) }
    var logDirectory: URL {
        home.appendingPathComponent("Library/Logs/Teslatlas Hub", isDirectory: true)
    }
    var standardOutputLog: URL { logDirectory.appendingPathComponent("hub.out.log") }
    var standardErrorLog: URL { logDirectory.appendingPathComponent("hub.err.log") }

    static func production(bundle: Bundle = .main, home: URL) throws -> Self {
        guard let binary = bundle.url(forResource: "teslatlas-hub", withExtension: nil) else {
            throw HubActionError.missingResource("teslatlas-hub")
        }
        let short = bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "development"
        let build = bundle.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "development"
        return Self(
            home: home,
            binary: binary,
            config: home.appendingPathComponent("Library/Application Support/Teslatlas Hub/config.toml"),
            ownerUID: getuid(),
            bundleVersion: "\(short)+\(build)"
        )
    }

    func propertyList(identity: String) -> [String: Any] {
        [
            "Label": HubLaunchAgentContract.label,
            "ProgramArguments": [binary.path, "--config", config.path, "serve"],
            "EnvironmentVariables": [HubLaunchAgentContract.identityVariable: identity],
            "StandardOutPath": standardOutputLog.path,
            "StandardErrorPath": standardErrorLog.path,
            "RunAtLoad": true,
            "KeepAlive": ["SuccessfulExit": false],
            "ProcessType": "Background",
            "Umask": HubLaunchAgentContract.umask,
            "ThrottleInterval": HubLaunchAgentContract.throttleInterval,
            "ExitTimeOut": HubLaunchAgentContract.exitTimeOut
        ]
    }
}

enum HubLaunchAgentChange: Equatable {
    case unchanged
    case created
    case replaced
}

protocol HubLaunchAgentManaging: AnyObject {
    /// A regular, single-link, owner file that parses with `Label` equal to the contract label.
    var isInstalled: Bool { get }
    func ensureInstalled(requireConfig: Bool) throws -> HubLaunchAgentChange
    func prepareLogsForBootstrap() throws
    /// Read-only: the checks of `removeInstalled` without the unlink (Delete Hub and Data).
    func validateRemovable() throws
    /// Unlinks the plist only; never touches data, logs or the app.
    func removeInstalled() throws
}

/// Pure admission rules for the bundle path, testable with fabricated `stat` values.
enum HubLaunchAgentPathPolicy {
    /// Above the bundle only "not world-writable" (so `/Applications`, root:admin 0775,
    /// passes); from the bundle root to `Contents/Resources`, owned by root or the user
    /// and not writable by group or others.
    static func admitsBundleAncestor(_ entry: stat, ownerUID: uid_t, insideBundle: Bool) -> Bool {
        guard entry.st_mode & S_IFMT == S_IFDIR, entry.st_mode & 0o002 == 0 else { return false }
        guard insideBundle else { return true }
        return (entry.st_uid == 0 || entry.st_uid == ownerUID) && entry.st_mode & 0o020 == 0
    }

    static func admitsBinary(_ entry: stat, ownerUID: uid_t) -> Bool {
        entry.st_mode & S_IFMT == S_IFREG
            && (entry.st_uid == 0 || entry.st_uid == ownerUID)
            && entry.st_mode & 0o022 == 0
            && entry.st_mode & 0o100 != 0
    }

    /// String-only: an App Translocation path or an external volume.
    static func isUnstableLocation(_ path: String) -> Bool {
        path.contains("/AppTranslocation/") || path.hasPrefix("/Volumes/")
    }
}

final class HubLaunchAgentWriter: HubLaunchAgentManaging {
    private let configuration: () throws -> HubLaunchAgentConfiguration
    private let environment: [String: String]
    private let realHome: URL
    private let afterOpeningComponent: ((String) -> Void)?
    private let fileHooks: HubFileSystemHooks
    private let temporaryName: () -> String

    init(configuration: @escaping () throws -> HubLaunchAgentConfiguration,
         environment: [String: String] = ProcessInfo.processInfo.environment,
         realHome: URL = HubTestHost.defaultRealHome(),
         afterOpeningComponent: ((String) -> Void)? = nil,
         fileHooks: HubFileSystemHooks = .system,
         temporaryName: @escaping () -> String = {
             ".\(HubLaunchAgentContract.plistName).\(UUID().uuidString).tmp"
         }) {
        self.configuration = configuration
        self.environment = environment
        self.realHome = realHome
        self.afterOpeningComponent = afterOpeningComponent
        self.fileHooks = fileHooks
        self.temporaryName = temporaryName
    }

    // MARK: - Identity

    /// `<short>+<build>:<st_ino>:<st_size>:<mtime sec>.<mtime nsec>`, nine digits of
    /// nanoseconds, no device (which can change across boots).
    static func identity(bundleVersion: String, entry: stat) -> String {
        "\(bundleVersion):\(entry.st_ino):\(entry.st_size):\(entry.st_mtimespec.tv_sec)."
            + String(format: "%09ld", entry.st_mtimespec.tv_nsec)
    }

    // MARK: - HubLaunchAgentManaging

    var isInstalled: Bool {
        guard let configuration = try? guardedConfiguration(),
              let directory = (try? openLaunchAgents(configuration, create: false)) ?? nil else { return false }
        defer { close(directory) }
        guard let existing = try? inspectExistingPlist(directory: directory, configuration: configuration,
                                                       requireLabel: true),
              existing != nil else { return false }
        return true
    }

    func ensureInstalled(requireConfig: Bool) throws -> HubLaunchAgentChange {
        let configuration = try guardedConfiguration()
        let binary = try validateBundleAndBinary(configuration)
        if requireConfig { try validateConfigurationFile(configuration) }
        guard let directory = try openLaunchAgents(configuration, create: true) else {
            throw Self.failure("launch_agents_unavailable", "The LaunchAgents folder is not available.")
        }
        defer { close(directory) }
        let desired = configuration.propertyList(
            identity: Self.identity(bundleVersion: configuration.bundleVersion, entry: binary)
        )
        let existing = try inspectExistingPlist(directory: directory, configuration: configuration,
                                                requireLabel: false)
        if let existing, existing.mode == 0o600, let value = existing.value,
           Self.plistValuesEqual(desired, value) {
            return .unchanged
        }
        let data = try PropertyListSerialization.data(fromPropertyList: desired, format: .xml, options: 0)
        try HubAtomicFile.replace(data,
                                  inDirectory: directory,
                                  named: HubLaunchAgentContract.plistName,
                                  temporaryName: temporaryName,
                                  durable: true,
                                  hooks: fileHooks)
        return existing == nil ? .created : .replaced
    }

    func prepareLogsForBootstrap() throws {
        let configuration = try guardedConfiguration()
        let library = try openLibrary(configuration)
        defer { close(library) }
        let logs = try openOwnedSubdirectory(library, "Logs", configuration: configuration,
                                             tighten: false, create: true)
        defer { close(logs) }
        let hub = try openOwnedSubdirectory(logs, "Teslatlas Hub", configuration: configuration,
                                            tighten: true, create: true)
        defer { close(hub) }
        for name in ["hub.out.log", "hub.err.log"] {
            do {
                try HubPrivateLogFile.prepare(
                    directory: hub, name: name, owner: configuration.ownerUID,
                    compaction: (HubLaunchAgentContract.compactAbove, HubLaunchAgentContract.compactKeep)
                )
            } catch let failure as HubPrivateLogFailure {
                throw Self.logFailure(failure, path: configuration.logDirectory.appendingPathComponent(name).path)
            }
        }
    }

    func validateRemovable() throws {
        if let (directory, _) = try removableDirectory() { close(directory) }
    }

    func removeInstalled() throws {
        guard let (directory, _) = try removableDirectory() else { return }
        defer { close(directory) }
        guard unlinkat(directory, HubLaunchAgentContract.plistName, 0) == 0 else {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        guard fsync(directory) == 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
    }

    // MARK: - Removal checks

    /// The held LaunchAgents descriptor and its configuration, or nil when there is
    /// nothing to remove (absent folder or absent plist). The plist must be the Hub's own.
    private func removableDirectory() throws -> (Int32, HubLaunchAgentConfiguration)? {
        let configuration = try guardedConfiguration()
        guard let directory = try openLaunchAgents(configuration, create: false) else { return nil }
        do {
            guard try inspectExistingPlist(directory: directory, configuration: configuration,
                                           requireLabel: true) != nil else {
                close(directory)
                return nil
            }
        } catch {
            close(directory)
            throw error
        }
        // validateRemovable closes its own copy; removeInstalled keeps and closes it.
        return (directory, configuration)
    }

    // MARK: - Guard

    /// D-E16: under XCTest a writer aimed at the real home refuses before any file-system call.
    private func guardedConfiguration() throws -> HubLaunchAgentConfiguration {
        let configuration = try self.configuration()
        if HubTestHost.refusesHome(configuration.home, realHome: realHome, environment: environment) {
            throw Self.failure("test_host_refused",
                               "Hub service files are not changed by a test host in the real home folder.")
        }
        return configuration
    }

    // MARK: - Bundle and binary

    private func validateBundleAndBinary(_ configuration: HubLaunchAgentConfiguration) throws -> stat {
        let path = configuration.binary.path
        guard path.hasPrefix("/"), HubTestHost.lexicalPath(path) == path,
              path.hasSuffix(".app/Contents/Resources/teslatlas-hub") else {
            throw Self.failure("binary_location",
                               "The Hub program is not inside a Teslatlas Hub app. Move Teslatlas Hub to the Applications folder and open it again.")
        }
        if HubLaunchAgentPathPolicy.isUnstableLocation(path) {
            throw Self.moveToApplications()
        }
        let components = path.split(separator: "/").map(String.init)
        let bundleIndex = components.count - 4
        var descriptor = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW)
        guard descriptor >= 0 else { throw Self.unsafeBinary() }
        defer { close(descriptor) }
        for (index, component) in components.dropLast().enumerated() {
            let next = openat(descriptor, component, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW)
            guard next >= 0 else { throw Self.unsafeBinary() }
            close(descriptor)
            descriptor = next
            var entry = stat()
            guard fstat(descriptor, &entry) == 0,
                  HubLaunchAgentPathPolicy.admitsBundleAncestor(entry, ownerUID: configuration.ownerUID,
                                                                insideBundle: index >= bundleIndex) else {
                throw Self.unsafeBinary()
            }
        }
        var binary = stat()
        guard fstatat(descriptor, components[components.count - 1], &binary, AT_SYMLINK_NOFOLLOW) == 0,
              HubLaunchAgentPathPolicy.admitsBinary(binary, ownerUID: configuration.ownerUID) else {
            throw Self.unsafeBinary()
        }
        return binary
    }

    /// The configuration is a regular owner-only file below safe ancestors (the
    /// development `.privateFile` rule).
    private func validateConfigurationFile(_ configuration: HubLaunchAgentConfiguration) throws {
        let path = configuration.config.path
        guard path.hasPrefix("/"), HubTestHost.lexicalPath(path) == path else { throw Self.unsafeConfiguration() }
        let components = path.split(separator: "/").map(String.init)
        guard let leaf = components.last else { throw Self.unsafeConfiguration() }
        var descriptor = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW)
        guard descriptor >= 0 else { throw Self.unsafeConfiguration() }
        defer { close(descriptor) }
        for component in components.dropLast() {
            let next = openat(descriptor, component, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW)
            guard next >= 0 else { throw Self.unsafeConfiguration() }
            close(descriptor)
            descriptor = next
            var entry = stat()
            guard fstat(descriptor, &entry) == 0,
                  DevelopmentEventLog.admitsDirectoryEntry(entry, leaf: false,
                                                           allowOwnedGroupWritableAncestors: true) else {
                throw Self.unsafeConfiguration()
            }
        }
        var file = stat()
        guard fstatat(descriptor, leaf, &file, AT_SYMLINK_NOFOLLOW) == 0,
              file.st_mode & S_IFMT == S_IFREG,
              file.st_uid == configuration.ownerUID,
              file.st_mode & 0o077 == 0 else { throw Self.unsafeConfiguration() }
    }

    // MARK: - Directories

    /// `<home>/Library` through the descriptor walk from `/` (no link, safe ancestors,
    /// a leaf owned by the user and not writable by group or others).
    private func openLibrary(_ configuration: HubLaunchAgentConfiguration) throws -> Int32 {
        let library = configuration.home.appendingPathComponent("Library", isDirectory: true)
        do {
            return try DevelopmentEventLog.validateDirectoryReadOnly(
                library,
                allowOwnedGroupWritableAncestors: true,
                policy: .launchAgentsLeaf,
                afterOpeningComponent: afterOpeningComponent
            )
        } catch {
            throw Self.failure("library_unsafe", "The Library folder is not safe to use.")
        }
    }

    private func libraryExists(_ configuration: HubLaunchAgentConfiguration) -> Bool {
        var entry = stat()
        return lstat(configuration.home.appendingPathComponent("Library").path, &entry) == 0
    }

    /// `~/Library/LaunchAgents`: created 0700 when missing and `create` is set, never
    /// chmod-ed when it exists. nil when absent and not created.
    private func openLaunchAgents(_ configuration: HubLaunchAgentConfiguration, create: Bool) throws -> Int32? {
        guard libraryExists(configuration) else {
            if create { throw Self.failure("library_unsafe", "The Library folder is not safe to use.") }
            return nil
        }
        let library = try openLibrary(configuration)
        defer { close(library) }
        let flags = O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW
        var directory = openat(library, "LaunchAgents", flags)
        if directory < 0, errno == ENOENT {
            guard create else { return nil }
            guard mkdirat(library, "LaunchAgents", mode_t(0o700)) == 0 || errno == EEXIST else {
                throw Self.unsafeLaunchAgents()
            }
            directory = openat(library, "LaunchAgents", flags)
        }
        guard directory >= 0 else { throw Self.unsafeLaunchAgents() }
        afterOpeningComponent?(configuration.launchAgentsDirectory.path)
        var entry = stat()
        guard fstat(directory, &entry) == 0,
              DevelopmentEventLog.admitsDirectoryEntry(entry, leaf: true,
                                                       allowOwnedGroupWritableAncestors: true,
                                                       policy: .launchAgentsLeaf) else {
            close(directory)
            throw Self.unsafeLaunchAgents()
        }
        return directory
    }

    private func openOwnedSubdirectory(_ parent: Int32, _ name: String,
                                       configuration: HubLaunchAgentConfiguration,
                                       tighten: Bool, create: Bool) throws -> Int32 {
        let flags = O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW
        var directory = openat(parent, name, flags)
        if directory < 0, errno == ENOENT, create {
            guard mkdirat(parent, name, mode_t(0o700)) == 0 || errno == EEXIST else {
                throw Self.unsafeLogs()
            }
            directory = openat(parent, name, flags)
        }
        guard directory >= 0 else { throw Self.unsafeLogs() }
        var entry = stat()
        guard fstat(directory, &entry) == 0,
              entry.st_mode & S_IFMT == S_IFDIR,
              entry.st_uid == configuration.ownerUID,
              entry.st_mode & 0o022 == 0 else {
            close(directory)
            throw Self.unsafeLogs()
        }
        if tighten, entry.st_mode & 0o777 != 0o700, fchmod(directory, mode_t(0o700)) != 0 {
            close(directory)
            throw Self.unsafeLogs()
        }
        return directory
    }

    // MARK: - Existing property list

    private struct ExistingPlist {
        let mode: mode_t
        /// nil when the contents do not parse as a property list.
        let value: Any?
    }

    /// nil when the plist is absent. It must be a regular file owned by the user with
    /// one link, read through `openat(O_NOFOLLOW)` with a same-inode check; with
    /// `requireLabel` it must also name the contract label. Anything else throws and
    /// leaves the file untouched.
    private func inspectExistingPlist(directory: Int32,
                                      configuration: HubLaunchAgentConfiguration,
                                      requireLabel: Bool) throws -> ExistingPlist? {
        let name = HubLaunchAgentContract.plistName
        var entry = stat()
        guard fstatat(directory, name, &entry, AT_SYMLINK_NOFOLLOW) == 0 else {
            if errno == ENOENT { return nil }
            throw Self.unsafePlist(configuration, .notFile)
        }
        guard entry.st_mode & S_IFMT != S_IFLNK else { throw Self.unsafePlist(configuration, .link) }
        guard entry.st_mode & S_IFMT == S_IFREG else { throw Self.unsafePlist(configuration, .notFile) }
        guard entry.st_nlink == 1 else { throw Self.unsafePlist(configuration, .moreThanOneName) }
        guard entry.st_uid == configuration.ownerUID else {
            throw Self.unsafePlist(configuration, .anotherUser)
        }
        let descriptor = openat(directory, name, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK)
        guard descriptor >= 0 else { throw Self.unsafePlist(configuration, .link) }
        defer { close(descriptor) }
        var opened = stat()
        guard fstat(descriptor, &opened) == 0,
              opened.st_dev == entry.st_dev, opened.st_ino == entry.st_ino,
              opened.st_mode & S_IFMT == S_IFREG, opened.st_size >= 0, opened.st_size <= 1 << 20 else {
            throw Self.unsafePlist(configuration, .notFile)
        }
        var bytes = [UInt8](repeating: 0, count: Int(opened.st_size))
        var offset = 0
        while offset < bytes.count {
            let count = bytes.withUnsafeMutableBytes {
                read(descriptor, $0.baseAddress!.advanced(by: offset), $0.count - offset)
            }
            if count < 0, errno == EINTR { continue }
            guard count > 0 else { throw Self.unsafePlist(configuration, .notFile) }
            offset += count
        }
        let value = try? PropertyListSerialization.propertyList(from: Data(bytes), options: [], format: nil)
        if requireLabel {
            guard let dictionary = value as? [String: Any],
                  dictionary["Label"] as? String == HubLaunchAgentContract.label else {
                throw Self.unsafePlist(configuration, .anotherService)
            }
        }
        return ExistingPlist(mode: opened.st_mode & 0o777, value: value)
    }

    /// Strict structural equality: a boolean never equals a number.
    static func plistValuesEqual(_ lhs: Any, _ rhs: Any) -> Bool {
        switch (lhs, rhs) {
        case let (left as [String: Any], right as [String: Any]):
            return left.count == right.count
                && left.allSatisfy { key, value in right[key].map { plistValuesEqual(value, $0) } ?? false }
        case let (left as [Any], right as [Any]):
            return left.count == right.count && zip(left, right).allSatisfy { plistValuesEqual($0, $1) }
        case let (left as String, right as String):
            return left == right
        case let (left as NSNumber, right as NSNumber):
            let leftIsBoolean = CFGetTypeID(left) == CFBooleanGetTypeID()
            let rightIsBoolean = CFGetTypeID(right) == CFBooleanGetTypeID()
            return leftIsBoolean == rightIsBoolean && left == right
        default:
            return false
        }
    }

    // MARK: - Messages (new strings: English fallbacks, translations carried)

    private static func failure(_ key: String, _ text: String) -> HubActionError {
        .commandFailed(HubL10n.text("hub.launchAgent.\(key)", fallback: text))
    }

    private static func moveToApplications() -> HubActionError {
        failure("move_to_applications",
                "Teslatlas Hub cannot run its background service from this location. Move Teslatlas Hub to the Applications folder and open it again.")
    }

    private static func unsafeBinary() -> HubActionError {
        failure("binary_unsafe",
                "The Hub program inside the app is missing or is not safe to run. Move Teslatlas Hub to the Applications folder and open it again.")
    }

    private static func unsafeConfiguration() -> HubActionError {
        failure("configuration_unsafe", "The Hub configuration is missing or is not private to your account.")
    }

    private static func unsafeLaunchAgents() -> HubActionError {
        failure("launch_agents_unsafe", "The LaunchAgents folder is not safe to use.")
    }

    private static func unsafeLogs() -> HubActionError {
        failure("logs_unsafe", "The Hub logs folder is not safe to use.")
    }

    /// Why an existing property list is not this app's to change; each sentence is localised.
    private enum PlistReason {
        case notFile, link, moreThanOneName, anotherUser, anotherService

        var text: String {
            switch self {
            case .notFile:
                return HubL10n.text("hub.launchAgent.reason.not_a_file", fallback: "it is not a file")
            case .link:
                return HubL10n.text("hub.launchAgent.reason.link", fallback: "it is a link")
            case .moreThanOneName:
                return HubL10n.text("hub.launchAgent.reason.more_than_one_name",
                                    fallback: "it has more than one name")
            case .anotherUser:
                return HubL10n.text("hub.launchAgent.reason.another_user",
                                    fallback: "it belongs to another user")
            case .anotherService:
                return HubL10n.text("hub.launchAgent.reason.another_service",
                                    fallback: "it names another service")
            }
        }
    }

    private static func unsafePlist(_ configuration: HubLaunchAgentConfiguration, _ reason: PlistReason) -> HubActionError {
        let path = (configuration.plist.path as NSString)
            .replacingOccurrences(of: configuration.home.path, with: "~")
        return .commandFailed(
            HubL10n.format("hub.launchAgent.plist_not_ours",
                           fallback: "The background service file %1$@ was not written by Teslatlas Hub (%2$@). Nothing was changed.",
                           arguments: [String(describing: path), reason.text])
        )
    }

    private static func logFailure(_ failure: HubPrivateLogFailure, path: String) -> HubActionError {
        switch failure {
        case .cannotOpen:
            return .commandFailed(HubL10n.format("hub.launchAgent.log_open",
                                                  fallback: "A Hub log cannot be safely opened: %1$@",
                                                  arguments: [String(describing: path)]))
        case .unsafeShape, .unsupportedPermissions, .repairFailed, .identityChanged:
            return .commandFailed(HubL10n.format("hub.launchAgent.log_unsafe",
                                                  fallback: "A Hub log is not a private single-link file of this account: %1$@",
                                                  arguments: [String(describing: path)]))
        case .compactionFailed:
            return .commandFailed(HubL10n.format("hub.launchAgent.log_compact",
                                                  fallback: "A Hub log could not be shortened: %1$@",
                                                  arguments: [String(describing: path)]))
        }
    }
}

/// Marker for the initializer default "build the production writer"; never called.
final class HubAutomaticLaunchAgent: HubLaunchAgentManaging {
    var isInstalled: Bool { false }
    func ensureInstalled(requireConfig: Bool) throws -> HubLaunchAgentChange { throw HubActionError.preview }
    func prepareLogsForBootstrap() throws { throw HubActionError.preview }
    func validateRemovable() throws { throw HubActionError.preview }
    func removeInstalled() throws { throw HubActionError.preview }
}

/// The agent of a controller that has none (preview, development): nothing is installed
/// and every write is refused.
final class HubInertLaunchAgent: HubLaunchAgentManaging {
    var isInstalled: Bool { false }
    func ensureInstalled(requireConfig: Bool) throws -> HubLaunchAgentChange { throw HubActionError.preview }
    func prepareLogsForBootstrap() throws { throw HubActionError.preview }
    func validateRemovable() throws { throw HubActionError.preview }
    func removeInstalled() throws { throw HubActionError.preview }
}
