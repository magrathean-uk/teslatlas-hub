#!/bin/sh

set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
SCRIPT="$ROOT/scripts/release-macos.sh"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-release-test.XXXXXX")
cleanup() {
    find "$TMP" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

fail() {
    printf '%s\n' "test-macos-release: $*" >&2
    exit 1
}

BIN="$TMP/bin"
INPUT="$TMP/input"
LOG="$TMP/tool.log"
mkdir -p "$BIN" "$INPUT/Teslatlas Hub.app/Contents/MacOS" \
    "$INPUT/Teslatlas Hub.app/Contents/Resources"
: >"$INPUT/Teslatlas Hub.app/Contents/MacOS/Teslatlas Hub"
: >"$INPUT/Teslatlas Hub.app/Contents/Resources/teslatlas-hub"
: >"$INPUT/Teslatlas Hub.app/Contents/Resources/tesla-http-proxy"
: >"$INPUT/Teslatlas Hub.app/Contents/Resources/TeslatlasHubService.pkg"
: >"$INPUT/service.pkg"
: >"$LOG"

cat >"$BIN/uname" <<'EOF'
#!/bin/sh
printf '%s\n' Darwin
EOF

cat >"$BIN/security" <<'EOF'
#!/bin/sh
cat <<'OUT'
  1) A "Developer ID Application: Bolyki Gyorgy (4AA2EMZ2HA)"
  2) B "Developer ID Installer: Bolyki Gyorgy (4AA2EMZ2HA)"
OUT
EOF

cat >"$BIN/codesign" <<'EOF'
#!/bin/sh
printf 'codesign %s\n' "$*" >>"$RELEASE_TEST_LOG"
exit 0
EOF

cat >"$BIN/ditto" <<'EOF'
#!/bin/sh
printf 'ditto %s\n' "$*" >>"$RELEASE_TEST_LOG"
previous=
for arg in "$@"; do
    source=$previous
    previous=$arg
done
destination=$previous
case " $* " in
    *' -c '*) : >"$destination" ;;
    *) cp -R "$source" "$destination" ;;
esac
EOF

cat >"$BIN/pkgutil" <<'EOF'
#!/bin/sh
printf 'pkgutil %s\n' "$*" >>"$RELEASE_TEST_LOG"
case "$1" in
    --expand-full)
        mkdir -p "$3/Payload/Library/Application Support/Teslatlas Hub/bin"
        : >"$3/Payload/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
        : >"$3/Payload/Library/Application Support/Teslatlas Hub/bin/tesla-http-proxy"
        ;;
    --flatten-full) : >"$3" ;;
    --check-signature) ;;
    *) exit 2 ;;
esac
EOF

cat >"$BIN/productsign" <<'EOF'
#!/bin/sh
printf 'productsign %s\n' "$*" >>"$RELEASE_TEST_LOG"
previous=
for arg in "$@"; do
    source=$previous
    previous=$arg
done
cp "$source" "$previous"
EOF

cat >"$BIN/xcrun" <<'EOF'
#!/bin/sh
printf 'xcrun %s\n' "$*" >>"$RELEASE_TEST_LOG"
[ "$1" = notarytool ] || {
    [ "$1" = stapler ] && exit 0
    exit 2
}
case "$2" in
    history) printf '%s\n' '{"history":[]}' ;;
    submit)
        case "$3" in
            *.pkg) id=11111111-1111-1111-1111-111111111111 ;;
            *) id=22222222-2222-2222-2222-222222222222 ;;
        esac
        printf '{"id":"%s","status":"Accepted"}\n' "$id"
        ;;
    log)
        previous=
        for arg in "$@"; do previous=$arg; done
        printf '%s\n' '{"issues":null}' >"$previous"
        ;;
    *) exit 2 ;;
esac
EOF

