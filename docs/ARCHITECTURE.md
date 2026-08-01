# Architecture

This is the current target architecture. Its boundaries and proof requirements
are governed by the ordered [Wayfinder map](../roadmap/000-map.md).

Teslatlas Hub is a native service, not a container bundle.

| Layer | Choice | Boundary |
| --- | --- | --- |
| Host | Apple-silicon macOS; Debian 12+ amd64 or arm64 | Native process; systemd only on Debian |
| Hub | Rust, bundled SQLite, systemd credentials | Host owns tokens and pack catalog |
| Transport | Rustls TLS, HTTP/2, zstd SQLite packs | iPhone gets typed mirror data only |
| Phone | Swift networking, Rust-owned SQLite mirror | One selected Hub vehicle per source profile |

## Data lanes

`owner-token` is the compatibility lane. It performs explicit GET-only vehicle
discovery, never calls wake, stores bounded raw observations, and publishes a
signed typed car snapshot. This establishes an immediate iPhone source without
pretending that a single current-state response is completed trip history.

TeslaMate PostgreSQL is the history lane. It performs a TLS, read-only,
repeatable-read capture for one selected car, then emits parent-complete typed
history fragments.

Fleet is the future ongoing-data lane. It needs an owner-registered Tesla
application, its own credentials, callback and telemetry setup. It is not a
fallback that silently changes token behaviour.

## Credential mode selection

The configured source selects its credential mode explicitly: data-only needs
no Tesla credential, `owner-token` enables compatibility collection, and a
TeslaMate source enables only the separate read-only PostgreSQL migration
credential. Fleet is a distinct future source mode with its own application
authorization. It never falls back to an owner token, and a legacy token never
activates Fleet behavior.

During migration, legacy compatibility and Fleet sources may coexist across
different vehicles. For one source vehicle, Hub permits exactly one active
collection authority at a time; moving that vehicle from owner-token to Fleet
first stops the old authority, then activates the new one. The cursor-signing
key and paired-device bearers are Hub credentials, not Tesla credential modes.

## Credential lifecycle

Owner tokens enter only from a protected file or password agent and TeslaMate
passwords only from an explicit protected input. Each is encrypted with the
host systemd credential key, written to a private temporary file, then renamed
atomically before a unit may use it. Units receive decrypted bytes only through
their private credential directory, read them only for their operation, and
never store, export, or log them. Configuration, argv, process environment,
Hub SQLite, packs, diagnostics, and backups contain no Tesla secret.

Replacement first verifies a complete encrypted candidate, atomically replaces
the old ciphertext, then restarts only the affected Hub-owned collection
authority. A failed write preserves the old credential. Revocation removes the
encrypted credential and its Hub service drop-in, stops that Hub authority,
and records only mode, source, time, and outcome in the audit journal. Missing,
invalid, expired, or revoked credentials fail closed; recovery requires a new
explicit provision. Fleet refresh rotation follows the same candidate-before-
replace rule, with crash tests at each durable transition.

## TeslaMate credential handoff

Credential handoff is optional and separate from history migration. Data-only
migration is the default. An owner may request a Hub candidate credential from
a discovered read-only TeslaMate source through a protected local channel; the
candidate is decrypted only in memory, never printed, logged, placed in argv,
environment, configuration, SQLite, packs, diagnostics, backups, or a plaintext
temporary file. It streams directly into a Hub-owned host-encrypted credential
candidate, which is private, fsynced, and atomically renamed only after complete
encryption succeeds. TeslaMate ciphertext, keys, rows, files, and refresh state
are never rewritten, revoked, rotated, deleted, or otherwise changed.

Hub verifies a candidate without disclosure using only the permitted read-only
vehicle discovery endpoint and compares non-secret selected-vehicle identity
evidence. Failed verification leaves the prior Hub credential and all
TeslaMate material untouched. The audit records source kind, candidate outcome,
non-secret identity hashes, time, and rollback custody, never a token, refresh
value, key, ciphertext, or decryptable digest. A candidate remains inactive
until a later explicit owner action starts the Hub collector.

Legacy owner and Fleet credentials are different modes and never convert into
each other. A Fleet bundle is accepted only by a versioned Fleet handoff adapter
with its own identity/scope verification; otherwise the owner provisions a new
Hub Fleet credential through the protected channel. Handoff never starts a
second refresh authority or automates TeslaMate cutover. Rollback means keeping
or removing only the Hub candidate and selecting the prior Hub source; custody
of the unchanged TeslaMate credential remains with TeslaMate and the owner.

## Tesla API client boundary

Legacy compatibility uses only an explicit operator-selected HTTPS base and
GETs for `/api/1/products` and online-vehicle `vehicle_data`. It does not
discover a region or endpoint from redirects: every redirect is rejected before
the bearer can be replayed. Fleet selects its documented regional base as part
of an explicit source configuration; it never silently crosses a region. TLS
uses the platform trust store and rejects plaintext, embedded credentials,
query parameters, and fragment-bearing base URLs.

Every response is capped at four MiB and decoded as a bounded JSON envelope.
Unknown vehicle-data fields remain journal data unless credential-shaped; known
control fields must validate before projection. Each request has the explicit
configured timeout and no client-layer retry. Status handling distinguishes
authentication or scope failure, vehicle-unavailable, rate limiting with its
validated retry hint, transient server or transport failure, and terminal
protocol failure. The next retry policy consumes those classes; it never
retries a wake, command, or ambiguous write because this client has none.

## Collection recovery

Recovery state is durable per account and source vehicle. A `429` honors a
valid delta `Retry-After`; absent, malformed, or excessive hints fall back to
the TeslaMate-compatible five-minute account hold. The known disabled-account
limit signal holds the account for fifteen minutes. The account gate prevents
every vehicle request until it expires; a vehicle-unavailable result delays
only that vehicle and never causes a wake.

Transient vehicle faults use bounded exponential backoff with deterministic
full jitter derived from the durable source, vehicle, and attempt number.
TeslaMate's observed vehicle fuse is the lower bound: three non-timeout,
non-auth API failures inside ten minutes open the vehicle circuit for five
minutes. Authentication or scope failure disables the collection authority
until explicit reprovisioning. A success closes only that vehicle's transient
state. Restart reloads the same gates and retry budget before a request; no
failure path may create an account-wide request storm.

## No-wake policy

Hub never has a wake or command route. Compatibility discovery may read the
product list, but `vehicle_data` is permitted only after that same collection
reports the vehicle exactly `online`. Asleep, offline, suspended, and unknown
or malformed state are all non-queryable: record their discovery state and
last-observation age, then wait for a later discovery result. Charging,
driving, and updating are queryable only when their discovery state is
`online`; state labels never override that guard.

The state machine schedules at most the locked freshness ceilings: five
seconds driving, ten seconds charging, and seventy-five seconds ordinary
online. Asleep and offline have no freshness promise. A `vehicle-unavailable`
result returns to discovery only; it never escalates to wake. Every decision
records state, guard, chosen action, and non-secret reason in the durable
operation journal, allowing deterministic replay and audit after restart.

## Owner-authorized live probe

The live probe is a manual Hub-only script and the only stage that pauses for
a physical action. It first records a redacted baseline: selected Hub
source/vehicle identity, latest durable observation ID, payload hash and
receipt time, lifecycle cursor, manifest sequence, readiness, and probe start
time. It then asks the owner to wake the vehicle using the Tesla app or a
physical action, and waits for acknowledgement. Hub never sends a wake
request, command, or TeslaMate/Docker/service action.

After acknowledgement the script waits one minute, then grants one temporary
Hub-owned lease for the selected vehicle and runs exactly one no-wake
`collect-once` worker. The worker may discover vehicles and reads
`vehicle_data` only when discovery reports that selected vehicle online. It
exits on completion; no schedule, supervised collector, or second authority
remains. The script records worker request classes and rejects any route
outside that allowlist before accepting proof.

Success requires a post-baseline durable observation for the selected vehicle:
a new observation ID, committed receipt time after the baseline, canonical
payload hash, valid source observation time when supplied, no duplicate-only
result, and a verified current Hub manifest/lifecycle cursor that includes the
commit. The signed report contains before/after cursors, timestamps, redacted
identity, worker outcome, request audit, and manifest/pack evidence. Offline
discovery, no new durable fact, duplicate-only reply, failed validation,
timeout, or unexpected request fails the probe with baseline data intact. The
script may cancel only its Hub worker and removes the temporary lease in every
outcome; it never changes TeslaMate.

## Dual-run safety

TeslaMate and Hub must not run continuous collection for the same vehicle.
During side-by-side operation Hub remains `migration-only`; the one-minute
owner-authorized live probe is the sole, bounded exception. It uses one
explicit temporary lease, one discovery request, and at most one online-only
vehicle-data request, then exits. It neither starts a refresh loop nor assumes
TeslaMate is idle, and it may not expand the probe because TeslaMate has not
reported a conflicting state.

