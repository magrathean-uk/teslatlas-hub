// SPDX-License-Identifier: AGPL-3.0-only

import Darwin
import Foundation
import Security
import CryptoKit

enum HistoryOnlyControl {
    private static let contents = Array("history-only-v1\n".utf8)

    static func url(for config: URL) -> URL {
        config.deletingLastPathComponent().appendingPathComponent(".history-only-control")
    }

    static func isSelected(for config: URL) throws -> Bool {
        let path = url(for: config).path
        let descriptor = Darwin.open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK)
        guard descriptor >= 0 else {
            if errno == ENOENT { return false }
            throw HubActionError.commandFailed(HubL10n.text("hub.DevelopmentHubRuntime.18.82", fallback: "History-only control file is unsafe or unavailable."))
        }
        defer { Darwin.close(descriptor) }
        var information = stat()
        guard fstat(descriptor, &information) == 0,
              information.st_mode & S_IFMT == S_IFREG,
              information.st_uid == getuid(),
              information.st_mode & 0o777 == 0o600,
              information.st_nlink == 1,
              information.st_size == off_t(contents.count) else {
            throw HubActionError.commandFailed(HubL10n.text("hub.DevelopmentHubRuntime.28.83", fallback: "History-only control file must be an owner-only regular file."))
        }
        var bytes = [UInt8](repeating: 0, count: contents.count)
        var offset = 0
        while offset < bytes.count {
            let remaining = bytes.count - offset
            let count = bytes.withUnsafeMutableBytes { buffer in
                Darwin.read(descriptor, buffer.baseAddress!.advanced(by: offset), remaining)
            }
            guard count > 0 else {
                if count < 0 && errno == EINTR { continue }
                throw HubActionError.commandFailed(HubL10n.text("hub.DevelopmentHubRuntime.39.84", fallback: "History-only control file could not be read."))
            }
            offset += count
        }
        guard bytes == contents else {
            throw HubActionError.commandFailed(HubL10n.text("hub.DevelopmentHubRuntime.44.85", fallback: "History-only control file has unexpected contents."))
        }
        return true
    }

    static func write(for config: URL) throws {
        if try isSelected(for: config) { return }
        let file = url(for: config)
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(),
                                                withIntermediateDirectories: true)
        try Data(contents).write(to: file, options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o600)],
                                              ofItemAtPath: file.path)
        guard try isSelected(for: config) else {
            throw HubActionError.commandFailed(HubL10n.text("hub.DevelopmentHubRuntime.58.86", fallback: "History-only control file was not saved."))
        }
    }
}

enum DevelopmentHubMode: String, Equatable {
    case fixture
    case standalone
    case edge
}

/// Explicit, owner-controlled source runtime used by unsigned development builds.
/// Production builds do not enter this path unless the opt-in variable is exactly `1`.
struct DevelopmentHubConfiguration: Equatable {
    static let enableVariable = "TESLATLAS_HUB_DEVELOPMENT"
    static let binaryVariable = "TESLATLAS_HUB_DEVELOPMENT_BINARY"
    static let configVariable = "TESLATLAS_HUB_DEVELOPMENT_CONFIG"
    static let stateVariable = "TESLATLAS_HUB_DEVELOPMENT_STATE_DIRECTORY"
    static let logVariable = "TESLATLAS_HUB_DEVELOPMENT_LOG_DIRECTORY"
    static let modeVariable = "TESLATLAS_HUB_DEVELOPMENT_MODE"
    static let controlVariable = "TESLATLAS_HUB_DEVELOPMENT_CONTROL_DIRECTORY"

    let binary: URL
    let config: URL
    let stateDirectory: URL
    let logDirectory: URL
    let controlDirectory: URL
    let ownerUID: uid_t
    let mode: DevelopmentHubMode

    var serviceLabel: String {
        var hash: UInt64 = 14_695_981_039_346_656_037
        for byte in "\(binary.path)\u{0}\(config.path)\u{0}\(stateDirectory.path)".utf8 {
            hash ^= UInt64(byte)
            hash = hash &* 1_099_511_628_211
        }
        return "com.teslatlas.hub.development.\(String(hash, radix: 16))"
    }

    var plist: URL {
        controlDirectory.appendingPathComponent(".\(serviceLabel).plist")
    }

