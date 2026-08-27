#!/bin/sh

set -eu

umask 077

usage() {
    cat <<'EOF'
Usage: scripts/release-macos.sh \
  --app PATH \
  --service-package PATH \
  --go-proxy-evidence PATH \
  --fleet-telemetry-evidence PATH \
  --app-identity "Developer ID Application: Name (TEAMID)" \
  --installer-identity "Developer ID Installer: Name (TEAMID)" \
  --notary-profile PROFILE-TEAMID \
  --output-dir PATH

Builds release copies only. The input app and package are never modified.
The notary profile name must end in the same Team ID as both identities.
EOF
}

die() {
    printf '%s\n' "release-macos: $*" >&2
    exit 1
}

app=
service_package=
go_proxy_evidence=
fleet_telemetry_evidence=
app_identity=
installer_identity=
notary_profile=
output_arg=

while [ "$#" -gt 0 ]; do
    case "$1" in
        --app)
            [ "$#" -ge 2 ] || die "--app requires a path"
            app=$2
            shift 2
            ;;
        --service-package)
            [ "$#" -ge 2 ] || die "--service-package requires a path"
            service_package=$2
            shift 2
            ;;
        --go-proxy-evidence)
            [ "$#" -ge 2 ] || die "--go-proxy-evidence requires a path"
            go_proxy_evidence=$2
            shift 2
            ;;
        --fleet-telemetry-evidence)
            [ "$#" -ge 2 ] || die "--fleet-telemetry-evidence requires a path"
            fleet_telemetry_evidence=$2
            shift 2
            ;;
        --app-identity)
            [ "$#" -ge 2 ] || die "--app-identity requires a value"
            app_identity=$2
            shift 2
            ;;
        --installer-identity)
            [ "$#" -ge 2 ] || die "--installer-identity requires a value"
            installer_identity=$2
            shift 2
            ;;
        --notary-profile)
            [ "$#" -ge 2 ] || die "--notary-profile requires a value"
            notary_profile=$2
            shift 2
            ;;
        --output-dir)
            [ "$#" -ge 2 ] || die "--output-dir requires a path"
            output_arg=$2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            die "unknown argument: $1"
            ;;
    esac
done

[ "$(uname -s)" = Darwin ] || die "macOS is required"
[ -n "$app" ] || die "--app is required"
[ -n "$service_package" ] || die "--service-package is required"
[ -n "$go_proxy_evidence" ] || die "--go-proxy-evidence is required"
[ -n "$fleet_telemetry_evidence" ] || die "--fleet-telemetry-evidence is required"
[ -n "$app_identity" ] || die "--app-identity is required"
[ -n "$installer_identity" ] || die "--installer-identity is required"
[ -n "$notary_profile" ] || die "--notary-profile is required"
[ -n "$output_arg" ] || die "--output-dir is required"

for tool in awk basename codesign dirname ditto find grep install mkdir mktemp mv \
    pkgbuild pkgutil plutil productsign python3 security sed shasum sort xcrun; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required"
done

[ -d "$app" ] && [ ! -L "$app" ] || die "app must be a real directory"
[ -f "$service_package" ] && [ ! -L "$service_package" ] \
    || die "service package must be a regular file"
[ -d "$go_proxy_evidence" ] && [ ! -L "$go_proxy_evidence" ] \
    || die "Go proxy evidence must be a real directory"
[ -d "$fleet_telemetry_evidence" ] && [ ! -L "$fleet_telemetry_evidence" ] \
    || die "Fleet Telemetry evidence must be a real directory"
case "$(basename "$app")" in
    *.app) ;;
    *) die "app path must end in .app" ;;
esac
embedded_package="$app/Contents/Resources/TeslatlasHubService.pkg"
[ -f "$embedded_package" ] && [ ! -L "$embedded_package" ] \
    || die "app is missing Contents/Resources/TeslatlasHubService.pkg"
embedded_hub="$app/Contents/Resources/teslatlas-hub"
[ -f "$embedded_hub" ] && [ ! -L "$embedded_hub" ] \
    || die "app is missing Contents/Resources/teslatlas-hub"
embedded_proxy="$app/Contents/Resources/tesla-http-proxy"
[ -f "$embedded_proxy" ] && [ ! -L "$embedded_proxy" ] \
    || die "app is missing Contents/Resources/tesla-http-proxy"
