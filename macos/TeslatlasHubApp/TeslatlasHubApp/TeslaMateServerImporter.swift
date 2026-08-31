// SPDX-License-Identifier: AGPL-3.0-only

import Darwin
import Foundation

enum TeslaMateSSHAuthentication: Equatable {
    case key(identityFile: URL?)
    case password(String)
}

enum TeslaMateVersionClassification: Equatable {
    case supported(String)
    case tooOld(String)
    case unknown
}

enum TeslaMateSSHRecoveryAction: Equatable {
    case chooseKey
    case usePassword
    case useKey
    case openLogs
}

struct TeslaMateSSHDiagnostic: LocalizedError, Equatable {
    let reasonCode: String
    let title: String
    let summary: String
    let suggestions: [String]
    let recoveryActions: [TeslaMateSSHRecoveryAction]

    var errorDescription: String? { summary }

    var safeReport: String {
        ([title, summary] + suggestions.map { "- \($0)" } + ["Code: \(reasonCode)"])
            .joined(separator: "\n")
    }
}

final class TeslaMateServerImportSession {
    let source: String
    let carID: String
    let passwordFile: URL
    let encryptionKeyFile: URL
    let teslaMateVersion: String?

    private let tunnel: Process
    private let temporaryDirectory: URL
    private let lock = NSLock()
    private var closed = false

    init(source: String,
         carID: String,
         passwordFile: URL,
         encryptionKeyFile: URL,
         teslaMateVersion: String?,
         tunnel: Process,
         temporaryDirectory: URL) {
        self.source = source
        self.carID = carID
        self.passwordFile = passwordFile
        self.encryptionKeyFile = encryptionKeyFile
        self.teslaMateVersion = teslaMateVersion
        self.tunnel = tunnel
        self.temporaryDirectory = temporaryDirectory
    }

    deinit { close() }

    func close() {
        lock.lock()
        guard !closed else { lock.unlock(); return }
        closed = true
        lock.unlock()
        if tunnel.isRunning {
            let process = tunnel
            let pid = process.processIdentifier
            process.terminate()
            DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + 1) {
                if process.isRunning {
                    let killed = Darwin.kill(pid, SIGKILL) == 0
                    HubAppLog.shared.record("tunnel.force_stop", category: "teslamate_import",
                                            level: "WARN",
                                            fields: ["signal_sent": killed ? "true" : "false"])
                }
            }
        }
        do {
            try FileManager.default.removeItem(at: temporaryDirectory)
            HubAppLog.shared.record("tunnel.closed", category: "teslamate_import",
                                    fields: ["temporary_files_removed": "true"])
        } catch {
            HubAppLog.shared.record("tunnel.closed", category: "teslamate_import", level: "WARN",
                                    fields: [
                                        "error_code": HubAppLog.errorCode(error),
                                        "temporary_files_removed": "false"
                                    ])
        }
    }
}

enum TeslaMateServerImporter {
    private static let temporaryDirectoryPrefix = "th-"
    private static let legacyTemporaryDirectoryPrefix = "teslatlas-hub-import-"
    private static let temporaryRoot = URL(fileURLWithPath: "/tmp", isDirectory: true)
    private static let maximumTunnelDiagnosticBytes = 64 * 1024
    private static let ssh = URL(fileURLWithPath: "/usr/bin/ssh")

    /// Remove secret-bearing import directories left by a previous crashed app.
    /// Only exact UUID names, real directories, and the current user's inodes
    /// are admitted; similarly named files and symlinks are left untouched.
    @discardableResult
    static func cleanupStaleTemporaryDirectories(
        in root: URL = temporaryRoot
    ) -> Int {
        let manager = FileManager.default
        let entries: [URL]
        do {
            entries = try manager.contentsOfDirectory(
                at: root,
                includingPropertiesForKeys: nil,
                options: [.skipsHiddenFiles]
            )
        } catch {
            HubAppLog.shared.record("temporary_files.scan_failed",
                                    category: "teslamate_import", level: "WARN",
                                    fields: ["error_code": HubAppLog.errorCode(error)])
            return 0
        }
        var removed = 0
        for entry in entries {
            let name = entry.lastPathComponent
            let prefix = [temporaryDirectoryPrefix, legacyTemporaryDirectoryPrefix]
                .first { name.hasPrefix($0) }
            guard let prefix,
                  UUID(uuidString: String(name.dropFirst(prefix.count))) != nil else { continue }
            var information = stat()
            guard lstat(entry.path, &information) == 0,
                  information.st_mode & S_IFMT == S_IFDIR,
                  information.st_uid == getuid()
            else { continue }
            do {
                try manager.removeItem(at: entry)
                removed += 1
            } catch {
                HubAppLog.shared.record("temporary_files.cleanup_failed",
                                        category: "teslamate_import", level: "WARN",
                                        fields: ["error_code": HubAppLog.errorCode(error)])
                continue
            }
        }
        return removed
    }

    private struct SSHConnectionResources {
        enum Method: Equatable {
            case keyOrAgent
            case password
        }

        let temporaryDirectory: URL
        let arguments: [String]
        let environment: [String: String]
        let method: Method
    }

