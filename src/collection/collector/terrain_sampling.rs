// SPDX-License-Identifier: AGPL-3.0-only

// Terrain sampling (C9 part S): the tile of a point, the bilinear sampler and the
// attribution records. Pure functions over the public SRTM HGT layout: one file
// per whole-degree cell, named by its south-west corner, `n` by `n` big-endian
// `i16` samples in metres with the first row on the northern edge and columns
// running west to east, `n` being 3601 (1 arc-second) or 1201 (3 arc-seconds),
// and -32768 marking a void. Edge rows and columns are duplicated in the
// neighbouring cell, so the four nodes around any point always lie in the tile
// of that point. The stored error codes (C9 R47) are mapped by `terrain_error_code`.

/// Lowest accepted elevation in metres (C9 R35).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by the terrain sampler of C9 Part P")
)]
const K_TERRAIN_MIN: i16 = -500;
/// Highest accepted elevation in metres (C9 R35).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by the terrain sampler of C9 Part P")
)]
const K_TERRAIN_MAX: i16 = 9_000;

/// Result of sampling one point.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by the terrain sampler of C9 Part P")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SampleOutcome {
    /// Whole metres inside `K_TERRAIN_MIN..=K_TERRAIN_MAX`.
    Elevation(i16),
    /// The point lies in an unknown (void) area.
    Void,
    /// The rounded value is outside the accepted range.
    OutOfRange,
}

/// The tile of a point (C9 R30). A point on a whole-degree line belongs to the
/// tile north or east of it; 180 and -180 are one meridian, and the pole belongs
/// to the northernmost row. Invalid coordinates fail with the retained codes.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by the terrain sampler of C9 Part P")
)]
fn terrain_tile_for(latitude: f64, longitude: f64) -> Result<TileId, TerrainError> {
    TileId::from_coordinates(latitude, longitude)?;
    let longitude = if longitude == 180.0 {
        -180.0
    } else {
        longitude
    };
    let latitude_origin = (latitude.floor() as i32).min(89);
    let longitude_origin = longitude.floor() as i32;
    TileId::parse(&format!(
        "{}{:02}{}{:03}",
        if latitude_origin < 0 { 'S' } else { 'N' },
        latitude_origin.unsigned_abs(),
        if longitude_origin < 0 { 'W' } else { 'E' },
        longitude_origin.unsigned_abs(),
    ))
}

/// Rounds half away from zero and applies the validity range (C9 R34, C9 R35).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by the terrain sampler of C9 Part P")
)]
fn finish_sample(value: f64) -> SampleOutcome {
    let rounded = value.round();
    if rounded < f64::from(K_TERRAIN_MIN) || rounded > f64::from(K_TERRAIN_MAX) {
        SampleOutcome::OutOfRange
    } else {
        SampleOutcome::Elevation(rounded as i16)
    }
}

/// Samples the tile that holds the point (C9 R31 to C9 R35). A pure function of
/// the tile bytes and the coordinates, in IEEE 754 double arithmetic evaluated in
/// the stated order. It fails, so that the caller can retry, for a coordinate no
/// tile covers (the codes of C9 R47), for a tile that is not the point's tile
/// (`WrongTile`) and for a failed read of a file-backed tile (`Io`); none of
/// these is a void, which is a terminal outcome (C9 R46).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by the terrain sampler of C9 Part P")
)]
pub(crate) fn sample_elevation(
    tile: &HgtTile,
    latitude: f64,
    longitude: f64,
) -> Result<SampleOutcome, TerrainError> {
    let requested = terrain_tile_for(latitude, longitude)?;
    if requested != tile.tile_id() {
        return Err(TerrainError::WrongTile);
    }
    // Past the two checks above the grid is the one of the point's own tile, so
    // the four nodes lie inside it (C9 R31).
    let longitude = if longitude == 180.0 {
        -180.0
    } else {
        longitude
    };
    let side = tile.side();
    let span = (side - 1) as f64;
    let r = (f64::from(requested.latitude() + 1) - latitude) * span;
    let c = (longitude - f64::from(requested.longitude())) * span;
    let r = r.clamp(0.0, span);
    let c = c.clamp(0.0, span);
    let r0 = (r.floor() as usize).min(side - 2);
    let c0 = (c.floor() as usize).min(side - 2);
    let fr = r - r0 as f64;
    let fc = c - c0 as f64;
    let nodes = [
        tile.try_cell(r0, c0)?,
        tile.try_cell(r0, c0 + 1)?,
        tile.try_cell(r0 + 1, c0)?,
        tile.try_cell(r0 + 1, c0 + 1)?,
    ];
    if let [Some(v00), Some(v01), Some(v10), Some(v11)] = nodes {
        let (v00, v01, v10, v11) = (
            f64::from(v00),
            f64::from(v01),
            f64::from(v10),
            f64::from(v11),
        );
        let e = v00 * (1.0 - fr) * (1.0 - fc)
            + v01 * (1.0 - fr) * fc
            + v10 * fr * (1.0 - fc)
            + v11 * fr * fc;
        return Ok(finish_sample(e));
    }
    // A void is unknown, never averaged in: the nearest node decides, and a tie
    // goes to the southern or eastern node (C9 R33). It is one of the four nodes
    // already read, so no node is read twice.
    let nearest = nodes[2 * usize::from(fr >= 0.5) + usize::from(fc >= 0.5)];
    Ok(match nearest {
        Some(value) => finish_sample(f64::from(value)),
        None => SampleOutcome::Void,
    })
}

