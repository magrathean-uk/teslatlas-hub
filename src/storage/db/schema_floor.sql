-- SPDX-License-Identifier: AGPL-3.0-only

CREATE TABLE hub_metadata (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                ) STRICT;

CREATE TABLE sources (
                    source_id TEXT PRIMARY KEY NOT NULL,
                    source_kind TEXT NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation >= 1),
                    created_at_ms INTEGER NOT NULL
                ) STRICT;

CREATE TABLE sync_ledger (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    sequence INTEGER NOT NULL CHECK (sequence >= 1),
                    entity_kind TEXT NOT NULL,
                    entity_key TEXT NOT NULL,
                    operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),
                    committed_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (source_id, sequence, entity_kind, entity_key)
                ) STRICT;

CREATE TABLE sync_manifests (
                    snapshot_id TEXT PRIMARY KEY NOT NULL,
                    vehicle_id TEXT NOT NULL,
                    head_sequence INTEGER NOT NULL CHECK (head_sequence >= 0),
                    manifest_json BLOB NOT NULL
                ) STRICT;

CREATE TABLE sync_packs (
                    sha256 TEXT PRIMARY KEY NOT NULL,
                    snapshot_id TEXT NOT NULL REFERENCES sync_manifests(snapshot_id) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
                    relative_path TEXT NOT NULL,
                    compressed_bytes INTEGER NOT NULL CHECK (compressed_bytes > 0),
                    uncompressed_bytes INTEGER NOT NULL CHECK (uncompressed_bytes >= 100),
                    UNIQUE(snapshot_id, ordinal)
                ) STRICT;

CREATE TABLE source_identities (
                    source_id TEXT PRIMARY KEY NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    source_kind TEXT NOT NULL,
                    source_key TEXT NOT NULL,
                    UNIQUE(source_kind, source_key),
                    CHECK(length(CAST(source_kind AS BLOB)) BETWEEN 1 AND 64),
                    CHECK(length(CAST(source_key AS BLOB)) BETWEEN 1 AND 256)
                ) STRICT;

CREATE TABLE vehicles (
                    vehicle_id TEXT PRIMARY KEY NOT NULL,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    source_vehicle_key TEXT NOT NULL,
                    vin TEXT,
                    display_name TEXT,
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    last_seen_at_ms INTEGER NOT NULL CHECK(last_seen_at_ms >= created_at_ms), retired_at_ms INTEGER
                    CHECK(retired_at_ms IS NULL OR retired_at_ms >= created_at_ms),
                    UNIQUE(source_id, source_vehicle_key),
                    CHECK(length(CAST(source_vehicle_key AS BLOB)) BETWEEN 1 AND 256),
                    CHECK(vin IS NULL OR length(CAST(vin AS BLOB)) BETWEEN 1 AND 32),
                    CHECK(display_name IS NULL OR length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256)
                ) STRICT;

CREATE TABLE raw_observations (
                    observation_id INTEGER PRIMARY KEY,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms >= 0),
                    received_at_ms INTEGER NOT NULL CHECK(received_at_ms >= 0),
                    payload_sha256 BLOB NOT NULL CHECK(length(payload_sha256) = 32),
                    payload_json TEXT NOT NULL CHECK(json_valid(payload_json))
                        CHECK(length(CAST(payload_json AS BLOB)) <= 262144),
                    UNIQUE(source_id, vehicle_id, observed_at_ms, payload_sha256)
                ) STRICT;

CREATE TABLE pairing_challenges (
                    pairing_id TEXT PRIMARY KEY NOT NULL,
                    label TEXT NOT NULL,
                    secret_sha256 BLOB NOT NULL CHECK(length(secret_sha256) = 32),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > created_at_ms), all_vehicles INTEGER NOT NULL DEFAULT 0
                    CHECK(all_vehicles IN (0, 1)),
                    CHECK(length(CAST(label AS BLOB)) BETWEEN 1 AND 128)
                ) STRICT;

CREATE TABLE vehicle_lifecycle_state (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    last_observation_id INTEGER NOT NULL CHECK(last_observation_id >= 0),
                    open_session_json BLOB NOT NULL
                        CHECK(length(open_session_json) BETWEEN 2 AND 65536),
                    quarantined INTEGER NOT NULL DEFAULT 0 CHECK(quarantined IN (0, 1)),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
                ) STRICT;

