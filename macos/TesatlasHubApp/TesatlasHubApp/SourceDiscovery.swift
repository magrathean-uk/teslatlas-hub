// SPDX-License-Identifier: AGPL-3.0-only

import Foundation

enum SourceVersionClassification: Equatable {
    case supported(String)
    case tooOld(String)
    case unknown
}


enum SourceDiscovery {
    /// The only database fields discovery selects from the source container.
    static let allowListedNames = [SourceDeploymentFacts.databaseUser,
                                   SourceDeploymentFacts.databasePassword,
                                   SourceDeploymentFacts.databaseName]

    /// A Docker template that prints `NAME=value` only for the allow-listed names.
    static var allowListedValuesTemplate: String {
        let names = allowListedNames.map { "\"\($0)\"" }.joined(separator: " ")
        return "{{range .Config.Env}}{{if eq (index (split . \"=\") 0) \(names)}}{{println .}}{{end}}{{end}}"
    }

    static let script = #"""
set -eu

docker_run() {
    if [ "$(id -u)" -eq 0 ]; then
        docker "$@"
    elif [ "${TESATLAS_USE_SUDO:-1}" -eq 1 ]; then
        sudo -n docker "$@"
    else
        docker "$@"
    fi
}

database_service_ids=$(docker_run ps --filter label=com.docker.compose.service=\#(SourceDeploymentFacts.databaseService) --format '{{.ID}}')
[ -n "$database_service_ids" ] || { printf '%s\n' 'Database service container not found.' >&2; exit 22; }
project_names=
while IFS= read -r database_service_id; do
    [ -n "$database_service_id" ] || continue
    project_name=$(docker_run inspect -f '{{index .Config.Labels "com.docker.compose.project"}}' "$database_service_id")
    case "$project_name" in *[!a-z0-9_-]*|[-_]*|'') printf '%s\n' 'Compose project name is invalid.' >&2; exit 21;; esac
    [ -n "$project_name" ] || { printf '%s\n' 'Compose project could not be identified.' >&2; exit 21; }
    project_names=$(printf '%s\n%s\n' "$project_names" "$project_name" | awk 'NF && !seen[$0]++')
done <<EOF_DATABASE_IDS
$database_service_ids
EOF_DATABASE_IDS
candidate_rows=
while IFS= read -r project_name; do
    [ -n "$project_name" ] || continue
    src_container_ids=$(docker_run ps -a --filter "label=com.docker.compose.project=$project_name" --format '{{.ID}}')
    while IFS= read -r src_container_id; do
        [ -n "$src_container_id" ] || continue
        environment_names=$(docker_run inspect -f '{{range .Config.Env}}{{index (split . "=") 0}}{{println}}{{end}}' "$src_container_id")
        candidate=1
        for required_name in \#(SourceDeploymentFacts.databaseUser) \#(SourceDeploymentFacts.databasePassword) \#(SourceDeploymentFacts.databaseName) \#(SourceDeploymentFacts.sourceShapeMarker); do
            printf '%s\n' "$environment_names" | grep -Fxq "$required_name" || candidate=0
        done
        if [ "$candidate" -eq 1 ]; then
            candidate_rows="${candidate_rows}${candidate_rows:+
}${src_container_id}|${project_name}"
        fi
    done <<EOF_SOURCE_IDS
