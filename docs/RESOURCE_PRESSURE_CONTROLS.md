# Resource pressure controls v1

Pressure thresholds come only from the selected measured runtime profile and
the non-negotiable recovery reserve. Every detector records observed value,
profile threshold, affected work, action, and recovery outcome in the audit
journal. State progresses from `normal` to `constrained` to `critical`; it
returns to `normal` only after a fresh qualifying observation. No pressure
handler touches TeslaMate, PostgreSQL configuration, Docker, services, or
credentials.

| Pressure | Constrained action | Critical action | Recovery |
| --- | --- | --- | --- |
| Free bytes/inodes or WAL/stage/pack growth | stop admitting new migration pages/packs; drain only already committed Hub work | fail migration before the reserve is crossed; retain published data and seal/discard only the exact unpublished Hub stage | new preflight and profile selection; never resume an open source snapshot |
| Memory or bounded-channel occupancy | backpressure readers and reduce migration lanes only at a page boundary | cancel the read-only capture if bounded retention cannot be maintained; no whole-history spill or allocation | discard incomplete stage and start a fresh capture after capacity passes |
| CPU saturation or thermal/power constraint | reduce optional migration/pack parallelism and defer optional enrichment | pause/fail nonessential migration work when profile budget cannot be met | resume only new work under a fresh profile; collection rules stay unchanged |
| Slow/fsync-failing storage | stop producer admission and let the current immediate transaction finish or fail | mark Hub not-ready where durable state cannot be proved; fail unpublished migration work | integrity/recovery check before readiness; never claim a failed fsync was durable |
| SQLite contention | serialize writers, bound waits, and pause optional projection/pack work | preserve committed journal facts, fail or defer nonessential work, and expose truthful readiness | retry only through the durable operation identity; no duplicate projection |
| Oversized history, row/byte/page limit, or decode expansion | reject before retaining the offending page/row | discard the incomplete private stage and report the bound violation | requires a new explicit limit/profile/corpus decision; no partial publication |
| Source/network backlog or timeout | bounded COPY/HTTP backpressure and account/vehicle retry gates | cancel read-only capture or fail the no-wake collection attempt according to its durable gate | fresh capture after source recovery; never reconnect into an old repeatable-read snapshot |

Durable collection has priority over optional projection, pack production,
geocoding, and migration work. Hub first completes or reports failure for the
current immediate observation/lifecycle transaction; it never acknowledges a
fact before commit or drops an acknowledged fact to relieve pressure. The
5/10/75-second service targets, no-wake guard, account/vehicle gates, and one
active authority remain in force. If pressure prevents truthful operation,
Hub exposes degraded/not-ready state and last-observation age rather than
inventing freshness.

All queues and workspaces have profile-bounded item and byte limits. A producer
must wait, shed only uncommitted optional work, or fail; it may not create an
unbounded retry, memory, disk, or network queue. Published manifests/packs and
sealed evidence are immutable. Cleanup may remove only the exact unreferenced
Hub temporary or verified orphan artifact after catalogue/integrity checks.

Pressure recovery is evidence-driven: re-measure capacity, validate SQLite and
packs, re-run capacity admission, and create a new source capture when needed.
It never silently changes profile, relaxes validation/durability, or promotes a
partial stage.
