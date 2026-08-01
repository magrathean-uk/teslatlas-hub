# Performance baseline protocol v1

The baseline is a measured release artifact, not a synthetic promise. Each run
uses a pinned Hub build, pinned TeslaMate schema/reference, immutable corpus
digest, and a supported host profile. The representative approximately
10-million-row corpus runs on Apple-silicon macOS and the local Apple
Virtualization Debian arm64 guest (8 vCPUs, 8 GiB RAM). Debian amd64 runs when
the supplied x86 host is available. Its end-to-end migration target is under
ten minutes; any run reaching thirty minutes is a hard failure. WAN and other
unsupported paths may be recorded for correctness, never used for this claim.

## Corpus and run matrix

Every host runs the same versioned fixtures: a small functional history, the
representative large history, and pathological histories covering maximum
positions, incomplete/open sessions, invalid fields, high fan-out joins,
duplicate/idempotent replay, source interruption, slow storage, and full
stage recovery. Reports label cache condition, concurrent host load, source
location/RTT/bandwidth class, and whether the corpus is synthetic or redacted.
No benchmark needs a live car or a mutable TeslaMate deployment.

Each run retains the source/destination reconciliation result. Failed,
cancelled, or pressure-limited runs remain in the series; summaries give all
individual elapsed times and the worst successful run, never only an average
or median. A corpus or host change starts a new baseline series.

## Measurements

| Area | Measurements |
| --- | --- |
| Host | OS/kernel, architecture, CPU model/count, RAM, local filesystem/type/free space, storage device class, network route/RTT/link class, power/thermal state, and cache/load condition |
| Process | wall and monotonic elapsed time, user/system CPU, peak RSS, thread/task count, context switches, page faults, open files, and exit/status reason |
| Disk and SQLite | stage/pack/database/WAL bytes over time, read/write/fsync latency and bytes, checkpoint duration/result, SQLite page/cache/journal settings, integrity result, free-space reserve, and recovery cleanup bytes |
| PostgreSQL and COPY | preflight and snapshot time, read-only session proof, query fingerprints/plans, lane count, rows/bytes/pages per lane, decode rejects, channel occupancy/backpressure, source wait, network bytes, and copy throughput |
| Migration | phase timings for discovery, preflight, capture, decode, stage commit/seal, set validation, projection, pack verification, manifest publication, reconciliation, and cleanup; total rows/bytes and end-to-end throughput |
| Collection and sync | request-to-durable-ack latency, observed freshness/state, retry/rate/circuit outcomes, raw/projected bytes, pack creation, manifest verification, download/apply/atomic-swap timing, and cursor outcome |
| Startup and recovery | cold start to truthful ready/not-ready, crash point, replay/recovery duration, journal/manifest/pack integrity, duplicate projected fact count, and durable-ack loss count |

Sampling never alters TeslaMate or relies on OS cache eviction. The runner reads
host counters before, during, and after phases; unavailable counters are named
as unavailable rather than estimated. Wall-clock changes are reported beside
monotonic durations.

## Acceptance and evidence

Every migration benchmark must pass source read-only proof, bounded-resource
limits, stage/pack integrity, and zero unexplained reconciliation differences
before its timing can count. The representative-baseline series passes only if
every qualifying supported-host run finishes below ten minutes; thirty minutes
is an immediate failure requiring redesign, not more retries. Collection and
recovery runs separately prove the locked freshness and sixty-second truthful
readiness objectives with zero durable-ack loss and zero duplicate projection.

The runner appends these records to the migration audit report as a `performance`
section and stores raw counters as protected, digest-addressed artifacts.
Adaptation consumes only completed baseline records; it cannot trade away
durability, validation, reconciliation, or no-wake behavior.

## Observed non-baseline run

The 2026-07-31 Debian arm64 native run copied a live read-only TeslaMate source
over SSH/WireGuard rather than the required direct low-latency LAN. It published
10,631,740 projected rows from a source containing 10,385,745 positions in
9m53.548s, with 58.690s process CPU and 186.8 MB peak memory. Full destination
pack hashing, backup, fresh-root restore, reboot, and subsequent live collection
passed. This is strong correctness and WAN-path performance evidence, but it
does not close the formal low-latency baseline series.
