# HTTP and sync API

The `/v1` read surface is the public foundation for paired clients. The signed,
content-addressed pack protocol remains available for efficient full-device
synchronisation. This release does not expose a general vehicle-control API.

Public capabilities are additive within an advertised API version. Clients
must discover support instead of assuming that an endpoint exists. Product
`2026.36.2` has an accepted bounded `hub-http-v1@1.0.0` binding with manifest
SHA-256
`b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`.
The richer external specification and event stream remain separate surfaces.
The accepted binding does not add routes beyond those advertised by discovery.
That record is historical: discovery now names the profile the Hub serves,
`hub-http-v1@1.2.0` (see Discovery below).

## Transport and authentication

- Plaintext HTTP is accepted only on loopback.
- A non-loopback bind requires TLS.
- TLS-facing mirror routes require a paired-device bearer.
- `POST /v1/pairings/{pairing_id}/claim` is the sole unauthenticated mutation
  and accepts only a live, single-use invitation.
- Internal Fleet Telemetry ingestion uses a separate private bearer and must
  remain on loopback behind the supervised receiver.
- Ordinary request bodies are capped at 4 KiB. Concurrency and handler time are
  bounded. Pack response streams use separate bounded file slots.

## Routes

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/healthz` | Process and store health. |
| `GET` | `/readyz` | Service readiness: `{"status":"ready"}` or `{"status":"not_ready","reason":"<reason>"}` with one of six reasons. Collection attention never appears here. |
| `GET` | `/.well-known/teslatlas-hub` | Stable Hub identity, API versions, capabilities, build, sync protocol, and configured source route. |
| `POST` | `/v1/pairings/{pairing_id}/claim` | Claim one pairing invitation. |
| `POST` | `/v1/device/rotate` | Rotate the current device bearer. The prior bearer keeps serving for 24 hours, and retrying with it returns the same successor. Once the device's stored expiry has reached its absolute maximum lifetime, and until that time arrives, it returns the current bearer unchanged with `"reason": "max_lifetime"`; from the maximum on the bearer is 401. |
| `POST` | `/v1/device/revoke` | Revoke the calling device (no request body; any body is 400). `204`; every bearer of the device is then 401. Clients call it only when discovery advertises `auth.revoke`, which the Hub does whenever paired-device authentication is on. |
| `GET` | `/v1/vehicles` | List vehicles visible to the paired device. |
| `GET` | `/v1/vehicles/{vehicle_id}/current` | Get bounded current state. |
| `GET` | `/v1/vehicles/{vehicle_id}/drives` | Get one bounded, newest-first drive page. |
| `GET` | `/v1/vehicles/{vehicle_id}/sync/signing-keys` | Get the authenticated signing-key manifest. |
| `POST` | `/v1/vehicles/{vehicle_id}/sync/changes-since` | Request a signed no-op, changed set, or replacement for a verified base. |
| `GET` | `/v1/vehicles/{vehicle_id}/sync/manifest` | Get a signed sync manifest. |
| `GET` | `/v1/vehicles/{vehicle_id}/sync/noop` | Schema 2.2 no-op synchronization response. |
| `GET` | `/v1/vehicles/{vehicle_id}/sync/prepared-artefacts/{artifact_id}` | Get an available, signed prepared-map receipt for the current physical head. |
| `GET` | `/v1/packs/sha256/{object_name}` | Stream one manifest-authorized immutable pack. |
| `POST` | `/v1/internal/fleet-telemetry` | Private supervised Fleet Telemetry ingestion. |

`/readyz` answers 200 `{"status":"ready"}` or 503 `{"status":"not_ready","reason":R}`,
where `R` is `catalogue_unavailable`, `lifecycle_quarantined`,
`published_content_unservable`, `collector_absent`, `collector_stale` or
`collector_auth_terminal`. Conditions you can fix while the Hub stays up (a refusal from
Tesla or a provider, a sign-in that still works but must be renewed, a Fleet refresh whose
outcome is unknown, a broken collector cadence and the like) are collection attention: they
appear in `teslatlas-hub status`, `doctor` and the Mac app, never in this body, and never
change the status code. The body and its headers keep the published 1.2.0 shape; an
optional detail member and a cadence diagnostic are held for the next protocol
declaration. See [Configuration](configuration.md#sign-in-status).

Pack objects are content-addressed and served only when an authorized manifest
references the digest and one of the vehicles that owns it is granted to the
paired device. Unknown, expired, orphaned, unauthorized, and ungranted digests
are rejected with the same empty 404.

A paired device reads only the vehicles it is granted at pairing. Every
vehicle-scoped route, the pack route included, answers an ungranted vehicle
exactly as it answers an unknown one, and `GET /v1/vehicles` lists only the
granted and published vehicles. Removing a grant applies to the next request on
every route, packs included. Retiring a vehicle hides it from the
vehicle-addressed routes and from `GET /v1/vehicles` at once, but a device that
still holds the grant can fetch the vehicle's already catalogued packs by
digest; removing the grant withdraws that access. The nil UUID is an unknown
vehicle on every route; its value never causes a 503.

## Physical history and prepared maps

The selected-car import publishes a complete signed PhysicalV3 schema-2.2 head.
The authenticated `changes-since` request negotiates the supported Hub Sync
profile; existing Protocol 1.3 requests retain their original response format.
Protocol 1.4 binds the request to the installation, account, vehicle, source
generation, selected car, and exact signed base manifest and receipt. An
unchanged head returns a signed no-op. An immediate predecessor may receive a
signed `physical_changed_set` with typed changed rows, removals, affected
projection IDs, impacted roots, and bounded context in up to 64 compressed
packs. A missing transition or older retained base receives a signed `409`
full-head rebase; an unknown or expired base is rejected. Full schema-2.2 packs
remain the bootstrap and capacity fallback. The Protocol 1.4 profile defines
the exact wire fields, signatures, and pack limits.

An eligible completed UTC month may have a prepared-map artifact derived from
the same signed head. Its deterministic ID includes the source receipt and
window, style, geometry, and algorithm identity. The authenticated artifact
route returns `404` when the current head has no such artifact, allowing the
client to render locally. Its pack uses the same authorized digest download
route. A published receipt or focused host check is not evidence that the
physical phone selected the prepared path; that acceptance belongs in the
workspace master plan.

## Discovery and drive queries

`GET /.well-known/teslatlas-hub` is not secret-bearing and is never cached
(`Cache-Control: no-store`). Its `hub_id` is the stable installation UUID.
`api_versions` currently contains `"1.0"`, `protocol` is `teslatlas-sync` and
`protocol_major` is `1`, so Home Assistant keeps working unchanged. The
`profile` member always names the wire profile the Hub serves,
`hub-http-v1@1.2.0`; it is the wire profile revision, which the `version` field
(the Hub product version, `2026.36.2` for the accepted cohort) is not.

`capabilities` contains only implemented and currently usable surfaces: a token
is advertised only when its routes are served, so the set is computed from how
the Hub runs.

| Token | Advertised when |
|---|---|
| `query.vehicles`, `query.current` | Always. |
| `query.drives`, `query.charges`, `query.charge.samples`, `query.positions`, `sync.packs` | The Hub holds its protected cursor-signing key. All five or none. |
| `auth.revoke` | Paired-device authentication is on, as on the TLS-facing router: only then is `POST /v1/device/revoke` served (a trusted local router answers it `404`). |

Tokens appear in the order of the `capabilities` list in the profile's
`profile.json`; the contract treats the list as a set. A TLS-facing Hub, which
has both its cursor key and paired-device authentication, advertises all eight
tokens. An unsigned loopback test or recovery instance, without the key,
advertises only `query.vehicles` and `query.current`. `query.enrichment`, the
ninth token of the profile, is not advertised: it requires the `attributions`
member, which this Hub does not emit.

`GET /v1/vehicles/{vehicle_id}/drives` requires the same paired-device bearer
as other non-loopback mirror reads. Query parameters are:

- `from_ms`: inclusive UTC Unix epoch milliseconds; default `0`.
- `to_ms`: exclusive UTC Unix epoch milliseconds; default the maximum signed
  64-bit value.
- `limit`: result count from `1` through `500`; default `100`.
- `cursor`: opaque continuation value returned as `next_cursor`.

The time bounds must be nonnegative and `from_ms` must be less than `to_ms`.
There is no duration cap; the indexed keyset query and result limit bound every
request. Pages sort by `(start_date_ms, id)` descending. A cursor is bound to
the vehicle and exact time bounds. Clients must not parse, manufacture, log, or
reuse it with different filters. Pagination is never offset based.

Public drive objects include `vehicle_id` and projection values, but exclude
source-private `car_id` and projection-maintenance `optimized_at_ms` fields.
Responses use `Cache-Control: no-store` and a strong `ETag`. Send that value in
`If-None-Match` to receive `304 Not Modified` when the page bytes are unchanged.

Drive-query failures use an `error.code` plus human-readable `error.message`.
Stable codes are `invalid_query`, `invalid_time_range`, `invalid_limit`,
`invalid_cursor`, `vehicle_not_found`, and `service_unavailable`. Authentication
failures retain the paired-device error contract.

`/v1/events` and `/v1/data-quality` are intentionally not advertised yet. The
current sync journal cannot provide a complete source-independent replay across
imports, and the current projections cannot truthfully infer historical data
completeness after raw-data pruning.

## Pair a device

Create a short-lived invitation on the Hub host:

```sh
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml pair \
  --label "Teslatlas iPhone" --expires-in-seconds 900
