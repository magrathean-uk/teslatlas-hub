// SPDX-License-Identifier: AGPL-3.0-only

//! Pure SRTM HGT tile naming and sample decoding.
//!
//! This matches the tile and sample behavior of the pinned TeslaMate SRTM
//! 0.8.0 dependency. It deliberately does not download, cache, or schedule
//! terrain work.

use std::{
    fs::{File, Metadata},
    os::unix::fs::{FileExt, MetadataExt},
    path::Path,
    sync::Arc,
};

use rustix::fs::{Mode, OFlags, open};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const SRTM3_SIDE: usize = 1_201;
pub const SRTM1_SIDE: usize = 3_601;
pub const SRTM3_BYTES: u64 = (SRTM3_SIDE as u64) * (SRTM3_SIDE as u64) * 2;
pub const SRTM1_BYTES: u64 = (SRTM1_SIDE as u64) * (SRTM1_SIDE as u64) * 2;
pub const TERRAIN_DATASET_VERSION: &str = "srtm-0.8.0-hgt";

#[derive(Debug, Error)]
pub enum TerrainError {
    #[error("coordinate is not finite")]
    NonFiniteCoordinate,
    #[error("latitude is outside WGS84 bounds")]
    InvalidLatitude,
    #[error("longitude is outside WGS84 bounds")]
    InvalidLongitude,
    #[error("invalid HGT tile name")]
    InvalidTileName,
    #[error("HGT tile name does not contain the requested coordinate")]
    WrongTile,
    #[error("unsupported HGT byte length")]
    InvalidHgtLength,
    #[error("HGT file error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HgtSide {
    Srtm3,
    Srtm1,
}

impl HgtSide {
    fn side(self) -> usize {
        match self {
            Self::Srtm3 => SRTM3_SIDE,
            Self::Srtm1 => SRTM1_SIDE,
        }
    }

    fn byte_len(self) -> u64 {
        match self {
            Self::Srtm3 => SRTM3_BYTES,
            Self::Srtm1 => SRTM1_BYTES,
        }
    }