Before the exception, Hub requires no active account hold, vehicle circuit,
authentication failure, or pending credential replacement. A rate-limit,
vehicle-unavailable response, unexpected route, timeout, or any failed
validation cancels the Hub lease and produces no retry within that window. The
existing durable account gate and vehicle circuit then control later Hub work;
the probe never bypasses a `Retry-After`, five-minute account hold,
fifteen-minute disabled-account hold, or credential reprovisioning requirement.
Discovery remains the sleep guard: Hub does not read vehicle data unless that
same response says the selected vehicle is online.

Credential handoff remains inactive during overlap. A copied legacy candidate
is read only for the single probe and never refreshes, rotates, revokes, or
replaces TeslaMate material; Fleet and owner-token modes remain separate. The
report records only redacted source identity, credential mode, gate state,
request count/classes, and outcome. It contains no bearer, refresh artifact,
or TeslaMate configuration data.

The probe does not merge or write TeslaMate history. It proves only Hub's
post-baseline durable observation and projection, retaining its receipt time,
source time when available, payload hash, cursor, and manifest evidence for
later read-only reconciliation. Any timestamp, hash, state, or projected-fact
difference is reported as unresolved rather than repaired, suppressed, or used
to change either collector. Continuous Hub collection requires an explicit,
owner-controlled cutover after the separate verification window; Hub automation
cannot pause, stop, or reconfigure TeslaMate to make that safe.

## Read-only verification window

Readiness observation lasts about twenty-four elapsed hours, measured from a
sealed Hub baseline to a final report; its actual start, end, and checkpoint
times are evidence rather than a fabricated cadence. The verifier is a
Hub-owned, manually invoked read-only command. It installs no TeslaMate unit,
timer, schedule, hook, or configuration, and it does not start collection
outside the separately authorized one-minute probe. Its only external source
access is the existing read-only migration connection when an operator requests
a fresh source fingerprint.

At baseline and each operator checkpoint, the verifier records Hub binary and
configuration identity, readiness, SQLite integrity and WAL/checkpoint state,
free-space reserve, journal/observation and projection counts, duplicate and
quarantine counts, lifecycle and manifest cursors, signature/pack verification,
credential mode/presence/expiry state without secrets, and the explicit
`migration-only` authority. It records any declared probe separately, including
its temporary lease and after-state; unexpected Hub data growth, collection
authority, route, credential change, failed integrity check, or readiness loss
is a failed window rather than normal activity.

Parity is rechecked against the sealed import stage and its source-copy facts:
table counts, ordered keyed hashes, relationship summaries, time ranges,
mapping-loss inventory, and every previously accepted anomaly. A requested
fresh PostgreSQL fingerprint is labeled as source drift after the seal, never
silently substituted for that parity baseline. New unexplained destination
differences, invalid pack/manifest signatures, or a changed source/destination
identity fail readiness. The report distinguishes immutable migration parity,
the single live-probe proof, and later source drift so that it never claims
live equality without a common capture boundary.

Credential and rollback checks validate only Hub custody: encrypted candidate
integrity, non-secret identity/scope, refusal while invalid or expired, and a
dry-run rollback plan that selects the prior Hub source or removes only the Hub
candidate. TeslaMate credential bytes, refresh state, service, database,
containers, schedules, and configuration are neither read beyond the already
authorized source boundary nor changed. The final signed readiness report is
advisory: it names passed and failed gates, all evidence IDs, and manual
operator cutover prerequisites; it performs no cutover.

## Operator-owned cutover and rollback

Cutover begins with a Hub read-only `cutover-plan` command. It refuses unless
the signed verification-window report, sealed migration reconciliation, Hub
backup/restore rehearsal, selected source identity, credential candidate, and
one-minute probe evidence are all current and passing. It discovers the
TeslaMate deployment only read-only and prints a signed, single-use plan: exact
machine-specific operator commands to relinquish the old collection authority,
the Hub commands to activate the selected source and collector, expected
identities and readiness output, consequences, expiry, and the matching
rollback commands. It never executes, pipes to a shell, or invokes any
TeslaMate, PostgreSQL, Docker, container, service, timer, schedule, or
configuration command.

The operator independently performs the printed TeslaMate-side action, then
confirms its completion to Hub with the plan nonce. Hub records an attestation,
not a claim that it controlled the action; where a read-only observation is
available it records that separately. Only then may the operator run the
printed Hub activation command. Hub atomically changes its per-vehicle
authority from `migration-only` to the selected Hub source, starts a new
provenance generation/full snapshot, and allows only that Hub collector. A
failed confirmation, stale plan, missing gate, failed Hub activation, or
ambiguous source deployment leaves Hub `migration-only` and demands a newly
generated plan.

Rollback is another signed operator plan. The operator first stops the active
Hub collector by its printed Hub-only command. Before Hub relinquishes that
authority, it seals and verifies an export of every Hub-only interval from
cutover to stop time, including source identity, observation range, payload
and pack hashes, manifest sequence, and archive location; a failed export
blocks rollback. Hub then atomically deactivates its collector, retains that
history as an inactive source generation, and records the rollback boundary.
The operator separately runs the plan's original TeslaMate-side restoration
command. No history is merged, overwritten, or silently reconciled; return to
Hub later needs a new verification window and a new plan.

The resulting cutover or rollback receipt contains plan digest and expiry,
operator confirmations, Hub authority transitions, source identities, archive
evidence, command result classes, and final readiness without secrets. It is
evidence and manual instructions, not authority to mutate TeslaMate.

## Migration audit report

Every migration attempt, including failure or cancellation, seals a redacted
canonical report defined by `docs/MIGRATION_AUDIT_REPORT.md`. It binds the
pinned source, Hub build, command/config digest, source discovery and
read-only/repeatable-read capture contract, typed bounded copy/stage evidence,
mapping and reconciliation outcomes, publication verification, credential and
no-wake audit, live/window evidence, and any cutover/rollback receipts. Each
gate names its inputs, expected/observed result, reason, timing, and terminal
decision. Report and artifact hashes form the reproducible evidence chain; a
later attempt may reference, but never rewrite, a prior report.

## Performance baseline

The normative measured-baseline protocol is
`docs/PERFORMANCE_BASELINE.md`. It captures host, CPU, memory, storage/WAL,
query/COPY, migration, collection, sync, startup, and recovery counters by
phase alongside reconciliation and integrity results. The representative
approximately ten-million-row corpus must complete end to end under ten
minutes on every qualifying supported-host run; thirty minutes is a hard
failure. Cache/load and unavailable-counter conditions are evidence, not
silently normalized away. No performance result can waive bounded resources,
read-only source access, durable acknowledgements, validation, reconciliation,
or no-wake rules.

## Adaptive runtime profile

`docs/ADAPTIVE_RUNTIME_PROFILE.md` defines startup-only profile selection from
qualifying measured baselines. A profile may bound execution resources and
parallelism, not weaken capture, durability, validation, reconciliation,
publication, source authority, or safety guarantees. Its admission facts,
rejected candidates, selected values, and digest are recorded before capture
and remain immutable for that run. Unknown or changing conditions choose the
serial/current-default safe profile; active pressure can only slow, pause, or
fail work, never increase resources.

## Resource-pressure controls

`docs/RESOURCE_PRESSURE_CONTROLS.md` defines bounded reactions to disk,
memory, CPU, storage, SQLite contention, oversized source data, and network
backlog. Collection durability takes precedence over optional work; migration
fails before crossing recovery reserve and an open source capture is discarded,
not resumed. Every action is recorded, preserves truthful readiness and
last-observation age, and can only remove exact unreferenced Hub artifacts
after integrity checks.

## Fake Tesla and fault injection

`docs/FAKE_TESLA_FAULTS.md` defines the virtual-clock, local-only simulator and
test-only named Hub fault points. It scripts allowed API and typed migration
source behavior, network/storage/corruption/crash faults, and exact request
audits without a live car or mutable external service. Private restart runs
must prove durable acknowledgement, idempotent projection, pack/manifest
consistency, and truthful readiness before a scenario passes.

## Differential conformance

`docs/DIFFERENTIAL_CONFORMANCE.md` requires every virtual scenario to compare
Hub against the clean pinned TeslaMate reference through normalized,
Teslatlas-visible facts rather than schema/layout similarity. It covers
lifecycle, data, calculations, request behavior, and crash/recovery; any
unexplained difference is fatal. The reference runner is disposable and never
touches the user deployment. MQTT and the excluded web integrations are checked
only negatively: Hub must emit none.

## Representative database corpus

`docs/REPRESENTATIVE_CORPUS.md` and its versioned manifest define deterministic
synthetic and reviewed-redacted TeslaMate source fixtures. They cover supported
and rejected schema versions, selected-car isolation, approximately ten-million
rows, open/corrupt histories, unusual charging, and settings without depending
on production data. Each fixture binds source identity, expected validation,
normalized result, and reproducible generator/sanitizer provenance.

## Security and privacy model

`docs/SECURITY_PRIVACY_MODEL.md` defines assets, boundaries, controls,
credential/device lifecycle, privacy limits, adversarial proof, and explicit
non-goals. Hub limits credentials to protected runtime delivery, exposes only
the paired typed mirror, and keeps TeslaMate external and read-only. It makes no
application-level encryption claim for telemetry at rest or compromise of host
root/physical disk; those require operator filesystem and access controls.

## Native packaging and supervision

