# Adaptive runtime profile v1

Hub selects one bounded profile before a migration, collection, or sync run.
The selector accepts only measured baseline records for the same supported
architecture, filesystem/device class, source-link class, corpus scale, and
Hub/adapter revision. It compares CPU availability, allocatable memory,
free bytes/inodes after the recovery reserve, measured storage/fsync behavior,
and measured source RTT/throughput with the record's admission bounds. Unknown,
changed, pressure-affected, or insufficient facts select the conservative
profile; they never guess a faster setting.

## Tunable and fixed behavior

| Control | Profile may select | Must remain fixed |
| --- | --- | --- |
| Migration | bounded COPY lane count, bounded decode/page/channel capacities, stage writer batching, cache bound, and pack chunk target | one exported read-only snapshot, typed decoding, selected-car scope, hard row/byte limits, set validation, sealing, reconciliation, and publication order |
| SQLite | a measured cache bound and a measured checkpoint trigger only after crash/write/recovery proof | full-synchronous stage transactions, integrity checks, atomic manifest publication, recovery reserve, and default SQLite-managed checkpoints when no qualifying proof exists |
| Collection | bounded local page/cache allocation only | one authority, no-wake route guard, 5/10/75-second ceilings, rate/circuit gates, durable acknowledgement, and no whole-history retention |
| Sync | bounded pack/download/apply workspace and chunk target | signature/cursor verification, full snapshot fallback, atomic swap, one refresh pipeline, and pack size/expansion hard limits |

Validation depth is never a tuning control. A profile cannot omit source
checks, relationship/accounting checks, pack verification, reconciliation, or
recovery tests; it may only change bounded execution parallelism proved by the
baseline.

## Selection and persistence

Profiles are versioned, conservative tables generated from the baseline
artifact, rather than formulas that invent host limits. Each entry declares
its exact admission predicates, maximum resources, tested corpus/result IDs,
and all selected values. The selector chooses the highest entry whose every
predicate holds; otherwise it chooses the serial/current-default safe entry.
It records host facts, rejected candidates, chosen profile ID/digest, resource
budget arithmetic, and reasons in the migration audit report before opening a
source transaction or mutating a Hub stage.

The profile is immutable for that capture and bound into its stage and manifest
evidence. A restart re-evaluates only before a new capture; interrupted
repeatable-read captures are discarded rather than resumed under another
profile. During an active run, later pressure may only trigger the documented
backpressure, pause, or failure path. It may never raise concurrency, buffers,
cache, or checkpoint aggressiveness mid-run.

Every profile change requires a new baseline series proving equal or better
reconciliation, integrity, crash recovery, durable-ack, duplicate, and
no-wake outcomes. A faster elapsed time alone cannot admit a profile.
