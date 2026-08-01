# Verification and evidence

## Evidence rule

A claim is only as broad as its artifact. Record the pinned TeslaMate revision,
Hub revision and package digest, command, fixture/corpus digest, host/runtime
profile, UTC time, expected result, actual result, and redacted outputs. Never
store a token, password, pairing secret, or source connection secret.

## Required proof families

| Family | Minimum gate | Current classification |
| --- | --- | --- |
| Reference lock | Clean pinned source plus fixture inventory | Proven reference identity; differential suite incomplete. |
| Unit and contract | Focused Rust/client tests | Local evidence. |
| Simulator | Deterministic owner API and lifecycle trace | Mock evidence only. |
| Live source | Read-only source counts/import/receipts | Exact dev72 native ARM import completed with 10,696,758 projected rows; remaining migration reconciliation and rehearsal gates are still open. |
| Live owner | Manual wake plus one-minute durable collection | Physical awake and 60-second persistence observation proof remains pending. |
| Migration | Snapshot lease, binary COPY, reconciliation, replay | Implementation is broad; complete native rehearsal unverified. |
| Durability | Crash, corruption, backup/restore, replay | Partial native backup/restore; full fault matrix unverified. |
| Delta v2 backend | Writer, catalogue, server negotiation, pack authorization | Completed locally; Hub release check passed. |
| Delta v2 client | Transport, base staging, ordered apply, atomic activation, resume, v1 fallback | Completed local cross-repo E2E. |
| Import staging/race/outbox | Multi-car staging, second-tail reconciliation, rollback, retry publication | Completed local focused implementation/tests; final rehearsal unverified. |
| PerformanceProfile v1 | CPU/filesystem measurement, bounded direct-import lane selection, deterministic override, non-secret receipt | Implemented in dev73; runtime/profile proof not yet captured. Memory-pressure and write-throughput profiling are deferred. |
| macOS arm64 | Current artifact full platform matrix | Earlier evidence only; fresh proof unverified. |
| Debian arm64 | Fresh install, space/database, service, backup, restore | Debian 13 ARM64 cloud-init/headless VM with 8 vCPUs/8 GiB has `0.1.0-dev78` installed. The credentialed production collector completed with correlation `3478e1ab-e125-4734-b43e-c7ab3289ccdc`, seeing one vehicle, zero online vehicles, snapshots, observations, and failures. `verify-no-wake` against audit watermark `2` returned `verified: true`, with 3 matching receipts, 0 direct wake, 0 unresolved requests, and 0 unresolved stream sessions. Encrypted state metadata is under `/var/lib/teslatlas/legacy-auth` mode `600`; physical persistence and remaining recovery/platform gates are open. |
| Cutover | Disposable operator-owned rehearsal and rollback | Unverified. |

## Implemented but unproven profile slice

PerformanceProfile v1 is implemented in dev73. It measures available CPU
parallelism plus filesystem capacity, safely reduces only direct-import COPY
lanes, supports a deterministic override, never raises configured or hard
safety limits, and logs a non-secret receipt. No runtime/profile proof is
claimed yet. Memory-pressure and write-throughput profiling remain deferred.

## Exact latest local evidence

```text
package: 0.1.0-dev78
collector: teslatlas-hub-collect.service
correlation: 3478e1ab-e125-4734-b43e-c7ab3289ccdc
vehicle_count: 1
online: 0
snapshots: 0
observations: 0
failures: 0
audit_watermark: 2
no_wake_verified: true
matching_receipts: 3
direct_wake_receipts: 0
unresolved_requests: 0
unresolved_stream_sessions: 0
encrypted_state_metadata: /var/lib/teslatlas/legacy-auth (mode 600)
```

```text
RUSTFLAGS='-D warnings' cargo check --release --all-targets
RUSTFLAGS='-D warnings' cargo check --release --manifest-path teslatlas-core/Cargo.toml
RUSTFLAGS='-D warnings' cargo test --manifest-path teslatlas-core/Cargo.toml --test hub_delta_e2e --no-fail-fast
```

The Hub check, Teslatlas core check, and cross-repo E2E all passed. The E2E
passed `real_hub_v2_e2e_preserves_base_and_resumes_deltas` with `1 passed, 0
failed`.

## Test data levels

- `Local`: source-free fixtures and temporary Hub roots.
- `Mock`: fake Tesla endpoint or simulator. It proves request shape and lifecycle
  behavior, not Tesla authorization or physical vehicle behavior.
- `Live`: actual source or owner vehicle. It must be redacted and read-only.
- `Release`: current native artifact plus required corpus and full evidence
  bundle. No present result has this status.

## Evidence gates

1. Record input identity before execution.
2. Capture source read-only proof before migration or live test.
3. Keep raw artifacts protected; publish only redacted digest/index records.
4. Verify exact expected invariants, not only exit status.
5. Classify failure as failed, skipped, or blocked. Do not convert it to pass.
6. Bind final approval to the exact artifacts. Any code, reference, corpus,
   protocol, configuration, profile, or platform change invalidates affected
   proof.

The release ledger remains [COMPLETION_EVIDENCE_MATRIX.md](../COMPLETION_EVIDENCE_MATRIX.md).
