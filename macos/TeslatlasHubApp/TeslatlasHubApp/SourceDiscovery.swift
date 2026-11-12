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
    elif [ "${TESLATLAS_USE_SUDO:-1}" -eq 1 ]; then
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
    src_container_ids=$(docker_run ps --filter "label=com.docker.compose.project=$project_name" --format '{{.ID}}')
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
[ "$src_container_count" -eq 1 ] || { printf '%s\n' 'Several matching source containers are running.' >&2; exit 27; }
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