`docs/NATIVE_PACKAGING.md` defines Debian native package contents, service
account/directories, hardened manual units, explicit activation, upgrade, and
data-preserving removal. Package operations own Hub only and never schedule or
mutate TeslaMate. Apple-silicon macOS requires an equivalent native,
non-systemd delivery contract; Intel macOS is outside the first platform set.

## Upgrade, downgrade, and rollback

`docs/UPGRADE_ROLLBACK.md` requires verified Hub backup and compatibility gates
before a Hub-only upgrade. Migrations are transactional and forward-only;
unsafe downgrade is refused, while incompatible rollback restores a matching
pre-upgrade backup into a fresh private generation. No upgrade changes source
authority, credentials, collection, pairing, or TeslaMate by implication.

## Platform and install matrix

`docs/PLATFORM_INSTALL_MATRIX.md` makes Apple-silicon macOS, Debian amd64, and
Debian arm64 independent native release gates across clean install, reboot,
upgrade/rollback, restore, storage, resource, corpus, and fault paths. A
Raspberry Pi-class arm64 run proves bounded correctness/recovery; only hosts
meeting the baseline envelope may carry the ten-minute migration claim.

## Release and supply-chain trust

`docs/RELEASE_SUPPLY_CHAIN.md` binds locked dependencies, verified vendored
SQLite, build provenance, repeatable native artifacts, offline manifest/hash
verification, and independently pinned Minisign trust roots. GitHub remains
storage only. Signed emergency revocation and trust-root rotation fail closed
without mutating TeslaMate or user data.

## Operator runbooks

`docs/OPERATOR_RUNBOOKS.md` gives single-command Hub-only normal paths and
gated procedures for installation, health, credentials, collection, migration,
repair, backup/restore, upgrade, cutover, rollback, and incidents. It preserves
secrets/evidence, forbids manual source mutation and live database copying, and
uses generated signed plans for any operator-owned TeslaMate action.

## Full parity rehearsal

`docs/FULL_PARITY_REHEARSAL.md` requires a disposable native, end-to-end
journey from signed install through read-only migration, sync, credential
handoff, bounded live proof, verification, operator cutover, injected failure,
and rollback. It records every gate and permits no manual repair or production
cutover; a missing artifact, unexplained difference, or Hub source mutation is
a rehearsal failure.

## Final parity signoff

`docs/FINAL_SIGNOFF.md` permits only a named accountable release authority to
sign a current evidence-bound approval. Every matrix row, hard safety target,
native proof, migration/recovery result, live proof, and release artifact must
pass with zero unexplained difference. Approval expires on any material input
or evidence change and never performs production cutover.

## Collection state machine

The pure per-vehicle machine has `start`, `offline`, `asleep`, `online`,
`driving`, `charging`, `updating`, `suspended`, `unknown`, and `quarantined`
states. Inputs are durable discovery/data observations, timer expiry, classified
failure, explicit Hub credential action, and restart. Its outputs are typed
commands only: schedule, product discovery, guarded data read, append fact,
apply projection, update retry state, or safe quarantine. The effect runner is
outside the machine, so replay requires neither network nor clock access.

`start`, asleep, offline, suspended, and unknown schedule discovery only.
An online data observation derives driving, charging, or updating when its
validated fields require it; otherwise it stays online. Drive and charge
submachines retain open sessions until deterministic terminal evidence or safe
timeout closure. A malformed observation quarantines the affected lifecycle
cursor without erasing its journal. Restart reloads the committed machine state
and replays only facts after its durable cursor; duplicate or earlier facts are
no-ops and cannot create duplicate projections.

## Vehicle state intervals

Intervals use two non-overlapping per-vehicle dimensions. Availability is one
of online, offline, asleep, suspended, or unknown; activity is idle, driving,
charging, or updating. This keeps TeslaMate-compatible availability history
separate from overlapping drive and charge entities. Every interval carries
source vehicle, state, start/end observation IDs and times, direct-versus-
derived provenance, and confidence; the immutable observation journal remains
the authority.

The reducer extends an equal open interval. A later changed state closes it at
the new observation time and opens a replacement in the same transaction.
Malformed or non-monotonic evidence never guesses a gap: it preserves facts
and quarantines the projection. Late facts do not mutate published history in
place; deterministic ordered replay rebuilds the derived interval set before
publication. Restart restores the open interval cursor and resumes idempotently.

## Drive lifecycle

A drive opens only on validated `D`, `R`, or `N` shift state or positive speed;
its first accepted location becomes the start point. Later valid locations
extend the same open drive. A parked/non-moving observation closes it only
after there is a recorded position. A later driving signal opens a new drive;
Hub does not merge or invent a journey across an unobserved gap. Charging wins
over inconsistent simultaneous drive signals.

TeslaMate's observed fifteen-minute drive timeout is the safe upper bound for
an unavailable open drive. Restart resumes the durable open session and its
observation cursor. A regressed timestamp, invalid coordinate, or malformed
transition cannot seal or discard the drive: it preserves the journal and
reaches safe quarantine for deterministic replay. Completed drive and attached
positions publish atomically only after the terminal transition.

## Position sampling

The observation journal retains every bounded source position payload. The
drive projection accepts a position only while a validated drive is open and
only with finite WGS84 latitude in `[-90, 90]` and longitude in `[-180, 180]`.
Invalid coordinates reject projection without deleting or altering the source
fact. Projection identity is the ordered observation, not a coordinate pair:
an exact repeat remains evidence of source receipt and is never silently
collapsed.

Hub does not interpolate, snap, or infer a path. Consecutive accepted samples
define the shown path and distance; their timestamps expose every sampling gap.
Source numeric precision remains in the journal and typed projection; any
privacy view or downsampling is a separately versioned, rebuildable output and
cannot replace the full-drive evidence.

## Charging lifecycle

A charging process opens on TeslaMate-compatible `Starting` or `Charging`
state, records its first sample, and extends with each ordered charging sample.
`Complete`, `Disconnected`, `Stopped`, and `NoPower` are terminal; sleep,
offline, updating, or a later non-charging observation closes the process only
with its last observed values. A resumed `Starting`/`Charging` state after a
terminal observation opens a new process; Hub never merges an unobserved gap.

Energy, state-of-charge, ranges, charge duration, maximum power, phase, and
DC evidence are derived from ordered samples, preserving source values rather
than inventing interpolation. A decreasing cumulative energy, regressed time,
or impossible electrical value quarantines the derived process and retains the
journal. Open state and every sample commit atomically; restart resumes it by
observation cursor, and a completed process publishes with all attached samples.

## Software update lifecycle

Raw software-update observations remain journal facts. `available`,
`downloading`, and scheduled data are pending evidence only; an `installing`
status opens one update interval at the source timestamp. Repeated installing
observations extend that interval. A return to available cancels it without a
completed update; a later non-installing observation with a valid new
`car_version` completes it. Unknown status is retained but does not invent a
transition.

Version-only observations create a zero-duration missed-update record only
when the normalized version differs from the last completed version, matching
TeslaMate's observed behavior. Completion identity is vehicle, installation
start observation, final version, and end observation; uniqueness plus the
durable cursor prevents duplicate completion after restart. A missing version,
regressed time, or contradictory final state quarantines the derived interval
while retaining raw evidence.

## Metadata and telemetry

Hub keeps each bounded, credential-safe vehicle-data map as an immutable
observation, including fields it does not yet project. Source identity,
vehicle configuration, firmware, state, drive, climate, charge, location,
range, temperature, tyre-pressure, door, and other telemetry facts therefore
remain recoverable without assuming Tesla's current schema is complete.

Typed projection keeps only fields required by the current mirror: identity and
configuration-derived model, firmware version, position, battery/range,
climate, drive, and charge values. Configuration and firmware are versioned
facts; mutable display names never overwrite source identity. Door, tyre, and
other optional telemetry remain durable journal fields until a separately
versioned consumer requires typed history. Missing or null is distinct from
false or zero, and conflicting source facts retain their provenance rather
than being guessed into one value.

Every typed metadata extension declares its source record type and field
version, is optional on read, and arrives through a forward-only schema
migration. Unknown fields survive unchanged in the journal; a newer Tesla or
TeslaMate schema cannot make old Hub data invalid. Rebuild reprojects the
retained observations with the selected extension version, while published
packs remain immutable evidence of the projection used at publication time.

## Event anomaly handling

The journal accepts each bounded source fact once, keyed by source, vehicle,
observed time, and payload hash. An exact retry is a duplicate and returns the
original fact. Same-time differing payloads are distinct conflicting evidence;
neither overwrites the other. Receipt order and source time are both retained:
the durable observation ID is the projection order, while source-time disorder
is classified rather than silently reordered.

Projection treats duplicate or already-cursored IDs as no-ops. Delayed,
out-of-order, future-dated, missing, contradictory, and malformed facts remain
in the journal with a deterministic classification. Invalid source timestamps
use receipt time only for safe storage ordering and retain the original payload;
they do not claim source-time truth. A missing fact creates an explicit gap,
never interpolated data. A contradictory fact may not close, merge, or revise
an accepted lifecycle; an unsafe effect quarantines that vehicle cursor while
preserving all facts and prior completed history.