/// One provider's attribution record (C9 R50).
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the attribution reader of G2-7 (C9 R51)")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerrainAttribution {
    pub provider: &'static str,
    pub dataset: &'static str,
    pub licence: &'static str,
    pub notice: &'static str,
    pub reference_url: &'static str,
}

/// Provisional attribution records for a stored `dataset_source` (C9 R50). An
/// origin that names no provider (`cache`) or is unknown lists both providers, so
/// attribution is never short. The table is derived at read time; the strings
/// are provisional until the approved text replaces them.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the attribution reader of G2-7 (C9 R51)")
)]
pub(crate) fn terrain_attribution(dataset_source: &str) -> &'static [TerrainAttribution] {
    const AWS: TerrainAttribution = TerrainAttribution {
        provider: "AWS Open Data",
        dataset: "Terrain Tiles",
        licence: "composite-attribution-required",
        notice: "Elevation data include SRTM (NASA/USGS); the dataset attribution document lists every source.",
        reference_url: "https://github.com/tilezen/joerd/blob/master/docs/attribution.md",
    };
    const ESA: TerrainAttribution = TerrainAttribution {
        provider: "ESA STEP",
        dataset: "SRTM 1 arc-second",
        licence: "public-domain",
        notice: "SRTM data courtesy of NASA/USGS.",
        reference_url: "https://step.esa.int/auxdata/dem/SRTMGL1/",
    };
    match dataset_source {
        "aws" => &[AWS],
        "esa" => &[ESA],
        _ => &[AWS, ESA],
    }
}

/// The stable stored code of a cache error (C9 R47), including the code of the
/// variant that C9 R47's table predates, `LockingUnsupported`. The match is
/// exhaustive with no catch-all arm, so a new cache or tile variant must be given
/// its code here. The acquisition's own failures (C9 R47: `LocalTimeout`,
/// `TileMismatch`, `HashMismatch`) belong to C9 Part A, which maps them to `io`,
/// `wrong_tile` and `unproven_tile`.
fn terrain_error_code(error: &TerrainCacheError) -> &'static str {
    match error {
        TerrainCacheError::Network(_) => "source_unavailable",
        TerrainCacheError::BadResponse => "invalid_response",
        TerrainCacheError::Timeout => "timeout",
        TerrainCacheError::InvalidArchive => "invalid_archive",
        TerrainCacheError::InsufficientSpace => "insufficient_free_space",
        TerrainCacheError::CacheQuotaExceeded => "cache_quota_exceeded",
        TerrainCacheError::Io(_) => "io",
        TerrainCacheError::InvalidConfig => "invalid_config",
        TerrainCacheError::EgressDenied => "egress_denied",
        TerrainCacheError::UntrustedRoot(_) => "untrusted_root",
        TerrainCacheError::LockingUnsupported(_) => "locking_unsupported",
        TerrainCacheError::Store(_) => "store",
        TerrainCacheError::UnprovenTile => "unproven_tile",
        TerrainCacheError::InvalidTile(tile_error) => match tile_error {
            TerrainError::NonFiniteCoordinate => "nonfinite_coordinate",
            TerrainError::InvalidLatitude => "invalid_latitude",
            TerrainError::InvalidLongitude => "invalid_longitude",
            TerrainError::InvalidTileName => "invalid_tile_name",
            TerrainError::WrongTile => "wrong_tile",
            TerrainError::InvalidHgtLength => "invalid_hgt_length",
            TerrainError::Io(_) => "io",
        },
    }
}
