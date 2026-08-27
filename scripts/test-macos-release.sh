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
/usr/bin/plutil -create xml1 "$INPUT/Teslatlas Hub.app/Contents/Info.plist"
: >"$INPUT/Teslatlas Hub.app/Contents/MacOS/Teslatlas Hub"
: >"$INPUT/Teslatlas Hub.app/Contents/Resources/teslatlas-hub"
: >"$INPUT/Teslatlas Hub.app/Contents/Resources/TeslatlasHubService.pkg"
: >"$INPUT/service.pkg"
: >"$LOG"

GO_EVIDENCE="$INPUT/go-proxy-evidence"
TEST_PROXY="$INPUT/Teslatlas Hub.app/Contents/Resources/tesla-http-proxy"
FLEET_TELEMETRY_EVIDENCE="$INPUT/fleet-telemetry-evidence"
TEST_FLEET_TELEMETRY="$INPUT/Teslatlas Hub.app/Contents/Resources/fleet-telemetry"
if [ -n "${TESLATLAS_TEST_GO_PROXY:-}" ] || [ -n "${TESLATLAS_TEST_GO_EVIDENCE:-}" ]; then
    [ -n "${TESLATLAS_TEST_GO_PROXY:-}" ] \
        && [ -n "${TESLATLAS_TEST_GO_EVIDENCE:-}" ] \
        || fail "both TESLATLAS_TEST_GO_PROXY and TESLATLAS_TEST_GO_EVIDENCE are required"
    cp "$TESLATLAS_TEST_GO_PROXY" "$TEST_PROXY"
    cp -R "$TESLATLAS_TEST_GO_EVIDENCE" "$GO_EVIDENCE"
else
    "$ROOT/scripts/build-tesla-command-proxy.sh" --output "$TEST_PROXY" >/dev/null
    python3 "$ROOT/scripts/go-proxy-evidence.py" --repo "$ROOT" \
        --proxy-binary "$TEST_PROXY" --output-dir "$GO_EVIDENCE" >/dev/null
fi
python3 "$ROOT/scripts/go-proxy-evidence.py" --repo "$ROOT" \
    --verify-dir "$GO_EVIDENCE" >/dev/null
cp "$TEST_PROXY" "$TEST_FLEET_TELEMETRY"
python3 "$ROOT/scripts/fleet-telemetry-evidence.py" --repo "$ROOT" \
    --receiver-binary "$TEST_FLEET_TELEMETRY" \
    --output-dir "$FLEET_TELEMETRY_EVIDENCE" >/dev/null
python3 "$ROOT/scripts/fleet-telemetry-evidence.py" --repo "$ROOT" \
    --verify-dir "$FLEET_TELEMETRY_EVIDENCE" >/dev/null
export RELEASE_TEST_PROXY="$TEST_PROXY"
export RELEASE_TEST_FLEET_TELEMETRY="$TEST_FLEET_TELEMETRY"

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
    *' -c '*) exec /usr/bin/ditto "$@" ;;
    *)
        cp -R "$source" "$destination"
        if [ "${RELEASE_MUTATE_COPIED_PROXY:-0}" = 1 ]; then
            printf '%s\n' changed >>"$destination/Contents/Resources/tesla-http-proxy"
        fi
        ;;
esac
EOF

cat >"$BIN/pkgutil" <<'EOF'
#!/bin/sh
printf 'pkgutil %s\n' "$*" >>"$RELEASE_TEST_LOG"
case "$1" in
    --expand-full)
        mkdir -p "$3/Payload/Library/Application Support/Teslatlas Hub/bin"
        mkdir -p "$3/Scripts"
        : >"$3/Payload/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
        cp "$RELEASE_TEST_PROXY" \
            "$3/Payload/Library/Application Support/Teslatlas Hub/bin/tesla-http-proxy"
        cp "$RELEASE_TEST_FLEET_TELEMETRY" \
            "$3/Payload/Library/Application Support/Teslatlas Hub/bin/fleet-telemetry"
        cat >"$3/PackageInfo" <<'PACKAGE_INFO'
<pkg-info identifier="com.teslatlas.hub.service" version="1.0.0a1" install-location="/"/>
PACKAGE_INFO
        ;;
    --check-signature) ;;
    *) exit 2 ;;
esac
EOF

