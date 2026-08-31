# Concurrent Home Assistant foundation merge design

## Status

Draft for user review. The core merge direction was approved in chat on
2026-08-31; this written specification also incorporates protocol-review
corrections that require review before implementation planning. It reconciles
the executable Home Assistant foundation on GitHub with the independently
written local security/runtime plan and concurrent public protocol work.

The implementation base is `teslatlas-home-assistant` commit `55d7024`. The
second local repository contains no competing implementation: it remains at
`0c14212` with one untracked design plan. That plan is evidence for design
decisions, not a branch to merge or a source of executable code.

The protocol baseline is `teslatlas-protocol` commit `79ced4c`, profile `1.2.0`.
Its discovery, query, current-state, problem-details, and SSE artifacts are now
public and executable. Its own documentation explicitly leaves pairing claim,
credential rotation, and revocation to deployment contracts. It contains no
mDNS/Zeroconf contract and makes no claim that a released Hub deploys profile
`1.2.0`.

The public Hub repository remains at `v1.0.0-beta.1` commit `43d7e96`. Its API
guide documents a pairing route but warns that protocol and response formats
may change during beta; it publishes no pairing schemas, mDNS contract, or SSE
route. That documentation is not sufficient to close a deployment gate.

## Goal

Produce one honest, secure `main` that retains the working Home Assistant
foundation, adopts the stronger local-plan constraints, aligns every modeled
field and event rule with released protocol `1.2.0`, and stays fail-closed at
the network boundary until the missing deployment/synchronization contracts and
live Hub proof exist.

This merge does not publish through HACS, create a release, add hosted
automation, implement commands, access Hub storage, import Hub source, request
Tesla credentials, poll Tesla, or touch the proprietary app.

## Source selection

### Retain from `55d7024`

- Domain `teslatlas_hub` and single-integration HACS-shaped layout.
- Typed config-entry runtime data and Home Assistant platform forwarding.
- Immutable owned snapshot values.
- One config-entry-owned event task with clean cancellation and close.
- `update_interval=None`; no healthy-state request polling.
- Stable Hub and vehicle device identifiers, with explicit entity-registry
  migration for retained, changed, and removed semantics.
- Transient password-style pairing input and same-entry reauthentication shape.
- Reconfiguration identity guard shape.
- Read-only entities, isolated `unknown`, and disconnect `unavailable` behavior.
- Deterministic pytest/Home Assistant harness, Ruff, lockfile, translations,
  migration tests, and privacy tests.
- Fail-closed production-client factory.

### Adopt from the local plan

- HTTPS for every non-loopback origin.
- Never send a bearer or pairing secret to an unverified replacement origin.
- Treat public Hub identity as registry identity, not cryptographic origin
  authentication.
- Validate authenticated read access before a future config entry is created.
- Reconcile state atomically on an explicit continuity failure.
- Validate identity, negotiated protocol, event type, resource identity, and
  revision before publication.
- Allow-list diagnostics and support output instead of serializing raw runtime
  or config dictionaries.
- Use classified safe errors; never surface response bodies, endpoint data,
  request IDs, headers, or raw exception strings.
- Contract-drive metric existence, units, enums, device classes, state classes,
  availability, and diagnostic defaults.
- Add stale-device and per-vehicle availability rules before transport ships.

### Reject from the local plan

- Domain rename to `teslatlas`. No released installation needs migration, but
  the rename adds churn without a contract or product benefit.
- Wholesale copy of the untracked plan. Its baseline and several hard blockers
  predate protocol `1.2.0`.
- Comparing or ordering opaque SSE event IDs. Protocol `1.2.0` forbids it.
- Mandatory snapshot reconciliation after every ordinary reconnect. A resumed
  stream with an accepted `Last-Event-ID` retains continuity. Invalid, expired,
  reset, or absent checkpoints require the synchronized resynchronization gate;
  malformed known events fail terminally.
- A concrete pairing request/response shape, TLS pin format, QR encoding, or
  mDNS service/TXT layout before those deployment contracts are released.
- A 300-second SSE retry cap. Protocol `1.2.0` caps a server-supplied retry at
  30 seconds; the integration will preserve that bound.

## Contract and deployment gates

The old single `protocol_not_ready` gate becomes five explicit gates.