    var standardOutputLog: URL { logDirectory.appendingPathComponent("hub.out.log") }
    var standardErrorLog: URL { logDirectory.appendingPathComponent("hub.err.log") }
    static func from(environment: [String: String], ownerUID: uid_t = getuid()) throws -> Self? {
        let variables = [
            enableVariable, binaryVariable, configVariable, stateVariable, logVariable, modeVariable, controlVariable
        ]
        let supplied = variables.contains { environment[$0] != nil }
        guard supplied else { return nil }
        guard environment[enableVariable] == "1" else {
            throw HubActionError.commandFailed(
                HubL10n.format("hub.DevelopmentHubRuntime.109.98", fallback: "Local development mode was not enabled. Set %1$@=1 together with all four development paths.", arguments: [String(describing: enableVariable)])
            )
        }

        func requiredPath(_ name: String) throws -> URL {
            guard let value = environment[name], !value.isEmpty, value.hasPrefix("/") else {
                throw HubActionError.commandFailed(HubL10n.format("hub.DevelopmentHubRuntime.115.99", fallback: "%1$@ must be a non-empty absolute path.", arguments: [String(describing: name)]))
            }
            let url = URL(fileURLWithPath: value).standardizedFileURL
            guard url.path == value || (value.hasSuffix("/") && url.path + "/" == value) else {
                throw HubActionError.commandFailed(HubL10n.format("hub.DevelopmentHubRuntime.119.100", fallback: "%1$@ must not contain relative path components.", arguments: [String(describing: name)]))
            }
            return url
        }

        let mode: DevelopmentHubMode
        if let rawMode = environment[modeVariable] {
            guard let parsed = DevelopmentHubMode(rawValue: rawMode) else {
                throw HubActionError.commandFailed(
                    HubL10n.format("hub.DevelopmentHubRuntime.128.101", fallback: "%1$@ must be fixture, standalone, or edge.", arguments: [String(describing: modeVariable)])
                )
            }
            mode = parsed
        } else {
            mode = .fixture
        }
        let value = Self(
            binary: try requiredPath(binaryVariable),
            config: try requiredPath(configVariable),
            stateDirectory: try requiredPath(stateVariable),
            logDirectory: try requiredPath(logVariable),
            controlDirectory: environment[controlVariable] == nil
                ? URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true).appendingPathComponent("dev/runtime", isDirectory: true)
                : try requiredPath(controlVariable),
            ownerUID: ownerUID,
            mode: mode
        )
        return value
    }

    func validate(requireConfig: Bool) throws {
        let control = try DevelopmentEventLog.validateDirectoryReadOnly(controlDirectory,
                                                                       allowOwnedGroupWritableAncestors: false)
        close(control)
        try Self.validatePath(binary, kind: .executable, ownerUID: ownerUID, allowOwnedGroupWritableAncestors: false)
        try Self.validatePath(stateDirectory, kind: .privateDirectory, ownerUID: ownerUID)
        try Self.validatePath(logDirectory, kind: .privateDirectory, ownerUID: ownerUID)
        try Self.validateAncestors(of: config, ownerUID: ownerUID, allowMissingLeaf: !requireConfig, allowOwnedGroupWritableAncestors: false)
        if Self.pathEntryExists(config) || requireConfig {
            try Self.validatePath(config, kind: .privateFile, ownerUID: ownerUID, allowOwnedGroupWritableAncestors: false)
        }
        if Self.pathEntryExists(plist) {
            try Self.validatePath(plist, kind: .privateFile, ownerUID: ownerUID)
        }
        for log in [standardOutputLog, standardErrorLog]
        where Self.pathEntryExists(log) {
            try Self.validatePath(log, kind: .privateFile, ownerUID: ownerUID)
        }
    }

    /// Prepare the two fixed launchd log targets without following links or
    /// replacing content. launchd has been observed creating absent targets
    /// as 0644 despite the plist Umask, so that one exact owner-safe shape is
    /// repaired through its held descriptor before bootstrap or kickstart.
    func preparePrivateLaunchLogs() throws {
        try Self.validatePath(logDirectory, kind: .privateDirectory, ownerUID: ownerUID)
        let directory = try DevelopmentEventLog.validateDirectoryReadOnly(logDirectory)
        defer { close(directory) }
        var directoryInformation = stat()
        guard fstat(directory, &directoryInformation) == 0,
              directoryInformation.st_mode & S_IFMT == S_IFDIR,
              directoryInformation.st_uid == ownerUID,
              directoryInformation.st_mode & 0o077 == 0 else {
            throw HubActionError.commandFailed(
                HubL10n.format("hub.DevelopmentHubRuntime.182.103", fallback: "Development log directory changed during preparation: %1$@", arguments: [String(describing: logDirectory.path)])
            )
        }

        for name in ["hub.out.log", "hub.err.log"] {
            var descriptor = openat(
                directory,
                name,
                O_RDWR | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW,
                mode_t(0o600)
            )
            if descriptor < 0, errno == EEXIST {
                descriptor = openat(
                    directory,
                    name,
                    O_RDWR | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK
                )
            }
            guard descriptor >= 0 else {
                throw HubActionError.commandFailed(
                    HubL10n.format("hub.DevelopmentHubRuntime.202.106", fallback: "Development log cannot be safely opened: %1$@", arguments: [String(describing: logDirectory.appendingPathComponent(name).path)])
                )
            }
            defer { close(descriptor) }

            var information = stat()
            guard fstat(descriptor, &information) == 0,
                  information.st_mode & S_IFMT == S_IFREG,
                  information.st_uid == ownerUID,
                  information.st_nlink == 1 else {
                throw HubActionError.commandFailed(
                    HubL10n.format("hub.DevelopmentHubRuntime.213.107", fallback: "Development log must be a current-owner regular single-link file: %1$@", arguments: [String(describing: logDirectory.appendingPathComponent(name).path)])
                )
            }
            let permissions = information.st_mode & 0o777
            guard permissions == 0o600 || permissions == 0o644 else {
                throw HubActionError.commandFailed(
                    HubL10n.format("hub.DevelopmentHubRuntime.219.108", fallback: "Development log has unsupported permissions: %1$@", arguments: [String(describing: logDirectory.appendingPathComponent(name).path)])
                )
            }
            if permissions == 0o644, fchmod(descriptor, mode_t(0o600)) != 0 {
                throw HubActionError.commandFailed(
                    HubL10n.format("hub.DevelopmentHubRuntime.224.109", fallback: "Development log permissions could not be repaired: %1$@", arguments: [String(describing: logDirectory.appendingPathComponent(name).path)])
                )
            }
            var repaired = stat()
            guard fstat(descriptor, &repaired) == 0,
                  repaired.st_dev == information.st_dev,
                  repaired.st_ino == information.st_ino,
                  repaired.st_mode & S_IFMT == S_IFREG,
                  repaired.st_uid == ownerUID,
                  repaired.st_nlink == 1,
                  repaired.st_mode & 0o777 == 0o600 else {
                throw HubActionError.commandFailed(
                    HubL10n.format("hub.DevelopmentHubRuntime.236.110", fallback: "Development log identity changed during preparation: %1$@", arguments: [String(describing: logDirectory.appendingPathComponent(name).path)])
                )
            }
        }
    }

    /// A loaded KeepAlive job must remain stoppable if its executable or log
    /// has subsequently disappeared or become unsafe. Bootout uses only the
    /// current user's launchd domain and this derived development-only label.
    func validateStopTarget() throws {
        guard ownerUID == getuid() else {
            throw HubActionError.commandFailed(
                HubL10n.text("hub.DevelopmentHubRuntime.248.111", fallback: "Development service must belong to the current user.")
            )
        }
        let paths = [binary, config, stateDirectory, logDirectory]
        guard paths.allSatisfy({ url in
            url.path.hasPrefix("/") && url.standardizedFileURL.path == url.path
        }) else {
            throw HubActionError.commandFailed(
                HubL10n.text("hub.DevelopmentHubRuntime.256.112", fallback: "Development service identity contains an unsafe path.")
            )
        }
        guard serviceLabel.hasPrefix("com.teslatlas.hub.development."),
              serviceLabel.count > "com.teslatlas.hub.development.".count,
              serviceLabel != "com.teslatlas.hub" else {
            throw HubActionError.commandFailed(HubL10n.text("hub.DevelopmentHubRuntime.262.116", fallback: "Unsafe development service label."))
        }
    }

    private static func pathEntryExists(_ url: URL) -> Bool {
        var information = stat()
        return lstat(url.path, &information) == 0
    }

    private enum PathKind {
        case executable
        case privateFile
        case privateDirectory
    }

    private static func validatePath(_ url: URL, kind: PathKind, ownerUID: uid_t,
                                     allowOwnedGroupWritableAncestors: Bool = true) throws {
        try validateAncestors(of: url, ownerUID: ownerUID, allowMissingLeaf: false,
                              allowOwnedGroupWritableAncestors: allowOwnedGroupWritableAncestors)
        var information = stat()
        guard lstat(url.path, &information) == 0 else {
            throw HubActionError.commandFailed(HubL10n.format("hub.DevelopmentHubRuntime.281.117", fallback: "Development path is unavailable: %1$@", arguments: [String(describing: url.path)]))
        }
        guard information.st_uid == ownerUID else {
            throw HubActionError.commandFailed(HubL10n.format("hub.DevelopmentHubRuntime.284.118", fallback: "Development path is not owned by the current user: %1$@", arguments: [String(describing: url.path)]))
        }
        let type = information.st_mode & S_IFMT
        switch kind {
        case .executable:
            guard type == S_IFREG, information.st_mode & 0o022 == 0,
                  FileManager.default.isExecutableFile(atPath: url.path) else {
                throw HubActionError.commandFailed(
                    HubL10n.format("hub.DevelopmentHubRuntime.292.119", fallback: "Development binary must be a regular executable not writable by group or others: %1$@", arguments: [String(describing: url.path)])
                )
            }
        case .privateFile:
            guard type == S_IFREG, information.st_mode & 0o077 == 0 else {
                throw HubActionError.commandFailed(
                    HubL10n.format("hub.DevelopmentHubRuntime.298.120", fallback: "Development file must be regular and accessible only to its owner: %1$@", arguments: [String(describing: url.path)])
                )
            }
        case .privateDirectory:
            guard type == S_IFDIR, information.st_mode & 0o077 == 0 else {
                throw HubActionError.commandFailed(
                    HubL10n.format("hub.DevelopmentHubRuntime.304.121", fallback: "Development directory must be owned and accessible only by its owner: %1$@", arguments: [String(describing: url.path)])
                )
            }
        }
    }

    private static func validateAncestors(of url: URL,
                                          ownerUID: uid_t,
                                          allowMissingLeaf: Bool,
                                          allowOwnedGroupWritableAncestors: Bool = true) throws {
        let components = url.standardizedFileURL.pathComponents
        var path = "/"
        for (index, component) in components.dropFirst().enumerated() {
            path = (path as NSString).appendingPathComponent(component)
            var information = stat()
            if lstat(path, &information) != 0 {
                if allowMissingLeaf && index == components.count - 2 && errno == ENOENT { return }
                throw HubActionError.commandFailed(HubL10n.format("hub.DevelopmentHubRuntime.281.117", fallback: "Development path is unavailable: %1$@", arguments: [String(describing: path)]))
            }
            guard information.st_mode & S_IFMT != S_IFLNK else {
                throw HubActionError.commandFailed(HubL10n.format("hub.DevelopmentHubRuntime.323.123", fallback: "Development paths must not contain symbolic links: %1$@", arguments: [String(describing: path)]))
            }
            if index < components.count - 2 {
                guard information.st_mode & S_IFMT == S_IFDIR else {
                    throw HubActionError.commandFailed(HubL10n.format("hub.DevelopmentHubRuntime.327.124", fallback: "Development path parent is not a directory: %1$@", arguments: [String(describing: path)]))
                }
                guard information.st_uid == 0 || information.st_uid == ownerUID else {
                    throw HubActionError.commandFailed(HubL10n.format("hub.DevelopmentHubRuntime.330.125", fallback: "Development path parent has an unexpected owner: %1$@", arguments: [String(describing: path)]))
                }
                guard information.st_mode & 0o002 == 0,
                      information.st_mode & 0o020 == 0 || (allowOwnedGroupWritableAncestors && information.st_uid == ownerUID && information.st_gid == getgid()) else {
                    throw HubActionError.commandFailed(HubL10n.format("hub.DevelopmentHubRuntime.333.126", fallback: "Development path parent is writable by group or others: %1$@", arguments: [String(describing: path)]))
                }
            }
        }
    }
}