Classification is stable from record type, payload hash, receipt order, source
time, and projection version. Rebuild reads the same ordered journal and emits
the same accepted rows, gaps, warnings, and quarantine state. Operator repair
can report or rebuild from retained facts, but cannot delete an anomaly or
clear its quarantine as a side effect.

## Address and geocoding

TeslaMate uses Nominatim reverse lookup and identifies its cache by returned
OSM type and ID, attaching the resulting address to completed drive endpoints
and charging starts. Hub preserves imported TeslaMate address labels as source
history. Compatibility collection has geocoding disabled by default: raw
coordinates stay local, and a missing address never delays durable drive or
charge completion.

An explicitly enabled Hub geocoder is a separate, asynchronous enrichment
authority. It uses a configured provider and language, validates finite WGS84
coordinates, and caches a bounded spatial request key together with provider,
language, returned stable feature identity, normalized address fields, raw
response hash, and expiry. In-flight equal keys coalesce. Provider output is
untrusted optional metadata: no result, changed result, error, or rate limit
can alter coordinates, lifecycle boundaries, or imported address history.

Enrichment retries only after a classified transient failure, honoring provider
limits and a bounded backoff. Permanent malformed or no-result replies record
an outcome and stop retrying until a changed key or explicit refresh. Each
attachment names its source position, enrichment version, and cache result;
new enrichment publishes a new immutable projection rather than editing old
history. No geocoder request, raw provider response, or address label is sent
to the phone unless that optional address view is selected.

## Geofences

TeslaMate models a geofence as a named circle with radius under five kilometres
and chooses the containing fence nearest to the point centre. Imported
TeslaMate fence names and their completed drive/charge attachments are source
history and publish unchanged. Hub local geofences are opt-in, vehicle-neutral
configuration facts; validation requires finite WGS84 centre, positive bounded
radius, stable ID, and a versioned effective time.

For a local assignment, Hub tests only the completed drive endpoint or charge
start position. Among overlapping containing circles it selects shortest
great-circle centre distance, then stable fence ID as the exact tie-breaker.
No fresh location is obtained while asleep or offline, and a last known fence
is status only, not new historical evidence. Fence matching never changes
collection cadence, wakes a vehicle, or changes a drive or charge boundary.

Create, edit, disable, and delete append a new fence version. Existing
attachments name the fence version and match input; they never change in place.
An explicit rebuild may generate a later derived-assignment revision across a
declared time range, preserving old projections and reporting every changed
attachment. Tariff fields remain separate versioned inputs for the cost model;
they do not retroactively mutate a charge or invent a cost.

## Terrain and elevation

Tesla-provided or TeslaMate-imported position elevation is source evidence and
is never replaced. For a completed drive position without it, optional terrain
enrichment uses versioned local SRTM-compatible terrain data and a bounded
on-disk tile cache. Each derived sample records source position, coordinate,
terrain dataset/version, tile hash, lookup result, and projection version.
Terrain is unavailable by default until the local dataset is provisioned.

The enrichment worker is asynchronous and replayable: it reads only completed
drive positions missing elevation, uses bounded work pages, and never delays
collection, closure, readiness, or pack publication. A lookup miss, invalid
coordinate, timeout, corrupt tile, or unavailable dataset leaves elevation
absent with a classified result. Transient tile failures use a bounded circuit
gate; no failed lookup is converted to zero or a guessed height.

Ascent and descent use ordered accepted elevation differences: sum positive
and absolute negative deltas separately. For TeslaMate parity, an absent
series, one sample, or either aggregate at the signed-smallint ceiling yields
zero rather than overflow. A later terrain dataset can produce a new derived
projection from retained source positions; old packs and their terrain
provenance remain immutable.

## Energy and efficiency

Hub retains raw charge-state values without merging their meanings: reported
battery and usable battery level, ideal, estimated, and rated range, cumulative
`charge_energy_added`, and electrical samples are distinct facts. Typed range
values are kilometres; imported TeslaMate car efficiency is stored as kWh/km
at the source boundary and exposed in the mirror as Wh/km. Null remains
unknown, never zero energy or zero efficiency.

For a completed TeslaMate-compatible charge, energy added is the final
cumulative value (or its maximum when the final value is zero) minus the first;
negative results are absent. Grid energy used is the ordered sum of nonnegative
sample intervals: charger power when phase data is absent, otherwise actual
current times voltage times determined phases, divided by 1000 and multiplied
by elapsed hours. Phase correction needs more than fifteen samples and follows
the source's stable average/rounding rules. Raw imported aggregates win over
recalculation; a missing aggregate may use this versioned fallback only.

The TeslaMate efficiency factor is `charge_energy_added / range_delta` from
completed charges longer than ten minutes, ending at or below 95% state of
charge, with positive energy and selected ideal or rated range. Hub chooses the
same most-common rounded factor through precision/confirmation tiers 5/8,
4/5, 3/3, then 2/2, and records range preference and formula version. Drive
consumption, battery-net energy, and Teslatlas-specific estimates are separate
named metrics with inputs and formula version; none may overwrite source gross
or grid energy.

## Charging costs

Imported TeslaMate `cost` is an authoritative source value with its original
numeric representation and unknown currency unless the source supplies one.
Hub never silently assigns a currency to that value. Hub-authored tariffs and
results use signed scaled decimals, ISO 4217 currency, scale, tariff version,
effective interval, geofence match version, input charge revision, and formula
version. Tariff input, calculated result, and a manual override are separate
facts; an override names an actor, reason, amount, currency, and superseded
result without erasing it.

TeslaMate parity tariffs support per-kWh, per-minute, and session fee. Per-kWh
uses the greater of non-null grid energy used and battery energy added; per
minute uses completed duration; session fee adds once. Free Supercharging makes
a Tesla fast-charger result zero. Missing every applicable tariff or required
quantity yields no calculated cost, not zero. Negative per-unit rates remain
valid source-compatible credits. Flat fees and compound Hub tariffs are later
explicit formula versions, never inferred from a geofence label.

Tariff creation, edit, expiry, and geofence reassignment append versions.
Completed sessions retain their selected tariff and result. An explicit audited
reprice creates a new result revision over a declared range; it cannot mutate
source cost, historical calculations, or manual overrides in place. Currency
conversion requires a separately captured rate source and timestamp, and is
never assumed from locale.

## External import scope

Direct read-only TeslaMate PostgreSQL migration is the required historical
import path. It is the only importer that may publish a Hub parity snapshot:
its schema, source version, selected vehicle identity, completeness checks,
and source aggregates are all reviewed and auditable. TeslaMate's own legacy
CSV importer targets TeslaFi-shaped monthly exports and reconstructs vehicle
events with timezone and ambiguous-time loss; it is not a sufficient parity
source for Hub.

Hub does not support TeslaFi CSV, tesla-apiscraper, generic CSV, or opaque
third-party exports in this release. A future importer needs a named Teslatlas
migration need, immutable input digest and provenance, bounded streaming parse,
declared timezone/DST rules, source identity mapping, field-loss inventory,
golden corpus, no-network execution, and differential reconciliation against
its source contract. It must publish as a distinct source kind and can never
masquerade as a TeslaMate migration.

## TeslaMate migration discovery

The planned `import-teslamate --discover` command is read-only against
TeslaMate. It gathers explicitly supplied Hub migration configuration, safe
file metadata, process/service/container metadata, and PostgreSQL catalog
facts; it never guesses a password, prints a secret, executes inside a
container, or changes a source file, database, service, schedule, or
container. PostgreSQL inspection starts a TLS read-only session and reports
only the reviewed migration high-water mark, required public tables/columns,
server version, selected cars, and non-secret row/count estimates. It rejects
an unavailable, untrusted, unreviewed, or non-read-only source before capture.

Discovery prints one machine-readable scope report: detected deployment shape
and evidence path/category, service-manager/container observations, redacted
endpoint and database/user identity, schema compatibility result, candidate
vehicles, credential presence without value, free-space and Hub-target
writability probe, and every unavailable or ambiguous item. A writable target
probe creates and removes only a private Hub-owned temporary file/database;
it cannot touch TeslaMate. The report includes the exact later import command
but does not run it, and an ambiguous deployment remains a failed preflight,
not a reason to choose a source silently.

Read-only host inspection is bounded to known locations and commands: regular
file reads, process listing, service-manager query, container metadata query,
and catalog SQL. `docker exec`, shelling into a container, helper containers,
Compose mutation, source credential extraction, and every TeslaMate control
action are prohibited. Credentials enter only when the owner separately
installs a Hub-owned protected read-only database credential; discovery reports
that it is present and usable, never its path contents or value.

## TeslaMate migration preflight

Preflight is a new read-only source session and a Hub-owned target check. It
must pass source reachability, TLS/loopback policy, read-only transaction
setup, selected-car existence, reviewed migration/schema interval, required
`SELECT` permissions, source clock/timestamp range, and bounded catalog/table
estimates. It records service and port observations but does not use a running
TeslaMate service as an authority to mutate anything. TeslaMate backup presence
and age are report-only because the source cannot be changed; a verified Hub
backup and restore path are mandatory before an unpublished import is allowed.