embedded_fleet_telemetry="$app/Contents/Resources/fleet-telemetry"
[ -f "$embedded_fleet_telemetry" ] && [ ! -L "$embedded_fleet_telemetry" ] \
    || die "app is missing Contents/Resources/fleet-telemetry"
app_info="$app/Contents/Info.plist"
[ -f "$app_info" ] && [ ! -L "$app_info" ] \
    || die "app is missing Contents/Info.plist"
plutil -lint "$app_info" >/dev/null || die "app Info.plist is invalid"

script_root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd) \
    || die "cannot resolve repository root"
go_evidence_helper="$script_root/scripts/go-proxy-evidence.py"
[ -f "$go_evidence_helper" ] && [ ! -L "$go_evidence_helper" ] \
    || die "Go proxy evidence verifier is missing"
python3 "$go_evidence_helper" --repo "$script_root" \
    --verify-dir "$go_proxy_evidence" >/dev/null \
    || die "Go proxy evidence is invalid"
fleet_evidence_helper="$script_root/scripts/fleet-telemetry-evidence.py"
[ -f "$fleet_evidence_helper" ] && [ ! -L "$fleet_evidence_helper" ] \
    || die "Fleet Telemetry evidence verifier is missing"
python3 "$fleet_evidence_helper" --repo "$script_root" \
    --verify-dir "$fleet_telemetry_evidence" >/dev/null \
    || die "Fleet Telemetry evidence is invalid"
evidence_proxy_sha256=$(python3 -c \
    'import json,sys; print(json.load(open(sys.argv[1]))["subject"]["sha256"])' \
    "$go_proxy_evidence/go-component-manifest.json") \
    || die "cannot read Go proxy evidence subject"
embedded_proxy_sha256=$(shasum -a 256 "$embedded_proxy" | awk '{ print $1 }')
[ "$evidence_proxy_sha256" = "$embedded_proxy_sha256" ] \
    || die "Go proxy evidence does not match the unsigned app proxy"
evidence_fleet_telemetry_sha256=$(python3 -c \
    'import json,sys; print(json.load(open(sys.argv[1]))["subject"]["sha256"])' \
    "$fleet_telemetry_evidence/fleet-telemetry-component-manifest.json") \
    || die "cannot read Fleet Telemetry evidence subject"
embedded_fleet_telemetry_sha256=$(shasum -a 256 "$embedded_fleet_telemetry" | awk '{ print $1 }')
[ "$evidence_fleet_telemetry_sha256" = "$embedded_fleet_telemetry_sha256" ] \
    || die "Fleet Telemetry evidence does not match the unsigned app receiver"

identity_team() {
    value=$1
    printf '%s\n' "$value" | sed -nE 's/^.*\(([A-Z0-9]{10})\)$/\1/p'
}

case "$app_identity" in
    "Developer ID Application: "*) ;;
    *) die "app identity is not a Developer ID Application identity" ;;
esac
case "$installer_identity" in
    "Developer ID Installer: "*) ;;
    *) die "installer identity is not a Developer ID Installer identity" ;;
esac
app_team=$(identity_team "$app_identity")
installer_team=$(identity_team "$installer_identity")
[ -n "$app_team" ] || die "cannot read Team ID from app identity"
[ "$app_team" = "$installer_team" ] \
    || die "Developer ID identities belong to different teams"
case "$notary_profile" in
    *-"$app_team"|*."$app_team"|*_"$app_team") ;;
    *) die "notary profile name is not bound to Team ID $app_team" ;;
esac

# Finish every credential and input check before creating a staging directory.
identities=$(security find-identity -v 2>/dev/null) \
    || die "cannot read signing identities"
printf '%s\n' "$identities" | grep -Fq "\"$app_identity\"" \
    || die "app signing identity is unavailable"
printf '%s\n' "$identities" | grep -Fq "\"$installer_identity\"" \
    || die "installer signing identity is unavailable"
xcrun notarytool history \
    --keychain-profile "$notary_profile" \
    --output-format json >/dev/null \
    || die "notary profile validation failed"

output_parent=$(CDPATH='' cd "$(dirname "$output_arg")" && pwd) \
    || die "output parent does not exist"