CREATE TABLE materialised_drives (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    drive_id INTEGER NOT NULL CHECK(drive_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    drive_json TEXT NOT NULL CHECK(json_valid(drive_json)), inside_temp_avg REAL, power_max REAL, power_min REAL, start_ideal_range_km REAL, end_ideal_range_km REAL, ascent INTEGER, descent INTEGER,
                    PRIMARY KEY (vehicle_id, drive_id)
                ) STRICT;

CREATE TABLE materialised_charges (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    charge_id INTEGER NOT NULL CHECK(charge_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    charge_json TEXT NOT NULL CHECK(json_valid(charge_json)),
                    PRIMARY KEY (vehicle_id, charge_id)
                ) STRICT;

CREATE TABLE materialised_charge_samples (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    sample_id INTEGER NOT NULL CHECK(sample_id > 0),
                    charge_id INTEGER NOT NULL CHECK(charge_id > 0),
                    sample_json TEXT NOT NULL CHECK(json_valid(sample_json)),
                    PRIMARY KEY (vehicle_id, sample_id)
                ) STRICT;

CREATE TABLE snapshot_fingerprints (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    fingerprint_sha256 BLOB NOT NULL CHECK(length(fingerprint_sha256) = 32)
                , snapshot_id TEXT, head_sequence INTEGER) STRICT;

CREATE TABLE vehicle_snapshot_sequences (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    next_sequence INTEGER NOT NULL CHECK(next_sequence >= 2)
                ) STRICT;

CREATE TABLE materialised_positions (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    position_id INTEGER NOT NULL CHECK(position_id > 0),
                    drive_id INTEGER CHECK(drive_id IS NULL OR drive_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    position_json TEXT NOT NULL CHECK(json_valid(position_json)), battery_heater INTEGER
                    CHECK (battery_heater IN (0, 1)), battery_heater_on INTEGER
                    CHECK (battery_heater_on IN (0, 1)), battery_heater_no_power INTEGER
                    CHECK (battery_heater_no_power IN (0, 1)), speed INTEGER, power REAL, est_battery_range_km REAL, fan_status INTEGER, driver_temp_setting REAL, passenger_temp_setting REAL, is_climate_on INTEGER
                    CHECK (is_climate_on IN (0, 1)), is_rear_defroster_on INTEGER
                    CHECK (is_rear_defroster_on IN (0, 1)), is_front_defroster_on INTEGER
                    CHECK (is_front_defroster_on IN (0, 1)), tpms_pressure_fl REAL, tpms_pressure_fr REAL, tpms_pressure_rl REAL, tpms_pressure_rr REAL,
                    PRIMARY KEY (vehicle_id, position_id)
                ) STRICT;

CREATE TABLE materialised_states (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    state_id INTEGER NOT NULL CHECK(state_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    state_json TEXT NOT NULL CHECK(json_valid(state_json)),
                    PRIMARY KEY (vehicle_id, state_id)
                ) STRICT;

CREATE TABLE materialised_updates (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    update_id INTEGER NOT NULL CHECK(update_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    update_json TEXT NOT NULL CHECK(json_valid(update_json)),
                    PRIMARY KEY (vehicle_id, update_id)
                ) STRICT;

CREATE TABLE materialised_cars (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    car_json TEXT NOT NULL CHECK(json_valid(car_json))
                ) STRICT;

CREATE TABLE address_cache (
                    osm_type TEXT NOT NULL CHECK(length(CAST(osm_type AS BLOB)) BETWEEN 1 AND 32),
                    osm_id INTEGER NOT NULL CHECK(osm_id > 0),
                    display_name TEXT NOT NULL
                        CHECK(length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256),
                    name TEXT CHECK(name IS NULL OR length(CAST(name AS BLOB)) <= 256), latitude REAL CHECK(latitude IS NULL OR (latitude >= -90.0 AND latitude <= 90.0)), longitude REAL CHECK(longitude IS NULL OR (longitude >= -180.0 AND longitude <= 180.0)), house_number TEXT, road TEXT, neighbourhood TEXT, city TEXT, county TEXT, postcode TEXT, state TEXT, state_district TEXT, country TEXT, raw_json TEXT CHECK(raw_json IS NULL OR json_valid(raw_json)),
                    PRIMARY KEY(osm_type, osm_id)
                ) STRICT;

CREATE TABLE address_lookup_cache (
                    lookup_key TEXT PRIMARY KEY NOT NULL
                        CHECK(length(CAST(lookup_key AS BLOB)) BETWEEN 1 AND 64),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    osm_type TEXT NOT NULL,
                    osm_id INTEGER NOT NULL,
                    looked_up_at_ms INTEGER NOT NULL CHECK(looked_up_at_ms >= 0), provider TEXT
                    CHECK(provider IS NULL OR provider = 'nominatim'), endpoint TEXT
                    CHECK(endpoint IS NULL OR length(CAST(endpoint AS BLOB)) BETWEEN 1 AND 2048), endpoint_host TEXT
                    CHECK(endpoint_host IS NULL OR length(CAST(endpoint_host AS BLOB)) BETWEEN 1 AND 255), language TEXT
                    CHECK(language IS NULL OR length(CAST(language AS BLOB)) BETWEEN 1 AND 64), licence TEXT
                    CHECK(licence IS NULL OR length(CAST(licence AS BLOB)) BETWEEN 1 AND 512), display_name TEXT
                    CHECK(display_name IS NULL OR length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256), name TEXT
                    CHECK(name IS NULL OR length(CAST(name AS BLOB)) <= 256), result_latitude REAL
                    CHECK(result_latitude IS NULL OR (result_latitude >= -90.0 AND result_latitude <= 90.0)), result_longitude REAL
                    CHECK(result_longitude IS NULL OR (result_longitude >= -180.0 AND result_longitude <= 180.0)), house_number TEXT, road TEXT, neighbourhood TEXT, city TEXT, county TEXT, postcode TEXT, state TEXT, state_district TEXT, country TEXT, raw_json TEXT
                    CHECK(raw_json IS NULL OR json_valid(raw_json)),
                    FOREIGN KEY(osm_type, osm_id)
                        REFERENCES address_cache(osm_type, osm_id)
                ) STRICT;

CREATE TABLE address_enrichment_jobs (
                    job_key TEXT PRIMARY KEY NOT NULL
                        CHECK(length(CAST(job_key AS BLOB)) BETWEEN 1 AND 256),
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    target_type TEXT NOT NULL CHECK(target_type IN ('drive', 'charge')),
                    target_id INTEGER NOT NULL CHECK(target_id > 0),
                    field TEXT NOT NULL CHECK(field IN ('start_address', 'end_address', 'address')),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    status TEXT NOT NULL CHECK(status IN ('pending', 'running', 'retry', 'complete')),
                    attempts INTEGER NOT NULL CHECK(attempts >= 0),
                    next_attempt_ms INTEGER NOT NULL CHECK(next_attempt_ms >= 0),
                    lease_until_ms INTEGER NOT NULL CHECK(lease_until_ms >= 0),
                    completed_at_ms INTEGER,
                    last_error TEXT
                ) STRICT;

CREATE TABLE car_settings (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
                    use_streaming_api INTEGER NOT NULL CHECK(use_streaming_api IN (0, 1)),
                    suspend_after_idle_min INTEGER NOT NULL CHECK(suspend_after_idle_min > 0),
                    suspend_min INTEGER NOT NULL CHECK(suspend_min > 0),
                    req_not_unlocked INTEGER NOT NULL CHECK(req_not_unlocked IN (0, 1)),
                    free_supercharging INTEGER NOT NULL CHECK(free_supercharging IN (0, 1)),
                    lfp_battery INTEGER NOT NULL CHECK(lfp_battery IN (0, 1))
                , suspend_min_resolved INTEGER NOT NULL DEFAULT 1
                    CHECK(suspend_min_resolved IN (0, 1))) STRICT;

CREATE TABLE stream_watermarks (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    last_timestamp_ms INTEGER NOT NULL CHECK(last_timestamp_ms >= 0)
                ) STRICT;

CREATE TABLE lifecycle_source_watermarks (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    domain TEXT NOT NULL,
                    max_source_row_id INTEGER,
                    max_timestamp_ms INTEGER,
                    PRIMARY KEY(source_id, vehicle_id, domain)
                ) STRICT;

CREATE TABLE terrain_enrichment_state (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    cursor_position_id INTEGER NOT NULL DEFAULT 0
                        CHECK(cursor_position_id >= 0),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
                , import_position_id_max
             INTEGER NOT NULL DEFAULT 0 CHECK(import_position_id_max >= 0), import_position_date_max_ms
             INTEGER NOT NULL DEFAULT 0 CHECK(import_position_date_max_ms >= 0)) STRICT;

CREATE TABLE terrain_elevation_provenance (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    position_id INTEGER NOT NULL CHECK(position_id > 0),
                    drive_id INTEGER NOT NULL CHECK(drive_id > 0),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    elevation_m INTEGER,
                    tile_name TEXT,
                    tile_hash TEXT,
                    dataset_source TEXT,
                    dataset_version TEXT,
                    status TEXT NOT NULL CHECK(status IN ('success', 'void', 'failed')),
                    error_code TEXT,
                    attempts INTEGER NOT NULL CHECK(attempts >= 1),
                    attempted_at_ms INTEGER NOT NULL CHECK(attempted_at_ms >= 0),
                    retry_after_ms INTEGER NOT NULL CHECK(retry_after_ms >= 0),
                    PRIMARY KEY(vehicle_id, position_id)
                ) STRICT;

CREATE TABLE vehicle_identity_aliases (
                    alias_kind TEXT NOT NULL CHECK(alias_kind IN ('source_key', 'tesla_eid', 'tesla_vid', 'vin')),
                    alias_value TEXT NOT NULL,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    source_vehicle_key TEXT NOT NULL,
                    PRIMARY KEY(alias_kind, alias_value),
                    CHECK(length(CAST(alias_value AS BLOB)) BETWEEN 1 AND 256),
                    CHECK(length(CAST(source_vehicle_key AS BLOB)) BETWEEN 1 AND 256)
                ) STRICT;

CREATE TABLE sync_bases (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    snapshot_id TEXT NOT NULL UNIQUE,
                    base_sequence INTEGER NOT NULL CHECK(base_sequence >= 0),
                    base_digest TEXT NOT NULL CHECK(length(base_digest) = 64),
                    packs_json BLOB NOT NULL
                ) STRICT;

CREATE TABLE sync_deltas (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    from_sequence INTEGER NOT NULL CHECK(from_sequence >= 0),
                    to_sequence INTEGER NOT NULL CHECK(to_sequence > from_sequence),
                    parent_chain_digest TEXT NOT NULL CHECK(length(parent_chain_digest) = 64),
                    chain_digest TEXT NOT NULL CHECK(length(chain_digest) = 64),
                    pack_digest TEXT NOT NULL CHECK(length(pack_digest) = 64),
                    pack_json BLOB NOT NULL,
                    PRIMARY KEY(vehicle_id, from_sequence, to_sequence),
                    UNIQUE(vehicle_id, chain_digest)
                ) STRICT;

CREATE TABLE sync_heads (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    base_snapshot_id TEXT NOT NULL REFERENCES sync_bases(snapshot_id) ON DELETE RESTRICT,
                    head_sequence INTEGER NOT NULL CHECK(head_sequence >= 0),
                    head_digest TEXT NOT NULL CHECK(length(head_digest) = 64),
                    terminal_cursor TEXT NOT NULL
                ) STRICT;

CREATE TABLE import_generations (
                    run_id TEXT PRIMARY KEY NOT NULL,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    status TEXT NOT NULL CHECK(status IN ('staging', 'promoting')),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0)
                , base_last_observation_id
                    INTEGER NOT NULL DEFAULT 0 CHECK(base_last_observation_id >= 0), base_updated_at_ms
                    INTEGER NOT NULL DEFAULT 0 CHECK(base_updated_at_ms >= 0)) STRICT;

CREATE TABLE import_generation_sessions (
                    run_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES import_generations(run_id) ON DELETE CASCADE,
                    session_json TEXT NOT NULL CHECK(json_valid(session_json))
                ) STRICT;

CREATE TABLE export_outbox (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    dirty_revision INTEGER NOT NULL CHECK(dirty_revision > 0),
                    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
                    next_attempt_ms INTEGER NOT NULL DEFAULT 0 CHECK(next_attempt_ms >= 0),
                    claimed_until_ms INTEGER NOT NULL DEFAULT 0 CHECK(claimed_until_ms >= 0),
                    last_error TEXT
                ) STRICT;

CREATE TABLE sync_mutation_sequences (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    next_revision INTEGER NOT NULL CHECK(next_revision > 0)
                ) STRICT;

CREATE TABLE sync_mutations (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    revision INTEGER NOT NULL CHECK(revision > 0),
                    entity TEXT NOT NULL CHECK(entity IN
                        ('car', 'car_setting', 'geofence', 'address', 'drive',
                         'position', 'charge', 'charge_sample', 'state', 'update')),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    operation TEXT NOT NULL CHECK(operation IN ('upsert', 'tombstone')),
                    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
                    published INTEGER NOT NULL DEFAULT 0 CHECK(published IN (0, 1)),
                    claimed_until_ms INTEGER NOT NULL DEFAULT 0 CHECK(claimed_until_ms >= 0),
                    PRIMARY KEY(vehicle_id, revision)
                ) STRICT;

CREATE TABLE v2_base_bindings (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    snapshot_id TEXT NOT NULL UNIQUE
                        REFERENCES sync_bases(snapshot_id) ON DELETE CASCADE,
                    installation_id TEXT NOT NULL CHECK(length(installation_id) = 36),
                    account_id TEXT NOT NULL CHECK(length(account_id) = 36),
                    generation INTEGER NOT NULL CHECK(generation >= 1),
                    selected_car_id INTEGER NOT NULL CHECK(selected_car_id > 0)
                ) STRICT;

CREATE TABLE "import_projection_heads" (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    base_snapshot_id TEXT NOT NULL UNIQUE
                        REFERENCES sync_bases(snapshot_id) ON DELETE CASCADE,
                    selected_car_id INTEGER NOT NULL CHECK(selected_car_id > 0),
                    head_sequence INTEGER NOT NULL CHECK(head_sequence >= 0)
                ) STRICT;

CREATE TABLE "import_projection_state_heads" (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    base_snapshot_id TEXT NOT NULL
                        REFERENCES sync_bases(snapshot_id) ON DELETE CASCADE,
                    selected_car_id INTEGER NOT NULL CHECK(selected_car_id > 0),
                    head_sequence INTEGER NOT NULL CHECK(head_sequence >= 0)
                ) STRICT;

CREATE TABLE "import_projection_state_bridges" (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    base_snapshot_id TEXT NOT NULL
                        REFERENCES sync_bases(snapshot_id) ON DELETE CASCADE,
                    head_sequence INTEGER NOT NULL CHECK(head_sequence >= 0),
                    algorithm TEXT NOT NULL CHECK(algorithm = 'logical_projection_v1'),
                    legacy_fingerprint_sha256 BLOB NOT NULL
                        CHECK(length(legacy_fingerprint_sha256) = 32),
                    logical_fingerprint_sha256 BLOB NOT NULL
                        CHECK(length(logical_fingerprint_sha256) = 32)
                ) STRICT;

CREATE TABLE "import_projection_rows" (
                    vehicle_id TEXT NOT NULL
                        REFERENCES "import_projection_heads"(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'drive', 'position', 'charge', 'charge_sample', 'state', 'update'
                    )),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    PRIMARY KEY(vehicle_id, entity, entity_id)
                ) STRICT;

CREATE TABLE "import_projection_state_rows" (
                    vehicle_id TEXT NOT NULL
                        REFERENCES "import_projection_state_heads"(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'car', 'drive', 'position', 'charge', 'charge_sample', 'state', 'update'
                    )),
                    entity_ordinal INTEGER NOT NULL CHECK(entity_ordinal BETWEEN 0 AND 6),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    projection_sha256 BLOB NOT NULL CHECK(length(projection_sha256) = 32),
                    CHECK(
                        (entity = 'car' AND entity_ordinal = 0) OR
                        (entity = 'drive' AND entity_ordinal = 1) OR
                        (entity = 'position' AND entity_ordinal = 2) OR
                        (entity = 'charge' AND entity_ordinal = 3) OR
                        (entity = 'charge_sample' AND entity_ordinal = 4) OR
                        (entity = 'state' AND entity_ordinal = 5) OR
                        (entity = 'update' AND entity_ordinal = 6)
                    ),
                    PRIMARY KEY(vehicle_id, entity_ordinal, entity_id),
                    UNIQUE(vehicle_id, entity, entity_id)
                ) STRICT, WITHOUT ROWID;

CREATE TABLE sync_live_delta_spans (
                    vehicle_id TEXT NOT NULL,
                    from_sequence INTEGER NOT NULL CHECK(from_sequence >= 0),
                    to_sequence INTEGER NOT NULL CHECK(to_sequence > from_sequence),
                    from_revision INTEGER NOT NULL CHECK(from_revision > 0),
                    to_revision INTEGER NOT NULL CHECK(to_revision >= from_revision),
                    pack_digest TEXT NOT NULL CHECK(length(pack_digest) = 64),
                    PRIMARY KEY(vehicle_id, from_sequence, to_sequence),
                    FOREIGN KEY(vehicle_id, from_sequence, to_sequence)
                        REFERENCES sync_deltas(vehicle_id, from_sequence, to_sequence)
                        ON DELETE CASCADE,
                    CHECK(to_sequence - from_sequence = to_revision - from_revision + 1)
                ) STRICT;

CREATE TABLE stream_session_receipts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                    vehicle_tesla_id INTEGER NOT NULL CHECK(vehicle_tesla_id > 0),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    outcome TEXT NOT NULL CHECK(outcome IN (
                        'started', 'orderly_shutdown',
                        'cancelled_before_subscription', 'transport_ended', 'failed'
                    )),
                    unsubscribe_receipt_id INTEGER,
                    CHECK(
                        (outcome = 'started'
                         AND completed_at_ms IS NULL AND duration_ms IS NULL
                         AND unsubscribe_receipt_id IS NULL)
                        OR (outcome = 'orderly_shutdown'
                            AND completed_at_ms IS NOT NULL AND duration_ms IS NOT NULL
                            AND completed_at_ms >= started_at_ms AND duration_ms >= 0
                            AND unsubscribe_receipt_id IS NOT NULL)
                        OR (outcome IN (
                                'cancelled_before_subscription', 'transport_ended', 'failed'
                            )
                            AND completed_at_ms IS NOT NULL AND duration_ms IS NOT NULL
                            AND completed_at_ms >= started_at_ms AND duration_ms >= 0
                            AND unsubscribe_receipt_id IS NULL)
                    )
                ) STRICT;

CREATE TABLE sync_retired_lineages (
                    vehicle_id TEXT NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    head_digest TEXT NOT NULL CHECK(length(head_digest) = 64),
                    manifest_json BLOB NOT NULL CHECK(length(manifest_json) > 0),
                    retired_at_ms INTEGER NOT NULL CHECK(retired_at_ms >= 0),
                    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > retired_at_ms),
                    PRIMARY KEY(vehicle_id, head_digest)
                ) STRICT;

