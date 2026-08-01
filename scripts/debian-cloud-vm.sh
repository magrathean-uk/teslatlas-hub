#!/bin/bash
set -euo pipefail

VM_DIR="${TESLATLAS_VM_DIR:-$HOME/VMs/teslatlas-hub-cloud}"
SSH_PORT="${TESLATLAS_VM_SSH_PORT:-2224}"
NETWORK_MODE="${TESLATLAS_VM_NETWORK:-vmnet-shared}"
VM_MAC="${TESLATLAS_VM_MAC:-52:54:00:74:48:01}"
VMNET_GATEWAY="${TESLATLAS_VMNET_GATEWAY:-192.168.76.1}"
VMNET_GUEST_IP="${TESLATLAS_VMNET_GUEST_IP:-192.168.76.10}"
VMNET_DHCP_END="${TESLATLAS_VMNET_DHCP_END:-192.168.76.254}"
VMNET_NETMASK="${TESLATLAS_VMNET_NETMASK:-255.255.255.0}"
CPUS="${TESLATLAS_VM_CPUS:-8}"
MEMORY_MIB="${TESLATLAS_VM_MEMORY_MIB:-8192}"
DISK_SIZE="${TESLATLAS_VM_DISK_SIZE:-80G}"
IMAGE_URL="${TESLATLAS_VM_IMAGE_URL:-https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-arm64.qcow2}"
IMAGE_NAME="${IMAGE_URL##*/}"
BASE_IMAGE="$VM_DIR/$IMAGE_NAME"
DISK_IMAGE="$VM_DIR/disk.qcow2"
SEED_IMAGE="$VM_DIR/seed.iso"
KEY_FILE="$VM_DIR/id_ed25519"
KNOWN_HOSTS="$VM_DIR/known_hosts"
PID_FILE="$VM_DIR/qemu.pid"
CONSOLE_LOG="$VM_DIR/console.log"
NETWORK_STATE_FILE="$VM_DIR/network-mode"
QEMU_SHARE="${TESLATLAS_QEMU_SHARE:-}"
FIRMWARE=""

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require() {
  command -v "$1" >/dev/null 2>&1 || die "missing command: $1"
}

ssh_options=(
  -o BatchMode=yes
  -o ConnectTimeout=5
  -o StrictHostKeyChecking=accept-new
  -o "UserKnownHostsFile=$KNOWN_HOSTS"
  -i "$KEY_FILE"
)
ssh_agent_options=()
if [[ "${TESLATLAS_VM_FORWARD_AGENT:-0}" == "1" ]]; then
  ssh_agent_options=(-o ForwardAgent=yes)
fi

validate_network() {
  case "$NETWORK_MODE" in
    user | vmnet-shared) ;;
    *) die "unsupported TESLATLAS_VM_NETWORK: $NETWORK_MODE" ;;
  esac
  [[ "$VM_MAC" =~ ^([[:xdigit:]]{2}:){5}[[:xdigit:]]{2}$ ]] ||
    die "TESLATLAS_VM_MAC must be a MAC address"
}

ensure_network_state() {
  if [[ -f "$NETWORK_STATE_FILE" ]]; then
    [[ "$(cat "$NETWORK_STATE_FILE")" == "$NETWORK_MODE" ]] ||
      die "VM network mode differs from $NETWORK_STATE_FILE; use a new TESLATLAS_VM_DIR or rebuild the VM"
    return
  fi
  if [[ -e "$DISK_IMAGE" && "$NETWORK_MODE" != "user" ]]; then
    die "existing VM may use user networking; use a new TESLATLAS_VM_DIR for vmnet-shared"
  fi
  printf '%s\n' "$NETWORK_MODE" >"$NETWORK_STATE_FILE"
}

vm_ssh_host() {
  case "$NETWORK_MODE" in
    user) printf '127.0.0.1' ;;
    vmnet-shared) printf '%s' "$VMNET_GUEST_IP" ;;
  esac
}

vm_ssh_port() {
  case "$NETWORK_MODE" in
    user) printf '%s' "$SSH_PORT" ;;
    vmnet-shared) printf '22' ;;
  esac
}

