# Debian arm64 Apple Virtualization bench

Native Apple Virtualization bench for this Mac. It does not use QEMU.

Profile:

- Debian arm64 guest
- 8 vCPU
- 8 GiB RAM
- disk image at least 50 GiB
- NAT networking
- local graphical installer window

Create the empty sparse disk and EFI store explicitly. This does not start a
VM and never overwrites an existing path:

```sh
./launch.sh \
  --iso /absolute/path/debian-arm64.iso \
  --disk /absolute/path/debian-arm64.img \
  --efi-vars /absolute/path/debian-arm64-efi-vars.bin \
  --create-disk --create-efi --validate
```

Validation only is then the default. It does not start a VM:

```sh
./launch.sh \
  --iso /absolute/path/debian-arm64.iso \
  --disk /absolute/path/debian-arm64.img \
  --efi-vars /absolute/path/debian-arm64-efi-vars.bin
```

Start is explicit and manual. It opens a local graphical installer window:

```sh
./launch.sh \
  --iso /absolute/path/debian-arm64.iso \
  --disk /absolute/path/debian-arm64.img \
  --efi-vars /absolute/path/debian-arm64-efi-vars.bin \
  --start
```

The launcher uses a Virtualization entitlement and the documented Virtio
graphics, USB input, USB mass-storage, NAT, and block-device configuration.
The launcher never contacts TeslaMate, the VPS, Docker, or any other service.