    private static let managedSSHArguments = [
        "-o", "PermitLocalCommand=no",
        "-o", "RemoteCommand=none",
        "-o", "RequestTTY=no",
        "-o", "StdinNull=no",
        "-o", "StrictHostKeyChecking=yes"
    ]

    private static let isolatedSSHArguments = [
        "-o", "ControlMaster=no",
        "-o", "ControlPath=none",
        "-o", "ControlPersist=no",
        "-o", "ForkAfterAuthentication=no",
        "-o", "SessionType=default"
    ] + managedSSHArguments

    static var sshIsolationArgumentsForTests: [String] { isolatedSSHArguments }

    private static func ownedTunnelArguments(controlPath: String) -> [String] {
        [
            "-o", "ControlMaster=yes",
            "-o", "ControlPath=\(controlPath)",
            "-o", "ControlPersist=no",
            "-o", "ForkAfterAuthentication=no"
        ] + managedSSHArguments
    }

    static func ownedTunnelArgumentsForTests(controlPath: String) -> [String] {
        ownedTunnelArguments(controlPath: controlPath)
    }

    private static let discoveryScript = #"""
set -eu

docker_run() {
    if [ "$(id -u)" -eq 0 ]; then
        docker "$@"
    elif [ "${TESLATLAS_USE_SUDO:-1}" -eq 1 ]; then
        sudo -n docker "$@"
    else
        docker "$@"
    fi
}

tm_ids=$(docker_run ps --filter label=com.docker.compose.service=teslamate --format '{{.ID}}' | head -n 2)
tm_count=$(printf '%s\n' "$tm_ids" | awk 'NF {count += 1} END {print count + 0}')
[ "$tm_count" -gt 0 ] || { printf '%s\n' 'TeslaMate container not found.' >&2; exit 20; }
[ "$tm_count" -eq 1 ] || { printf '%s\n' 'Multiple TeslaMate containers are running.' >&2; exit 27; }
tm_id=$(printf '%s\n' "$tm_ids" | head -n 1)
project=$(docker_run inspect -f '{{index .Config.Labels "com.docker.compose.project"}}' "$tm_id")
[ -n "$project" ] || { printf '%s\n' 'TeslaMate Compose project not found.' >&2; exit 21; }
db_ids=$(docker_run ps --filter "label=com.docker.compose.project=$project" --filter label=com.docker.compose.service=database --format '{{.ID}}' | head -n 2)
db_count=$(printf '%s\n' "$db_ids" | awk 'NF {count += 1} END {print count + 0}')
[ "$db_count" -gt 0 ] || { printf '%s\n' 'TeslaMate database container not found.' >&2; exit 22; }
[ "$db_count" -eq 1 ] || { printf '%s\n' 'Multiple TeslaMate database containers are running.' >&2; exit 28; }
db_id=$(printf '%s\n' "$db_ids" | head -n 1)

env_value() {
    docker_run inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$tm_id" | awk -F= -v key="$1" '$1 == key {sub(/^[^=]*=/, ""); print; exit}'
}

db_user=$(env_value DATABASE_USER)
db_pass=$(env_value DATABASE_PASS)
db_name=$(env_value DATABASE_NAME)
encryption_key=$(env_value ENCRYPTION_KEY)
[ -n "$db_user" ] && [ -n "$db_pass" ] && [ -n "$db_name" ] && [ -n "$encryption_key" ] \
    || { printf '%s\n' 'TeslaMate credentials are incomplete.' >&2; exit 23; }

tm_image=$(docker_run inspect -f '{{.Config.Image}}' "$tm_id" 2>/dev/null || true)
tm_label_version=$(docker_run inspect -f '{{index .Config.Labels "org.opencontainers.image.version"}}' "$tm_id" 2>/dev/null || true)

network=$(docker_run inspect -f '{{range $name, $value := .NetworkSettings.Networks}}{{println $name}}{{end}}' "$db_id" | head -n 1)
db_ip=$(docker_run inspect -f "{{with index .NetworkSettings.Networks \"$network\"}}{{.IPAddress}}{{end}}" "$db_id")
case "$db_ip" in *[!0-9.]*|'') printf '%s\n' 'TeslaMate database network address is invalid.' >&2; exit 24;; esac

car_ids=$(docker_run exec "$db_id" psql -U "$db_user" -d "$db_name" -Atc 'select id from cars order by id limit 2' 2>/dev/null)
car_id=$(printf '%s\n' "$car_ids" | head -n 1)
count=$(printf '%s\n' "$car_ids" | awk 'NF {count += 1} END {print count + 0}')
[ "$count" -eq 1 ] || { printf '%s\n' 'Guided import requires a TeslaMate database with one vehicle.' >&2; exit 25; }
case "$car_id" in *[!0-9]*|'') printf '%s\n' 'TeslaMate vehicle could not be identified.' >&2; exit 26;; esac