CREATE TABLE sync_retired_lineage_packs (
                    vehicle_id TEXT NOT NULL,
                    head_digest TEXT NOT NULL,
                    pack_digest TEXT NOT NULL CHECK(length(pack_digest) = 64),
                    relative_path TEXT NOT NULL,
                    compressed_bytes INTEGER NOT NULL CHECK(compressed_bytes > 0),
                    PRIMARY KEY(vehicle_id, head_digest, pack_digest),
                    FOREIGN KEY(vehicle_id, head_digest)
                        REFERENCES sync_retired_lineages(vehicle_id, head_digest)
                        ON DELETE CASCADE
                ) STRICT;

CREATE TABLE supervised_collector_lease (
                    singleton_id INTEGER PRIMARY KEY NOT NULL
                        CHECK(singleton_id = 1),
                    instance_id TEXT NOT NULL CHECK(length(instance_id) = 36),
                    state TEXT NOT NULL CHECK(state IN ('active', 'auth_terminal')),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    heartbeat_at_ms INTEGER NOT NULL
                        CHECK(heartbeat_at_ms >= started_at_ms),
                    lease_until_ms INTEGER NOT NULL
                        CHECK(lease_until_ms > heartbeat_at_ms)
                ) STRICT;

CREATE TABLE legacy_refresh_input_fences (
                    input_credential_generation TEXT PRIMARY KEY COLLATE NOCASE
                        CHECK(length(input_credential_generation) = 36)
                ) STRICT, WITHOUT ROWID;