The target gates prove the Hub store is ready, the destination filesystem is
private and writable, and free bytes/inodes cover the measured durable
footprint, configured maximum stage, pack workspace, WAL/checkpoint growth,
backup window, and recovery reserve. They also record available memory and a
bounded page/worker plan whose maximum retained rows and buffers fit that
memory plan. All arithmetic is checked. Insufficient capacity, unsupported
schema, missing selected car, non-read-only privilege, target corruption,
unavailable protected credential, or unresolved clock/identity condition is a
reason-coded non-mutating failure.

Preflight estimates elapsed time from selected-car row/byte estimates and the
slowest measured read, decode/stage, projection, compression, and durable-write
rates on the same supported host class. It includes setup/finalization and
reports the model inputs. No production import starts without a representative
measurement proving the prediction below ten minutes and below the thirty
minute hard ceiling; missing or stale measurement fails the release gate.
The result is canonical JSON with a SHA-256 digest and, when the protected Hub
signing key is available, a signature. It names reference commit, source and
Hub versions, checks, estimates, limits, backup/rollback readiness, and reason
codes without secrets. A later import must bind exactly that unexpired report
digest; any source, target, configuration, or capacity change requires a new
preflight.

## Consistent TeslaMate copy

Capture begins with one PostgreSQL control connection in a TLS, read-only,
repeatable-read transaction. It validates the reviewed schema and selected car,
exports that transaction snapshot, and remains open only while worker lanes
attach. Each lane starts its own read-only repeatable-read transaction, imports
the exact exported snapshot, and streams one fixed selected-car projection
through `COPY (query) TO STDOUT (FORMAT BINARY)`. Queries, table order, and
binary field types are compiled-in and fully qualified; no user SQL, text dump,
JSON copy, raw data-file copy, temporary source object, or source write exists.

Lanes have bounded byte/row channels and typed binary decoders. They may read
independent large child tables in parallel, but parents and required lookup
tables are captured/validated before dependent projection publication. A single
Hub stage writer commits bounded pages with source ID keysets and accounting;
it cannot retain a whole table or allow an unbounded producer to outrun disk.
Every lane records snapshot ID, fixed query revision, table, source count,
bytes, first/last key, decoder result, and duration in the unpublished stage
ledger. The control transaction rolls back after workers join; it never commits
or changes TeslaMate.

The exported snapshot is one immutable source boundary even while TeslaMate
continues to write newer rows. Cancellation, worker failure, TCP loss, bad
binary field, page limit, disk pressure, or deadline failure aborts every lane,
rolls back their read-only transactions, and discards only the open private
stage. An open or partly copied stage is never resumable because its PostgreSQL
snapshot may no longer exist. Only a sealed, integrity/accounting-verified
stage can resume later Hub-only pack construction; source capture retries start
from a fresh preflight and new exported snapshot. This keeps MVCC retention
bounded by the measured under-ten-minute target and rejects any attempt that
could approach the thirty-minute ceiling.

## TeslaMate source-version compatibility

The migration adapter is versioned against the pinned TeslaMate 4.1 development
reference and accepts only an explicit reviewed migration-set digest, its
reviewed high-water interval, and the fixed structural probe. For the current
adapter, the first supported migration is `20260411070212`; the pinned source
has that same high-water mark. A source must also contain every required
reviewed migration ID and the exact required public projection tables,
columns, PostgreSQL type families, and selected-car relationships. A high-water
number alone is never compatibility proof.

Extra unrelated tables and columns are ignored only after the fixed projection
probe succeeds. Unknown or changed required columns, enum/type families,
constraints that make the fixed read impossible, missing reviewed migrations,
custom migration IDs, a newer high-water mark, or a partial/active upgrade all
fail closed with a source-version reason code. Required PostgreSQL extensions
and types are identified by catalog OID/name and version evidence; extensions
outside the selected projection are reported but do not broaden acceptance.
Unknown source enum values remain raw staged evidence and project only as an
explicit unknown state, never as a guessed TeslaMate status.

Supporting a later TeslaMate release is a new adapter revision: inspect its
pinned migration files and schema, record migration-set and extension/type
diffs, update only fixed projections/decoders, add golden and differential
fixtures, and ship the new contract alongside the old one. A staged capture
records its adapter revision and source schema fingerprint. Hub never switches
adapters during one capture, treats an incompatible source as non-mutating,
and never infers compatibility merely because a custom schema resembles a
known one.

## MQTT and integrations

MQTT, Home Assistant discovery, broker availability, and external event timing
are outside Hub's declared backend surface. Hub neither connects to a broker
nor exposes a listener, discovery payload, retained topic, or MQTT credential.
Its signed HTTP mirror and local operation journal are the only current
integration boundary.

A future optional TeslaMate-compatibility publisher must be isolated from
collection and serving. Before it may publish, golden topic fixtures must fix
the `teslamate[/namespace]/cars/{source-car-id}` naming, value encoding,
change-only behavior, retained state, non-retained `healthy` cleanup, QoS 1
acknowledgement, restart behavior, availability transitions, and ordering.
Publisher failure or a disconnected broker may delay only its own output; it
cannot affect durable collection, lifecycle projection, readiness, or phone
sync. Home Assistant discovery requires a separate versioned contract and is
not implied by topic compatibility.

## Operational observability

`/healthz` is liveness only: it reports running Hub binary version without a
database, credential, network, or source query. `/readyz` is truthful serving
readiness: schema compatibility, catalogue integrity, required local identity,
and absence of lifecycle quarantine must all pass. It returns only `ready` or
`not_ready`; detailed reasons remain local to `doctor` and structured logs.
`doctor` is the operator machine-readable diagnostic boundary and never emits
tokens, bearer headers, raw observations, coordinates, VINs, pairing secrets,
or third-party response bodies.

The future local metrics view is low-cardinality and reason-coded. It records
binary/schema/protocol versions; readiness and quarantine counts; durable
acknowledgement, collection, projection, publication, backup, and import
outcomes; retry/circuit classes; each selected vehicle's state and
last-observation age; freshness against the 5/10/75-second ceilings; stage and
pack queue/work counters; SQLite/WAL/checkpoint status; free bytes, inode and
reserve pressure; and credential mode/presence/expiry state without secrets.
Vehicle labels use stable Hub IDs only and are disabled where more vehicles
would break the configured cardinality bound.

Every event has time, component, operation ID, source kind, stable non-secret
IDs, outcome, and reason code. It may include bounded counts, durations, and
sizes, never payload values. Logs and metrics are advisory: a failed exporter,
full log sink, or unavailable metrics reader cannot delay durable writes,
readiness recovery, collection safety, or pack publication. Retention follows
the host logging policy; the Hub database is not a duplicate metrics store.

## Teslatlas data contract

The typed Hub boundary is independent from the catalogue and raw telemetry.
Its current paired full-snapshot contract, fixed projection tables, integrity
binding, parity extensions, recovery states, and one-release compatibility rule
are normative in [the data contract](DATA_CONTRACT.md). Unknown major versions
fail closed; immutable rebuild revisions never alter a previously published
phone snapshot.

## Full-snapshot synchronization

Each snapshot starts from one source-consistent boundary: owner collection has
durably acknowledged observations, while TeslaMate history uses one read-only
repeatable-read capture. Hub reserves a per-vehicle sequence before pack work;
a crash may leave a harmless unused marker but never reuses one. Open source
captures are discarded rather than resumed across a reconnect. Sealed captures
can resume bounded local pack construction without rereading TeslaMate.

The projection streams into bounded zstd SQLite chunks, capped at 512 packs,
64 MiB compressed and 256 MiB uncompressed each. Each chunk repeats its needed
selected-car and parent rows, passes SQLite/schema/foreign-key and protocol
verification, and is published once under its SHA-256 content path. The signed
manifest binds exact ordered chunks, totals, schema, installation, account,
vehicle, generation, snapshot ID, and terminal cursor. No manifest catalogue
entry exists until every referenced immutable pack is verified.

The phone verifies the exact manifest signature, then each same-origin pack's
ETag, range response, size, hash, compression, and SQLite identity. A retained
private partial may resume only the same immutable ETag tail. All chunks stage
into a fresh mirror; failure preserves the prior active mirror. Only the full
verified receipt set atomically activates the next generation. Hub retains old
published manifests and referenced packs under the no-prune policy, so a
failed build, download, or activation never withdraws the prior generation.

## Delta synchronization

Delta sync is not enabled for the current typed phone contract. The generic
transport validator reserves its shape, but Hub publishes full snapshots until
the reader and differential corpus accept the delta schema. It never labels a
fresh snapshot as incremental or asks a current reader to apply generic rows.

The future delta ledger is durable, per installation/account/vehicle/generation
and monotonically source-ordered after a projection transaction commits. Each
entry has sequence, stable mutation ID, entity identity, operation, complete
typed values or tombstone reason, input revision, and schema version. Exact
retry of a committed mutation is a no-op. Parent upserts precede children;
child tombstones precede parents. A lifecycle fact becomes eligible only after
its complete projected entity is durable, so no delta exposes an open drive or
charge as completed history.

