import AppKit
import Darwin
import Foundation

private enum HubRelease {
    static let fallbackVersion = "1.0.0-alpha.1"
    static var bundledVersion: String {
        guard let value = Bundle.main.object(forInfoDictionaryKey: "TeslatlasHubVersion") as? String,
              !value.isEmpty,
              !value.contains("$(") else { return fallbackVersion }
        return value
    }
}

enum HubShareRedactor {
    private static let replacements: [(pattern: String, template: String)] = [
        (#"(?i)(authorization\s*[:=]\s*(?:bearer|basic)\s+)[^\s,;]+"#, "$1[redacted]"),
        (#"(?i)(\b(?:access_?token|refresh_?token|client_?secret|encryption_?key|password|authorization_?code|oauth_?code)\b\s*[\"']?\s*[:=]\s*[\"']?)[^\"'\s,&;}]+"#, "$1[redacted]"),
        (#"(?i)([?&](?:access_token|refresh_token|client_secret|code)=)[^&#\s]+"#, "$1[redacted]"),
        (#"(?i)((?:postgres(?:ql)?|https?)://[^/\s:@]+:)[^@/\s]+(@)"#, "$1[redacted]$2"),
        (#"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b"#, "[redacted-jwt]")
    ]

    static func redact(_ text: String, homeDirectory: String = NSHomeDirectory()) -> String {
        var redacted = homeDirectory.isEmpty
            ? text
            : text.replacingOccurrences(of: homeDirectory, with: "~")
        for replacement in replacements {
            guard let expression = try? NSRegularExpression(pattern: replacement.pattern) else {
                continue
            }
            let range = NSRange(redacted.startIndex..<redacted.endIndex, in: redacted)
            redacted = expression.stringByReplacingMatches(
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
        case .legacy: return "Legacy token"
        case .fleet: return "Fleet API"
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

    init(phase: HubMigrationHandoverPhase,
         previousIntervalSeconds: Int,
         previousProvider: String? = nil) {
        self.phase = phase
        self.previousIntervalSeconds = previousIntervalSeconds
        self.previousProvider = previousProvider
    }
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
        case .wake: return "Wake Vehicle"
        case .climateStart: return "Start Climate"
        case .climateStop: return "Stop Climate"
        case .lock: return "Lock Doors"
        case .unlock: return "Unlock Doors"
        case .flashLights: return "Flash Lights"
        case .honkHorn: return "Honk Horn"
        }
    }

    var acceptedMessage: String {
        switch self {
        case .wake: return "Check the vehicle to confirm it woke."
        case .climateStart, .climateStop: return "Check the vehicle to confirm the climate changed."
        case .lock, .unlock: return "Check the vehicle to confirm the doors changed."
        case .flashLights: return "The flash-lights command was accepted."
        case .honkHorn: return "The honk-horn command was accepted."
        }
    }
}

struct HubControlVehicle: Equatable {
    let id: UUID
    let displayName: String
    let status: String
}

struct HubSnapshot {
    var health: HubHealth
    var service: String
    var account: String
    var provider: HubAccountProvider?
    var vehicleName: String
    var vehicle: String
    var controlVehicleID: UUID?
    var controlVehicles: [HubControlVehicle]
    var database: String
    var activity: [HubActivity]
    var version: String
    var dataDirectory: URL?
    var diagnosticLines: [String]

    var accountDisplay: String {
        guard account == "Connected", let provider else { return account }
        return "\(account) · \(provider.displayName)"
    }

    static let previewRunning = HubSnapshot(
        health: .running,
        service: "Installed and running",
        account: "Connected",
        provider: .fleet,
        vehicleName: "Athena",
        vehicle: "Online · seen just now",
        controlVehicleID: nil,
        controlVehicles: [],
        database: "Healthy · 18,426 records",
        activity: [
            HubActivity(message: "Vehicle online", age: "just now", color: .systemGreen),
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
        provider: nil,
        vehicleName: "Vehicle",
        vehicle: "No configured vehicle",
        controlVehicleID: nil,
        controlVehicles: [],
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
    case untrustedInstaller(String)
    case commandFailed(String)
    case commandExited(Int32, String)
    case commandTimedOut

    var errorDescription: String? {
        switch self {
        case .preview:
            return "Preview mode is read-only. No process, installer, or launchctl action was run."
        case let .missingResource(name):
            return "Embedded resource is missing: \(name)"
        case let .untrustedInstaller(reason):
            return "Installer trust check failed: \(reason)"
        case let .commandFailed(message):
            return message
        case let .commandExited(_, message):
            return message
        case .commandTimedOut:
            return "Hub command timed out."
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

enum HubProcessExecutor {
    static let defaultMaximumOutputBytes = 256 * 1024
    static let defaultTimeout: TimeInterval = 5 * 60
    static let defaultTerminationGrace: TimeInterval = 2
    static let defaultOutputDrainTimeout: TimeInterval = 2

    static func run(executable: URL,
                    arguments: [String],
                    stdin: String? = nil,
                    environment: [String: String]? = nil,
                    maximumOutputBytes: Int = defaultMaximumOutputBytes,
                    timeout: TimeInterval = defaultTimeout,
                    terminationGrace: TimeInterval = defaultTerminationGrace,
                    outputDrainTimeout: TimeInterval = defaultOutputDrainTimeout,
                    completion: @escaping (Result<String, Error>) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async {
            let process = Process()
            let output = Pipe()
            let retained = BoundedProcessOutput(maximumBytes: maximumOutputBytes)
            let reader = DispatchGroup()
            let terminated = DispatchSemaphore(value: 0)
            process.executableURL = executable
            process.arguments = arguments
            if let environment {
                process.environment = ProcessInfo.processInfo.environment.merging(environment) { _, new in new }
            }
            process.standardOutput = output
            process.standardError = output
            if stdin != nil { process.standardInput = Pipe() }
            process.terminationHandler = { _ in terminated.signal() }
            do {
                try process.run()
                reader.enter()
                DispatchQueue.global(qos: .userInitiated).async {
                    while true {
                        let chunk = output.fileHandleForReading.readData(ofLength: 16 * 1024)
                        if chunk.isEmpty { break }
                        retained.append(chunk)
                    }
                    reader.leave()
                }
                if let stdin, let input = process.standardInput as? Pipe {
                    input.fileHandleForWriting.write(Data(stdin.utf8))
                    input.fileHandleForWriting.closeFile()
                }
                if terminated.wait(timeout: .now() + max(0.001, timeout)) == .timedOut {
                    let pid = process.processIdentifier
                    if process.isRunning { process.terminate() }
                    if terminated.wait(timeout: .now() + max(0.001, terminationGrace)) == .timedOut {
                        if process.isRunning { Darwin.kill(pid, SIGKILL) }
                        _ = terminated.wait(timeout: .now() + max(0.001, terminationGrace))
                    }
                    _ = reader.wait(timeout: .now() + max(0.001, outputDrainTimeout))
                    completion(.failure(HubActionError.commandTimedOut))
                    return
                }
                guard reader.wait(timeout: .now() + max(0.001, outputDrainTimeout)) == .success else {
                    completion(.failure(HubActionError.commandFailed("Hub command output did not close.")))
                    return
                }
                let text = String(decoding: retained.snapshot(), as: UTF8.self)
                if process.terminationStatus == 0 {
                    completion(.success(text))
                } else {
                    completion(.failure(HubActionError.commandExited(
                        process.terminationStatus,
                        text.isEmpty ? "Hub command failed." : text
                    )))
                }
            } catch {
                completion(.failure(error))
            }
        }
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
        HubProcessExecutor.run(executable: executable,
                               arguments: arguments,
                               stdin: stdin,
                               maximumOutputBytes: Self.maximumOutputBytes(for: arguments),
                               timeout: Self.timeout(for: arguments),
                               completion: completion)
    }

    private static func timeout(for arguments: [String]) -> TimeInterval {
        if arguments.contains("migrate") { return 24 * 60 * 60 }
        if arguments.contains("doctor") { return 15 * 60 }
        if arguments.contains("teslamate-check") { return 5 * 60 }
        if arguments.contains("setup") { return 5 * 60 }
        if arguments.contains("status") || arguments.contains("preflight") { return 30 }
        return HubProcessExecutor.defaultTimeout
    }

    private static func maximumOutputBytes(for arguments: [String]) -> Int {
        if arguments.contains("doctor") { return 1024 * 1024 }
        return HubProcessExecutor.defaultMaximumOutputBytes
    }
}

final class InstalledHubCommandRunner: HubCommandRunning {
    private let executable = URL(fileURLWithPath: "/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub")

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        runProcess(arguments: arguments, stdin: nil, completion: completion)
    }

    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        runProcess(arguments: arguments, stdin: stdin, completion: completion)
    }

    private func runProcess(arguments: [String], stdin: String?, completion: @escaping (Result<String, Error>) -> Void) {
        let timeout: TimeInterval
        if arguments.contains("migrate") {
            timeout = 24 * 60 * 60
        } else if arguments.contains("doctor") {
            timeout = 15 * 60
        } else if arguments.contains("teslamate-check") || arguments.contains("setup") {
            timeout = 5 * 60
        } else if arguments.contains("control") {
            timeout = 45
        } else {
            timeout = 30
        }
        let maximumOutputBytes = arguments.contains("doctor")
            ? 1024 * 1024
            : HubProcessExecutor.defaultMaximumOutputBytes
        HubProcessExecutor.run(executable: executable,
                               arguments: arguments,
                               stdin: stdin,
                               maximumOutputBytes: maximumOutputBytes,
                               timeout: timeout,
                               completion: completion)
    }
}

protocol HubInstalling {
    func install(completion: @escaping (Result<String, Error>) -> Void)
    func uninstall(deleteData: Bool, completion: @escaping (Result<String, Error>) -> Void)
}

final class EmbeddedInstaller: HubInstalling {
    func install(completion: @escaping (Result<String, Error>) -> Void) {
        guard let package = Bundle.main.url(forResource: "TeslatlasHubService", withExtension: "pkg") else {
            completion(.failure(HubActionError.missingResource("TeslatlasHubService.pkg")))
            return
        }
        guard let digest = Bundle.main.object(forInfoDictionaryKey: "TeslatlasServicePackageSHA256") as? String,
              let teamID = Bundle.main.object(forInfoDictionaryKey: "TeslatlasReleaseTeamIdentifier") as? String,
              Bundle.main.object(forInfoDictionaryKey: "TeslatlasOfficialRelease") as? Bool == true else {
            completion(.failure(HubActionError.untrustedInstaller(
                "signed release metadata is missing; local ad-hoc builds cannot install or update the service"
            )))
            return
        }
        do {
            let command = try Self.installCommand(packagePath: package.path,
                                                  appPath: Bundle.main.bundleURL.path,
                                                  expectedSHA256: digest,
                                                  expectedTeamID: teamID)
            runAdministratorCommand(command, completion: completion)
        } catch {
            completion(.failure(error))
        }
    }

    func uninstall(deleteData: Bool, completion: @escaping (Result<String, Error>) -> Void) {
        runAdministratorCommand(Self.uninstallCommand(deleteData: deleteData),
                                completion: completion)
    }

    static func installCommand(packagePath: String,
                               appPath: String,
                               expectedSHA256: String,
                               expectedTeamID: String) throws -> String {
        guard expectedSHA256.count == 64,
              expectedSHA256.unicodeScalars.allSatisfy({
                  (48 ... 57).contains($0.value) || (97 ... 102).contains($0.value)
              }) else {
            throw HubActionError.untrustedInstaller("package digest metadata is invalid")
        }
        guard expectedTeamID.count == 10,
              expectedTeamID.unicodeScalars.allSatisfy({
                  (48 ... 57).contains($0.value) || (65 ... 90).contains($0.value)
              }) else {
            throw HubActionError.untrustedInstaller("Team ID metadata is invalid")
        }
        return "staging=$(/usr/bin/mktemp -d /private/var/tmp/teslatlas-hub-install.XXXXXX)" +
            " || exit 1; " +
            "trap '/usr/bin/find -x \"$staging\" -depth -delete' EXIT HUP INT TERM; " +
            "app=\(shellQuote(appPath)); package=\(shellQuote(packagePath)); " +
            "expected_sha=\(shellQuote(expectedSHA256)); expected_team=\(shellQuote(expectedTeamID)); " +
            "staged=\"$staging/TeslatlasHubService.pkg\"; " +
            "/usr/bin/test \"$(/usr/bin/stat -f '%u:%g:%Lp' \"$staging\")\" = 0:0:700" +
            " && /usr/bin/test -d \"$app\" && /usr/bin/test ! -L \"$app\"" +
            " && /usr/bin/test \"$package\" = \"$app/Contents/Resources/TeslatlasHubService.pkg\"" +
            " && /usr/bin/test -f \"$package\" && /usr/bin/test ! -L \"$package\"" +
            " && /usr/bin/codesign --verify --deep --strict --verbose=2 \"$app\" >/dev/null 2>&1" +
            " && /usr/sbin/spctl --assess --type execute --verbose=4 \"$app\" >/dev/null 2>&1" +
            " && app_team=$(/usr/bin/codesign -dv --verbose=4 \"$app\" 2>&1" +
            " | /usr/bin/awk -F= '$1 == \"TeamIdentifier\" { print $2; exit }')" +
            " && /usr/bin/test \"$app_team\" = \"$expected_team\"" +
            " && /usr/bin/test \"$(/usr/libexec/PlistBuddy -c 'Print :TeslatlasServicePackageSHA256' \"$app/Contents/Info.plist\")\" = \"$expected_sha\"" +
            " && /usr/bin/test \"$(/usr/libexec/PlistBuddy -c 'Print :TeslatlasReleaseTeamIdentifier' \"$app/Contents/Info.plist\")\" = \"$expected_team\"" +
            " && /usr/bin/test \"$(/usr/libexec/PlistBuddy -c 'Print :TeslatlasOfficialRelease' \"$app/Contents/Info.plist\")\" = true" +
            " && /usr/bin/install -o root -g wheel -m 0600 \"$package\" \"$staged\"" +
            " && /usr/bin/test -f \"$staged\" && /usr/bin/test ! -L \"$staged\"" +
            " && /usr/bin/test \"$(/usr/bin/stat -f '%u:%g:%Lp' \"$staged\")\" = 0:0:600" +
            " && actual_sha=$(/usr/bin/shasum -a 256 \"$staged\" | /usr/bin/awk '{ print $1 }')" +
            " && /usr/bin/test \"$actual_sha\" = \"$expected_sha\"" +
            " && /usr/sbin/pkgutil --check-signature \"$staged\" >/dev/null 2>&1" +
            " && /usr/sbin/spctl --assess --type install \"$staged\" >/dev/null 2>&1" +
            " && package_team=$(/usr/sbin/spctl --assess --type install --verbose=4 \"$staged\" 2>&1" +
            " | /usr/bin/sed -nE 's/^origin=.*\\(([A-Z0-9]{10})\\)$/\\1/p')" +
            " && /usr/bin/test \"$package_team\" = \"$expected_team\"" +
            " && /usr/sbin/installer -pkg \"$staged\" -target /"
    }

    static func uninstallCommand(deleteData: Bool) -> String {
        let option = deleteData ? " --delete-data" : ""
        let root = "/Library/Application Support/Teslatlas Hub"
        return "root=\(shellQuote(root)); libexec=\"$root/libexec\"; " +
            "uninstaller=\"$libexec/uninstall-macos-service.sh\"; common=\"$libexec/common.sh\"; " +
            "/usr/bin/test -d \"$root\" && /usr/bin/test ! -L \"$root\"" +
            " && /usr/bin/test \"$(/usr/bin/stat -f '%u:%g' \"$root\")\" = 0:0" +
            " && /usr/bin/test -z \"$(/usr/bin/find \"$root\" -prune -perm +022 -print)\"" +
            " && /usr/bin/test -d \"$libexec\" && /usr/bin/test ! -L \"$libexec\"" +
            " && /usr/bin/test \"$(/usr/bin/stat -f '%u:%g' \"$libexec\")\" = 0:0" +
            " && /usr/bin/test -z \"$(/usr/bin/find \"$libexec\" -prune -perm +022 -print)\"" +
            " && /usr/bin/test -f \"$uninstaller\" && /usr/bin/test ! -L \"$uninstaller\"" +
            " && /usr/bin/test -x \"$uninstaller\"" +
            " && /usr/bin/test -f \"$common\" && /usr/bin/test ! -L \"$common\"" +
            " && /usr/bin/test \"$(/usr/bin/stat -f '%u:%g' \"$uninstaller\")\" = 0:0" +
            " && /usr/bin/test \"$(/usr/bin/stat -f '%u:%g' \"$common\")\" = 0:0" +
            " && /usr/bin/test -z \"$(/usr/bin/find \"$uninstaller\" \"$common\" -prune -perm +022 -print)\"" +
            " && /bin/sh \"$uninstaller\"\(option)"
    }

    private func runAdministratorCommand(_ command: String,
                                         completion: @escaping (Result<String, Error>) -> Void) {
        let script = "do shell script \"\(Self.appleScriptQuote(command))\" with administrator privileges"
        HubProcessExecutor.run(executable: URL(fileURLWithPath: "/usr/bin/osascript"),
                               arguments: ["-e", script],
                               timeout: 15 * 60,
                               completion: completion)
    }

    private static func shellQuote(_ value: String) -> String { "'\(value.replacingOccurrences(of: "'", with: "'\\''"))'" }
    private static func appleScriptQuote(_ value: String) -> String { value.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"") }
}

private enum ProcessRunner {
    static func run(executable: URL, arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        HubProcessExecutor.run(executable: executable,
                               arguments: arguments,
                               timeout: 30,
                               completion: completion)
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

protocol HubServiceControlling: HubCommandRunning {
    func loadedState(completion: @escaping (HubServiceLoadState) -> Void)
}

final class LaunchctlServiceController: HubServiceControlling {
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
                if case let HubActionError.commandExited(status, output) = error,
                   Self.isKnownUnloadedPrintFailure(status: status, output: output, service: service) {
                    completion(.success(false))
                } else {
                    completion(.failure(error))
                }
            }
        }
    }

    static func isKnownUnloadedPrintFailure(status: Int32, output: String, service: String) -> Bool {
        guard status == 113, let label = service.split(separator: "/").last else { return false }
        let expected = "Could not find service \"\(label)\" in domain for user gui: \(getuid())"
        return output.split(whereSeparator: \Character.isNewline).contains {
            $0.trimmingCharacters(in: .whitespacesAndNewlines) == expected
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
    let onboardingPreviewRoute: String?
    private let commandRunner: HubCommandRunning
    private let installedCommandRunner: HubCommandRunning
    private let installer: HubInstalling
    private let serviceRunner: HubServiceControlling
    private let homeDirectory: URL
    private let serviceInstalledOverride: Bool?
    private(set) var snapshot: HubSnapshot

    init(environment: [String: String] = ProcessInfo.processInfo.environment,
         commandRunner: HubCommandRunning = EmbeddedHubCommandRunner(),
         installedCommandRunner: HubCommandRunning = InstalledHubCommandRunner(),
         installer: HubInstalling = EmbeddedInstaller(),
         serviceRunner: HubServiceControlling = LaunchctlServiceController(),
         homeDirectory: URL = URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true),
         serviceInstalledOverride: Bool? = nil,
         initialSnapshot: HubSnapshot? = nil) {
        previewMode = environment["TESLATLAS_HUB_UI_PREVIEW"] == "1"
        onboardingPreviewRoute = environment["TESLATLAS_HUB_ONBOARDING_PREVIEW"]
        self.commandRunner = commandRunner
        self.installedCommandRunner = installedCommandRunner
        self.installer = installer
        self.serviceRunner = serviceRunner
        self.homeDirectory = homeDirectory
        self.serviceInstalledOverride = serviceInstalledOverride
        snapshot = initialSnapshot ?? (previewMode ? .previewRunning : .firstRun)
    }

    static func isBundledServiceVersionOutput(_ output: String) -> Bool {
        output.trimmingCharacters(in: .whitespacesAndNewlines)
            == "teslatlas-hub \(HubRelease.bundledVersion)"
    }

    private func installedServiceMatchesBundledVersion(completion: @escaping (Bool) -> Void) {
        commandRunner.run(arguments: ["--version"]) { [weak self] bundledResult in
            guard let self,
                  case let .success(bundledOutput) = bundledResult,
                  Self.isBundledServiceVersionOutput(bundledOutput) else {
                completion(false)
                return
            }
            self.installedCommandRunner.run(arguments: ["--version"]) { installedResult in
                guard case let .success(installedOutput) = installedResult else {
                    completion(false)
                    return
                }
                completion(
                    installedOutput.trimmingCharacters(in: .whitespacesAndNewlines)
                        == bundledOutput.trimmingCharacters(in: .whitespacesAndNewlines)
                )
            }
        }
    }

    var hasPendingMigrationHandover: Bool {
        !previewMode && FileManager.default.fileExists(atPath: migrationHandoverMarker.path)
    }

    var pendingMigrationHandoverPhase: HubMigrationHandoverPhase? {
        previewMode ? nil : migrationHandoverState?.phase
    }

    func shouldShowOnboarding(for snapshot: HubSnapshot) -> Bool {
        onboardingPreviewRoute != nil
            || hasPendingMigrationHandover
            || !FileManager.default.fileExists(atPath: configPath.path)
            || snapshot.health == .needsInstall
            || snapshot.account == "Not configured"
    }

    func refresh(completion: @escaping (HubSnapshot) -> Void) {
        guard !previewMode else {
            completion(snapshot)
            return
        }
        serviceRunner.loadedState { [weak self] loaded in
            guard let self else { return }
            let installed = self.isServiceInstalled
            let runner = installed ? self.installedCommandRunner : self.commandRunner
            runner.run(arguments: ["--config", self.configPath.path, "status"]) { result in
                DispatchQueue.main.async {
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

    func uninstallService(deleteData: Bool, completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        installer.uninstall(deleteData: deleteData) { result in
            DispatchQueue.main.async {
                switch result {
                case .success:
                    self.refresh { _ in completion(.success(())) }
                case let .failure(error):
                    completion(.failure(error))
                }
            }
        }
    }

    func signOutTeslaAccount(completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        guard !hasPendingMigrationHandover else {
            completion(.failure(HubActionError.commandFailed(
                "Finish or cancel the TeslaMate migration handover before signing out."
            )))
            return
        }
        HubAppLog.shared.record("sign_out.requested", category: "account")
        let runner = isServiceInstalled ? installedCommandRunner : commandRunner
        runner.run(arguments: ["--config", configPath.path, "control", "sign-out"]) { [weak self] result in
            DispatchQueue.main.async {
                guard let self else { completion(result.map { _ in () }); return }
                switch result {
                case .success:
                    HubAppLog.shared.record("sign_out.completed", category: "account")
                    self.refresh { _ in completion(.success(())) }
                case let .failure(error):
                    HubAppLog.shared.record("sign_out.failed", category: "account", level: "ERROR",
                                            fields: ["error_code": HubAppLog.errorCode(error)])
                    self.refresh { _ in
                        completion(.failure(HubActionError.commandFailed(
                            "Disconnect did not finish cleanly. Hub status was refreshed; check whether the account is still connected. \(error.localizedDescription)"
                        )))
                    }
                }
            }
        }
    }

    func checkTeslaMateCompatibility(source: String,
                                     carID: String,
                                     passwordFile: String,
                                     completion: @escaping (Result<HubTeslaMateCompatibility, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        do {
            try Self.validateMigrationSource(source)
            guard let selectedCarID = Int64(carID), selectedCarID > 0 else {
                throw HubActionError.commandFailed("TeslaMate car ID must be a positive number.")
            }
            guard !passwordFile.isEmpty else {
                throw HubActionError.commandFailed("Choose the protected PostgreSQL password file.")
            }
            let arguments = ["--config", configPath.path, "teslamate-check",
                             "--source", source, "--car-id", String(selectedCarID),
                             "--postgres-password-file", passwordFile]
            commandRunner.run(arguments: arguments) { result in
                DispatchQueue.main.async {
                    switch result {
                    case let .success(output):
                        guard let report = Self.parseTeslaMateCompatibility(output) else {
                            completion(.failure(HubActionError.commandFailed(
                                "TeslaMate compatibility check returned no valid report."
                            )))
                            return
                        }
                        completion(.success(report))
                    case let .failure(error):
                        if let report = Self.parseTeslaMateCompatibility(error.localizedDescription) {
                            completion(.success(report))
                        } else {
                            completion(.failure(error))
                        }
                    }
                }
            }
        } catch {
            completion(.failure(error))
        }
    }

    func importTeslaMateOnline(source: String,
                               carID: String,
                               passwordFile: String,
                               encryptionKeyFile: String,
                               completion: @escaping (Result<Void, Error>) -> Void) {
        HubAppLog.shared.record("import.requested", category: "teslamate_import",
                                fields: ["capture_mode": "online_snapshot"])
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        guard !encryptionKeyFile.isEmpty else {
            completion(.failure(HubActionError.commandFailed(
                "Choose the TeslaMate ENCRYPTION_KEY file."
            )))
            return
        }
        checkTeslaMateCompatibility(source: source,
                                    carID: carID,
                                    passwordFile: passwordFile) { [weak self] checkResult in
            guard let self else { return }
            switch checkResult {
            case let .failure(error):
                HubAppLog.shared.record("import.preflight.failed", category: "teslamate_import",
                                        level: "ERROR",
                                        fields: ["error_code": HubAppLog.errorCode(error)])
                completion(.failure(error))
            case let .success(report):
                guard report.compatible else {
                    HubAppLog.shared.record("import.preflight.rejected", category: "teslamate_import",
                                            level: "WARN", fields: ["reason": report.reasonCode])
                    completion(.failure(HubActionError.commandFailed(report.message)))
                    return
                }
                HubAppLog.shared.record("import.preflight.completed", category: "teslamate_import")
                self.prepareOnlineMigration(source: source,
                                            carID: carID,
                                            passwordFile: passwordFile,
                                            encryptionKeyFile: encryptionKeyFile,
                                            completion: completion)
            }
        }
    }

    private func prepareOnlineMigration(source: String,
                                        carID: String,
                                        passwordFile: String,
                                        encryptionKeyFile: String,
                                        completion: @escaping (Result<Void, Error>) -> Void) {
        let previousInterval = migrationHandoverState?.previousIntervalSeconds
            ?? configuredCollectorIntervalSeconds()
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
                                          previousProvider: previousProvider)
            )
            try ensureConfig(collectorIntervalSeconds: 0)
        } catch {
            completion(.failure(recoverFailedImport(error, originalConfig: originalConfig)))
            return
        }
        let arguments = ["--config", configPath.path, "migrate",
                         "--source", source, "--car-id", carID,
                         "--postgres-password-file", passwordFile,
                         "--encryption-key-file", encryptionKeyFile,
                         "--online-snapshot"]
        let finish: (Result<Void, Error>) -> Void = { result in
            DispatchQueue.main.async { completion(result) }
        }
        let abortBeforeImport: (Error) -> Void = { [weak self] error in
            guard let self else { finish(.failure(error)); return }
            finish(.failure(self.recoverFailedImport(error, originalConfig: originalConfig)))
        }
        let failStartedImport: (Error) -> Void = { error in
            finish(.failure(Self.migrationStoppedError(error)))
        }
        let runImport = { [weak self] in
            guard let self else { return }
            HubAppLog.shared.record("import.process.started", category: "teslamate_import",
                                    fields: ["capture_mode": "online_snapshot"])
            self.commandRunner.run(arguments: arguments) { result in
                switch result {
                case let .success(output):
                    guard Self.containsOnlineMigrationReport(output) else {
                        HubAppLog.shared.record("import.process.invalid_report",
                                                category: "teslamate_import", level: "ERROR")
                        failStartedImport(HubActionError.commandFailed(
                            "TeslaMate import returned no valid completion report. Hub remains stopped."
                        ))
                        return
                    }
                    do {
                        try self.writeMigrationHandoverMarker(
                            HubMigrationHandoverState(phase: .awaitingVerification,
                                                      previousIntervalSeconds: previousInterval,
                                                      previousProvider: previousProvider)
                        )
                        HubAppLog.shared.record("import.completed", category: "teslamate_import",
                                                fields: ["handover": "awaiting_verification"])
                        finish(.success(()))
                    } catch {
                        HubAppLog.shared.record("import.handover.failed", category: "teslamate_import",
                                                level: "ERROR",
                                                fields: ["error_code": HubAppLog.errorCode(error)])
                        finish(.failure(HubActionError.commandFailed(
                            "Import completed, but Hub could not record the safe handover gate: \(error.localizedDescription). Hub remains stopped."
                        )))
                    }
                case let .failure(error):
                    HubAppLog.shared.record("import.process.failed", category: "teslamate_import",
                                            level: "ERROR",
                                            fields: ["error_code": HubAppLog.errorCode(error)])
                    failStartedImport(error)
                }
            }
        }
        // This controls Teslatlas Hub only. TeslaMate is never stopped or changed.
        serviceRunner.run(arguments: ["service", "stop"]) { stopResult in
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

    static func parseTeslaMateCompatibility(_ output: String) -> HubTeslaMateCompatibility? {
        for line in output.split(whereSeparator: { $0.isNewline }) {
            guard let data = String(line).data(using: .utf8),
                  let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let status = root["status"] as? String,
                  let reason = root["reasonCode"] as? String,
                  let required = root["requiredVersion"] as? String,
                  let guidance = root["guidance"] as? String else { continue }
            return HubTeslaMateCompatibility(
                compatible: status == "compatible"
                    && reason == "exact_4_1_1"
                    && required == "4.1.1",
                message: guidance,
                reasonCode: reason,
                requiredVersion: required
            )
        }
        return nil
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

    func importTeslaMate(source: String, carID: String, passwordFile: String, encryptionKeyFile: String, completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        do {
            try Self.validateMigrationSource(source)
        } catch {
            completion(.failure(error))
            return
        }
        let previousInterval = migrationHandoverState?.previousIntervalSeconds
            ?? configuredCollectorIntervalSeconds()
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
                                          previousProvider: previousProvider)
            )
            try ensureConfig(collectorIntervalSeconds: 0)
        } catch {
            completion(.failure(recoverFailedImport(error, originalConfig: originalConfig)))
            return
        }
        let arguments = ["--config", configPath.path, "migrate", "--source", source, "--car-id", carID,
                         "--postgres-password-file", passwordFile, "--encryption-key-file", encryptionKeyFile]
        let finish: (Result<Void, Error>) -> Void = { result in
            DispatchQueue.main.async { completion(result) }
        }
        let abortBeforeImport: (Error) -> Void = { [weak self] error in
            guard let self else { finish(.failure(error)); return }
            finish(.failure(self.recoverFailedImport(error, originalConfig: originalConfig)))
        }
        let failStartedImport: (Error) -> Void = { error in
            finish(.failure(Self.migrationStoppedError(error)))
        }
        let runImport = { [weak self] in
            guard let self else { return }
            self.commandRunner.run(arguments: arguments, stdin: "y\nn\n") { result in
                switch result {
                case .success:
                    do {
                        try self.writeMigrationHandoverMarker(
                            HubMigrationHandoverState(phase: .awaitingVerification,
                                                      previousIntervalSeconds: previousInterval,
                                                      previousProvider: previousProvider)
                        )
                        finish(.success(()))
                    } catch {
                        finish(.failure(HubActionError.commandFailed(
                            "Import completed, but Hub could not record the safe handover gate: \(error.localizedDescription). Hub remains stopped."
                        )))
                    }
                case let .failure(error):
                    failStartedImport(error)
                }
            }
        }
        // Dashboard import never starts Hub. Collection stays stopped until the
        // same explicit handover used by onboarding.
        guard isServiceInstalled else {
            runImport()
            return
        }
        serviceRunner.run(arguments: ["service", "stop"]) { stopResult in
            switch stopResult {
            case .success: runImport()
            case let .failure(error): abortBeforeImport(error)
            }
        }
    }

    static func validateMigrationSource(_ source: String) throws {
        guard let components = URLComponents(string: source),
              let scheme = components.scheme?.lowercased(),
              scheme == "postgres" || scheme == "postgresql",
              let host = components.host, !host.isEmpty,
              components.path.count > 1 else {
            throw HubActionError.commandFailed(
                "PostgreSQL source must include a postgres or postgresql scheme, host, and database name."
            )
        }
        guard components.password == nil else {
            throw HubActionError.commandFailed("PostgreSQL source must not contain a password. Use the password file field.")
        }
    }

    func configureFleetAccount(credentials: HubFleetSetupCredentials,
                               completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        let installed = isServiceInstalled
        let invocation: HubSetupInvocation
        let originalConfig: String?
        do {
            invocation = try Self.fleetSetupInvocation(configPath: configPath,
                                                       credentials: credentials)
            originalConfig = try readConfigIfPresent()
        } catch {
            completion(.failure(error))
            return
        }
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
        let startService = { [weak self] in
            guard let self else { return }
            self.serviceRunner.run(arguments: ["service", "start"]) { result in
                switch result {
                case .success: finish(.success(()))
                case let .failure(error): finish(.failure(error))
                }
            }
        }
        guard installed else {
            do {
                try ensureConfig(provider: "fleet")
            } catch {
                finish(.failure(error))
                return
            }
            commandRunner.run(arguments: invocation.arguments,
                              stdin: invocation.standardInput) { [weak self] result in
                guard let self else { return }
                switch result {
                case .success:
                    self.installer.install { installResult in
                        switch installResult {
                        case .success:
                            self.serviceRunner.loadedState { state in
                                switch state {
                                case .loaded: finish(.success(()))
                                case .unloaded: startService()
                                case let .unknown(error): finish(.failure(error))
                                }
                            }
                        case let .failure(error): finish(.failure(error))
                        }
                    }
                case let .failure(error): finish(.failure(error))
                }
            }
            return
        }

        if snapshot.account != "Connected" {
            let restoreBeforeMutation = { [weak self] (setupError: Error) in
                guard let self else { return }
                do {
                    try self.restoreConfig(originalConfig)
                    finish(.failure(setupError))
                } catch let recoveryError {
                    finish(.failure(HubActionError.commandFailed(
                        "Fleet setup failed: \(setupError.localizedDescription) Hub configuration recovery also failed: \(recoveryError.localizedDescription)"
                    )))
                }
            }
            serviceRunner.run(arguments: ["service", "stop"]) { [weak self] stopResult in
                guard let self else { return }
                guard case .success = stopResult else {
                    if case let .failure(error) = stopResult { finish(.failure(error)) }
                    return
                }
                do {
                    try self.ensureConfig(provider: "fleet")
                } catch {
                    restoreBeforeMutation(error)
                    return
                }
                // No working old provider exists. Configure with the embedded
                // current binary, then package that same version. Never restart
                // the old binary after this point because setup may migrate data.
                self.commandRunner.run(arguments: invocation.arguments,
                                       stdin: invocation.standardInput) { setupResult in
                    switch setupResult {
                    case .success:
                        self.installedServiceMatchesBundledVersion { matches in
                            if matches {
                                HubAppLog.shared.record("installed_version.reused",
                                                        category: "service",
                                                        fields: ["provider": "fleet"])
                                startService()
                                return
                            }
                            self.installer.install { installResult in
                                switch installResult {
                                case .success: startService()
                                case let .failure(error):
                                    finish(.failure(HubActionError.commandFailed(
                                        "Fleet is configured, but the service update failed. Hub remains stopped; retry Update Service. \(error.localizedDescription)"
                                    )))
                                }
                            }
                        }
                    case let .failure(error):
                        finish(.failure(Self.providerSwitchStoppedError(error)))
                    }
                }
            }
            return
        }

        serviceRunner.loadedState { [weak self] state in
            guard let self else { return }
            let wasLoaded: Bool
            switch state {
            case .loaded: wasLoaded = true
            case .unloaded: wasLoaded = false
            case let .unknown(error):
                finish(.failure(error))
                return
            }
            let recover = { (setupError: Error, restartIsSafe: Bool) in
                guard restartIsSafe else {
                    finish(.failure(Self.providerSwitchStoppedError(setupError)))
                    return
                }
                guard let originalConfig else {
                    finish(.failure(setupError))
                    return
                }
                do {
                    try self.restoreConfig(originalConfig)
                } catch let recoveryError {
                    finish(.failure(HubActionError.commandFailed(
                        "Fleet setup failed: \(setupError.localizedDescription) Hub configuration recovery also failed: \(recoveryError.localizedDescription)"
                    )))
                    return
                }
                guard wasLoaded else {
                    finish(.failure(setupError))
                    return
                }
                self.serviceRunner.run(arguments: ["service", "start"]) { restartResult in
                    switch restartResult {
                    case .success: finish(.failure(setupError))
                    case let .failure(startError):
                        finish(.failure(HubActionError.commandFailed(
                            "Fleet setup failed: \(setupError.localizedDescription) Hub restart also failed: \(startError.localizedDescription)"
                        )))
                    }
                }
            }
            self.serviceRunner.run(arguments: ["service", "stop"]) { stopResult in
                guard case .success = stopResult else {
                    if case let .failure(error) = stopResult { finish(.failure(error)) }
                    return
                }
                // Update the package before setup. Only the newly installed binary may
                // open and migrate the database for Fleet credentials. Keep the
                // existing valid provider config through package preflight.
                self.installer.install { installResult in
                    switch installResult {
                    case .success:
                        do {
                            try self.ensureConfig(provider: "fleet")
                        } catch {
                            recover(error, true)
                            return
                        }
                        self.installedCommandRunner.run(arguments: invocation.arguments,
                                                        stdin: invocation.standardInput) { setupResult in
                            switch setupResult {
                            case .success: startService()
                            case let .failure(error):
                                recover(error, false)
                            }
                        }
                    case let .failure(error):
                        recover(error, !Self.isForwardOnlyUpgradeFailure(error))
                    }
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
            throw HubActionError.commandFailed("Fleet credentials are incomplete or invalid.")
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
            throw HubActionError.commandFailed("Could not encode Fleet credentials.")
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
        let installed = isServiceInstalled
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
            completion(.failure(error))
            return
        }
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
        let startService = { [weak self] in
            guard let self else { return }
            self.serviceRunner.run(arguments: ["service", "start"]) { result in
                finish(result.map { _ in () })
            }
        }
        guard installed else {
            do {
                try ensureConfig(provider: "legacy")
            } catch {
                finish(.failure(error))
                return
            }
            commandRunner.run(arguments: invocation.arguments,
                              stdin: invocation.standardInput) { [weak self] setupResult in
                guard let self else { return }
                switch setupResult {
                case .success:
                    self.installer.install { installResult in
                        finish(installResult.map { _ in () })
                    }
                case let .failure(error):
                    finish(.failure(error))
                }
            }
            return
        }

        if snapshot.account != "Connected" {
            let restoreBeforeMutation = { [weak self] (setupError: Error) in
                guard let self else { return }
                do {
                    try self.restoreConfig(originalConfig)
                    finish(.failure(setupError))
                } catch let recoveryError {
                    finish(.failure(HubActionError.commandFailed(
                        "Tesla setup failed: \(setupError.localizedDescription) Hub configuration recovery also failed: \(recoveryError.localizedDescription)"
                    )))
                }
            }
            serviceRunner.run(arguments: ["service", "stop"]) { [weak self] stopResult in
                guard let self else { return }
                guard case .success = stopResult else {
                    if case let .failure(error) = stopResult { finish(.failure(error)) }
                    return
                }
                do {
                    try self.ensureConfig(provider: "legacy")
                } catch {
                    restoreBeforeMutation(error)
                    return
                }
                // An installed but unconfigured package cannot pass package
                // preflight. Configure with the embedded current binary first,
                // then install that exact version and start it.
                self.commandRunner.run(arguments: invocation.arguments,
                                       stdin: invocation.standardInput) { setupResult in
                    switch setupResult {
                    case .success:
                        self.installedServiceMatchesBundledVersion { matches in
                            if matches {
                                HubAppLog.shared.record("installed_version.reused",
                                                        category: "service",
                                                        fields: ["provider": "legacy"])
                                startService()
                                return
                            }
                            self.installer.install { installResult in
                                switch installResult {
                                case .success: startService()
                                case let .failure(error):
                                    finish(.failure(HubActionError.commandFailed(
                                        "Legacy Tesla login is configured, but the service update failed. Hub remains stopped; retry Update Service. \(error.localizedDescription)"
                                    )))
                                }
                            }
                        }
                    case let .failure(error):
                        finish(.failure(Self.providerSwitchStoppedError(error)))
                    }
                }
            }
            return
        }

        serviceRunner.loadedState { [weak self] state in
            guard let self else { return }
            let wasLoaded: Bool
            switch state {
            case .loaded: wasLoaded = true
            case .unloaded: wasLoaded = false
            case let .unknown(error):
                finish(.failure(error))
                return
            }
            let recover = { (action: String, actionError: Error, restartIsSafe: Bool) in
                guard restartIsSafe else {
                    finish(.failure(Self.providerSwitchStoppedError(actionError)))
                    return
                }
                do {
                    try self.restoreConfig(originalConfig)
                } catch let recoveryError {
                    finish(.failure(HubActionError.commandFailed(
                        "\(action): \(actionError.localizedDescription) Hub configuration recovery also failed: \(recoveryError.localizedDescription)"
                    )))
                    return
                }
                guard wasLoaded else {
                    finish(.failure(actionError))
                    return
                }
                self.serviceRunner.run(arguments: ["service", "start"]) { restartResult in
                    switch restartResult {
                    case .success:
                        finish(.failure(actionError))
                    case let .failure(restartError):
                        finish(.failure(HubActionError.commandFailed(
                            "\(action): \(actionError.localizedDescription) Hub restart also failed: \(restartError.localizedDescription)"
                        )))
                    }
                }
            }
            let handleSetupResult: (Result<String, Error>) -> Void = { setupResult in
                switch setupResult {
                case .success:
                    startService()
                case let .failure(error):
                    recover("Tesla setup failed", error, false)
                }
            }
            let runSetup = {
                self.installedCommandRunner.run(arguments: invocation.arguments,
                                                stdin: invocation.standardInput) { setupResult in
                    if case let .failure(error) = setupResult,
                       Self.isAllVehiclesUnsupported(error) {
                        self.installedCommandRunner.run(arguments: installedInvocation.arguments,
                                                        stdin: installedInvocation.standardInput,
                                                        completion: handleSetupResult)
                    } else {
                        handleSetupResult(setupResult)
                    }
                }
            }
            self.serviceRunner.run(arguments: ["service", "stop"]) { stopResult in
                guard case .success = stopResult else {
                    if case let .failure(error) = stopResult { finish(.failure(error)) }
                    return
                }
                // Preserve the old provider and its credentials through package
                // verification. Only the installed current binary performs setup.
                self.installer.install { installResult in
                    switch installResult {
                    case .success:
                        do {
                            try self.ensureConfig(provider: "legacy")
                            runSetup()
                        } catch {
                            recover("Tesla setup failed", error, true)
                        }
                    case let .failure(error):
                        recover("Service update failed", error,
                                !Self.isForwardOnlyUpgradeFailure(error))
                    }
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
            throw HubActionError.commandFailed("Could not encode Tesla login credentials.")
        }
        var arguments = ["--config", configPath.path, "setup", "--tokens-stdin"]
        if let vehicleID {
            guard vehicleID > 0 else {
                throw HubActionError.commandFailed("Tesla vehicle ID must be positive.")
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

    static func isForwardOnlyUpgradeFailure(_ error: Error) -> Bool {
        error.localizedDescription.contains("TESLATLAS_FORWARD_ONLY_UPGRADE")
    }

    static func isProviderSwitchOutcomeAmbiguous(_ error: Error) -> Bool {
        error.localizedDescription.contains("TESLATLAS_PROVIDER_SWITCH_OUTCOME_AMBIGUOUS")
    }

    static func isMigrationOutcomeAmbiguous(_ error: Error) -> Bool {
        error.localizedDescription.contains("TESLATLAS_MIGRATION_OUTCOME_AMBIGUOUS")
    }

    static func providerSwitchStoppedError(_ error: Error) -> Error {
        HubActionError.commandFailed(
            "Tesla provider switch outcome needs verification. Hub remains stopped; run diagnostics before retrying. \(error.localizedDescription)"
        )
    }

    static func migrationStoppedError(_ error: Error) -> Error {
        HubActionError.commandFailed(
            "TeslaMate migration outcome needs verification. The handover gate remains and Hub remains stopped; reopen migration and run the checks again. \(error.localizedDescription)"
        )
    }

    static func isAllVehiclesUnsupported(_ error: Error) -> Bool {
        let message = error.localizedDescription.lowercased()
        return message.contains("--all-vehicles")
            && (message.contains("unexpected argument")
                || message.contains("unknown option")
                || message.contains("unrecognized option"))
    }

    func startHub(completion: @escaping (Result<Void, Error>) -> Void) {
        guard !hasPendingMigrationHandover else {
            completion(.failure(HubActionError.commandFailed(
                "Finish the TeslaMate handover before starting Hub."
            )))
            return
        }
        runServiceCommand(["service", "start"], completion: completion)
    }

    func stopHub(completion: @escaping (Result<Void, Error>) -> Void) {
        runServiceCommand(["service", "stop"], completion: completion)
    }

    func restartHub(completion: @escaping (Result<Void, Error>) -> Void) {
        guard !hasPendingMigrationHandover else {
            completion(.failure(HubActionError.commandFailed(
                "Finish the TeslaMate handover before restarting Hub."
            )))
            return
        }
        runServiceCommand(["service", "restart"], completion: completion)
    }

    func acknowledgeMigrationHandoverAndStart(completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        guard let handover = migrationHandoverState,
              handover.phase == .awaitingHandover else {
            completion(.failure(HubActionError.commandFailed(
                "Finish the migration checks before starting Hub."
            )))
            return
        }
        do {
            try ensureConfig(provider: handover.previousProvider,
                             collectorIntervalSeconds: handover.previousIntervalSeconds)
        } catch {
            completion(.failure(error))
            return
        }
        serviceRunner.run(arguments: ["service", "start"]) { [weak self] result in
            guard let self else { return }
            switch result {
            case .success:
                do {
                    try FileManager.default.removeItem(at: self.migrationHandoverMarker)
                    DispatchQueue.main.async { completion(.success(())) }
                } catch {
                    let cleanupError = error
                    self.serviceRunner.run(arguments: ["service", "stop"]) { _ in
                        do {
                            try self.ensureConfig(provider: handover.previousProvider,
                                                  collectorIntervalSeconds: 0)
                            try self.writeMigrationHandoverMarker(handover)
                        } catch {
                            // Preserve the original cleanup failure; Hub was explicitly stopped.
                        }
                        DispatchQueue.main.async {
                            completion(.failure(HubActionError.commandFailed(
                                "Hub was stopped because the migration handover gate could not be cleared: \(cleanupError.localizedDescription)"
                            )))
                        }
                    }
                }
            case let .failure(startError):
                do {
                    try self.ensureConfig(provider: handover.previousProvider,
                                          collectorIntervalSeconds: 0)
                    try self.writeMigrationHandoverMarker(handover)
                    DispatchQueue.main.async { completion(.failure(startError)) }
                } catch {
                    DispatchQueue.main.async {
                        completion(.failure(HubActionError.commandFailed(
                            "Hub did not start: \(startError.localizedDescription). The collector pause could not be restored: \(error.localizedDescription)"
                        )))
                    }
                }
            }
        }
    }

    func runOnboardingChecks(expectRunning: Bool,
                             completion: @escaping (Result<[HubOnboardingCheck], Error>) -> Void) {
        HubAppLog.shared.record("verification.started", category: "onboarding",
                                fields: ["expect_running": expectRunning ? "true" : "false"])
        if previewMode {
            let checks = [
                HubOnboardingCheck(title: "Service", detail: "Installed and running", passed: true),
                HubOnboardingCheck(title: "Tesla account", detail: "Connected", passed: true),
                HubOnboardingCheck(title: "Vehicle", detail: "Available", passed: true),
                HubOnboardingCheck(title: "Database", detail: "Healthy", passed: true),
                HubOnboardingCheck(title: "Diagnostics", detail: "Passed", passed: true),
                HubOnboardingCheck(title: "Logs", detail: "Readable", passed: true)
            ]
            DispatchQueue.main.async { completion(.success(checks)) }
            return
        }
        if !expectRunning, migrationHandoverState != nil {
            let stopAndCheck = { [weak self] in
                guard let self else { return }
                self.serviceRunner.run(arguments: ["service", "stop"]) { stopResult in
                    switch stopResult {
                    case .success:
                        self.performOnboardingChecks(expectRunning: false,
                                                     completion: completion)
                    case let .failure(error):
                        DispatchQueue.main.async { completion(.failure(error)) }
                    }
                }
            }
            guard !isServiceInstalled else {
                stopAndCheck()
                return
            }
            installer.install { installResult in
                switch installResult {
                case .success: stopAndCheck()
                case let .failure(error):
                    DispatchQueue.main.async { completion(.failure(error)) }
                }
            }
            return
        }
        performOnboardingChecks(expectRunning: expectRunning, completion: completion)
    }

    private func performOnboardingChecks(expectRunning: Bool,
                                         completion: @escaping (Result<[HubOnboardingCheck], Error>) -> Void) {
        refresh { [weak self] snapshot in
            guard let self else { return }
            let runner = self.isServiceInstalled ? self.installedCommandRunner : self.commandRunner
            runner.run(arguments: ["--config", self.configPath.path, "doctor"]) { doctorResult in
                self.logs { logText in
                    let servicePassed = expectRunning
                        ? snapshot.health == .running
                        : snapshot.health == .stopped
                    let serviceDetail = expectRunning
                        ? snapshot.service
                        : (servicePassed ? "Installed and safely stopped" : snapshot.service)
                    let vehiclePassed = snapshot.vehicleName != "Vehicle"
                        && snapshot.vehicleName != "No configured vehicle"
                        && snapshot.vehicle != "No configured vehicle"
                        && snapshot.vehicle != "Unknown"
                    let doctorPassed: Bool
                    let doctorDetail: String
                    switch doctorResult {
                    case .success:
                        doctorPassed = true
                        doctorDetail = "Passed"
                    case let .failure(error):
                        doctorPassed = false
                        doctorDetail = Self.conciseDiagnostic(error.localizedDescription)
                    }
                    let noLogsYet = logText.hasPrefix("No Hub logs are available yet.")
                    let logsPassed = expectRunning ? !noLogsYet && !logText.isEmpty : true
                    let logsDetail = noLogsYet
                        ? (expectRunning ? "No logs yet" : "No service logs while stopped")
                        : (logText.isEmpty ? "Unavailable" : "Readable")
                    let checks = [
                        HubOnboardingCheck(title: "Service", detail: serviceDetail, passed: servicePassed),
                        HubOnboardingCheck(title: "Tesla account", detail: snapshot.account,
                                           passed: snapshot.account == "Connected"),
                        HubOnboardingCheck(title: "Vehicle", detail: snapshot.vehicleName,
                                           passed: vehiclePassed),
                        HubOnboardingCheck(title: "Database", detail: snapshot.database,
                                           passed: snapshot.database.hasPrefix("Healthy")),
                        HubOnboardingCheck(title: "Diagnostics", detail: doctorDetail,
                                           passed: doctorPassed),
                        HubOnboardingCheck(title: "Logs",
                                           detail: logsDetail,
                                           passed: logsPassed)
                    ]
                    guard !expectRunning,
                          checks.allSatisfy(\.passed),
                          var handover = self.migrationHandoverState else {
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
                        try self.writeMigrationHandoverMarker(handover)
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
    }

    private static func conciseDiagnostic(_ message: String) -> String {
        let line = message.split(whereSeparator: { $0.isNewline }).first.map(String.init)
            ?? "Failed"
        return String(line.prefix(160))
    }

    func performVehicleControl(_ action: HubVehicleControl,
                               vehicleID requestedVehicleID: UUID? = nil,
                               completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        guard snapshot.health == .running else {
            completion(.failure(HubActionError.commandFailed("Hub must be running before sending a vehicle command.")))
            return
        }
        guard snapshot.account == "Connected" else {
            completion(.failure(HubActionError.commandFailed("Connect Tesla before sending a vehicle command.")))
            return
        }
        guard let vehicleID = requestedVehicleID ?? snapshot.controlVehicleID else {
            completion(.failure(HubActionError.commandFailed("Choose a vehicle before sending a command.")))
            return
        }
        guard snapshot.controlVehicles.contains(where: { $0.id == vehicleID }) else {
            completion(.failure(HubActionError.commandFailed("The selected vehicle is no longer configured.")))
            return
        }
        HubAppLog.shared.record("command.requested", category: "vehicle_control",
                                fields: ["action": action.rawValue])
        let runner = isServiceInstalled ? installedCommandRunner : commandRunner
        let arguments = ["--config", configPath.path, "control", "--vehicle-id",
                         vehicleID.uuidString.lowercased(), action.rawValue, "--confirm"]
        runner.run(arguments: arguments) { result in
            DispatchQueue.main.async {
                switch result {
                case .success:
                    HubAppLog.shared.record("command.accepted", category: "vehicle_control",
                                            fields: ["action": action.rawValue])
                    completion(.success(()))
                case let .failure(error):
                    HubAppLog.shared.record("command.failed", category: "vehicle_control", level: "ERROR",
                                            fields: [
                                                "action": action.rawValue,
                                                "error_code": HubAppLog.errorCode(error)
                                            ])
                    completion(.failure(error))
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
            let folder = self.homeDirectory.appendingPathComponent("Library/Logs/Teslatlas Hub", isDirectory: true)
            let files = [
                ("hub.out.log", folder.appendingPathComponent("hub.out.log")),
                ("hub.err.log", folder.appendingPathComponent("hub.err.log"))
            ]
            let contents = files.compactMap { name, url in
                Self.logTail(of: url, maximumBytes: maximumBytes).map { "== \(name) ==\n\($0)" }
            }
            let text = contents.isEmpty ? "No Hub logs are available yet.\n" : contents.joined(separator: "\n")
            DispatchQueue.main.async { completion(text) }
        }
    }

    func diagnostics() -> [String] {
        snapshot.diagnosticLines
    }

    func runFullDiagnostics(completion: @escaping (String) -> Void) {
        if previewMode {
            completion("Preview mode\n\n" + supportMetadata() + "\n\n"
                + snapshot.diagnosticLines.joined(separator: "\n"))
            return
        }
        let runner = isServiceInstalled ? installedCommandRunner : commandRunner
        let config = ["--config", configPath.path]
        func section(_ title: String, _ result: Result<String, Error>) -> String {
            switch result {
            case let .success(output):
                return "== \(title) ==\n\(output.trimmingCharacters(in: .whitespacesAndNewlines))\n"
            case let .failure(error):
                return "== \(title) (failed) ==\n\(error.localizedDescription)\n"
            }
        }
        runner.run(arguments: config + ["doctor"]) { doctor in
            runner.run(arguments: config + ["preflight"]) { preflight in
                runner.run(arguments: config + ["status"]) { status in
                    self.logs(maximumBytes: 512 * 1024) { logText in
                        let report = [
                            "Teslatlas Hub diagnostics",
                            "Full database, credential, connection, and log check.",
                            "TeslaMate is not written. Stored Owner and Fleet tokens are not deleted.",
                            "",
                            self.supportMetadata(),
                            "",
                            section("doctor — Hub database, tokens, TLS, collector", doctor),
                            section("preflight — selected provider credentials", preflight),
                            section("status — vehicles and credential presence", status),
                            "== recent logs ==",
                            logText.trimmingCharacters(in: .whitespacesAndNewlines)
                        ].joined(separator: "\n")
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
        return [
            "== support metadata ==",
            "App: \(appVersion) (\(appBuild))",
            "Expected Hub: \(HubRelease.bundledVersion)",
            "Observed Hub: \(snapshot.version)",
            "Service: \(serviceState)",
            "Provider: \(provider)",
            "macOS: \(ProcessInfo.processInfo.operatingSystemVersionString)",
            "Architecture: \(architecture)"
        ].joined(separator: "\n")
    }

    func showDataFolder() {
        guard !previewMode, let dataDirectory = snapshot.dataDirectory else { return }
        NSWorkspace.shared.open(dataDirectory)
    }

    private func runServiceCommand(_ arguments: [String], completion: @escaping (Result<Void, Error>) -> Void) {
        guard !previewMode else { completion(.failure(HubActionError.preview)); return }
        let action = arguments.last ?? "unknown"
        HubAppLog.shared.record("service.requested", category: "service",
                                fields: ["action": action])
        serviceRunner.run(arguments: arguments) { result in
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
                    completion(.failure(error))
                }
            }
        }
    }

    private var isServiceInstalled: Bool {
        if let serviceInstalledOverride { return serviceInstalledOverride }
        let binary = "/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
        let plist = homeDirectory.appendingPathComponent("Library/LaunchAgents/com.teslatlas.hub.plist").path
        return FileManager.default.isExecutableFile(atPath: binary) && FileManager.default.fileExists(atPath: plist)
    }

    private var configPath: URL {
        homeDirectory
            .appendingPathComponent("Library/Application Support/Teslatlas Hub", isDirectory: true)
            .appendingPathComponent("config.toml")
    }

    private var dataDirectory: URL { configPath.deletingLastPathComponent().appendingPathComponent("data", isDirectory: true) }

    private var migrationHandoverMarker: URL {
        configPath.deletingLastPathComponent().appendingPathComponent(".teslamate-handover-pending")
    }

    private var migrationHandoverState: HubMigrationHandoverState? {
        guard FileManager.default.fileExists(atPath: migrationHandoverMarker.path) else {
            return nil
        }
        guard let values = try? migrationHandoverMarker.resourceValues(
            forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey]
        ), values.isRegularFile == true, values.isSymbolicLink != true,
              let size = values.fileSize, size <= 4_096,
              let data = try? Data(contentsOf: migrationHandoverMarker),
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
            throw HubActionError.commandFailed("Hub configuration is not a regular file.")
        }
        defer { Darwin.close(descriptor) }

        var information = stat()
        guard fstat(descriptor, &information) == 0,
              information.st_mode & S_IFMT == S_IFREG else {
            throw HubActionError.commandFailed("Hub configuration is not a regular file.")
        }
        let maximumBytes = 1024 * 1024
        guard information.st_size >= 0, information.st_size <= off_t(maximumBytes) else {
            throw HubActionError.commandFailed("Hub configuration is too large.")
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
                throw HubActionError.commandFailed("Hub configuration could not be read.")
            }
            guard data.count + count <= maximumBytes else {
                throw HubActionError.commandFailed("Hub configuration is too large.")
            }
            data.append(contentsOf: buffer.prefix(count))
        }
        guard let content = String(data: data, encoding: .utf8) else {
            throw HubActionError.commandFailed("Hub configuration must use UTF-8 text.")
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
                    "Hub configuration recovery refused to remove a replaced configuration path."
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
                ? "The migration safety marker remains and Hub must stay stopped."
                : "Hub must stay stopped until its configuration is repaired."
            return HubActionError.commandFailed(
                "TeslaMate import failed: \(importError.localizedDescription) Configuration recovery also failed: \(recoveryError.localizedDescription) \(safetyState)"
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
        guard maximumBytes > 0,
              let values = try? url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey]),
              values.isRegularFile == true,
              values.isSymbolicLink != true,
              let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        do {
            let boundedMaximum = min(maximumBytes, 1024 * 1024)
            let size = try handle.seekToEnd()
            let offset = size > UInt64(boundedMaximum) ? size - UInt64(boundedMaximum) : 0
            try handle.seek(toOffset: offset)
            let data = try handle.read(upToCount: boundedMaximum) ?? Data()
            return String(decoding: data, as: UTF8.self)
        } catch {
            return nil
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
            ? "\(vehicles.count) vehicles"
            : vehicle?["displayName"] as? String ?? "No configured vehicle"
        let vehicleSummary: String
        if let observed = vehicle?["latestObservedAtMs"] as? NSNumber {
            vehicleSummary = "Last seen \(relativeAge(milliseconds: observed.int64Value))"
        } else {
            vehicleSummary = vehicle == nil ? "No configured vehicle" : "No observations yet"
        }
        let controlVehicleID = vehicles.count == 1
            ? (vehicles[0]["vehicleId"] as? String).flatMap(UUID.init(uuidString:))
            : nil
        let controlVehicles = vehicles.compactMap { vehicle -> HubControlVehicle? in
            guard let idValue = vehicle["vehicleId"] as? String,
                  let id = UUID(uuidString: idValue) else { return nil }
            let name = (vehicle["displayName"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
            let displayName = name.flatMap { $0.isEmpty ? nil : $0 } ?? "Vehicle"
            let status: String
            if let observed = vehicle["latestObservedAtMs"] as? NSNumber {
                status = "Last seen \(relativeAge(milliseconds: observed.int64Value))"
            } else {
                status = "No observations yet"
            }
            return HubControlVehicle(id: id, displayName: displayName, status: status)
        }
        let dbBytes = database?["bytes"] as? NSNumber
        let dbText = dbBytes.map { "Healthy · \($0.int64Value / 1_048_576) MB" } ?? "Waiting for setup or import"
        let dataDirectory = (database?["path"] as? String).map { URL(fileURLWithPath: $0).deletingLastPathComponent() }
        let service = ready ? "Installed and running" : "Installed · needs attention"
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
            ? "Connected"
            : "Not configured"
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
        return HubSnapshot(health: ready ? .running : .degraded,
                           service: service,
                           account: account,
                           provider: provider,
                           vehicleName: vehicleName,
                           vehicle: vehicleSummary,
                           controlVehicleID: controlVehicleID,
                           controlVehicles: controlVehicles,
                           database: dbText,
                           activity: [],
                           version: root["version"] as? String ?? HubRelease.fallbackVersion,
                           dataDirectory: dataDirectory,
                           diagnosticLines: diagnosticLines)
    }

    private func statusSnapshot(_ status: HubSnapshot, installed: Bool, loaded: HubServiceLoadState) -> HubSnapshot {
        guard installed else { return HubSnapshot(health: .needsInstall, service: "Not installed", account: status.account, provider: status.provider, vehicleName: status.vehicleName, vehicle: status.vehicle, controlVehicleID: nil, controlVehicles: status.controlVehicles, database: status.database, activity: status.activity, version: status.version, dataDirectory: status.dataDirectory ?? dataDirectory, diagnosticLines: status.diagnosticLines) }
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
        if status.version != HubRelease.bundledVersion {
            result.diagnosticLines.insert(
                "Version mismatch: service \(status.version), app \(HubRelease.bundledVersion)",
                at: 0
            )
            if case .loaded = loaded {
                result.health = .degraded
                result.service = "Installed · version mismatch"
            }
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
        return HubSnapshot(health: health, service: service, account: "Unknown", provider: nil, vehicleName: "Vehicle", vehicle: "Unknown", controlVehicleID: nil, controlVehicles: [], database: "Unknown", activity: [], version: HubRelease.fallbackVersion, dataDirectory: dataDirectory, diagnosticLines: [service, "Hub status command did not return a valid report."])
    }

    private func relativeAge(milliseconds: Int64) -> String {
        let seconds = max(0, Int(Date().timeIntervalSince1970 - Double(milliseconds) / 1_000))
        if seconds < 60 { return "just now" }
        if seconds < 3_600 { return "\(seconds / 60) minutes ago" }
        if seconds < 86_400 { return "\(seconds / 3_600) hours ago" }
        return "\(seconds / 86_400) days ago"
    }
}
