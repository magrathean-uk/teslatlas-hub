#!/bin/sh

set -eu

umask 022

PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin
export PATH
MACOSX_DEPLOYMENT_TARGET=12.0
export MACOSX_DEPLOYMENT_TARGET
COPYFILE_DISABLE=1
export COPYFILE_DISABLE

die() {
    printf '%s\n' "build-macos-app: $*" >&2
    exit 1
}

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
APP_SOURCE="$ROOT/macos/TeslatlasHubApp"
DERIVED="$ROOT/target/macos-app"
GENERATED="$DERIVED/generated"
PROJECT_DIR="$DERIVED/project"
PROJECT="$PROJECT_DIR/TeslatlasHubApp.xcodeproj"
RUST_BINARY="$ROOT/target/release/teslatlas-hub"
PROXY_BINARY="$ROOT/target/release/tesla-http-proxy"
SERVICE_PACKAGE="$GENERATED/TeslatlasHubService.pkg"
PRODUCT="$DERIVED/Build/Products/Release/Teslatlas Hub.app"
DIST="$ROOT/dist"
DIST_APP="$DIST/Teslatlas Hub.app"

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

require_macos_12_or_earlier() {
    version=$(minimum_macos "$1")
    case "$version" in
        ''|*[!0-9.]*) die "cannot read macOS deployment target: $1" ;;
    esac
    major=${version%%.*}
    [ "$major" -le 12 ] \
        || die "requires macOS $version, not macOS 12: $1"
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

RUST_CARGO=$(command -v cargo) || die "cargo is required"
RUST_COMPILER=$(command -v rustc) || die "rustc is required"
if command -v rustup >/dev/null 2>&1; then
    RUST_CARGO=$(rustup which --toolchain stable cargo) \
        || die "cannot find the stable rustup cargo"
    RUST_COMPILER=$(rustup which --toolchain stable rustc) \
        || die "cannot find the stable rustup rustc"
fi
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

"$ROOT/scripts/build-macos-service-package.sh" \
    --binary "$RUST_BINARY" \
    --proxy-binary "$PROXY_BINARY" \
    --version "$version" \
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
    MACOSX_DEPLOYMENT_TARGET=12.0 \
    MARKETING_VERSION="$marketing_version" \
    CURRENT_PROJECT_VERSION="$bundle_version" \
    TESLATLAS_HUB_VERSION="$version" \
    CODE_SIGNING_ALLOWED=NO \
    build >/dev/null

[ -d "$PRODUCT" ] || die "Xcode did not produce the app"
resources="$PRODUCT/Contents/Resources"
/bin/mkdir -p "$resources"
/usr/bin/install -m 0755 "$RUST_BINARY" "$resources/teslatlas-hub"
# Keep a separately signed app resource for release provenance; the service
# package copy is the LaunchAgent's runtime sibling beside the Hub binary.
/usr/bin/install -m 0755 "$PROXY_BINARY" "$resources/tesla-http-proxy"
/usr/bin/install -m 0644 "$SERVICE_PACKAGE" "$resources/TeslatlasHubService.pkg"
for legal_file in LICENSE NOTICE THIRD_PARTY_NOTICES.md PROVENANCE.md TRADEMARKS.md PRIVACY.md LEGAL.md; do
    if [ -f "$ROOT/$legal_file" ]; then
        /usr/bin/install -m 0644 "$ROOT/$legal_file" "$resources/$legal_file"
    fi
done
/usr/bin/xattr -cr "$resources" >/dev/null 2>&1 \
    || die "cannot clear resource metadata"
[ "$(/usr/bin/lipo -archs "$resources/teslatlas-hub")" = arm64 ] \
    || die "embedded Hub binary is not arm64-only"
is_executable_macho "$resources/teslatlas-hub" \
    || die "embedded Hub binary is not a Mach-O executable"
require_macos_12_or_earlier "$resources/teslatlas-hub"
[ -f "$resources/TeslatlasHubService.pkg" ] \
    || die "service package resource is missing"
[ "$(/usr/bin/lipo -archs "$resources/tesla-http-proxy")" = arm64 ] \
    || die "embedded Tesla command proxy is not arm64-only"
is_executable_macho "$resources/tesla-http-proxy" \
    || die "embedded Tesla command proxy is not a Mach-O executable"
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
[ "$proxy_minimum_major" -le 12 ] \
    || die "embedded Tesla command proxy requires macOS $proxy_minimum_macos"
reject_appledouble "$resources"
app_binary="$PRODUCT/Contents/MacOS/Teslatlas Hub"
[ "$(/usr/bin/lipo -archs "$app_binary")" = arm64 ] \
    || die "App binary is not arm64-only"
is_executable_macho "$app_binary" || die "App binary is not a Mach-O executable"
require_macos_12_or_earlier "$app_binary"

/usr/bin/codesign --force --sign - --timestamp=none "$resources/teslatlas-hub" >/dev/null
/usr/bin/codesign --force --sign - --timestamp=none "$resources/tesla-http-proxy" >/dev/null
/usr/bin/codesign --force --deep --sign - --timestamp=none "$PRODUCT" >/dev/null
/usr/bin/codesign --verify --deep --strict "$PRODUCT"

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
/usr/bin/codesign --verify --deep --strict "$DIST_APP"

printf '%s\n' "$DIST_APP"
