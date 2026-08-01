# Current status

## Short truth

Hub is a working, advancing backend. Core delta sync and import publication
paths are implemented and locally verified. It is not yet a complete,
live-proven TeslaMate replacement. No final signoff exists and this is not a
100% claim.

## Proven or directly observed

| Area | State | Evidence and limit |
| --- | --- | --- |
| Reference | Proven | TeslaMate `4.1.0-dev`, pinned commit `7054517c10475f39f480edeae8f90c6f717985a3`; see [mapping](../TESLAMATE_MIGRATION_MAPPING.md). |
| Read-only source stance | Proven design and implementation boundary | Hub scripts and import paths preserve source read-only behavior. Final native negative-request audit remains required. |
| Debian ARM runtime | Verified bounded runtime slice | Debian 13 ARM64 cloud-init/headless VM, 8 vCPUs and 8 GiB, with `0.1.0-dev78` installed. The production credentialed `teslatlas-hub-collect.service` completed with correlation `3478e1ab-e125-4734-b43e-c7ab3289ccdc`; one vehicle was seen, with zero online vehicles, snapshots, observations, and failures. |
| Durability path | Local and native-arm evidence | Pack verification, 438-pack backup, fresh-root restore, reboot, and preservation reinstall were recorded for the earlier Debian arm64 run. |
| Collector lifecycle | Local and fixture evidence | Lifecycle, state, drive, charge, update, sleep, stream-health, and no-wake seams have executable Rust coverage. Physical online/drive/charge proof remains open. |
| Import staging, race, and outbox | Implemented locally | Multi-car staging, open-session second-snapshot reconciliation, atomic failure cleanup, and durable outbox publication are covered by the focused import/direct/db/collector/pack suites. Final disposable rehearsal remains open. |
| Delta v2 Hub backend/catalog/server | Completed locally | Hub catalog and pack publication are wired for immutable base plus ordered deltas. `RUSTFLAGS='-D warnings' cargo check --release --all-targets` in `teslatlas-hub` passed with zero warnings. |
| Delta v2 Teslatlas client/apply/transport | Completed local cross-repo gate | `teslatlas-core/tests/hub_delta_e2e.rs` passed: real in-process Hub server, native Rust HTTP adapter, base plus delta apply/activation, second sync downloads only the later delta, and no-capability request receives unchanged v1. |
| Debian cloud VM | Directly observed | Generic cloud-init Debian 13 ARM64 VM is running headless by SSH under the 8-vCPU/8-GiB profile. Native install of `0.1.0-dev78` succeeded. |
| No-wake audit | Verified bounded technical proof | Against audit watermark `2`, `verify-no-wake` returned `verified: true`: three matching receipts, zero direct-wake receipts, zero unresolved requests, and zero unresolved stream sessions. |
| Rotated credential state | Metadata verified | Encrypted state metadata is under `/var/lib/teslatlas/legacy-auth` with mode `600`. Token contents are not recorded here. |
| Real vehicle state | Live, bounded | Real offline discovery occurred. It does not prove online, driving, charging, stream, or one-minute collection behavior. |

## Exact latest local gates

```text
RUSTFLAGS='-D warnings' cargo check --release --all-targets
RUSTFLAGS='-D warnings' cargo check --release --manifest-path teslatlas-core/Cargo.toml
RUSTFLAGS='-D warnings' cargo test --manifest-path teslatlas-core/Cargo.toml --test hub_delta_e2e --no-fail-fast
```

All three passed. The E2E result was `1 passed, 0 failed`.

## Pending live proof

The physical awake and 60-second persistence observation proof is pending. After
a manual wake, Hub must record a new durable fact within the defined one-minute
proof window without issuing a wake command. MacOS runtime proof is also pending.

## Platform truth

macOS arm64 runtime proof is pending. Debian arm64 now has current native
install and a bounded credentialed collector/no-wake receipt, but the remaining
platform and live-vehicle gates are not complete. Debian amd64 is deferred until
an x86 host is supplied.
Intel macOS is out of scope.

## Migration truth

Typed migration, multi-car staging, open-session modelling, second-snapshot
reconciliation, pack publication, catalogue validation, and outbox retry paths
exist and have local evidence. A final disposable migration and cutover
rehearsal is still required before operational signoff.

## What this does not claim

It does not claim 100%, zero differential differences, a qualifying
low-latency 10-million-row baseline, owner wake success, physical streaming
success, the pending manual-wake plus 60-second no-wake observation, fresh
macOS proof, the remaining Debian ARM platform matrix,
Debian amd64 proof, optional backend integrations, or a completed cutover
rehearsal.