    fn from_byte_len(length: u64) -> Result<Self, TerrainError> {
        match length {
            SRTM3_BYTES => Ok(Self::Srtm3),
            SRTM1_BYTES => Ok(Self::Srtm1),
            _ => Err(TerrainError::InvalidHgtLength),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileId {
    latitude: i32,
    longitude: i32,
}

impl TileId {
    pub fn from_coordinates(latitude: f64, longitude: f64) -> Result<Self, TerrainError> {
        validate_coordinates(latitude, longitude)?;
        Ok(Self {
            latitude: latitude.floor() as i32,
            longitude: longitude.floor() as i32,
        })
    }

    pub fn parse(name: &str) -> Result<Self, TerrainError> {
        let bytes = name.as_bytes();
        if bytes.len() != 7
            || !matches!(bytes[0], b'N' | b'S')
            || !matches!(bytes[3], b'E' | b'W')
            || !bytes[1..3].iter().all(u8::is_ascii_digit)
            || !bytes[4..7].iter().all(u8::is_ascii_digit)
        {
            return Err(TerrainError::InvalidTileName);
        }

        let latitude = i32::from(bytes[1] - b'0') * 10 + i32::from(bytes[2] - b'0');
        let longitude = i32::from(bytes[4] - b'0') * 100
            + i32::from(bytes[5] - b'0') * 10
            + i32::from(bytes[6] - b'0');
        if latitude > 90 || longitude > 180 {
            return Err(TerrainError::InvalidTileName);
        }

        Ok(Self {
            latitude: if bytes[0] == b'S' {
                -latitude
            } else {
                latitude
            },
            longitude: if bytes[3] == b'W' {
                -longitude
            } else {
                longitude
            },
        })
    }

    pub fn name(self) -> String {
        format!(
            "{}{:02}{}{:03}",
            if self.latitude < 0 { 'S' } else { 'N' },
            self.latitude.unsigned_abs(),
            if self.longitude < 0 { 'W' } else { 'E' },
            self.longitude.unsigned_abs()
        )
    }

    pub fn latitude(self) -> i32 {
        self.latitude
    }

    pub fn longitude(self) -> i32 {
        self.longitude
    }
}

pub fn tile_name(latitude: f64, longitude: f64) -> Result<String, TerrainError> {
    Ok(TileId::from_coordinates(latitude, longitude)?.name())
}

enum HgtStorage {
    Bytes(Arc<[u8]>),
    File(Arc<File>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HgtFileIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

impl HgtFileIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }
}

pub struct HgtTile {
    id: TileId,
    side: HgtSide,
    storage: HgtStorage,
}

impl HgtTile {
    pub fn from_bytes(name: &str, bytes: Vec<u8>) -> Result<Self, TerrainError> {
        let side = HgtSide::from_byte_len(bytes.len() as u64)?;
        Ok(Self {
            id: TileId::parse(name)?,
            side,
            storage: HgtStorage::Bytes(bytes.into()),
        })
    }

    pub fn open(name: &str, path: impl AsRef<Path>) -> Result<Self, TerrainError> {
        let path = path.as_ref();
        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        let file = File::from(descriptor);
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() {
            return Err(TerrainError::InvalidHgtLength);
        }
        let side = HgtSide::from_byte_len(metadata.len())?;
        Ok(Self {
            id: TileId::parse(name)?,
            side,
            storage: HgtStorage::File(Arc::new(file)),
        })
    }

    pub fn tile_id(&self) -> TileId {
        self.id
    }

    pub fn side(&self) -> usize {
        self.side.side()
    }

    pub(crate) fn file_identity(&self) -> Result<Option<HgtFileIdentity>, TerrainError> {
        match &self.storage {
            HgtStorage::Bytes(_) => Ok(None),
            HgtStorage::File(file) => Ok(Some(HgtFileIdentity::from_metadata(&file.metadata()?))),
        }
    }

    /// Hash the exact bytes admitted by this tile instance. File-backed tiles
    /// keep using the pinned descriptor even if the pathname is replaced.
    pub fn sha256_hex(&self) -> Result<String, TerrainError> {
        let mut digest = Sha256::new();
        match &self.storage {
            HgtStorage::Bytes(bytes) => digest.update(bytes),
            HgtStorage::File(file) => {
                let before = HgtFileIdentity::from_metadata(&file.metadata()?);
                let expected = self.side.byte_len();
                let mut offset = 0_u64;
                let mut buffer = [0_u8; 64 * 1024];
                while offset < expected {
                    let remaining = usize::try_from((expected - offset).min(buffer.len() as u64))
                        .map_err(|_| TerrainError::InvalidHgtLength)?;
                    let read = file.read_at(&mut buffer[..remaining], offset)?;
                    if read == 0 {
                        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
                    }
                    digest.update(&buffer[..read]);
                    offset += read as u64;
                }
                let after = HgtFileIdentity::from_metadata(&file.metadata()?);
                if before != after {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "HGT tile changed while hashing",
                    )
                    .into());
                }
            }
        }
        Ok(hex::encode(digest.finalize()))
    }

    /// Return the raw HGT cell selected by TeslaMate's SRTM dependency.
    /// There is no interpolation. Invalid and void cells return `None`.
    pub fn elevation_at(&self, latitude: f64, longitude: f64) -> Result<Option<i16>, TerrainError> {
        let requested = TileId::from_coordinates(latitude, longitude)?;
        if requested != self.id {
            return Err(TerrainError::WrongTile);
        }

        let side = self.side.side();
        let scale = (side - 1) as f64;
        let row = ((self.id.latitude as f64 + 1.0 - latitude) * scale).trunc();
        let column = ((longitude - self.id.longitude as f64) * scale).trunc();
        if row < 0.0 || column < 0.0 || row >= side as f64 || column >= side as f64 {
            return Ok(None);
        }
        let row = row as usize;
        let column = column as usize;
        let offset = ((row * side + column) * 2) as u64;
        let mut raw = [0_u8; 2];
        match &self.storage {
            HgtStorage::Bytes(bytes) => {
                let start = offset as usize;
                raw.copy_from_slice(&bytes[start..start + 2]);
            }
            HgtStorage::File(file) => {
                file.read_exact_at(&mut raw, offset)?;
            }
        }

        let elevation = i16::from_be_bytes(raw);
        if elevation == i16::MIN || !(-1_000..=10_000).contains(&elevation) {
            Ok(None)
        } else {
            Ok(Some(elevation))
        }
    }
}

fn validate_coordinates(latitude: f64, longitude: f64) -> Result<(), TerrainError> {
    if !latitude.is_finite() || !longitude.is_finite() {
        return Err(TerrainError::NonFiniteCoordinate);
    }
    if !(-90.0..=90.0).contains(&latitude) {
        return Err(TerrainError::InvalidLatitude);
    }
    if !(-180.0..=180.0).contains(&longitude) {
        return Err(TerrainError::InvalidLongitude);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::{Seek, SeekFrom, Write},
    };

    use tempfile::tempdir;

    use super::*;

    fn sparse_tile(name: &str, side: usize) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join(format!("{name}.hgt"));
        let file = fs::File::create(&path).expect("create sparse tile");
        file.set_len((side as u64) * (side as u64) * 2)
            .expect("set exact HGT length");
        (directory, path)
    }

