#!/bin/zsh
set -euo pipefail

script_dir="${0:A:h}"
vm_dir="${TESLATLAS_VM_DIR:-/Users/bolyki/VMs/teslatlas-hub}"
ssh_port="${TESLATLAS_VM_SSH_PORT:-2223}"
base_image="$vm_dir/debian-13-genericcloud-arm64.qcow2"
disk_image="$vm_dir/teslatlas-hub-arm64.qcow2"
seed_dir="$vm_dir/cloud-init"
seed_image="$vm_dir/seed.iso"
private_key="$vm_dir/id_ed25519"
public_key="$private_key.pub"
pid_file="$vm_dir/qemu.pid"
serial_log="$vm_dir/serial.log"
firmware="/opt/homebrew/share/qemu/edk2-aarch64-code.fd"
image_url="https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-arm64.qcow2"

for tool in curl hdiutil qemu-img qemu-system-aarch64 sed ssh-keygen; do
  command -v "$tool" >/dev/null || {
    print -u2 "missing required tool: $tool"
    exit 1
  }
done
[[ -f "$firmware" ]] || {
  print -u2 "missing QEMU ARM64 firmware: $firmware"
  exit 1
}

mkdir -p "$vm_dir" "$seed_dir"
seed_build_dir="$(mktemp -d "$vm_dir/.seed-build.XXXXXX")"
trap 'rm -rf "$seed_build_dir"' EXIT
if [[ ! -f "$private_key" ]]; then
  ssh-keygen -q -t ed25519 -N "" -C teslatlas-hub-arm64 -f "$private_key"
fi
chmod 0600 "$private_key"

if [[ ! -f "$base_image" ]]; then
  curl -fL --retry 3 -o "$base_image.part" "$image_url"
  mv "$base_image.part" "$base_image"
fi
if [[ ! -f "$disk_image" ]]; then
  qemu-img create -q -f qcow2 -F qcow2 -b "$base_image" "$disk_image" 64G
fi

public_key_text="$(<"$public_key")"
sed "s|__SSH_PUBLIC_KEY__|$public_key_text|" \
  "$script_dir/user-data.in" >"$seed_dir/user-data"
install -m 0644 "$script_dir/meta-data" "$seed_dir/meta-data"
hdiutil makehybrid -quiet -iso -joliet -default-volume-name cidata \
  -o "$seed_build_dir/seed.iso" "$seed_dir"
mv "$seed_build_dir/seed.iso" "$seed_image"

if [[ -f "$pid_file" ]] && kill -0 "$(<"$pid_file")" 2>/dev/null; then
  print "VM already running: PID $(<"$pid_file")"
  exit 0
fi

qemu-system-aarch64 \
  -machine virt,highmem=on \
  -accel hvf \
  -cpu host \
  -smp 8 \
  -m 8192 \
  -bios "$firmware" \
  -drive "if=virtio,file=$disk_image,format=qcow2,discard=unmap" \
  -drive "if=virtio,file=$seed_image,format=raw,readonly=on" \
  -device virtio-rng-pci \
  -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:$ssh_port-:22" \
  -device virtio-net-pci,netdev=net0 \
  -display none \
  -serial "file:$serial_log" \
  -daemonize \
  -pidfile "$pid_file"

print "VM started. SSH: ssh -i $private_key -p $ssh_port teslatlas@127.0.0.1"