1. **Released representation, pagination, and replay artifacts:** closed by
   protocol `1.2.0`. Production models, bounded parsers, fixtures, and individual
   query/event semantics may target these artifacts.
2. **Pairing and origin-binding deployment contract:** open. The UI pairing,
   reauthentication, and endpoint migration paths remain fail-closed until a
   machine-readable, versioned contract defines claim input/output, bearer
   scope/lifecycle, stable errors, and origin binding.
3. **Zeroconf deployment contract:** open. The manifest must not advertise a
   service type, and discovery code must not mutate entries, until a versioned
   service/TXT/TLS/identity contract exists.
4. **Snapshot/stream continuity and vehicle lifecycle:** open. Protocol `1.2.0`
   supplies neither a query snapshot checkpoint tied to the SSE stream nor a
   catalog-change event. Query-then-stream can miss a change, while
   stream-then-query cannot order buffered events against the query snapshot.
   A versioned synchronization primitive must close that race. Vehicle
   retirement/rename detection additionally needs a contracted lifecycle event
   or an explicitly bounded list-reconciliation policy. The integration never
   infers cross-surface order from resource revision, `observed_at`, list
   `snapshot_revision`, or opaque event IDs.
5. **Runtime and publication proof:** open. Enabling the real network client
   requires a fresh released Hub that implements the selected protocol profile
   and passes integration conformance. HACS publication separately requires
   brand approval, required repository topics, release packaging, clean-install
   proof, and then-current validation.

The production factory continues returning a pending adapter. Its error text
must say that Hub deployment/pairing and synchronized-runtime readiness are
missing, not that the published query/event representations are unfrozen.

## Protocol-aligned model

Temporary fixture semantics are replaced with values supported by protocol
profile `1.2.0`.

### Hub discovery and negotiation

Discovery supplies version candidates; it does not negotiate a version. On
every setup, the client revalidates discovery and selects profile `1.2.0` using
the advertised current, supported, and minimum versions. It sends that version
on the first authenticated query and marks it negotiated only after the
response repeats the selected version. The SSE response must independently
confirm the same version before any event is dispatched. A prior config or
runtime value is never trusted as fresh negotiation evidence. Every subsequent
versioned request sends the selected header and every corresponding response,
including each vehicle page and current-resource response, must repeat the same
selection; mismatch is terminal before candidate commit.

The validated runtime snapshot stores only:

- opaque stable `hub_id`;
- negotiated protocol version;
- stable capability IDs and versions;
- contract-declared API and event origins after same-origin/TLS validation;
- contract-declared limits needed by the client.

Runtime requires exactly one descriptor each for the surfaces it uses:
`query.vehicles` and `events.sse`, at capability versions the client explicitly
supports. Stable descriptors are accepted. Deprecated descriptors are accepted
only when their semantics remain in the client allow-list and their required
deprecation metadata has no effective sunset; experimental descriptors are
rejected. Each relative `href` must resolve to the corresponding validated API
or event endpoint. Missing, duplicate, conflicting, unsupported, or
origin-inconsistent descriptors fail closed before authentication or query use.
The optional `data-quality` descriptor authorizes its query endpoint, which this
integration does not call; required nested current-state quality and catalogued
quality events remain part of the two used surfaces.

Protocol discovery has no safe Hub display-name field. Entry and device titles
therefore use the fixed translated name `Teslatlas Hub` until a safe label is
contracted. Config-entry migration rewrites any temporary legacy Hub title and
this entry's Hub-device name to that fixed translation; no `HubInfo.name` value
is preserved.

### Vehicle summary and current state

The initial authoritative projection comes from the complete vehicle listing
and each vehicle's current-state resource. Modeled values are limited to:

- opaque vehicle ID and safe display name from the vehicle resource;
- current resource revision and observation timestamp;
- vehicle state from `current_state.state`; the vehicle-list copy is validation
  and catalog metadata only;
- battery level percentage;
- estimated range in kilometers;
- odometer in kilometers;
- inside and outside temperature in Celsius;
- locked boolean;
- climate-on boolean;
- charging state;
- public data-quality classification.

Charging power, charge limit, software version/state, telemetry-age duration,
collector health, Fleet cost, and backup age are absent from current-state
profile `1.2.0`. They must be removed from the active entity model rather than
left as permanently unknown placeholders. A later protocol profile can add
them through a versioned migration.

