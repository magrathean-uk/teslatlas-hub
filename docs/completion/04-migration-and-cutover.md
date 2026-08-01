# Migration and cutover

## Safety rule

Hub may inspect and copy TeslaMate. It may not write the source database or
change TeslaMate service, Docker, container, schedule, configuration, or
credentials. The operator alone controls any later TeslaMate change. Hub never
installs a kill timer.

## Required sequence

| Phase | Small action | Current state and gate |
| --- | --- | --- |
| M01 | Inspect source schema/version, selected/all-car scope, disk space, inodes, permissions, and network path. | Implementation exists; current Debian fresh space/database proof remains. |
| M02 | Establish one read-only repeatable-read source snapshot lease. | Remaining native source proof. |
| M03 | Discover every intended car before capture. | Implemented with explicit selected, skipped, and failed results; native full rehearsal remains. |
| M04 | Stream one relation through fixed typed binary COPY into bounded staging. | Implemented; per-relation native count/checksum proof remains. |
| M05 | Repeat relation capture independently for each remaining car. | Multi-car staging and partial-failure cleanup implemented; final rehearsal remains. |
| M06 | Reconcile source open drive, charging process, state, and standalone positions with a second bounded source read. | Implemented and covered by open-child tail, close transition, duplicate, restart, and rollback fixtures. |
| M07 | Produce immutable packs and publish only verified catalogue state. | Implemented; Hub release and cross-repo delta E2E passed. |
| M08 | Re-run the exact same input after interruption and after success. | Import race, staging, and outbox retry paths implemented; final disposable rehearsal remains. |
| M09 | Hand off eligible legacy or Fleet credentials to Hub-owned secure storage. | Implemented with custody/redaction paths; operational rehearsal remains. |
| M10 | Start Hub only and run verification window. | Pending fresh Debian reinstall, space/database validation, backup/restore, and source-drift proof. |
| M11 | Capture observation and outbound-request audit watermarks; ask owner to wake car manually; wait one minute; collect once. | The scripts now require a UUID correlation receipt, durable observation verification, and `verify-no-wake`; physical wake and streaming proof remain pending. |
| M12 | Generate cutover and rollback plans. | The redacted report records the audit watermark, correlation ID, no-wake result, direct-wake count, unresolved request count, and unresolved stream-session count. Final disposable rehearsal and operator signoff remain pending. |

## All-car and open-session contract

All cars means all intended source cars, not merely the first discoverable car.
Each car gets its own identity, source watermarks, result, and immutable
publication record. Batch reporting separates `succeeded`, `skipped`, and
`failed`; partial completion is visible.

An open drive, charge, state, or standalone position is not ordinary finished
history. Import records its source parent and watermark. The cutover
reconciliation decides whether new children require continuation. It must not
close a session merely because a race, partial row, sleep transition, or source
read boundary made it look quiet.

## Speed and space

Use direct binary COPY, bounded staging, and pack publication. Do not create a
whole-database dump or unbounded in-memory history. Admission reserves space
for stage, pack, WAL/catalogue, backup, and recovery headroom before capture.
The qualifying direct-LAN 10-million-row run must remain under ten minutes;
30 minutes fails.

See [full rehearsal](../FULL_PARITY_REHEARSAL.md) and
[migration audit design](../../roadmap/066-design-migration-audit-report.md).
