// SPDX-License-Identifier: AGPL-3.0-only

//! C9 part S spec tests (C9 section 9: C9 ST1 to C9 ST10, C9 ST46 and C9 ST49).
//! Synthetic tiles only. A 1201 tile `N10E020` has every cell 0 unless stated;
//! cell `(r, c)` lies at `lat = 11 - r / 1200` and `lon = 20 + c / 1200`.

use std::{
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    time::Duration,
};

use super::super::{
    HgtTile, K_TERRAIN_MAX, K_TERRAIN_MIN, SampleOutcome, TerrainCacheError, TerrainError,
    sample_elevation, terrain_attribution, terrain_error_code, terrain_tile_for,
};
use crate::{
    runtime::lifecycle::elevation_totals,
    terrain::{K_HGT_VOID, SRTM1_SIDE as FINE, SRTM3_SIDE as N},
};

fn blank(side: usize) -> Vec<u8> {
    vec![0; side * side * 2]
}

fn put(bytes: &mut [u8], side: usize, row: usize, col: usize, value: i16) {
    let offset = (row * side + col) * 2;
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn tile(name: &str, bytes: &[u8]) -> HgtTile {
    HgtTile::from_bytes(name, bytes.to_vec()).expect("synthetic tile has an exact size")
}

/// Samples a tile named for the point it is asked about, in 1201 or 3601 size.
fn sample(name: &str, bytes: &[u8], latitude: f64, longitude: f64) -> SampleOutcome {
    sample_elevation(&tile(name, bytes), latitude, longitude).expect("sampling succeeds")
}

fn tile_name(latitude: f64, longitude: f64) -> String {
    terrain_tile_for(latitude, longitude)
        .expect("coordinates are valid")
        .name()
}

/// The four-cell fixture of C9 ST2, C9 ST3 and C9 ST6: nodes (562,562) to (563,563).
fn square(values: [i16; 4]) -> Vec<u8> {
    let mut bytes = blank(N);
    for (index, value) in values.into_iter().enumerate() {
        put(&mut bytes, N, 562 + index / 2, 562 + index % 2, value);
    }
    bytes
}

#[test]
fn c9_st01_to_st06_sampling() {
    let at =
        |bytes: &[u8], latitude: f64, longitude: f64| sample("N10E020", bytes, latitude, longitude);

    // C9 ST1 exact node: no weight on any neighbour.
    let mut cells = blank(N);
    put(&mut cells, N, 600, 600, 100);
    assert_eq!(at(&cells, 10.5, 20.5), SampleOutcome::Elevation(100));
    // A neighbour's value does not leak into an exact node.
    put(&mut cells, N, 600, 601, 900);
    put(&mut cells, N, 601, 600, 900);
    put(&mut cells, N, 601, 601, 900);
    assert_eq!(at(&cells, 10.5, 20.5), SampleOutcome::Elevation(100));

    // C9 ST2 midpoint: r = c = 562.5 exactly.
    let cells = square([100, 200, 300, 400]);
    assert_eq!(
        at(&cells, 10.53125, 20.46875),
        SampleOutcome::Elevation(250)
    );

    // C9 ST3 weights: r = 562.25, c = 562.75, exact value 225.
    assert_eq!(
        at(&cells, 10.531458333333333, 20.468958333333333),
        SampleOutcome::Elevation(225)
    );
    // The mirrored point weights the other diagonal: r = 562.75, c = 562.25
    // gives 100*.25*.75 + 200*.25*.25 + 300*.75*.75 + 400*.75*.25 = 275.
    assert_eq!(
        at(&cells, 10.531041666666667, 20.468541666666667),
        SampleOutcome::Elevation(275)
    );

    // C9 ST4 planar slope: v(r, c) = 1000 + 4c - 2r is reproduced exactly.
    let mut slope = blank(N);
    for r in 561..=564usize {
        for c in 561..=564usize {
            put(
                &mut slope,
                N,
                r,
                c,
                (1000 + 4 * c as i64 - 2 * r as i64) as i16,
            );
        }
    }
    assert_eq!(
        at(&slope, 10.53125, 20.46875),
        SampleOutcome::Elevation(2125)
    );

    // C9 ST5 rounding: exactly 100.5 rounds away from zero to 101, -100.5 to -101.
    assert_eq!(
        at(&square([100, 101, 100, 101]), 10.53125, 20.46875),
        SampleOutcome::Elevation(101)
    );
    assert_eq!(
        at(&square([-100, -101, -100, -101]), 10.53125, 20.46875),
        SampleOutcome::Elevation(-101)
    );

    // C9 ST6 voids: a void is never averaged in.
    let voided = square([100, 200, 300, K_HGT_VOID]);
    // Nearest node (562, 562).
    assert_eq!(
        at(&voided, 10.531458333333333, 20.468541666666667),
        SampleOutcome::Elevation(100)
    );
    // fr = 0.5 exactly, fc = 0.25: the tie goes south to (563, 562).
    assert_eq!(
        at(&voided, 10.53125, 20.468541666666667),
        SampleOutcome::Elevation(300)
    );
    // fr = fc = 0.5 exactly: the tie goes south and east, onto the void.
    assert_eq!(at(&voided, 10.53125, 20.46875), SampleOutcome::Void);
    // Nearest node is the void itself.
    assert_eq!(
        at(&voided, 10.531041666666667, 20.468958333333333),
        SampleOutcome::Void
    );
    // The east tie: fr = 0.25, fc = 0.5 exactly goes to (562, 563) = 200.
    assert_eq!(
        at(&voided, 10.531458333333333, 20.46875),
        SampleOutcome::Elevation(200)
    );
}

#[test]
fn c9_st07_to_st10_edges() {
    let at =
        |bytes: &[u8], latitude: f64, longitude: f64| sample("N10E020", bytes, latitude, longitude);

    // C9 ST7 validity range, inclusive at both ends.
    for (value, expected) in [
        (9_500, SampleOutcome::OutOfRange),
        (-600, SampleOutcome::OutOfRange),
        (-430, SampleOutcome::Elevation(-430)),
        (9_000, SampleOutcome::Elevation(9_000)),
        (-500, SampleOutcome::Elevation(-500)),
        (9_001, SampleOutcome::OutOfRange),
        (-501, SampleOutcome::OutOfRange),
    ] {
        assert_eq!(
            at(&square([value; 4]), 10.53125, 20.46875),
            expected,
            "value {value}"
        );
    }
    assert_eq!((K_TERRAIN_MIN, K_TERRAIN_MAX), (-500, 9_000));
    // The range applies to a void fallback's node as well.
    assert_eq!(
        at(
            &square([9_500, K_HGT_VOID, 0, 0]),
            10.531458333333333,
            20.468541666666667
        ),
        SampleOutcome::OutOfRange
    );
    // Every accepted extreme counts in the shared ascent and descent walk.
    assert_eq!(
        elevation_totals([
            Some(i64::from(K_TERRAIN_MIN)),
            Some(i64::from(K_TERRAIN_MAX))
        ]),
        (Some(9_500), Some(0))
    );

    // C9 ST8 edges: the southern edge row belongs to the tile and the clamp keeps
    // the far row inside the last cell with its full weight.
    let mut cells = blank(N);
    put(&mut cells, N, 1200, 600, 50);
    put(&mut cells, N, 1200, 601, 150);
    // r = 1200 exactly on a whole-degree latitude: without the clamp of
    // r0 = min(floor(r), n - 2) both lower nodes lie outside the grid.
    assert_eq!(at(&cells, 10.0, 20.5), SampleOutcome::Elevation(50));
    // Along row 1200: c = 600.25 weights (1200, 601) by 0.25, 50 * .75 + 150 * .25 = 75.
    assert_eq!(
        at(&cells, 10.0, 20.5 + 0.25 / 1200.0),
        SampleOutcome::Elevation(75)
    );
    // The north edge row (r = 0) and the west column (c = 0).
    put(&mut cells, N, 0, 0, 12);
    assert_eq!(at(&cells, 11.0 - 1e-12, 20.0), SampleOutcome::Elevation(12));
    // Far east column stays inside the last cell: c just below 1200.
    put(&mut cells, N, 600, 1200, 31);
    put(&mut cells, N, 601, 1200, 31);
    assert_eq!(at(&cells, 10.5, 21.0 - 1e-12), SampleOutcome::Elevation(31));
    // Whole-degree lines belong to the tile north or east.
    assert_eq!(tile_name(11.0, 20.5), "N11E020");
    assert_eq!(tile_name(10.5, 21.0), "N10E021");
    assert_eq!(tile_name(10.0, 20.0), "N10E020");

    // C9 ST9 tile names.
    assert_eq!(tile_name(-33.5, 151.2), "S34E151");
    assert_eq!(tile_name(10.5, 180.0), "N10W180");
    assert_eq!(tile_name(10.5, -180.0), "N10W180");
    assert_eq!(tile_name(90.0, 0.5), "N89E000");
    assert_eq!(tile_name(-0.5, -0.5), "S01W001");
    assert_eq!(tile_name(0.0, 0.0), "N00E000");
    assert_eq!(tile_name(-0.0, -0.0), "N00E000");
    assert_eq!(tile_name(-1.0, -1.0), "S01W001");
    assert_eq!(tile_name(-90.0, 0.0), "S90E000");
    assert_eq!(tile_name(5.0, 100.0), "N05E100");
    assert_eq!(tile_name(89.999, 179.999), "N89E179");
    // The dateline and the pole sample from their one tile: 180 and -180 are
    // the same meridian (column 0), the pole is row 0 of the N89 tile.
    let mut dateline = blank(N);
    put(&mut dateline, N, 600, 0, 33);
    for longitude in [180.0, -180.0] {
        assert_eq!(
            sample("N10W180", &dateline, 10.5, longitude),
            SampleOutcome::Elevation(33)
        );
    }
    let mut pole = blank(N);
    put(&mut pole, N, 0, 600, 44);
    assert_eq!(
        sample("N89E000", &pole, 90.0, 0.5),
        SampleOutcome::Elevation(44)
    );
    // The south pole: r = n - 1 exactly in the S90 tile, the clamp keeps the
    // cell inside the grid and the bottom row holds the value.
    let mut south_pole = blank(N);
    put(&mut south_pole, N, 1200, 600, 55);
    put(&mut south_pole, N, 1200, 601, 55);
    assert_eq!(
        sample("S90E000", &south_pole, -90.0, 0.5),
        SampleOutcome::Elevation(55)
    );

    // Coordinates no tile covers fail with the retained codes; they are not a
    // void, which would be a terminal outcome (C9 R46).
    for (latitude, longitude, code) in [
        (f64::NAN, 0.0, "nonfinite_coordinate"),
        (0.0, f64::INFINITY, "nonfinite_coordinate"),
        (90.5, 0.0, "invalid_latitude"),
        (-90.5, 0.0, "invalid_latitude"),
        (0.0, 180.5, "invalid_longitude"),
        (0.0, -180.5, "invalid_longitude"),
    ] {
        let error = terrain_tile_for(latitude, longitude).expect_err("no tile covers the point");
        assert_eq!(
            terrain_error_code(&TerrainCacheError::InvalidTile(error)),
            code
        );
        let error = sample_elevation(&tile("N00E000", &blank(N)), latitude, longitude)
            .expect_err("no tile covers the point");
        assert_eq!(
            terrain_error_code(&TerrainCacheError::InvalidTile(error)),
            code
        );
    }
    // A tile that is not the point's tile is refused, never sampled.
    assert!(matches!(
        sample_elevation(&tile("N10E021", &blank(N)), 10.5, 20.5),
        Err(TerrainError::WrongTile)
    ));

    // C9 ST10 1 arc-second geometry: side 3601, cell (1800, 1800) = 77.
    let mut fine = blank(FINE);
    put(&mut fine, FINE, 1800, 1800, 77);
    assert_eq!(
        sample("N10E020", &fine, 10.5, 20.5),
        SampleOutcome::Elevation(77)
    );
    // Quarter step: r = 1800.25 weights row 1801 by 0.25 (fc = 0).
    put(&mut fine, FINE, 1801, 1800, 177);
    assert_eq!(
        sample("N10E020", &fine, 10.5 - 0.25 / 3600.0, 20.5),
        SampleOutcome::Elevation(102)
    );

    // Indexing: byte (row * side + col) * 2, big-endian, signed, void is None.
    let offset = (1800 * FINE + 1800) * 2;
    assert_eq!(fine[offset..offset + 2], [0x00, 0x4D]);
    put(&mut fine, FINE, 5, 7, -100);
    assert_eq!(fine[(5 * FINE + 7) * 2..][..2], [0xFF, 0x9C]);
    put(&mut fine, FINE, 5, 8, K_HGT_VOID);
    let fine = tile("N10E020", &fine);
    assert_eq!(fine.side(), FINE);
    assert_eq!(fine.cell(5, 7), Some(-100));
    assert_eq!(fine.cell(7, 5), Some(0));
    assert_eq!(fine.cell(5, 8), None);
    assert_eq!(fine.cell(3600, 3600), Some(0));
    // A node outside the grid is an error, never a void.
    assert_eq!(fine.cell(3601, 0), None);
    assert!(fine.try_cell(3601, 0).is_err());
    assert!(fine.try_cell(0, 3601).is_err());
    assert_eq!(fine.try_cell(5, 8).expect("a void reads"), None);
    failed_read_is_not_a_void();
    // Only the two tile sizes with their exact byte lengths are accepted.
    for length in [FINE * FINE * 2 - 1, blank(N).len() + 2, 8, 0] {
        assert!(matches!(
            HgtTile::from_bytes("N10E020", vec![0; length]),
            Err(TerrainError::InvalidHgtLength)
        ));
    }
}

/// A file-backed tile whose read fails after it was opened is an error, never a void:
/// a void is terminal for the position (C9 R46) and a failed read is not.
fn failed_read_is_not_a_void() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("N10E020.hgt");
    let mut bytes = blank(N);
    put(&mut bytes, N, 600, 600, 100);
    put(&mut bytes, N, 600, 601, 100);
    put(&mut bytes, N, 601, 600, 100);
    put(&mut bytes, N, 601, 601, 100);
    std::fs::write(&path, &bytes).expect("write tile");
    let file_tile = HgtTile::open("N10E020", &path).expect("valid HGT");
    assert_eq!(
        sample_elevation(&file_tile, 10.5, 20.5).expect("file read"),
        SampleOutcome::Elevation(100)
    );
    // The file is cut short after it was admitted.
    let mut file = OpenOptions::new().write(true).open(&path).expect("open");
    file.seek(SeekFrom::Start(0)).expect("seek");
    file.set_len(100).expect("truncate");
    file.flush().expect("flush");
    assert!(matches!(
        sample_elevation(&file_tile, 10.5, 20.5),
        Err(TerrainError::Io(_))
    ));
    assert_eq!(file_tile.cell(600, 600), None);
    assert!(file_tile.try_cell(600, 600).is_err());
}