/// Fail-closed replacement for every installer entry point while the control
/// app is attached to a source-run development Hub.
final class DevelopmentHubInstaller: HubInstalling {
    func install(completion: @escaping (Result<String, Error>) -> Void) {
        completion(.failure(Self.rejection))
    }

    func uninstall(deleteData: Bool,
                   completion: @escaping (Result<String, Error>) -> Void) {
        completion(.failure(Self.rejection))
    }

    private static var rejection: Error {
        HubActionError.commandFailed(
            HubL10n.text("hub.DevelopmentHubRuntime.354.127", fallback: "A source-run development Hub cannot mutate the production service installation.")
        )
    }
}

final class DevelopmentHubCommandRunner: HubCommandRunning {
    private let configuration: DevelopmentHubConfiguration

    init(configuration: DevelopmentHubConfiguration) {
        self.configuration = configuration
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, stdin: nil, onOutputLine: nil, completion: completion)
    }

    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, stdin: stdin, onOutputLine: nil, completion: completion)
    }

    func run(arguments: [String],
             onOutputLine: @escaping (String) -> Void,
             completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, stdin: nil, onOutputLine: onOutputLine, completion: completion)
    }

    private func run(arguments: [String],
                     stdin: String?,
                     onOutputLine: ((String) -> Void)?,
                     completion: @escaping (Result<String, Error>) -> Void) {
        let journal = DevelopmentEventLog.shared(directory: configuration.logDirectory)
        let action = DevelopmentEventLog.action(arguments: arguments)
        let operation = UUID(); let started = DispatchTime.now().uptimeNanoseconds
        journal.record(kind: .command, action: action, outcome: .start, operation: operation)
        let originalCompletion = completion
        let completion: (Result<String, Error>) -> Void = { result in
            journal.completion(kind: .command, action: action, operation: operation, started: started, result: result)
            originalCompletion(result)
        }
        do {
            try configuration.validate(requireConfig: arguments.contains("serve"))
        } catch {
            completion(.failure(error))
            return
        }
        HubProcessExecutor.run(
            executable: configuration.binary,
            arguments: arguments,
            stdin: stdin,
            environment: [DevelopmentHubConfiguration.logVariable: configuration.logDirectory.path],
            maximumOutputBytes: arguments.contains("doctor") ? 1024 * 1024 : HubProcessExecutor.defaultMaximumOutputBytes,
            timeout: Self.timeout(for: arguments),
            onOutputLine: onOutputLine,
            completion: completion
        )
    }

    private static func timeout(for arguments: [String]) -> TimeInterval {
        if arguments.contains("migrate") { return 24 * 60 * 60 }
        if arguments.contains("doctor") { return 15 * 60 }
        if arguments.contains("teslamate-check") || arguments.contains("setup") { return 5 * 60 }
        if arguments.contains("control") { return 45 }
        if arguments.contains("status") || arguments.contains("preflight") { return 30 }
        return HubProcessExecutor.defaultTimeout
    }
}