    fn write_cell(path: &std::path::Path, side: usize, row: usize, column: usize, value: i16) {
        let mut file = OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open sparse tile");
        let offset = ((row * side + column) * 2) as u64;
        file.seek(SeekFrom::Start(offset)).expect("seek cell");
        file.write_all(&value.to_be_bytes()).expect("write cell");
    }

    fn coordinate_for_cell(
        latitude_origin: f64,
        longitude_origin: f64,
        side: usize,
        row: usize,
        column: usize,
    ) -> (f64, f64) {
        (
            latitude_origin + 1.0 - (row as f64 + 0.25) / (side - 1) as f64,
            longitude_origin + (column as f64 + 0.25) / (side - 1) as f64,
        )
    }

    #[test]
    fn names_use_floor_and_preserve_boundary_signs() {
        assert_eq!(tile_name(47.9, 19.9).unwrap(), "N47E019");
        assert_eq!(tile_name(-47.1, -19.1).unwrap(), "S48W020");
        assert_eq!(tile_name(0.0, 0.0).unwrap(), "N00E000");
        assert_eq!(tile_name(-0.0001, -0.0001).unwrap(), "S01W001");
        assert_eq!(tile_name(-90.0, -180.0).unwrap(), "S90W180");
        assert_eq!(tile_name(90.0, 180.0).unwrap(), "N90E180");
        assert_eq!(tile_name(0.0, -180.0).unwrap(), "N00W180");
        assert_eq!(tile_name(0.0, 180.0).unwrap(), "N00E180");
        assert_eq!(tile_name(1.0, 1.0).unwrap(), "N01E001");
    }

    #[test]
    fn validates_wgs84_coordinates() {
        assert!(matches!(
            tile_name(f64::NAN, 0.0),
            Err(TerrainError::NonFiniteCoordinate)
        ));
        assert!(matches!(
            tile_name(0.0, f64::INFINITY),
            Err(TerrainError::NonFiniteCoordinate)
        ));
        assert!(matches!(
            tile_name(90.000_001, 0.0),
            Err(TerrainError::InvalidLatitude)
        ));
        assert!(matches!(
            tile_name(-90.000_001, 0.0),
            Err(TerrainError::InvalidLatitude)
        ));
        assert!(matches!(
            tile_name(0.0, 180.000_001),
            Err(TerrainError::InvalidLongitude)
        ));
        assert!(matches!(
            tile_name(0.0, -180.000_001),
            Err(TerrainError::InvalidLongitude)
        ));
    }