#[tokio::test]
async fn c9_st46_codes() {
    use crate::platform::private_directory::PrivateDirectoryError;
    use TerrainCacheError as E;

    // Network: a reqwest error from a refused loopback connection.
    crate::crypto::install_default_provider();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local address").port();
    drop(listener);
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client");
    let refused = client
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .expect_err("a refused connection");

    let io = || std::io::Error::from_raw_os_error(5);
    let store = || crate::db::StoreError::InvalidTerrainTileDigest;
    let table: Vec<(E, &str)> = vec![
        (E::Network(refused), "source_unavailable"),
        (E::BadResponse, "invalid_response"),
        (E::Timeout, "timeout"),
        (E::InvalidArchive, "invalid_archive"),
        (E::InsufficientSpace, "insufficient_free_space"),
        (E::CacheQuotaExceeded, "cache_quota_exceeded"),
        (E::Io(io()), "io"),
        (E::InvalidTile(TerrainError::Io(io())), "io"),
        (E::InvalidConfig, "invalid_config"),
        (E::EgressDenied, "egress_denied"),
        (
            E::UntrustedRoot(PrivateDirectoryError::InvalidPath),
            "untrusted_root",
        ),
        // Not in the C9 R47 table (it predates the variant): the persisted code
        // the C9-S coordinator ruling of 2026-10-21 adds.
        (E::LockingUnsupported(io()), "locking_unsupported"),
        (E::Store(Box::new(store())), "store"),
        (E::UnprovenTile, "unproven_tile"),
        (
            E::InvalidTile(TerrainError::NonFiniteCoordinate),
            "nonfinite_coordinate",
        ),
        (
            E::InvalidTile(TerrainError::InvalidLatitude),
            "invalid_latitude",
        ),
        (
            E::InvalidTile(TerrainError::InvalidLongitude),
            "invalid_longitude",
        ),
        (
            E::InvalidTile(TerrainError::InvalidTileName),
            "invalid_tile_name",
        ),
        (E::InvalidTile(TerrainError::WrongTile), "wrong_tile"),
        (
            E::InvalidTile(TerrainError::InvalidHgtLength),
            "invalid_hgt_length",
        ),
    ];
    for (error, code) in &table {
        assert_eq!(terrain_error_code(error), *code, "{error:?}");
    }
    // Only the documented merges share a code: both io rows. Every other row has
    // its own code.
    let mut codes: Vec<&str> = table.iter().map(|(_, code)| *code).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), table.len() - 1);
}

