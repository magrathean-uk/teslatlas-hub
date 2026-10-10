# Design: per-device vehicle scope and credential lifetime

Status: G2-0 design, 2026-10-08. Per-device vehicle grants (schema v68) are implemented in G2-3a and the Hub side of the credential lifetime in G2-3b, see "G2-3a as built" and "G2-3b as built (Hub)" at the end; the TypeScript client and Home Assistant parts of the lifetime design are later batches. Anchors: `hub@6ac8ef0` (`ID` = `src/storage/db/identity_and_audit.rs`, `MIG` = `src/storage/db/migrations.rs`, `SRV` = `src/api/server.rs`).

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
- Near the cap: a client that rotates early (HA rotates 7 days before expiry) would otherwise loop. When the stored expiry has reached the maximum, rotate answers `200` with the current bearer unchanged, the expiry unchanged and `reason: "max_lifetime"`, and issues and writes nothing (coordinator decision 6); HA keeps working through that no-op until the maximum and its reauth flow starts with the `401` after it. A later HA release stops rotating on `reason`, surfaces "re-pair required", and the Hub GUI lists devices approaching the cap. HA's reauth flow (C-4) must not burn a single-use invitation on a failed attempt: the Hub accepts one failed claim per invitation without invalidating it (owner-visible in status).
- Self-revoke: `POST /v1/device/revoke` (bearer, 204; a repeat is `401`, because the revoked bearer is dead on every route, and a client treats `401` as already revoked) calls `revoke_device_at(principal.device_id)`; TS client `logout()` and HA's unload call it before clearing local credentials; a failed call still clears locally and is retried on next start.

## GUI (G2-8)

A "Paired devices" row in the Settings "Connections & data" card (`HubNavigationBar.swift:444-492`), an embedded page modelled on `showEmbeddedServiceDetails` (`MainWindowController.swift:1069`), data through the existing CLI runner (`control paired-devices`, `control revoke-device <id>`), and a "Pair a device" sheet that runs `control pair --json --vehicle …` and renders the QR (`pairingUri`) with the selected cars shown.

## Tests

- Migration backfill; zero-grant device sees nothing; car-A device gets 404 for car-B's current, drives, charges, manifest, changes-since and a pack digest; de-grant takes effect on the next request; vehicles list filtered.
- Absolute cap reached: authenticate 401, rotate returns the current bearer with `reason: "max_lifetime"` then 401 after; coalescing writes at most one UPDATE per 15 minutes under a 1 Hz client.
- Grace retry returns the identical successor; a second successor is never minted; pre-migration grace rows return 401.
- Revoke route: a repeat is 401; revoked device fails on JSON, sync and pack routes.
- Existing: `db/tests/catalogue_and_pairing.rs:569-762`, `server/tests.rs:2080, 2718, 3127` updated.

## G2-3a as built

The implementation follows the sections above, with these differences recorded by the G2-3 decisions:

- The `vehicle_id` foreign keys of `device_vehicle_grants` and `pairing_challenge_vehicles` are `ON DELETE CASCADE`, not `RESTRICT`: placeholder cleanup deletes `vehicles` rows, and a grant for a deleted vehicle is simply gone (fail closed). Because of those cascades, **no later migration may rebuild (`DROP`) `vehicles`, `paired_devices` or `pairing_challenges` with foreign keys on**: the drop would cascade and empty the grants (and `paired_device_token_grace`). Use additive `ALTER TABLE` steps. The restore connection sets `PRAGMA foreign_keys = ON` explicitly and also deletes the grant tables, the grace table and the challenge vehicles before their parents, and `validate_pairing_authority_absent` counts each of them where the schema has it.
- `pairing_challenges.all_vehicles` (0 or 1) records the default scope: an invitation either lists vehicles or grants every vehicle that is active at claim time. Both claim forms skip retired vehicles, and `persist_pairing_with_scope` checks retirement again inside its IMMEDIATE transaction. A vehicle added later is never auto-granted; a vehicle retired at claim time is not granted either, so a device paired during a retirement does not gain the vehicle on reactivation.
- `require_vehicle_access(state, &device, vehicle_id, visibility)` takes a visibility: `PublishedAndActive` for `/v1/vehicles`, `/current` and the six history routes (the predicate of the vehicle list), `Active` for manifest, no-op, signing keys, prepared artefacts and changes-since (an active vehicle without a published full snapshot is the empty 404 `no_full_snapshot`, not an unknown vehicle). The nil UUID is `not found`: its value never causes a 503. The bypass for the loopback router is keyed on `require_device_auth == false` (the plain-loopback `serve` behind `LocalLoopbackAccess` and the test routers), not on development use.
- Sync routes and the pack parse the vehicle (segment 2), the prepared-artefact id (segment 5) and the pack object name (segment 3) from the raw path after authentication. Manifest, no-op and prepared artefacts answer a malformed id with the empty 404 and the nil UUID with the JSON `vehicle_not_found`; signing keys answer both with the empty 404, changes-since both with the JSON error.
- Pack ownership sits in each lookup's `WHERE`, before any `LIMIT`: `sync_packs` through `sync_manifests`, `sync_retired_lineage_packs`, `pending_physical_v3_packs` through the admission, `physical_v3_delta_packs` through the transition, and `prepared_map_months`. With no cursor key the prepared-pack lookup cannot verify a receipt, so the route asks the same candidate query whether a month exists in the device's scope: a candidate is the 503, anything else (an ungranted digest included) is the empty 404 of an unknown digest. Retirement is not a pack predicate: a device that still holds a grant keeps fetching the retired vehicle's catalogued packs, and only removing the grant withdraws them.
- `pair --json` is byte-compatible: it has no `vehicles` or `allVehicles` member. The text output names the scope, and `control paired-devices` lists each device's `vehicles`. `control pair` (same flags) runs through `HubStore::open_existing`, without the instance lock.
- The `max_expires_at_ms` column is written at claim (`claimed_at + 180 days`, one named constant) and backfilled by the migration with `max(created_at_ms, migration time) + 180 days`, saturating at `i64::MAX` (so a device paired more than 180 days before the upgrade is not over its cap the moment 3b first enforces it); G2-3b enforces it. `paired_device_token_grace.sealed_successor` is added for G2-3b's sealed successor.