The odometer has no statistics state class until the contract guarantees the
required monotonic/reset semantics. Diagnostic data-quality and observation
timestamp entities are disabled by default. `locked` uses Home Assistant's lock
binary-sensor class with `is_on = not locked`, because that class defines on as
unlocked; both boolean polarities and `null -> unknown` are tested. Climate-on
is a direct read-only binary sensor. Neither becomes a command or lock/climate
control.

Setup creates the Hub parent explicitly in the device registry with identifier
`(DOMAIN, hub_id)`, even though no Hub-level sensor remains. Every vehicle device
uses that identifier as `via_device`. A synthetic Hub-status sensor is not added
merely to create the parent.

### Entity-registry migration

The integration has not been released, but manual installations may exist.
Preserve the existing unique-ID suffix only when the meaning is unchanged:

| Existing suffix | Protocol `1.2.0` source | Action |
| --- | --- | --- |
| `state_of_charge` | `battery_level_percent` | Keep |
| `charging_state` | `charging_state` | Keep |
| `estimated_range` | `range_km` | Keep |
| `odometer` | `odometer_km` | Keep; remove old state class |
| `activity_state` | `state` | Keep |
| `inside_temperature` | `inside_temperature_c` | Keep |
| `outside_temperature` | `outside_temperature_c` | Keep |
| `data_quality` | `quality.quality` | Keep; disable by default |

`access_state` is not preserved: remove that integration-owned legacy sensor
entry and create a new lock binary sensor with suffix `locked`. Create new
`climate_on` and `observation_timestamp` suffixes. Remove only this config
entry's integration-owned legacy entries for charging power, charge limit,
software fields, telemetry age, and all three unsupported Hub sensors. A
preexisting-registry migration test proves the exact mapping, platform change,
config-entry/Hub-device title rewrite using a sentinel legacy name, and isolation
from unrelated registry entries and devices.

## Runtime state machine

The coordinator remains the Home Assistant-facing owner, but stream lifecycle
becomes explicit.

### Synchronized startup

1. Validate discovery, version, capability, origin, and authenticated access
   through the client boundary.
2. Traverse every vehicle-list page with the same endpoint, normalized filters,
   principal, and negotiated version. Cursors remain opaque. Reject cursor
   loops, repeated vehicle IDs, or a changed `snapshot_revision`; any page
   failure aborts the traversal without publishing or retiring devices.
3. Fetch and validate one current-state resource for every listed vehicle,
   respecting the contracted concurrency limit and rechecking the exact selected
   response version. Any required-resource failure or mismatch aborts the whole
   candidate projection.
4. Use a future contract-defined checkpoint or barrier to prove that the event
   stream begins strictly after the candidate projection.
5. Atomically commit and publish the immutable projection with its bound replay
   checkpoint, then start exactly one stream bound to the same Hub identity,
   principal, filters, and negotiated protocol version.

Every page repeats the negotiated version and validates against its exact
schema. Follow `next_cursor` verbatim with the original query until it is
`null`, enforcing local page/item bounds. `cursor_expired` discards the private
candidate and restarts at page one. A scope-changed cursor does the same under
the still-valid credential and new scope; only a classified authentication
failure starts reauthentication. Both restart causes share one bounded
attempt/time budget, cancellable backoff, and unload cancellation. Budget
exhaustion aborts the private candidate and stays unavailable. An invalid or
query-mismatched cursor is protocol failure. No partial traversal can update
entities or drive stale-device removal.

The existing query and SSE contracts cannot implement step 4 race-free. The
pending production adapter therefore prevents synchronized startup until that
gate closes. Fixture clients may supply an explicitly atomic session boundary
to exercise Home Assistant lifecycle behavior, but that is test scaffolding,
not live transport or protocol proof.

### Bounded JSON responses

Before UTF-8 or JSON decoding, the client streams every response through a hard
byte budget: 2 MiB for discovery or vehicle pages, 256 KiB for one current-state
resource, and 64 KiB for problem details. A declared `Content-Length` above the
applicable limit is rejected immediately; absent or false length never bypasses
the streaming cap. Exact-limit input is allowed and one byte over fails closed.
Success bodies require their contracted JSON media type and problem bodies
require `application/problem+json`; content sniffing is forbidden. Oversize,
wrong-media, malformed UTF-8, and JSON/schema errors use safe classifications
and never log or diagnose the response body.

