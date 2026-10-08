# Design: per-device vehicle scope and credential lifetime

Status: G2-0 design, 2026-10-08. Not implemented. Anchors: `hub@6ac8ef0` (`ID` = `src/storage/db/identity_and_audit.rs`, `MIG` = `src/storage/db/migrations.rs`, `SRV` = `src/api/server.rs`).

## Today

- `paired_devices(device_id, display_name, token_sha256, created_at_ms, expires_at_ms, revoked_at_ms, last_authenticated_at_ms)` (MIG:1513-1540), `pairing_challenges` (MIG:151-160), `paired_device_token_grace(device_id, token_sha256, valid_until_ms)` (MIG:2066-2072). No vehicle column anywhere; no device audit table.
- `authenticate_device_at` (ID:197-264) sets `expires = max(expires, now + 30 d)` and writes it on every request (csf_9811, STO-10). No absolute cap: an active leaked bearer never expires.
- `rotate_device` (ID:266-373) always mints a new token, also when presented with a grace token, and overwrites the successor of the first rotation (csf_9a86).
- Every vehicle route checks only `vehicle_is_active`; `pack()` (SRV:1988) checks nothing beyond the bearer. `GET /v1/vehicles` is global.
- Revocation exists only as `control revoke-device` on the CLI; the macOS app has no device list; TS client `logout()` clears local storage only; HA rotates 7 days before expiry.

## Schema (v67 → v68)

```sql
CREATE TABLE device_vehicle_grants (
  device_id TEXT NOT NULL REFERENCES paired_devices(device_id) ON DELETE CASCADE,
  vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
  granted_at_ms INTEGER NOT NULL CHECK (granted_at_ms >= 0),
  PRIMARY KEY (device_id, vehicle_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX device_vehicle_grants_vehicle ON device_vehicle_grants(vehicle_id, device_id);

CREATE TABLE pairing_challenge_vehicles (
  pairing_id TEXT NOT NULL REFERENCES pairing_challenges(pairing_id) ON DELETE CASCADE,
  vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
  PRIMARY KEY (pairing_id, vehicle_id)
) STRICT, WITHOUT ROWID;

ALTER TABLE paired_devices ADD COLUMN max_expires_at_ms INTEGER;        -- absolute cap
ALTER TABLE paired_device_token_grace ADD COLUMN sealed_successor BLOB;  -- successor sealed under the grace token
```

- Backfill (owner decision recorded here): existing devices receive a grant for every vehicle active at migration time (`INSERT … SELECT d.device_id, v.vehicle_id, d.created_at_ms FROM paired_devices d CROSS JOIN vehicles v WHERE v.retired_at_ms IS NULL`). Vehicles added later are never auto-granted. `max_expires_at_ms` backfills to `created_at_ms + 180 d`.
- Grants survive vehicle retirement (ID:446-449 keeps pairing authority); a retired vehicle is still filtered by `vehicle_is_active`.
- `data_recovery.rs:705-712` and `:1325-1340` (restore deletes pairing authority) add the two new tables; the restore connection must enable `foreign_keys` or delete explicitly.
- Ordering: this migration lands before the G2-4 store-floor collapse, or is folded into it; `SCHEMA_VERSION` bumps either way.

## Pairing with a scope

- `pair` gains `--vehicle <uuid>` (repeatable) and `--all-vehicles`; the chosen set is written to `pairing_challenge_vehicles` by `persist_pairing` (ID:30). Scope never enters the invitation URI or QR.
- `claim_pairing` (ID:96-184) copies the challenge's vehicles into `device_vehicle_grants` inside the same IMMEDIATE transaction that inserts the device and deletes the challenge.
- `pair` currently needs the instance lock (`command_requires_user_hub_admission`, `src/application/main/migration.rs:4-21`), so it cannot run while the service serves. Decision: move `pair` to the `control` family (short SQLite transactions, no admission), as `paired-devices` and `revoke-device` already are. The GUI pairing screen (G2-8) then works while collection runs.
- A device with zero grants sees no vehicles and gets 404 on every vehicle route (fail closed). The nil loopback principal used when `require_device_auth` is false (SRV:2095-2106) bypasses grants; it is development-only.

## Enforcement