    #[test]
    fn accepts_only_the_two_exact_sparse_hgt_sizes() {
        for (name, side) in [("N00E000", SRTM3_SIDE), ("N00E001", SRTM1_SIDE)] {
            let (_directory, path) = sparse_tile(name, side);
            let tile = HgtTile::open(name, &path).expect("valid sparse HGT");
            assert_eq!(tile.side(), side);
        }

        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("bad.hgt");
        fs::write(&path, [0_u8; 4]).expect("bad HGT");
        assert!(matches!(
            HgtTile::open("N00E000", &path),
            Err(TerrainError::InvalidHgtLength)
        ));
    }

    #[test]
    fn selects_one_raw_big_endian_cell_without_interpolation() {
        let (_directory, path) = sparse_tile("N00E000", SRTM3_SIDE);
        write_cell(&path, SRTM3_SIDE, 600, 600, 123);
        write_cell(&path, SRTM3_SIDE, 600, 601, 456);
        let tile = HgtTile::open("N00E000", &path).expect("valid HGT");

        let first = coordinate_for_cell(0.0, 0.0, SRTM3_SIDE, 600, 600);
        let second = coordinate_for_cell(0.0, 0.0, SRTM3_SIDE, 600, 601);
        assert_eq!(tile.elevation_at(first.0, first.1).unwrap(), Some(123));
        assert_eq!(tile.elevation_at(second.0, second.1).unwrap(), Some(456));
    }

    #[test]
    fn opened_tile_pins_the_validated_file_across_path_replacement() {
        let (_directory, path) = sparse_tile("N00E000", SRTM3_SIDE);
        write_cell(&path, SRTM3_SIDE, 600, 600, 321);
        let tile = HgtTile::open("N00E000", &path).expect("valid HGT");
        let expected_hash = tile.sha256_hex().expect("original tile hash");
        let retained = path.with_extension("retained");
        fs::rename(&path, &retained).expect("retain original tile inode");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .expect("run mkfifo")
                .success()
        );

        let coordinate = coordinate_for_cell(0.0, 0.0, SRTM3_SIDE, 600, 600);
        assert_eq!(
            tile.elevation_at(coordinate.0, coordinate.1).unwrap(),
            Some(321)
        );
        assert_eq!(tile.sha256_hex().expect("pinned tile hash"), expected_hash);
    }

    #[test]
    fn void_and_out_of_range_values_are_missing() {
        let (_directory, path) = sparse_tile("S00W001", SRTM3_SIDE);
        write_cell(&path, SRTM3_SIDE, 1, 1, i16::MIN);
        write_cell(&path, SRTM3_SIDE, 1, 2, -1_001);
        write_cell(&path, SRTM3_SIDE, 1, 3, 10_001);
        write_cell(&path, SRTM3_SIDE, 1, 4, -999);
        let tile = HgtTile::open("S00W001", &path).expect("valid HGT");

        for column in 1..=3 {
            let coordinate = coordinate_for_cell(0.0, -1.0, SRTM3_SIDE, 1, column);
            assert_eq!(tile.elevation_at(coordinate.0, coordinate.1).unwrap(), None);
        }
        let coordinate = coordinate_for_cell(0.0, -1.0, SRTM3_SIDE, 1, 4);
        assert_eq!(
            tile.elevation_at(coordinate.0, coordinate.1).unwrap(),
            Some(-999)
        );
    }

    #[test]
    fn rejects_coordinates_from_a_different_tile() {
        let bytes = vec![0_u8; 4];
        assert!(matches!(
            HgtTile::from_bytes("N00E000", bytes),
            Err(TerrainError::InvalidHgtLength)
        ));

        let (_directory, path) = sparse_tile("N00E000", SRTM3_SIDE);
        let tile = HgtTile::open("N00E000", &path).expect("valid HGT");
        assert!(matches!(
            tile.elevation_at(1.0, 0.0),
            Err(TerrainError::WrongTile)
        ));
    }
}