### SSE framing and backpressure

An event session accepts only a successful UTF-8 `text/event-stream` response
that confirms the negotiated profile. Parsing is incremental across arbitrary
network chunks, accepts LF, CRLF, or bare CR, dispatches only at a blank line,
joins repeated `data:` fields with a newline, treats comments as liveness only, and
ignores unknown SSE fields. It never parses one `data:` line as a complete JSON
event.

Client-local limits are independent of the discovery request-body limit: 64 KiB
per decoded line, 1 MiB per dispatched event frame, and 64 queued events. The
transport applies backpressure by awaiting a queue slot for at most five seconds
on an injected clock; while full, socket reading pauses. It never drops or
coalesces events. Malformed UTF-8, an oversized line/frame, or an expired enqueue
deadline closes the stream, retains the last accepted checkpoint, marks all
entities unavailable, and raises a safe terminal stream-capacity condition.
Untrusted raw frame content is never logged or placed in diagnostics.

### Event acceptance

`vehicle.current.changed` replaces one vehicle's current projection.
`data_quality.changed` replaces only the allow-listed quality classification when its
`subject_type` is `vehicle` and the vehicle is known. Other known profile events
do not mutate Home Assistant state, but they are decoded, validated against
their exact profile schema, and checked for every applicable envelope/payload
semantic equality before being accepted and ignored. Unknown event names are
ignored before JSON decoding or schema validation, as required by the protocol.

Before accepting any known event, verify:

- accepted stream session and negotiated profile;
- the decoded envelope against the event schema selected by that confirmed
  profile;
- SSE `event` equals the envelope `event_type`;
- SSE ID equals the envelope event ID;
- every applicable envelope/payload vehicle, resource, and revision equality;
- every selected-profile semantic validator, including metadata audit-chain and
  deletion-tombstone consistency;
- Hub/config-entry binding has not changed.

Event visibility stays bound to the authenticated principal and normalized
filters. A non-null event `vehicle_id` must be in the authorized vehicle set or
enter the gated unknown-vehicle path; it is never exposed merely because the
stream supplied it. Ignored resource IDs remain absent from state, registries,
logs, and diagnostics. Enabling the adapter requires the protocol's
`sse-principal-visibility` conformance case against the deployed Hub.

A projected current-state event additionally requires a known vehicle and full
current-state identity/revision equality. A projected quality event additionally
requires `subject_type == vehicle`, a known `subject_id`, and
`resource_id == subject_id`; protocol-valid `vehicle_id: null` is accepted. Both
mutate an immutable shadow projection and publish atomically.

Event IDs remain opaque. For a projected current-state event, acceptance means
successful validation and atomic application. The same rule applies to a
projected quality event. A known but unprojected event is accepted only after
full validation; an unknown event name is accepted after a valid SSE dispatch
with a non-empty ID, without decoding its data. After any acceptance, persist
the exact ID as the replay checkpoint. Track exact accepted IDs for idempotence,
using at least the last ID or a bounded recent-ID set, but never compare or order
them. Stream wire order is authoritative. Resource revisions are checked only
for required envelope/payload equality; the protocol does not guarantee that
revisions are monotonic.

An exact duplicate accepted ID is a no-op only when it matches the previously
accepted event identity/content. Reuse of one ID for different content is
terminal protocol non-conformance.

Projection and replay state share one commit boundary. A projected event's
allow-listed modeled state and exact checkpoint are durably committed together
before Home Assistant publication. An ignored accepted event commits only its
new checkpoint against the already committed projection generation. Startup
never restores a checkpoint without the matching projection generation and
Hub/principal/filter/profile binding. If atomic durable storage is unavailable,
discard the checkpoint and require synchronized startup; never resume after an
orphan checkpoint.

A valid current-state or vehicle-quality event for an unknown vehicle is held
pending, the coordinator stays unavailable, and later stream messages are
buffered within strict size/time bounds. A contract-defined synchronized
reconciliation must confirm the vehicle before the pending event can be
applied and checkpointed. The current `1.2.0` surfaces cannot close that race,
so production transport remains gated rather than inventing recovery semantics.

### Disconnect and replay

- Ordinary transport loss marks coordinator entities unavailable. Automatic
  replay may restore continuity only when a non-empty last accepted event ID
  exists and the resumed stream confirms the same Hub, principal, filters, and
  profile. A live-only reconnect with no checkpoint cannot restore availability.
