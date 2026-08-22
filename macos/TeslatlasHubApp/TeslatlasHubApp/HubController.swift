import AppKit
import Foundation

private enum HubRelease {
    static let fallbackVersion = "1.0.0-alpha.1"
}

enum HubHealth: Equatable {
    case running
    case stopped
    case needsInstall
    case degraded

    var title: String {
        switch self {
        case .running: return "Collecting vehicle data"
        case .stopped: return "Hub is stopped"
        case .needsInstall: return "Install Teslatlas Hub"
        case .degraded: return "Attention needed"
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

struct HubSnapshot {
    var health: HubHealth
    var service: String
    var account: String
    var vehicleName: String
    var vehicle: String
    var database: String
    var activity: [HubActivity]
    var version: String
    var dataDirectory: URL?
    var diagnosticLines: [String]

    static let previewRunning = HubSnapshot(
        health: .running,
        service: "Installed and running",
        account: "Connected",
        vehicleName: "Model 3",
        vehicle: "Offline · last seen 2 minutes ago",
        database: "Healthy · 18,426 records",
        activity: [
            HubActivity(message: "Vehicle went offline", age: "2 minutes ago", color: .systemGray),
            HubActivity(message: "Position stored", age: "4 minutes ago", color: .systemGreen)
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
        service: "Not installed",
        account: "Not configured",
        vehicleName: "Vehicle",
        vehicle: "No configured vehicle",
        database: "Waiting for setup or import",
        activity: [],
        version: HubRelease.fallbackVersion,
        dataDirectory: URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/Application Support/Teslatlas Hub/data"),
        diagnosticLines: ["Hub has not been configured or installed."]
    )
}

enum HubActionError: LocalizedError {
    case preview
    case missingResource(String)
    case commandFailed(String)

    var errorDescription: String? {
        switch self {
        case .preview:
            return "Preview mode is read-only. No process, installer, or launchctl action was run."
        case let .missingResource(name):
            return "Embedded resource is missing: \(name)"
        case let .commandFailed(message):
            return message
        }
    }
}

protocol HubCommandRunning {
    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void)
    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void)
}

extension HubCommandRunning {
    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, completion: completion)
    }
}

final class EmbeddedHubCommandRunner: HubCommandRunning {
    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        runProcess(arguments: arguments, stdin: nil, completion: completion)
    }

    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        runProcess(arguments: arguments, stdin: stdin, completion: completion)
    }

    private func runProcess(arguments: [String], stdin: String?, completion: @escaping (Result<String, Error>) -> Void) {
        guard let executable = Bundle.main.url(forResource: "teslatlas-hub", withExtension: nil) else {
            completion(.failure(HubActionError.missingResource("teslatlas-hub")))
            return
        }
        DispatchQueue.global(qos: .userInitiated).async {
            let process = Process()
            let output = Pipe()
            process.executableURL = executable
            process.arguments = arguments
            process.standardOutput = output
            process.standardError = output
            if stdin != nil { process.standardInput = Pipe() }
            do {
                try process.run()
                if let stdin, let input = process.standardInput as? Pipe {
                    input.fileHandleForWriting.write(stdin.data(using: .utf8)!)
                    input.fileHandleForWriting.closeFile()
                }
                process.waitUntilExit()
                let data = output.fileHandleForReading.readDataToEndOfFile()
                let text = String(decoding: data, as: UTF8.self)
                if process.terminationStatus == 0 {
                    completion(.success(text))
                } else {
                    completion(.failure(HubActionError.commandFailed(text.isEmpty ? "Hub command failed." : text)))
                }
            } catch {
                completion(.failure(error))
            }
        }
    }
}