typealias DevelopmentHubProcessRunner = (
    URL,
    [String],
    TimeInterval,
    @escaping (Result<String, Error>) -> Void
) -> Void

typealias DevelopmentHubReadinessScheduler = (
    TimeInterval,
    @escaping () -> Void
) -> Void

typealias DevelopmentHubReadinessClock = () -> TimeInterval

/// Live serving readiness is separate from launchd ownership and offline catalogue health.
protocol HubLiveReadinessChecking {
    func checkLiveReadiness(completion: @escaping (Bool) -> Void)
}

typealias DevelopmentHubHTTPSProbe = (
    DevelopmentHubConfiguration, Int, TimeInterval, @escaping (Bool) -> Void
) -> Void

/// This endpoint is public readiness, not a pairing or credential operation. Trust
/// is limited to the selected configuration's certificate, with normal SSL policy.
final class DevelopmentHubHTTPSReadiness: NSObject, URLSessionDataDelegate {
    private static let readLock = NSLock()
    private static var readsInProgress = Set<String>()
    private let lock = NSLock()
    private var completion: ((Bool) -> Void)?
    private var session: URLSession?
    private var certificate: SecCertificate?
    private var host: String?
    private var configDigest: String?
    private var processID: Int?
    private var body = Data()
    private static let maximumBytes = 16_384

    private init(completion: @escaping (Bool) -> Void) { self.completion = completion }

    static func probe(configuration: DevelopmentHubConfiguration, processID: Int, timeout: TimeInterval,
                      completion: @escaping (Bool) -> Void) {
        probe(configuration: configuration, processID: processID, timeout: timeout,
              sessionConfiguration: .ephemeral, completion: completion)
    }

    // The configuration seam is internal and used only for deterministic transport tests.
    static func probe(configuration: DevelopmentHubConfiguration, processID: Int, timeout: TimeInterval,
                      sessionConfiguration: URLSessionConfiguration,
                      completion: @escaping (Bool) -> Void) {
        let probe = DevelopmentHubHTTPSReadiness(completion: completion)
        let boundedTimeout = min(5, max(0.1, timeout))
        readLock.lock()
        let acquired = readsInProgress.insert(configuration.config.path).inserted
        readLock.unlock()
        guard acquired else { completion(false); return }
        // Also bounds protected file reads: a volume permission prompt must not
        // hold the app-facing startup completion indefinitely.
        DispatchQueue.global(qos: .userInitiated).asyncAfter(deadline: .now() + boundedTimeout) {
            probe.finish(false)
        }
        DispatchQueue.global(qos: .userInitiated).async {
            defer {
                readLock.lock(); readsInProgress.remove(configuration.config.path); readLock.unlock()
            }
            do {
                let config = try protectedData(configuration.config)
                let target = try endpoint(config)
                // TLS material may live below the same owned, current-group
                // writable volume ancestors admitted for the state and logs.
                // The configuration and executable retain their stricter policy.
                let pem = try protectedData(target.certificate, allowOwnedGroupWritableAncestors: true)
                guard let text = String(data: pem, encoding: .utf8),
                      let start = text.range(of: "-----BEGIN CERTIFICATE-----"),
                      let end = text.range(of: "-----END CERTIFICATE-----", range: start.upperBound..<text.endIndex),
                      let der = Data(base64Encoded: String(text[start.upperBound..<end.lowerBound]), options: .ignoreUnknownCharacters),
                      let certificate = SecCertificateCreateWithData(nil, der as CFData) else {
                    probe.finish(false); return
                }
                let settings = sessionConfiguration
                settings.timeoutIntervalForRequest = boundedTimeout
                settings.timeoutIntervalForResource = boundedTimeout
                settings.urlCache = nil
                settings.httpCookieStorage = nil
                settings.urlCredentialStorage = nil
                settings.requestCachePolicy = .reloadIgnoringLocalCacheData
                probe.lock.lock()
                guard probe.completion != nil else { probe.lock.unlock(); return }
                probe.certificate = certificate
                probe.host = target.url.host
                probe.configDigest = digest(config)
                probe.processID = processID
                let session = URLSession(configuration: settings, delegate: probe, delegateQueue: nil)
                probe.session = session
                var request = URLRequest(url: target.url)
                request.httpMethod = "GET"
                request.setValue("application/json", forHTTPHeaderField: "Accept")
                let task = session.dataTask(with: request)
                probe.lock.unlock()
                task.resume()
            } catch { probe.finish(false) }
        }
    }