- An empty SSE `id:` clears the replay checkpoint immediately without breaking
  the still-open stream. If that stream later disconnects before another event
  is accepted, synchronized startup is required.
- `410 event_replay_expired` clears the expired point and requires synchronized
  resynchronization. `400 event_id_invalid` first revalidates Hub, principal,
  filters, and profile; mismatch is terminal, while an unchanged binding may
  recover only through the same synchronized boundary. Until gate 4 closes,
  both cases stay unavailable.
- A malformed known event or any schema, identity, or semantic-equality failure
  is terminal protocol non-conformance. Retain the last good checkpoint, never
  checkpoint the poison event, stop automatic reconnect, stay unavailable, and
  expose a translated safe repair condition. A query refresh cannot re-enable
  the integration.
- HTTP `204` is terminal: stop reconnecting, stay unavailable, and expose a
  translated non-sensitive repair condition.
- Authentication failure stops reconnecting and starts reauthentication.
- Silence beyond the protocol's 15-second heartbeat guarantee plus a bounded
  grace period is transport loss. The watchdog uses an injected clock in tests
  and is cancelled during unload.
- Reconnect delay uses deterministic injected scheduling in tests, honors a
  valid server retry, adds bounded jitter in production, and never exceeds the
  protocol's 30-second cap. The default is 3,000 ms until a valid server `retry`
  replaces it; the old 1-second exponential schedule is not protocol semantics.
- HTTP `429` is separate: require status/body agreement, code `rate_limited` or
  `concurrency_limit`, `retryable: true`, and integer `Retry-After` in seconds
  from 0 through 86,400. If body `retry_after_seconds` is present, it must agree
  with the header. Remain unavailable and honor that bounded delay without
  applying the SSE millisecond cap. A false retryable flag, conflicting code or
  delay, or missing/malformed required delay is terminal protocol
  non-conformance.

No event removes a vehicle in profile `1.2.0`. Vehicle retirement occurs only
when a complete synchronized list omits it. Its entities first become
unavailable; stale-device cleanup follows Home Assistant's registry rules. No
eventual retirement/rename claim is made while gate 4 lacks a lifecycle event or
approved bounded list-reconciliation policy.

### Availability

- Stream, authentication, protocol, terminal, or continuity failure makes every
  coordinator entity unavailable.
- In a live synchronized session, a listed vehicle with a validated current
  resource is available. Reported `offline`, `asleep`, or `unknown` vehicle state
  does not by itself make the entity unavailable.
- A present required nullable metric with value `null`, or an omitted
  schema-optional temperature, makes only that metric entity `unknown`. Missing
  any required current-state field rejects the whole resource/event and cannot
  produce a partial vehicle projection.
- `partial` or `degraded` protocol quality remains available and is exposed only
  through the disabled-by-default diagnostic entity. The integration does not
  recalculate quality or impose a local age cutoff.
- A listed vehicle without a valid current resource is never partially
  published. A retired vehicle becomes unavailable before registry cleanup.

## Configuration and transport security

- Non-loopback endpoints require HTTPS. Plain HTTP accepts only literal IPv4 or
  IPv6 loopback, never a hostname that merely resolves to loopback.
- Discovery probe is unauthenticated and follows the released schema.
- Automatic redirects are disabled. Every `3xx` from discovery, query, SSE, or
  future pairing is rejected, including same-origin redirects, unless a later
  versioned deployment contract defines that exact redirect behavior. A bearer
  is never forwarded across an origin change.
- Hub ID deduplicates config entries but never authenticates an endpoint.
- Pairing input stays transient and password-masked.
- Every secret-bearing pairing, reauthentication, or migration claim requires
  the deployment contract's pre-secret cryptographic origin proof or channel
  binding; TLS reachability and a matching Hub ID alone are insufficient.
- Future entry creation requires pairing success, same-Hub verification, and
  at least one authenticated contract read.
- Reauthentication requires a fresh pairing claim, same-Hub verification, and
  authenticated read before atomic bearer update/reload.
- Reconfiguration sends neither the stored bearer nor pairing material to a
  candidate endpoint. Before any secret leaves the client, the candidate needs
  cryptographic origin proof or channel binding defined by the deployment
  contract. A matching Hub ID, successful claim, or authenticated read is not
  origin proof. Until that gate closes, reconfiguration returns the
  deployment-readiness error without changing entry data.
