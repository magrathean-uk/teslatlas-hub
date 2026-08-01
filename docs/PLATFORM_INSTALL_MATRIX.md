# Platform and install matrix v1

Release proof requires native evidence on every first platform. A green result
names artifact/revision, hardware/OS, filesystem/device, resource profile,
command, exact inputs/corpus, timestamps, outcome, and redacted artifacts. A
result on one architecture or filesystem does not stand in for another.

| Platform | Native delivery/supervision proof | Required paths |
| --- | --- | --- |
| Apple-silicon macOS | signed native arm64 artifact and macOS-native manual/supervised Hub ownership | clean install, protected credentials/state, APFS startup/reboot equivalent, TLS/pairing, import/collection refusal/explicit paths, upgrade/refusal/restore, removal preservation |
| Debian amd64 | native `amd64` package and hardened systemd units | clean install, explicit setup, service reboot/restart, credential injection, manual collector/import, upgrade/downgrade refusal/rollback, restore, removal/purge preservation |
| Debian arm64 | native `arm64` package and hardened systemd units in a local Apple Virtualization guest | same as amd64 in an 8-vCPU, 8-GiB Debian arm64 VM; Raspberry Pi-class proof is a later additional run |

Each platform runs a baseline host case meeting four CPU cores, 8 GiB RAM,
50 GiB free local SSD, supported local filesystem, and direct low-latency LAN
source path. It also runs a constrained small-host case. The baseline case must
prove the representative approximately ten-million-row migration target;
constrained/Pi-class hosts prove correctness, bounded pressure response,
truthful readiness, and recovery only unless they independently meet the
baseline envelope. The initial Debian arm64 baseline is local Apple
Virtualization on this Mac with 8 vCPUs and 8 GiB RAM. Debian amd64 native proof
is deferred until an x86 host is supplied.

Supported storage combinations are APFS on macOS and ext4/XFS on Debian with
durable sync/atomic rename semantics. The matrix includes negative admission
tests for network filesystems, FAT/exFAT/removable media, spinning storage,
active swapping, unsafe permissions/symlinks, insufficient free bytes/inodes,
and wrong architecture. Those outcomes must fail closed without source mutation.

For every platform/filesystem/resource cell, prove fresh install with and
without optional credentials, local-only serving, explicit TLS/pairing,
reboot/restart after durable data, clean backup/restore on a fresh host,
compatible upgrade, refused unsafe downgrade, recovery from interrupted work,
and package/app removal that preserves Hub state. The separate owner-authorized
live probe and operator cutover rehearsal are recorded once on an eligible
native host, never fabricated from a simulator.

Platform proof also runs the representative corpus, differential/fault suite,
package/artifact integrity checks, no-wake/request audit, and readiness within
the locked recovery objective. Any unsupported architecture, Intel macOS,
missing native evidence, unexplained corpus difference, failed restore, or
release-artifact mismatch blocks release.

## Evidence ledger

| UTC date | Platform | Artifact | Proven | Still open |
| --- | --- | --- | --- | --- |
| 2026-07-31 | Debian 13 arm64, QEMU/HVF, 8 vCPU, 7.75 GiB RAM, ext4 | `teslatlas-hub_0.1.0-dev11_arm64.deb` | native install; hardened service; encrypted credentials; TLS/pairing; 10.63M-row read-only import; live/manual and three-cycle supervised collection; full catalogue `doctor`; 438-pack online backup and fresh-root restore; reboot survival; removal/reinstall with identical catalogue hash, pack count, and credentials | constrained-host negatives; upgrade/downgrade; purge preservation; low-latency baseline |
| 2026-07-31 | Apple-silicon macOS, APFS, user LaunchAgents | locally ad-hoc-signed native arm64 binary | protected install; Keychain credentials; TLS/pairing; live/manual and three-cycle supervised collection; backup/fresh-root restore; launchd restart; data-preserving uninstall/reinstall | Developer ID signing/notarization; distributable package; full corpus; upgrade/downgrade; constrained-host negatives |

Debian amd64 and Apple-silicon macOS remain release-blocking cells.