    /// Accept the ordinary generated [tls] scalar strings. Unsupported TOML
    /// forms fail closed; the Rust serve preflight remains the full validator.
    static func endpoint(_ data: Data) throws -> (url: URL, certificate: URL) {
        guard let text = String(data: data, encoding: .utf8) else { throw POSIXError(.EINVAL) }
        var inTLS = false
        var seenTLS = false
        var values: [String: String] = [:]
        let assignment = try NSRegularExpression(pattern: #"^\s*(public_url|certificate_path)\s*=\s*(\"(?:[^\"\\]|\\.)*\"|'[^']*')\s*(?:#.*)?$"#)
        for rawLine in text.split(whereSeparator: \.isNewline) {
            let line = String(rawLine).trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("[") {
                inTLS = line.range(of: #"^\[tls\]\s*(?:#.*)?$"#, options: .regularExpression) != nil
                if inTLS { guard !seenTLS else { throw POSIXError(.EINVAL) }; seenTLS = true }
                continue
            }
            guard inTLS, !line.isEmpty, !line.hasPrefix("#") else { continue }
            guard let match = assignment.firstMatch(in: line, range: NSRange(line.startIndex..., in: line)) else {
                // Other TLS fields (including the private key) are never read.
                if line.hasPrefix("public_url") || line.hasPrefix("certificate_path") { throw POSIXError(.EINVAL) }
                continue
            }
            let key = String(line[Range(match.range(at: 1), in: line)!])
            let quoted = String(line[Range(match.range(at: 2), in: line)!])
            guard values[key] == nil else { throw POSIXError(.EINVAL) }
            if quoted.hasPrefix("'") { values[key] = String(quoted.dropFirst().dropLast()) }
            else {
                guard let value = try JSONSerialization.jsonObject(with: Data(quoted.utf8), options: .fragmentsAllowed) as? String else { throw POSIXError(.EINVAL) }
                values[key] = value
            }
        }
        guard let publicURL = values["public_url"], var components = URLComponents(string: publicURL),
              components.scheme == "https", components.host?.isEmpty == false,
              components.user == nil, components.password == nil,
              components.query == nil, components.fragment == nil,
              components.path.isEmpty || components.path == "/",
              let certificate = values["certificate_path"], certificate.hasPrefix("/"),
              URL(fileURLWithPath: certificate).standardizedFileURL.path == certificate else { throw POSIXError(.EINVAL) }
        components.path = "/readyz"
        guard let url = components.url else { throw POSIXError(.EINVAL) }
        return (url, URL(fileURLWithPath: certificate))
    }

    static func protectedData(_ file: URL, allowOwnedGroupWritableAncestors: Bool = false) throws -> Data {
        let parent = try DevelopmentEventLog.validateDirectoryReadOnly(file.deletingLastPathComponent(),
                                                                       allowOwnedGroupWritableAncestors: allowOwnedGroupWritableAncestors)
        defer { close(parent) }
        let fd = openat(parent, file.lastPathComponent, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK)
        guard fd >= 0 else { throw POSIXError(.EPERM) }
        defer { close(fd) }
        var before = stat()
        guard fstat(fd, &before) == 0, before.st_mode & S_IFMT == S_IFREG,
              before.st_uid == getuid(), before.st_mode & 0o777 == 0o600,
              before.st_nlink == 1, before.st_size > 0, before.st_size <= 65_536 else { throw POSIXError(.EPERM) }
        var bytes = [UInt8](repeating: 0, count: Int(before.st_size))
        var offset = 0
        while offset < bytes.count {
            let count = bytes.withUnsafeMutableBytes { buffer in
                Darwin.read(fd, buffer.baseAddress!.advanced(by: offset), buffer.count - offset)
            }
            if count < 0 && errno == EINTR { continue }
            guard count > 0 else { throw POSIXError(.EIO) }
            offset += count
        }
        var after = stat()
        guard fstat(fd, &after) == 0, before.st_dev == after.st_dev, before.st_ino == after.st_ino,
              before.st_size == after.st_size, before.st_mode == after.st_mode,
              before.st_uid == after.st_uid, before.st_nlink == after.st_nlink,
              before.st_mtimespec.tv_sec == after.st_mtimespec.tv_sec,
              before.st_mtimespec.tv_nsec == after.st_mtimespec.tv_nsec else { throw POSIXError(.EPERM) }
        return Data(bytes)
    }

    static func accepts(trust: SecTrust, certificate: SecCertificate, host: String) -> Bool {
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate], let leaf = chain.first,
              SecCertificateCopyData(leaf) as Data == SecCertificateCopyData(certificate) as Data,
              SecTrustSetPolicies(trust, SecPolicyCreateSSL(true, host as CFString)) == errSecSuccess,
              SecTrustSetAnchorCertificates(trust, [certificate] as CFArray) == errSecSuccess,
              SecTrustSetAnchorCertificatesOnly(trust, true) == errSecSuccess else { return false }
        SecTrustSetNetworkFetchAllowed(trust, false)
        return SecTrustEvaluateWithError(trust, nil)
    }

    static func digest(_ bytes: Data) -> String {
        SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
    }

    static func accepts(response: URLResponse, configDigest: String, processID: Int) -> Bool {
        guard let http = response as? HTTPURLResponse, http.statusCode == 200,
              response.expectedContentLength <= Self.maximumBytes,
              let loadedDigest = http.value(forHTTPHeaderField: "x-teslatlas-native-config-sha256"),
              loadedDigest.range(of: #"^[0-9a-f]{64}$"#, options: .regularExpression) != nil,
              loadedDigest == configDigest,
              processID > 0,
              http.value(forHTTPHeaderField: "x-teslatlas-native-process-id") == String(processID) else { return false }
        return true
    }

    func urlSession(_ session: URLSession, didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust, let certificate, let host,
              Self.accepts(trust: trust, certificate: certificate, host: host) else {
            completionHandler(.cancelAuthenticationChallenge, nil); return
        }
        completionHandler(.useCredential, URLCredential(trust: trust))
    }

    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        guard let configDigest, let processID, Self.accepts(response: response, configDigest: configDigest, processID: processID) else {
            completionHandler(.cancel); finish(false); return
        }
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        guard body.count + data.count <= Self.maximumBytes else { finish(false); return }
        body.append(data)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any]
        finish(error == nil && object?["status"] as? String == "ready")
    }

    private func finish(_ ready: Bool) {
        lock.lock()
        let callback = completion; completion = nil
        let session = session; self.session = nil
        lock.unlock()
        session?.invalidateAndCancel()
        callback?(ready)
    }
}

final class DevelopmentLaunchctlServiceController: HubServiceControlling, HubLiveReadinessChecking {
    private let configuration: DevelopmentHubConfiguration
    private let processRunner: DevelopmentHubProcessRunner
    private let readinessPollInterval: TimeInterval
    private let readinessMaxAttempts: Int
    private let readinessTimeout: TimeInterval
    private let readinessSchedule: DevelopmentHubReadinessScheduler
    private let readinessClock: DevelopmentHubReadinessClock
    private let httpsProbe: DevelopmentHubHTTPSProbe
    private var domain: String { "gui/\(configuration.ownerUID)" }
    private var service: String { "\(domain)/\(configuration.serviceLabel)" }