cat >"$BIN/pkgbuild" <<'EOF'
#!/bin/sh
printf 'pkgbuild %s\n' "$*" >>"$RELEASE_TEST_LOG"
previous=
for arg in "$@"; do previous=$arg; done
: >"$previous"
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
        --go-proxy-evidence "$GO_EVIDENCE" \
        --fleet-telemetry-evidence "$FLEET_TELEMETRY_EVIDENCE" \
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

WRONG_GO_EVIDENCE="$INPUT/wrong-go-proxy-evidence"
cp -R "$GO_EVIDENCE" "$WRONG_GO_EVIDENCE"
python3 - "$WRONG_GO_EVIDENCE/go-component-manifest.json" <<'PY'
import json
from pathlib import Path
import sys

path = Path(sys.argv[1])
manifest = json.loads(path.read_text())
manifest["subject"]["sha256"] = "0" * 64
path.write_text(json.dumps(manifest, sort_keys=True) + "\n")
PY
original_go_evidence=$GO_EVIDENCE
GO_EVIDENCE=$WRONG_GO_EVIDENCE
wrong_go_output="$TMP/wrong-go-evidence"
if run_release "$PKG_ID" teslatlas-hub-notary-4AA2EMZ2HA \
    "$wrong_go_output" >"$TMP/wrong-go.out" 2>&1; then
    fail "Go evidence for a different proxy was accepted"
fi
GO_EVIDENCE=$original_go_evidence
grep -Fq 'subject does not match the locked proxy' "$TMP/wrong-go.out" \
    || fail "wrong Go evidence did not fail for the expected reason"
[ ! -e "$wrong_go_output" ] || fail "wrong Go evidence created output"
[ ! -s "$LOG" ] || fail "wrong Go evidence invoked signing or notary tools"

staged_swap_output="$TMP/staged-proxy-swap"
if RELEASE_MUTATE_COPIED_PROXY=1 run_release "$PKG_ID" \
    teslatlas-hub-notary-4AA2EMZ2HA "$staged_swap_output" \
    >"$TMP/staged-proxy-swap.out" 2>&1; then
    fail "proxy changed while copying the release app was accepted"
fi
unset RELEASE_MUTATE_COPIED_PROXY
grep -Fq 'app proxy changed while staging the release' "$TMP/staged-proxy-swap.out" \
    || fail "staged proxy replacement did not fail for the expected reason"
[ ! -e "$staged_swap_output" ] || fail "staged proxy replacement created output"
: >"$LOG"

good_output="$TMP/release"
run_release "$PKG_ID" teslatlas-hub-notary-4AA2EMZ2HA "$good_output" >/dev/null
[ -z "$(find "$TMP" -maxdepth 1 -name '.teslatlas-macos-release.*' -print -quit)" ] \
    || fail "successful release retained staging data"
[ ! -e "$good_output/Teslatlas Hub.app" ] || fail "redundant release app was retained"
[ -f "$good_output/TeslatlasHubService.pkg" ] || fail "release pkg is missing"
[ -f "$good_output/Teslatlas Hub.zip" ] || fail "final zip is missing"
[ -f "$good_output/SHA256SUMS" ] || fail "checksums are missing"
[ -f "$good_output/go-proxy-evidence/go-component-manifest.json" ] \
    || fail "Go proxy evidence is missing"
[ -f "$good_output/notary-logs/service-package-submit.json" ] \
    || fail "package submit log is missing"
[ -f "$good_output/notary-logs/service-package-log.json" ] \
    || fail "package detail log is missing"
[ -f "$good_output/notary-logs/app-submit.json" ] \
    || fail "app submit log is missing"
[ -f "$good_output/notary-logs/app-log.json" ] \
    || fail "app detail log is missing"
extracted_release="$TMP/extracted-release"
mkdir "$extracted_release"
/usr/bin/ditto -x -k "$good_output/Teslatlas Hub.zip" "$extracted_release"
release_info="$extracted_release/Teslatlas Hub.app/Contents/Info.plist"
embedded_package="$extracted_release/Teslatlas Hub.app/Contents/Resources/TeslatlasHubService.pkg"
embedded_digest=$(shasum -a 256 "$embedded_package" | awk '{ print $1 }')
[ "$(/usr/libexec/PlistBuddy -c 'Print :TeslatlasServicePackageSHA256' "$release_info")" = "$embedded_digest" ] \
    || fail "signed package digest was not bound into the app"
[ "$(/usr/libexec/PlistBuddy -c 'Print :TeslatlasReleaseTeamIdentifier' "$release_info")" = 4AA2EMZ2HA ] \
    || fail "release Team ID was not bound into the app"