encode() { printf '%s' "$1" | base64 | tr -d '\r\n'; }
printf 'user=%s\n' "$(encode "$db_user")"
printf 'password=%s\n' "$(encode "$db_pass")"
printf 'database=%s\n' "$(encode "$db_name")"
printf 'key=%s\n' "$(encode "$encryption_key")"
printf 'car=%s\n' "$(encode "$car_id")"
printf 'address=%s\n' "$(encode "$db_ip")"
printf 'image=%s\n' "$(encode "$tm_image")"
printf 'label_version=%s\n' "$(encode "$tm_label_version")"
"""#

    static func connect(host: String,
                        user: String,
                        port: Int,
                        authentication: TeslaMateSSHAuthentication,
                        usePasswordlessSudo: Bool,
                        completion: @escaping (Result<TeslaMateServerImportSession, Error>) -> Void) {
        let authenticationName: String
        switch authentication {
        case .key: authenticationName = "key_or_agent"
        case .password: authenticationName = "password"
        }
        HubAppLog.shared.record("ssh.connect.requested",
                                category: "teslamate_import",
                                fields: [
                                    "authentication": authenticationName,
                                    "passwordless_sudo": usePasswordlessSudo ? "true" : "false",
                                    "nonstandard_port": port == 22 ? "false" : "true"
                                ])
        guard host.range(of: #"^[A-Za-z0-9._:%-]+$"#, options: .regularExpression) != nil,
              !host.hasPrefix("-"),
              (user.isEmpty
                  || user.range(of: #"^[A-Za-z_][A-Za-z0-9_-]*$"#,
                                options: .regularExpression) != nil),
              (1...65535).contains(port) else {
            HubAppLog.shared.record("ssh.connect.rejected", category: "teslamate_import",
                                    level: "WARN", fields: ["reason": "invalid_input"])
            completion(.failure(diagnostic(reason: "invalid_input", method: method(for: authentication))))
            return
        }
        let resources: SSHConnectionResources
        do {
            resources = try prepareConnectionResources(authentication: authentication)
        } catch {
            HubAppLog.shared.record("ssh.credentials.rejected", category: "teslamate_import",
                                    level: "WARN",
                                    fields: ["error_code": HubAppLog.errorCode(error)])
            completion(.failure(error))
            return
        }
        let destination = user.isEmpty ? host : "\(user)@\(host)"
        let discoveryStarted = Date()
        let common = isolatedSSHArguments + [
            "-o", "ConnectTimeout=12",
            "-o", "ForwardAgent=no",
        ] + resources.arguments + [
            "-p", String(port),
            destination
        ]
        HubProcessExecutor.run(executable: ssh,
                               arguments: common + ["env", "TESLATLAS_USE_SUDO=\(usePasswordlessSudo ? 1 : 0)", "sh", "-s"],
                               stdin: discoveryScript,
                               environment: resources.environment,
                               maximumOutputBytes: 64 * 1024,
                               timeout: 30) { result in
            switch result {
            case let .failure(error):
                let reason = discoveryFailureReason(error)
                HubAppLog.shared.record("ssh.discovery.failed", category: "teslamate_import",
                                        level: "ERROR",
                                        fields: [
                                            "duration_ms": String(Int(Date().timeIntervalSince(discoveryStarted) * 1000)),
                                            "error_code": HubAppLog.errorCode(error),
                                            "reason": reason
                                        ])
                removeTemporaryDirectory(resources.temporaryDirectory,
                                         context: "discovery_failed")
                DispatchQueue.main.async {
                    completion(.failure(diagnostic(reason: reason, method: resources.method)))
                }
            case let .success(output):
                do {
                    HubAppLog.shared.record(
                        "ssh.discovery.completed",
                        category: "teslamate_import",
                        fields: [
                            "duration_ms": String(Int(Date().timeIntervalSince(discoveryStarted) * 1000))
                        ]
                    )
                    let values = try parse(output)
                    let session = try makeSession(values: values,
                                                  destination: destination,
                                                  sshPort: port,
                                                  resources: resources)
                    DispatchQueue.main.async { completion(.success(session)) }
                } catch {
                    HubAppLog.shared.record("ssh.tunnel.failed", category: "teslamate_import",
                                            level: "ERROR",
                                            fields: ["error_code": HubAppLog.errorCode(error)])
                    removeTemporaryDirectory(resources.temporaryDirectory,
                                             context: "discovery_parse_failed")
                    DispatchQueue.main.async { completion(.failure(error)) }
                }
            }
        }
    }

    private static func prepareConnectionResources(
        authentication: TeslaMateSSHAuthentication
    ) throws -> SSHConnectionResources {
        let manager = FileManager.default
        let directory = temporaryRoot
            .appendingPathComponent("\(temporaryDirectoryPrefix)\(UUID().uuidString)", isDirectory: true)
        try manager.createDirectory(at: directory,
                                    withIntermediateDirectories: false,
                                    attributes: [.posixPermissions: 0o700])
        do {
            switch authentication {
            case let .key(identityFile):
                var arguments = ["-o", "BatchMode=yes"]
                if let identityFile {
                    var info = stat()
                    guard lstat(identityFile.path, &info) == 0,
                          info.st_mode & S_IFMT == S_IFREG,
                          info.st_uid == getuid(),
                          info.st_mode & 0o022 == 0 else {
                        throw diagnostic(reason: "unsafe_identity_file", method: .keyOrAgent)
                    }
                    arguments += ["-o", "IdentitiesOnly=yes", "-i", identityFile.path]
                }
                return SSHConnectionResources(temporaryDirectory: directory,
                                              arguments: arguments,
                                              environment: [:],
                                              method: .keyOrAgent)
            case let .password(password):
                guard !password.isEmpty else {
                    throw diagnostic(reason: "password_required", method: .password)
                }
                let passwordFile = directory.appendingPathComponent("ssh-password")
                let askpass = directory.appendingPathComponent("ssh-askpass")
                try Data(password.utf8).write(to: passwordFile, options: .atomic)
                try Data(#"""
