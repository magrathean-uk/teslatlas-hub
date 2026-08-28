#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

[ "$(uname -s)" = Darwin ] || {
    echo 'build-app-icon: macOS iconutil is required' >&2
    exit 69
}
command -v iconutil >/dev/null 2>&1 || {
    echo 'build-app-icon: iconutil is required' >&2
    exit 69
}

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
source_iconset="$root/macos/TeslatlasHubApp/Artwork/AppIcon.iconset"
app_icon="$root/macos/TeslatlasHubApp/TeslatlasHubApp/Resources/AppIcon.icns"
documentation_icon="$root/docs/assets/teslatlas-hub-icon.png"

[ -d "$source_iconset" ] && [ ! -L "$source_iconset" ] || {
    echo 'build-app-icon: canonical iconset is missing or unsafe' >&2
    exit 65
}
for required in \
    icon_16x16.png icon_16x16@2x.png \
    icon_32x32.png icon_32x32@2x.png \
    icon_128x128.png icon_128x128@2x.png \
    icon_256x256.png icon_256x256@2x.png \
    icon_512x512.png icon_512x512@2x.png; do
    [ -f "$source_iconset/$required" ] && [ ! -L "$source_iconset/$required" ] || {
        echo "build-app-icon: missing canonical input: $required" >&2
        exit 65
    }
done

stage=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-app-icon.XXXXXX")
trap 'find "$stage" -depth -delete 2>/dev/null || true' EXIT HUP INT TERM
iconutil -c icns -o "$stage/AppIcon.icns" "$source_iconset"
install -m 0644 "$stage/AppIcon.icns" "$app_icon"
install -m 0644 "$source_iconset/icon_512x512.png" "$documentation_icon"

echo 'build-app-icon: PASS'