$src_container_ids
EOF_SOURCE_IDS
done <<EOF_PROJECT_NAMES
$project_names
EOF_PROJECT_NAMES
src_container_count=$(printf '%s\n' "$candidate_rows" | awk 'NF {count += 1} END {print count + 0}')
[ "$src_container_count" -gt 0 ] || { printf '%s\n' 'No matching source container was found.' >&2; exit 20; }
[ "$src_container_count" -eq 1 ] || { printf '%s\n' 'Several matching source containers were found.' >&2; exit 27; }
candidate_row=$(printf '%s\n' "$candidate_rows" | sed -n '1p')
src_container_id=${candidate_row%%|*}
project_name=${candidate_row#*|}
database_ids=$(docker_run ps --filter "label=com.docker.compose.project=$project_name" --filter label=com.docker.compose.service=\#(SourceDeploymentFacts.databaseService) --format '{{.ID}}' | head -n 2)
database_count=$(printf '%s\n' "$database_ids" | awk 'NF {count += 1} END {print count + 0}')
[ "$database_count" -gt 0 ] || { printf '%s\n' 'Database service container not found.' >&2; exit 22; }
[ "$database_count" -eq 1 ] || { printf '%s\n' 'Several database service containers are running.' >&2; exit 28; }
database_id=$(printf '%s\n' "$database_ids" | head -n 1)

# Read values only after one candidate has been selected, and only the allow-listed
# database fields: the template selects them by name inside Docker, so no other
# variable's value leaves the container record.
environment_values=$(docker_run inspect -f '\#(SourceDiscovery.allowListedValuesTemplate)' "$src_container_id")
env_value() {
    printf '%s\n' "$environment_values" | awk -F= -v key="$1" '$1 == key {sub(/^[^=]*=/, ""); print; exit}'
}
db_user=$(env_value \#(SourceDeploymentFacts.databaseUser))
db_pass=$(env_value \#(SourceDeploymentFacts.databasePassword))
db_name=$(env_value \#(SourceDeploymentFacts.databaseName))
[ -n "$db_user" ] && [ -n "$db_pass" ] && [ -n "$db_name" ] || { printf '%s\n' 'Source database credentials are incomplete.' >&2; exit 23; }
src_oci_version=$(docker_run inspect -f '{{index .Config.Labels "\#(SourceDeploymentFacts.ociVersionLabel)"}}' "$src_container_id" 2>/dev/null || true)
network=$(docker_run inspect -f '{{range $name, $value := .NetworkSettings.Networks}}{{println $name}}{{end}}' "$database_id" | head -n 1)
db_ip=$(docker_run inspect -f "{{with index .NetworkSettings.Networks \"$network\"}}{{.IPAddress}}{{end}}" "$database_id")
case "$db_ip" in *[!0-9.]*|'') printf '%s\n' 'Source database network address is invalid.' >&2; exit 24;; esac
car_ids=$(docker_run exec "$database_id" psql -U "$db_user" -d "$db_name" -Atc 'select id from cars order by id limit 2' 2>/dev/null)
car_id=$(printf '%s\n' "$car_ids" | head -n 1)
car_count=$(printf '%s\n' "$car_ids" | awk 'NF {count += 1} END {print count + 0}')
[ "$car_count" -eq 1 ] || { printf '%s\n' 'Guided import requires a source database with one vehicle.' >&2; exit 25; }
case "$car_id" in *[!0-9]*|'') printf '%s\n' 'Source vehicle could not be identified.' >&2; exit 26;; esac
encode() { printf '%s' "$1" | base64 | tr -d '\r\n'; }
printf 'user=%s\n' "$(encode "$db_user")"
printf 'password=%s\n' "$(encode "$db_pass")"
printf 'database=%s\n' "$(encode "$db_name")"
printf 'car=%s\n' "$(encode "$car_id")"
printf 'address=%s\n' "$(encode "$db_ip")"
printf 'oci_version=%s\n' "$(encode "$src_oci_version")"
"""#

    /// The only fields a discovery answer may carry; any other line is dropped unread.
    static let resultNames: Set<String> = ["user", "password", "database", "car", "address", "oci_version"]

    static func parse(_ output: String) throws -> [String: String] {
        var values: [String: String] = [:]
        for line in output.split(whereSeparator: \.isNewline) {
            let parts = line.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            guard parts.count == 2, resultNames.contains(String(parts[0])),
                  let data = Data(base64Encoded: String(parts[1])),
                  let value = String(data: data, encoding: .utf8) else { continue }
            values[String(parts[0])] = value
        }
        guard ["user", "password", "database", "car", "address"].allSatisfy({ !(values[$0] ?? "").isEmpty }) else {
            throw HubActionError.commandFailed(HubL10n.text("hub.SourceServerImporter.428.2461", fallback: "Source server returned incomplete setup details."))
        }
        return values
    }

    static func classifyVersion(ociVersion: String?) -> SourceVersionClassification {
        parseVersion(ociVersion)
    }

    static func admittedVersion(ociVersion: String?) throws -> String? {
        switch classifyVersion(ociVersion: ociVersion) {
        case let .supported(version): return version
        case .tooOld: throw HubActionError.commandFailed(HubL10n.text("hub.SourceServerImporter.747.2589", fallback: "Guided import requires source application release 4.4.0 or later."))
        case .unknown: return nil
        }
    }

    private static func parseVersion(_ rawValue: String?) -> SourceVersionClassification {
        guard var value = rawValue?.trimmingCharacters(in: .whitespacesAndNewlines),
              !value.isEmpty else { return .unknown }
        if value.hasPrefix("refs/tags/") { value.removeFirst("refs/tags/".count) }
        if value.contains("@") { return .unknown }
        if value.hasPrefix("v") { value.removeFirst() }
        let displayedVersion = value
        let stable = !value.contains("-")
        let core = value.split(separator: "+", maxSplits: 1).first.map(String.init) ?? value
        let components = core.split(separator: ".", omittingEmptySubsequences: false)
        let patchValue = components.count == 3 ? Int(components[2]) : 0
        guard (2...3).contains(components.count),
              let major = Int(components[0]),
              let minor = Int(components[1]),
              let patch = patchValue, major >= 0, minor >= 0, patch >= 0 else { return .unknown }
        guard stable else { return .tooOld(displayedVersion) }
        if major > 4 || (major == 4 && minor >= 4) {
            return .supported(displayedVersion)
        }
        return .tooOld(displayedVersion)
    }

}

/// Whether the source application's own container runs, as the stop check saw it.
enum SourceRunState: Equatable {
    case stopped
    case running
}

/// The stop check of the migration path (M09-T05): before the import and the token
/// takeover, the source application must be stopped. It runs over the SSH connection the
/// import already holds (its control socket) and asks Docker only for container state and
/// environment variable names: no value is selected, read or returned.
enum SourceStopCheck {
    /// Finds the source application containers the same way discovery does (the database
    /// service's Compose project, then the environment-name shape), but also among stopped
    /// containers, and prints how many were found and how many still run. A state other
    /// than exactly `false` counts as running.
    static let script = #"""
set -eu

docker_run() {
    if [ "$(id -u)" -eq 0 ]; then
        docker "$@"
    elif [ "${TESATLAS_USE_SUDO:-1}" -eq 1 ]; then
        sudo -n docker "$@"
    else
        docker "$@"
    fi
}

database_service_ids=$(docker_run ps --filter label=com.docker.compose.service=\#(SourceDeploymentFacts.databaseService) --format '{{.ID}}')
[ -n "$database_service_ids" ] || { printf '%s\n' 'Database service container not found.' >&2; exit 22; }
project_names=
while IFS= read -r database_service_id; do
    [ -n "$database_service_id" ] || continue
    project_name=$(docker_run inspect -f '{{index .Config.Labels "com.docker.compose.project"}}' "$database_service_id")
    case "$project_name" in *[!a-z0-9_-]*|[-_]*|'') printf '%s\n' 'Compose project name is invalid.' >&2; exit 21;; esac
    project_names=$(printf '%s\n%s\n' "$project_names" "$project_name" | awk 'NF && !seen[$0]++')
done <<EOF_DATABASE_IDS
$database_service_ids
EOF_DATABASE_IDS
found=0
running=0
while IFS= read -r project_name; do
    [ -n "$project_name" ] || continue
    container_ids=$(docker_run ps -a --filter "label=com.docker.compose.project=$project_name" --format '{{.ID}}')
    while IFS= read -r container_id; do
        [ -n "$container_id" ] || continue
        environment_names=$(docker_run inspect -f '{{range .Config.Env}}{{index (split . "=") 0}}{{println}}{{end}}' "$container_id")
        candidate=1
        for required_name in \#(SourceDeploymentFacts.databaseUser) \#(SourceDeploymentFacts.databasePassword) \#(SourceDeploymentFacts.databaseName) \#(SourceDeploymentFacts.sourceShapeMarker); do
            printf '%s\n' "$environment_names" | grep -Fxq "$required_name" || candidate=0
        done
        [ "$candidate" -eq 1 ] || continue
        found=$((found + 1))
        state=$(docker_run inspect -f '{{.State.Running}}' "$container_id")
        [ "$state" = "false" ] || running=$((running + 1))
    done <<EOF_CONTAINER_IDS
$container_ids
EOF_CONTAINER_IDS
done <<EOF_PROJECT_NAMES
$project_names
EOF_PROJECT_NAMES
printf 'found=%s\n' "$found"
printf 'running=%s\n' "$running"
"""#

    /// Reads the answer: exactly one `found` and one `running` count. No source container,
    /// a malformed or repeated line, or more running than found is a failure, never "stopped".
    static func parse(_ output: String) throws -> SourceRunState {
        var counts: [String: Int] = [:]
        for line in output.split(whereSeparator: \.isNewline) {
            let parts = line.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            guard parts.count == 2 else { continue }
            let name = String(parts[0])
            guard name == "found" || name == "running" else { continue }
            guard counts[name] == nil,
                  parts[1].allSatisfy(\.isASCII), parts[1].allSatisfy(\.isNumber),
                  let value = Int(parts[1]) else { throw unreadable }
            counts[name] = value
        }
        guard let found = counts["found"], let running = counts["running"], running <= found else {
            throw unreadable
        }
        guard found > 0 else {
            throw HubActionError.commandFailed(HubL10n.text(
                "hub.sourceTakeover.stopCheckNotFound",
                fallback: "Hub could not find the source application's container on the server, so it cannot confirm that it is stopped."))
        }
        return running == 0 ? .stopped : .running
    }

    private static var unreadable: Error {
        HubActionError.commandFailed(HubL10n.text(
            "hub.sourceTakeover.stopCheckUnreadable",
            fallback: "Hub could not read whether the source application is stopped. Check the server connection, then check again."))
    }

    /// A control socket in the import tunnel's private directory, if any.
    /// Ownership and socket type are checked here; liveness is not established.
    static func controlSocket(inSessionDirectory directory: URL) -> URL? {
        for name in ["c2", "c1", "c0"] {
            let candidate = directory.appendingPathComponent(name, isDirectory: false)
            var info = stat()
            guard lstat(candidate.path, &info) == 0,
                  info.st_mode & S_IFMT == S_IFSOCK,
                  info.st_uid == getuid() else { continue }
            return candidate
        }
        return nil
    }

    /// Prefer the tunnel's control socket without prompting (BatchMode). If the master
    /// is gone, OpenSSH can try configured keys or its agent in batch mode. ForwardAgent
    /// only prevents forwarding; host keys stay strictly checked.
    static func arguments(controlSocket: URL,
                          destination: String,
                          port: Int,
                          usePasswordlessSudo: Bool) -> [String] {
        [
            "-o", "ControlMaster=no",
            "-o", "ControlPath=\(controlSocket.path)",
            "-o", "ControlPersist=no",
            "-o", "BatchMode=yes",
            "-o", "ForwardAgent=no",
            "-o", "ConnectTimeout=12",
            "-o", "PermitLocalCommand=no",
            "-o", "RemoteCommand=none",
            "-o", "RequestTTY=no",
            "-o", "StdinNull=no",
            "-o", "StrictHostKeyChecking=yes",
            "-p", String(port),
            destination,
            "env", "TESATLAS_USE_SUDO=\(usePasswordlessSudo ? 1 : 0)", "sh", "-s"
        ]
    }
}
