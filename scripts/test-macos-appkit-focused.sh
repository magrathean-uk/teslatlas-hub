#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

[ "$#" -gt 0 ] || {
    echo 'usage: test-macos-appkit-focused.sh TestClass[/testMethod] [...]' >&2
    exit 64
}
[ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || {
    echo 'test-macos-appkit-focused: Apple-silicon macOS is required' >&2
    exit 69
}

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
source_root="$root/macos/TeslatlasHubApp"
stage=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-appkit-focused.XXXXXX")
trap 'find "$stage" -depth -delete 2>/dev/null || true' EXIT HUP INT TERM
project_root="$stage/TeslatlasHubApp"
derived_data="$stage/DerivedData"
mkdir -p "$project_root"

developer_dir=${DEVELOPER_DIR:-/Applications/Xcode-beta.app/Contents/Developer}
[ -d "$developer_dir" ] && [ ! -L "$developer_dir" ] || {
    echo "test-macos-appkit-focused: Xcode developer directory is unavailable: $developer_dir" >&2
    exit 69
}
DEVELOPER_DIR="$developer_dir"
TESLATLAS_HUB_TEST_MODE=1
PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
export DEVELOPER_DIR TESLATLAS_HUB_TEST_MODE PATH

command -v xcodegen >/dev/null 2>&1 || {
    echo 'test-macos-appkit-focused: xcodegen is required' >&2
    exit 69
}
command -v xcodebuild >/dev/null 2>&1 || {
    echo 'test-macos-appkit-focused: xcodebuild is required' >&2
    exit 69
}

cp "$source_root/project.yml" "$project_root/project.yml"
cp -R "$source_root/TeslatlasHubApp" "$project_root/TeslatlasHubApp"
cp -R "$source_root/TeslatlasHubAppTests" "$project_root/TeslatlasHubAppTests"
xcodegen generate --quiet --spec "$project_root/project.yml" --project "$project_root"

set -- "$@"
only_testing=
for selector in "$@"; do
    only_testing="$only_testing -only-testing:TeslatlasHubAppTests/$selector"
done
result_bundle_args=
if [ -n "${TESLATLAS_HUB_SNAPSHOT_RESULT_BUNDLE:-}" ]; then
    case "$TESLATLAS_HUB_SNAPSHOT_RESULT_BUNDLE" in
        *[[:space:]]*)
            echo 'test-macos-appkit-focused: snapshot result path cannot contain whitespace' >&2
            exit 64
            ;;
    esac
    result_bundle_args="-resultBundlePath $TESLATLAS_HUB_SNAPSHOT_RESULT_BUNDLE"
fi

# Selectors are caller-provided XCTest identifiers, not shell fragments.
# shellcheck disable=SC2086
xcodebuild \
    -quiet \
    -project "$project_root/TeslatlasHubApp.xcodeproj" \
    -scheme TeslatlasHubApp \
    -configuration Debug \
    -derivedDataPath "$derived_data" \
    -destination 'platform=macOS,arch=arm64' \
    ARCHS=arm64 \
    ONLY_ACTIVE_ARCH=YES \
    CODE_SIGNING_ALLOWED=NO \
    $only_testing \
    $result_bundle_args \
    test

echo 'test-macos-appkit-focused: PASS'