A delta request must present the exact signed terminal cursor for the selected
identity, generation, schema, and retained base sequence. Hub serves one
contiguous `(base, head]` range or a machine-readable snapshot-required result;
it never fills a gap, rewinds a cursor, or silently changes generation. The
phone verifies every immutable pack and applies the entire range in one local
transaction with mutation IDs before advancing its cursor. Failure leaves the
old cursor and mirror active; replay is idempotent.

No delta ledger entry is compacted under the current retention policy. Before a
future retention floor advances, Hub must retain a verified full snapshot at or
above that floor plus the backup window, record the floor durably, and force a
snapshot for older or unknown cursors. Reprojection, source switch, quarantine,
or schema-major change creates a new generation and requires a full snapshot.

## Pairing and device authorization

Pairing starts only after a remote TLS listener is configured. The owner makes
a labelled invitation with a random secret and a fifteen-minute default expiry;
the Hub stores only its SHA-256 digest. The URI/QR is the one disclosure of the
secret and carries the public endpoint, invitation ID, and pinned leaf
certificate fingerprint. Before claiming, the phone establishes pinned TLS to
that identity. Claiming atomically consumes the invitation and returns one new,
opaque device bearer, whose digest alone is durable. Unknown, malformed,
expired, reused, and incorrect invitations receive the same rejection.

Each phone owns an independent device record and bearer. The current public
permission is `mirror:read` for the Hub's published vehicles; choosing a
vehicle is local phone profile state, not extra authority. A bearer gives no
Tesla token, Hub signing key, raw owner response, database handle, or access
outside the signed manifest and immutable pack routes. The bearer is
Keychain-only, redacted in diagnostics, and sent only in the TLS authorization
header. Multiple phones pair independently and do not share a credential.

Device credentials do not silently expire in the current contract: a phone
keeps working until its owner replaces or revokes it. Before multi-phone remote
release, Hub must provide owner-authorized device listing, explicit immediate
revocation, and an auditable replacement flow. Replacement creates a fresh
one-use invitation and new device bearer; the owner verifies that new phone
then revokes the old record, without affecting other phones or Tesla access.
Certificate identity changes require fresh owner-approved pairing; a phone
never follows a changed leaf identity merely because an endpoint is unchanged.
Future per-device vehicle scopes must be a durable allowlist checked on every
manifest and pack authorization, never a client-side filter.

## iPhone transfer

1. The owner creates a short-lived pairing invitation after configuring TLS.
2. Teslatlas pins the leaf certificate, claims the invitation once, and keeps
   only the paired bearer in its Keychain-backed source profile.
3. The Hub signs the exact manifest bytes with an Ed25519 key derived from the
   protected Hub cursor key.
4. The phone verifies the raw manifest signature, downloads content-addressed
   zstd SQLite packs over same-origin HTTP/2, and resumes an interrupted tail
   only when `ETag`, `Content-Range`, size, and final SHA-256 all agree.
5. Rust stages every pack into a fresh private SQLite file. The live local
   mirror swaps only after the full signed receipt set seals.

The phone never receives Hub credentials, raw owner responses, PostgreSQL
credentials, or a remote SQLite database handle.

## Mirror replacement failure rules

The active Teslatlas mirror is immutable from the perspective of a refresh.
The phone fetches and verifies the signed manifest before work, keeps at most
two content-addressed pack downloads in flight, and writes only to private
cache and per-attempt stage paths. A partial download resumes only its exact
content hash and byte offset with a matching range response; a complete
partial is rehashed before publication. Bad length, ETag/range, compression,
SQLite, receipt, signature, identity, or cursor binding rejects the attempt.
Corrupt cache entries are never appended to or activated.

Interruption, retry, cancellation, and duplicate refreshes do not change the
active mirror. Per-content leases serialize writers, verified immutable cached
packs may be reused, and every new import uses a new private stage database.
On any failed or cancelled attempt the stage and sidecars are removed while a
safe resumable pack prefix remains available. Exhausted disk space or any
filesystem error has the same result: no activation, no cursor advance, and a
retry only after space is available. The bounded pack and two-download limits
also prevent a manifest from creating unbounded temporary download pressure.

Rust accepts ordered receipts only, verifies the entire receipt set against the
same signed manifest, and swaps the live SQLite mirror only after finalization.
No partial stage, duplicate pack, or incomplete receipt can become visible.
A generation, source identity, schema, or selected-vehicle mismatch is a new
mirror identity, not a merge target; the old mirror stays live until a complete
matching snapshot succeeds. Delta mode remains disabled. When it is enabled,
an absent, invalid, stale, or source-swapped cursor must return
snapshot-required and follow this same full replacement path.

## Background mirror refresh

Background refresh is cache maintenance, never a freshness promise. Hub states
the source observation age; the phone reports its last successful signed
generation and does not imply that iOS has run a task. The planned iOS task is
a best-effort network-and-external-power processing task, anchored at 02:00
local time after two idle hours. It fetches the manifest first and performs the
same cursor-aware, atomic full replacement as foreground. A foreground launch
always checks again; it is the recovery path if background execution was
deferred, expired, cancelled, offline, or power constrained.

One paired profile permits one refresh pipeline at a time. It consumes an
expiration signal by cancelling and joining downloads and staging before
reporting failure, leaving the old mirror readable. Transient failures back off
at 5 minutes, 15 minutes, 1 hour, 3 hours, 6 hours, then 12 hours; persistent
failures use 15 minutes, 1 hour, 3 hours, 6 hours, 12 hours, then 24 hours.
The attempt state resets on success or after six quiet hours. Authentication,
TLS identity, manifest-signature, schema, identity, and authorization failures
are persistent and require visible foreground repair; transport, temporary
HTTP, and task-expiry failures are transient.

Before staging, the phone calculates the manifest's remaining compressed
downloads plus the declared decompressed/stage requirement and refuses work
when that capacity is unavailable. It evicts only inactive verified
content-addressed cache packs, never the active mirror, an in-flight pack, or
a resumable partial selected for the current retry. On storage pressure it
first deletes stale partials and old unreferenced packs, then reports a
recoverable low-storage state. Every retry and foreground recovery starts from
the manifest, so cache eviction cannot move a cursor or expose a mixed mirror.

## Source provenance and switching

Hub does not equate histories by display name, local row number, or VIN alone.
Every fact names a stable non-secret `(source kind, source key)` and every
vehicle is scoped by `(source ID, source vehicle key)`. TeslaMate vehicle UUIDs
derive from that source namespace and VIN/EID, while Hub manifests bind the
installation, account, vehicle, generation, and selected mirror car. Thus two
sources may legitimately report equal-looking vehicles or row IDs without
colliding, and every pack, cursor, provenance record, and export remains
attributable to its actual source.

Changing source is an explicit owner action, never a background deduction.
The phone first retains the active source profile and mirror, cancels and joins
its refresh work, validates the candidate's pinned identity and selected
vehicle, then stages a complete candidate generation. Only successful signed
finalization atomically activates the candidate profile and local mirror; a
failure or cancellation leaves the old source selected and readable. A source
switch starts a new generation and full snapshot and invalidates all prior
delta cursors for that local mirror.

No switch silently merges raw histories or overwrites their provenance.
The old source is retained as an inactive profile/history until an explicit
owner removal, and rollback means selecting it and completing its own verified
refresh. Hub, TeslaMate, Fleet, and future imports must each present an
independent durable source identity. Any request to reconcile them is a later,
explicit, auditable user-directed operation with conflict output; it is never
part of ordinary sync.

## Host deployment shape

The package starts loopback-only. Remote phone use is an opt-in direct TLS
listener with a public HTTPS origin. The TLS certificate private key remains
host-local; the pairing URI contains only endpoint, one-use pairing secret,
pairing ID and leaf-certificate fingerprint.

## Process topology

The always-on `teslatlas-hub.service` owns serving, readiness, and the Hub
SQLite store. It is a single supervised Rust process; in-process work remains
bounded and must not make readiness claim success while durable state is
unusable.

Collection and TeslaMate history capture are separate, explicitly started
systemd oneshot jobs. They share the Hub store but are isolated from the
network listener, receive only their necessary credentials, and can fail or be
cancelled without killing the serving process. Collection never runs by timer
or implicit background start. Migration remains a manual, read-only source
operation. Repair is an explicit Hub command, never automatic destructive
maintenance.

The topology deliberately does not introduce a worker daemon, queue broker, or
second writable store. SQLite transactions are the cross-process serialization
boundary. A future always-on collector may be added only after fault-injection
proves bounded, truthful readiness and no-loss replay under concurrent serving;
until then it remains opt-in and separately supervised.

## Side-by-side Hub startup

Side-by-side startup installs and starts only Hub-owned package files, units,
state, credentials, SQLite, packs, TLS identity, and listener. Before start it
proves that its configured storage is private and distinct, its unit names do
not overlap, its listener endpoint is available, and no TeslaMate database,
port, credential path, container, Compose file, service, timer, or schedule is
selected for mutation. A port collision, shared path, missing Hub identity, or
unready Hub store fails the Hub start without touching TeslaMate.

