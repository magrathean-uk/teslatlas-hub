# Design: canonical read contract `hub-http-v1@1.2.0`

Status: G2-0 design, 2026-10-08. Not implemented. Source anchors are the current Hub (`hub@6ac8ef0`), Protocol (`teslatlas-protocol@9e88653`), Home Assistant (`f56919a`) and the preserved App client.

## Decision summary

One served HTTP profile, `hub-http-v1@1.2.0`, replaces `hub-http-v1@1.0.0` (admitted) and `1.1.0` (candidate, adds the 24 h rotation grace). It keeps every existing route byte-compatible, adds JSON pages for charges, charge samples and drive positions, adds provenance for addresses and elevation, adds self-revocation, changes grace rotation to "retrieve the already-issued successor", and tolerates additive fields. Signed sync stays in `hub-sync-v1` (1.3.0 and 1.4.0) and is untouched by this document except the vehicle-scope enforcement.

Retired once 1.2.0 is served and conformance passes: the root `teslatlas-v1` family (13 of 17 paths were never served; SSE, commands and metadata are out of v7), `hub-http-v1` 1.0.0 and 1.1.0, `hub-sync-v1` 1.0.0 to 1.2.0, and `edge-delivery-v2` except `receiver-envelope.schema.json` and its vectors, which move to `protocol/receiver/`. Nothing is repinned before deletion; inside the workspace, pins become path references.

## Compatibility rules

- `api_versions` stays `["1.0"]`. Home Assistant hard-checks `api_versions == ["1.0"]`, `protocol == "teslatlas-sync"`, `protocol_major == 1` and the presence of `query.vehicles` and `query.current` (`current_hub_client.py:334-352`); it ignores unknown fields. It keeps working without a change.
- Capability tokens stay within `[a-z0-9.]`, 1 to 64 characters, at most 16, unique (the preserved App rule, `TeslatlasHubProtocol.swift:190-215`). New tokens: `query.charges`, `query.charge.samples`, `query.positions`, `query.enrichment`, `auth.revoke`.
- Discovery gains `profile` (`"hub-http-v1@1.2.0"`) and optional `attributions`. The preserved App rejects unknown discovery keys (`TeslatlasHubProtocol.swift:77-80`); App v7 updates its decoder as part of integration. App 6.5 has no Hub client, so nothing shipped breaks.
- `additive_policy` in `profile.json` becomes: "Clients ignore unknown object fields and unknown capability tokens. Servers may add optional fields and tokens within 1.x. Removing or retyping a field, changing an enum value, or changing a route's status set is a new major profile."
- Existing routes (`/healthz`, `/readyz`, discovery, claim, rotate, `/v1/vehicles`, `/current`, `/drives`) keep their shapes, status sets and error forms. `/current` keeps its empty-body 404 and 503 (the preserved App accepts only those). `drives` keeps `tqd1` cursors and the 1 MiB page bound.
- Integers stay signed 64-bit on the wire; the generator recipe tells JavaScript consumers to reject values outside ±2^53 (ids and millisecond timestamps are far below it).

## Discovery (`GET /.well-known/teslatlas-hub`)

Today's fields unchanged (`hub_id`, `protocol`, `protocol_major`, `api_versions`, `capabilities`, `version`, `sourceUrl`, `pack_format`, `manifestPublicKey` when signing exists). Added:

- `profile`: `"hub-http-v1@1.2.0"`.
- `capabilities`: existing tokens plus `query.charges`, `query.charge.samples`, `query.positions`, `auth.revoke`; `query.enrichment` only when address or terrain enrichment is enabled; `query.drives` and `sync.packs` still only with a cursor key.
- `attributions` (optional array, present when enrichment is enabled): `[{id, text, licence, url}]`, for example `{id:"osm", text:"© OpenStreetMap contributors", licence:"ODbL-1.0", url:"https://www.openstreetmap.org/copyright"}` and the terrain dataset entry. Clients that display enriched values show these.

The Hub constants `PUBLIC_API_VERSIONS` and `PUBLIC_CAPABILITIES` (`server.rs:74-81`) become computed from configuration.

## Access rules (apply to every route below and to `hub-sync-v1`)