CREATE TABLE raw_observation_prune_guard (
                    singleton INTEGER PRIMARY KEY CHECK(singleton = 1)
                ) STRICT;

CREATE TABLE "paired_devices" (
                    device_id TEXT PRIMARY KEY NOT NULL,
                    display_name TEXT NOT NULL,
                    token_sha256 BLOB NOT NULL UNIQUE CHECK(length(token_sha256) = 32),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > created_at_ms),
                    revoked_at_ms INTEGER,
                    last_authenticated_at_ms INTEGER, max_expires_at_ms INTEGER
                    CHECK(max_expires_at_ms IS NULL OR max_expires_at_ms >= created_at_ms),
                    CHECK(revoked_at_ms IS NULL OR revoked_at_ms >= created_at_ms),
                    CHECK(last_authenticated_at_ms IS NULL OR last_authenticated_at_ms >= created_at_ms),
                    CHECK(length(CAST(display_name AS BLOB)) BETWEEN 1 AND 128)
                ) STRICT;

CREATE TABLE "legacy_owner_tokens" (
                    singleton_id INTEGER PRIMARY KEY NOT NULL CHECK(singleton_id = 1),
                    access BLOB NOT NULL CHECK(length(access) > 0),
                    refresh BLOB NOT NULL CHECK(length(refresh) > 0),
                    expires_at INTEGER NOT NULL CHECK(expires_at >= 0),
                    next_refresh_at INTEGER NOT NULL CHECK(next_refresh_at >= 0),
                    credential_generation TEXT
                        CHECK(credential_generation IS NULL
                              OR length(credential_generation) = 36),
                    CHECK(
                        (expires_at = 0 AND next_refresh_at = 0)
                        OR (expires_at > next_refresh_at AND next_refresh_at > 0)
                    )
                 ) STRICT;

CREATE TABLE "lifecycle_open_rows" (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    source_table TEXT NOT NULL,
                    source_row_id INTEGER NOT NULL CHECK(source_row_id > 0),
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    domain TEXT NOT NULL CHECK(domain IN (
                        'drive', 'position', 'charge', 'charge_sample', 'state',
                        'standalone_position'
                    )),
                    parent_source_row_id INTEGER,
                    row_json TEXT NOT NULL CHECK(json_valid(row_json)),
                    PRIMARY KEY(source_id, vehicle_id, source_table, source_row_id)
                ) STRICT;

CREATE TABLE "outbound_request_receipts" (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    vehicle_tesla_id INTEGER CHECK(vehicle_tesla_id > 0),
                    transport TEXT NOT NULL CHECK(transport IN (
                        'owner_api', 'fleet_api', 'stream', 'legacy_auth'
                    )),
                    operation TEXT NOT NULL CHECK(operation IN (
                        'products', 'vehicle_probe', 'vehicle_data',
                        'vehicle_wake', 'vehicle_command', 'token_refresh',
                        'stream_connect', 'stream_subscribe', 'stream_unsubscribe'
                    )),
                    safety_class TEXT NOT NULL CHECK(safety_class IN (
                        'non_wake_endpoint', 'conditional_read',
                        'direct_wake_command', 'explicit_vehicle_command'
                    )),
                    precondition TEXT NOT NULL CHECK(precondition IN (
                        'not_required', 'stream_power_confirmed'
                    )),
                    outcome TEXT NOT NULL CHECK(outcome IN (
                        'started', 'success', 'http_error', 'timeout',
                        'transport_error', 'authentication_rejected',
                        'protocol_error', 'response_too_large', 'cancelled'
                    )),
                    http_status INTEGER CHECK(http_status BETWEEN 100 AND 599),
                    retry_after_seconds INTEGER CHECK(retry_after_seconds >= 0),
                    CHECK(
                        (outcome = 'started' AND completed_at_ms IS NULL
                         AND duration_ms IS NULL AND http_status IS NULL
                         AND retry_after_seconds IS NULL)
                        OR
                        (outcome <> 'started' AND completed_at_ms IS NOT NULL
                         AND duration_ms IS NOT NULL
                         AND completed_at_ms >= started_at_ms
                         AND duration_ms >= 0)
                    )
                ) STRICT;