vm_ssh() {
  local target="teslatlas@$(vm_ssh_host)"
  if ((${#ssh_agent_options[@]})); then
    ssh "${ssh_options[@]}" "${ssh_agent_options[@]}" -p "$(vm_ssh_port)" "$target" "$@"
  else
    ssh "${ssh_options[@]}" -p "$(vm_ssh_port)" "$target" "$@"
  fi
}

running() {
  [[ -s "$PID_FILE" ]] && ps -p "$(cat "$PID_FILE")" >/dev/null 2>&1
}

download_image() {
  if [[ ! -s "$BASE_IMAGE" ]]; then
    rm -f "$BASE_IMAGE.part"
    curl --fail --location --retry 3 --output "$BASE_IMAGE.part" "$IMAGE_URL"
    mv "$BASE_IMAGE.part" "$BASE_IMAGE"
  fi

  local sums_url="${IMAGE_URL%/*}/SHA512SUMS"
  local expected actual
  expected="$(
    curl --fail --location --retry 3 "$sums_url" |
      awk -v image="$IMAGE_NAME" '$2 == image || $2 == "*" image {print $1; exit}'
  )"
  [[ -n "$expected" ]] || die "image checksum not found"
  actual="$(shasum -a 512 "$BASE_IMAGE" | awk '{print $1}')"
  [[ "$actual" == "$expected" ]] || die "cloud image checksum mismatch"
}

create_seed() {
  local seed_dir public_key
  seed_dir="$(mktemp -d "$VM_DIR/seed.XXXXXX")"
  trap 'rm -rf "$seed_dir"' RETURN
  public_key="$(cat "$KEY_FILE.pub")"

  cat >"$seed_dir/meta-data" <<EOF
instance-id: teslatlas-hub-arm64
local-hostname: teslatlas-hub
EOF
  cat >"$seed_dir/user-data" <<EOF
#cloud-config
users:
  - default
  - name: teslatlas
    gecos: Teslatlas Hub
    groups: [adm, sudo]
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    lock_passwd: true
    ssh_authorized_keys:
      - $public_key
disable_root: true
ssh_pwauth: false
package_update: true
packages:
  - ca-certificates
  - curl
  - jq
  - openssh-server
  - postgresql-client
  - sqlite3
  - xz-utils
runcmd:
  - systemctl enable --now ssh
  - install -d -m 0755 /var/lib/teslatlas-cloud
  - touch /var/lib/teslatlas-cloud/ready
EOF
  if [[ "$NETWORK_MODE" == "vmnet-shared" ]]; then
    cat >"$seed_dir/network-config" <<EOF
version: 2
ethernets:
  teslatlas:
    match:
      macaddress: "$VM_MAC"
    set-name: eth0
    dhcp4: false
    addresses:
      - "$VMNET_GUEST_IP/24"
    routes:
      - to: default
        via: "$VMNET_GATEWAY"
    nameservers:
      addresses: ["$VMNET_GATEWAY", "1.1.1.1"]
EOF
  fi
  local seed_part="$SEED_IMAGE.part.$$"
  rm -f "$seed_part"
  rm -f "$seed_part.iso"
  hdiutil makehybrid "$seed_dir" -iso -joliet \
    -default-volume-name cidata -o "$seed_part" >/dev/null
  local generated_seed="$seed_part"
  if [[ ! -f "$generated_seed" && -f "$seed_part.iso" ]]; then
    generated_seed="$seed_part.iso"
  fi
  [[ -f "$generated_seed" ]] || die "hdiutil did not create the seed image"
  mv -f "$generated_seed" "$SEED_IMAGE"
  trap - RETURN
  rm -rf "$seed_dir"
}

resolve_firmware() {
  local qemu_path
  if [[ -z "$QEMU_SHARE" ]]; then
    qemu_path="$(command -v qemu-system-aarch64)" || die "QEMU ARM64 binary not found"
    QEMU_SHARE="$(cd "$(dirname "$qemu_path")/../share/qemu" && pwd -P)" ||
      die "QEMU ARM64 share directory not found"
  fi
  FIRMWARE="$QEMU_SHARE/edk2-aarch64-code.fd"
}

create_vm() {
  require curl
  require hdiutil
  require qemu-img
  require qemu-system-aarch64
  require shasum
  require ssh-keygen
  validate_network
  [[ "$(uname -m)" == "arm64" ]] || die "this VM definition requires Apple silicon"
  resolve_firmware
  [[ -r "$FIRMWARE" ]] || die "QEMU ARM64 firmware not found"
  mkdir -p "$VM_DIR"
  chmod 0700 "$VM_DIR"
  ensure_network_state
  if [[ ! -s "$KEY_FILE" ]]; then
    ssh-keygen -q -t ed25519 -N '' -C teslatlas-hub-vm -f "$KEY_FILE"
  fi
  download_image
  if [[ ! -s "$DISK_IMAGE" ]]; then
    qemu-img create -q -f qcow2 -F qcow2 -b "$BASE_IMAGE" "$DISK_IMAGE" "$DISK_SIZE"
  fi
  create_seed
  : >"$KNOWN_HOSTS"
  chmod 0600 "$KNOWN_HOSTS"
  printf 'created %s\n' "$VM_DIR"
}

start_vm() {
  require qemu-system-aarch64
  validate_network
  ensure_network_state
  [[ -s "$DISK_IMAGE" && -s "$SEED_IMAGE" ]] || die "run create first"
  resolve_firmware
  [[ -r "$FIRMWARE" ]] || die "QEMU ARM64 firmware not found"
  if running; then
    printf 'already running\n'
    return
  fi
  rm -f "$PID_FILE"
  local -a device_args network_args qemu_command
  case "$NETWORK_MODE" in
    user)
      device_args=(-device virtio-net-pci,netdev=net0)
      network_args=(-netdev "user,id=net0,hostfwd=tcp:127.0.0.1:$SSH_PORT-:22")
      ;;
    vmnet-shared)
      device_args=(-device "virtio-net-pci,netdev=net0,mac=$VM_MAC")
      network_args=(-netdev "vmnet-shared,id=net0,start-address=$VMNET_GATEWAY,end-address=$VMNET_DHCP_END,subnet-mask=$VMNET_NETMASK,isolated=on")
      ;;
  esac
  qemu_command=(qemu-system-aarch64)
  if [[ "$NETWORK_MODE" == "vmnet-shared" ]]; then
    qemu_command=(sudo qemu-system-aarch64)
  fi
  "${qemu_command[@]}" \
    -name teslatlas-hub-cloud \
    -machine virt,accel=hvf,highmem=on \
    -cpu host \
    -smp "$CPUS" \
    -m "$MEMORY_MIB" \
    -bios "$FIRMWARE" \
    -drive "if=virtio,format=qcow2,file=$DISK_IMAGE,discard=unmap" \
    -drive "if=virtio,format=raw,file=$SEED_IMAGE,readonly=on" \
    "${device_args[@]}" \
    "${network_args[@]}" \
    -device virtio-rng-pci \
    -display none \
    -serial "file:$CONSOLE_LOG" \
    -monitor none \
    -daemonize \
    -pidfile "$PID_FILE"
  if [[ "$NETWORK_MODE" == "vmnet-shared" ]]; then
    sudo chown "$(id -u):$(id -g)" "$PID_FILE" "$CONSOLE_LOG" 2>/dev/null || true
  fi
  printf 'started pid=%s network=%s ssh=%s:%s\n' \
    "$(cat "$PID_FILE")" "$NETWORK_MODE" "$(vm_ssh_host)" "$(vm_ssh_port)"
}

