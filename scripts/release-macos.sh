#!/bin/sh

set -eu

umask 077

usage() {
    cat <<'EOF'
Usage: scripts/release-macos.sh \
  --app PATH \
  --service-package PATH \
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
[ -n "$app_identity" ] || die "--app-identity is required"
[ -n "$installer_identity" ] || die "--installer-identity is required"
[ -n "$notary_profile" ] || die "--notary-profile is required"
[ -n "$output_arg" ] || die "--output-dir is required"

for tool in awk basename codesign dirname ditto find grep install mkdir mktemp mv \
    pkgbuild pkgutil plutil productsign security sed shasum xcrun; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required"
done

[ -d "$app" ] && [ ! -L "$app" ] || die "app must be a real directory"
[ -f "$service_package" ] && [ ! -L "$service_package" ] \
    || die "service package must be a regular file"
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
app_info="$app/Contents/Info.plist"
[ -f "$app_info" ] && [ ! -L "$app_info" ] \
    || die "app is missing Contents/Info.plist"
plutil -lint "$app_info" >/dev/null || die "app Info.plist is invalid"

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

expanded="$work/service-expanded"
pkgutil --expand-full "$service_package" "$expanded"

sign_named_binaries() {
    root=$1
    require_hub=$2
    for name in teslatlas-hub tesla-http-proxy; do
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
/usr/libexec/PlistBuddy -c "Add :TeslatlasServicePackageSHA256 string $service_package_sha256" "$release_info"
/usr/libexec/PlistBuddy -c "Add :TeslatlasReleaseTeamIdentifier string $app_team" "$release_info"
/usr/libexec/PlistBuddy -c 'Add :TeslatlasOfficialRelease bool true' "$release_info"
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
(
    cd "$release"
    shasum -a 256 "$(basename "$signed_package")" \
        "$(basename "$final_zip")" >SHA256SUMS
)

mv "$release" "$output"
printf '%s\n' "$output"
