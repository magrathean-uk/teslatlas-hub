#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only

set -eu

umask 022

PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin
export PATH
MACOSX_DEPLOYMENT_TARGET=13.0
export MACOSX_DEPLOYMENT_TARGET
COPYFILE_DISABLE=1
export COPYFILE_DISABLE

die() {
    printf '%s\n' "build-macos-app: $*" >&2
    exit 1
}

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
ICON_BUILD="$ROOT/scripts/build-app-icon.sh"
APP_SOURCE="$ROOT/macos/TeslatlasHubApp"
DERIVED="$ROOT/target/macos-app"
GENERATED="$DERIVED/generated"
PROJECT_DIR="$DERIVED/project"
PROJECT="$PROJECT_DIR/TeslatlasHubApp.xcodeproj"
RUST_BINARY="$ROOT/target/release/teslatlas-hub"
PROXY_BINARY="$ROOT/target/release/tesla-http-proxy"
FLEET_TELEMETRY_BINARY="$ROOT/target/release/fleet-telemetry"
SERVICE_PACKAGE="$GENERATED/TeslatlasHubService.pkg"
PRODUCT="$DERIVED/Build/Products/Release/Teslatlas Hub.app"
DIST="$ROOT/dist"
DIST_APP="$DIST/Teslatlas Hub.app"
DIST_PACKAGE="$DIST/TeslatlasHubService.pkg"
GO_EVIDENCE="$GENERATED/go-proxy-evidence"
FLEET_TELEMETRY_EVIDENCE="$GENERATED/fleet-telemetry-evidence"
LEGAL_BUNDLE="$GENERATED/dependency-legal"
DIST_GO_EVIDENCE="$DIST/go-proxy-evidence"
DIST_FLEET_TELEMETRY_EVIDENCE="$DIST/fleet-telemetry-evidence"
DIST_LEGAL_BUNDLE="$DIST/dependency-legal"

[ -x "$ICON_BUILD" ] && [ ! -L "$ICON_BUILD" ] \
    || die "app icon generator is missing or unsafe"
"$ICON_BUILD"

ensure_real_directory() {
    directory=$1
    if [ -e "$directory" ] || [ -L "$directory" ]; then
        [ -d "$directory" ] && [ ! -L "$directory" ] \
            || die "unsafe build directory: $directory"
    else
        /bin/mkdir -p "$directory"
    fi
}

minimum_macos() {
    /usr/bin/otool -l "$1" | /usr/bin/awk '
        $1 == "cmd" { command = $2 }
        command == "LC_BUILD_VERSION" && $1 == "minos" { print $2; exit }
        command == "LC_VERSION_MIN_MACOSX" && $1 == "version" { print $2; exit }
    '
}

require_macos_13_or_earlier() {
    required_macos=$(minimum_macos "$1")
    case "$required_macos" in
        ''|*[!0-9.]*) die "cannot read macOS deployment target: $1" ;;
    esac
    required_macos_major=${required_macos%%.*}
    [ "$required_macos_major" -le 13 ] \
        || die "requires macOS $required_macos, not macOS 13: $1"
}

is_executable_macho() {
    /usr/bin/otool -hv "$1" 2>/dev/null | /usr/bin/awk '
        $1 ~ /^MH_MAGIC(_64)?$/ && $5 == "EXECUTE" { executable = 1 }
        END { exit(executable ? 0 : 1) }
    '
}

reject_appledouble() {
    path=$1
    metadata=$(/usr/bin/find "$path" \( -name '._*' -o -name '.DS_Store' \) -print -quit)
    [ -z "$metadata" ] || die "AppleDouble metadata in $path"
}

command -v rustup >/dev/null 2>&1 || die "rustup is required for the pinned Rust build"
RUST_TOOLCHAIN=$(
    /usr/bin/sed -nE 's/^rust-version = "([0-9]+\.[0-9]+(\.[0-9]+)?)"$/\1/p' \
        "$ROOT/Cargo.toml"
)
case "$RUST_TOOLCHAIN" in
    *.*.*) ;;
    *.*) RUST_TOOLCHAIN="$RUST_TOOLCHAIN.0" ;;
    *) die "cannot read the pinned Rust version" ;;
esac
RUST_CARGO=$(rustup which --toolchain "$RUST_TOOLCHAIN" cargo) \
    || die "cannot find Rust $RUST_TOOLCHAIN cargo"
RUST_COMPILER=$(rustup which --toolchain "$RUST_TOOLCHAIN" rustc) \
    || die "cannot find Rust $RUST_TOOLCHAIN rustc"