    init(configuration: DevelopmentHubConfiguration,
         processRunner: @escaping DevelopmentHubProcessRunner = {
             executable, arguments, timeout, completion in
             HubProcessExecutor.run(executable: executable,
                                    arguments: arguments,
                                    timeout: timeout,
                                    completion: completion)
         },
         readinessPollInterval: TimeInterval = 0.5,
         readinessMaxAttempts: Int = 121,
         readinessTimeout: TimeInterval = 60,
         readinessSchedule: @escaping DevelopmentHubReadinessScheduler = { delay, action in
             DispatchQueue.global(qos: .userInitiated).asyncAfter(
                 deadline: .now() + delay,
                 execute: action
             )
         },
         readinessClock: @escaping DevelopmentHubReadinessClock = {
             ProcessInfo.processInfo.systemUptime
         },
         httpsProbe: @escaping DevelopmentHubHTTPSProbe = DevelopmentHubHTTPSReadiness.probe) {
        self.configuration = configuration
        self.processRunner = processRunner
        self.readinessPollInterval = max(0, readinessPollInterval)
        self.readinessMaxAttempts = max(1, readinessMaxAttempts)
        self.readinessTimeout = max(0.1, readinessTimeout)
        self.readinessSchedule = readinessSchedule
        self.readinessClock = readinessClock
        self.httpsProbe = httpsProbe
    }

    func checkLiveReadiness(completion: @escaping (Bool) -> Void) {
        runLaunchctl(["print", service], timeout: 5) { [weak self] result in
            guard let self, case let .success(output) = result,
                  let pid = Self.runningProcessIdentifier(in: output,
                    expectedBinary: self.configuration.binary.path,
                    expectedConfig: self.configuration.config.path) else {
                completion(false)
                return
            }
            self.httpsProbe(self.configuration, pid, 5, completion)
        }
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        let action: HubServiceAction
        switch arguments.last {
        case "start": action = .start
        case "stop": action = .stop
        case "restart": action = .restart
        default:
            completion(.failure(HubActionError.commandFailed(HubL10n.text("hub.DevelopmentHubRuntime.472.142", fallback: "Unknown service action."))))
            return
        }
        let journal = DevelopmentEventLog.shared(directory: configuration.logDirectory)
        let logAction: DevelopmentEventLog.Action = action == .start ? .start : action == .stop ? .stop : .restart
        let operation = UUID(); let started = DispatchTime.now().uptimeNanoseconds
        journal.record(kind: .control, action: logAction, outcome: .start, operation: operation)
        let originalCompletion = completion
        let completion: (Result<String, Error>) -> Void = { result in
            journal.completion(kind: .control, action: logAction, operation: operation, started: started, result: result)
            originalCompletion(result)
        }
        if action == .stop {
            do {
                try configuration.validateStopTarget()
            } catch {
                completion(.failure(error))
                return
            }
            runLaunchPlan(action: action, completion: completion)
            return
        }
        do {
            try configuration.validate(requireConfig: true)
        } catch {
            completion(.failure(error))
            return
        }
        runServePreflight { [weak self] result in
            guard let self else { return }
            switch result {
            case .success:
                do {
                    try self.configuration.preparePrivateLaunchLogs()
                    try self.writeLaunchAgent()
                } catch {
                    completion(.failure(error))
                    return
                }
                self.runLaunchPlan(action: action, completion: completion)
            case let .failure(error):
                completion(.failure(error))
            }
        }
    }

    private func runServePreflight(completion: @escaping (Result<String, Error>) -> Void) {
        let historyOnly: Bool
        do { historyOnly = try HistoryOnlyControl.isSelected(for: configuration.config) }
        catch { completion(.failure(error)); return }
        let journal = DevelopmentEventLog.shared(directory: configuration.logDirectory)
        let operation = UUID(); let started = DispatchTime.now().uptimeNanoseconds
        journal.record(kind: .preflight, action: .preflight, outcome: .start, operation: operation)
        let originalCompletion = completion
        let completion: (Result<String, Error>) -> Void = { result in
            journal.completion(kind: .preflight, action: .preflight, operation: operation, started: started, result: result)
            originalCompletion(result)
        }
        processRunner(
            configuration.binary,
            [
                "--config", configuration.config.path,
                "serve-preflight", "--mode", configuration.mode.rawValue
            ] + (historyOnly ? ["--history-only"] : []),
            30,
            completion
        )
    }

