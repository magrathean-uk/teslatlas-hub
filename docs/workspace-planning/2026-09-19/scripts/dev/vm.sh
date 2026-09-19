#!/bin/bash
set -euo pipefail
dev_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$dev_script_dir/env.sh"
export LIMA_HOME="$TESLATLAS_LAB/vms/lima"
dev_tart="$TESLATLAS_LAB/tooling/tart-2.36.0/tart.app/Contents/MacOS/tart"
dev_tart_state="$TESLATLAS_LAB/vms/tart"
dev_ssh_config="$TESLATLAS_LAB/access/ssh_config"
case "${1:-status}:${2:-}" in
  status:)
    limactl list
    if [[ -d "$dev_tart_state" ]]; then
      "$dev_tart" list
    else
      echo "Tart state absent; skipping Tart inventory: $dev_tart_state"
    fi
    ;;
  debian:start) exec limactl start --tty=false debian13-arm64 ;;
  debian:stop) exec limactl stop debian13-arm64 ;;
  debian:shell) shift 2; exec limactl shell debian13-arm64 -- "$@" ;;
  debian:ssh) shift 2; exec ssh -F "$dev_ssh_config" teslatlas-debian13-arm64 "$@" ;;
  debian:ip) echo '127.0.0.1 (SSH port 60022)' ;;
  mac:start) exec "$dev_tart" run macos13-arm64 --no-graphics --no-audio --no-clipboard ;;
  mac:stop) exec "$dev_tart" stop macos13-arm64 ;;
  mac:shell) shift 2; exec "$dev_tart" exec macos13-arm64 "$@" ;;
  mac:ssh)
    shift 2
    dev_mac_ip="$("$dev_tart" ip macos13-arm64)"
    exec ssh -F "$dev_ssh_config" -o "HostName=$dev_mac_ip" teslatlas-macos13-arm64 "$@"
    ;;
  mac:ip) exec "$dev_tart" ip macos13-arm64 ;;
  *) echo "Usage: vm.sh [status | debian|mac start|stop|ip|shell|ssh [COMMAND ...]]" >&2; exit 2 ;;
esac