final class EmbeddedInstaller {
    func install(completion: @escaping (Result<String, Error>) -> Void) {
        guard let package = Bundle.main.url(forResource: "TeslatlasHubService", withExtension: "pkg") else {
            completion(.failure(HubActionError.missingResource("TeslatlasHubService.pkg")))
            return
        }
        let command = "/usr/sbin/installer -pkg \(shellQuote(package.path)) -target /"
        DispatchQueue.global(qos: .userInitiated).async {
            var errorInfo: NSDictionary?
            let script = NSAppleScript(source: "do shell script \"\(Self.appleScriptQuote(command))\" with administrator privileges")
            let result = script?.executeAndReturnError(&errorInfo)
            if let errorInfo {
                let message = errorInfo[NSAppleScript.errorMessage] as? String ?? "Administrator installation failed."
                completion(.failure(HubActionError.commandFailed(message)))
            } else {
                completion(.success(result?.stringValue ?? ""))
            }
        }
    }

    private func shellQuote(_ value: String) -> String { "'\(value.replacingOccurrences(of: "'", with: "'\\''"))'" }
    private static func appleScriptQuote(_ value: String) -> String { value.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"") }
}

private enum ProcessRunner {
    static func run(executable: URL, arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            let process = Process()
            let output = Pipe()
            process.executableURL = executable
            process.arguments = arguments
            process.standardOutput = output
            process.standardError = output
            do {
                try process.run()
                process.waitUntilExit()
                let text = String(decoding: output.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
                if process.terminationStatus == 0 { completion(.success(text)) }
                else { completion(.failure(HubActionError.commandFailed(text.isEmpty ? "Command failed." : text))) }
            } catch { completion(.failure(error)) }
        }
    }
}

enum HubServiceAction {
    case start
    case stop
    case restart
}

enum HubServiceLoadState {
    case loaded
    case unloaded
    case unknown(Error)
}

final class LaunchctlServiceController: HubCommandRunning {
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
        let domain = "gui/\(getuid())"
        let service = "\(domain)/com.teslatlas.hub"
        let plist = URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/LaunchAgents/com.teslatlas.hub.plist")
        loadedState { [weak self] loaded in
            switch loaded {
            case .loaded:
                self?.runCommands(Self.commandPlan(action: action, loaded: true, domain: domain, service: service, plist: plist.path), index: 0, completion: completion)
            case .unloaded:
                self?.runCommands(Self.commandPlan(action: action, loaded: false, domain: domain, service: service, plist: plist.path), index: 0, completion: completion)
            case let .unknown(error): completion(.failure(error))
            }
        }
    }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        let service = "gui/\(getuid())/com.teslatlas.hub"
        queryLoaded(service: service) { result in
            switch result {
            case .success(true): completion(.loaded)
            case .success(false): completion(.unloaded)
            case let .failure(error): completion(.unknown(error))
            }
        }
    }

    static func commandPlan(action: HubServiceAction, loaded: Bool, domain: String, service: String, plist: String) -> [[String]] {
        switch action {
        case .stop:
            return loaded ? [["bootout", service]] : []
        case .start, .restart:
            var commands = loaded ? [] : [["bootstrap", domain, plist]]
            commands.append(["kickstart", "-k", service])
            return commands
        }
    }

    private func queryLoaded(service: String, completion: @escaping (Result<Bool, Error>) -> Void) {
        ProcessRunner.run(executable: URL(fileURLWithPath: "/bin/launchctl"), arguments: ["print", service]) { result in
            switch result {
            case .success: completion(.success(true))
            case let .failure(error):
                let text = error.localizedDescription.lowercased()
                if text.contains("could not find service") || text.contains("no such process") || text.contains("not found") || text.contains("113") {
                    completion(.success(false))
                } else {
                    completion(.failure(error))
                }
            }
        }
    }

    private func runCommands(_ commands: [[String]], index: Int, completion: @escaping (Result<String, Error>) -> Void) {
        guard index < commands.count else { completion(.success("")); return }
        ProcessRunner.run(executable: URL(fileURLWithPath: "/bin/launchctl"), arguments: commands[index]) { [weak self] result in
            switch result {
            case .success: self?.runCommands(commands, index: index + 1, completion: completion)
            case let .failure(error): completion(.failure(error))
            }
        }
    }
}

final class HubController {
    let previewMode: Bool
    private let commandRunner: HubCommandRunning
    private let installer = EmbeddedInstaller()
    private let serviceRunner = LaunchctlServiceController()
    private(set) var snapshot: HubSnapshot