The serving unit may run first on its isolated loopback/TLS endpoint and expose
only Hub readiness and paired snapshot routes. The manual TeslaMate import job
runs only after a passing preflight and writes only the Hub stage/catalogue.
It is isolated from the serving process except for SQLite transaction locking;
its cancellation or failure leaves the prior Hub manifest and TeslaMate service
available. Package installation and serving do not imply remote exposure,
pairing, credential handoff, collection, Fleet activation, or source switch.

During the side-by-side phase, Hub records `migration-only` collection
authority for every imported vehicle. The collector command and any supervised
collector refuse those vehicles even if a Hub candidate credential exists.
Startup evidence records Hub binary/config identity, filesystem/device IDs,
listener binding, readiness, imported source identity, manifest state, and
explicitly absent Hub collection authority, all without secrets. TeslaMate
remains the unchanged operational source until later owner-controlled
verification and cutover steps.

## Persistence boundary

Hub's sole canonical writable store is Rust-owned SQLite. It uses the vendored
engine with WAL, full synchronous commits, foreign keys, an application ID,
and integrity checks. The store owns observations, projections, pairing state,
manifest catalog, and local repair metadata; immutable transport packs are
derived objects, not a second authority.

TeslaMate PostgreSQL stays an external, read-only migration source. Hub does
not expose TeslaMate SQL compatibility: its database schema, Phoenix API,
Grafana queries, MQTT, and web consumers are outside the declared surface.
The typed Teslatlas contract is the only compatibility projection. SQLite stays
the choice subject to the later durability, repair, concurrent-access, and
representative-corpus performance gates; failure of any gate requires a new
persistence decision, not a silent PostgreSQL dependency.

## Observation journal

Before collection is acknowledged, Hub appends one bounded immutable source
fact per vehicle response. A fact contains the stable source and vehicle
identities, source observation time, Hub receipt time, canonical payload hash,
record type, source vehicle state, and the complete normalized source payload.
Retries with the same identity, observation time, and payload hash resolve to
the original fact; updates and deletion are rejected.

Projection consumes facts in `(observed_at_ms, observation_id)` order and
stores its last durable observation cursor with open lifecycle state. It never
contacts Tesla during replay. Missing or malformed legacy state is treated as
`unknown`, preserving the raw fact while preventing a false lifecycle claim.
Derived history, manifests, and packs are replaceable outputs; the journal is
the authority for future deterministic rebuild and repair work.

## Identity and ordering

Each source is a stable non-secret `(kind, key)` identity. Each source-owned
vehicle is keyed by `(source_id, source_vehicle_key)`; TeslaMate vehicles also
derive the Hub UUID from that pair, using VIN first and Tesla EID only when VIN
is absent. A journal fact is deduplicated by source, vehicle, observation time,
and canonical payload hash, then replayed by `(observed_at_ms, observation_id)`.

Open lifecycle state advances only its durable observation ID, so a retry or
restart cannot emit the same projected fact twice. Each vehicle reserves a
durable monotonic full-snapshot sequence before pack construction. Failed
unpublished builds may leave a sequence gap; full snapshots are complete
replacements, so the latest successfully published head is authoritative.
Future incremental sync must reject any gap or overlap against its explicit
base sequence.

## Time, units, and precision

All Hub and transport times are non-negative Unix epoch integer milliseconds in
UTC. PostgreSQL reads inside a UTC session and normalizes its timestamps to
that precision; the frozen Teslatlas projection contract deliberately does not
carry a timezone or sub-millisecond field. Source receipt time is separate from
source observation time. No local timezone, display formatting, or rounding
participates in ordering, identity, or lifecycle decisions.

The canonical projection uses kilometres for distance and range, kilometres per
hour for charging rate, kilowatts for power, kilowatt-hours for energy, degrees
Celsius, percent SOC, WGS84 decimal degrees, and minutes only for TeslaMate's
stored duration field. Compatibility owner responses convert mile-valued range
and odometer values once with the exact `1.609344` factor. Continuous values
remain finite IEEE-754 binary64 values because the current projection adapter
and transport use float/REAL; Hub performs no display rounding. Any future
financial or tariff value needs an explicit scaled-decimal contract before it
can enter parity calculations.

## Core data model

`Source` owns many `Vehicle` identities. An owner-API `Observation` is the
immutable fact stream for one vehicle. `VehicleLifecycleState` is replaceable
open-session projection state. A TeslaMate capture is a read-only source
snapshot containing one selected `Car`, its `StateInterval`s, `Drive`s and
attached `Position`s, `ChargingProcess`es and attached `ChargeSample`s,
`SoftwareUpdate`s, plus shared `Address` and `Geofence` references. Effective
vehicle and global settings are versioned outcome values, never copied settings
tables or control surfaces.

Relations preserve source IDs until projection: drive endpoints reference
positions, addresses, and geofences; positions reference their drive; charge
samples reference their charging process; charging processes reference their
start position, address, and geofence; states, updates, and settings reference
their vehicle. Incomplete drives and charging processes remain open facts, not
completed records. The phone projection is intentionally smaller: one car plus
completed drives, attached positions, completed charges, and charge samples,
with address and geofence names flattened only at that compatibility boundary.

## Derived calculations

For TeslaMate migration, completed source aggregates are authoritative: drive
distance, duration, endpoint ranges, temperatures, speed, elevation, energy,
and charge aggregates are copied from the pinned source when present rather
than recomputed by Hub. Sample-derived fallback is allowed only for a missing
source aggregate: charge energy is last minus first cumulative sample, and
sample boundaries use stable `(timestamp, source_id)` order. Negative or
non-finite fallback results are absent, never coerced to zero.

The compatibility collector is not a replacement TeslaMate calculator. Its
provisional lifecycle path uses UTC elapsed minutes, arithmetic temperature
mean, maximum observed charger power, and great-circle position distance only
until the later drive, charging, terrain, efficiency, and cost parity tickets
provide golden TeslaMate fixtures. Costs, tariff, energy-used integration,
efficiency inference, address/geofence lookup, and elevation enrichment must
not claim TeslaMate parity before those versioned contracts and fixtures exist.

## TeslaMate migration transformation

The versioned field-level mapping is normative in
`docs/TESLAMATE_MIGRATION_MAPPING.md`. Fixed binary decoders capture every
selected source field into typed immutable evidence before mapping it to the
Hub provenance model, parity extension, or current typed mirror. Each mapping
declares source/destination field, identity and relationship rule, unit,
timestamp precision, nullable behavior, enum behavior, and intentional loss.
Unlisted or privacy-excluded fields are counted as named loss; no coercion,
display rounding, unit guess, timezone reinterpretation, or relationship repair
is silent.

Large destination work uses bounded prepared bulk transactions in the private
stage. Required identities and parent references validate first; child batches
then validate source IDs, selected-car ownership, ordering, finite values, and
completed-history eligibility. Nonessential indexes wait until the fastest
integrity-safe point after bulk load; required uniqueness and relationship
checks exist during ingest. The stage records adapter/mapping revision, source
schema fingerprint, table counts, canonical row hashes, accepted/rejected/loss
counts, and exact reason codes before it can seal or publish.

## TeslaMate source validation

Validation has three layers. Before copying, fixed read-only catalog and
aggregate queries prove schema, selected-car scope, primary-key/count shape,
permissions, and estimated relationships. During typed COPY, each row proves
its decoded type, ID domain, nullability, selected-car ownership, timestamp,
finite numeric value, SOC/range/energy bound, and WGS84 coordinates. After
capture, set-based source checks and bounded stage queries prove parent/child
existence, no duplicate IDs, no unexpected orphan, chronological keys,
interval direction, lifecycle completeness, aggregate/count/hash agreement,
and complete pack foreign-key/integrity validation.

The relation checks cover drive endpoints to positions/addresses/geofences,
position-to-drive ownership, charging-process references, charge samples to
their process, state/update-to-car ownership, and every selected-car boundary.
Temporal checks cover nonnegative UTC milliseconds, end not before start,
stable `(time,id)` ordering, complete-interval eligibility, and conflicting
state/drive/charge intervals. Aggregate checks compare source table counts and
canonical per-table hashes to the stage, then compare projected row counts and
source-authoritative drive/charge aggregates to their named mapping outcome;
they never replace a TeslaMate aggregate with a silent Hub calculation.

Every anomaly is classified in the sealed validation report. Fatal means
schema/identity corruption, required-reference loss, duplicate source identity,
unsafe time/value/coordinate, count/hash mismatch, or impossible completed
lifecycle; publication is forbidden. Repairable means a Hub-only decoder,
mapping, or staging defect with preserved source evidence; the stage remains
unpublished for corrected-adapter replay and TeslaMate is untouched. Accepted
with evidence means known source-compatible incompleteness such as an open
drive/charge, unattached position, optional null, or deliberately excluded
privacy field; it is counted with source IDs/reason and omitted only from the
affected projection. No class disappears from the report, and no validation
step repairs, writes, or reconfigures TeslaMate.

## TeslaMate destination reconciliation

The normative reconciliation matrix is
`docs/MIGRATION_RECONCILIATION.md`. Source COPY lanes compute table count,
first/last key, canonical ordered keyed hash, relationship summary, time range,
aggregate, spatial summary, and deterministic deep-sample keys while streaming
the exported snapshot. The stage writer computes the same summaries as it
receives typed rows, so proof needs no second source-wide serial parse. It then
compares source evidence to stage evidence, mapping/loss ledger, projection
report, verified packs, and signed manifest identity/sequence/cursor binding.