[ "$(/usr/libexec/PlistBuddy -c 'Print :TeslatlasOfficialRelease' "$release_info")" = true ] \
    || fail "official release marker is missing"
expected_proxy_sha=$(shasum -a 256 "$INPUT/Teslatlas Hub.app/Contents/Resources/tesla-http-proxy" | awk '{ print $1 }')
[ "$(/usr/libexec/PlistBuddy -c 'Print :TeslatlasUnsignedProxySHA256' "$release_info")" = "$expected_proxy_sha" ] \
    || fail "unsigned proxy digest is not bound into the app"
expected_evidence_sha=$(shasum -a 256 "$good_output/go-proxy-evidence/go-component-manifest.json" | awk '{ print $1 }')
[ "$(/usr/libexec/PlistBuddy -c 'Print :TeslatlasGoEvidenceManifestSHA256' "$release_info")" = "$expected_evidence_sha" ] \
    || fail "Go evidence manifest is not bound into the app"
expected_fleet_telemetry_sha=$(shasum -a 256 "$INPUT/Teslatlas Hub.app/Contents/Resources/fleet-telemetry" | awk '{ print $1 }')
[ "$(/usr/libexec/PlistBuddy -c 'Print :TeslatlasUnsignedFleetTelemetrySHA256' "$release_info")" = "$expected_fleet_telemetry_sha" ] \
    || fail "unsigned Fleet Telemetry receiver digest is not bound into the app"
expected_fleet_telemetry_evidence_sha=$(shasum -a 256 "$good_output/fleet-telemetry-evidence/fleet-telemetry-component-manifest.json" | awk '{ print $1 }')
[ "$(/usr/libexec/PlistBuddy -c 'Print :TeslatlasFleetTelemetryEvidenceManifestSHA256' "$release_info")" = "$expected_fleet_telemetry_evidence_sha" ] \
    || fail "Fleet Telemetry evidence manifest is not bound into the app"

line_number() {
    grep -n "$1" "$LOG" | head -1 | cut -d: -f1
}

payload_sign=$(line_number 'codesign .*service-expanded/Payload.*teslatlas-hub')
package_rebuild=$(line_number '^pkgbuild ')
product_sign=$(line_number '^productsign ')
package_submit=$(line_number 'xcrun notarytool submit .*TeslatlasHubService.pkg')
package_staple=$(line_number 'xcrun stapler staple .*TeslatlasHubService.pkg')
app_nested_sign=$(line_number 'codesign .*release/Teslatlas Hub.app/Contents/Resources/teslatlas-hub')
app_sign=$(line_number 'codesign --force --sign .*release/Teslatlas Hub.app$')
app_submit=$(line_number 'xcrun notarytool submit .*submission.zip')
app_staple=$(line_number 'xcrun stapler staple .*Teslatlas Hub.app')

[ "$payload_sign" -lt "$package_rebuild" ] || fail "package rebuilt before payload signing"
[ "$package_rebuild" -lt "$product_sign" ] || fail "package was signed before ownership rebuild"
[ "$product_sign" -lt "$package_submit" ] || fail "package was notarized before signing"
[ "$package_submit" -lt "$package_staple" ] || fail "package staple order is wrong"
[ "$package_staple" -lt "$app_nested_sign" ] || fail "unstapled package entered app"
[ "$app_nested_sign" -lt "$app_sign" ] || fail "app was signed before nested code"
[ "$app_sign" -lt "$app_submit" ] || fail "app was notarized before signing"
[ "$app_submit" -lt "$app_staple" ] || fail "app staple order is wrong"

grep -Fq -- '--timestamp --options runtime' "$LOG" \
    || fail "hardened runtime or secure timestamp is missing"
grep -Fq -- 'pkgbuild --root ' "$LOG" \
    || fail "signed payload was not rebuilt as a component package"
grep -Fq -- '--ownership recommended' "$LOG" \
    || fail "signed package does not restore root-recommended ownership"
[ "$(grep -c 'codesign --force .*tesla-http-proxy' "$LOG")" -eq 2 ] \
    || fail "package and app proxy copies were not both signed"
[ "$(grep -c 'codesign --force .*fleet-telemetry' "$LOG")" -eq 2 ] \
    || fail "package and app Fleet Telemetry receiver copies were not both signed"
(
    cd "$good_output"
    shasum -a 256 -c SHA256SUMS >/dev/null
) || fail "checksums do not verify"

printf '%s\n' "macOS release script tests passed"
