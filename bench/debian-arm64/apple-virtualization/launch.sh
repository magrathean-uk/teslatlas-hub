#!/bin/zsh
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  print -u2 "This launcher requires Apple Silicon macOS."
  exit 2
fi

script_dir="${0:A:h}"
binary="${TMPDIR:-/tmp}/teslatlas-hub-debian-arm64-vz-$$"
entitlements="${TMPDIR:-/tmp}/teslatlas-hub-debian-arm64-vz-$$.entitlements"
trap 'rm -f "$binary" "$entitlements"' EXIT

cat >"$entitlements" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>com.apple.security.virtualization</key>
  <true/>
</dict>
</plist>
EOF

swiftc="$(command -v swiftc)"
"$swiftc" -O -framework AppKit -framework Virtualization \
  "$script_dir/run-debian-arm64-vz.swift" -o "$binary"
codesign --force --sign - --entitlements "$entitlements" "$binary"
exec "$binary" "$@"
