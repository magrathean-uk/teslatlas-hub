# Debian arm64 completion

## Current position

The current test host is a generic cloud-init Debian 13 ARM64 VM, headless over
SSH, running under QEMU/HVF on Apple silicon with 8 vCPUs and 8 GiB configured
memory. Native installation of `0.1.0-dev78` succeeded.

The production credentialed `teslatlas-hub-collect.service` completed with
receipt correlation `3478e1ab-e125-4734-b43e-c7ab3289ccdc`. It saw one vehicle,
zero online vehicles, zero snapshots, zero observations, and zero failures.
Against audit watermark `2`, `verify-no-wake` returned `verified: true`, with
three matching receipts, zero direct-wake receipts, zero unresolved requests,
and zero unresolved stream sessions.

Encrypted credential state metadata is stored under
`/var/lib/teslatlas/legacy-auth` with mode `600`. This records metadata only;
token contents are not included.

Physical awake and 60-second persistence observation proof remains pending, as
does macOS runtime proof. The current receipt does not prove online collection,
streaming, or durable vehicle observations.

## Small completion steps

| ID | Action | Gate |
| --- | --- | --- |
| D01 | Save dev78 VM profile, package identity, service receipt, no-wake audit, and encrypted-state metadata. | Current Debian ARM runtime slice recorded; physical and remaining platform gates stay open. |
| D02 | Start one explicit collection against the offline vehicle. | Redacted no-wake discovery receipt and truthful last-observation state. |
| D03 | After owner manual wake, wait 60 seconds and collect. | New durable observation, pack/catalogue result, and no-wake request audit. |
| D04 | Exercise driving, charging, sleep, offline, update, and resume traces with fixtures and live data where available. | Differential and freshness trace per state. |
| D05 | Repeat encrypted credential, TLS/pairing, restart, reboot, backup, and restore on dev72. | Native current artifact evidence. |
| D06 | Run all-car/open-session migration and replay cases. | Per-car report, reconciliation, and idempotence proof. |
| D07 | Run representative corpus on a direct low-latency source path. | Under-ten-minute qualifying timing or a failed performance record. |
| D08 | Run upgrade, unsafe downgrade refusal, remove/reinstall preservation, and interrupted package paths. | State and credential preservation report. |
| D09 | Run low-space, low-inode, bad-storage, source-loss, and corrupt-pack cases. | Fail-closed admission and recovery evidence. |

## Architecture note

This VM provides Debian arm64 guest proof. Its cloud-init and SSH headless
setup is intentional and reproducible. It does not replace a separate Debian
amd64 native proof once the user supplies x86 hardware.

See [platform matrix](../PLATFORM_INSTALL_MATRIX.md) and
[performance baseline](../PERFORMANCE_BASELINE.md).
