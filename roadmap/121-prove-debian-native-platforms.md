---
type: wayfinder:task
status: in_progress
parent: 000-map
mode: AFK
---
# Prove Debian native platforms

Blocked by: [Verify backup path locally](120-verify-backup-path-locally.md).

## Question

Which fixed native environments run the required package, restore, and corpus
proof without touching a TeslaMate host?

## Starting recommendation

Use a Hub-owned local Apple Virtualization Debian arm64 guest on this Mac with
8 vCPUs and 8 GiB RAM. Build/install the native arm64 package there and run the
representative corpus, restore, and fault proof. Do not touch TeslaMate hosts
or containers.

## Resolution

Debian arm64 proof uses the local headless QEMU/HVF cloud guest: 8 vCPUs,
8 GiB RAM, native arm64 package, restore, and corpus evidence. Debian amd64
proof is deferred until the user provides an x86 host.

## Current evidence

On 2026-07-31 the Debian 13 arm64 guest ran kernel
`6.12.96+deb13-cloud-arm64` on ext4 with 8 vCPUs, 7.75 GiB visible RAM, and
56.7 GB free after evidence retention. Native package `0.1.0-dev10` installed
and survived reboot with the hardened service active, encrypted credentials
available, `teslatlas-hub-verify` passing, and `doctor` passing.

The read-only production-shaped TeslaMate source contained 10,385,745
positions. Hub published 10,631,740 projected rows in 9m53.548s over an
SSH/WireGuard WAN path, using 58.690s CPU and 186.8 MB peak memory. The result
contained 436 initial verified packs; subsequent live collections increased
the catalogue to 438 packs. A consistent online backup of all 438 referenced
packs restored into a fresh private root, where `doctor` rehashed the complete
catalogue successfully. After guest reboot, one online owner snapshot was
received and durably inserted with zero vehicle failures.

This closes the Debian arm64 install, reboot, credential, import, live
collection, backup, restore, and integrity cells. It does not close the
low-latency baseline benchmark, constrained-host negatives, purge preservation,
or Debian amd64 cells. Package removal/reinstall preserved the exact catalogue
hash, all 438 packs, configuration, encrypted credentials, and readiness.