CREATE TABLE "legacy_refresh_receipt_bindings" (
                    receipt_id INTEGER PRIMARY KEY NOT NULL
                        REFERENCES "outbound_request_receipts"(id) ON DELETE CASCADE,
                    attempt_id TEXT NOT NULL UNIQUE CHECK(length(attempt_id) = 36),
                    input_credential_generation TEXT NOT NULL
                        CHECK(length(input_credential_generation) = 36),
                    output_credential_generation TEXT
                        CHECK(output_credential_generation IS NULL
                              OR length(output_credential_generation) = 36),
                    CHECK(output_credential_generation IS NULL
                          OR output_credential_generation <> input_credential_generation)
                ) STRICT;

CREATE TABLE fleet_tokens (
                    singleton_id INTEGER PRIMARY KEY NOT NULL CHECK(singleton_id = 1),
                    access BLOB NOT NULL CHECK(length(access) BETWEEN 1 AND 16424),
                    refresh BLOB NOT NULL CHECK(length(refresh) BETWEEN 1 AND 16424),
                    client_id TEXT NOT NULL
                        CHECK(length(CAST(client_id AS BLOB)) BETWEEN 1 AND 255),
                    region TEXT NOT NULL CHECK(region IN ('na', 'eu', 'cn')),
                    expires_at INTEGER NOT NULL CHECK(expires_at > 0),
                    next_refresh_at INTEGER NOT NULL
                        CHECK(next_refresh_at > 0 AND next_refresh_at < expires_at)
                , credential_generation TEXT
                    CHECK(credential_generation IS NULL
                          OR length(credential_generation) = 36)) STRICT;

CREATE TABLE fleet_refresh_receipt_bindings (
                    receipt_id INTEGER PRIMARY KEY NOT NULL
                        REFERENCES outbound_request_receipts(id) ON DELETE CASCADE,
                    attempt_id TEXT NOT NULL UNIQUE CHECK(length(attempt_id) = 36),
                    input_credential_generation TEXT NOT NULL
                        CHECK(length(input_credential_generation) = 36),
                    output_credential_generation TEXT
                        CHECK(output_credential_generation IS NULL
                              OR length(output_credential_generation) = 36),
                    CHECK(output_credential_generation IS NULL
                          OR output_credential_generation <> input_credential_generation)
                ) STRICT;

CREATE TABLE fleet_refresh_input_fences (
                    input_credential_generation TEXT PRIMARY KEY COLLATE NOCASE
                        CHECK(length(input_credential_generation) = 36)
                ) STRICT, WITHOUT ROWID;

CREATE TABLE paired_device_token_grace (
                    device_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES paired_devices(device_id) ON DELETE CASCADE,
                    token_sha256 BLOB NOT NULL UNIQUE CHECK(length(token_sha256) = 32),
                    valid_until_ms INTEGER NOT NULL CHECK(valid_until_ms >= 0)
                , sealed_successor BLOB) STRICT;

CREATE TABLE pending_physical_v3_admissions (
                    vehicle_id TEXT PRIMARY KEY NOT NULL,
                    snapshot_id TEXT NOT NULL UNIQUE,
                    installation_id TEXT NOT NULL,
                    account_id TEXT NOT NULL,
                    selected_car_id INTEGER NOT NULL
                        CHECK(selected_car_id BETWEEN -32768 AND 32767),
                    profile TEXT NOT NULL
                        CHECK(profile = 'hub-sync-v1@1.3.0'),
                    head_sequence INTEGER NOT NULL
                        CHECK(head_sequence BETWEEN 0 AND 9007199254740991),
                    chunk_count INTEGER NOT NULL
                        CHECK(chunk_count BETWEEN 1 AND 1771),
                    manifest_sha256 TEXT NOT NULL
                        CHECK(length(manifest_sha256) = 64),
                    ordered_chunks_sha256 TEXT NOT NULL
                        CHECK(length(ordered_chunks_sha256) = 64),
                    receipt_id TEXT NOT NULL UNIQUE
                        CHECK(length(receipt_id) = 68),
                    manifest_json BLOB NOT NULL
                        CHECK(length(manifest_json) BETWEEN 2 AND 2097152), serve_state TEXT NOT NULL DEFAULT 'public_first'
                    CHECK(serve_state IN ('public_first', 'blocked_rotation')),
                    CHECK(length(vehicle_id) = 36),
                    CHECK(length(snapshot_id) = 36),
                    CHECK(length(installation_id) = 36),
                    CHECK(length(account_id) = 36)
                ) STRICT;

CREATE TABLE pending_physical_v3_packs (
                    sha256 TEXT PRIMARY KEY NOT NULL CHECK(length(sha256) = 64),
                    snapshot_id TEXT NOT NULL
                        REFERENCES pending_physical_v3_admissions(snapshot_id)
                        ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 0 AND 1770),
                    relative_path TEXT NOT NULL,
                    compressed_bytes INTEGER NOT NULL
                        CHECK(compressed_bytes BETWEEN 1 AND 16777216),
                    uncompressed_bytes INTEGER NOT NULL CHECK(uncompressed_bytes >= 100),
                    UNIQUE(snapshot_id, ordinal)
                ) STRICT;

CREATE TABLE "retained_physical_v3_admissions" (
                    vehicle_id TEXT NOT NULL,
                    snapshot_id TEXT NOT NULL,
                    installation_id TEXT NOT NULL,
                    account_id TEXT NOT NULL,
                    generation INTEGER NOT NULL
                        CHECK(generation BETWEEN 1 AND 9007199254740991),
                    selected_car_id INTEGER NOT NULL
                        CHECK(selected_car_id BETWEEN -32768 AND 32767),
                    profile TEXT NOT NULL CHECK(profile = 'hub-sync-v1@1.3.0'),
                    head_sequence INTEGER NOT NULL
                        CHECK(head_sequence BETWEEN 1 AND 9007199254740991),
                    chunk_count INTEGER NOT NULL CHECK(chunk_count BETWEEN 1 AND 1771),
                    manifest_sha256 TEXT NOT NULL CHECK(length(manifest_sha256) = 64),
                    ordered_chunks_sha256 TEXT NOT NULL
                        CHECK(length(ordered_chunks_sha256) = 64),
                    receipt_id TEXT PRIMARY KEY NOT NULL CHECK(length(receipt_id) = 68),
                    retained_at_ms INTEGER NOT NULL
                        CHECK(retained_at_ms BETWEEN 0 AND 9007199254740991),
                    expires_at_ms INTEGER NOT NULL
                        CHECK(expires_at_ms BETWEEN 1 AND 9007199254740991),
                    manifest_json BLOB NOT NULL CHECK(length(manifest_json) BETWEEN 2 AND 2097152),
                    UNIQUE(vehicle_id, head_sequence),
                    CHECK(expires_at_ms > retained_at_ms),
                    CHECK(length(vehicle_id) = 36),
                    CHECK(length(snapshot_id) = 36),
                    CHECK(length(installation_id) = 36),
                    CHECK(length(account_id) = 36)
                 ) STRICT;