- Zeroconf advertisement and automatic endpoint updates are removed. Future
  discovery supplies a candidate only and always requires user confirmation.

## Diagnostics, logging, and errors

Diagnostics are built from an explicit safe schema. They never start from
`dict(entry.data)` or a raw runtime object. Output may include:

- config-entry major/minor version;
- TLS enabled boolean without host or port;
- negotiated protocol version and non-sensitive capability IDs;
- connection/reconnect state and classified error code;
- aggregate vehicle count and data-quality counts;
- boolean presence of a replay checkpoint, never its value.

Omit or redact all endpoints, ports, bearer/claim material, Hub and vehicle
identities, names, locations, precise vehicle timestamps, raw query/event
payloads, headers, request/trace IDs, and exception text. The pending client
representation must not reveal host or port.

Client exceptions carry a stable internal classification and a generic safe
message. Coordinator errors and logs use only that classification. Tests seed
sentinel values into known and unknown legacy config fields and prove none
appears in serialized diagnostics, exceptions, logs, entity attributes, or
repair text.

## Packaging and documentation

- Keep one `custom_components/teslatlas_hub` integration.
- Remove the manifest Zeroconf entry while that contract gate is open.
- Retain root `hacs.json`, pinned local environment, Apache-2.0 licence, and no
  hosted automation.
- Copy only the minimum redacted protocol examples needed by tests, preserving
  their Apache-2.0 provenance and exact source commit.
- Update README and readiness documentation to distinguish released protocol
  artifacts from missing deployment and live-conformance proof.
- Incorporate the useful local-plan rules into repository-owned current docs;
  do not commit its obsolete route/domain assertions verbatim.
- Do not create a brand asset, release, GitHub topics, HACS submission, Actions,
  Dependabot, or release automation in this merge.

## Test strategy

Every behavior change follows a failing-test-first cycle.

### Contract/model tests

- Exercise declared, absent, false, exact-limit, and one-byte-over JSON response
  lengths for discovery, pages, current state, and problems, including chunked
  reads, wrong media types, malformed UTF-8, and raw sentinel non-leakage.
- Parse frozen discovery, vehicle, current-state, current-state-event, and
  data-quality examples copied from protocol commit `79ced4c`. Build the
  data-quality event wrapper deterministically from the frozen event schema and
  quality example, recording that provenance instead of claiming a copied event
  fixture that does not exist upstream.
- Prove discovery only proposes a version and authenticated query plus SSE
  responses independently confirm it; reject mismatched list-page and
  current-resource response versions.
- Cover required capability presence, uniqueness, supported version/status,
  deprecation/sunset handling, and endpoint/href consistency; reject
  experimental required capabilities, while absence of the unused optional
  `data-quality` query capability does not block nested/event quality handling.
- Cover cursor pass-through, null termination, stable snapshot revision,
  duplicate vehicles, cursor loops, local bounds, expiry restart, query
  mismatch, invalid cursor, repeated scope change/budget exhaustion, unload
  cancellation, and all-or-nothing publication.
- Reject invalid identity, version, capability, origin, field type, event
  equality, and revision cases.
- Exercise authorized principal/filter visibility and prove hidden vehicle or
  resource sentinels cannot reach state, registries, logs, or diagnostics.
- Prove unsupported fixture fields cannot create entities.
- Use divergent list/current state fixtures to prove the entity always publishes
  `current_state.state`.

### Configuration/security tests

- Reject non-loopback plaintext, hostname aliases for loopback, same-origin and
  cross-origin redirects, and any bearer or pairing material sent to a candidate
  without contract-defined pre-secret origin proof.
- Keep pairing input transient and unknown legacy config fields absent from
  diagnostics.
- Prove reauth/reconfigure never mutate entries on identity/readiness failure.
- Prove Zeroconf is not advertised or auto-applied.

### Runtime tests

- Validate response media type/encoding and incremental SSE framing across every
  chunk boundary, including split UTF-8 code points, multiline data,
  LF/CRLF/bare-CR endings, comments, unknown fields, blank-line dispatch, empty
  ID, and retry. Cover
  malformed UTF-8, exact byte/line/frame boundaries, queue backpressure, and
  enqueue-deadline expiry without raw-data leakage.
