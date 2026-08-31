// SPDX-License-Identifier: AGPL-3.0-only

fn migrate(connection: &Connection) -> Result<(), StoreError> {
    let mut version = schema_version(connection)?;
    if version == 0 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS hub_metadata (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sources (
                    source_id TEXT PRIMARY KEY NOT NULL,
                    source_kind TEXT NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation >= 1),
                    created_at_ms INTEGER NOT NULL
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sync_ledger (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    sequence INTEGER NOT NULL CHECK (sequence >= 1),
                    entity_kind TEXT NOT NULL,
                    entity_key TEXT NOT NULL,
                    operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),
                    committed_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (source_id, sequence, entity_kind, entity_key)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS sync_ledger_source_sequence
                    ON sync_ledger(source_id, sequence);
                PRAGMA user_version = 1;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 1;
    }

    if version == 1 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS sync_manifests (
                    snapshot_id TEXT PRIMARY KEY NOT NULL,
                    vehicle_id TEXT NOT NULL,
                    head_sequence INTEGER NOT NULL CHECK (head_sequence >= 0),
                    manifest_json BLOB NOT NULL
                ) STRICT;
                CREATE INDEX IF NOT EXISTS sync_manifests_vehicle_head
                    ON sync_manifests(vehicle_id, head_sequence DESC);
                CREATE TABLE IF NOT EXISTS sync_packs (
                    sha256 TEXT PRIMARY KEY NOT NULL,
                    snapshot_id TEXT NOT NULL REFERENCES sync_manifests(snapshot_id) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
                    relative_path TEXT NOT NULL,
                    compressed_bytes INTEGER NOT NULL CHECK (compressed_bytes > 0),
                    uncompressed_bytes INTEGER NOT NULL CHECK (uncompressed_bytes >= 100),
                    UNIQUE(snapshot_id, ordinal)
                ) STRICT;
                PRAGMA user_version = 2;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 2;
    }

    if version == 2 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS hub_metadata (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                ) STRICT;
                -- The pre-v3 source table stays intact: it already anchors
                -- sync sequence history. This companion table gives collectors
                -- a stable, non-secret external identity without rewriting it.
                CREATE TABLE IF NOT EXISTS source_identities (
                    source_id TEXT PRIMARY KEY NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    source_kind TEXT NOT NULL,
                    source_key TEXT NOT NULL,
                    UNIQUE(source_kind, source_key),
                    CHECK(length(CAST(source_kind AS BLOB)) BETWEEN 1 AND 64),
                    CHECK(length(CAST(source_key AS BLOB)) BETWEEN 1 AND 256)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS vehicles (
                    vehicle_id TEXT PRIMARY KEY NOT NULL,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    source_vehicle_key TEXT NOT NULL,
                    vin TEXT,
                    display_name TEXT,
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    last_seen_at_ms INTEGER NOT NULL CHECK(last_seen_at_ms >= created_at_ms),
                    UNIQUE(source_id, source_vehicle_key),
                    CHECK(length(CAST(source_vehicle_key AS BLOB)) BETWEEN 1 AND 256),
                    CHECK(vin IS NULL OR length(CAST(vin AS BLOB)) BETWEEN 1 AND 32),
                    CHECK(display_name IS NULL OR length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS vehicles_source_id
                    ON vehicles(source_id);
                CREATE TABLE IF NOT EXISTS raw_observations (
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
                CREATE INDEX IF NOT EXISTS raw_observations_vehicle_observed
                    ON raw_observations(vehicle_id, observed_at_ms, observation_id);
                CREATE TRIGGER IF NOT EXISTS raw_observations_match_vehicle_source
                BEFORE INSERT ON raw_observations
                FOR EACH ROW
                WHEN (SELECT source_id FROM vehicles WHERE vehicle_id = NEW.vehicle_id)
                     != NEW.source_id
                BEGIN
                    SELECT RAISE(ABORT, 'raw observation source and vehicle mismatch');
                END;
                CREATE TRIGGER IF NOT EXISTS raw_observations_append_only_update
                BEFORE UPDATE ON raw_observations
                FOR EACH ROW
                BEGIN
                    SELECT RAISE(ABORT, 'raw observations are append-only');
                END;
                CREATE TRIGGER IF NOT EXISTS raw_observations_append_only_delete
                BEFORE DELETE ON raw_observations
                FOR EACH ROW
                BEGIN
                    SELECT RAISE(ABORT, 'raw observations are append-only');
                END;
                PRAGMA user_version = 3;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 3;
    }

    if version == 3 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS pairing_challenges (
                    pairing_id TEXT PRIMARY KEY NOT NULL,
                    label TEXT NOT NULL,
                    secret_sha256 BLOB NOT NULL CHECK(length(secret_sha256) = 32),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > created_at_ms),
                    CHECK(length(CAST(label AS BLOB)) BETWEEN 1 AND 128)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS pairing_challenges_expiry
                    ON pairing_challenges(expires_at_ms);
                CREATE TABLE IF NOT EXISTS paired_devices (
                    device_id TEXT PRIMARY KEY NOT NULL,
                    display_name TEXT NOT NULL,
                    token_sha256 BLOB NOT NULL UNIQUE CHECK(length(token_sha256) = 32),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > created_at_ms),
                    revoked_at_ms INTEGER,
                    last_authenticated_at_ms INTEGER,
                    CHECK(revoked_at_ms IS NULL OR revoked_at_ms >= created_at_ms),
                    CHECK(last_authenticated_at_ms IS NULL OR last_authenticated_at_ms >= created_at_ms),
                    CHECK(length(CAST(display_name AS BLOB)) BETWEEN 1 AND 128)
                ) STRICT;
                PRAGMA user_version = 4;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 4;
    }

    if version == 4 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS vehicle_lifecycle_state (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    last_observation_id INTEGER NOT NULL CHECK(last_observation_id >= 0),
                    open_session_json BLOB NOT NULL
                        CHECK(length(open_session_json) BETWEEN 2 AND 65536),
                    quarantined INTEGER NOT NULL DEFAULT 0 CHECK(quarantined IN (0, 1)),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS materialised_drives (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    drive_id INTEGER NOT NULL CHECK(drive_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    drive_json TEXT NOT NULL CHECK(json_valid(drive_json)),
                    PRIMARY KEY (vehicle_id, drive_id)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS materialised_positions (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    position_id INTEGER NOT NULL CHECK(position_id > 0),
                    drive_id INTEGER NOT NULL CHECK(drive_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    position_json TEXT NOT NULL CHECK(json_valid(position_json)),
                    PRIMARY KEY (vehicle_id, position_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS materialised_positions_drive
                    ON materialised_positions(vehicle_id, drive_id);
                CREATE TABLE IF NOT EXISTS materialised_charges (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    charge_id INTEGER NOT NULL CHECK(charge_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    charge_json TEXT NOT NULL CHECK(json_valid(charge_json)),
                    PRIMARY KEY (vehicle_id, charge_id)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS materialised_charge_samples (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    sample_id INTEGER NOT NULL CHECK(sample_id > 0),
                    charge_id INTEGER NOT NULL CHECK(charge_id > 0),
                    sample_json TEXT NOT NULL CHECK(json_valid(sample_json)),
                    PRIMARY KEY (vehicle_id, sample_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS materialised_charge_samples_charge
                    ON materialised_charge_samples(vehicle_id, charge_id);
                PRAGMA user_version = 5;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 5;
    }

    if version == 5 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS snapshot_fingerprints (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    fingerprint_sha256 BLOB NOT NULL CHECK(length(fingerprint_sha256) = 32)
                ) STRICT;
                PRAGMA user_version = 6;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 6;
    }

    if version == 6 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS vehicle_snapshot_sequences (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    next_sequence INTEGER NOT NULL CHECK(next_sequence >= 2)
                ) STRICT;
                PRAGMA user_version = 7;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 7;
    }

    if version == 7 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE materialised_positions RENAME TO materialised_positions_v7;
                CREATE TABLE materialised_positions (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    position_id INTEGER NOT NULL CHECK(position_id > 0),
                    drive_id INTEGER CHECK(drive_id IS NULL OR drive_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    position_json TEXT NOT NULL CHECK(json_valid(position_json)),
                    PRIMARY KEY (vehicle_id, position_id)
                ) STRICT;
                INSERT INTO materialised_positions(
                    vehicle_id, position_id, drive_id, car_id, position_json
                )
                SELECT vehicle_id, position_id, drive_id, car_id, position_json
                FROM materialised_positions_v7;
                DROP TABLE materialised_positions_v7;
                CREATE INDEX materialised_positions_drive
                    ON materialised_positions(vehicle_id, drive_id);
                PRAGMA user_version = 8;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 8;
    }

    if version == 8 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE materialised_states (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    state_id INTEGER NOT NULL CHECK(state_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    state_json TEXT NOT NULL CHECK(json_valid(state_json)),
                    PRIMARY KEY (vehicle_id, state_id)
                ) STRICT;
                PRAGMA user_version = 9;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 9;
    }

    if version == 9 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE materialised_updates (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    update_id INTEGER NOT NULL CHECK(update_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    update_json TEXT NOT NULL CHECK(json_valid(update_json)),
                    PRIMARY KEY (vehicle_id, update_id)
                ) STRICT;
                PRAGMA user_version = 10;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 10;
    }

    if version == 10 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS materialised_cars (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    car_json TEXT NOT NULL CHECK(json_valid(car_json))
                ) STRICT;
                PRAGMA user_version = 11;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 11;
    }

    if version == 11 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS geofences (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE RESTRICT,
                    source_geofence_id INTEGER NOT NULL CHECK(source_geofence_id > 0),
                    name TEXT NOT NULL CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND 256),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    radius_m REAL NOT NULL CHECK(radius_m > 0.0 AND radius_m <= 5000.0),
                    PRIMARY KEY(vehicle_id, source_geofence_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS geofences_vehicle_location
                    ON geofences(vehicle_id, latitude, longitude);
                PRAGMA user_version = 12;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 12;
    }

    if version == 12 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS address_cache (
                    osm_type TEXT NOT NULL CHECK(length(CAST(osm_type AS BLOB)) BETWEEN 1 AND 32),
                    osm_id INTEGER NOT NULL CHECK(osm_id > 0),
                    display_name TEXT NOT NULL
                        CHECK(length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256),
                    name TEXT CHECK(name IS NULL OR length(CAST(name AS BLOB)) <= 256),
                    PRIMARY KEY(osm_type, osm_id)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS address_lookup_cache (
                    lookup_key TEXT PRIMARY KEY NOT NULL
                        CHECK(length(CAST(lookup_key AS BLOB)) BETWEEN 1 AND 64),
                    latitude REAL NOT NULL CHECK(latitude >= -90.0 AND latitude <= 90.0),
                    longitude REAL NOT NULL CHECK(longitude >= -180.0 AND longitude <= 180.0),
                    osm_type TEXT NOT NULL,
                    osm_id INTEGER NOT NULL,
                    looked_up_at_ms INTEGER NOT NULL CHECK(looked_up_at_ms >= 0),
                    FOREIGN KEY(osm_type, osm_id)
                        REFERENCES address_cache(osm_type, osm_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS address_lookup_cache_identity
                    ON address_lookup_cache(osm_type, osm_id);
                PRAGMA user_version = 13;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 13;
    }

    if version == 13 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS address_enrichment_jobs (
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
                CREATE UNIQUE INDEX IF NOT EXISTS address_enrichment_target
                    ON address_enrichment_jobs(vehicle_id, target_type, target_id, field);
                PRAGMA user_version = 14;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 14;
    }

    if version == 14 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE materialised_positions ADD COLUMN battery_heater INTEGER
                    CHECK (battery_heater IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN battery_heater_on INTEGER
                    CHECK (battery_heater_on IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN battery_heater_no_power INTEGER
                    CHECK (battery_heater_no_power IN (0, 1));
                PRAGMA user_version = 15;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 15;
    }

    if version == 15 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE materialised_positions ADD COLUMN speed INTEGER;
                ALTER TABLE materialised_positions ADD COLUMN power REAL;
                ALTER TABLE materialised_positions ADD COLUMN est_battery_range_km REAL;
                ALTER TABLE materialised_positions ADD COLUMN fan_status INTEGER;
                ALTER TABLE materialised_positions ADD COLUMN driver_temp_setting REAL;
                ALTER TABLE materialised_positions ADD COLUMN passenger_temp_setting REAL;
                ALTER TABLE materialised_positions ADD COLUMN is_climate_on INTEGER
                    CHECK (is_climate_on IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN is_rear_defroster_on INTEGER
                    CHECK (is_rear_defroster_on IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN is_front_defroster_on INTEGER
                    CHECK (is_front_defroster_on IN (0, 1));
                ALTER TABLE materialised_positions ADD COLUMN tpms_pressure_fl REAL;
                ALTER TABLE materialised_positions ADD COLUMN tpms_pressure_fr REAL;
                ALTER TABLE materialised_positions ADD COLUMN tpms_pressure_rl REAL;
                ALTER TABLE materialised_positions ADD COLUMN tpms_pressure_rr REAL;
                PRAGMA user_version = 16;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 16;
    }

    if version == 16 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE materialised_drives ADD COLUMN inside_temp_avg REAL;
                ALTER TABLE materialised_drives ADD COLUMN power_max REAL;
                ALTER TABLE materialised_drives ADD COLUMN power_min REAL;
                ALTER TABLE materialised_drives ADD COLUMN start_ideal_range_km REAL;
                ALTER TABLE materialised_drives ADD COLUMN end_ideal_range_km REAL;
                ALTER TABLE materialised_drives ADD COLUMN ascent INTEGER;
                ALTER TABLE materialised_drives ADD COLUMN descent INTEGER;
                PRAGMA user_version = 17;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 17;
    }

    if version == 17 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
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
                ) STRICT;
                PRAGMA user_version = 18;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 18;
    }

    if version == 18 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE geofences ADD COLUMN billing_type TEXT
                    CHECK(billing_type IS NULL OR billing_type IN ('per_kwh', 'per_minute'));
                ALTER TABLE geofences ADD COLUMN cost_per_unit REAL;
                ALTER TABLE geofences ADD COLUMN session_fee REAL
                    CHECK(session_fee IS NULL OR session_fee >= 0.0);
                PRAGMA user_version = 19;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 19;
    }

    if version == 19 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS stream_watermarks (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    last_timestamp_ms INTEGER NOT NULL CHECK(last_timestamp_ms >= 0)
                ) STRICT;
                PRAGMA user_version = 20;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 20;
    }

    if version == 20 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS lifecycle_open_rows (
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
                    PRIMARY KEY(source_id, source_table, source_row_id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS lifecycle_open_rows_vehicle_domain
                    ON lifecycle_open_rows(vehicle_id, domain, source_row_id);
                CREATE TABLE IF NOT EXISTS lifecycle_source_watermarks (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    domain TEXT NOT NULL,
                    max_source_row_id INTEGER,
                    max_timestamp_ms INTEGER,
                    PRIMARY KEY(source_id, vehicle_id, domain)
                ) STRICT;
                PRAGMA user_version = 21;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 21;
    }

    if version == 21 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE car_settings ADD COLUMN suspend_min_resolved INTEGER NOT NULL DEFAULT 1
                    CHECK(suspend_min_resolved IN (0, 1));
                PRAGMA user_version = 22;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 22;
    }

    if version == 22 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS terrain_enrichment_state (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    cursor_position_id INTEGER NOT NULL DEFAULT 0
                        CHECK(cursor_position_id >= 0),
                    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS terrain_elevation_provenance (
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
                CREATE INDEX IF NOT EXISTS terrain_provenance_retry
                    ON terrain_elevation_provenance(status, retry_after_ms);
                PRAGMA user_version = 23;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 23;
    }

    if version == 23 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS vehicle_identity_aliases (
                    alias_kind TEXT NOT NULL CHECK(alias_kind IN ('source_key', 'tesla_eid', 'tesla_vid', 'vin')),
                    alias_value TEXT NOT NULL,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    source_vehicle_key TEXT NOT NULL,
                    PRIMARY KEY(alias_kind, alias_value),
                    CHECK(length(CAST(alias_value AS BLOB)) BETWEEN 1 AND 256),
                    CHECK(length(CAST(source_vehicle_key AS BLOB)) BETWEEN 1 AND 256)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS vehicle_identity_aliases_vehicle
                    ON vehicle_identity_aliases(vehicle_id);
                DROP TRIGGER IF EXISTS raw_observations_match_vehicle_source;
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
                INSERT OR IGNORE INTO vehicle_identity_aliases
                    (alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)
                SELECT 'vin', v.vin, v.vehicle_id, v.source_id, v.source_vehicle_key
                FROM vehicles v WHERE v.vin IS NOT NULL AND length(v.vin) > 0;
                INSERT OR IGNORE INTO vehicle_identity_aliases
                    (alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)
                SELECT 'source_key', v.source_id || ':' || v.source_vehicle_key,
                       v.vehicle_id, v.source_id, v.source_vehicle_key
                FROM vehicles v;
                INSERT OR IGNORE INTO vehicle_identity_aliases
                    (alias_kind, alias_value, vehicle_id, source_id, source_vehicle_key)
                SELECT 'tesla_eid', substr(v.source_vehicle_key, 5), v.vehicle_id,
                       v.source_id, v.source_vehicle_key
                FROM vehicles v
                WHERE v.source_vehicle_key GLOB 'eid:[0-9]*'
                  AND length(substr(v.source_vehicle_key, 5)) > 0;
                PRAGMA user_version = 24;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 24;
    }

    if version == 24 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS sync_bases (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    snapshot_id TEXT NOT NULL UNIQUE,
                    base_sequence INTEGER NOT NULL CHECK(base_sequence >= 0),
                    base_digest TEXT NOT NULL CHECK(length(base_digest) = 64),
                    packs_json BLOB NOT NULL
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sync_deltas (
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
                CREATE TABLE IF NOT EXISTS sync_heads (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    base_snapshot_id TEXT NOT NULL REFERENCES sync_bases(snapshot_id) ON DELETE RESTRICT,
                    head_sequence INTEGER NOT NULL CHECK(head_sequence >= 0),
                    head_digest TEXT NOT NULL CHECK(length(head_digest) = 64),
                    terminal_cursor TEXT NOT NULL
                ) STRICT;
                CREATE INDEX IF NOT EXISTS sync_deltas_vehicle_sequence
                    ON sync_deltas(vehicle_id, from_sequence, to_sequence);
                PRAGMA user_version = 25;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 25;
    }

    if version == 25 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS import_generations (
                    run_id TEXT PRIMARY KEY NOT NULL,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    status TEXT NOT NULL CHECK(status IN ('staging', 'promoting')),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS import_generations_vehicle
                    ON import_generations(vehicle_id, status);
                CREATE TABLE IF NOT EXISTS import_generation_sessions (
                    run_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES import_generations(run_id) ON DELETE CASCADE,
                    session_json TEXT NOT NULL CHECK(json_valid(session_json))
                ) STRICT;
                PRAGMA user_version = 26;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 26;
    }

    if version == 26 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE import_generations ADD COLUMN base_last_observation_id
                    INTEGER NOT NULL DEFAULT 0 CHECK(base_last_observation_id >= 0);
                ALTER TABLE import_generations ADD COLUMN base_updated_at_ms
                    INTEGER NOT NULL DEFAULT 0 CHECK(base_updated_at_ms >= 0);
                PRAGMA user_version = 27;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 27;
    }

    if version == 27 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS export_outbox (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    dirty_revision INTEGER NOT NULL CHECK(dirty_revision > 0),
                    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
                    next_attempt_ms INTEGER NOT NULL DEFAULT 0 CHECK(next_attempt_ms >= 0),
                    claimed_until_ms INTEGER NOT NULL DEFAULT 0 CHECK(claimed_until_ms >= 0),
                    last_error TEXT
                ) STRICT;
                PRAGMA user_version = 28;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 28;
    }

    if version == 28 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS sync_mutation_sequences (
                    vehicle_id TEXT PRIMARY KEY NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    next_revision INTEGER NOT NULL CHECK(next_revision > 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS sync_mutations (
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
                CREATE INDEX IF NOT EXISTS sync_mutations_pending
                    ON sync_mutations(vehicle_id, published, revision, claimed_until_ms);
                PRAGMA user_version = 29;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 29;
    }

    if version == 29 {
        // Version 29 is retained for migration ordering. New databases do
        // not create the removed MQTT tables.
        connection
            .execute_batch("PRAGMA user_version = 30;")
            .map_err(StoreError::Migrate)?;
        version = 30;
    }

    if version == 30 {
        // Existing rows have no trustworthy manifest identity. Preserve the
        // hash, but leave these nullable columns unset so it cannot skip a
        // later import by accidentally matching an arbitrary manifest.
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE snapshot_fingerprints ADD COLUMN snapshot_id TEXT;
                ALTER TABLE snapshot_fingerprints ADD COLUMN head_sequence INTEGER;
                CREATE INDEX IF NOT EXISTS snapshot_fingerprints_manifest
                    ON snapshot_fingerprints(snapshot_id, head_sequence);
                PRAGMA user_version = 31;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 31;
    }

    if version == 31 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS outbound_request_receipts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    vehicle_tesla_id INTEGER CHECK(vehicle_tesla_id > 0),
                    transport TEXT NOT NULL CHECK(transport IN ('owner_api', 'stream', 'legacy_auth')),
                    operation TEXT NOT NULL CHECK(operation IN ('products', 'vehicle_probe', 'vehicle_data', 'token_refresh', 'stream_connect', 'stream_subscribe', 'stream_unsubscribe')),
                    safety_class TEXT NOT NULL CHECK(safety_class IN ('non_wake_endpoint', 'conditional_read', 'direct_wake_command')),
                    precondition TEXT NOT NULL CHECK(precondition IN ('not_required', 'stream_power_confirmed')),
                    outcome TEXT NOT NULL CHECK(outcome IN ('started', 'success', 'http_error', 'timeout', 'transport_error', 'authentication_rejected', 'protocol_error', 'response_too_large', 'cancelled')),
                    http_status INTEGER CHECK(http_status BETWEEN 100 AND 599),
                    CHECK((outcome = 'started' AND completed_at_ms IS NULL AND duration_ms IS NULL AND http_status IS NULL) OR (outcome <> 'started' AND completed_at_ms IS NOT NULL AND duration_ms IS NOT NULL AND completed_at_ms >= started_at_ms AND duration_ms >= 0))
                ) STRICT;
                CREATE INDEX IF NOT EXISTS outbound_request_receipts_proof ON outbound_request_receipts(correlation_id, id, safety_class, outcome);
                CREATE INDEX IF NOT EXISTS outbound_request_receipts_retention ON outbound_request_receipts(outcome, completed_at_ms, id);
                PRAGMA user_version = 32;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 32;
    }

    if version == 32 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS stream_session_receipts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                    vehicle_tesla_id INTEGER NOT NULL CHECK(vehicle_tesla_id > 0),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    outcome TEXT NOT NULL CHECK(outcome IN ('started', 'orderly_shutdown')),
                    unsubscribe_receipt_id INTEGER,
                    CHECK((outcome = 'started' AND completed_at_ms IS NULL AND duration_ms IS NULL AND unsubscribe_receipt_id IS NULL) OR (outcome = 'orderly_shutdown' AND completed_at_ms IS NOT NULL AND duration_ms IS NOT NULL AND completed_at_ms >= started_at_ms AND duration_ms >= 0 AND unsubscribe_receipt_id IS NOT NULL))
                ) STRICT;
                CREATE INDEX IF NOT EXISTS stream_session_receipts_proof
                    ON stream_session_receipts(correlation_id, outcome, id);
                CREATE INDEX IF NOT EXISTS stream_session_receipts_retention
                    ON stream_session_receipts(outcome, completed_at_ms, id);
                PRAGMA user_version = 33;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 33;
    }

    if version == 33 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE outbound_request_receipts
                    ADD COLUMN retry_after_seconds INTEGER
                    CHECK(retry_after_seconds >= 0);
                PRAGMA user_version = 34;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 34;
    }

    if version == 34 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                PRAGMA user_version = 35;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 35;
    }

    if version == 35 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- The base transport metadata does not expose its selected
                -- source-car identifier through SyncManifest.  Persist the
                -- exact binding used to create each new V2 base so later
                -- deltas never reconstruct it from mutable source aliases.
                CREATE TABLE IF NOT EXISTS v2_base_bindings (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    snapshot_id TEXT NOT NULL UNIQUE
                        REFERENCES sync_bases(snapshot_id) ON DELETE CASCADE,
                    installation_id TEXT NOT NULL CHECK(length(installation_id) = 36),
                    account_id TEXT NOT NULL CHECK(length(account_id) = 36),
                    generation INTEGER NOT NULL CHECK(generation >= 1),
                    selected_car_id INTEGER NOT NULL CHECK(selected_car_id > 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS teslamate_import_projection_heads (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    base_snapshot_id TEXT NOT NULL UNIQUE
                        REFERENCES sync_bases(snapshot_id) ON DELETE CASCADE,
                    selected_car_id INTEGER NOT NULL CHECK(selected_car_id > 0),
                    head_sequence INTEGER NOT NULL CHECK(head_sequence >= 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS teslamate_import_projection_rows (
                    vehicle_id TEXT NOT NULL
                        REFERENCES teslamate_import_projection_heads(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'drive', 'position', 'charge', 'charge_sample', 'state'
                    )),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    PRIMARY KEY(vehicle_id, entity, entity_id)
                ) STRICT;
                PRAGMA user_version = 36;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 36;
    }

    if version == 36 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- This is deliberately separate from the legacy deletion
                -- inventory: it includes `car` and records only canonical
                -- digests, never projection payload JSON.
                CREATE TABLE IF NOT EXISTS teslamate_import_projection_state_heads (
                    vehicle_id TEXT PRIMARY KEY NOT NULL
                        REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    base_snapshot_id TEXT NOT NULL
                        REFERENCES sync_bases(snapshot_id) ON DELETE CASCADE,
                    selected_car_id INTEGER NOT NULL CHECK(selected_car_id > 0),
                    head_sequence INTEGER NOT NULL CHECK(head_sequence >= 0)
                ) STRICT;
                CREATE TABLE IF NOT EXISTS teslamate_import_projection_state_rows (
                    vehicle_id TEXT NOT NULL
                        REFERENCES teslamate_import_projection_state_heads(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'car', 'drive', 'position', 'charge', 'charge_sample', 'state'
                    )),
                    entity_ordinal INTEGER NOT NULL CHECK(entity_ordinal BETWEEN 0 AND 5),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    projection_sha256 BLOB NOT NULL CHECK(length(projection_sha256) = 32),
                    CHECK(
                        (entity = 'car' AND entity_ordinal = 0) OR
                        (entity = 'drive' AND entity_ordinal = 1) OR
                        (entity = 'position' AND entity_ordinal = 2) OR
                        (entity = 'charge' AND entity_ordinal = 3) OR
                        (entity = 'charge_sample' AND entity_ordinal = 4) OR
                        (entity = 'state' AND entity_ordinal = 5)
                    ),
                    PRIMARY KEY(vehicle_id, entity_ordinal, entity_id),
                    UNIQUE(vehicle_id, entity, entity_id)
                ) STRICT, WITHOUT ROWID;
                PRAGMA user_version = 37;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 37;
    }

    if version == 37 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- A durable, one-time audit marker for the only supported
                -- migration from the retired fragment-dependent direct
                -- fingerprint to the fragment-independent logical one.
                CREATE TABLE IF NOT EXISTS teslamate_import_projection_state_bridges (
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
                PRAGMA user_version = 38;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 38;
    }

    if version == 38 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- Firmware-update history is now part of the TeslaMate V2
                -- projection inventory and digest state. Rebuild the two
                -- constrained WITHOUT ROWID tables so existing bases retain
                -- their exact rows while new `update` facts use ordinal 6.
                CREATE TABLE teslamate_import_projection_rows_v39 (
                    vehicle_id TEXT NOT NULL
                        REFERENCES teslamate_import_projection_heads(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'drive', 'position', 'charge', 'charge_sample', 'state', 'update'
                    )),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    PRIMARY KEY(vehicle_id, entity, entity_id)
                ) STRICT;
                INSERT INTO teslamate_import_projection_rows_v39(
                    vehicle_id, entity, entity_id
                )
                SELECT vehicle_id, entity, entity_id
                  FROM teslamate_import_projection_rows;
                DROP TABLE teslamate_import_projection_rows;
                ALTER TABLE teslamate_import_projection_rows_v39
                    RENAME TO teslamate_import_projection_rows;

                CREATE TABLE teslamate_import_projection_state_rows_v39 (
                    vehicle_id TEXT NOT NULL
                        REFERENCES teslamate_import_projection_state_heads(vehicle_id) ON DELETE CASCADE,
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
                INSERT INTO teslamate_import_projection_state_rows_v39(
                    vehicle_id, entity, entity_ordinal, entity_id, car_id, projection_sha256
                )
                SELECT vehicle_id, entity, entity_ordinal, entity_id, car_id, projection_sha256
                  FROM teslamate_import_projection_state_rows;
                DROP TABLE teslamate_import_projection_state_rows;
                ALTER TABLE teslamate_import_projection_state_rows_v39
                    RENAME TO teslamate_import_projection_state_rows;
                PRAGMA user_version = 39;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 39;
    }

    if version == 39 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- Only collector-published deltas have durable sync-mutation
                -- provenance. Import successors deliberately remain outside
                -- this table so compaction can never guess how to rebuild
                -- source-owned history.
                CREATE TABLE IF NOT EXISTS sync_live_delta_spans (
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
                CREATE UNIQUE INDEX IF NOT EXISTS sync_live_delta_spans_revision_range
                    ON sync_live_delta_spans(vehicle_id, from_revision, to_revision);
                CREATE INDEX IF NOT EXISTS sync_mutations_compaction_latest
                    ON sync_mutations(vehicle_id, entity, entity_id, revision DESC);
                PRAGMA user_version = 40;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 40;
    }

    if version == 40 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                DROP INDEX IF EXISTS stream_session_receipts_proof;
                DROP INDEX IF EXISTS stream_session_receipts_retention;
                ALTER TABLE stream_session_receipts RENAME TO stream_session_receipts_v40;
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
                INSERT INTO stream_session_receipts(
                    id, correlation_id, vehicle_tesla_id, started_at_ms,
                    completed_at_ms, duration_ms, outcome, unsubscribe_receipt_id
                )
                SELECT id, correlation_id, vehicle_tesla_id, started_at_ms,
                       completed_at_ms, duration_ms, outcome, unsubscribe_receipt_id
                  FROM stream_session_receipts_v40;
                DROP TABLE stream_session_receipts_v40;
                CREATE INDEX stream_session_receipts_proof
                    ON stream_session_receipts(correlation_id, outcome, id);
                CREATE INDEX stream_session_receipts_retention
                    ON stream_session_receipts(outcome, completed_at_ms, id);
                PRAGMA user_version = 41;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 41;
    }

    if version == 41 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- A client may already hold the signed lineage that was
                -- current immediately before live-delta compaction. Retain
                -- only the replaced objects, bound to that exact validated
                -- manifest and a finite authorization window. Arbitrary
                -- orphan files never gain an authorization row.
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
                CREATE INDEX sync_retired_lineage_packs_authorization
                    ON sync_retired_lineage_packs(pack_digest, vehicle_id, head_digest);
                CREATE INDEX sync_retired_lineages_expiry
                    ON sync_retired_lineages(expires_at_ms, vehicle_id, head_digest);
                PRAGMA user_version = 42;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 42;
    }

    if version == 42 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- Cross-process operational truth for the opt-in supervised
                -- collector. A random instance ID fences stale processes;
                -- state is a closed, redacted vocabulary rather than an
                -- arbitrary error string.
                CREATE TABLE IF NOT EXISTS supervised_collector_lease (
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
                PRAGMA user_version = 43;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 43;
    }

    if version == 43 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                -- Bind each refresh request to the exact encrypted-journal
                -- attempt and credential generation that authorized it. The
                -- parent receipt remains redacted request metadata; this
                -- child has no token material or endpoint data.
                CREATE TABLE legacy_refresh_receipt_bindings (
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
                CREATE UNIQUE INDEX legacy_refresh_receipt_output_generation
                    ON legacy_refresh_receipt_bindings(output_credential_generation)
                    WHERE output_credential_generation IS NOT NULL;
                PRAGMA user_version = 44;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 44;
    }

    if version == 44 {
        connection
            .execute_batch(&format!(
                "
                BEGIN IMMEDIATE;
                -- Permanent refresh-input fence. Forgetting a consumed
                -- single-use token could authorize an unsafe retry.
                CREATE TABLE legacy_refresh_input_fences (
                    input_credential_generation TEXT PRIMARY KEY COLLATE NOCASE
                        CHECK(length(input_credential_generation) = 36)
                ) STRICT, WITHOUT ROWID;
                INSERT INTO legacy_refresh_input_fences(input_credential_generation)
                    SELECT lower(input_credential_generation)
                      FROM legacy_refresh_receipt_bindings
                     GROUP BY lower(input_credential_generation);
                CREATE TABLE legacy_refresh_input_fence_migration_guard (
                    fence_count INTEGER NOT NULL CHECK(fence_count <= {0})
                ) STRICT;
                INSERT INTO legacy_refresh_input_fence_migration_guard(fence_count)
                    SELECT COUNT(*) FROM legacy_refresh_input_fences;
                DROP TABLE legacy_refresh_input_fence_migration_guard;
                PRAGMA user_version = 45;
                COMMIT;
                ",
                MAX_LEGACY_REFRESH_INPUT_FENCES
            ))
            .map_err(StoreError::Migrate)?;
        version = 45;
    }

    if version == 45 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                DROP TABLE IF EXISTS mqtt_delivery_state;
                DROP TABLE IF EXISTS mqtt_summary_revisions;
                PRAGMA user_version = 46;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 46;
    }

    if version == 46 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS teslamate_legacy_tokens (
                    singleton_id INTEGER PRIMARY KEY NOT NULL CHECK(singleton_id = 1),
                    access BLOB NOT NULL CHECK(length(access) > 0),
                    refresh BLOB NOT NULL CHECK(length(refresh) > 0),
                    expires_at INTEGER NOT NULL CHECK(expires_at >= 0),
                    next_refresh_at INTEGER NOT NULL CHECK(next_refresh_at >= 0),
                    CHECK(
                        (expires_at = 0 AND next_refresh_at = 0)
                        OR (expires_at > next_refresh_at AND next_refresh_at > 0)
                    )
                ) STRICT;
                PRAGMA user_version = 47;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 47;
    }

    if version == 47 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                DROP TABLE IF EXISTS migration_request_intents;
                DROP TABLE IF EXISTS migration_wake_leases;
                PRAGMA user_version = 48;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 48;
    }

    if version == 48 {
        migrate_address_cache_metadata(connection)?;
        version = 49;
    }

    if version == 49 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE current_observations (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    record_type TEXT NOT NULL CHECK(record_type IN (
                        'owner_api_discovery_v1',
                        'owner_api_vehicle_data_v1',
                        'tesla_stream_update_v1'
                    )),
                    observation_id INTEGER NOT NULL CHECK(observation_id > 0),
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms >= 0),
                    received_at_ms INTEGER NOT NULL CHECK(received_at_ms >= 0),
                    payload_sha256 BLOB NOT NULL CHECK(length(payload_sha256) = 32),
                    payload_json TEXT NOT NULL CHECK(json_valid(payload_json))
                        CHECK(length(CAST(payload_json AS BLOB)) <= 262144),
                    PRIMARY KEY(vehicle_id, record_type)
                ) STRICT, WITHOUT ROWID;
                INSERT INTO current_observations(
                    vehicle_id, record_type, observation_id, source_id,
                    observed_at_ms, received_at_ms, payload_sha256, payload_json
                )
                SELECT r.vehicle_id,
                       json_extract(r.payload_json, '$.record_type'),
                       r.observation_id, r.source_id, r.observed_at_ms,
                       r.received_at_ms, r.payload_sha256, r.payload_json
                FROM raw_observations AS r
                WHERE json_extract(r.payload_json, '$.record_type') IN (
                    'owner_api_discovery_v1',
                    'owner_api_vehicle_data_v1',
                    'tesla_stream_update_v1'
                )
                  AND r.observation_id = (
                    SELECT candidate.observation_id
                    FROM raw_observations AS candidate
                    WHERE candidate.vehicle_id = r.vehicle_id
                      AND json_extract(candidate.payload_json, '$.record_type') =
                          json_extract(r.payload_json, '$.record_type')
                    ORDER BY candidate.observed_at_ms DESC,
                             candidate.observation_id DESC
                    LIMIT 1
                  );
                CREATE TABLE raw_observation_prune_guard (
                    singleton INTEGER PRIMARY KEY CHECK(singleton = 1)
                ) STRICT;
                DROP TRIGGER raw_observations_append_only_delete;
                CREATE TRIGGER raw_observations_append_only_delete
                BEFORE DELETE ON raw_observations
                FOR EACH ROW
                WHEN NOT EXISTS (
                    SELECT 1 FROM raw_observation_prune_guard WHERE singleton = 1
                )
                BEGIN
                    SELECT RAISE(ABORT, 'raw observations are append-only');
                END;
                PRAGMA user_version = 50;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 50;
    }

    if version == 50 {
        // Historical paired bearers had no expiry or revocation state. Give
        // each one a finite migration grace period in one atomic replacement.
        let expires_at_ms =
            retired_lineage_clock_ms()?.saturating_add(PAIRED_DEVICE_TOKEN_LIFETIME_MS);
        connection
            .execute_batch(&format!(
                "BEGIN IMMEDIATE;
                CREATE TABLE paired_devices_v51 (
                    device_id TEXT PRIMARY KEY NOT NULL,
                    display_name TEXT NOT NULL,
                    token_sha256 BLOB NOT NULL UNIQUE CHECK(length(token_sha256) = 32),
                    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms > created_at_ms),
                    revoked_at_ms INTEGER,
                    last_authenticated_at_ms INTEGER,
                    CHECK(revoked_at_ms IS NULL OR revoked_at_ms >= created_at_ms),
                    CHECK(last_authenticated_at_ms IS NULL OR last_authenticated_at_ms >= created_at_ms),
                    CHECK(length(CAST(display_name AS BLOB)) BETWEEN 1 AND 128)
                ) STRICT;
                INSERT INTO paired_devices_v51(
                    device_id, display_name, token_sha256, created_at_ms,
                    expires_at_ms, revoked_at_ms, last_authenticated_at_ms
                )
                SELECT device_id, display_name, token_sha256,
                       CASE WHEN created_at_ms = 9223372036854775807
                            THEN 9223372036854775806 ELSE created_at_ms END,
                       CASE WHEN created_at_ms = 9223372036854775807
                            THEN 9223372036854775807
                            WHEN {expires_at_ms} > created_at_ms
                            THEN {expires_at_ms}
                            ELSE created_at_ms + 1 END,
                       NULL, last_authenticated_at_ms
                  FROM paired_devices;
                DROP TABLE paired_devices;
                ALTER TABLE paired_devices_v51 RENAME TO paired_devices;
                PRAGMA user_version = 51;
                COMMIT;",
                expires_at_ms = expires_at_ms
            ))
            .map_err(StoreError::Migrate)?;
        version = 51;
    }

    if version == 51 {
        let migration_completed_at_ms = outbound_request_clock_ms()?;
        connection
            .execute_batch("BEGIN IMMEDIATE;")
            .map_err(StoreError::Migrate)?;
        let migration = (|| -> Result<(), rusqlite::Error> {
            let has_generation: bool = connection.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM pragma_table_info('teslamate_legacy_tokens')
                     WHERE name = 'credential_generation'
                 )",
                [],
                |row| row.get(0),
            )?;
            connection.execute_batch(
                "CREATE TABLE teslamate_legacy_tokens_v52 (
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
                 ) STRICT;",
            )?;
            let generation = if has_generation {
                "credential_generation"
            } else {
                "NULL"
            };
            connection.execute_batch(&format!(
                "INSERT INTO teslamate_legacy_tokens_v52(
                    singleton_id, access, refresh, expires_at, next_refresh_at,
                    credential_generation
                 )
                 SELECT singleton_id, access, refresh, expires_at, next_refresh_at,
                        {generation}
                   FROM teslamate_legacy_tokens;
                 DROP TABLE teslamate_legacy_tokens;
                 ALTER TABLE teslamate_legacy_tokens_v52
                    RENAME TO teslamate_legacy_tokens;"
            ))?;
            connection.execute(
                "UPDATE outbound_request_receipts AS r
                    SET completed_at_ms = MAX(?1, started_at_ms),
                        duration_ms = MAX(?1, started_at_ms) - started_at_ms,
                        outcome = 'cancelled'
                  WHERE transport = 'legacy_auth'
                    AND operation = 'token_refresh'
                    AND outcome = 'started'
                    AND NOT EXISTS (
                        SELECT 1 FROM legacy_refresh_receipt_bindings AS b
                         WHERE b.receipt_id = r.id
                    )",
                params![migration_completed_at_ms],
            )?;
            connection.execute_batch("PRAGMA user_version = 52; COMMIT;")
        })();
        if let Err(error) = migration {
            let _ = connection.execute_batch("ROLLBACK;");
            return Err(StoreError::Migrate(error));
        }
        version = 52;
    }

    if version == 52 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE lifecycle_open_rows_v53 (
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
                INSERT INTO lifecycle_open_rows_v53(
                    source_id, source_table, source_row_id, vehicle_id, car_id,
                    domain, parent_source_row_id, row_json
                )
                SELECT source_id, source_table, source_row_id, vehicle_id, car_id,
                       domain, parent_source_row_id, row_json
                  FROM lifecycle_open_rows;
                DROP TABLE lifecycle_open_rows;
                ALTER TABLE lifecycle_open_rows_v53 RENAME TO lifecycle_open_rows;
                CREATE INDEX lifecycle_open_rows_vehicle_domain
                    ON lifecycle_open_rows(vehicle_id, domain, source_row_id);
                PRAGMA user_version = 53;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 53;
    }

    if version == 53 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE TABLE outbound_request_receipts_v54 (
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
                INSERT INTO outbound_request_receipts_v54(
                    id, correlation_id, started_at_ms, completed_at_ms,
                    duration_ms, vehicle_tesla_id, transport, operation,
                    safety_class, precondition, outcome, http_status,
                    retry_after_seconds
                )
                SELECT id, correlation_id, started_at_ms, completed_at_ms,
                       duration_ms, vehicle_tesla_id, transport, operation,
                       safety_class, precondition, outcome, http_status,
                       retry_after_seconds
                  FROM outbound_request_receipts;
                CREATE TABLE legacy_refresh_receipt_bindings_v54 (
                    receipt_id INTEGER PRIMARY KEY NOT NULL
                        REFERENCES outbound_request_receipts_v54(id) ON DELETE CASCADE,
                    attempt_id TEXT NOT NULL UNIQUE CHECK(length(attempt_id) = 36),
                    input_credential_generation TEXT NOT NULL
                        CHECK(length(input_credential_generation) = 36),
                    output_credential_generation TEXT
                        CHECK(output_credential_generation IS NULL
                              OR length(output_credential_generation) = 36),
                    CHECK(output_credential_generation IS NULL
                          OR output_credential_generation <> input_credential_generation)
                ) STRICT;
                INSERT INTO legacy_refresh_receipt_bindings_v54(
                    receipt_id, attempt_id, input_credential_generation,
                    output_credential_generation
                )
                SELECT receipt_id, attempt_id, input_credential_generation,
                       output_credential_generation
                  FROM legacy_refresh_receipt_bindings;
                DROP TABLE legacy_refresh_receipt_bindings;
                DROP TABLE outbound_request_receipts;
                ALTER TABLE outbound_request_receipts_v54
                    RENAME TO outbound_request_receipts;
                ALTER TABLE legacy_refresh_receipt_bindings_v54
                    RENAME TO legacy_refresh_receipt_bindings;
                CREATE INDEX outbound_request_receipts_proof
                    ON outbound_request_receipts(
                        correlation_id, id, safety_class, outcome
                    );
                CREATE INDEX outbound_request_receipts_retention
                    ON outbound_request_receipts(outcome, completed_at_ms, id);
                CREATE UNIQUE INDEX legacy_refresh_receipt_output_generation
                    ON legacy_refresh_receipt_bindings(output_credential_generation)
                    WHERE output_credential_generation IS NOT NULL;
                CREATE TABLE IF NOT EXISTS fleet_tokens (
                    singleton_id INTEGER PRIMARY KEY NOT NULL CHECK(singleton_id = 1),
                    access BLOB NOT NULL CHECK(length(access) BETWEEN 1 AND 16424),
                    refresh BLOB NOT NULL CHECK(length(refresh) BETWEEN 1 AND 16424),
                    client_id TEXT NOT NULL
                        CHECK(length(CAST(client_id AS BLOB)) BETWEEN 1 AND 255),
                    region TEXT NOT NULL CHECK(region IN ('na', 'eu', 'cn')),
                    expires_at INTEGER NOT NULL CHECK(expires_at > 0),
                    next_refresh_at INTEGER NOT NULL
                        CHECK(next_refresh_at > 0 AND next_refresh_at < expires_at)
                ) STRICT;
                PRAGMA user_version = 54;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 54;
    }

    if version == 54 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                ALTER TABLE fleet_tokens ADD COLUMN credential_generation TEXT
                    CHECK(credential_generation IS NULL
                          OR length(credential_generation) = 36);
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
                CREATE UNIQUE INDEX fleet_refresh_receipt_output_generation
                    ON fleet_refresh_receipt_bindings(output_credential_generation)
                    WHERE output_credential_generation IS NOT NULL;
                CREATE TABLE fleet_refresh_input_fences (
                    input_credential_generation TEXT PRIMARY KEY COLLATE NOCASE
                        CHECK(length(input_credential_generation) = 36)
                ) STRICT, WITHOUT ROWID;
                CREATE TABLE current_observations_v55 (
                    vehicle_id TEXT NOT NULL REFERENCES vehicles(vehicle_id) ON DELETE CASCADE,
                    record_type TEXT NOT NULL CHECK(record_type IN (
                        'owner_api_discovery_v1',
                        'owner_api_vehicle_data_v1',
                        'fleet_api_discovery_v1',
                        'fleet_api_vehicle_data_v1',
                        'tesla_stream_update_v1'
                    )),
                    observation_id INTEGER NOT NULL CHECK(observation_id > 0),
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE RESTRICT,
                    observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms >= 0),
                    received_at_ms INTEGER NOT NULL CHECK(received_at_ms >= 0),
                    payload_sha256 BLOB NOT NULL CHECK(length(payload_sha256) = 32),
                    payload_json TEXT NOT NULL CHECK(json_valid(payload_json))
                        CHECK(length(CAST(payload_json AS BLOB)) <= 262144),
                    PRIMARY KEY(vehicle_id, record_type)
                ) STRICT, WITHOUT ROWID;
                INSERT INTO current_observations_v55(
                    vehicle_id, record_type, observation_id, source_id,
                    observed_at_ms, received_at_ms, payload_sha256, payload_json
                )
                SELECT vehicle_id, record_type, observation_id, source_id,
                       observed_at_ms, received_at_ms, payload_sha256, payload_json
                  FROM current_observations;
                DROP TABLE current_observations;
                ALTER TABLE current_observations_v55 RENAME TO current_observations;
                PRAGMA user_version = 55;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 55;
    }

    if version == 55 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                CREATE INDEX IF NOT EXISTS raw_observations_vehicle_cursor
                    ON raw_observations(vehicle_id, observation_id);
                PRAGMA user_version = 56;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 56;
    }

    if version == 56 {
        connection
            .execute_batch(
                "
                BEGIN IMMEDIATE;
                DROP INDEX IF EXISTS materialised_drives_public_query;
                CREATE INDEX materialised_drives_public_query
                    ON materialised_drives(
                        vehicle_id,
                        CAST(json_extract(drive_json, '$.start_date_ms') AS INTEGER) DESC,
                        drive_id DESC
                    );
                PRAGMA user_version = 57;
                COMMIT;
                ",
            )
            .map_err(StoreError::Migrate)?;
        version = 57;
    }

    if version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(StoreError::UnsupportedSchema(version))
    }
}