CREATE TABLE "retained_physical_v3_packs" (
                    receipt_id TEXT NOT NULL
                        REFERENCES "retained_physical_v3_admissions"(receipt_id)
                        ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 0 AND 1770),
                    sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
                    relative_path TEXT NOT NULL,
                    compressed_bytes INTEGER NOT NULL
                        CHECK(compressed_bytes BETWEEN 1 AND 16777216),
                    uncompressed_bytes INTEGER NOT NULL CHECK(uncompressed_bytes >= 100),
                    PRIMARY KEY(receipt_id, ordinal),
                    UNIQUE(receipt_id, sha256)
                 ) STRICT, WITHOUT ROWID;

CREATE TABLE prepared_map_months (
                    artifact_id TEXT PRIMARY KEY NOT NULL,
                    vehicle_id TEXT NOT NULL,
                    month TEXT NOT NULL,
                    input_manifest_id TEXT NOT NULL,
                    input_receipt_id TEXT NOT NULL,
                    input_sequence INTEGER NOT NULL CHECK(input_sequence >= 1),
                    source_digest TEXT NOT NULL CHECK(length(source_digest) = 64),
                    pack_sha256 TEXT NOT NULL CHECK(length(pack_sha256) = 64),
                    compressed_bytes INTEGER NOT NULL CHECK(compressed_bytes BETWEEN 1 AND 16777216),
                    receipt_json BLOB NOT NULL CHECK(length(receipt_json) BETWEEN 2 AND 2097152),
                    UNIQUE(vehicle_id, input_receipt_id, month)
                 ) STRICT;

CREATE TABLE physical_v3_delta_transitions (
                    target_receipt_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES pending_physical_v3_admissions(receipt_id) ON DELETE CASCADE,
                    base_receipt_id TEXT NOT NULL
                        REFERENCES retained_physical_v3_admissions(receipt_id) ON DELETE CASCADE,
                    vehicle_id TEXT NOT NULL,
                    base_manifest_signed_sha256 TEXT NOT NULL CHECK(length(base_manifest_signed_sha256)=64),
                    target_manifest_signed_sha256 TEXT NOT NULL CHECK(length(target_manifest_signed_sha256)=64),
                    receipt_json BLOB NOT NULL CHECK(length(receipt_json) BETWEEN 2 AND 2097152),
                    chunk_count INTEGER NOT NULL CHECK(chunk_count BETWEEN 1 AND 64),
                    total_compressed_bytes INTEGER NOT NULL CHECK(total_compressed_bytes BETWEEN 1 AND 268435456),
                    total_uncompressed_bytes INTEGER NOT NULL CHECK(total_uncompressed_bytes BETWEEN 1 AND 2147483648),
                    total_rows INTEGER NOT NULL CHECK(total_rows BETWEEN 1 AND 2000000),
                    impacted_roots_count INTEGER NOT NULL CHECK(impacted_roots_count BETWEEN 0 AND 10000),
                    UNIQUE(vehicle_id,base_receipt_id,target_receipt_id)
                 ) STRICT;

CREATE TABLE physical_v3_delta_packs (
                    target_receipt_id TEXT NOT NULL
                        REFERENCES physical_v3_delta_transitions(target_receipt_id) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 0 AND 63),
                    sha256 TEXT NOT NULL CHECK(length(sha256)=64),
                    relative_path TEXT NOT NULL,
                    compressed_bytes INTEGER NOT NULL CHECK(compressed_bytes BETWEEN 1 AND 16777216),
                    uncompressed_bytes INTEGER NOT NULL CHECK(uncompressed_bytes BETWEEN 1 AND 268435456),
                    PRIMARY KEY(target_receipt_id,ordinal)
                 ) STRICT, WITHOUT ROWID;

CREATE TABLE device_vehicle_grants (
                    device_id TEXT NOT NULL
                        REFERENCES paired_devices(device_id) ON DELETE CASCADE,
                    vehicle_id TEXT NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    granted_at_ms INTEGER NOT NULL CHECK(granted_at_ms >= 0),
                    PRIMARY KEY (device_id, vehicle_id)
                ) STRICT, WITHOUT ROWID;

CREATE TABLE pairing_challenge_vehicles (
                    pairing_id TEXT NOT NULL
                        REFERENCES pairing_challenges(pairing_id) ON DELETE CASCADE,
                    vehicle_id TEXT NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    PRIMARY KEY (pairing_id, vehicle_id)
                ) STRICT, WITHOUT ROWID;

CREATE TABLE terrain_tile_digests (
                    tile_name TEXT PRIMARY KEY NOT NULL
                        CHECK(tile_name GLOB '[NS][0-9][0-9][EW][0-9][0-9][0-9]'),
                    sha256 TEXT NOT NULL
                        CHECK(length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
                    recorded_at_ms INTEGER NOT NULL CHECK(recorded_at_ms >= 0)
                ) STRICT;

CREATE TABLE collector_account_state (
                    singleton_id INTEGER PRIMARY KEY CHECK(singleton_id = 1),
                    mode TEXT NOT NULL CHECK(mode IN ('legacy', 'provider', 'fleet_polling')),
                    config_fingerprint TEXT NOT NULL CHECK(length(config_fingerprint) = 64),
                    state TEXT NOT NULL CHECK(state IN ('collecting', 'owner_api_refused',
                        'provider_refused', 'fleet_access_refused', 'billing_suspended',
                        'cap_reached', 'config_error', 'account_locked')),
                    reason_code TEXT NOT NULL CHECK(length(reason_code) BETWEEN 1 AND 64),
                    since_ms INTEGER NOT NULL CHECK(since_ms >= 0),
                    rate_limited_until_ms INTEGER
                        CHECK(rate_limited_until_ms IS NULL OR rate_limited_until_ms >= 0),
                    next_probe_ms INTEGER CHECK(next_probe_ms IS NULL OR next_probe_ms >= 0),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
                ) STRICT;

CREATE TABLE collector_vehicle_state (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    phase TEXT NOT NULL CHECK(phase IN ('unknown', 'asleep', 'offline',
                        'online', 'suspended', 'driving', 'charging', 'updating')),
                    hold TEXT NOT NULL CHECK(hold IN ('none', 'disabled', 'service',
                        'paused', 'removed')),
                    reason_code TEXT NOT NULL CHECK(length(reason_code) BETWEEN 1 AND 64),
                    phase_since_ms INTEGER NOT NULL CHECK(phase_since_ms >= 0),
                    suspended_until_ms INTEGER
                        CHECK(suspended_until_ms IS NULL OR suspended_until_ms >= 0),
                    next_probe_ms INTEGER CHECK(next_probe_ms IS NULL OR next_probe_ms >= 0),
                    last_wake_read_at_ms INTEGER
                        CHECK(last_wake_read_at_ms IS NULL OR last_wake_read_at_ms >= 0),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
                ) STRICT;

CREATE TABLE collector_request_counters (
                    month TEXT NOT NULL CHECK(length(month) = 7),
                    mode TEXT NOT NULL CHECK(mode IN ('legacy', 'provider', 'fleet_polling')),
                    category TEXT NOT NULL CHECK(category IN ('data', 'state', 'wake', 'command')),
                    sent INTEGER NOT NULL CHECK(sent >= 0),
                    billable INTEGER NOT NULL CHECK(billable >= 0 AND billable <= sent),
                    reserved INTEGER NOT NULL CHECK(reserved >= billable),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0),
                    PRIMARY KEY(month, mode, category)
                ) STRICT, WITHOUT ROWID;