Exact keyed hashes prove retained source-row equivalence; aggregate and
relationship comparisons prove transformation shape; deterministic samples
compare every mapped field, null, enum, parent chain, timestamp, and coordinate
value. Boundary rows are always sampled. Every intentional omission must match
one named mapping-loss or accepted-anomaly record. Any other count, key, hash,
relationship, lifecycle, range, spatial, aggregate, or sampled-value difference
is fatal and leaves the stage unpublished. Reconciliation records zero
unexplained differences only when every observed difference has an explicit,
versioned source-compatible reason.

## Schema evolution

Hub catalogue migrations are ordered, monotonic SQLite transactions. Startup
accepts only the known exact schema version or applies each next migration in
one `BEGIN IMMEDIATE` transaction; a failed migration rolls back that step and
Hub stays not ready. Migrations never rewrite, delete, or mutate TeslaMate.
Any destructive or large rewrite requires preflight integrity and free-space
checks, a verified Hub backup, resumable copy into new structures, and an
operator-visible restore path. There are no down migrations: rollback restores
the pre-upgrade Hub backup with its matching binary.

The Hub catalogue version, transport pack schema, and wire protocol are
separate. Pack schema is encoded in both SQLite `application_id`/`user_version`
and the signed manifest. Compatible extensions increase a minor version only
when old readers can safely ignore them; incompatible changes require a new
major version, explicit client capability, and a supported transition path.
Unknown versions fail closed before pack work or mirror activation.

## Database invariants

SQLite enforces source and vehicle ownership, stable source/vehicle uniqueness,
positive IDs and generations, non-negative times, bounded JSON payloads and
secrets, unique content-aware observation identity, append-only observations,
single-use pairing, token-hash uniqueness, and positive snapshot reservation.
Foreign keys are enabled on every connection. A source/vehicle mismatch or an
observation update/delete aborts in the database, not only in application code.

Projection commits atomically couple completed entities with the durable
lifecycle cursor. Pack construction separately validates required parents,
single selected-car ownership, unique IDs, chronological intervals, finite
numeric values, SOC bounds, and WGS84 coordinates before its SQLite integrity
check and publication. Store validation repeats critical checks with named
errors; `quick_check`, full stage/pack integrity checks, and later reconciliation
are readiness gates, never advisory diagnostics.

## Query and index contract

Online paths are bounded point or range queries only: source identity,
source-owned vehicle identity, pairing-token lookup, latest manifest per
vehicle, pack digest, lifecycle state, and journal replay pages. Their indexes
are respectively the source/vehicle unique keys, token and primary keys,
`(vehicle_id, head_sequence DESC)`, pack digest, lifecycle vehicle key, and
`(vehicle_id, observed_at_ms, observation_id)`. Materialized history uses its
vehicle-and-row-ID primary keys. TeslaMate staging pages by `(table_name,
source_id)`, with a partial charge-sample-by-process index for parent assembly.

The network server has no arbitrary historical query endpoint. Full catalogue
or pack-reference scans are repair and audit work only, never request paths.
Every new index needs a named workload, selectivity rationale, write-cost
measurement, and representative-corpus `EXPLAIN QUERY PLAN` proof: online
paths must avoid unbounded scans and temporary sort structures; migration pages
must stay keyset-bounded. Final timing and memory ceilings remain performance
gate work, not assumptions in this design.

## Durability policy

The Hub catalogue uses WAL with `synchronous=FULL`; every acknowledged journal
append, lifecycle delta, pairing claim, sequence reservation, and manifest
catalogue update commits in an immediate SQLite transaction. A collection may
report a persisted observation only after its journal transaction commits.
Projection publication writes and verifies the complete temporary SQLite/zstd
pack, `fsync`s it, links it immutably into the content directory, `fsync`s that
directory, then commits the manifest catalogue. A crash before the catalogue
commit leaves at most an unreferenced verified pack; a crash after it leaves a
durable referenced pack.

Migration staging uses full synchronous SQLite page transactions and seals only
after full integrity/accounting checks. No adaptive or performance profile may
weaken observation, lifecycle, manifest, or stage durability. WAL checkpoint
tuning is deferred until its measured crash, write-latency, and recovery proof;
the default remains SQLite-managed.

## Retention and compaction

Hub retains canonical source identity, immutable observations, lifecycle
history, published manifests, referenced packs, and sealed migration evidence
by default. It performs no automatic journal deletion, history compaction,
`VACUUM`, pack pruning, or cursor-floor advance. A complete rebuild therefore
remains possible from retained Hub facts without contacting Tesla, subject to
the later backup and restore proof.

Repair may remove only an unreferenced immutable pack after catalogue and
integrity checks; temporary build files may disappear before publication. A
sealed migration stage remains until publication and its later audit/recovery
rules complete. Current transfer is full-snapshot only, so no client cursor can
be stranded by retention. Before delta sync or any operator-selected retention
policy, Hub must publish a replacement full snapshot, record a durable recovery
floor, retain everything above it and the required backup window, and refuse a
cursor below it with explicit snapshot recovery.

## Capacity preflight

Before a migration writes Hub data, preflight records source table counts and
byte estimates, current Hub database/WAL/packs/stages/backups, free bytes and
filesystem type, available inodes, and configured recovery reserve. Required
space is the measured current durable footprint plus the bounded unpublished
stage, pack build workspace, worst-case WAL/checkpoint growth, retained backup
window, and reserve; every addition is overflow-checked. It fails closed if the
same local filesystem cannot satisfy that demand or its inode floor.

The stage currently enforces an explicit maximum allocation plus untouched
free-space reserve before capture and rejects row/byte limits during each page.
Those defaults are safety caps, not evidence for the representative
ten-million-row migration. Corpus measurement must set the production stage,
pack, WAL, backup, and inode budgets before that release gate can pass.

## Integrity and repair

Readiness runs a fast SQLite `quick_check` plus the lifecycle quarantine gate.
`doctor` additionally verifies every currently referenced immutable pack's
canonical path, regular-file type, byte count, and SHA-256 digest. Sealed
migration stages and generated packs receive full SQLite integrity/accounting
verification before publication. Semantic failures quarantine lifecycle state
and preserve the immutable journal; they never silently discard observations or
make a damaged cursor healthy.

Safe repair is limited to detection/reporting, retaining quarantine evidence,
and removing verified unreferenced packs. Journal-based deterministic rebuild is
the future safe reconstruction path and needs no Tesla recontact. A corrupt
catalogue, referenced pack, or sealed stage makes Hub not ready and requires
offline restore/rebuild from a verified backup; no automatic destructive repair
or source mutation is permitted. Every repair/recovery action emits a
machine-readable report; backup precedes any irreversible operation.

## Backup and restore

A backup generation is a versioned manifest plus a consistent SQLite catalogue
image, every manifest-referenced immutable pack, sealed migration evidence
needed for audit/recovery, and hashes/sizes for every member. The manifest binds
Hub binary revision, catalogue schema, protocol/pack schemas, installation ID,
generation timestamp, and required recovery floor. The catalogue image must use
SQLite backup/checkpoint-safe capture, never a blind copy of live WAL files.

Systemd host-encrypted credential blobs are deliberately not portable backup
material. Cross-host recovery requires separately escrowed, backup-recipient
encrypted Hub identity/key material or explicit re-pairing and credential
provisioning; TeslaMate credentials are never copied. Restore first verifies
manifest signatures and every hash into a fresh private directory, runs
integrity/schema checks while offline, atomically selects the complete
generation, then proves `doctor`, manifest verification, and a fresh paired
sync. The clean-host drill is a release gate; no backup claim exists before it.

## Startup reconciliation

Startup first applies only transactional Hub schema migrations, then checks the
catalogue and durable operation state before serving. A healthy committed
journal/lifecycle cursor resumes idempotently from its next fact; a partial pack
without manifest is unreferenced cleanup; a manifest never activates until its
verified pack exists. Open migration stages are not source-consistent and stay
unpublished until explicitly resumed or rebuilt; sealed stages remain evidence.

Readiness fails on SQLite catalogue corruption, unsupported schema, missing
required credentials for the chosen listener, or any quarantined lifecycle
session. `doctor` also fails on a missing, malformed, resized, or digest-mismatched
referenced pack. Neither path clears quarantine or recontacts Tesla. Future
operation-state records must cover capture, projection, publication, backup,
and credential rotation; each either resumes deterministically or reaches a
machine-readable safe quarantine before readiness can succeed.

The release path is detached-manifest signed with an independently pinned
Minisign public key. Development bootstrap pins a reviewed Git object. Both
paths build or install native Debian packages and use systemd encrypted
credentials; neither puts a Tesla token in configuration, argv, environment,
or the Hub database.

## Intentional limits

- No implicit production API endpoint or background legacy polling.
- No vehicle wake, command, or charging-control capability in the token path.
- No partial phone mirror activation.
- No data deletion during package removal or upgrade.
- No performance claim without a measured Pi/VPS-class benchmark.
