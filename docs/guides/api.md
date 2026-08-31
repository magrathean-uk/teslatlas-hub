# HTTP and sync API

The `/v1` read surface is the public foundation for paired clients. The signed,
content-addressed pack protocol remains available for efficient full-device
synchronisation. This release does not expose a general vehicle-control API.

Public capabilities are additive within an advertised API version. Clients
must discover support instead of assuming that an endpoint exists. The full
external specification, generated SDK, event stream, and conformance gate are
not complete in this release.

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
| `GET` | `/readyz` | Collector and serving readiness. |
| `GET` | `/.well-known/teslatlas-hub` | Stable Hub identity, API versions, capabilities, build, sync protocol, and source route. Tagged builds expose their version-bound source; development builds expose the repository. |
| `POST` | `/v1/pairings/{pairing_id}/claim` | Claim one pairing invitation. |
| `POST` | `/v1/device/rotate` | Rotate the current device bearer. |
| `GET` | `/v1/vehicles` | List vehicles visible to the paired device. |
| `GET` | `/v1/vehicles/{vehicle_id}/current` | Get bounded current state. |
| `GET` | `/v1/vehicles/{vehicle_id}/drives` | Get one bounded, newest-first drive page. |
| `GET` | `/v1/vehicles/{vehicle_id}/sync/manifest` | Get a signed sync manifest. |
| `GET` | `/v1/vehicles/{vehicle_id}/sync/noop` | Schema 2.2 no-op synchronization response. |
| `GET` | `/v1/packs/sha256/{object_name}` | Stream one manifest-authorized immutable pack. |
| `POST` | `/v1/internal/fleet-telemetry` | Private supervised Fleet Telemetry ingestion. |

Pack objects are content-addressed and served only when an authorized manifest
references the digest. Unknown, retired, orphaned, and unauthorized digests are
rejected.

## Discovery and drive queries

`GET /.well-known/teslatlas-hub` is not secret-bearing. Its `hub_id` is the
stable installation UUID. `api_versions` currently contains `"1.0"`.
`capabilities` contains only implemented and currently usable surfaces. A Hub
with its protected cursor-signing key advertises `query.vehicles`,
`query.current`, `query.drives`, and `sync.packs`. An unsigned loopback test or
recovery instance advertises only `query.vehicles` and `query.current`.

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

## Errors and request IDs

Responses include `x-request-id` when the request reaches the router. Preserve
that identifier in a redacted bug report. Never attach bearers, pairing
payloads, precise locations, or pack contents to a public issue.