wait_vm() {
  local deadline=$((SECONDS + 300))
  while ((SECONDS < deadline)); do
    if vm_ssh \
      'set -eu
       cloud-init status --wait >/dev/null
       test -e /var/lib/teslatlas-cloud/ready
       test "$(uname -m)" = aarch64
       test "$(dpkg --print-architecture)" = arm64
      systemctl is-active --quiet ssh' \
      >/dev/null 2>&1; then
      printf 'ready arch=arm64 ssh=%s:%s cloud-init=done\n' "$(vm_ssh_host)" "$(vm_ssh_port)"
      return
    fi
    sleep 2
  done
  die "VM did not become SSH/cloud-init ready; inspect $CONSOLE_LOG"
}

stop_vm() {
  if ! running; then
    rm -f "$PID_FILE"
    printf 'not running\n'
    return
  fi
  vm_ssh 'sudo poweroff' >/dev/null 2>&1 || true
  local deadline=$((SECONDS + 60))
  while running && ((SECONDS < deadline)); do
    sleep 1
  done
  if running; then
    if [[ "$NETWORK_MODE" == "vmnet-shared" ]]; then
      sudo kill "$(cat "$PID_FILE")"
    else
      kill "$(cat "$PID_FILE")"
    fi
  fi
  rm -f "$PID_FILE"
  printf 'stopped\n'
}