#[test]
fn c9_st49_attribution() {
    // Values are provisional until the approved text replaces them.
    let aws = terrain_attribution("aws");
    assert_eq!(aws.len(), 1);
    assert_eq!(aws[0].licence, "composite-attribution-required");
    assert_eq!(aws[0].dataset, "Terrain Tiles");
    assert!(aws[0].notice.contains("SRTM (NASA/USGS)"));
    assert!(aws[0].reference_url.starts_with("https://"));

    let esa = terrain_attribution("esa");
    assert_eq!(esa.len(), 1);
    assert_eq!(esa[0].licence, "public-domain");
    assert_eq!(esa[0].dataset, "SRTM 1 arc-second");
    assert!(esa[0].notice.contains("NASA/USGS"));

    // `cache` names no provider and an unknown origin is never under-attributed:
    // both entries, in provider order.
    for origin in ["cache", "x", "", "AWS"] {
        let both = terrain_attribution(origin);
        assert_eq!(both.len(), 2, "{origin:?}");
        assert_eq!(both[0], aws[0]);
        assert_eq!(both[1], esa[0]);
    }
    for entry in aws.iter().chain(esa) {
        for text in [
            entry.provider,
            entry.dataset,
            entry.licence,
            entry.notice,
            entry.reference_url,
        ] {
            assert!(!text.is_empty());
        }
    }
}