fn migrate_address_cache_metadata(connection: &Connection) -> Result<(), StoreError> {
    const COLUMNS: [(&str, &str); 12] = [
        (
            "latitude",
            "ALTER TABLE address_cache ADD COLUMN latitude REAL CHECK(latitude IS NULL OR (latitude >= -90.0 AND latitude <= 90.0));",
        ),
        (
            "longitude",
            "ALTER TABLE address_cache ADD COLUMN longitude REAL CHECK(longitude IS NULL OR (longitude >= -180.0 AND longitude <= 180.0));",
        ),
        (
            "house_number",
            "ALTER TABLE address_cache ADD COLUMN house_number TEXT;",
        ),
        ("road", "ALTER TABLE address_cache ADD COLUMN road TEXT;"),
        (
            "neighbourhood",
            "ALTER TABLE address_cache ADD COLUMN neighbourhood TEXT;",
        ),
        ("city", "ALTER TABLE address_cache ADD COLUMN city TEXT;"),
        (
            "county",
            "ALTER TABLE address_cache ADD COLUMN county TEXT;",
        ),
        (
            "postcode",
            "ALTER TABLE address_cache ADD COLUMN postcode TEXT;",
        ),
        ("state", "ALTER TABLE address_cache ADD COLUMN state TEXT;"),
        (
            "state_district",
            "ALTER TABLE address_cache ADD COLUMN state_district TEXT;",
        ),
        (
            "country",
            "ALTER TABLE address_cache ADD COLUMN country TEXT;",
        ),
        (
            "raw_json",
            "ALTER TABLE address_cache ADD COLUMN raw_json TEXT CHECK(raw_json IS NULL OR json_valid(raw_json));",
        ),
    ];

    let mut migration = String::from("BEGIN IMMEDIATE;\n");
    for (column, statement) in COLUMNS {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM pragma_table_info('address_cache') WHERE name = ?1
                )",
                [column],
                |row| row.get(0),
            )
            .map_err(StoreError::Migrate)?;
        if !exists {
            migration.push_str(statement);
            migration.push('\n');
        }
    }
    migration.push_str("PRAGMA user_version = 49;\nCOMMIT;");
    connection
        .execute_batch(&migration)
        .map_err(StoreError::Migrate)
}
