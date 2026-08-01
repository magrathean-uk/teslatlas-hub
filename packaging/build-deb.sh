#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
umask 022

readonly PROGRAM_NAME="${0##*/}"
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
readonly PROJECT_ROOT

usage() {
  cat <<'EOF'
Usage:
  packaging/build-deb.sh --version VERSION [options]

Options:
  --binary PATH   Hub executable. Default: target/release/teslatlas-hub
  --arch ARCH     Debian architecture (amd64 or arm64). Default: host dpkg arch
  --out DIR       Output directory. Default: dist
  --help          Show this text

The resulting package contains no credentials and never starts the service.
EOF
}

die() {
  printf '%s: %s\n' "$PROGRAM_NAME" "$*" >&2
  exit 1
}

version=""
binary="$PROJECT_ROOT/target/release/teslatlas-hub"
arch=""
out_dir="$PROJECT_ROOT/dist"

while (($#)); do
  case "$1" in
    --version)
      (($# >= 2)) || die "--version requires a value"
      version="$2"
      shift 2
      ;;
    --binary)
      (($# >= 2)) || die "--binary requires a path"
      binary="$2"
      shift 2
      ;;
    --arch)
      (($# >= 2)) || die "--arch requires a value"
      arch="$2"
      shift 2
      ;;
    --out)
      (($# >= 2)) || die "--out requires a path"
      out_dir="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

[[ -n "$version" ]] || die "--version is required"
command -v dpkg-deb >/dev/null 2>&1 || die "dpkg-deb is required"
command -v dpkg >/dev/null 2>&1 || die "dpkg is required"
dpkg --validate-version "$version" >/dev/null 2>&1 || die "invalid Debian version: $version"

if [[ -z "$arch" ]]; then
  arch="$(dpkg --print-architecture)"
fi
case "$arch" in
  amd64|arm64) ;;
  *) die "unsupported architecture: $arch (supported: amd64, arm64)" ;;
esac

[[ -f "$binary" && -x "$binary" ]] || die "executable not found: $binary"
command -v readelf >/dev/null 2>&1 || die "readelf is required to verify binary architecture"
binary_machine="$(LC_ALL=C readelf -h "$binary" 2>/dev/null | awk -F: '/Machine:/ { gsub(/^[[:space:]]+/, "", $2); print $2; exit }')"
case "$arch" in
  amd64) expected_machine="Advanced Micro Devices X86-64" ;;
  arm64) expected_machine="AArch64" ;;
esac
[[ "$binary_machine" == "$expected_machine" ]] || \
  die "binary architecture ${binary_machine:-unknown} does not match package architecture $arch"

stage_dir="$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-hub-deb.XXXXXX")"
cleanup() {
  rm -rf -- "$stage_dir"
}
trap cleanup EXIT HUP INT TERM

package_dir="$stage_dir/teslatlas-hub_${version}_${arch}"
control_dir="$package_dir/DEBIAN"
mkdir -p "$control_dir" "$package_dir/usr/bin" \
  "$package_dir/etc/teslatlas" "$package_dir/usr/lib/systemd/system" \
  "$package_dir/usr/share/doc/teslatlas-hub" "$package_dir/usr/share/teslatlas-hub"

sed -e "s/@VERSION@/$version/g" -e "s/@ARCH@/$arch/g" \
  "$PROJECT_ROOT/packaging/debian/control.in" > "$control_dir/control"
install -m 0755 "$PROJECT_ROOT/packaging/debian/postinst" "$control_dir/postinst"
install -m 0755 "$PROJECT_ROOT/packaging/debian/postrm" "$control_dir/postrm"
install -m 0644 "$PROJECT_ROOT/packaging/debian/conffiles" "$control_dir/conffiles"
install -m 0755 "$binary" "$package_dir/usr/bin/teslatlas-hub"
install -m 0755 "$PROJECT_ROOT/scripts/verify-native-install.sh" \
  "$package_dir/usr/bin/teslatlas-hub-verify"
install -m 0755 "$PROJECT_ROOT/scripts/setup.sh" \
  "$package_dir/usr/bin/teslatlas-hub-setup"
install -m 0755 "$PROJECT_ROOT/scripts/teslamate-cutover.sh" \
  "$package_dir/usr/bin/teslatlas-hub-cutover"
install -m 0644 "$PROJECT_ROOT/packaging/teslatlas-hub.service" \
  "$package_dir/usr/lib/systemd/system/teslatlas-hub.service"
install -m 0644 "$PROJECT_ROOT/packaging/teslatlas-hub-collect.service" \
  "$package_dir/usr/lib/systemd/system/teslatlas-hub-collect.service"
install -m 0644 "$PROJECT_ROOT/packaging/teslatlas-hub-supervised.service" \
  "$package_dir/usr/lib/systemd/system/teslatlas-hub-supervised.service"
install -m 0644 "$PROJECT_ROOT/packaging/teslatlas-hub-import@.service" \
  "$package_dir/usr/lib/systemd/system/teslatlas-hub-import@.service"
install -m 0644 "$PROJECT_ROOT/packaging/teslatlas-hub-token-import.service" \
  "$package_dir/usr/lib/systemd/system/teslatlas-hub-token-import.service"
install -m 0755 "$PROJECT_ROOT/scripts/import-teslamate-legacy-token.sh" \
  "$package_dir/usr/bin/teslatlas-hub-import-teslamate-token"
install -m 0755 "$PROJECT_ROOT/scripts/import-teslamate-fleet-proxy.sh" \
  "$package_dir/usr/bin/teslatlas-hub-import-teslamate-fleet-proxy"
install -m 0755 "$PROJECT_ROOT/scripts/import-teslamate-rpc.sh" \
  "$package_dir/usr/bin/teslatlas-hub-import-teslamate-rpc"
install -m 0755 "$PROJECT_ROOT/scripts/test-systemd-credential-activation.sh" \
  "$package_dir/usr/share/teslatlas-hub/test-systemd-credential-activation.sh"
install -m 0644 "$PROJECT_ROOT/packaging/config.toml" \
  "$package_dir/etc/teslatlas/config.toml"
install -m 0644 "$PROJECT_ROOT/packaging/debian/copyright" \
  "$package_dir/usr/share/doc/teslatlas-hub/copyright"

mkdir -p "$out_dir"
artifact="$out_dir/teslatlas-hub_${version}_${arch}.deb"
dpkg-deb --root-owner-group --build "$package_dir" "$artifact" >/dev/null

dpkg-deb --info "$artifact" >/dev/null
dpkg-deb --contents "$artifact" >/dev/null
printf '%s\n' "$artifact"
