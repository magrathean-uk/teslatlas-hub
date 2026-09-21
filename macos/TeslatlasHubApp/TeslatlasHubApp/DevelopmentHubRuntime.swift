// SPDX-License-Identifier: AGPL-3.0-only

import Darwin
import Foundation

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

    let binary: URL
    let config: URL
    let stateDirectory: URL
    let logDirectory: URL
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
        stateDirectory.appendingPathComponent(".\(serviceLabel).plist")
    }

    var standardOutputLog: URL { logDirectory.appendingPathComponent("hub.out.log") }
    var standardErrorLog: URL { logDirectory.appendingPathComponent("hub.err.log") }

    static func from(environment: [String: String], ownerUID: uid_t = getuid()) throws -> Self? {
        let variables = [
            enableVariable, binaryVariable, configVariable, stateVariable, logVariable, modeVariable
        ]
        let supplied = variables.contains { environment[$0] != nil }
        guard supplied else { return nil }
        guard environment[enableVariable] == "1" else {
            throw HubActionError.commandFailed(
                "Local development mode was not enabled. Set \(enableVariable)=1 together with all four development paths."
            )
        }

        func requiredPath(_ name: String) throws -> URL {
            guard let value = environment[name], !value.isEmpty, value.hasPrefix("/") else {
                throw HubActionError.commandFailed("\(name) must be a non-empty absolute path.")
            }
            let url = URL(fileURLWithPath: value).standardizedFileURL
            guard url.path == value || (value.hasSuffix("/") && url.path + "/" == value) else {
                throw HubActionError.commandFailed("\(name) must not contain relative path components.")
            }
            return url
        }

        let mode: DevelopmentHubMode
        if let rawMode = environment[modeVariable] {
            guard let parsed = DevelopmentHubMode(rawValue: rawMode) else {
                throw HubActionError.commandFailed(
                    "\(modeVariable) must be fixture, standalone, or edge."
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
            ownerUID: ownerUID,
            mode: mode
        )
        return value
    }

    func validate(requireConfig: Bool) throws {
        try Self.validatePath(binary, kind: .executable, ownerUID: ownerUID)
        try Self.validatePath(stateDirectory, kind: .privateDirectory, ownerUID: ownerUID)
        try Self.validatePath(logDirectory, kind: .privateDirectory, ownerUID: ownerUID)
        try Self.validateAncestors(of: config, ownerUID: ownerUID, allowMissingLeaf: !requireConfig)
        if Self.pathEntryExists(config) || requireConfig {
            try Self.validatePath(config, kind: .privateFile, ownerUID: ownerUID)
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
        let directory = open(logDirectory.path, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW)
        guard directory >= 0 else {
            throw HubActionError.commandFailed(
                "Development log directory cannot be safely opened: \(logDirectory.path)"
            )
        }
        defer { close(directory) }
        var directoryInformation = stat()
        guard fstat(directory, &directoryInformation) == 0,
              directoryInformation.st_mode & S_IFMT == S_IFDIR,
              directoryInformation.st_uid == ownerUID,
              directoryInformation.st_mode & 0o077 == 0 else {
            throw HubActionError.commandFailed(
                "Development log directory changed during preparation: \(logDirectory.path)"
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
                    "Development log cannot be safely opened: \(logDirectory.appendingPathComponent(name).path)"
                )
            }
            defer { close(descriptor) }

            var information = stat()
            guard fstat(descriptor, &information) == 0,
                  information.st_mode & S_IFMT == S_IFREG,
                  information.st_uid == ownerUID,
                  information.st_nlink == 1 else {
                throw HubActionError.commandFailed(
                    "Development log must be a current-owner regular single-link file: \(logDirectory.appendingPathComponent(name).path)"
                )
            }
            let permissions = information.st_mode & 0o777
            guard permissions == 0o600 || permissions == 0o644 else {
                throw HubActionError.commandFailed(
                    "Development log has unsupported permissions: \(logDirectory.appendingPathComponent(name).path)"
                )
            }
            if permissions == 0o644, fchmod(descriptor, mode_t(0o600)) != 0 {
                throw HubActionError.commandFailed(
                    "Development log permissions could not be repaired: \(logDirectory.appendingPathComponent(name).path)"
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
                    "Development log identity changed during preparation: \(logDirectory.appendingPathComponent(name).path)"
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
                "Development service must belong to the current user."
            )
        }
        let paths = [binary, config, stateDirectory, logDirectory]
        guard paths.allSatisfy({ url in
            url.path.hasPrefix("/") && url.standardizedFileURL.path == url.path
        }) else {
            throw HubActionError.commandFailed(
                "Development service identity contains an unsafe path."
            )
        }
        guard serviceLabel.hasPrefix("com.teslatlas.hub.development."),
              serviceLabel.count > "com.teslatlas.hub.development.".count,
              serviceLabel != "com.teslatlas.hub" else {
            throw HubActionError.commandFailed("Unsafe development service label.")
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

    private static func validatePath(_ url: URL, kind: PathKind, ownerUID: uid_t) throws {
        try validateAncestors(of: url, ownerUID: ownerUID, allowMissingLeaf: false)
        var information = stat()
        guard lstat(url.path, &information) == 0 else {
            throw HubActionError.commandFailed("Development path is unavailable: \(url.path)")
        }
        guard information.st_uid == ownerUID else {
            throw HubActionError.commandFailed("Development path is not owned by the current user: \(url.path)")
        }
        let type = information.st_mode & S_IFMT
        switch kind {
        case .executable:
            guard type == S_IFREG, information.st_mode & 0o022 == 0,
                  FileManager.default.isExecutableFile(atPath: url.path) else {
                throw HubActionError.commandFailed(
                    "Development binary must be a regular executable not writable by group or others: \(url.path)"
                )
            }
        case .privateFile:
            guard type == S_IFREG, information.st_mode & 0o077 == 0 else {
                throw HubActionError.commandFailed(
                    "Development file must be regular and accessible only to its owner: \(url.path)"
                )
            }
        case .privateDirectory:
            guard type == S_IFDIR, information.st_mode & 0o077 == 0 else {
                throw HubActionError.commandFailed(
                    "Development directory must be owned and accessible only by its owner: \(url.path)"
                )
            }
        }
    }

    private static func validateAncestors(of url: URL,
                                          ownerUID: uid_t,
                                          allowMissingLeaf: Bool) throws {
        let components = url.standardizedFileURL.pathComponents
        var path = "/"
        for (index, component) in components.dropFirst().enumerated() {
            path = (path as NSString).appendingPathComponent(component)
            var information = stat()
            if lstat(path, &information) != 0 {
                if allowMissingLeaf && index == components.count - 2 && errno == ENOENT { return }
                throw HubActionError.commandFailed("Development path is unavailable: \(path)")
            }
            guard information.st_mode & S_IFMT != S_IFLNK else {
                throw HubActionError.commandFailed("Development paths must not contain symbolic links: \(path)")
            }
            if index < components.count - 2 {
                guard information.st_mode & S_IFMT == S_IFDIR else {
                    throw HubActionError.commandFailed("Development path parent is not a directory: \(path)")
                }
                guard information.st_uid == 0 || information.st_uid == ownerUID else {
                    throw HubActionError.commandFailed("Development path parent has an unexpected owner: \(path)")
                }
                guard information.st_mode & 0o022 == 0 else {
                    throw HubActionError.commandFailed("Development path parent is writable by group or others: \(path)")
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
            "A source-run development Hub cannot mutate the production service installation."
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

final class DevelopmentLaunchctlServiceController: HubServiceControlling {
    private let configuration: DevelopmentHubConfiguration
    private let processRunner: DevelopmentHubProcessRunner
    private let readinessPollInterval: TimeInterval
    private let readinessMaxAttempts: Int
    private let readinessSchedule: DevelopmentHubReadinessScheduler
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
         readinessSchedule: @escaping DevelopmentHubReadinessScheduler = { delay, action in
             DispatchQueue.global(qos: .userInitiated).asyncAfter(
                 deadline: .now() + delay,
                 execute: action
             )
         }) {
        self.configuration = configuration
        self.processRunner = processRunner
        self.readinessPollInterval = max(0, readinessPollInterval)
        self.readinessMaxAttempts = max(1, readinessMaxAttempts)
        self.readinessSchedule = readinessSchedule
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        let action: HubServiceAction
        switch arguments.last {
        case "start": action = .start
        case "stop": action = .stop
        case "restart": action = .restart
        default:
            completion(.failure(HubActionError.commandFailed("Unknown service action.")))
            return
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
        processRunner(
            configuration.binary,
            [
                "--config", configuration.config.path,
                "serve-preflight", "--mode", configuration.mode.rawValue
            ],
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
                    self.waitUntilReady(previousReadyPID: nil,
                                        attemptsRemaining: self.readinessMaxAttempts,
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
        case .start: return loaded ? [["kickstart", service]] : [["bootstrap", domain, plist]]
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
        let temporary = configuration.stateDirectory
            .appendingPathComponent(".launch-agent.\(UUID().uuidString).tmp")
        try data.write(to: temporary, options: .withoutOverwriting)
        do {
            try FileManager.default.setAttributes(
                [.posixPermissions: NSNumber(value: 0o600)],
                ofItemAtPath: temporary.path
            )
            if rename(temporary.path, configuration.plist.path) != 0 {
                throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
            }
            try configuration.validate(requireConfig: true)
        } catch {
            try? FileManager.default.removeItem(at: temporary)
            throw error
        }
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
                "RUST_LOG": "info,tower_http=debug"
            ],
            "StandardOutPath": configuration.standardOutputLog.path,
            "StandardErrorPath": configuration.standardErrorLog.path
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
                    "Development Hub did not finish stopping."
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
                                completion: @escaping (Result<String, Error>) -> Void) {
        runLaunchctl(["print", service]) { [weak self] launchResult in
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
                        failure: "The owned LaunchAgent is loaded but is not running the intended binary and configuration.",
                        completion: completion
                    )
                    return
                }
                self.processRunner(
                    self.configuration.binary,
                    ["--config", self.configuration.config.path, "status"],
                    30
                ) { [weak self] statusResult in
                    guard let self else { return }
                    switch statusResult {
                    case let .success(statusOutput)
                        where Self.isUsableStatusOutput(statusOutput):
                        if previousReadyPID == pid {
                            completion(.success("Development Hub is running as PID \(pid)."))
                        } else {
                            self.retryReadiness(
                                previousReadyPID: pid,
                                attemptsRemaining: attemptsRemaining,
                                failure: "The intended process has not remained stable for two readiness checks.",
                                completion: completion
                            )
                        }
                    case let .success(statusOutput):
                        self.retryReadiness(
                            previousReadyPID: nil,
                            attemptsRemaining: attemptsRemaining,
                            failure: "The intended process returned an invalid status response: \(Self.boundedDiagnostic(statusOutput))",
                            completion: completion
                        )
                    case let .failure(error):
                        self.retryReadiness(
                            previousReadyPID: nil,
                            attemptsRemaining: attemptsRemaining,
                            failure: "The intended process status check failed: \(Self.boundedDiagnostic(error.localizedDescription))",
                            completion: completion
                        )
                    }
                }
            case let .failure(error):
                self.retryReadiness(
                    previousReadyPID: nil,
                    attemptsRemaining: attemptsRemaining,
                    failure: "The owned LaunchAgent is not running: \(Self.boundedDiagnostic(error.localizedDescription))",
                    completion: completion
                )
            }
        }
    }

    private func retryReadiness(previousReadyPID: Int?,
                                attemptsRemaining: Int,
                                failure: String,
                                completion: @escaping (Result<String, Error>) -> Void) {
        guard attemptsRemaining > 1 else {
            completion(.failure(startupFailure(lastFailure: failure)))
            return
        }
        readinessSchedule(readinessPollInterval) { [weak self] in
            self?.waitUntilReady(previousReadyPID: previousReadyPID,
                                 attemptsRemaining: attemptsRemaining - 1,
                                 completion: completion)
        }
    }

    private func startupFailure(lastFailure: String) -> Error {
        var detail = "Development Hub did not become ready under \(service). \(lastFailure)"
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
              object["ready"] is Bool else { return false }
        return true
    }

    private static func boundedDiagnostic(_ value: String) -> String {
        let singleLine = value.replacingOccurrences(of: "\n", with: " ")
            .replacingOccurrences(of: "\r", with: " ")
        return String(singleLine.prefix(1_024))
    }

    private func runLaunchctl(_ arguments: [String],
                              completion: @escaping (Result<String, Error>) -> Void) {
        processRunner(URL(fileURLWithPath: "/bin/launchctl"), arguments, 30, completion)
    }
}
