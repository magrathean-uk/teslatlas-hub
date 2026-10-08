# Design: live-collected data on the signed sync wire

Status: G2-0 design, 2026-10-08. Not implemented. Revises the 2026-10-08 plan decision "the collector publishes physical changed-set packs" after reading the code; the plan row is updated to match this note. Anchors: `hub@6ac8ef0` and the preserved App client (`A/`).

## Problem

After a TeslaMate migration the App holds a PhysicalV3 checkpoint and follows physical heads. Physical heads are produced only by the importer from a sealed TeslaMate stage (`physical_publication.rs:139-300`); the live collector publishes lineage-2.1 deltas (`owner_supervision.rs:899-954`) which the App never requests (R05). Once Hub is the sole collector, live drives never reach the App through signed sync (PUB-05).

## Three walls in the current design

1. **Packs are bound to a head.** Every pack embeds `snapshot_id` and `head_sequence` (`sqlite_v2_2.rs:34-125`) and the App checks them (`hub_import_v22_stage.rs:2005-2008`), so a new head always means a full pack-set rewrite (about 126 packs for the household history).
2. **The wire is single-step.** The App requires `target.sequence == base + 1` (`hub_sync_v14.rs:166`), the Hub serves a stored transition only for `base + 1` (`hub_sync_v1.rs:838`), and only the transition whose target is the current head survives (the prior pending row is deleted, FK cascade on). An App two heads behind gets a full rebase (up to 1,771 chunks).
3. **No live-to-physical converter and no physical row store.** Live rows exist only as lineage-2.1 JSON (`materialised_*`); physical packs carry exact PostgreSQL shapes (`model.rs:949-1097`) with address and geofence id spaces that exist only inside the head packs; `charging_processes.position_id` is NOT NULL and live charges have no position id.

Consequence: "the collector publishes physical changed-set packs" as a per-commit operation is either O(history) per drive (rewrite the head) or needs a wire change. The plan's incremental-work target forbids the first.

## Decision

- **Target wire (hub-sync-v1 1.5.0, "live overlay"):** the import head stays as it is. Live changes become a second axis, `live_sequence`, served as the existing `teslatlas-physical-v3-delta-v1` changed-set format against the current head. The checkpoint carries `(receipt_id, head_sequence, live_sequence)`; `changes-since` returns the next live changed set when the head matches and `live_sequence` is behind, a `no_op` when current, `rebase_required` only when the head itself changed (a later import). After a bootstrap or rebase the App fetches the live overlay immediately. Cost is O(affected rows) per publication; no pack rewrite.
- **Not chosen:** rebuilding a full head from the prior packs plus a live overlay on every drive close (no wire change, but O(history) per publication and 24 h retention of every superseded head: the pattern the plan forbids). Also not chosen: making JSON reads the live path for the App (two sources of truth in the offline cache, and the App's delta-apply code would go unused).
- **Interim:** lineage-2.1 publication stays until App v7 ships the 1.5.0 path; both publications run meanwhile. Lineage is retired afterwards (plan Decisions, Live sync wire).

## Shared core (Hub)

- (a) **Physical live-row store** `physical_v3_live_rows(vehicle_id, table_name, row_id, row_blob, digest, live_revision)` with an id seeding step for `addresses` and `geofences` taken from the head packs at head activation, and a second consumer cursor `physical_live_cursor(vehicle_id, published_revision, head_receipt_id)` over `sync_mutations`, because `claim_sync_mutations`/`published` is single-consumer and lineage still needs it.
- (b) **Converter** from `materialised_*` rows (cars, car_settings, drives, positions, charges, charge_samples, states, updates) to `Projection*V2_2` physical rows. Only closed drives and charges are converted (the same boundary as live commits: open-drive positions produce no mutation, `geofences_and_terrain.rs:391-416`). A charge without a `position_id` gets the first position of its stationary span or a synthesised stationary position row; the receipt records it in `row_roles`.
- (c) **Trigger**: in `replay_export_outbox_blocking` (`owner_supervision.rs:810-861`) after the lineage publish, when the vehicle's physical state is `Public`, for mutations of drive close, charge close, state, update and car.
- (d) **Directed changed-set builder**: a constructor on `PhysicalDeltaComparison` that fills `row_versions` (side 1) and `edges` from the live store only, side 0 empty (live never deletes), then reuses `derive_changes`, `prepare_wire_scope`, `affected_months`, `DeltaOutput`, the receipt signer and `insert_physical_v3_delta_transition` with a new `live_sequence` column; `copy_target_context` reads roots from the head packs through `open_verified_physical_pack_2_2`. No scan of either head (removes the IMP-14 cost for live publication).
- (e) **Journal** `physical_v3_live_journal(vehicle_id, live_sequence, from_revision, to_revision, receipt_json, pack digests, published_at_ms)`, pruned by a watermark once every paired device has acknowledged past it or after the retention window; packs served through the existing pack handler with the vehicle-scope check.

## Protocol change (hub-sync-v1 1.5.0)

- Request adds `base_live_sequence`; receipt adds `live_sequence`, `from_live_sequence`, `to_live_sequence`; manifest adds `live_sequence` of the head's current overlay. `accepted_changed_set_formats` unchanged. Discovery capability token `sync.live`.
- Rules: a live changed set is valid only for the exact head `(receipt_id, manifest_sha256)`; `to_live_sequence == from + 1`; a later import rotates the head, resets `live_sequence` to 0 and folds the live rows into the new stage (below), so a client with an old head gets `rebase_required` as today.

## Importer interaction (the hard part)

- A later migrate captures a pure TeslaMate stage. Rotating it would tombstone every live-only row. Rule: the importer unions the live-row store into the sealed stage before publication (one pass, keyed by `(table, id)`), under the publication gate, and fences the live cursor at capture time so rows published after the fence are re-published against the new head.
- Id collisions: live ids are seeded from the import state (`projection.rs:811-890`); if TeslaMate kept writing after handover, its new ids can collide with live ids. Handover (G2-4) requires TeslaMate stopped before the final snapshot; the union step rejects a collision with a differing digest and blocks the rotation with an actionable report rather than merging.
- A drive captured both live and by TeslaMate is a duplicate; the union keeps the TeslaMate row (supplied values win) and tombstones the live row in the transition.

## App v7 (the App writer)

- Checkpoint stores `live_sequence` beside the `hub_sync_22_*` metadata; requests carry it; the `target == base + 1` rule becomes "same head, advancing live sequence"; `stage_and_verify_delta`, `project_and_apply_delta` and `invalidate_physical_delta_maps(affected_months)` are reused unchanged.

## Risks and tests

- Gate hold: live publication must never take the long rotation path; it runs under the existing flock gate but is bounded by the changed set size (minutes become milliseconds).
- Three sequence axes (physical head, lineage, mutation revision) plus `live_sequence`: a mapping table and an idempotent crash replay (60 s claim lease) are required; a test interrupts publication after the journal insert and before the pack commit and expects one logical result.
- Open drives: the converter publishes only closed roots; a test ends a drive after a restart and expects exactly one transition.
- Fidelity: a live row may differ from the TeslaMate row of the same drive at the next import; the union step's digest rule above decides, and the test imports a snapshot containing a drive already live-published.
- Retention: `sync_mutations` is never pruned today; the live journal gets its own watermark; an App offline longer than the retention window gets `rebase_required`, not an error.
- Acceptance (plan journey 7): a drive collected after handover appears in the App through `changes-since` without a bootstrap, with the affected month's prepared map invalidated and regenerated.