## G2-3b as built (Hub)

- **Maximum lifetime.** `[pairing] device_max_lifetime_days` in the Hub configuration (1 to 3650, default 180) is the span a claim writes into `paired_devices.max_expires_at_ms`, counted from the claim, through `HubStore::claim_pairing_with_max_lifetime`; `claim_pairing` is the wrapper with the default. The TLS serve paths hand the configured span to the claim route; test routers use the default. A change applies to devices paired after the restart; a device keeps the maximum it was given. The first bearer never outlives the maximum (`min(claim + 30 days, maximum)`). A NULL maximum (only a hand-edited catalogue has one) means no cap.
- **Cap in the store.** `authenticate_device_at` and `rotate_device` read `max_expires_at_ms` themselves (`PairedDeviceRecord` is unchanged). A bearer, current or grace, is refused (`401`) once `now >= max_expires_at_ms`, whatever its stored expiry; every expiry they report or write is limited to the maximum.
- **Coalesced writes.** `authenticate_device_at` judges revocation, expiry and the maximum on every request from a fresh read-only connection. It writes only when the slid expiry would move by more than 15 minutes or the stored `last_authenticated_at_ms` is more than 15 minutes old or empty (`PAIRED_DEVICE_AUTH_WRITE_INTERVAL_MS`); the `UPDATE` repeats that decision against the row it locks (bearer, revocation, expiry, maximum and the 15-minute interval), so a burst of requests that all read the same stale row performs one write, and it clamps to the maximum and to the larger of the stored and new expiry, so concurrent slides stay monotonic. A request whose write finds nothing to do is answered from a new read: the row was refreshed meanwhile (the bearer is still current), a rotation replaced the bearer (it is the grace bearer now), or a revocation or expiry landed (`401`). The grace bearer never writes.
- **Rotation.** One consistent read decides: the current bearer with room below the maximum issues a successor (expiry `min(max(stored, now + 30 days), maximum)`), moves itself into the device's single grace row for 24 hours and seals the successor there; the grace bearer inside its window gets exactly the sealed successor back, if it is still the device's current bearer, and nothing is written, so a lost response is retried without minting; the current bearer of a device whose stored expiry has reached the maximum gets itself back with `reason: "max_lifetime"` and nothing is issued or written. Grace retries never carry `reason`. Only an issue takes the SQLite writer, and it decides again inside its transaction.
- **Sealed successor.** Key = HKDF-SHA256 (RFC 5869) over the crate's `hmac_sha256`: salt `teslatlas-hub grace successor v1`, input key material the presented bearer as transmitted, info the device id's text, one 32-byte block. The blob is a random 96-bit nonce followed by AES-256-GCM ciphertext and tag with the device id's text as associated data (92 bytes for a 64-character bearer). A NULL blob (a grace row from before v68), a blob that does not open, or a successor that is no longer the device's current bearer is `401`. Keys, pseudo-random keys and plaintext tokens are held in zeroizing buffers.
- **Self-revoke.** `POST /v1/device/revoke` is registered on the ordinary router with a CORS `POST` entry like rotate. Order: no paired-device authentication is `404`; no or an unknown, expired or revoked bearer is `401` (current or grace bearer accepted); any non-empty request body, of any size, is `400` and revokes nothing, never `413` (the body is read through `Request<Body>` with a one-byte limit, not `Bytes` or `Json`); then the revocation, `204` with `Cache-Control: no-store`. Discovery is unchanged in this batch: `auth.revoke` and `profile` are advertised by the later flip commit.
- **Claim device name.** A claim whose `device_name` is empty, over 128 UTF-8 bytes or holds a control character is `422` with a short `text/plain` body, before the invitation is consulted: the refusal consumes nothing and reveals nothing. Order: claims disabled `404`, name `422`, malformed pairing id `401`, then the store.
- **Accepted limits.** (1) The `UPDATE` of a coalesced write judges expiry and the maximum against the request's own clock reading, taken before it waited for the writer (the busy timeout is 5 s). A request that read a still valid row can therefore slide an expiry that passed while it waited; the slide is clamped to the maximum, so the maximum is never exceeded, and revocation is judged by the stored column, not the clock. The window is at most the busy timeout. (2) The claim and rotate `200` answers carry a bearer and send no `Cache-Control: no-store`, as before this batch; the 1.2.0 contract does not list the header for them, so adding it waits for the flip commit with a contract note. (3) The race tests rendezvous threads of one process through a test-only hook; there is no multi-process test. (4) The AES key schedule and the SHA-256 hasher states are not zeroized: enabling the crates' `zeroize` features needs a `Cargo.lock` change.