```

The displayed QR contains secret-bearing claim material. Do not screenshot,
log, email, or reuse it. Revoke a paired device from the local CLI if a client
is lost.

`pair` needs the Hub stopped; `control pair` creates the same invitation while
it serves. Add `--vehicle UUID` (repeatable) to grant only those vehicles, or
`--all-vehicles` (the default) to grant every vehicle active when the
invitation is claimed; the output names the scope.

## Errors and request IDs

Responses include `x-request-id` when the request reaches the router. Preserve
that identifier in a redacted bug report. Never attach bearers, pairing
payloads, precise locations, or pack contents to a public issue.

## Accepted evidence boundary

G3 r2 (`teslatlas-protocol/docs/development/g3-compatibility-admission-2026-09-19-r2.json`)
admitted the exact product/profile records using accepted
G4 TypeScript (`teslatlas-sdk-typescript/docs/development/macos-arm64-packed-node-browser-g4-2026-09-18-r1.json`),
G5 Edge (`teslatlas-edge/docs/development/g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json`),
and G6 Swift (`teslatlas-sdk-swift/docs/development/g6-macos-arm64-external-consumer-acceptance-2026-09-19-r2.json`)
receipts. The G3 receipt SHA-256 is
`5df27073463ca985f409332a043b5f46aa753ba4b1b65fe214bc434521ef1865`.
Those receipts cover source-built synthetic
journeys on macOS 27 Apple silicon and Debian 13.6 ARM64, including pairing,
rotation, current/history reads, bounded pagination, restart behavior, and the
recorded Edge durable-delivery lane. They do not establish an installer,
notarized or service-managed release, real Tesla data, deferred client
integration, declared minimum OS floors, production, or a full platform matrix.
No source publication or tag is implied.