output="$output_parent/$(basename "$output_arg")"
[ ! -e "$output" ] && [ ! -L "$output" ] \
    || die "output directory already exists"

stage=$(mktemp -d "$output_parent/.teslatlas-macos-release.XXXXXX") \
    || die "cannot create staging directory"
cleanup() {
    find "$stage" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

release="$stage/release"
work="$stage/work"
logs="$release/notary-logs"
mkdir -p "$release" "$work" "$logs"

app_name=$(basename "$app")
release_app="$release/$app_name"
ditto --noextattr --norsrc "$app" "$release_app"
release_app_proxy="$release_app/Contents/Resources/tesla-http-proxy"
[ -f "$release_app_proxy" ] && [ ! -L "$release_app_proxy" ] \
    || die "copied release app is missing tesla-http-proxy"
release_app_proxy_sha256=$(shasum -a 256 "$release_app_proxy" | awk '{ print $1 }')
[ "$release_app_proxy_sha256" = "$embedded_proxy_sha256" ] \
    || die "app proxy changed while staging the release"
[ "$release_app_proxy_sha256" = "$evidence_proxy_sha256" ] \
    || die "copied app proxy does not match Go evidence"
release_app_fleet_telemetry="$release_app/Contents/Resources/fleet-telemetry"
[ -f "$release_app_fleet_telemetry" ] && [ ! -L "$release_app_fleet_telemetry" ] \
    || die "copied release app is missing fleet-telemetry"
release_app_fleet_telemetry_sha256=$(shasum -a 256 "$release_app_fleet_telemetry" | awk '{ print $1 }')
[ "$release_app_fleet_telemetry_sha256" = "$embedded_fleet_telemetry_sha256" ] \
    || die "app Fleet Telemetry receiver changed while staging the release"
[ "$release_app_fleet_telemetry_sha256" = "$evidence_fleet_telemetry_sha256" ] \
    || die "copied app Fleet Telemetry receiver does not match evidence"
release_go_evidence="$release/go-proxy-evidence"
ditto --noextattr --norsrc "$go_proxy_evidence" "$release_go_evidence"
python3 "$go_evidence_helper" --repo "$script_root" \
    --verify-dir "$release_go_evidence" >/dev/null \
    || die "copied Go proxy evidence is invalid"
copied_evidence_proxy_sha256=$(python3 -c \
    'import json,sys; print(json.load(open(sys.argv[1]))["subject"]["sha256"])' \
    "$release_go_evidence/go-component-manifest.json") \
    || die "cannot read copied Go proxy evidence subject"
[ "$copied_evidence_proxy_sha256" = "$embedded_proxy_sha256" ] \
    || die "copied Go proxy evidence does not match the unsigned app proxy"
evidence_proxy_sha256=$copied_evidence_proxy_sha256
go_component_manifest_sha256=$(shasum -a 256 \
    "$release_go_evidence/go-component-manifest.json" | awk '{ print $1 }')
release_fleet_telemetry_evidence="$release/fleet-telemetry-evidence"
ditto --noextattr --norsrc "$fleet_telemetry_evidence" "$release_fleet_telemetry_evidence"
python3 "$fleet_evidence_helper" --repo "$script_root" \
    --verify-dir "$release_fleet_telemetry_evidence" >/dev/null \
    || die "copied Fleet Telemetry evidence is invalid"
copied_evidence_fleet_telemetry_sha256=$(python3 -c \
    'import json,sys; print(json.load(open(sys.argv[1]))["subject"]["sha256"])' \
    "$release_fleet_telemetry_evidence/fleet-telemetry-component-manifest.json") \
    || die "cannot read copied Fleet Telemetry evidence subject"
[ "$copied_evidence_fleet_telemetry_sha256" = "$embedded_fleet_telemetry_sha256" ] \
    || die "copied Fleet Telemetry evidence does not match the unsigned app receiver"
evidence_fleet_telemetry_sha256=$copied_evidence_fleet_telemetry_sha256
fleet_telemetry_component_manifest_sha256=$(shasum -a 256 \
    "$release_fleet_telemetry_evidence/fleet-telemetry-component-manifest.json" | awk '{ print $1 }')

expanded="$work/service-expanded"
pkgutil --expand-full "$service_package" "$expanded"
expanded_proxy="$expanded/Payload/Library/Application Support/Teslatlas Hub/bin/tesla-http-proxy"
[ -f "$expanded_proxy" ] && [ ! -L "$expanded_proxy" ] \
    || die "expanded package is missing tesla-http-proxy"
expanded_proxy_sha256=$(shasum -a 256 "$expanded_proxy" | awk '{ print $1 }')
[ "$expanded_proxy_sha256" = "$evidence_proxy_sha256" ] \
    || die "Go proxy evidence does not match the unsigned package proxy"
expanded_fleet_telemetry="$expanded/Payload/Library/Application Support/Teslatlas Hub/bin/fleet-telemetry"
[ -f "$expanded_fleet_telemetry" ] && [ ! -L "$expanded_fleet_telemetry" ] \
    || die "expanded package is missing fleet-telemetry"
expanded_fleet_telemetry_sha256=$(shasum -a 256 "$expanded_fleet_telemetry" | awk '{ print $1 }')
[ "$expanded_fleet_telemetry_sha256" = "$evidence_fleet_telemetry_sha256" ] \
    || die "Fleet Telemetry evidence does not match the unsigned package receiver"

sign_named_binaries() {
    root=$1
    require_hub=$2
    for name in teslatlas-hub tesla-http-proxy fleet-telemetry; do
        link=$(find "$root" -type l -name "$name" -print -quit)
        [ -z "$link" ] || die "refusing symlinked nested executable: $link"
        matches=$(find "$root" -type f -name "$name" -print)
        if [ "$name" = teslatlas-hub ] && [ "$require_hub" = 1 ]; then
            [ -n "$matches" ] || die "expanded package is missing teslatlas-hub"
        fi
        [ -n "$matches" ] || continue
        printf '%s\n' "$matches" | while IFS= read -r binary; do
            codesign --force --sign "$app_identity" \
                --timestamp --options runtime "$binary"
            codesign --verify --strict --verbose=2 "$binary"
        done
    done
}

sign_named_binaries "$expanded/Payload" 1
unsigned_package="$work/TeslatlasHubService-unsigned.pkg"
signed_package="$release/TeslatlasHubService.pkg"
package_info="$expanded/PackageInfo"
[ -f "$package_info" ] && [ ! -L "$package_info" ] \
    || die "expanded package is missing PackageInfo"
package_identifier=$(sed -nE 's/.* identifier="([^"]+)".*/\1/p' "$package_info")
package_version=$(sed -nE 's/.* version="([^"]+)".*/\1/p' "$package_info")
install_location=$(sed -nE 's/.* install-location="([^"]+)".*/\1/p' "$package_info")
[ "$package_identifier" = com.teslatlas.hub.service ] \
    || die "expanded package identifier is unexpected"
printf '%s\n' "$package_version" | grep -Eq '^[0-9A-Za-z][0-9A-Za-z.-]{0,63}$' \
    || die "expanded package version is invalid"
[ "$install_location" = / ] || die "expanded package install location is unexpected"
[ -d "$expanded/Scripts" ] && [ ! -L "$expanded/Scripts" ] \
    || die "expanded package is missing Scripts"
# `pkgutil --flatten-full` records the build user's ownership after signing the
# expanded payload. Rebuild the component package instead so Installer applies
# root-recommended ownership before the postinstall trust checks run.
pkgbuild \
    --root "$expanded/Payload" \
    --scripts "$expanded/Scripts" \
    --identifier "$package_identifier" \
    --version "$package_version" \
    --install-location "$install_location" \
    --ownership recommended \
    "$unsigned_package"
productsign --sign "$installer_identity" "$unsigned_package" "$signed_package"
pkgutil --check-signature "$signed_package" >/dev/null

notarize() {
    artifact=$1
    label=$2
    submit_log="$logs/$label-submit.json"
    detail_log="$logs/$label-log.json"
    xcrun notarytool submit "$artifact" \
        --keychain-profile "$notary_profile" \
        --no-s3-acceleration \
        --wait \
        --timeout 30m \
        --output-format json >"$submit_log"
    status=$(plutil -extract status raw -o - "$submit_log")
    submission_id=$(plutil -extract id raw -o - "$submit_log")
    printf '%s\n' "$submission_id" \
        | grep -Eq '^[[:xdigit:]]{8}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{12}$' \
        || die "$label notarization returned an invalid submission ID"
    xcrun notarytool log "$submission_id" \
        --keychain-profile "$notary_profile" "$detail_log"
    [ "$status" = Accepted ] \
        || die "$label notarization returned ${status:-no status}"
}

notarize "$signed_package" service-package
xcrun stapler staple "$signed_package"
xcrun stapler validate "$signed_package"

release_resources="$release_app/Contents/Resources"
install -m 0644 "$signed_package" "$release_resources/TeslatlasHubService.pkg"
service_package_sha256=$(shasum -a 256 "$signed_package" | awk '{ print $1 }')
release_info="$release_app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Delete :TeslatlasServicePackageSHA256' "$release_info" >/dev/null 2>&1 || true
/usr/libexec/PlistBuddy -c 'Delete :TeslatlasReleaseTeamIdentifier' "$release_info" >/dev/null 2>&1 || true
/usr/libexec/PlistBuddy -c 'Delete :TeslatlasOfficialRelease' "$release_info" >/dev/null 2>&1 || true
/usr/libexec/PlistBuddy -c 'Delete :TeslatlasUnsignedProxySHA256' "$release_info" >/dev/null 2>&1 || true
/usr/libexec/PlistBuddy -c 'Delete :TeslatlasGoEvidenceManifestSHA256' "$release_info" >/dev/null 2>&1 || true
/usr/libexec/PlistBuddy -c 'Delete :TeslatlasUnsignedFleetTelemetrySHA256' "$release_info" >/dev/null 2>&1 || true
/usr/libexec/PlistBuddy -c 'Delete :TeslatlasFleetTelemetryEvidenceManifestSHA256' "$release_info" >/dev/null 2>&1 || true
/usr/libexec/PlistBuddy -c "Add :TeslatlasServicePackageSHA256 string $service_package_sha256" "$release_info"
/usr/libexec/PlistBuddy -c "Add :TeslatlasReleaseTeamIdentifier string $app_team" "$release_info"
/usr/libexec/PlistBuddy -c 'Add :TeslatlasOfficialRelease bool true' "$release_info"
/usr/libexec/PlistBuddy -c "Add :TeslatlasUnsignedProxySHA256 string $evidence_proxy_sha256" "$release_info"
/usr/libexec/PlistBuddy -c "Add :TeslatlasGoEvidenceManifestSHA256 string $go_component_manifest_sha256" "$release_info"
/usr/libexec/PlistBuddy -c "Add :TeslatlasUnsignedFleetTelemetrySHA256 string $evidence_fleet_telemetry_sha256" "$release_info"
/usr/libexec/PlistBuddy -c "Add :TeslatlasFleetTelemetryEvidenceManifestSHA256 string $fleet_telemetry_component_manifest_sha256" "$release_info"
plutil -lint "$release_info" >/dev/null || die "release trust metadata is invalid"
sign_named_binaries "$release_resources" 1
codesign --force --sign "$app_identity" \
    --timestamp --options runtime "$release_app"
codesign --verify --deep --strict --verbose=2 "$release_app"

submission_zip="$work/${app_name%.app}-submission.zip"
ditto -c -k --sequesterRsrc --keepParent "$release_app" "$submission_zip"
notarize "$submission_zip" app
xcrun stapler staple "$release_app"
xcrun stapler validate "$release_app"
codesign --verify --deep --strict --verbose=2 "$release_app"

final_zip="$release/${app_name%.app}.zip"
ditto -c -k --sequesterRsrc --keepParent "$release_app" "$final_zip"
find "$release_app" -depth -delete \
    || die "cannot remove redundant unpacked release app"
[ ! -e "$release_app" ] && [ ! -L "$release_app" ] \
    || die "redundant unpacked release app remains"
(
    cd "$release"
    {
        shasum -a 256 "$(basename "$signed_package")" \
            "$(basename "$final_zip")"
        find go-proxy-evidence -type f -print | LC_ALL=C sort | while IFS= read -r evidence_file; do
            shasum -a 256 "$evidence_file"
        done
        find fleet-telemetry-evidence -type f -print | LC_ALL=C sort | while IFS= read -r evidence_file; do
            shasum -a 256 "$evidence_file"
        done
    } >SHA256SUMS
)

mv "$release" "$output"
printf '%s\n' "$output"