    private func runLaunchPlan(action: HubServiceAction,
                               completion: @escaping (Result<String, Error>) -> Void) {
        loadedState(validateStopTarget: action == .stop) { [weak self] state in
            guard let self else { return }
            let loaded: Bool
            switch state {
            case .loaded: loaded = true
            case .unloaded: loaded = false
            case let .unknown(error): completion(.failure(error)); return
            }
            self.runCommands(Self.commandPlan(action: action,
                                              loaded: loaded,
                                              domain: self.domain,
                                              service: self.service,
                                              plist: self.configuration.plist.path),
                             index: 0) { result in
                switch result {
                case .success where action == .stop:
                    completion(.success(""))
                case .success:
                    let readinessDeadline = self.readinessClock() + self.readinessTimeout
                    self.waitUntilReady(previousReadyPID: nil,
                                        attemptsRemaining: self.readinessMaxAttempts,
                                        deadline: readinessDeadline,
                                        completion: completion)
                case let .failure(error):
                    completion(.failure(error))
                }
            }
        }
    }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        loadedState(validateStopTarget: false, completion: completion)
    }

    private func loadedState(validateStopTarget: Bool,
                             completion: @escaping (HubServiceLoadState) -> Void) {
        do {
            if validateStopTarget {
                try configuration.validateStopTarget()
            } else {
                try configuration.validate(requireConfig: false)
            }
        } catch {
            completion(.unknown(error))
            return
        }
        runLaunchctl(["print", service]) { [service] result in
            switch result {
            case .success: completion(.loaded)
            case let .failure(error):
                if case let HubActionError.commandExited(status, output) = error,
                   LaunchctlServiceController.isKnownUnloadedPrintFailure(
                       status: status,
                       output: output,
                       service: service
                   ) {
                    completion(.unloaded)
                } else {
                    completion(.unknown(error))
                }
            }
        }
    }

    static func commandPlan(action: HubServiceAction,
                            loaded: Bool,
                            domain: String,
                            service: String,
                            plist: String) -> [[String]] {
        switch action {
        case .stop: return loaded ? [["bootout", service]] : []
        // launchd keeps the loaded job's environment after a plist rewrite.
        case .start:
            return loaded
                ? [["bootout", service], ["bootstrap", domain, plist]]
                : [["bootstrap", domain, plist]]
        case .restart:
            return loaded
                ? [["bootout", service], ["bootstrap", domain, plist]]
                : [["bootstrap", domain, plist]]
        }
    }

    private func writeLaunchAgent() throws {
        let data = try PropertyListSerialization.data(fromPropertyList: launchAgentPropertyList(),
                                                      format: .xml,
                                                      options: 0)
        let directory = try DevelopmentEventLog.validateDirectoryReadOnly(configuration.controlDirectory,
                                                                         allowOwnedGroupWritableAncestors: false)
        defer { close(directory) }
        let temporary = ".launch-agent.\(UUID().uuidString).tmp"
        let descriptor = openat(directory, temporary, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, mode_t(0o600))
        guard descriptor >= 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
        defer { close(descriptor); _ = unlinkat(directory, temporary, 0) }
        try data.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let count = write(descriptor, raw.baseAddress!.advanced(by: offset), raw.count - offset)
                guard count > 0 else {
                    if errno == EINTR { continue }
                    throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
                }
                offset += count
            }
        }
        guard renameat(directory, temporary, directory, configuration.plist.lastPathComponent) == 0 else {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        try configuration.validate(requireConfig: true)
    }

    func launchAgentPropertyList() -> [String: Any] {
        [
            "Label": configuration.serviceLabel,
            "ProgramArguments": [
                configuration.binary.path,
                "--config", configuration.config.path,
                "serve"
            ],
            "WorkingDirectory": configuration.stateDirectory.path,
            "RunAtLoad": true,
            "KeepAlive": true,
            "ProcessType": "Background",
            "ThrottleInterval": 10,
            "Umask": 63,
            "EnvironmentVariables": [
                DevelopmentHubConfiguration.enableVariable: "1",
                DevelopmentHubConfiguration.modeVariable: configuration.mode.rawValue,
                "RUST_LOG": "info,tower_http=debug",
                DevelopmentHubConfiguration.logVariable: configuration.logDirectory.path
            ],
            // The typed journals own retention. Raw stdio can contain sensitive
            // free-form diagnostics and an open launchd FD cannot be rotated.
            "StandardOutPath": "/dev/null",
            "StandardErrorPath": "/dev/null"
        ]
    }

    private func runCommands(_ commands: [[String]],
                             index: Int,
                             completion: @escaping (Result<String, Error>) -> Void) {
        guard index < commands.count else { completion(.success("")); return }
        runLaunchctl(commands[index]) { [weak self] result in
            switch result {
            case .success:
                if commands[index].first == "bootout", commands[index].count == 2 {
                    self?.waitUntilUnloaded(service: commands[index][1], attemptsRemaining: 100) {
                        waitResult in
                        switch waitResult {
                        case .success:
                            self?.runCommands(commands, index: index + 1, completion: completion)
                        case let .failure(error): completion(.failure(error))
                        }
                    }
                } else {
                    self?.runCommands(commands, index: index + 1, completion: completion)
                }
            case let .failure(error): completion(.failure(error))
            }
        }
    }

    private func waitUntilUnloaded(service: String,
                                   attemptsRemaining: Int,
                                   completion: @escaping (Result<Void, Error>) -> Void) {
        runLaunchctl(["print", service]) { [weak self] result in
            switch result {
            case .success where attemptsRemaining > 1:
                DispatchQueue.global(qos: .userInitiated).asyncAfter(deadline: .now() + 0.1) {
                    self?.waitUntilUnloaded(service: service,
                                            attemptsRemaining: attemptsRemaining - 1,
                                            completion: completion)
                }
            case .success:
                completion(.failure(HubActionError.commandFailed(
                    HubL10n.text("hub.DevelopmentHubRuntime.692.174", fallback: "Development Hub did not finish stopping.")
                )))
            case let .failure(error):
                if case let HubActionError.commandExited(status, output) = error,
                   LaunchctlServiceController.isKnownUnloadedPrintFailure(
                       status: status, output: output, service: service
                   ) {
                    completion(.success(()))
                } else {
                    completion(.failure(error))
                }
            }
        }
    }

    private func waitUntilReady(previousReadyPID: Int?,
                                attemptsRemaining: Int,
                                deadline: TimeInterval,
                                completion: @escaping (Result<String, Error>) -> Void) {
        guard let requestTimeout = readinessRequestTimeout(deadline: deadline) else {
            failStartupAndStop(startupFailure(
                lastFailure: HubL10n.text("hub.DevelopmentHubRuntime.713.175", fallback: "The readiness deadline expired.")
            ), completion: completion)
            return
        }
        runLaunchctl(["print", service], timeout: requestTimeout) { [weak self] launchResult in
            guard let self else { return }
            switch launchResult {
            case let .success(output):
                guard let pid = Self.runningProcessIdentifier(
                    in: output,
                    expectedBinary: self.configuration.binary.path,
                    expectedConfig: self.configuration.config.path
                ) else {
                    self.retryReadiness(
                        previousReadyPID: nil,
                        attemptsRemaining: attemptsRemaining,
                        deadline: deadline,
                        failure: HubL10n.text("hub.DevelopmentHubRuntime.730.177", fallback: "The owned LaunchAgent is loaded but is not running the intended binary and configuration."),
                        completion: completion
                    )
                    return
                }
                guard let statusTimeout = self.readinessRequestTimeout(deadline: deadline) else {
                    self.failStartupAndStop(self.startupFailure(
                        lastFailure: HubL10n.text("hub.DevelopmentHubRuntime.737.178", fallback: "The readiness deadline expired before the status check.")
                    ), completion: completion)
                    return
                }
                self.processRunner(
                    self.configuration.binary,
                    ["--config", self.configuration.config.path, "status"],
                    statusTimeout
                ) { [weak self] statusResult in
                    guard let self else { return }
                    switch statusResult {
                    case let .success(statusOutput)
                        where Self.isUsableStatusOutput(statusOutput):
                        guard let probeTimeout = self.readinessRequestTimeout(deadline: deadline) else {
                            self.failStartupAndStop(self.startupFailure(lastFailure: "The HTTPS readiness deadline expired."), completion: completion)
                            return
                        }
                        self.httpsProbe(self.configuration, pid, min(5, probeTimeout)) { ready in
                            guard ready, self.readinessClock() < deadline else {
                                self.retryReadiness(previousReadyPID: nil,
                                    attemptsRemaining: attemptsRemaining, deadline: deadline,
                                    failure: "The intended process has not become ready over HTTPS. Check the selected runtime's volume access and logs.",
                                    completion: completion)
                                return
                            }
                            if previousReadyPID == pid {
                                completion(.success(HubL10n.format("hub.DevelopmentHubRuntime.751.181", fallback: "Development Hub is running as PID %1$@.", arguments: [String(describing: pid)])))
                                return
                            }
                            self.retryReadiness(
                                previousReadyPID: pid,
                                attemptsRemaining: attemptsRemaining,
                                deadline: deadline,
                                failure: HubL10n.text("hub.DevelopmentHubRuntime.757.182", fallback: "The intended process has not remained stable for two readiness checks."),
                                completion: completion
                            )
                        }
                    case let .success(statusOutput):
                        self.retryReadiness(
                            previousReadyPID: nil,
                            attemptsRemaining: attemptsRemaining,
                            deadline: deadline,
                            failure: HubL10n.format("hub.DevelopmentHubRuntime.766.183", fallback: "The intended process returned an invalid status response: %1$@", arguments: [String(describing: Self.boundedDiagnostic(statusOutput))]),
                            completion: completion
                        )
                    case let .failure(error):
                        self.retryReadiness(
                            previousReadyPID: nil,
                            attemptsRemaining: attemptsRemaining,
                            deadline: deadline,
                            failure: HubL10n.format("hub.DevelopmentHubRuntime.774.184", fallback: "The intended process status check failed: %1$@", arguments: [String(describing: Self.boundedDiagnostic(error.localizedDescription))]),
                            completion: completion
                        )
                    }
                }
            case let .failure(error):
                self.retryReadiness(
                    previousReadyPID: nil,
                    attemptsRemaining: attemptsRemaining,
                    deadline: deadline,
                    failure: HubL10n.format("hub.DevelopmentHubRuntime.784.185", fallback: "The owned LaunchAgent is not running: %1$@", arguments: [String(describing: Self.boundedDiagnostic(error.localizedDescription))]),
                    completion: completion
                )
            }
        }
    }

    private func retryReadiness(previousReadyPID: Int?,
                                attemptsRemaining: Int,
                                deadline: TimeInterval,
                                failure: String,
                                completion: @escaping (Result<String, Error>) -> Void) {
        let remaining = deadline - readinessClock()
        guard attemptsRemaining > 1, remaining > 0 else {
            failStartupAndStop(startupFailure(lastFailure: failure), completion: completion)
            return
        }
        readinessSchedule(min(readinessPollInterval, remaining)) { [weak self] in
            self?.waitUntilReady(previousReadyPID: previousReadyPID,
                                 attemptsRemaining: attemptsRemaining - 1,
                                 deadline: deadline,
                                 completion: completion)
        }
    }

    private func readinessRequestTimeout(deadline: TimeInterval) -> TimeInterval? {
        let remaining = deadline - readinessClock()
        guard remaining > 0 else { return nil }
        return min(30, remaining)
    }

    private func failStartupAndStop(_ startupError: Error,
                                    completion: @escaping (Result<String, Error>) -> Void) {
        runLaunchctl(["bootout", service]) { [weak self] result in
            guard let self else { return }
            switch result {
            case .success:
                self.waitUntilUnloaded(service: self.service, attemptsRemaining: 100) { result in
                    switch result {
                    case .success:
                        completion(.failure(startupError))
                    case let .failure(cleanupError):
                        completion(.failure(self.startupCleanupFailure(
                            startupError: startupError,
                            cleanupError: cleanupError
                        )))
                    }
                }
            case let .failure(cleanupError):
                if case let HubActionError.commandExited(status, output) = cleanupError,
                   LaunchctlServiceController.isKnownUnloadedPrintFailure(
                       status: status,
                       output: output,
                       service: self.service
                   ) {
                    completion(.failure(startupError))
                } else {
                    completion(.failure(self.startupCleanupFailure(
                        startupError: startupError,
                        cleanupError: cleanupError
                    )))
                }
            }
        }
    }

    private func startupCleanupFailure(startupError: Error, cleanupError: Error) -> Error {
        HubActionError.commandFailed(
            HubL10n.format("hub.DevelopmentHubRuntime.852.187", fallback: "%1$@ The failed development LaunchAgent could not be unloaded: %2$@", arguments: [String(describing: startupError.localizedDescription), String(describing: Self.boundedDiagnostic(cleanupError.localizedDescription))])
        )
    }

    private func startupFailure(lastFailure: String) -> Error {
        var detail = HubL10n.format("hub.DevelopmentHubRuntime.857.188", fallback: "Development Hub did not become ready under %1$@. %2$@", arguments: [String(describing: service), String(describing: lastFailure)])
        if let stderr = HubAppLog.regularFileTail(
            of: configuration.standardErrorLog,
            maximumBytes: 4_096
        )?.trimmingCharacters(in: .whitespacesAndNewlines), !stderr.isEmpty {
            detail += " Recent stderr: \(Self.boundedDiagnostic(stderr))"
        }
        detail += " Logs: \(configuration.logDirectory.path)"
        return HubActionError.commandFailed(detail)
    }

    static func runningProcessIdentifier(in launchctlOutput: String,
                                         expectedBinary: String,
                                         expectedConfig: String) -> Int? {
        let lines = launchctlOutput.split(whereSeparator: \.isNewline)
            .map { String($0).trimmingCharacters(in: .whitespaces) }
        guard lines.contains("state = running"),
              lines.contains("program = \(expectedBinary)"),
              lines.contains(expectedBinary),
              lines.contains("--config"),
              lines.contains(expectedConfig),
              lines.contains("serve") else { return nil }
        guard let pidLine = lines.first(where: { $0.hasPrefix("pid = ") }),
              let pid = Int(pidLine.dropFirst("pid = ".count)), pid > 0 else { return nil }
        return pid
    }

    static func isUsableStatusOutput(_ output: String) -> Bool {
        guard let data = output.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              object["status"] as? String == "ok",
              object["ready"] as? Bool == true else { return false }
        return true
    }

    private static func boundedDiagnostic(_ value: String) -> String {
        let singleLine = value.replacingOccurrences(of: "\n", with: " ")
            .replacingOccurrences(of: "\r", with: " ")
        return String(singleLine.prefix(1_024))
    }

    private func runLaunchctl(_ arguments: [String],
                              timeout: TimeInterval = 30,
                              completion: @escaping (Result<String, Error>) -> Void) {
        processRunner(URL(fileURLWithPath: "/bin/launchctl"), arguments, timeout, completion)
    }
}
