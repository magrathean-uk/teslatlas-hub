import Darwin
import Foundation

enum TeslaMateSSHAuthentication: Equatable {
    case key(identityFile: URL?)
    case password(String)
}

final class TeslaMateServerImportSession {
    let source: String
    let carID: String
    let passwordFile: URL
    let encryptionKeyFile: URL

    private let tunnel: Process
    private let temporaryDirectory: URL
    private let lock = NSLock()
    private var closed = false

    init(source: String,
         carID: String,
         passwordFile: URL,
         encryptionKeyFile: URL,
         tunnel: Process,
         temporaryDirectory: URL) {
        self.source = source
        self.carID = carID
        self.passwordFile = passwordFile
        self.encryptionKeyFile = encryptionKeyFile
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
                if process.isRunning { Darwin.kill(pid, SIGKILL) }
            }
        }
        try? FileManager.default.removeItem(at: temporaryDirectory)
        HubAppLog.shared.record("tunnel.closed", category: "teslamate_import")
    }
}

enum TeslaMateServerImporter {
    private static let temporaryDirectoryPrefix = "teslatlas-hub-import-"
    private static let maximumTunnelDiagnosticBytes = 64 * 1024
    private static let ssh = URL(fileURLWithPath: "/usr/bin/ssh")

    /// Remove secret-bearing import directories left by a previous crashed app.
    /// Only exact UUID names, real directories, and the current user's inodes
    /// are admitted; similarly named files and symlinks are left untouched.
    @discardableResult
    static func cleanupStaleTemporaryDirectories(
        in root: URL = FileManager.default.temporaryDirectory
    ) -> Int {
        let manager = FileManager.default
        guard let entries = try? manager.contentsOfDirectory(
            at: root,
            includingPropertiesForKeys: nil,
            options: [.skipsHiddenFiles]
        ) else { return 0 }
        var removed = 0
        for entry in entries {
            let name = entry.lastPathComponent
            guard name.hasPrefix(temporaryDirectoryPrefix),
                  UUID(uuidString: String(name.dropFirst(temporaryDirectoryPrefix.count))) != nil
            else { continue }
            var information = stat()
            guard lstat(entry.path, &information) == 0,
                  information.st_mode & S_IFMT == S_IFDIR,
                  information.st_uid == getuid()
            else { continue }
            do {
                try manager.removeItem(at: entry)
                removed += 1
            } catch {
                continue
            }
        }
        return removed
    }

    private struct SSHConnectionResources {
        let temporaryDirectory: URL
        let arguments: [String]
        let environment: [String: String]
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
        guard host.range(of: #"^[A-Za-z0-9.-]+$"#, options: .regularExpression) != nil,
              user.range(of: #"^[A-Za-z_][A-Za-z0-9_-]*$"#, options: .regularExpression) != nil,
              (1...65535).contains(port) else {
            HubAppLog.shared.record("ssh.connect.rejected", category: "teslamate_import",
                                    level: "WARN", fields: ["reason": "invalid_input"])
            completion(.failure(HubActionError.commandFailed("Server, SSH user, or port is invalid.")))
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
        let destination = "\(user)@\(host)"
        let discoveryStarted = Date()
        let common = [
            "-o", "ConnectTimeout=12",
            "-o", "StrictHostKeyChecking=accept-new",
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
                try? FileManager.default.removeItem(at: resources.temporaryDirectory)
                DispatchQueue.main.async {
                    completion(.failure(HubActionError.commandFailed(
                        discoveryFailureMessage(error)
                    )))
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
                    try? FileManager.default.removeItem(at: resources.temporaryDirectory)
                    DispatchQueue.main.async { completion(.failure(error)) }
                }
            }
        }
    }

    private static func prepareConnectionResources(
        authentication: TeslaMateSSHAuthentication
    ) throws -> SSHConnectionResources {
        let manager = FileManager.default
        let directory = manager.temporaryDirectory
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
                        throw HubActionError.commandFailed(
                            "The selected SSH key must be a safe file owned by this user."
                        )
                    }
                    arguments += ["-o", "IdentitiesOnly=yes", "-i", identityFile.path]
                }
                return SSHConnectionResources(temporaryDirectory: directory,
                                              arguments: arguments,
                                              environment: [:])
            case let .password(password):
                guard !password.isEmpty else {
                    throw HubActionError.commandFailed("Enter the SSH password.")
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
                    ]
                )
            }
        } catch {
            try? manager.removeItem(at: directory)
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

    private static func makeSession(values: [String: String],
                                    destination: String,
                                    sshPort: Int,
                                    resources: SSHConnectionResources) throws -> TeslaMateServerImportSession {
        let manager = FileManager.default
        let directory = resources.temporaryDirectory
        do {
            let passwordFile = directory.appendingPathComponent("postgres-password")
            let keyFile = directory.appendingPathComponent("encryption-key")
            try Data(values["password"]!.utf8).write(to: passwordFile, options: .atomic)
            try Data(values["key"]!.utf8).write(to: keyFile, options: .atomic)
            try manager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: passwordFile.path)
            try manager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: keyFile.path)

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
                tunnel: tunnel,
                temporaryDirectory: directory
            )
        } catch {
            try? manager.removeItem(at: directory)
            throw error
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
            tunnel.arguments = [
                "-N", "-o", "ConnectTimeout=12",
                "-o", "ExitOnForwardFailure=yes", "-o", "ForwardAgent=no",
                "-o", "StrictHostKeyChecking=accept-new",
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
                if localPortAcceptsConnections(localPort) {
                    HubAppLog.shared.record("ssh.tunnel.ready", category: "teslamate_import",
                                            fields: [
                                                "attempt": String(attempt + 1),
                                                "duration_ms": String(Int(Date().timeIntervalSince(started) * 1000))
                                            ])
                    return (tunnel, localPort)
                }
                if !tunnel.isRunning { break }
                Thread.sleep(forTimeInterval: 0.1)
            }

            if tunnel.isRunning {
                stopTunnel(tunnel)
                _ = diagnosticDrain.wait(timeout: .now() + 1)
                HubAppLog.shared.record("ssh.tunnel.timeout", category: "teslamate_import",
                                        level: "ERROR")
                throw HubActionError.commandFailed(
                    "The protected TeslaMate database tunnel did not become ready. Try again."
                )
            }
            _ = diagnosticDrain.wait(timeout: .now() + 1)
            let diagnostic = String(decoding: diagnosticOutput.snapshot(), as: UTF8.self)
            if diagnostic.localizedCaseInsensitiveContains("address already in use"), attempt < 2 {
                HubAppLog.shared.record("ssh.tunnel.port_collision", category: "teslamate_import",
                                        level: "WARN")
                continue
            }
            let message = tunnelFailureMessage(diagnostic)
            HubAppLog.shared.record("ssh.tunnel.rejected", category: "teslamate_import",
                                    level: "ERROR", fields: ["reason": tunnelFailureReason(diagnostic)])
            throw HubActionError.commandFailed(message)
        }
        throw HubActionError.commandFailed("Could not open the protected TeslaMate database tunnel.")
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
