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
if [ -z "${DEV_ENV_LOADED:-}" ] && [ -r "$HOME/dev/env.zsh" ]; then . "$HOME/dev/env.zsh" || exit 1; fi
: "${TMPDIR:?Load ~/dev/env.zsh to select scratch storage}"
source_root="$root/macos/TeslatlasHubApp"
stage=$(mktemp -d "$TMPDIR/teslatlas-appkit-focused.XXXXXX")
# R-E1F6-22: test fixtures live in <per-user Darwin temp dir>/teslatlas-hub-tests (never under the home);
# remove exactly that folder after every run, including an interrupted one, unless another run is alive.
# Another live run has its private volume (/Volumes/TeslatlasHubTests-<its script pid>) mounted and shares the
# folder until it detaches: leave the folder for that run to remove.
other_hub_test_run_alive() {
    for candidate in $(mount | sed -n 's|^.* on /Volumes/TeslatlasHubTests-\([0-9][0-9]*\) (apfs,.*$|\1|p'); do
        [ "$candidate" = "$$" ] && continue
        kill -0 "$candidate" 2>/dev/null && return 0
    done
    return 1
}
clean_hub_test_scratch() {
    other_hub_test_run_alive && return 0
    darwin_tmp=$(getconf DARWIN_USER_TEMP_DIR 2>/dev/null) || return 0
    [ -n "$darwin_tmp" ] && [ -d "$darwin_tmp" ] || return 0
    scratch="$(CDPATH='' cd -- "$darwin_tmp" && pwd -P)/teslatlas-hub-tests" || return 0
    if [ -L "$scratch" ]; then
        rm -f "$scratch"
    elif [ -d "$scratch" ]; then
        chmod -R u+rwX "$scratch" 2>/dev/null || true
        rm -rf "$scratch"
    fi
}
# The app's own path rules (a descriptor walk from / that refuses links and group-writable ancestors) reject
# every path below the per-user temporary directory (/var is a link) and the shared external volume, so the
# fixtures they validate live on a small disk image mounted hidden for the run and detached afterwards.
hub_test_volume=
detach_hub_test_volume() {
    [ -n "$hub_test_volume" ] || return 0
    hdiutil detach -quiet "$hub_test_volume" >/dev/null 2>&1 || {
        sleep 1
        hdiutil detach -quiet -force "$hub_test_volume" >/dev/null 2>&1 || true
    }
    hub_test_volume=
}
trap 'detach_hub_test_volume; find "$stage" -depth -delete 2>/dev/null || true; clean_hub_test_scratch' EXIT HUP INT TERM
project_root="$stage/TeslatlasHubApp"
derived_data="$stage/DerivedData"
mkdir -p "$project_root"

developer_dir=${DEVELOPER_DIR:-/Applications/Xcode-beta.app/Contents/Developer}
if [ -z "${DEVELOPER_DIR-}" ] && [ ! -d "$developer_dir" ]; then
    developer_dir=/Applications/Xcode.app/Contents/Developer
fi
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

command -v hdiutil >/dev/null 2>&1 || {
    echo 'test-macos-appkit-focused: hdiutil is required' >&2
    exit 69
}
volume="/Volumes/TeslatlasHubTests-$$"
[ ! -e "$volume" ] || {
    echo "test-macos-appkit-focused: $volume already exists" >&2
    exit 69
}
hdiutil create -size 64m -type SPARSE -fs APFS -volname "TeslatlasHubTests-$$" -quiet "$stage/hub-tests" &&
    hdiutil attach -nobrowse -noautoopen -noverify -quiet "$stage/hub-tests.sparseimage" || {
    echo 'test-macos-appkit-focused: cannot mount the private test volume' >&2
    exit 69
}
hub_test_volume=$volume
[ -d "$hub_test_volume" ] || {
    echo 'test-macos-appkit-focused: the private test volume is not mounted' >&2
    exit 69
}
TEST_RUNNER_TESLATLAS_HUB_TEST_VOLUME=$hub_test_volume
export TEST_RUNNER_TESLATLAS_HUB_TEST_VOLUME

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
    -parallel-testing-enabled NO \
    CODE_SIGNING_ALLOWED=NO \
    $only_testing \
    $result_bundle_args \
    test

echo 'test-macos-appkit-focused: PASS'