chmod 0755 "$BIN"/*
export PATH="$BIN:/usr/bin:/bin:/usr/sbin:/sbin"
export RELEASE_TEST_LOG="$LOG"

APP_ID='Developer ID Application: Bolyki Gyorgy (4AA2EMZ2HA)'
PKG_ID='Developer ID Installer: Bolyki Gyorgy (4AA2EMZ2HA)'

run_release() {
    "$SCRIPT" \
        --app "$INPUT/Teslatlas Hub.app" \
        --service-package "$INPUT/service.pkg" \
        --app-identity "$APP_ID" \
        --installer-identity "$1" \
        --notary-profile "$2" \
        --output-dir "$3"
}

bad_output="$TMP/bad-team"
if run_release 'Developer ID Installer: Wrong Team (NPSQV9WYS5)' \
    teslatlas-hub-notary-4AA2EMZ2HA "$bad_output" >/dev/null 2>&1; then
    fail "different identity teams were accepted"
fi
[ ! -e "$bad_output" ] || fail "team mismatch created output"
[ ! -s "$LOG" ] || fail "team mismatch invoked external tools"

bad_profile_output="$TMP/bad-profile"
if run_release "$PKG_ID" teslatlas-hub-notary-NPSQV9WYS5 \
    "$bad_profile_output" >/dev/null 2>&1; then
    fail "different notary profile team was accepted"
fi
[ ! -e "$bad_profile_output" ] || fail "profile mismatch created output"
[ ! -s "$LOG" ] || fail "profile mismatch invoked external tools"

good_output="$TMP/release"
run_release "$PKG_ID" teslatlas-hub-notary-4AA2EMZ2HA "$good_output" >/dev/null
[ -z "$(find "$TMP" -maxdepth 1 -name '.teslatlas-macos-release.*' -print -quit)" ] \
    || fail "successful release retained staging data"
[ -d "$good_output/Teslatlas Hub.app" ] || fail "release app is missing"
[ -f "$good_output/TeslatlasHubService.pkg" ] || fail "release pkg is missing"
[ -f "$good_output/Teslatlas Hub.zip" ] || fail "final zip is missing"
[ -f "$good_output/SHA256SUMS" ] || fail "checksums are missing"
[ -f "$good_output/notary-logs/service-package-submit.json" ] \
    || fail "package submit log is missing"
[ -f "$good_output/notary-logs/service-package-log.json" ] \
    || fail "package detail log is missing"
[ -f "$good_output/notary-logs/app-submit.json" ] \
    || fail "app submit log is missing"
[ -f "$good_output/notary-logs/app-log.json" ] \
    || fail "app detail log is missing"

line_number() {
    grep -n "$1" "$LOG" | head -1 | cut -d: -f1
}

payload_sign=$(line_number 'codesign .*service-expanded/Payload.*teslatlas-hub')
product_sign=$(line_number '^productsign ')
package_submit=$(line_number 'xcrun notarytool submit .*TeslatlasHubService.pkg')
package_staple=$(line_number 'xcrun stapler staple .*TeslatlasHubService.pkg')
app_nested_sign=$(line_number 'codesign .*release/Teslatlas Hub.app/Contents/Resources/teslatlas-hub')
app_sign=$(line_number 'codesign --force --sign .*release/Teslatlas Hub.app$')
app_submit=$(line_number 'xcrun notarytool submit .*submission.zip')
app_staple=$(line_number 'xcrun stapler staple .*Teslatlas Hub.app')

[ "$payload_sign" -lt "$product_sign" ] || fail "package payload was signed too late"
[ "$product_sign" -lt "$package_submit" ] || fail "package was notarized before signing"
[ "$package_submit" -lt "$package_staple" ] || fail "package staple order is wrong"
[ "$package_staple" -lt "$app_nested_sign" ] || fail "unstapled package entered app"
[ "$app_nested_sign" -lt "$app_sign" ] || fail "app was signed before nested code"
[ "$app_sign" -lt "$app_submit" ] || fail "app was notarized before signing"
[ "$app_submit" -lt "$app_staple" ] || fail "app staple order is wrong"

grep -Fq -- '--timestamp --options runtime' "$LOG" \
    || fail "hardened runtime or secure timestamp is missing"
[ "$(grep -c 'codesign --force .*tesla-http-proxy' "$LOG")" -eq 2 ] \
    || fail "package and app proxy copies were not both signed"
(
    cd "$good_output"
    shasum -a 256 -c SHA256SUMS >/dev/null
) || fail "checksums do not verify"

printf '%s\n' "macOS release script tests passed"
