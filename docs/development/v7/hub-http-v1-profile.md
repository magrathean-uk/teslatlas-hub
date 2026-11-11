# Canonical read contract `hub-http-v1@1.3.0`

Status: implemented in the Hub source. Discovery advertises `hub-http-v1@1.3.0`.
DEC-005 selects this HTTP revision with `hub-sync-v1@1.4.0` for App v7 H1.
Profile checks establish contract consistency; device and live-service acceptance
require their own evidence.

## Served profiles

- HTTP `hub-http-v1@1.3.0` is served and advertised in discovery.
- HTTP 1.2.0 is retired as an advertised revision. Its published profile bytes,
  including the historical `status: served` field, remain frozen. Its keys stay
  in 1.3.0 as deprecated aliases, with the exceptions in `deprecated_aliases`.
- Sync `hub-sync-v1@1.3.0` is served for the legacy and lineage 2.1 path.
- Sync `hub-sync-v1@1.4.0` is served for PhysicalV3 checkpoints and changed sets.
- Sync `hub-sync-v1@1.5.0` is a candidate and is not served.

H1 does not deliver live-collected drives after handover. The live-overlay
revision follows separately, and the source-to-Hub handover waits for it.

## Contract authority

The complete [HTTP 1.3.0 profile](../../../protocol/profiles/hub-http-v1/1.3.0/)
contains `profile.json`, OpenAPI 3.1.0, adjacent schemas, field semantics,
examples, cases and `SHA256SUMS`. Distribute the complete directory and bind it
by digest. The [current-Hub guide](../../../protocol/docs/reference/current-hub.md)
provides the walkthrough; [verification limits](../../../protocol/docs/reference/verification.md)
separate source and fixture checks from runtime acceptance.

HTTP 1.3.0 adds neutral names to the retained read surface: 28 current keys,
car and car-settings keys, and 33 history keys. Clients read names such as
`started_at_ms`, `ended_at_ms`, `timestamp_ms`, `speed_kmh`, `odometer_km` and
`battery_range_km`. Every old key remains a deprecated alias. The two exceptions
are the old miles-to-arrival value and the stored car-model text, as stated in
`profile.json`. Null and numeric zero remain distinct.

## Discovery and access

`GET /.well-known/teslatlas-hub` names `hub-http-v1@1.3.0`; `version` is the
product version, while `api_versions` remains `["1.0"]`. Capabilities reflect
available routes and configuration. Require the capability for a route before
calling it; clients ignore unknown fields and capability tokens.

Every vehicle-scoped read enforces the paired device's vehicle grant. Unknown
and ungranted vehicles receive the same response on each route. Pack reads
resolve their owning vehicle before streaming; an ungranted digest returns the
same empty 404 as an unknown digest. Sync-route status sets are specified in the
profiles and are not all identical to the JSON read routes.

A grace bearer on `POST /v1/device/rotate` retrieves the already-issued successor
within the grace window. `POST /v1/device/revoke` revokes the calling device with
204; subsequent requests, including a repeated revoke, return 401. A client
handles 401 on revoke as already revoked.

## Read pages and cursors

Vehicles, current state, drives, drive positions, charges and charge samples
use the shapes and limits in the profile. History lists contain closed sessions.
Drives and charges order by `started_at_ms DESC, id DESC`; positions and charge
samples order by `timestamp_ms ASC, id ASC`. Cursors are opaque `tqp1` values
bound to kind, vehicle, scope and time window; retained `tqd1` cursors are accepted
only for drives. Pages are bounded to 1 MiB and never split an item. Conditional
page reads use a quoted SHA-256 ETag and return 304 on a match.

Address and elevation provenance use the schema-defined objects and preserve
null when no truthful record exists. Enrichment capabilities and attributions
are advertised only when configured. Browser origins are empty by default.

## Signing keys and sync

The [sync guide](../../../protocol/docs/reference/hub-sync.md) describes the
selectors and checkpoints. The authenticated vehicle-bound signing-keys route
returns `profile_id: hub-sync-v1@1.3.0` for both served revisions. The 1.4.0
signing-keys schema validates that shared document, so a strict 1.4 client can
verify the keys without expecting a different profile identity.

Only the immediate predecessor can receive a 1.4 physical changed set. Its pack
includes unchanged context needed to close impacted roots. An older retained
base receives a signed complete replacement; an expired base receives unsigned
422 `unknown_base_receipt`. Live-overlay behaviour belongs to candidate 1.5.0.
