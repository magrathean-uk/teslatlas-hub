# Execution order

This order keeps every change inside one agent context. A step may begin only
when its listed gate passes or records a genuine external block. No broad
rewrite, unbounded benchmark, or multi-feature batch is allowed.

## Completed before the remaining order

| Area | Evidence |
| --- | --- |
| Delta v2 backend, server, catalogue, client transport, staged apply, atomic activation, resume, and v1 fallback | Hub/core release checks plus `teslatlas-core/tests/hub_delta_e2e.rs`; all latest gates passed. |
| Import staging, multi-car outcomes, open-session second-tail reconciliation, race handling, and export outbox retry | Focused importer/direct/database/collector/pack coverage; final disposable rehearsal still open. |
| P1 integrity corrections: open-parent transition, states/geofences identity, single publication gate, pack metadata, exact stream closure, and in-flight audit scope | Completed implementation slice; physical, macOS, and full migration proof remain open. |
| Debian ARM64 dev80 package and second credentialed offline collector/no-wake receipt | Package compiled and installed with no compiler warnings; correlation and audit watermark 5 are recorded. |

## Remaining order

| Order | One small step | Exit gate |
| --- | --- | --- |
| 1 | Obtain manual owner wake and observe persistence for 60 seconds. | New durable live fact, no wake request, and matching audit receipt. |
| 3 | Establish source snapshot lease. | Source write refusal and lease-loss fail closed. |
| 4 | Attach one binary COPY lane. | Same snapshot visibility from two readers. |
| 5 | Stream one source relation at a time. | Count/checksum/projection gate for each relation. |
| 6 | Add bounded lane/profile control. | Resource admission and source-pressure gate. |
| 7 | Add set-based reconciliation and candidate cleanup. | No unexplained difference or reachable failed pack. |
| 8 | Build differential TeslaMate behavior runner by family. | Pinned differential artifact per family. |
| 9 | Run crash/corruption/network/storage fault matrix. | Recovery, no-loss, no-duplicate gates. |
| 10 | Add conservative adaptive profile. | Correctness preserved under measured profiles. |
| 11 | Finish fresh macOS arm64 platform proof. | All macOS matrix cells current. |
| 12 | Finish remaining Debian arm64 platform proof: reinstall, restart, recovery, backup/restore, and admission matrix. | Current dev78 install and bounded collector/no-wake evidence is recorded; all remaining matrix cells pass. |
| 13 | Decide optional MQTT/updater scope. | Implemented proof or approved explicit exclusion. |
| 14 | Run disposable full rehearsal, cutover plan, rollback plan. | Signed rehearsal report; source untouched. |
| 15 | Produce final evidence index and signoff decision. | Every required matrix row is current. |

## External waits

Owner vehicle wake is needed for the physical awake and 60-second persistence
proof. An x86 host is needed for deferred Debian amd64 work. macOS runtime proof
is pending. Fresh platform validation requires the current release artifacts,
not historical results.

## Stop rule

If a gate fails, fix only its smallest identified cause and rerun that gate.
Do not mark a later row complete from a related or older success. Do not claim
100% before final evidence and signoff.