- Bearer as today. Every vehicle-scoped route checks the device→vehicle grant (see `device-vehicle-scope.md`): an ungranted or unknown vehicle is `404 vehicle_not_found` on JSON routes and an empty 404 on `/current` and sync routes, so devices cannot enumerate cars.
- `GET /v1/vehicles` lists only granted vehicles.
- `GET /v1/packs/sha256/{digest}` resolves the pack's vehicle before streaming; ungranted packs are an empty 404.
- `POST /v1/device/rotate` with a grace (previous) bearer returns the successor already issued to that device, unchanged, or `401` after the grace window; it never mints new authority (plan decision 2026-10-08; replaces 1.1.0's `rotation-lost-response-retry` semantics).
- `POST /v1/device/revoke` (new, bearer): revokes the calling device, `204`, idempotent; subsequent requests `401`. Client logout calls it.
- Token lifetime: sliding expiry as today plus an absolute maximum (owner-set, default 180 days) carried in `expires_at_ms`.

## Pages

Common rules, lifted from the drives route: query `from_ms` inclusive (default 0), `to_ms` exclusive (default i64::MAX), `limit`, `cursor`; `deny_unknown_fields`; `400 invalid_query | invalid_time_range | invalid_limit | invalid_cursor`; response `{items:[…], next_cursor: string|null}`; serialised page at most 1 MiB and never split inside an item (an oversize item is `503 service_unavailable` with the existing message); `ETag` = quoted SHA-256 of the body, `If-None-Match` → `304`; `Cache-Control: no-store`; `Content-Type: application/json`.

Cursor `tqp1` (page cursor, version 1): `"tqp1." + hex(payload) + "." + hex(tag)`; payload = version byte 1, kind byte (1 drives, 2 charges, 3 positions, 4 charge samples), vehicle UUID (16), scope id (8, the drive or charge id for nested pages, else 0), `from_ms` (8), `to_ms` (8), last key A (8), last key B (8); tag = `CursorKey::public_query_cursor_tag` over the payload. Rejected when the kind, vehicle, scope or window differs, or the keys fall outside the window. `drives` keeps accepting `tqd1` for one release and issues `tqp1`.

| Route | Order and keys | Limit | Items |
| --- | --- | --- | --- |
| `GET /v1/vehicles/{id}/drives` | `start_date_ms DESC, id DESC` (unchanged) | default 100, max 500 | `PublicDrive` (28 fields, unchanged) plus optional `start_address_provenance`, `end_address_provenance` |
| `GET /v1/vehicles/{id}/drives/{drive_id}` | single | | `PublicDrive`; `404 drive_not_found` |
| `GET /v1/vehicles/{id}/drives/{drive_id}/positions` | `date_ms ASC, id ASC` (reuses `drive_positions_page`, `observations_and_lifecycle.rs:946`) | default 1000, max 2048 | `PublicPosition` |
| `GET /v1/vehicles/{id}/charges` | `start_date_ms DESC, id DESC` | default 100, max 500 | `PublicCharge` plus optional `address_provenance` |
| `GET /v1/vehicles/{id}/charges/{charge_id}` | single | | `PublicCharge`; `404 charge_not_found` |
| `GET /v1/vehicles/{id}/charges/{charge_id}/samples` | `timestamp_ms ASC, id ASC` | default 1000, max 2048 | `PublicChargeSample` |

Field lists (from the projection models in `sync/hub_pack/model.rs`; `car_id` and source ids are dropped, as `PublicDrive` already does):

- `PublicPosition`: `id, drive_id, date_ms, latitude, longitude, speed, power, battery_level, usable_battery_level, elevation, odometer, ideal_battery_range_km, est_battery_range_km, rated_battery_range_km, inside_temp, outside_temp, is_climate_on, fan_status, driver_temp_setting, passenger_temp_setting, battery_heater, battery_heater_on, tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr, elevation_provenance`.
- `PublicCharge`: `id, vehicle_id, start_date_ms, end_date_ms, duration_min, charge_energy_added, charge_energy_used_kwh, start_battery_level, end_battery_level, start_ideal_range_km, end_ideal_range_km, start_rated_range_km, end_rated_range_km, start_latitude, start_longitude, address, location_name, geofence, fast_charger_type, is_dc, charge_rate_km_per_hour, max_charger_power_kw, outside_temp_avg, cost, billing_type, cost_per_unit, session_fee, address_provenance`.
- `PublicChargeSample`: `id, charge_id, timestamp_ms, battery_level, usable_battery_level, charge_energy_added_kwh, charger_power_kw, charger_voltage, charger_actual_current, charger_pilot_current, charger_phases, ideal_range_km, rated_range_km, outside_temp_c, battery_heater_on, fast_charger_present, fast_charger_brand, fast_charger_type, charge_cable`.

All numeric fields nullable as in the projections; no field is dropped from a page because it is null.

## Provenance objects

- `address_provenance` (drives: `start_…`/`end_…`; charges: one): `{source: "import" | "geocoder" | "manual", provider: string|null, endpoint_host: string|null, language: string|null, looked_up_at_ms: int|null, licence: string|null}`. `import` values come from the migrated TeslaMate data and are never re-geocoded; `geocoder` values come from `address_lookup_cache`, which gains `provider`, `endpoint_host`, `language` and `licence` columns in G2-7 (the csf_22e2 fix binds cache rows to coordinates, provider and language at the same time). Null when no record exists.
- `elevation_provenance` (positions): `{source: "vehicle" | "import" | "terrain", dataset: string|null, dataset_version: string|null, tile: string|null}` from `terrain_elevation_provenance` (`migrations.rs:639`); `import` when the position came from TeslaMate with an elevation and no terrain row; `vehicle` when live-collected with an elevation; null when the elevation is null.

## Errors

`{error:{code,message}}` with `Cache-Control: no-store` on every JSON route. Codes: existing six plus `drive_not_found`, `charge_not_found`. Empty-body 401 with `WWW-Authenticate: Bearer`, empty 404/503 on `/current`, sync and packs, unchanged.

## Limits

`DefaultBodyLimit` 4 KiB on ordinary routes, 8 KiB on `changes-since`, 32 in-flight requests, 15 s handler timeout, 1 MiB per JSON page, pack stream limits unchanged. Every array in the OpenAPI carries `maxItems` (`vehicles` 10,000, pages per the table, `capabilities` 16, `attributions` 8); generated validators check counts before deep validation (closes the csf_cd07 pattern for the new profile).

## CORS

`browser_cors.rs::route_policy` gains the new routes; `allowed_origins` stays empty by default (no browser client in v7).

## Protocol package layout (after the move into `protocol/`)

- `profiles/hub-http-v1/1.2.0/`: `profile.json` (`previous_profile: "hub-http-v1@1.1.0"`, `status: "served"`), `openapi.json` (3.1, `info.version` 1.2.0, all routes above, `pairedBearer`), `discovery.schema.json`, `errors.schema.json`, `auth.schema.json` (adds revoke), `resources.schema.json` (adds `position`, `charge`, `charge_sample`, provenance `$defs`, pages), `cursor.schema.json` (`tqp1` grammar), `field-semantics.json`, `cases.json`, `examples/`.
- `conformance/`: cases per new route (empty page, keyset continuity across three pages, cursor rejections by kind/vehicle/scope/window, byte cap, ETag and 304, ungranted vehicle 404, pack 404 for an ungranted digest, revoke then 401, grace rotate returns the same successor), run against the Hub fixture lane.
- `generators/`: documented `openapi-generator` invocations for `swift5`, `typescript-fetch` and `python`, with the integer note; no hand-written SDK beyond the retained TypeScript Hub client, whose types are generated from this file.
- The `hub-sync-v1` 1.3.0 and 1.4.0 profiles stay as they are.

## Hub implementation outline (G2-3 and G2-5, for sizing only)

- `api/public_query.rs`: generalise the cursor to `tqp1`, keep `tqd1` decoding; page assembly and byte accounting shared by all pages.
- `storage/db/observations_and_lifecycle.rs`: add `materialised_charges_page`, `charge_samples_page`, `materialised_charge_for_vehicle`; reuse `drive_positions_page`.
- `api/server.rs`: six handlers, discovery computed fields, `revoke` handler, grant checks, pack→vehicle lookup.
- `browser_cors.rs`: route entries.
- Tests: one focused test per handler plus the conformance cases.

## Open points (not blocking)

- Whether `/current` should also carry `profile`-level additive fields later (no consumer asks today).
- Whether the TypeScript Hub client exposes the provenance objects typed or as passthrough (typed, generated).