[ -x "$RUST_CARGO" ] || die "cargo is not executable"
[ -x "$RUST_COMPILER" ] || die "rustc is not executable"
RUST_TOOLCHAIN_LIB="$($RUST_COMPILER --print sysroot)/lib"
[ -d "$RUST_TOOLCHAIN_LIB" ] || die "Rust toolchain library directory is missing"
if [ -n "${DYLD_FALLBACK_LIBRARY_PATH-}" ]; then
    RUST_TOOLCHAIN_LIB="$RUST_TOOLCHAIN_LIB:$DYLD_FALLBACK_LIBRARY_PATH"
fi
command -v xcodegen >/dev/null 2>&1 || die "xcodegen is required"
command -v xcodebuild >/dev/null 2>&1 || die "xcodebuild is required"
[ "$(/usr/bin/uname -m)" = arm64 ] || die "an Apple-silicon Mac is required"

version=$(
    /usr/bin/sed -nE '/^version = "[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?"$/ {
        s/^version = "([0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?)"$/\1/
        p
        q
    }' "$ROOT/Cargo.toml"
)
[ -n "$version" ] || die "cannot read Cargo package version"
/usr/bin/printf '%s\n' "$version" | /usr/bin/grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' \
    || die "invalid package version: $version"
marketing_version=${version%%-*}
case "$version" in
    *-alpha.[0-9]*) bundle_version="${marketing_version}a${version##*-alpha.}" ;;
    *-beta.[0-9]*) bundle_version="${marketing_version}b${version##*-beta.}" ;;
    *-rc.[0-9]*) bundle_version="${marketing_version}fc${version##*-rc.}" ;;
    *-*) die "unsupported prerelease for macOS version: $version" ;;
    *) bundle_version=$marketing_version ;;
esac

ensure_real_directory "$DERIVED"
ensure_real_directory "$GENERATED"
case "$PROJECT_DIR" in
    "$ROOT/target/macos-app/project") ;;
    *) die "refusing unsafe generated project directory" ;;
esac
if [ -e "$PROJECT_DIR" ] || [ -L "$PROJECT_DIR" ]; then
    [ -d "$PROJECT_DIR" ] && [ ! -L "$PROJECT_DIR" ] \
        || die "unsafe generated project directory"
    /usr/bin/find "$PROJECT_DIR" -depth -delete
fi
ensure_real_directory "$PROJECT_DIR"
ensure_real_directory "$DIST"
/usr/bin/ditto --noextattr --norsrc \
    "$APP_SOURCE/TeslatlasHubApp" "$PROJECT_DIR/TeslatlasHubApp"
/usr/bin/ditto --noextattr --norsrc \
    "$APP_SOURCE/TeslatlasHubAppTests" "$PROJECT_DIR/TeslatlasHubAppTests"
/usr/bin/install -m 0644 "$APP_SOURCE/project.yml" "$PROJECT_DIR/project.yml"

(
    cd "$ROOT"
    # The Xcode 27 linker corrupts optimized Rust proc-macro dylibs. Keep only
    # build-time helpers unoptimized; the shipped Hub binary remains optimized.
    DYLD_FALLBACK_LIBRARY_PATH="$RUST_TOOLCHAIN_LIB" RUSTC="$RUST_COMPILER" \
        CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_OPT_LEVEL=0 \
        "$RUST_CARGO" build --locked --release --bin teslatlas-hub
)

"$ROOT/scripts/build-tesla-command-proxy.sh" \
    --output "$PROXY_BINARY" >/dev/null
"$ROOT/scripts/build-fleet-telemetry-bridge.sh" \
    --target darwin-arm64 \
    --output "$FLEET_TELEMETRY_BINARY" >/dev/null