CREATE TABLE collection_gaps (
                    gap_id INTEGER PRIMARY KEY,
                    vehicle_id TEXT REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    kind TEXT NOT NULL CHECK(kind IN ('hub_down', 'hub_stopped',
                        'host_suspended', 'cap_reached', 'owner_api_refused',
                        'provider_refused', 'fleet_access_refused', 'billing_suspended',
                        'config_error', 'account_locked', 'auth_terminal', 'vehicle_paused',
                        'rate_limited', 'tesla_unreachable')),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    ended_at_ms INTEGER
                        CHECK(ended_at_ms IS NULL OR ended_at_ms >= started_at_ms),
                    alert INTEGER NOT NULL CHECK(alert IN (0, 1)),
                    notified_at_ms INTEGER CHECK(notified_at_ms IS NULL OR notified_at_ms >= 0),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0)
                ) STRICT;

CREATE TABLE collector_owner_actions (
                    action_id INTEGER PRIMARY KEY,
                    action TEXT NOT NULL CHECK(action IN ('retry_collection', 'resume_vehicle')),
                    vehicle_id TEXT REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    requested_at_ms INTEGER NOT NULL CHECK(requested_at_ms >= 0),
                    applied_at_ms INTEGER CHECK(applied_at_ms IS NULL OR applied_at_ms >= 0),
                    CHECK((action = 'resume_vehicle') = (vehicle_id IS NOT NULL))
                ) STRICT;

CREATE TABLE collector_auth_state (
                    singleton_id INTEGER PRIMARY KEY NOT NULL CHECK(singleton_id = 1),
                    state TEXT NOT NULL CHECK(state IN ('ready', 'refreshing', 'retrying',
                        'persistence_pending', 'awaiting_confirmation', 'observer_waiting',
                        'clock_invalid', 'configuration_error', 'sign_in_required',
                        'signed_out', 'api_refused')),
                    reason TEXT CHECK(reason IS NULL OR length(reason) BETWEEN 1 AND 64),
                    collecting INTEGER NOT NULL CHECK(collecting IN (0, 1)),
                    retry_at INTEGER CHECK(retry_at IS NULL OR retry_at >= 0),
                    stopped_generation TEXT
                        CHECK(stopped_generation IS NULL OR length(stopped_generation) = 36),
                    stopped_mode TEXT
                        CHECK(stopped_mode IS NULL OR stopped_mode IN ('legacy', 'provider')),
                    fuse_consecutive INTEGER NOT NULL CHECK(fuse_consecutive BETWEEN 0 AND 3),
                    fuse_generation TEXT
                        CHECK(fuse_generation IS NULL OR length(fuse_generation) = 36),
                    fuse_last_rejection_at INTEGER
                        CHECK(fuse_last_rejection_at IS NULL OR fuse_last_rejection_at >= 0),
                    fuse_pause_until INTEGER
                        CHECK(fuse_pause_until IS NULL OR fuse_pause_until >= 0),
                    clock_skew INTEGER NOT NULL CHECK(clock_skew IN (0, 1)),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0), fuse_rejected_access_digest TEXT
                    CHECK(fuse_rejected_access_digest IS NULL
                          OR length(fuse_rejected_access_digest) = 36),
                    CHECK((state IN ('sign_in_required', 'signed_out'))
                          = (stopped_generation IS NOT NULL)),
                    CHECK((state = 'api_refused') = (stopped_mode IS NOT NULL)),
                    CHECK((fuse_consecutive = 0) = (fuse_generation IS NULL))
                ) STRICT;

CREATE TABLE fleet_telemetry_accumulator_states (
        vehicle_id TEXT PRIMARY KEY NOT NULL
            REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
        state_version INTEGER NOT NULL CHECK(state_version = 2),
        state_json BLOB NOT NULL
            CHECK(length(state_json) BETWEEN 2 AND 262144),
        updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
    ) STRICT;

CREATE TABLE retired_delivery_gaps (
        retired_gap_id INTEGER PRIMARY KEY,
        installation_id TEXT NOT NULL,
        lineage TEXT NOT NULL,
        spool_seq INTEGER NOT NULL CHECK(spool_seq >= 1),
        item_id TEXT NOT NULL CHECK(length(item_id) = 64),
        reason TEXT NOT NULL,
        occurred_at_ms INTEGER,
        evidence_sha256 TEXT
            CHECK(evidence_sha256 IS NULL OR length(evidence_sha256) = 64),
        committed_at_ms INTEGER NOT NULL CHECK(committed_at_ms >= 0),
        vehicle_id TEXT REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
        copied_at_ms INTEGER NOT NULL CHECK(copied_at_ms >= 0),
        UNIQUE(installation_id, lineage, spool_seq)
    ) STRICT;

CREATE TABLE store_data_steps (
        step_key TEXT PRIMARY KEY NOT NULL,
        completed_at_ms INTEGER NOT NULL,
        summary_json TEXT NOT NULL CHECK(json_valid(summary_json))
    ) STRICT;

CREATE TABLE "geofences" (
            vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
            source_geofence_id INTEGER NOT NULL CHECK(source_geofence_id > 0),
            name TEXT NOT NULL
                CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND 1020),
            latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
            longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
            radius_m REAL NOT NULL CHECK(radius_m > 0.0 AND radius_m <= 32767.0),
            billing_type TEXT
                CHECK(billing_type IS NULL OR billing_type IN ('per_kwh', 'per_minute')),
            cost_per_unit REAL,
            session_fee REAL,
            source_owned INTEGER NOT NULL DEFAULT 0 CHECK(source_owned IN (0, 1)),
            PRIMARY KEY(vehicle_id, source_geofence_id)
        ) STRICT;

CREATE TABLE geofence_id_marks (
            vehicle_id TEXT PRIMARY KEY NOT NULL
                REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
            last_hub_id INTEGER NOT NULL CHECK(last_hub_id >= 2147483648)
        ) STRICT;

CREATE TABLE deleted_imported_geofences (
            vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
            source_geofence_id INTEGER NOT NULL
                CHECK(source_geofence_id BETWEEN 1 AND 2147483647),
            PRIMARY KEY(vehicle_id, source_geofence_id)
        ) STRICT;

CREATE TABLE terrain_tidy_state (
             step TEXT PRIMARY KEY NOT NULL CHECK(length(step) > 0),
             resume_vehicle_id TEXT NOT NULL DEFAULT '',
             resume_position_id INTEGER NOT NULL DEFAULT 0 CHECK(resume_position_id >= 0),
             examined INTEGER NOT NULL DEFAULT 0 CHECK(examined >= 0),
             cleared INTEGER NOT NULL DEFAULT 0 CHECK(cleared >= 0),
             requeued INTEGER NOT NULL DEFAULT 0 CHECK(requeued >= 0),
             removed INTEGER NOT NULL DEFAULT 0 CHECK(removed >= 0),
             kept INTEGER NOT NULL DEFAULT 0 CHECK(kept >= 0),
             drives_changed INTEGER NOT NULL DEFAULT 0 CHECK(drives_changed >= 0),
             run INTEGER NOT NULL DEFAULT 1 CHECK(run >= 1),
             armed_at_ms INTEGER CHECK(armed_at_ms IS NULL OR armed_at_ms >= 0),
             completed_at_ms INTEGER CHECK(completed_at_ms IS NULL OR completed_at_ms >= 0)
         ) STRICT;

