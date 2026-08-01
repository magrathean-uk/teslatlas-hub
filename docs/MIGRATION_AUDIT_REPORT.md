# Migration audit report v1

Each migration attempt emits one canonical JSON report, including a failed or
cancelled attempt. The report is immutable once sealed. It has a schema
version, report ID, UTC and monotonic start/end times, Hub build/revision,
pinned TeslaMate revision, command/config digest, host/runtime profile, and a
SHA-256 digest of its canonical bytes. When the protected Hub signing key is
available, it also has a detached signature. Referenced reports, stages,
packs, exports, and logs are named by stable ID, size, and digest.

## Required evidence

| Area | Required record |
| --- | --- |
| Discovery and preflight | selected source/vehicle identity hashes; pinned migration/schema versions; endpoint class without hostname or database name; required-table/schema checks; capacity limits; source clock range; backup/restore gate; and every pass/fail reason |
| Read-only capture | read-only/repeatable-read/UTC session contract; selected-table query fingerprints; source snapshot boundary; typed COPY lane/table/page counts; bounded decode, stage, and free-space limits; rows/bytes retained/rejected; source table count/key/hash/time/relationship summaries; connection/copy/transform/validation timings and throughput |
| Transform and publication | mapping contract/version and loss ledger; stage digest/seal result; projection counts/anomalies/quarantine; set-based validation; pack IDs, hashes, sizes and verifier result; manifest identity, sequence, cursor, totals, digest and signature verification |
| Reconciliation | source and destination keyed hashes, counts, aggregates, relationship/time/spatial/deep-value checks; every difference key/hash/reason/severity; accepted-anomaly approval ID; fatal gate outcome |
| Credentials and no-wake | credential mode and non-secret identity/scope/expiry outcome; candidate/rollback custody result; request class/count audit; sleep/online guard; rate/circuit gates; explicit proof that no wake or command route was used |
| Live, window, cutover, rollback | owner acknowledgement and redacted probe baseline/after-state; verification-window checkpoints; plan and receipt digests; operator confirmations; Hub authority transitions; Hub-only interval export and archive verification; final readiness and reversal outcome |

The report records a terminal status of `passed`, `failed`, `cancelled`, or
`blocked`, plus an ordered gate list. A gate names its input evidence IDs,
expected and observed result, decision, reason code, and elapsed time. A failed
or interrupted capture includes its partial timings and cleanup outcome, but
never presents an unpublished stage as a successful migration.

## Redaction and retention

Reports contain no bearer, refresh token, credential ciphertext, private key,
password, raw owner response, connection string, unredacted host identity, or
secret-bearing command line. Values needed for correlation use stable keyed
identifiers or one-way hashes; errors are classified before recording. The
report writer rejects forbidden field names and scans rendered text for known
credential locations before sealing. A report may reference a protected local
artifact, but that reference includes only artifact ID, type, digest, size, and
retention class.

Hub retains the report with its sealed stage/pack and reconciliation evidence
until the corresponding verified backup retention floor passes. A later report
may supersede an earlier attempt only by naming its digest; it cannot rewrite
or erase the earlier outcome.

## Current rehearsal record

The 2026-07-31 Debian arm64 rehearsal used a source-side read-only,
repeatable-read snapshot through encrypted SSH/WireGuard transport. Source
preflight observed one selected car, a 2.04 GB PostgreSQL database, 10,385,745
positions, and 57.5 GB guest free space. Hub published 10,631,740 projected rows
in 9m53.548s, then passed full catalogue hashing, repair, native install
verification, 438-pack online backup, fresh-root restore, reboot, and a new live
owner snapshot with zero vehicle failures.

This record is not yet the canonical signed JSON audit artifact required above.
It does not claim source keyed-hash reconciliation, a formal low-latency
baseline, Debian amd64 proof, or final release signoff.