for generated_legal_input in "$GO_EVIDENCE" "$FLEET_TELEMETRY_EVIDENCE" "$LEGAL_BUNDLE"; do
    case "$generated_legal_input" in
        "$GENERATED"/*) ;;
        *) die "refusing unsafe generated legal input path" ;;
    esac
    if [ -e "$generated_legal_input" ] || [ -L "$generated_legal_input" ]; then
        [ -d "$generated_legal_input" ] && [ ! -L "$generated_legal_input" ] \
            || die "unsafe generated legal input path"
        /usr/bin/find "$generated_legal_input" -depth -delete
    fi
done
"$ROOT/scripts/go-proxy-evidence.py" --repo "$ROOT" \
    --proxy-binary "$PROXY_BINARY" --output-dir "$GO_EVIDENCE" >/dev/null
"$ROOT/scripts/fleet-telemetry-evidence.py" --repo "$ROOT" \
    --receiver-binary "$FLEET_TELEMETRY_BINARY" \
    --output-dir "$FLEET_TELEMETRY_EVIDENCE" >/dev/null
"$ROOT/scripts/legal-bundle.py" --repo "$ROOT" \
    --go-proxy-evidence "$GO_EVIDENCE" \
    --fleet-telemetry-evidence "$FLEET_TELEMETRY_EVIDENCE" \
    --output-dir "$LEGAL_BUNDLE" >/dev/null

"$ROOT/scripts/build-macos-service-package.sh" \
    --binary "$RUST_BINARY" \
    --proxy-binary "$PROXY_BINARY" \
    --fleet-telemetry-binary "$FLEET_TELEMETRY_BINARY" \
    --version "$version" \
    --legal-bundle "$LEGAL_BUNDLE" \
    --go-proxy-evidence "$GO_EVIDENCE" \
    --fleet-telemetry-evidence "$FLEET_TELEMETRY_EVIDENCE" \
    --output "$SERVICE_PACKAGE" >/dev/null

xcodegen generate --quiet \
    --spec "$PROJECT_DIR/project.yml" \
    --project "$PROJECT_DIR"
xcodebuild \
    -project "$PROJECT" \
    -scheme TeslatlasHubApp \
    -configuration Release \
    -derivedDataPath "$DERIVED" \
    clean >/dev/null
xcodebuild \
    -project "$PROJECT" \
    -scheme TeslatlasHubApp \
    -configuration Release \
    -derivedDataPath "$DERIVED" \
    ARCHS=arm64 \
    ONLY_ACTIVE_ARCH=YES \
    MACOSX_DEPLOYMENT_TARGET=13.0 \
    MARKETING_VERSION="$marketing_version" \
    CURRENT_PROJECT_VERSION="$bundle_version" \
    TESLATLAS_HUB_VERSION="$version" \
    CODE_SIGNING_ALLOWED=NO \
    build >/dev/null

[ -d "$PRODUCT" ] || die "Xcode did not produce the app"
resources="$PRODUCT/Contents/Resources"
/bin/mkdir -p "$resources"
[ -f "$resources/AppIcon.icns" ] && [ ! -L "$resources/AppIcon.icns" ] \
    || die "App icon resource is missing or unsafe"
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIconFile' "$PRODUCT/Contents/Info.plist")" = AppIcon ] \
    || die "App icon Info.plist value is missing"
/usr/bin/install -m 0755 "$RUST_BINARY" "$resources/teslatlas-hub"
# Keep a separately signed app resource for release provenance; the service
# package copy is the LaunchAgent's runtime sibling beside the Hub binary.
/usr/bin/install -m 0755 "$PROXY_BINARY" "$resources/tesla-http-proxy"
/usr/bin/install -m 0755 "$FLEET_TELEMETRY_BINARY" "$resources/fleet-telemetry"
/usr/bin/install -m 0644 "$SERVICE_PACKAGE" "$resources/TeslatlasHubService.pkg"
for required_release_legal_file in LICENSE NOTICE docs/legal/third-party-notices.md \
    docs/legal/provenance.md docs/legal/additional-terms.md \
    docs/legal/source-availability.md docs/releases/verification.md; do
    [ -f "$ROOT/$required_release_legal_file" ] \
        && [ ! -L "$ROOT/$required_release_legal_file" ] \
        || die "required release legal file is missing or unsafe: $required_release_legal_file"
done
/bin/mkdir -p "$resources/DependencyLegal"
for legal_component in "$LEGAL_BUNDLE"/*; do
    /usr/bin/install -m 0644 "$legal_component" \
        "$resources/DependencyLegal/$(/usr/bin/basename "$legal_component")"
done
"$ROOT/scripts/legal-bundle.py" --repo "$ROOT" \
    --verify-dir "$resources/DependencyLegal" \
    --go-proxy-evidence "$GO_EVIDENCE" \
    --fleet-telemetry-evidence "$FLEET_TELEMETRY_EVIDENCE" >/dev/null \
    || die "app dependency legal bundle is invalid"
for legal_entry in \
    'LICENSE|LICENSE' \
    'NOTICE|NOTICE' \
    'docs/legal/third-party-notices.md|THIRD_PARTY_NOTICES.md' \
    'docs/legal/provenance.md|PROVENANCE.md' \
    'docs/legal/trademarks.md|TRADEMARKS.md' \
    'docs/legal/privacy.md|PRIVACY.md' \
    'docs/legal/overview.md|LEGAL.md' \
    'docs/legal/additional-terms.md|ADDITIONAL_TERMS.md' \
    'docs/legal/source-availability.md|SOURCE_AVAILABILITY.md' \
    'docs/releases/verification.md|RELEASE_VERIFICATION.md'; do
    legal_source=${legal_entry%%|*}
    legal_name=${legal_entry#*|}
    if [ -f "$ROOT/$legal_source" ]; then
        /usr/bin/install -m 0644 "$ROOT/$legal_source" "$resources/$legal_name"
    fi
done
/usr/bin/xattr -cr "$resources" >/dev/null 2>&1 \
    || die "cannot clear resource metadata"
[ "$(/usr/bin/lipo -archs "$resources/teslatlas-hub")" = arm64 ] \
    || die "embedded Hub binary is not arm64-only"
is_executable_macho "$resources/teslatlas-hub" \
    || die "embedded Hub binary is not a Mach-O executable"
require_macos_13_or_earlier "$resources/teslatlas-hub"
[ -f "$resources/TeslatlasHubService.pkg" ] \
    || die "service package resource is missing"
[ "$(/usr/bin/lipo -archs "$resources/tesla-http-proxy")" = arm64 ] \
    || die "embedded Tesla command proxy is not arm64-only"
is_executable_macho "$resources/tesla-http-proxy" \
    || die "embedded Tesla command proxy is not a Mach-O executable"
require_macos_13_or_earlier "$resources/tesla-http-proxy"
[ "$(/usr/bin/lipo -archs "$resources/fleet-telemetry")" = arm64 ] \
    || die "embedded Fleet Telemetry receiver is not arm64-only"
is_executable_macho "$resources/fleet-telemetry" \
    || die "embedded Fleet Telemetry receiver is not a Mach-O executable"
require_macos_13_or_earlier "$resources/fleet-telemetry"
proxy_minimum_macos=$(
    /usr/bin/otool -l "$resources/tesla-http-proxy" | /usr/bin/awk '
        $1 == "cmd" { command = $2 }
        command == "LC_BUILD_VERSION" && $1 == "minos" { print $2; exit }
        command == "LC_VERSION_MIN_MACOSX" && $1 == "version" { print $2; exit }
    '
)
proxy_minimum_major=${proxy_minimum_macos%%.*}
case "$proxy_minimum_macos" in
    ''|*[!0-9.]*) die "cannot read proxy macOS deployment target" ;;
esac
[ "$proxy_minimum_major" -le 13 ] \
    || die "embedded Tesla command proxy requires macOS $proxy_minimum_macos"
reject_appledouble "$resources"
app_binary="$PRODUCT/Contents/MacOS/Teslatlas Hub"
[ "$(/usr/bin/lipo -archs "$app_binary")" = arm64 ] \
    || die "App binary is not arm64-only"
is_executable_macho "$app_binary" || die "App binary is not a Mach-O executable"
require_macos_13_or_earlier "$app_binary"

# Rust and Go already emit valid ad-hoc signatures for these Apple-silicon
# Mach-O binaries. Preserve their exact evidence-bound bytes here. Re-signing
# nested resources would change the subjects before the release script can bind
# them to the clean-build evidence.
for evidence_bound_binary in \
    "$resources/teslatlas-hub" \
    "$resources/tesla-http-proxy" \
    "$resources/fleet-telemetry"; do
    /usr/bin/codesign --verify --strict "$evidence_bound_binary" \
        || die "embedded release binary lacks its build-time ad-hoc signature"
done
/usr/bin/codesign --force --sign - --timestamp=none "$PRODUCT" >/dev/null
/usr/bin/codesign --verify --deep --strict "$PRODUCT"

evidence_proxy_sha256=$(python3 -c \
    'import json,sys; print(json.load(open(sys.argv[1]))["subject"]["sha256"])' \
    "$GO_EVIDENCE/go-component-manifest.json") \
    || die "cannot read Go proxy evidence subject"
embedded_proxy_sha256=$(/usr/bin/shasum -a 256 \
    "$resources/tesla-http-proxy" | /usr/bin/awk '{print $1}')
[ "$evidence_proxy_sha256" = "$embedded_proxy_sha256" ] \
    || die "app proxy changed after evidence generation"
evidence_fleet_telemetry_sha256=$(python3 -c \
    'import json,sys; print(json.load(open(sys.argv[1]))["subject"]["sha256"])' \
    "$FLEET_TELEMETRY_EVIDENCE/fleet-telemetry-component-manifest.json") \
    || die "cannot read Fleet Telemetry evidence subject"
embedded_fleet_telemetry_sha256=$(/usr/bin/shasum -a 256 \
    "$resources/fleet-telemetry" | /usr/bin/awk '{print $1}')
[ "$evidence_fleet_telemetry_sha256" = "$embedded_fleet_telemetry_sha256" ] \
    || die "app Fleet Telemetry receiver changed after evidence generation"

case "$DIST_APP" in
    "$ROOT/dist/Teslatlas Hub.app") ;;
    *) die "refusing unsafe distribution destination" ;;
esac
if [ -e "$DIST_APP" ] || [ -L "$DIST_APP" ]; then
    [ -d "$DIST_APP" ] && [ ! -L "$DIST_APP" ] \
        || die "refusing unsafe distribution app destination"
    /bin/rm -rf "$DIST_APP"
fi
/usr/bin/ditto --noextattr --norsrc "$PRODUCT" "$DIST_APP"
/usr/bin/xattr -cr "$DIST_APP" >/dev/null 2>&1 \
    || die "cannot clear distribution metadata"
reject_appledouble "$DIST_APP"
[ -f "$DIST_APP/Contents/Resources/AppIcon.icns" ] \
    && [ ! -L "$DIST_APP/Contents/Resources/AppIcon.icns" ] \
    || die "distribution app icon resource is missing or unsafe"
[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIconFile' "$DIST_APP/Contents/Info.plist")" = AppIcon ] \
    || die "distribution app icon Info.plist value is missing"
/usr/bin/codesign --verify --deep --strict "$DIST_APP"

/usr/bin/install -m 0644 "$SERVICE_PACKAGE" "$DIST_PACKAGE"
[ -f "$DIST_PACKAGE" ] && [ ! -L "$DIST_PACKAGE" ] \
    || die "final installer package is missing"
[ "$(/usr/bin/shasum -a 256 "$DIST_PACKAGE" | /usr/bin/awk '{print $1}')" \
    = "$(/usr/bin/shasum -a 256 "$DIST_APP/Contents/Resources/TeslatlasHubService.pkg" | /usr/bin/awk '{print $1}')" ] \
    || die "external service package does not match the app's embedded package"

for dist_evidence in "$DIST_GO_EVIDENCE" "$DIST_FLEET_TELEMETRY_EVIDENCE" "$DIST_LEGAL_BUNDLE"; do
    case "$dist_evidence" in
        "$DIST"/*) ;;
        *) die "refusing unsafe distribution evidence path" ;;
    esac
    if [ -e "$dist_evidence" ] || [ -L "$dist_evidence" ]; then
        [ -d "$dist_evidence" ] && [ ! -L "$dist_evidence" ] \
            || die "unsafe distribution evidence path"
        /usr/bin/find "$dist_evidence" -depth -delete
    fi
done
/usr/bin/ditto --noextattr --norsrc "$GO_EVIDENCE" "$DIST_GO_EVIDENCE"
/usr/bin/ditto --noextattr --norsrc "$FLEET_TELEMETRY_EVIDENCE" "$DIST_FLEET_TELEMETRY_EVIDENCE"
/usr/bin/ditto --noextattr --norsrc "$LEGAL_BUNDLE" "$DIST_LEGAL_BUNDLE"
"$ROOT/scripts/legal-bundle.py" --repo "$ROOT" --verify-dir "$DIST_LEGAL_BUNDLE" \
    --go-proxy-evidence "$DIST_GO_EVIDENCE" \
    --fleet-telemetry-evidence "$DIST_FLEET_TELEMETRY_EVIDENCE" >/dev/null \
    || die "distribution dependency legal bundle is invalid"

# The app and byte-identical service-only package are the local deliverables.
# Drop the exact Xcode staging tree so repeated builds do not retain hundreds
# of megabytes. Failed builds intentionally keep it for diagnosis.
case "$DERIVED" in
    "$ROOT/target/macos-app") ;;
    *) die "refusing unsafe Xcode staging cleanup" ;;
esac
[ -d "$DERIVED" ] && [ ! -L "$DERIVED" ] \
    || die "unsafe Xcode staging cleanup target"
/usr/bin/find "$DERIVED" -depth -delete
[ ! -e "$DERIVED" ] && [ ! -L "$DERIVED" ] \
    || die "cannot clean Xcode staging"

printf '%s\n' \
    'build-macos-app: local ad-hoc build; privileged install and update are disabled. Use a signed, notarized release for service installation.' >&2
printf '%s\n%s\n' "$DIST_APP" "$DIST_PACKAGE"