CREATE TABLE "current_observations"(
                vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                record_type TEXT NOT NULL CHECK(record_type IN (
                    'owner_api_discovery_v1', 'owner_api_vehicle_data_v1',
                    'fleet_api_discovery_v1', 'fleet_api_vehicle_data_v1',
                    'tesla_stream_update_v1', 'source_db_position_v1')),
                observation_id INTEGER NOT NULL CHECK(observation_id > 0),
                source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms >= 0),
                received_at_ms INTEGER NOT NULL CHECK(received_at_ms >= 0),
                payload_sha256 BLOB NOT NULL CHECK(length(payload_sha256) = 32),
                payload_json TEXT NOT NULL CHECK(json_valid(payload_json))
                    CHECK(length(CAST(payload_json AS BLOB)) <= 262144),
                PRIMARY KEY(vehicle_id, record_type)
             ) STRICT, WITHOUT ROWID;

CREATE INDEX sync_ledger_source_sequence
                    ON sync_ledger(source_id, sequence);

CREATE INDEX sync_manifests_vehicle_head
                    ON sync_manifests(vehicle_id, head_sequence DESC);

CREATE INDEX vehicles_source_id
                    ON vehicles(source_id);

CREATE INDEX raw_observations_vehicle_observed
                    ON raw_observations(vehicle_id, observed_at_ms, observation_id);

CREATE INDEX pairing_challenges_expiry
                    ON pairing_challenges(expires_at_ms);

CREATE INDEX materialised_charge_samples_charge
                    ON materialised_charge_samples(vehicle_id, charge_id);

CREATE INDEX materialised_positions_drive
                    ON materialised_positions(vehicle_id, drive_id);

CREATE INDEX address_lookup_cache_identity
                    ON address_lookup_cache(osm_type, osm_id);

CREATE UNIQUE INDEX address_enrichment_target
                    ON address_enrichment_jobs(vehicle_id, target_type, target_id, field);

CREATE INDEX vehicle_identity_aliases_vehicle
                    ON vehicle_identity_aliases(vehicle_id);

CREATE INDEX sync_deltas_vehicle_sequence
                    ON sync_deltas(vehicle_id, from_sequence, to_sequence);

CREATE INDEX import_generations_vehicle
                    ON import_generations(vehicle_id, status);

CREATE INDEX sync_mutations_pending
                    ON sync_mutations(vehicle_id, published, revision, claimed_until_ms);

CREATE INDEX snapshot_fingerprints_manifest
                    ON snapshot_fingerprints(snapshot_id, head_sequence);

CREATE UNIQUE INDEX sync_live_delta_spans_revision_range
                    ON sync_live_delta_spans(vehicle_id, from_revision, to_revision);

CREATE INDEX sync_mutations_compaction_latest
                    ON sync_mutations(vehicle_id, entity, entity_id, revision DESC);

CREATE INDEX stream_session_receipts_proof
                    ON stream_session_receipts(correlation_id, outcome, id);

CREATE INDEX stream_session_receipts_retention
                    ON stream_session_receipts(outcome, completed_at_ms, id);

CREATE INDEX sync_retired_lineage_packs_authorization
                    ON sync_retired_lineage_packs(pack_digest, vehicle_id, head_digest);

CREATE INDEX sync_retired_lineages_expiry
                    ON sync_retired_lineages(expires_at_ms, vehicle_id, head_digest);

CREATE INDEX lifecycle_open_rows_vehicle_domain
                    ON lifecycle_open_rows(vehicle_id, domain, source_row_id);

CREATE INDEX outbound_request_receipts_proof
                    ON outbound_request_receipts(
                        correlation_id, id, safety_class, outcome
                    );

CREATE INDEX outbound_request_receipts_retention
                    ON outbound_request_receipts(outcome, completed_at_ms, id);

CREATE UNIQUE INDEX legacy_refresh_receipt_output_generation
                    ON legacy_refresh_receipt_bindings(output_credential_generation)
                    WHERE output_credential_generation IS NOT NULL;

CREATE UNIQUE INDEX fleet_refresh_receipt_output_generation
                    ON fleet_refresh_receipt_bindings(output_credential_generation)
                    WHERE output_credential_generation IS NOT NULL;

CREATE INDEX raw_observations_vehicle_cursor
                    ON raw_observations(vehicle_id, observation_id);

CREATE INDEX materialised_drives_public_query
                    ON materialised_drives(
                        vehicle_id,
                        CAST(json_extract(drive_json, '$.start_date_ms') AS INTEGER) DESC,
                        drive_id DESC
                    );

CREATE INDEX retained_physical_v3_expiry
                    ON retained_physical_v3_admissions(expires_at_ms, vehicle_id);

CREATE INDEX retained_physical_v3_vehicle_sequence
                    ON retained_physical_v3_admissions(vehicle_id, head_sequence DESC);

CREATE INDEX prepared_map_months_pack ON prepared_map_months(pack_sha256);

CREATE INDEX physical_v3_delta_packs_digest ON physical_v3_delta_packs(sha256);

CREATE INDEX device_vehicle_grants_vehicle
                    ON device_vehicle_grants(vehicle_id, device_id);

CREATE INDEX collection_gaps_open ON collection_gaps(ended_at_ms, gap_id);

CREATE INDEX collector_owner_actions_pending
                    ON collector_owner_actions(applied_at_ms, action_id);

CREATE INDEX geofences_vehicle_location
                 ON geofences(vehicle_id, latitude, longitude);

CREATE INDEX terrain_provenance_retry
             ON terrain_elevation_provenance(status, retry_after_ms, vehicle_id, position_id);

CREATE INDEX terrain_provenance_vehicle_retry
             ON terrain_elevation_provenance(vehicle_id, retry_after_ms) WHERE status = 'failed';

CREATE TRIGGER raw_observations_append_only_update
                BEFORE UPDATE ON raw_observations
                FOR EACH ROW
                BEGIN
                    SELECT RAISE(ABORT, 'raw observations are append-only');
                END;

CREATE TRIGGER raw_observations_match_vehicle_source
                BEFORE INSERT ON raw_observations
                FOR EACH ROW
                WHEN NOT EXISTS (
                    SELECT 1 FROM vehicle_identity_aliases
                    WHERE vehicle_id = NEW.vehicle_id AND source_id = NEW.source_id
                )
                BEGIN
                    SELECT RAISE(ABORT, 'raw observation source and vehicle mismatch');
                END;

CREATE TRIGGER raw_observations_append_only_delete
                BEFORE DELETE ON raw_observations
                FOR EACH ROW
                WHEN NOT EXISTS (
                    SELECT 1 FROM raw_observation_prune_guard WHERE singleton = 1
                )
                BEGIN
                    SELECT RAISE(ABORT, 'raw observations are append-only');
                END;

INSERT INTO terrain_tidy_state(step, resume_vehicle_id, resume_position_id, examined, cleared, requeued, removed, kept, drives_changed, completed_at_ms) VALUES ('earlier_build_values_v1', '', 0, 0, 0, 0, 0, 0, 0, NULL);