    init(environment: [String: String] = ProcessInfo.processInfo.environment, commandRunner: HubCommandRunning = EmbeddedHubCommandRunner()) {
        previewMode = environment["TESLATLAS_HUB_UI_PREVIEW"] == "1"
        self.commandRunner = commandRunner
        snapshot = previewMode ? .previewRunning : .firstRun
    }

    func refresh(completion: @escaping (HubSnapshot) -> Void) {
        guard !previewMode else {
            DispatchQueue.main.async { completion(self.snapshot) }
            return
        }
        serviceRunner.loadedState { [weak self] loaded in
            guard let self else { return }
            self.commandRunner.run(arguments: ["--config", self.configPath.path, "status"]) { result in
                DispatchQueue.main.async {
                    let installed = self.isServiceInstalled
                    if case let .success(output) = result, let status = self.parseStatus(output) {
                        self.snapshot = self.statusSnapshot(status, installed: installed, loaded: loaded)
                    } else {
                        self.snapshot = self.fallbackSnapshot(installed: installed, loaded: loaded)
                    }
                    completion(self.snapshot)
                }
            }
        }
    }

    func installService(completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        let finish: (Result<String, Error>) -> Void = { [weak self] result in
            DispatchQueue.main.async {
                switch result {
                case .success:
                    self?.refresh { _ in completion(.success(())) }
                case let .failure(error): completion(.failure(error))
                }
            }
        }
        installer.install(completion: finish)
    }

    func importTeslaMate(source: String, carID: String, passwordFile: String, encryptionKeyFile: String, completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        do {
            try ensureConfig()
        } catch {
            completion(.failure(error))
            return
        }
        let arguments = ["--config", configPath.path, "migrate", "--source", source, "--car-id", carID,
                         "--postgres-password-file", passwordFile, "--encryption-key-file", encryptionKeyFile]
        commandRunner.run(arguments: arguments, stdin: "n\n") { [weak self] result in
            DispatchQueue.main.async {
                switch result {
                case .success:
                    guard let self, self.isServiceInstalled else { completion(.success(())); return }
                    self.startHub { startResult in completion(startResult) }
                case let .failure(error): completion(.failure(error))
                }
            }
        }
    }

    func configureTeslaAccount(tokens: TeslaAuthTokens,
                               vehicleID: Int64? = nil,
                               completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        let invocation: HubSetupInvocation
        do {
            try ensureConfig()
            invocation = try Self.setupInvocation(configPath: configPath,
                                                  tokens: tokens,
                                                  vehicleID: vehicleID)
        } catch {
            completion(.failure(error))
            return
        }
        let installed = isServiceInstalled
        let finish: (Result<Void, Error>) -> Void = { [weak self] result in
            DispatchQueue.main.async {
                guard let self else { completion(result); return }
                switch result {
                case .success:
                    self.refresh { _ in completion(.success(())) }
                case .failure:
                    completion(result)
                }
            }
        }
        let runSetup = { [weak self] in
            guard let self else { return }
            self.commandRunner.run(arguments: invocation.arguments,
                                   stdin: invocation.standardInput) { result in
                switch result {
                case .success:
                    self.installer.install { install in
                        switch install {
                        case .success:
                            finish(.success(()))
                        case let .failure(installError):
                            guard installed else { finish(.failure(installError)); return }
                            self.serviceRunner.run(arguments: ["service", "start"]) { _ in
                                finish(.failure(installError))
                            }
                        }
                    }
                case let .failure(setupError):
                    guard installed else { finish(.failure(setupError)); return }
                    self.serviceRunner.run(arguments: ["service", "start"]) { _ in
                        finish(.failure(setupError))
                    }
                }
            }
        }
        if installed {
            serviceRunner.run(arguments: ["service", "stop"]) { result in
                switch result {
                case .success: runSetup()
                case let .failure(error): finish(.failure(error))
                }
            }
        } else {
            runSetup()
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
            throw HubActionError.commandFailed("Could not encode Tesla login credentials.")
        }
        var arguments = ["--config", configPath.path, "setup", "--tokens-stdin"]
        if let vehicleID {
            guard vehicleID > 0 else {
                throw HubActionError.commandFailed("Tesla vehicle ID must be positive.")
            }
            arguments += ["--vehicle-id", String(vehicleID)]
        }
        return HubSetupInvocation(arguments: arguments, standardInput: input)
    }

