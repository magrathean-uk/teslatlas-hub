-- SPDX-License-Identifier: AGPL-3.0-only
-- Reproduces the Teslatlas Protocol hub-sync-v1@1.3.0 contract prepared-pack-v1.sql, published under Apache-2.0.

CREATE TABLE prepared_metadata (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    payload TEXT NOT NULL CHECK (payload = 'teslatlas-prepared-v1'),
    payload_version INTEGER NOT NULL CHECK (payload_version = 1),
    scope TEXT NOT NULL CHECK (scope = 'map_months'),
    map_style TEXT NOT NULL CHECK (map_style = 'route-stroke-v8-opaque'),
    tile_geometry_version TEXT NOT NULL CHECK (tile_geometry_version = 'raster-v9-rounded-tile-px'),
    artifact_schema_version INTEGER NOT NULL CHECK (artifact_schema_version = 1),
    artifact_id TEXT NOT NULL CHECK (length(artifact_id) BETWEEN 1 AND 4096),
    vehicle_id TEXT NOT NULL CHECK (length(vehicle_id) = 36),
    input_manifest_id TEXT NOT NULL CHECK (length(input_manifest_id) BETWEEN 1 AND 4096),
    input_receipt_id TEXT NOT NULL CHECK (length(input_receipt_id) BETWEEN 1 AND 4096),
    input_manifest_schema TEXT NOT NULL CHECK (input_manifest_schema IN ('2.1', '2.2')),
    input_sequence INTEGER NOT NULL CHECK (input_sequence BETWEEN 1 AND 9007199254740991),
    algorithm_version TEXT NOT NULL CHECK (length(algorithm_version) BETWEEN 1 AND 128),
    window_from_ms INTEGER NOT NULL CHECK (window_from_ms >= 0),
    window_to_ms INTEGER NOT NULL CHECK (window_to_ms > window_from_ms)
) WITHOUT ROWID;

CREATE TABLE map_months (
    month TEXT NOT NULL PRIMARY KEY CHECK (
        length(month) = 7 AND substr(month, 5, 1) = '-' AND
        CAST(substr(month, 6, 2) AS INTEGER) BETWEEN 1 AND 12
    ),
    from_ms INTEGER NOT NULL CHECK (from_ms >= 0),
    to_ms INTEGER NOT NULL CHECK (to_ms > from_ms),
    resolution TEXT NOT NULL CHECK (resolution IN ('readyData', 'readyEmpty')),
    drive_count INTEGER NOT NULL CHECK (drive_count BETWEEN 0 AND 9007199254740991),
    tile_count INTEGER NOT NULL CHECK (tile_count BETWEEN 0 AND 4096),
    CHECK (
        (resolution = 'readyData' AND tile_count > 0) OR
        (resolution = 'readyEmpty' AND tile_count = 0)
    )
) WITHOUT ROWID;

CREATE TABLE map_tiles (
    month TEXT NOT NULL,
    zoom INTEGER NOT NULL CHECK (zoom BETWEEN 2 AND 13),
    tile_x INTEGER NOT NULL CHECK (tile_x >= 0 AND tile_x < (1 << zoom)),
    tile_y INTEGER NOT NULL CHECK (tile_y >= 0 AND tile_y < (1 << zoom)),
    segments BLOB NOT NULL CHECK (
        typeof(segments) = 'blob' AND
        length(segments) BETWEEN 8 AND 1600000 AND
        length(segments) % 8 = 0
    ),
    PRIMARY KEY (month, zoom, tile_x, tile_y),
    FOREIGN KEY (month) REFERENCES map_months(month)
) WITHOUT ROWID;
