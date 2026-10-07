#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

[ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || {
    echo 'test-macos-appkit: Apple-silicon macOS is required' >&2
    exit 69
}

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
if [ -z "${DEV_ENV_LOADED:-}" ] && [ -r "$HOME/dev/env.zsh" ]; then . "$HOME/dev/env.zsh" || exit 1; fi
: "${TMPDIR:?Load ~/dev/env.zsh to select scratch storage}"
source_root="$root/macos/TeslatlasHubApp"
stage=$(mktemp -d "$TMPDIR/teslatlas-appkit-test.XXXXXX")
trap 'find "$stage" -depth -delete 2>/dev/null || true' EXIT HUP INT TERM
project_root="$stage/TeslatlasHubApp"
derived_data="$stage/DerivedData"
mkdir -p "$project_root"

developer_dir=${DEVELOPER_DIR:-/Applications/Xcode-beta.app/Contents/Developer}
if [ -z "${DEVELOPER_DIR-}" ] && [ ! -d "$developer_dir" ]; then
    developer_dir=/Applications/Xcode.app/Contents/Developer
fi
[ -d "$developer_dir" ] && [ ! -L "$developer_dir" ] || {
    echo "test-macos-appkit: Xcode developer directory is unavailable: $developer_dir" >&2
    exit 69
}
DEVELOPER_DIR="$developer_dir"
export DEVELOPER_DIR
TESLATLAS_HUB_TEST_MODE=1
export TESLATLAS_HUB_TEST_MODE
PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
export PATH
command -v xcodegen >/dev/null 2>&1 || {
    echo 'test-macos-appkit: xcodegen is required' >&2
    exit 69
}
command -v xcodebuild >/dev/null 2>&1 || {
    echo 'test-macos-appkit: xcodebuild is required' >&2
    exit 69
}

cp "$source_root/project.yml" "$project_root/project.yml"
cp -R "$source_root/TeslatlasHubApp" "$project_root/TeslatlasHubApp"
cp -R "$source_root/TeslatlasHubAppTests" "$project_root/TeslatlasHubAppTests"
xcodegen generate --quiet --spec "$project_root/project.yml" --project "$project_root"
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
    test

echo 'test-macos-appkit: PASS'