    func startHub(completion: @escaping (Result<Void, Error>) -> Void) {
        runServiceCommand(["service", "start"], completion: completion)
    }

    func stopHub(completion: @escaping (Result<Void, Error>) -> Void) {
        runServiceCommand(["service", "stop"], completion: completion)
    }

    func restartHub(completion: @escaping (Result<Void, Error>) -> Void) {
        runServiceCommand(["service", "restart"], completion: completion)
    }

    func logs(completion: @escaping (String) -> Void) {
        if previewMode {
            completion("Preview mode\n\n[INFO] Teslatlas Hub is running in the background.\n[INFO] Vehicle went offline\n[INFO] Position stored\n")
            return
        }
        DispatchQueue.global(qos: .utility).async {
            let folder = URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/Logs/Teslatlas Hub", isDirectory: true)
            let files = [folder.appendingPathComponent("hub.out.log"), folder.appendingPathComponent("hub.err.log")]
            let contents = files.compactMap { Self.tail(of: $0, maximumBytes: 128 * 1024) }
            let text = contents.isEmpty ? "No Hub logs are available yet.\n" : contents.joined(separator: "\n")
            DispatchQueue.main.async { completion(text) }
        }
    }

    func diagnostics() -> [String] {
        snapshot.diagnosticLines
    }

    func showDataFolder() {
        guard !previewMode, let dataDirectory = snapshot.dataDirectory else { return }
        NSWorkspace.shared.open(dataDirectory)
    }

    private func runServiceCommand(_ arguments: [String], completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        serviceRunner.run(arguments: arguments) { result in
            DispatchQueue.main.async {
                switch result {
                case .success: completion(.success(()))
                case let .failure(error): completion(.failure(error))
                }
            }
        }
    }

    private var isServiceInstalled: Bool {
        let binary = "/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
        let plist = URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/LaunchAgents/com.teslatlas.hub.plist").path
        return FileManager.default.isExecutableFile(atPath: binary) && FileManager.default.fileExists(atPath: plist)
    }

    private var configPath: URL {
        URL(fileURLWithPath: NSHomeDirectory())
            .appendingPathComponent("Library/Application Support/Teslatlas Hub", isDirectory: true)
            .appendingPathComponent("config.toml")
    }

    private var dataDirectory: URL { configPath.deletingLastPathComponent().appendingPathComponent("data", isDirectory: true) }

    private func ensureConfig() throws {
        let manager = FileManager.default
        let configFolder = configPath.deletingLastPathComponent()
        try manager.createDirectory(at: configFolder, withIntermediateDirectories: true)
        try manager.setAttributes([.posixPermissions: NSNumber(value: 0o700)], ofItemAtPath: configFolder.path)
        try manager.createDirectory(at: dataDirectory, withIntermediateDirectories: true)
        try manager.setAttributes([.posixPermissions: NSNumber(value: 0o700)], ofItemAtPath: dataDirectory.path)
        guard !manager.fileExists(atPath: configPath.path) else { return }
        let content = "data_dir = '\(dataDirectory.path)'\nbind = '127.0.0.1:8080'\n"
        let temporary = configFolder.appendingPathComponent(".config.\(UUID().uuidString).tmp")
        try Data(content.utf8).write(to: temporary)
        try manager.setAttributes([.posixPermissions: NSNumber(value: 0o600)], ofItemAtPath: temporary.path)
        do { try manager.moveItem(at: temporary, to: configPath) }
        catch { try? manager.removeItem(at: temporary); throw error }
    }