#!/bin/sh
exec /bin/cat "$TESLATLAS_SSH_PASSWORD_FILE"
"""#.utf8).write(to: askpass, options: .atomic)
                try manager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: passwordFile.path)
                try manager.setAttributes([.posixPermissions: 0o700], ofItemAtPath: askpass.path)
                return SSHConnectionResources(
                    temporaryDirectory: directory,
                    arguments: [
                        "-o", "BatchMode=no",
                        "-o", "NumberOfPasswordPrompts=1",
                        "-o", "PreferredAuthentications=password,keyboard-interactive",
                        "-o", "PubkeyAuthentication=no"
                    ],
                    environment: [
                        "SSH_ASKPASS": askpass.path,
                        "SSH_ASKPASS_REQUIRE": "force",
                        "DISPLAY": "teslatlas-hub:0",
                        "TESLATLAS_SSH_PASSWORD_FILE": passwordFile.path
                    ],
                    method: .password
                )
            }
        } catch {
            removeTemporaryDirectory(directory, context: "credential_prepare_failed")
            throw error
        }
    }

    private static func parse(_ output: String) throws -> [String: String] {
        var values: [String: String] = [:]
        for line in output.split(whereSeparator: \.isNewline) {
            let parts = line.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            guard parts.count == 2,
                  let data = Data(base64Encoded: String(parts[1])),
                  let value = String(data: data, encoding: .utf8) else { continue }
            values[String(parts[0])] = value
        }
        let required = ["user", "password", "database", "key", "car", "address"]
        guard required.allSatisfy({ !(values[$0] ?? "").isEmpty }) else {
            throw HubActionError.commandFailed("TeslaMate server returned incomplete setup details.")
        }
        return values
    }

    static func classifyVersionForTests(image: String?,
                                        labelVersion: String?) -> TeslaMateVersionClassification {
        classifyVersion(image: image, labelVersion: labelVersion)
    }

    private static func classifyVersion(image: String?,
                                        labelVersion: String?) -> TeslaMateVersionClassification {
        if case let .tooOld(version) = parseVersion(labelVersion) {
            return .tooOld(version)
        }
        let identity = dockerImageIdentity(image)
        if let tag = identity?.tag {
            switch parseVersion(tag) {
            case let .tooOld(version):
                return .tooOld(version)
            case let .supported(version) where identity?.official == true:
                return .supported(version)
            case .supported, .unknown:
                break
            }
        }
        return .unknown
    }

    private static func dockerImageIdentity(_ rawValue: String?)
        -> (official: Bool, tag: String?)? {
        guard var image = rawValue?.trimmingCharacters(in: .whitespacesAndNewlines),
              !image.isEmpty else { return nil }
        if let digest = image.firstIndex(of: "@") {
            image = String(image[..<digest])
            return (official: isOfficialTeslaMateRepository(image), tag: nil)
        }
        let slash = image.lastIndex(of: "/")
        guard let colon = image.lastIndex(of: ":"), slash == nil || colon > slash! else {
            return (official: isOfficialTeslaMateRepository(image), tag: nil)
        }
        let repository = String(image[..<colon])
        let tag = String(image[image.index(after: colon)...])
        return (official: isOfficialTeslaMateRepository(repository), tag: tag)
    }

    private static func isOfficialTeslaMateRepository(_ rawValue: String) -> Bool {
        var repository = rawValue.lowercased()
        for prefix in ["docker.io/", "index.docker.io/"] where repository.hasPrefix(prefix) {
            repository.removeFirst(prefix.count)
            break
        }
        return repository == "teslamate/teslamate"
    }

    private static func parseVersion(_ rawValue: String?) -> TeslaMateVersionClassification {
        guard var value = rawValue?.trimmingCharacters(in: .whitespacesAndNewlines),
              !value.isEmpty else { return .unknown }
        if value.hasPrefix("refs/tags/") { value.removeFirst("refs/tags/".count) }
        if value.contains("@") { return .unknown }
        if let slash = value.lastIndex(of: "/"),
           let colon = value.lastIndex(of: ":"), colon > slash {
            value = String(value[value.index(after: colon)...])
        }
        if value.hasPrefix("v") { value.removeFirst() }
        let displayedVersion = value
        let stable = !value.contains("-")
        let core = value.split(separator: "+", maxSplits: 1).first.map(String.init) ?? value
        let components = core.split(separator: ".", omittingEmptySubsequences: false)
        let patchValue = components.count == 3 ? Int(components[2]) : 0
        guard (2...3).contains(components.count),
              let major = Int(components[0]),
              let minor = Int(components[1]),
              let patch = patchValue else { return .unknown }
        guard stable else { return .tooOld(displayedVersion) }
        if major > 4 || (major == 4 && minor >= 2) {
            return .supported(displayedVersion)
        }
        return .tooOld(displayedVersion)
    }

    private static func makeSession(values: [String: String],
                                    destination: String,
                                    sshPort: Int,
                                    resources: SSHConnectionResources) throws -> TeslaMateServerImportSession {
        let manager = FileManager.default
        let directory = resources.temporaryDirectory
        let passwordFile = directory.appendingPathComponent("postgres-password")
        let keyFile = directory.appendingPathComponent("encryption-key")
        try Data(values["password"]!.utf8).write(to: passwordFile, options: .atomic)
        try Data(values["key"]!.utf8).write(to: keyFile, options: .atomic)
        try manager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: passwordFile.path)
        try manager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: keyFile.path)

        let teslaMateVersion: String?
        switch classifyVersion(image: values["image"], labelVersion: values["label_version"]) {
        case let .supported(version):
            teslaMateVersion = version
        case .tooOld:
            throw diagnostic(reason: "teslamate_version_too_old", method: resources.method)
        case .unknown:
            teslaMateVersion = nil
        }

        let (tunnel, localPort) = try openTunnel(address: values["address"]!,
                                                 destination: destination,
                                                 sshPort: sshPort,
                                                 resources: resources)
        let user = percentEncode(values["user"]!)
        let database = percentEncode(values["database"]!)
        return TeslaMateServerImportSession(
            source: "postgresql://\(user)@127.0.0.1:\(localPort)/\(database)",
            carID: values["car"]!,
            passwordFile: passwordFile,
            encryptionKeyFile: keyFile,
            teslaMateVersion: teslaMateVersion,
            tunnel: tunnel,
            temporaryDirectory: directory
        )
    }

    private static func removeTemporaryDirectory(_ directory: URL, context: String) {
        do {
            try FileManager.default.removeItem(at: directory)
        } catch {
            HubAppLog.shared.record("temporary_files.cleanup_failed",
                                    category: "teslamate_import", level: "WARN",
                                    fields: [
                                        "context": context,
                                        "error_code": HubAppLog.errorCode(error)
                                    ])
        }
    }

    private static func openTunnel(address: String,
                                   destination: String,
                                   sshPort: Int,
                                   resources: SSHConnectionResources) throws -> (Process, Int) {
        let started = Date()
        for attempt in 0..<3 {
            HubAppLog.shared.record("ssh.tunnel.starting", category: "teslamate_import",
                                    fields: ["attempt": String(attempt + 1)])
            let localPort = Int.random(in: 49152...65000)
            let tunnel = Process()
            let errorPipe = Pipe()
            let diagnosticOutput = BoundedProcessOutput(maximumBytes: maximumTunnelDiagnosticBytes)
            let diagnosticDrain = DispatchGroup()
            tunnel.executableURL = ssh
            let controlPath = resources.temporaryDirectory
                .appendingPathComponent("c\(attempt)", isDirectory: false).path
            tunnel.arguments = ownedTunnelArguments(controlPath: controlPath) + [
                "-N", "-o", "ConnectTimeout=12",
                "-o", "ExitOnForwardFailure=yes", "-o", "ForwardAgent=no",
                "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=2",
            ] + resources.arguments + [
                "-p", String(sshPort),
                "-L", "127.0.0.1:\(localPort):\(address):5432",
                destination
            ]
            tunnel.environment = ProcessInfo.processInfo.environment.merging(resources.environment) { _, new in new }
            tunnel.standardOutput = FileHandle.nullDevice
            tunnel.standardError = errorPipe
            try tunnel.run()
            diagnosticDrain.enter()
            DispatchQueue.global(qos: .utility).async {
                while true {
                    let chunk = errorPipe.fileHandleForReading.readData(ofLength: 16 * 1024)
                    if chunk.isEmpty { break }
                    diagnosticOutput.append(chunk)
                }
                diagnosticDrain.leave()
            }

            let deadline = Date().addingTimeInterval(5)
            while Date() < deadline {
                if !tunnel.isRunning { break }
                if localPortAcceptsConnections(localPort) {
                    // A pre-existing listener can win the random-port race
                    // just before OpenSSH reports its bind failure. Require
                    // the SSH process and listener to remain live once more.
                    Thread.sleep(forTimeInterval: 0.1)
                    guard tunnel.isRunning, localPortAcceptsConnections(localPort) else {
                        break
                    }
                    HubAppLog.shared.record("ssh.tunnel.ready", category: "teslamate_import",
                                            fields: [
                                                "attempt": String(attempt + 1),
                                                "duration_ms": String(Int(Date().timeIntervalSince(started) * 1000))
                    ])
                    return (tunnel, localPort)
                }
                Thread.sleep(forTimeInterval: 0.1)
            }

            if tunnel.isRunning {
                stopTunnel(tunnel)
                _ = diagnosticDrain.wait(timeout: .now() + 1)
                HubAppLog.shared.record("ssh.tunnel.timeout", category: "teslamate_import",
                                        level: "ERROR")
                throw diagnostic(reason: "tunnel_timeout", method: resources.method)
            }
            _ = diagnosticDrain.wait(timeout: .now() + 1)
            let diagnostic = String(decoding: diagnosticOutput.snapshot(), as: UTF8.self)
            if diagnostic.localizedCaseInsensitiveContains("address already in use"), attempt < 2 {
                HubAppLog.shared.record("ssh.tunnel.port_collision", category: "teslamate_import",
                                        level: "WARN")
                continue
            }
            let reason = tunnelFailureReason(diagnostic)
            let safeDetail = String(HubShareRedactor.redact(diagnostic).prefix(512))
                .trimmingCharacters(in: .whitespacesAndNewlines)
            HubAppLog.shared.record("ssh.tunnel.rejected", category: "teslamate_import",
                                    level: "ERROR", fields: [
                                        "detail": safeDetail.isEmpty ? "none" : safeDetail,
                                        "exit_status": String(tunnel.terminationStatus),
                                        "reason": reason
                                    ])
            throw self.diagnostic(reason: reason, method: resources.method)
        }
        throw diagnostic(reason: "tunnel_failed", method: resources.method)
    }

    static func connectionDiagnostic(
        for error: Error,
        authentication: TeslaMateSSHAuthentication
    ) -> TeslaMateSSHDiagnostic {
        if let diagnostic = error as? TeslaMateSSHDiagnostic { return diagnostic }
        let reason = discoveryFailureReason(error)
        return diagnostic(reason: reason, method: method(for: authentication))
    }

    private static func method(for authentication: TeslaMateSSHAuthentication) -> SSHConnectionResources.Method {
        switch authentication {
        case .key: return .keyOrAgent
        case .password: return .password
        }
    }

    private static func diagnostic(
        reason: String,
        method: SSHConnectionResources.Method
    ) -> TeslaMateSSHDiagnostic {
        let actions: [TeslaMateSSHRecoveryAction]
        switch (reason, method) {
        case ("authentication_failed", .keyOrAgent),
             ("ssh_authentication_or_connection", .keyOrAgent):
            actions = [.chooseKey, .usePassword, .openLogs]
        case ("authentication_failed", .password),
             ("ssh_authentication_or_connection", .password):
            actions = [.useKey, .openLogs]
        case ("unsafe_identity_file", _):
            actions = [.chooseKey, .openLogs]
        case ("password_required", _):
            actions = [.useKey, .openLogs]
        default:
            actions = [.openLogs]
        }

        let title: String
        let summary: String
        let suggestions: [String]
        switch reason {
        case "invalid_input":
            title = "Check the server details"
            summary = "Enter a valid server, SSH user, and port."
            suggestions = ["A server name, IP address, or SSH config alias is accepted."]
        case "unsafe_identity_file":
            title = "Choose a usable private key"
            summary = "Hub could not safely use the selected SSH key."
            suggestions = ["Choose a private key owned by this Mac user.", "Do not select the .pub file."]
        case "password_required":
            title = "Enter the SSH password"
            summary = "Password authentication was selected but no password was entered."
            suggestions = ["Enter the server account password, or use SSH config, agent, or key authentication."]
        case "authentication_failed", "ssh_authentication_or_connection":
            title = "SSH authentication failed"
            summary = "The server did not accept the selected account or authentication method."
            suggestions = method == .keyOrAgent
                ? ["Leave the key empty to use ~/.ssh/config, ssh-agent, and standard keys.",
                   "Choose the private key that works in Terminal, or switch to Password."]
                : ["Check the account password, or switch to SSH config, agent, or key."]
        case "host_key_failed":
            title = "Server identity needs verification"
            summary = "OpenSSH refused the server identity."
            suggestions = ["Verify the server fingerprint through a trusted channel, then add or update its entry in ~/.ssh/known_hosts."]
        case "connection_refused":
            title = "SSH connection refused"
            summary = "The server is reachable, but SSH is not accepting this connection."
            suggestions = ["Check that SSH is running and the port is correct."]
        case "name_resolution_failed":
            title = "Server not found"
            summary = "The server name could not be resolved."
            suggestions = ["Check the server address, SSH config alias, DNS, and VPN."]
        case "route_unavailable":
            title = "Server unreachable"
            summary = "This Mac has no route to the TeslaMate server."
            suggestions = ["Check the network, VPN, and firewall."]
        case "connection_timed_out", "timed_out":
            title = "SSH connection timed out"
            summary = "The TeslaMate server did not respond in time."
            suggestions = ["Check the address, port, network, VPN, and firewall."]
        case "connection_closed":
            title = "SSH connection closed"
            summary = "The server closed the connection."
            suggestions = ["Check the server SSH logs and account policy."]
        case "forwarding_disabled":
            title = "SSH forwarding is disabled"
            summary = "Hub connected, but the server refused the protected database tunnel."
            suggestions = ["Allow TCP forwarding for this SSH account."]
        case "tunnel_timeout", "tunnel_failed":
            title = "Database tunnel failed"
            summary = "Hub connected, but the protected database tunnel did not become ready."
            suggestions = ["Try again, then open Logs for the safe reason code."]
        case "local_port_unavailable":
            title = "Local tunnel port unavailable"
            summary = "Hub could not reserve a local database tunnel port."
            suggestions = ["Try again."]
        case "teslamate_version_too_old":
            title = "Update TeslaMate first"
            summary = "Guided migration requires TeslaMate 4.2.0 or newer."
            suggestions = ["Back up TeslaMate, update it, let its migrations finish, then connect again."]
        default:
            title = "TeslaMate connection failed"
            summary = discoveryFailureMessageForReason(reason)
            suggestions = ["Check the server account and Docker access, then try again."]
        }
        return TeslaMateSSHDiagnostic(reasonCode: reason,
                                      title: title,
                                      summary: summary,
                                      suggestions: suggestions,
                                      recoveryActions: actions)
    }

    private static func discoveryFailureMessageForReason(_ reason: String) -> String {
        switch reason {
        case "teslamate_not_found": return "TeslaMate is not running or its container could not be found."
        case "compose_project_missing": return "The TeslaMate Docker Compose project could not be identified."
        case "database_not_found": return "The TeslaMate database container is not running or could not be found."
        case "credentials_incomplete": return "TeslaMate database or encryption credentials are incomplete."
        case "database_network_invalid": return "The TeslaMate database network could not be identified safely."
        case "multiple_vehicles": return "Guided import currently requires a TeslaMate database with one vehicle."
        case "vehicle_missing": return "TeslaMate has no vehicle available to import."
        case "multiple_teslamate_instances": return "More than one TeslaMate instance is running."
        case "multiple_database_instances": return "The TeslaMate project has more than one database container."
        case "passwordless_sudo_required": return "This account cannot run Docker with passwordless sudo."
        case "sudo_not_permitted": return "This account is not allowed to run Docker with sudo."
        case "docker_permission_denied": return "This account cannot access Docker."
        case "docker_missing": return "Docker was not found on the TeslaMate server."
        case "docker_unavailable": return "Docker is installed but unavailable."
        case "teslamate_version_too_old": return "Guided migration requires TeslaMate 4.2.0 or newer."
        default: return "Hub could not read TeslaMate over SSH."
        }
    }

    static func tunnelFailureMessage(_ diagnostic: String) -> String {
        switch sshFailureReason(diagnostic) {
        case "forwarding_disabled":
            return "The SSH server does not permit database forwarding. Enable TCP forwarding for this account."
        case "authentication_failed":
            return "SSH authentication failed while opening the database tunnel."
        case "host_key_failed":
            return "SSH host identity verification failed. Verify or update this server in your SSH known-hosts file."
        case "connection_refused":
            return "The SSH server refused the connection. Check that SSH is running and the port is correct."
        case "name_resolution_failed":
            return "The SSH server name could not be resolved. Check the server address and network."
        case "route_unavailable":
            return "The SSH server is unreachable from this Mac. Check the network, VPN, and firewall."
        case "connection_timed_out":
            return "The SSH connection timed out. Check the server address, port, network, and firewall."
        case "connection_closed":
            return "The SSH server closed the connection while opening the database tunnel."
        case "local_port_unavailable":
            return "A local database tunnel port was unavailable. Try again."
        default:
            return "Could not open the protected TeslaMate database tunnel."
        }
    }

    static func discoveryFailureMessage(_ error: Error) -> String {
        switch discoveryFailureReason(error) {
        case "teslamate_not_found":
            return "TeslaMate is not running or its container could not be found."
        case "compose_project_missing":
            return "The TeslaMate Docker Compose project could not be identified."
        case "database_not_found":
            return "The TeslaMate database container is not running or could not be found."
        case "credentials_incomplete":
            return "TeslaMate database or encryption credentials are incomplete."
        case "database_network_invalid":
            return "The TeslaMate database network could not be identified safely."
        case "multiple_vehicles":
            return "Guided import currently requires a TeslaMate database with one vehicle."
        case "vehicle_missing":
            return "TeslaMate has no vehicle available to import."
        case "multiple_teslamate_instances":
            return "More than one TeslaMate instance is running. Stop the instance you do not want to import, then try again."
        case "multiple_database_instances":
            return "The selected TeslaMate project has more than one database container. Stop the duplicate, then try again."
        case "passwordless_sudo_required":
            return "This account cannot run Docker with passwordless sudo. Grant Docker access or turn off passwordless sudo and use an account that can access Docker directly."
        case "sudo_not_permitted":
            return "This account is not allowed to run Docker with sudo. Grant Docker access or use another server account."
        case "docker_permission_denied":
            return "This account cannot access Docker. Grant direct Docker access or allow passwordless sudo for Docker."
        case "docker_missing":
            return "Docker was not found on the TeslaMate server. Check the server and account PATH."
        case "docker_unavailable":
            return "Docker is installed but unavailable. Check that the Docker service is running."
        case "teslamate_version_too_old":
            return "Guided migration requires TeslaMate 4.2.0 or newer. Update TeslaMate, then try again."
        case "ssh_authentication_or_connection":
            return "SSH could not connect or authenticate. Check the server, port, account, and selected authentication method."
        case "authentication_failed":
            return "SSH authentication failed. Check the account and selected authentication method."
        case "host_key_failed":
            return "SSH host identity verification failed. Verify or update this server in your SSH known-hosts file."
        case "connection_refused":
            return "The SSH server refused the connection. Check that SSH is running and the port is correct."
        case "name_resolution_failed":
            return "The SSH server name could not be resolved. Check the server address and network."
        case "route_unavailable":
            return "The SSH server is unreachable from this Mac. Check the network, VPN, and firewall."
        case "connection_timed_out":
            return "The SSH connection timed out. Check the server address, port, network, and firewall."
        case "connection_closed":
            return "The SSH server closed the connection. Check the server logs and SSH policy."
        case "timed_out":
            return "The SSH connection timed out. Check the server address, port, and network."
        default:
            return "Could not read TeslaMate over SSH. Check authentication and Docker access."
        }
    }

    static func discoveryFailureReason(_ error: Error) -> String {
        guard let actionError = error as? HubActionError else { return "process_start_failed" }
        switch actionError {
        case let .commandExited(status, message):
            if status != 255, let reason = remoteDockerFailureReason(message) {
                return reason
            }
            switch status {
            case 20: return "teslamate_not_found"
            case 21: return "compose_project_missing"
            case 22: return "database_not_found"
            case 23: return "credentials_incomplete"
            case 24: return "database_network_invalid"
            case 25: return "multiple_vehicles"
            case 26: return "vehicle_missing"
            case 27: return "multiple_teslamate_instances"
            case 28: return "multiple_database_instances"
            case 29: return "teslamate_version_too_old"
            case 255:
                let reason = sshFailureReason(message)
                return reason == "unclassified" ? "ssh_authentication_or_connection" : reason
            default: return "remote_command_failed_\(status)"
            }
        case .commandTimedOut:
            return "timed_out"
        case .commandFailed:
            return "process_failed"
        case .missingResource:
            return "missing_resource"
        case .untrustedInstaller:
            return "untrusted_installer"
        case .preview:
            return "preview_read_only"
        }
    }

    private static func remoteDockerFailureReason(_ diagnostic: String) -> String? {
        let message = diagnostic.lowercased()
        if message.contains("sudo: a password is required")
            || message.contains("sudo: a terminal is required")
            || message.contains("sudo: no tty present and no askpass program specified") {
            return "passwordless_sudo_required"
        }
        if message.contains("not in the sudoers") || message.contains("not allowed to execute") {
            return "sudo_not_permitted"
        }
        if message.contains("permission denied")
            && (message.contains("docker.sock") || message.contains("docker daemon socket")) {
            return "docker_permission_denied"
        }
        if message.contains("docker: not found") || message.contains("docker: command not found") {
            return "docker_missing"
        }
        if message.contains("cannot connect to the docker daemon")
            || message.contains("is the docker daemon running") {
            return "docker_unavailable"
        }
        return nil
    }

    private static func tunnelFailureReason(_ diagnostic: String) -> String {
        sshFailureReason(diagnostic)
    }

    private static func sshFailureReason(_ diagnostic: String) -> String {
        if diagnostic.localizedCaseInsensitiveContains("administratively prohibited") { return "forwarding_disabled" }
        if diagnostic.localizedCaseInsensitiveContains("permission denied")
            || diagnostic.localizedCaseInsensitiveContains("too many authentication failures") {
            return "authentication_failed"
        }
        if diagnostic.localizedCaseInsensitiveContains("host key verification failed")
            || diagnostic.localizedCaseInsensitiveContains("remote host identification has changed") {
            return "host_key_failed"
        }
        if diagnostic.localizedCaseInsensitiveContains("connection refused") { return "connection_refused" }
        if diagnostic.localizedCaseInsensitiveContains("could not resolve hostname")
            || diagnostic.localizedCaseInsensitiveContains("name or service not known") {
            return "name_resolution_failed"
        }
        if diagnostic.localizedCaseInsensitiveContains("no route to host")
            || diagnostic.localizedCaseInsensitiveContains("network is unreachable") {
            return "route_unavailable"
        }
        if diagnostic.localizedCaseInsensitiveContains("connection timed out")
            || diagnostic.localizedCaseInsensitiveContains("operation timed out") {
            return "connection_timed_out"
        }
        if diagnostic.localizedCaseInsensitiveContains("connection closed")
            || diagnostic.localizedCaseInsensitiveContains("connection reset") {
            return "connection_closed"
        }
        if diagnostic.localizedCaseInsensitiveContains("address already in use") { return "local_port_unavailable" }
        if diagnostic.localizedCaseInsensitiveContains("too long for Unix domain socket") {
            return "control_socket_path_too_long"
        }
        return "unclassified"
    }

    private static func localPortAcceptsConnections(_ port: Int) -> Bool {
        let descriptor = socket(AF_INET, SOCK_STREAM, 0)
        guard descriptor >= 0 else { return false }
        defer { Darwin.close(descriptor) }

        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = in_port_t(UInt16(port).bigEndian)
        address.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))
        return withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(descriptor, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) == 0
            }
        }
    }

    private static func stopTunnel(_ process: Process) {
        guard process.isRunning else { return }
        let pid = process.processIdentifier
        process.terminate()
        let deadline = Date().addingTimeInterval(1)
        while process.isRunning, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.05)
        }
        if process.isRunning { Darwin.kill(pid, SIGKILL) }
        process.waitUntilExit()
    }

    private static func percentEncode(_ value: String) -> String {
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? ""
    }
}