One helper, `require_vehicle_access(state, &device, vehicle_id)`, replaces `require_active_vehicle` (SRV:1298): active and granted, one read-only query, no caching, so revocation and de-grant take effect on the next request.

| Route | Today | Change |
| --- | --- | --- |
| `GET /v1/vehicles` (SRV:1284) | global `published_vehicles` | `published_vehicles_for_device(device_id)` joining the grants |
| `/current` (SRV:1328), `/drives` (SRV:1443), `/sync/signing-keys` (:1794), `/sync/prepared-artefacts` (:1827), `/sync/manifest` (:1667), `/sync/noop` (:1960), `changes-since` (`hub_sync_v1.rs:314`) | active only | access check first, before query parsing on `drives` so 400 cannot leak existence; failure keeps each route's existing not-found shape |
| `GET /v1/packs/sha256/{digest}` (SRV:1988) | bearer only | `pack_authorized_for_device(device_id, digest)`: resolve the pack's owning vehicle(s) through `sync_packs → sync_manifests`, `sync_retired_lineage_packs`, `pending_physical_v3_packs → admissions (serve_state public_first)`, `physical_v3_delta_packs → transitions`, `prepared_map_months`; authorise if any owner is granted; denied is an empty 404 identical to not-found, evaluated inside the lookup so the 406/503 branches (SRV:2049-2055) cannot act as an oracle |
| new pages (charges, positions, samples) | n/a | the same helper |

`control paired-devices` and `list_paired_devices` (ID:375) include each device's granted vehicles.

## Credential lifetime

- Absolute cap: `authenticate` and `rotate` clamp the sliding expiry to `max_expires_at_ms`. Default 180 days from pairing, owner-configurable; `expires_at_ms` in claim and rotate responses carries the clamped value.
- Write coalescing (csf_9811): update `expires_at_ms`/`last_authenticated_at_ms` only when the new value differs by more than 15 minutes from the stored one. Revocation checks stay on every request.
- Grace rotation (csf_9a86): on a successful rotation with the current bearer, store the new token sealed under a key derived from the presented (now grace) token in `sealed_successor` (AES-GCM, key = HKDF(grace token)). A rotate with a grace token inside the window returns the unsealed successor unchanged and mints nothing; outside the window, 401. Grace rows created before the migration have `sealed_successor` NULL and return 401 on a grace retry. Contract text for `hub-http-v1@1.2.0`: "A retry of rotate with the prior bearer during the grace window returns the same successor; it never issues a different token."
- Near the cap: a client that rotates early (HA rotates 7 days before expiry) would otherwise loop. Rotate returns the capped expiry and a `reason: "max_lifetime"` field; HA's coordinator stops rotating when `expires_at_ms == max`, surfaces "re-pair required", and the Hub GUI lists devices approaching the cap. HA's reauth flow (C-4) must not burn a single-use invitation on a failed attempt: the Hub accepts one failed claim per invitation without invalidating it (owner-visible in status).
- Self-revoke: `POST /v1/device/revoke` (bearer, 204, idempotent) calls `revoke_device_at(principal.device_id)`; TS client `logout()` and HA's unload call it before clearing local credentials; a failed call still clears locally and is retried on next start.

## GUI (G2-8)

A "Paired devices" row in the Settings "Connections & data" card (`HubNavigationBar.swift:444-492`), an embedded page modelled on `showEmbeddedServiceDetails` (`MainWindowController.swift:1069`), data through the existing CLI runner (`control paired-devices`, `control revoke-device <id>`), and a "Pair a device" sheet that runs `control pair --json --vehicle …` and renders the QR (`pairingUri`) with the selected cars shown.

## Tests

- Migration backfill; zero-grant device sees nothing; car-A device gets 404 for car-B's current, drives, charges, manifest, changes-since and a pack digest; de-grant takes effect on the next request; vehicles list filtered.
- Absolute cap reached: authenticate 401, rotate returns the capped value then 401 after; coalescing writes at most one UPDATE per 15 minutes under a 1 Hz client.
- Grace retry returns the identical successor; a second successor is never minted; pre-migration grace rows return 401.
- Revoke route idempotent; revoked device fails on JSON, sync and pack routes.
- Existing: `db/tests/catalogue_and_pairing.rs:569-762`, `server/tests.rs:2080, 2718, 3127` updated.