    private static func tail(of url: URL, maximumBytes: Int) -> String? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        let size = (try? handle.seekToEnd()) ?? 0
        try? handle.seek(toOffset: size > UInt64(maximumBytes) ? size - UInt64(maximumBytes) : 0)
        return String(decoding: handle.readDataToEndOfFile(), as: UTF8.self)
    }

    private func parseStatus(_ output: String) -> HubSnapshot? {
        guard let data = output.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let database = root["database"] as? [String: Any]
        let vehicle = root["vehicle"] as? [String: Any]
        let credentials = root["legacyCredentials"] as? [String: Any]
        let ready = root["ready"] as? Bool ?? false
        let vehicleName = vehicle?["displayName"] as? String ?? "No configured vehicle"
        let vehicleSummary: String
        if let observed = vehicle?["latestObservedAtMs"] as? NSNumber {
            vehicleSummary = "Last seen \(relativeAge(milliseconds: observed.int64Value))"
        } else {
            vehicleSummary = vehicle == nil ? "No configured vehicle" : "No observations yet"
        }
        let account = (credentials?["present"] as? Bool == true) ? "Connected" : "Not configured"
        let dbBytes = database?["bytes"] as? NSNumber
        let dbText = dbBytes.map { "Healthy · \($0.int64Value / 1_048_576) MB" } ?? "Waiting for setup or import"
        let dataDirectory = (database?["path"] as? String).map { URL(fileURLWithPath: $0).deletingLastPathComponent() }
        let service = ready ? "Installed and running" : "Installed · needs attention"
        return HubSnapshot(health: ready ? .running : .degraded,
                           service: service,
                           account: account,
                           vehicleName: vehicleName,
                           vehicle: vehicleSummary,
                           database: dbText,
                           activity: [],
                           version: root["version"] as? String ?? HubRelease.fallbackVersion,
                           dataDirectory: dataDirectory,
                           diagnosticLines: [
                               "Service: \(service)",
                               "Account: \(account)",
                               "Vehicle: \(vehicleSummary)",
                               "Database: \(dbText)",
                               "Readiness: \(root["readinessReason"] as? String ?? "ready")"
                           ])
    }

    private func statusSnapshot(_ status: HubSnapshot, installed: Bool, loaded: HubServiceLoadState) -> HubSnapshot {
        guard installed else { return HubSnapshot(health: .needsInstall, service: "Not installed", account: status.account, vehicleName: status.vehicleName, vehicle: status.vehicle, database: status.database, activity: status.activity, version: status.version, dataDirectory: status.dataDirectory ?? dataDirectory, diagnosticLines: status.diagnosticLines) }
        var result = status
        switch loaded {
        case .loaded:
            result.health = status.health == .running ? .running : .degraded
            result.service = status.health == .running ? "Installed and running" : "Installed · needs attention"
        case .unloaded:
            result.health = .stopped
            result.service = "Installed but stopped"
        case .unknown:
            result.health = .degraded
            result.service = "Installed · service state unavailable"
        }
        return result
    }

    private func fallbackSnapshot(installed: Bool, loaded: HubServiceLoadState) -> HubSnapshot {
        guard installed else { return .firstRun }
        let health: HubHealth
        let service: String
        switch loaded {
        case .loaded: health = .degraded; service = "Installed · status unavailable"
        case .unloaded: health = .stopped; service = "Installed but stopped"
        case .unknown: health = .degraded; service = "Installed · service state unavailable"
        }
        return HubSnapshot(health: health, service: service, account: "Unknown", vehicleName: "Vehicle", vehicle: "Unknown", database: "Unknown", activity: [], version: HubRelease.fallbackVersion, dataDirectory: dataDirectory, diagnosticLines: [service, "Hub status command did not return a valid report."])
    }

    private func relativeAge(milliseconds: Int64) -> String {
        let seconds = max(0, Int(Date().timeIntervalSince1970 - Double(milliseconds) / 1_000))
        if seconds < 60 { return "just now" }
        if seconds < 3_600 { return "\(seconds / 60) minutes ago" }
        if seconds < 86_400 { return "\(seconds / 3_600) hours ago" }
        return "\(seconds / 86_400) days ago"
    }
}