- Atomic fixture-bound startup; ordinary resume with a checkpoint; disconnect
  without one; exact duplicate; envelope/payload revision mismatch; known
  unprojected and unknown event checkpointing; projected vehicle-quality update;
  schema-valid but semantically broken metadata rejection;
  valid null quality `vehicle_id`; non-vehicle quality no-op; quality resource
  mismatch; invalid or expired replay ID; empty-ID reset; malformed-event terminal
  failure; terminal `204`; auth failure; SSE retry milliseconds/default/cap;
  `429 Retry-After` seconds/maximum/malformed handling plus false-retryable and
  header/body mismatch cases; heartbeat timeout;
  unknown-vehicle gated reconciliation; explicit
  synchronized vehicle retirement; reconnect cap; recovery; atomic
  projection/checkpoint restoration; and clean unload.
- Availability remains false through reconciliation failure and returns only
  after an atomic authoritative replacement or accepted replay continuity.
- Test every availability rule, including reported asleep/offline, nullable
  required-null metrics, optional-temperature omission, required-field omission
  rejection, partial/degraded quality, and no local freshness inference.
- No healthy-state scheduled query polling; the unresolved lifecycle limitation
  remains explicit while gate 4 is open.

### Entity and privacy tests

- Exact protocol-backed entities, unique IDs, devices, translations, units,
  classes, diagnostic defaults, per-vehicle availability, and stale-device
  behavior.
- Explicit Hub parent-device creation, vehicle `via_device`, and both lock
  binary-sensor polarities.
- No commands, writable entities, raw attributes, private Hub data, secrets,
  identity, location, or precise vehicle timestamps in support output.

### Full gate

- Locked dependency validation.
- Full pytest branch coverage with config flow, SSE parser, and checkpoint state
  machine at 100%, and the package at least 95%.
- Ruff format and lint.
- JSON parsing, manifest/translation checks, boundary scan, and diff check.
- Independent P1/P2 reviews for security/contract, runtime/state integrity, and
  Home Assistant/HACS packaging.

No test result will be described as live Hub, HACS install, or release proof.

## Completion criteria

The merged `main` is complete when:

1. the executable foundation and relevant local-plan constraints coexist in
   one clean history;
2. documentation accurately describes protocol `1.2.0` and every open gate;
3. no production path can send secrets over plaintext or to an unverified
   origin;
4. no unvalidated event, orphan checkpoint, or unproven continuity path can
   restore availability or publish state;
5. the entity surface contains only contracted read-only profile `1.2.0`
   semantics;
6. diagnostics and errors are allow-listed and sentinel-tested;
7. synchronized transport, Zeroconf, HACS publication, commands, and external
   automation remain disabled where proof is absent;
8. all local gates and independent reviews pass;
9. the adapted changes are committed and pushed to `origin/main` without
   deleting or modifying the separate local plan repository.

## Evidence anchors

- Home Assistant base: `55d70245d9e368e6bb0606c1cfd7fcd06a73a3b4`.
- Concurrent local plan: untracked
  `docs/plans/2026-08-30-local-push-hacs.md` in the separate output checkout.
- Protocol profile: `teslatlas-protocol` commit
  [`79ced4c`](https://github.com/magrathean-uk/teslatlas-protocol/commit/79ced4c7fdc79520ad31d72a0280bf5f3f19f407).
- Protocol HTTP and deployment boundary:
  [`docs/http.md`](https://github.com/magrathean-uk/teslatlas-protocol/blob/79ced4c7fdc79520ad31d72a0280bf5f3f19f407/docs/http.md).
- Protocol SSE continuity:
  [`docs/events.md`](https://github.com/magrathean-uk/teslatlas-protocol/blob/79ced4c7fdc79520ad31d72a0280bf5f3f19f407/docs/events.md).
- Hub beta API guide:
  [`docs/guides/api.md`](https://github.com/magrathean-uk/teslatlas-hub/blob/43d7e967a074d8de70f581fa4ed37b9b61b7bbde/docs/guides/api.md).
- Home Assistant config-flow connection rule:
  [test before configure](https://developers.home-assistant.io/docs/core/integration-quality-scale/rules/test-before-configure/).
- Home Assistant sensor statistics semantics:
  [sensor entity](https://developers.home-assistant.io/docs/core/entity/sensor/).