sync_source() {
  local project_root
  project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  (
    cd "$project_root"
    COPYFILE_DISABLE=1 tar --format=ustar --no-xattrs --no-mac-metadata -czf - \
      Cargo.toml Cargo.lock src vendor packaging scripts fixtures tests
  ) | vm_ssh \
    'mkdir -p ~/teslatlas-hub &&
     rm -rf ~/teslatlas-hub/src ~/teslatlas-hub/vendor ~/teslatlas-hub/packaging ~/teslatlas-hub/scripts ~/teslatlas-hub/fixtures ~/teslatlas-hub/tests &&
     rm -f ~/teslatlas-hub/Cargo.toml ~/teslatlas-hub/Cargo.lock &&
     tar -xzf - -C ~/teslatlas-hub'
  printf 'source synced\n'
}

build_install() {
  local version="${1:-0.1.0-cloud}"
  sync_source
  vm_ssh bash -s -- "$version" <<'REMOTE'
set -euo pipefail
version="$1"
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
  build-essential clang curl libssl-dev pkg-config >/dev/null
if [[ ! -x "$HOME/.cargo/bin/rustc" ]] ||
   [[ "$("$HOME/.cargo/bin/rustc" --version | awk '{print $2}')" != "1.97.0" ]]; then
  curl --proto '=https' --tlsv1.2 -fsS https://sh.rustup.rs -o /tmp/rustup-init.sh
  RUSTUP_INIT_SKIP_PATH_CHECK=yes sh /tmp/rustup-init.sh \
    -y --profile minimal --default-toolchain 1.97.0 >/dev/null
  rm -f /tmp/rustup-init.sh
fi
. "$HOME/.cargo/env"
cd "$HOME/teslatlas-hub"
cargo build --release --locked
packaging/build-deb.sh --version "$version" >/tmp/teslatlas-package-path
package_path="$(cat /tmp/teslatlas-package-path)"
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y "$package_path" >/dev/null
if [[ ! -f /etc/teslatlas/credentials/cursor-key ]]; then
  sudo teslatlas-hub-setup --no-start >/tmp/teslatlas-setup.out 2>/tmp/teslatlas-setup.err
  sudo rm -f /tmp/teslatlas-setup.out /tmp/teslatlas-setup.err
fi
sudo systemctl enable --now teslatlas-hub.service
sudo "$HOME/teslatlas-hub/scripts/verify-native-install.sh" --allow-supervised-paused
dpkg-query -W teslatlas-hub
REMOTE
}

case "${1:-}" in
  create)
    create_vm
    ;;
  start)
    start_vm
    ;;
  wait)
    wait_vm
    ;;
  up)
    create_vm
    start_vm
    wait_vm
    ;;
  ssh)
    shift
    vm_ssh "$@"
    ;;
  stop)
    stop_vm
    ;;
  sync)
    sync_source
    ;;
  build-install)
    shift
    build_install "${1:-}"
    ;;
  status)
    if running; then
      printf 'running pid=%s network=%s ssh=%s:%s\n' \
        "$(cat "$PID_FILE")" "$NETWORK_MODE" "$(vm_ssh_host)" "$(vm_ssh_port)"
    else
      printf 'stopped\n'
    fi
    ;;
  *)
    printf 'usage: %s {create|start|wait|up|ssh [command...]|stop|status|sync|build-install [version]}\n' "$0" >&2
    exit 2
    ;;
esac
