// SPDX-License-Identifier: AGPL-3.0-only

//! The compact kept replay base of a vehicle's import (DEC-250): one
//! versioned, immutable, zstd-compressed image of the sealed physical stage,
//! made from a private `VACUUM INTO` candidate whose integrity and logical
//! digest are checked before it is compressed. A handback that has to plan
//! decodes it, bounded, straight into one owned writable sealed working
//! stage and authenticates the decoded rows before any edit.
//!
//! Layout: a fixed header (magic, format, vehicle, binding digest, logical
//! digest, decoded byte count and the stage's limits) followed by exactly one
//! zstd frame of the compact SQLite image. The digest of the whole file and
//! its length are recorded by the caller in the catalogue.

use std::{
    ffi::OsString,
    io::{BufReader, Read, Write},
    os::{fd::AsFd, unix::fs::FileExt},
    path::Path,
};

use rustix::fs::{AtFlags, fstat, fsync, renameat, unlinkat};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    STAGE_FILE_EXTENSION, STAGING_DIRECTORY, SourceDbStage, SourceDbStageError,
    SourceDbStageFormat, SourceDbStageLimits, SourceDbStageState, available_bytes,
    create_private_stage_file, ensure_private_child_directory, ensure_private_directory,
    open_private_directory, open_private_stage_file_at,
};
use crate::protocol::Sha256Digest;

/// The only archive format this build writes and reads.
pub(crate) const KEPT_ARCHIVE_FORMAT: u32 = 1;
const KEPT_ARCHIVE_MAGIC: [u8; 8] = *b"HUBKEPT\n";
const HEADER_BYTES: usize = 8 + 4 + 16 + 32 + 32 + 8 * 4;
const BLOCK_BYTES: usize = 1024 * 1024;
const COMPRESSION_LEVEL: i32 = 9;
/// zstd's window for the image: matches reach back 128 MiB, which the
/// default decoder limit accepts.
const WINDOW_LOG: u32 = 27;

#[cfg(test)]
thread_local! {
    /// A simulated crash of the next archive build at the named point
    /// (`candidate`: the compacted candidate exists; `partial`: the archive
    /// is written but not yet renamed into place). Tests catch the panic.
    pub(crate) static KEPT_ARCHIVE_CRASH_AT: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn crash_point(point: &'static str) {
    if KEPT_ARCHIVE_CRASH_AT.with(|at| at.get() == Some(point)) {
        KEPT_ARCHIVE_CRASH_AT.with(|at| at.set(None));
        panic!("simulated crash at kept archive point {point}");
    }
}

#[cfg(not(test))]
fn crash_point(_point: &'static str) {}

/// Whose base an archive is: the vehicle and a digest of its import binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeptArchiveIdentity {
    pub(crate) vehicle_id: Uuid,
    pub(crate) binding: [u8; 32],
}

/// What a written archive is, for its catalogue record and the size report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeptArchiveReceipt {
    pub(crate) format: u32,
    /// SHA-256 of the whole archive file.
    pub(crate) digest: Sha256Digest,
    /// The archive file's length.
    pub(crate) bytes: u64,
    /// The compact SQLite image's length, the decode's exact output.
    pub(crate) decoded_bytes: u64,
    /// The stage's logical digest, the same before and after compaction.
    pub(crate) logical: Sha256Digest,
    /// The sealed stage file's length the archive was made from.
    pub(crate) raw_bytes: u64,
    /// The most scratch the build held at once: candidate plus archive.
    pub(crate) peak_scratch_bytes: u64,
}

/// The catalogue's record of an archive, checked before and while it is
/// decoded.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KeptArchiveExpected<'a> {
    pub(crate) identity: KeptArchiveIdentity,
    pub(crate) logical: &'a str,
    pub(crate) digest: &'a str,
    pub(crate) bytes: u64,
    pub(crate) decoded_bytes: u64,
}

struct KeptArchiveHeader {
    format: u32,
    identity: KeptArchiveIdentity,
    logical: [u8; 32],
    decoded_bytes: u64,
    limits: SourceDbStageLimits,
}

impl KeptArchiveHeader {
    fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut bytes = [0_u8; HEADER_BYTES];
        let mut at = 0;
        let mut put = |part: &[u8]| {
            bytes[at..at + part.len()].copy_from_slice(part);
            at += part.len();
        };
        put(&KEPT_ARCHIVE_MAGIC);
        put(&self.format.to_be_bytes());
        put(self.identity.vehicle_id.as_bytes());
        put(&self.identity.binding);
        put(&self.logical);
        put(&self.decoded_bytes.to_be_bytes());
        put(&self.limits.max_rows.to_be_bytes());
        put(&self.limits.max_stage_bytes.to_be_bytes());
        put(&self.limits.minimum_free_bytes.to_be_bytes());
        bytes
    }

    fn decode(bytes: &[u8; HEADER_BYTES]) -> Result<Self, SourceDbStageError> {
        if bytes[..8] != KEPT_ARCHIVE_MAGIC {
            return Err(SourceDbStageError::KeptArchiveInvalid("not a kept archive"));
        }
        let mut at = 8;
        let mut take = |length: usize| {
            let part = &bytes[at..at + length];
            at += length;
            part
        };
        let u64_of = |part: &[u8]| u64::from_be_bytes(part.try_into().expect("eight bytes"));
        let format = u32::from_be_bytes(take(4).try_into().expect("four bytes"));
        if format != KEPT_ARCHIVE_FORMAT {
            return Err(SourceDbStageError::KeptArchiveInvalid(
                "the kept archive's format is not one this build reads",
            ));
        }
        let vehicle_id = Uuid::from_slice(take(16)).expect("sixteen bytes");
        let binding = take(32).try_into().expect("32 bytes");
        let logical = take(32).try_into().expect("32 bytes");
        let decoded_bytes = u64_of(take(8));
        let limits = SourceDbStageLimits {
            max_rows: u64_of(take(8)),
            max_stage_bytes: u64_of(take(8)),
            minimum_free_bytes: u64_of(take(8)),
        };
        Ok(Self {
            format,
            identity: KeptArchiveIdentity {
                vehicle_id,
                binding,
            },
            logical,
            decoded_bytes,
            limits,
        })
    }
}

/// Counts and hashes every byte written through it.
struct HashingWriter<W> {
    inner: W,
    hash: Sha256,
    bytes: u64,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.hash.update(&buffer[..written]);
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Counts and hashes every byte read through it.
struct HashingReader<R> {
    inner: R,
    hash: Sha256,
    bytes: u64,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hash.update(&buffer[..read]);
        self.bytes += read as u64;
        Ok(read)
    }
}

/// zstd's worst-case output for `bytes` of input, rounded up generously, so
/// the archive's admission never under-reserves.
fn compressed_bound(bytes: u64) -> Option<u64> {
    bytes
        .checked_add(bytes / 128)?
        .checked_add(1024 * 1024)?
        .checked_add(HEADER_BYTES as u64)
}

fn descriptor_len(descriptor: impl AsFd, path: &Path) -> Result<u64, SourceDbStageError> {
    let stat = fstat(descriptor).map_err(|source| SourceDbStageError::SecureInspect {
        path: path.to_path_buf(),
        source,
    })?;
    u64::try_from(stat.st_size).map_err(|_| SourceDbStageError::InvalidStoredAccounting)
}

fn io(error: std::io::Error) -> SourceDbStageError {
    SourceDbStageError::Durability(error)
}

impl SourceDbStage {
    /// Writes this sealed physical stage's kept archive as `file_name` in
    /// the private directory `kept_dir`, durably (file and directory synced)
    /// and complete or not at all: it is written under a partial name and
    /// renamed into place. The private compacted candidate lives under
    /// `<kept_dir>/.staging` only while the archive is made.
    pub(crate) fn write_kept_archive(
        &self,
        kept_dir: impl AsRef<Path>,
        file_name: &str,
        identity: KeptArchiveIdentity,
    ) -> Result<KeptArchiveReceipt, SourceDbStageError> {
        let format = self.format()?;
        if format != SourceDbStageFormat::PhysicalV3 {
            return Err(SourceDbStageError::TableFormatMismatch {
                format: format.as_str(),
                table: "stage_rows",
            });
        }
        let stats = self.stats()?;
        if stats.state != SourceDbStageState::Sealed {
            return Err(SourceDbStageError::StageNotSealed);
        }
        let logical = self.sealed_logical_digest()?;
        let raw_bytes = descriptor_len(self.file_descriptor.as_fd(), &self.path)?;
        let kept = ensure_private_directory(kept_dir.as_ref())?;
        let staging = ensure_private_child_directory(&kept, STAGING_DIRECTORY)?;
        // The candidate is at most the stage's pages, the archive at most
        // zstd's bound of the candidate.
        let required = compressed_bound(raw_bytes)
            .and_then(|bound| bound.checked_add(raw_bytes))
            .and_then(|bound| bound.checked_add(stats.limits.minimum_free_bytes))
            .ok_or(SourceDbStageError::StageCapacityOverflow)?;
        let available = available_bytes(&staging.path)?;
        if available < required {
            return Err(SourceDbStageError::InsufficientFreeSpace {
                required,
                available,
            });
        }

        let candidate_name =
            OsString::from(format!("{}.{}", Uuid::new_v4(), STAGE_FILE_EXTENSION));
        let candidate_path = staging.path.join(&candidate_name);
        let (candidate_descriptor, _) =
            create_private_stage_file(&staging, &candidate_name, &candidate_path)?;
        drop(candidate_descriptor);
        let built = (|| -> Result<KeptArchiveReceipt, SourceDbStageError> {
            let target = candidate_path
                .to_str()
                .ok_or(SourceDbStageError::InvalidStagePath)?;
            self.connection.execute("VACUUM INTO ?1", [target])?;
            crash_point("candidate");
            // Reopened as a sealed stage: SQLite's full integrity check, the
            // accounting, then the original rows.
            let candidate = Self::open_sealed(&candidate_path)?;
            let compacted = candidate.stats()?;
            if compacted.limits != stats.limits
                || compacted.row_count != stats.row_count
                || compacted.payload_bytes != stats.payload_bytes
            {
                return Err(SourceDbStageError::KeptArchiveInvalid(
                    "the compacted candidate's accounting differs from the stage",
                ));
            }
            if candidate.sealed_logical_digest()? != logical {
                return Err(SourceDbStageError::KeptArchiveInvalid(
                    "the compacted candidate's rows differ from the stage",
                ));
            }
            let decoded_bytes = descriptor_len(candidate.file_descriptor.as_fd(), &candidate.path)?;
            let receipt = candidate.compress_into(
                &kept,
                file_name,
                KeptArchiveHeader {
                    format: KEPT_ARCHIVE_FORMAT,
                    identity,
                    logical: *logical.as_bytes(),
                    decoded_bytes,
                    limits: stats.limits,
                },
            )?;
            candidate.discard()?;
            Ok(KeptArchiveReceipt {
                raw_bytes,
                peak_scratch_bytes: decoded_bytes.saturating_add(receipt.bytes),
                ..receipt
            })
        })();
        if built.is_err() {
            let _ = unlinkat(&staging.descriptor, &candidate_name, AtFlags::empty());
        }
        built
    }

    fn compress_into(
        &self,
        kept: &super::PrivateDirectory,
        file_name: &str,
        header: KeptArchiveHeader,
    ) -> Result<KeptArchiveReceipt, SourceDbStageError> {
        if file_name.contains('/') || file_name.starts_with('.') {
            return Err(SourceDbStageError::InvalidStagePath);
        }
        let partial_name = OsString::from(format!("{file_name}.partial"));
        let partial_path = kept.path.join(&partial_name);
        let (descriptor, _) = create_private_stage_file(kept, &partial_name, &partial_path)?;
        let written = (|| -> Result<(Sha256Digest, u64), SourceDbStageError> {
            let file = std::fs::File::from(descriptor);
            let mut writer = HashingWriter {
                inner: std::io::BufWriter::with_capacity(BLOCK_BYTES, file),
                hash: Sha256::new(),
                bytes: 0,
            };
            writer.write_all(&header.encode()).map_err(io)?;
            let mut encoder =
                zstd::stream::write::Encoder::new(writer, COMPRESSION_LEVEL).map_err(io)?;
            encoder.include_checksum(true).map_err(io)?;
            encoder.window_log(WINDOW_LOG).map_err(io)?;
            encoder.long_distance_matching(true).map_err(io)?;
            encoder
                .set_pledged_src_size(Some(header.decoded_bytes))
                .map_err(io)?;
            let source = std::fs::File::from(
                self.file_descriptor
                    .as_fd()
                    .try_clone_to_owned()
                    .map_err(io)?,
            );
            let mut buffer = vec![0_u8; BLOCK_BYTES];
            let mut offset = 0_u64;
            loop {
                let read = source.read_at(&mut buffer, offset).map_err(io)?;
                if read == 0 {
                    break;
                }
                encoder.write_all(&buffer[..read]).map_err(io)?;
                offset += read as u64;
            }
            if offset != header.decoded_bytes {
                return Err(SourceDbStageError::KeptArchiveInvalid(
                    "the compacted candidate changed while it was compressed",
                ));
            }
            let mut writer = encoder.finish().map_err(io)?;
            writer.flush().map_err(io)?;
            let digest = Sha256Digest::from_bytes(writer.hash.finalize().into());
            let bytes = writer.bytes;
            let file = writer.inner.into_inner().map_err(|error| io(error.into_error()))?;
            file.sync_all().map_err(io)?;
            Ok((digest, bytes))
        })();
        let (digest, bytes) = match written {
            Ok(written) => written,
            Err(error) => {
                let _ = unlinkat(&kept.descriptor, &partial_name, AtFlags::empty());
                return Err(error);
            }
        };
        crash_point("partial");
        renameat(&kept.descriptor, &partial_name, &kept.descriptor, file_name).map_err(
            |source| SourceDbStageError::RemoveStage {
                path: partial_path.clone(),
                source: std::io::Error::from(source),
            },
        )?;
        fsync(&kept.descriptor).map_err(|source| SourceDbStageError::SyncStageDirectory {
            path: kept.path.clone(),
            source,
        })?;
        Ok(KeptArchiveReceipt {
            format: header.format,
            digest,
            bytes,
            decoded_bytes: header.decoded_bytes,
            logical: Sha256Digest::from_bytes(header.logical),
            raw_bytes: 0,
            peak_scratch_bytes: 0,
        })
    }

    /// Decodes the kept archive `file_name` of `kept_dir`, as `expected`
    /// records it, into one new private working stage under
    /// `<kept_dir>/.staging`, opened writable and still sealed so
    /// [`SourceDbStage::edit_sealed`] can plan it. The decode is bounded by
    /// the recorded decoded size; the archive's length and digest and the
    /// decoded rows' logical digest are checked before the stage is
    /// returned, so nothing is edited before it is authenticated. The
    /// working stage needs the stage's own byte limit free, as a copy does.
    pub(crate) fn decode_kept_archive(
        kept_dir: impl AsRef<Path>,
        file_name: &str,
        expected: KeptArchiveExpected<'_>,
    ) -> Result<Self, SourceDbStageError> {
        if file_name.contains('/') || file_name.starts_with('.') {
            return Err(SourceDbStageError::InvalidStagePath);
        }
        let kept = open_private_directory(kept_dir.as_ref())?;
        let archive_path = kept.path.join(file_name);
        let (descriptor, _) =
            open_private_stage_file_at(&kept, file_name.as_ref(), &archive_path, false)?;
        if descriptor_len(descriptor.as_fd(), &archive_path)? != expected.bytes {
            return Err(SourceDbStageError::KeptArchiveInvalid(
                "the kept archive's size differs from its record",
            ));
        }
        let mut reader = HashingReader {
            inner: std::fs::File::from(descriptor),
            hash: Sha256::new(),
            bytes: 0,
        };
        let mut header = [0_u8; HEADER_BYTES];
        reader.read_exact(&mut header).map_err(|_| {
            SourceDbStageError::KeptArchiveInvalid("the kept archive's header is truncated")
        })?;
        let header = KeptArchiveHeader::decode(&header)?;
        if header.identity != expected.identity {
            return Err(SourceDbStageError::KeptArchiveInvalid(
                "the kept archive belongs to another vehicle or binding",
            ));
        }
        if Sha256Digest::from_bytes(header.logical).to_string() != expected.logical
            || header.decoded_bytes != expected.decoded_bytes
        {
            return Err(SourceDbStageError::KeptArchiveInvalid(
                "the kept archive's header differs from its record",
            ));
        }
        header.limits.validate()?;
        if header.decoded_bytes > header.limits.max_stage_bytes {
            return Err(SourceDbStageError::KeptArchiveDecodedSizeExceeded {
                maximum: header.limits.max_stage_bytes,
            });
        }
        let staging = ensure_private_child_directory(&kept, STAGING_DIRECTORY)?;
        let required = header
            .limits
            .max_stage_bytes
            .checked_add(header.limits.minimum_free_bytes)
            .ok_or(SourceDbStageError::StageCapacityOverflow)?;
        let available = available_bytes(&staging.path)?;
        if available < required {
            return Err(SourceDbStageError::InsufficientFreeSpace {
                required,
                available,
            });
        }
        let working_name = OsString::from(format!("{}.{}", Uuid::new_v4(), STAGE_FILE_EXTENSION));
        let working_path = staging.path.join(&working_name);
        let (working_descriptor, working_identity) =
            create_private_stage_file(&staging, &working_name, &working_path)?;
        let decoded = (|| -> Result<(), SourceDbStageError> {
            let target = std::fs::File::from(
                working_descriptor
                    .as_fd()
                    .try_clone_to_owned()
                    .map_err(io)?,
            );
            let corrupt =
                |_| SourceDbStageError::KeptArchiveInvalid("the kept archive does not decode");
            let mut decoder =
                zstd::stream::read::Decoder::with_buffer(BufReader::with_capacity(BLOCK_BYTES, reader))
                    .map_err(io)?
                    .single_frame();
            let mut buffer = vec![0_u8; BLOCK_BYTES];
            let mut offset = 0_u64;
            loop {
                let read = decoder.read(&mut buffer).map_err(corrupt)?;
                if read == 0 {
                    break;
                }
                let next = offset
                    .checked_add(read as u64)
                    .filter(|next| *next <= header.decoded_bytes)
                    .ok_or(SourceDbStageError::KeptArchiveDecodedSizeExceeded {
                        maximum: header.decoded_bytes,
                    })?;
                target.write_all_at(&buffer[..read], offset).map_err(io)?;
                offset = next;
            }
            if offset != header.decoded_bytes {
                return Err(SourceDbStageError::KeptArchiveInvalid(
                    "the kept archive decodes short of its record",
                ));
            }
            let mut rest = decoder.finish();
            let mut trailing = [0_u8; 1];
            if rest.read(&mut trailing).map_err(io)? != 0 {
                return Err(SourceDbStageError::KeptArchiveInvalid(
                    "the kept archive has bytes after its frame",
                ));
            }
            let reader = rest.into_inner();
            if reader.bytes != expected.bytes
                || Sha256Digest::from_bytes(reader.hash.finalize().into()).to_string()
                    != expected.digest
            {
                return Err(SourceDbStageError::KeptArchiveInvalid(
                    "the kept archive's digest differs from its record",
                ));
            }
            target.sync_all().map_err(io)
        })();
        #[cfg(test)]
        crate::hub_pack::record_verification(
            "kept-archive-decode",
            archive_path.display(),
            decoded.is_ok(),
        );
        #[cfg(test)]
        if decoded.is_ok()
            && let Some(damage) = super::AFTER_SEALED_COPY.with(|hook| hook.borrow_mut().take())
        {
            damage(&working_path);
        }
        if let Err(error) = decoded {
            drop(working_descriptor);
            let _ = unlinkat(&staging.descriptor, &working_name, AtFlags::empty());
            return Err(error);
        }
        let working = Self::open_private_sealed_copy(
            staging,
            working_name,
            working_path,
            working_descriptor,
            working_identity,
            header.limits,
        )?;
        let authenticated = (|| -> Result<(), SourceDbStageError> {
            if working.stats()?.limits != header.limits {
                return Err(SourceDbStageError::KeptArchiveInvalid(
                    "the decoded stage's limits differ from the kept archive's",
                ));
            }
            if *working.sealed_logical_digest()?.as_bytes() != header.logical {
                return Err(SourceDbStageError::KeptArchiveInvalid(
                    "the decoded stage's rows differ from the kept archive's record",
                ));
            }
            Ok(())
        })();
        match authenticated {
            Ok(()) => Ok(working),
            Err(error) => {
                let _ = working.discard();
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub_pack::{ProjectionFixedNumericV2_2 as Fixed, ProjectionFloat64BitsV2_2 as Bits};
    use crate::source_db_projection::{
        SourceDbChargePhysicalV2_2, SourceDbChargingProcessPhysicalV2_2,
        SourceDbDrivePhysicalV2_2, SourceDbPositionPhysicalV2_2,
    };
    use crate::source_db_stage::SourceDbStageTable;

    /// A deterministic generator (xorshift64*), so both scales and every
    /// run see the same rows.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, bound: u64) -> i64 {
            i64::try_from(self.next() % bound).expect("small")
        }

        fn some<T>(&mut self, percent: u64, value: impl FnOnce(&mut Self) -> T) -> Option<T> {
            (self.next() % 100 < percent).then(|| value(self))
        }
    }

    const IDENTITY: KeptArchiveIdentity = KeptArchiveIdentity {
        vehicle_id: Uuid::from_u128(0x5eed),
        binding: [7; 32],
    };

    #[derive(Debug, Default, Clone, Copy)]
    struct Counts {
        drives: u64,
        positions: u64,
        processes: u64,
        charges: u64,
    }

    /// A sealed physical stage of `positions` synthetic positions with
    /// varied optional values and realistic widths: about one drive per 400
    /// positions (15% parked, without a drive), one charging process per
    /// 2,000 positions with 40 samples each, and the fixture roots.
    fn synthetic_stage(root: &Path, positions: u64) -> (SourceDbStage, Counts) {
        let limits = SourceDbStageLimits {
            max_rows: 50_000_000,
            max_stage_bytes: 8 * 1024 * 1024 * 1024,
            minimum_free_bytes: 0,
        };
        let mut stage =
            SourceDbStage::create_physical_v3(root.join("imports"), limits).expect("stage");
        crate::sync::physical_fragments::tests::seed_roots(&mut stage);
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut counts = Counts::default();
        let start_us = 600_000_000_000_000_i64;
        let mut page = Vec::new();
        let mut drive = 0_i32;
        for id in 1..=positions {
            let id_i32 = i32::try_from(id).expect("id");
            if id % 400 == 1 {
                drive += 1;
            }
            let parked = id % 400 > 340;
            let at = start_us + i64::from(id_i32) * 15_000_000;
            page.push((
                i64::from(id_i32),
                SourceDbPositionPhysicalV2_2 {
                    id: id_i32,
                    car_id: 1,
                    drive_id: (!parked).then_some(drive),
                    date_pg_us: at,
                    latitude_e6: Fixed::Finite(47_400_000 + rng.below(300_000)),
                    longitude_e6: Fixed::Finite(18_900_000 + rng.below(400_000)),
                    elevation: rng.some(90, |rng| i16::try_from(rng.below(900)).expect("small")),
                    speed: (!parked).then(|| i16::try_from(rng.below(130)).expect("small")),
                    power: Some(i16::try_from(rng.below(160) - 30).expect("small")),
                    odometer: Some(Bits::from_f64(12_000.0 + id as f64 * 0.137)),
                    ideal_battery_range_km_e2: Some(Fixed::Finite(20_000 + rng.below(30_000))),
                    est_battery_range_km_e2: rng.some(70, |rng| Fixed::Finite(18_000 + rng.below(30_000))),
                    rated_battery_range_km_e2: Some(Fixed::Finite(21_000 + rng.below(30_000))),
                    battery_level: Some(i16::try_from(20 + rng.below(80)).expect("small")),
                    usable_battery_level: rng.some(80, |rng| i16::try_from(19 + rng.below(80)).expect("small")),
                    battery_heater: rng.some(60, |_| false),
                    battery_heater_on: rng.some(60, |_| false),
                    battery_heater_no_power: rng.some(20, |_| false),
                    outside_temp_e1: rng.some(85, |rng| Fixed::Finite(rng.below(400) - 100)),
                    inside_temp_e1: rng.some(85, |rng| Fixed::Finite(150 + rng.below(150))),
                    fan_status: rng.some(50, |rng| i32::try_from(rng.below(8)).expect("small")),
                    driver_temp_setting_e1: rng.some(70, |rng| Fixed::Finite(200 + rng.below(30))),
                    passenger_temp_setting_e1: rng.some(70, |rng| Fixed::Finite(200 + rng.below(30))),
                    is_climate_on: rng.some(70, |rng| rng.next() % 2 == 0),
                    is_rear_defroster_on: rng.some(50, |_| false),
                    is_front_defroster_on: rng.some(50, |_| false),
                    tpms_pressure_fl_e1: rng.some(60, |rng| Fixed::Finite(28 + rng.below(4))),
                    tpms_pressure_fr_e1: rng.some(60, |rng| Fixed::Finite(28 + rng.below(4))),
                    tpms_pressure_rl_e1: rng.some(60, |rng| Fixed::Finite(28 + rng.below(4))),
                    tpms_pressure_rr_e1: rng.some(60, |rng| Fixed::Finite(28 + rng.below(4))),
                },
            ));
            if page.len() == 1_000 {
                counts.positions += page.len() as u64;
                stage
                    .insert_page(SourceDbStageTable::Positions, page.drain(..))
                    .expect("positions");
            }
        }
        counts.positions += page.len() as u64;
        stage
            .insert_page(SourceDbStageTable::Positions, page.drain(..))
            .expect("positions");
        let drives: Vec<_> = (1..=drive)
            .map(|id| {
                let first = i64::from(id - 1) * 400 + 1;
                (
                    i64::from(id),
                    SourceDbDrivePhysicalV2_2 {
                        id,
                        car_id: 1,
                        start_date_pg_us: start_us + first * 15_000_000,
                        end_date_pg_us: Some(start_us + (first + 340) * 15_000_000),
                        start_position_id: Some(i32::try_from(first).expect("id")),
                        end_position_id: Some(i32::try_from(first + 340).expect("id")),
                        start_address_id: None,
                        end_address_id: None,
                        start_geofence_id: None,
                        end_geofence_id: None,
                        outside_temp_avg_e1: rng.some(90, |rng| Fixed::Finite(rng.below(300))),
                        inside_temp_avg_e1: rng.some(90, |rng| Fixed::Finite(150 + rng.below(100))),
                        speed_max: Some(i16::try_from(60 + rng.below(90)).expect("small")),
                        power_max: Some(i16::try_from(100 + rng.below(150)).expect("small")),
                        power_min: Some(i16::try_from(-60 + rng.below(30)).expect("small")),
                        start_ideal_range_km_e2: Some(Fixed::Finite(30_000 + rng.below(10_000))),
                        end_ideal_range_km_e2: Some(Fixed::Finite(20_000 + rng.below(10_000))),
                        start_rated_range_km_e2: Some(Fixed::Finite(30_000 + rng.below(10_000))),
                        end_rated_range_km_e2: Some(Fixed::Finite(20_000 + rng.below(10_000))),
                        start_km: Some(Bits::from_f64(12_000.0 + first as f64 * 0.137)),
                        end_km: Some(Bits::from_f64(12_000.0 + (first + 340) as f64 * 0.137)),
                        distance: Some(Bits::from_f64(46.58)),
                        duration_min: Some(85),
                        ascent: rng.some(80, |rng| i16::try_from(rng.below(400)).expect("small")),
                        descent: rng.some(80, |rng| i16::try_from(rng.below(400)).expect("small")),
                    },
                )
            })
            .collect();
        counts.drives = drives.len() as u64;
        stage
            .insert_page(SourceDbStageTable::Drives, drives)
            .expect("drives");
        let processes = (positions / 2_000).max(1);
        let mut samples = Vec::new();
        let mut sample_id = 0_i32;
        for process in 1..=processes {
            let id = i32::try_from(process).expect("id");
            let position = i32::try_from((process - 1) * 2_000 + 380).expect("id").min(i32::try_from(positions).expect("id"));
            let at = start_us + i64::from(position) * 15_000_000;
            stage
                .insert(
                    SourceDbStageTable::ChargingProcesses,
                    i64::from(id),
                    &SourceDbChargingProcessPhysicalV2_2 {
                        id,
                        car_id: 1,
                        position_id: position,
                        address_id: None,
                        geofence_id: None,
                        start_date_pg_us: at,
                        end_date_pg_us: Some(at + 40 * 60_000_000),
                        charge_energy_added_e2: Some(Fixed::Finite(1_000 + rng.below(5_000))),
                        charge_energy_used_e2: rng.some(70, |rng| Fixed::Finite(1_100 + rng.below(5_000))),
                        start_ideal_range_km_e2: Some(Fixed::Finite(10_000 + rng.below(10_000))),
                        end_ideal_range_km_e2: Some(Fixed::Finite(30_000 + rng.below(10_000))),
                        start_rated_range_km_e2: Some(Fixed::Finite(10_000 + rng.below(10_000))),
                        end_rated_range_km_e2: Some(Fixed::Finite(30_000 + rng.below(10_000))),
                        start_battery_level: Some(i16::try_from(10 + rng.below(40)).expect("small")),
                        end_battery_level: Some(i16::try_from(60 + rng.below(40)).expect("small")),
                        duration_min: Some(40),
                        outside_temp_avg_e1: rng.some(90, |rng| Fixed::Finite(rng.below(300))),
                        cost_e2: rng.some(40, |rng| Fixed::Finite(rng.below(3_000))),
                    },
                )
                .expect("process");
            for sample in 0..40_i64 {
                sample_id += 1;
                samples.push((
                    i64::from(sample_id),
                    SourceDbChargePhysicalV2_2 {
                        id: sample_id,
                        charging_process_id: id,
                        date_pg_us: at + sample * 60_000_000,
                        battery_heater: rng.some(60, |_| false),
                        battery_heater_on: rng.some(60, |_| false),
                        battery_heater_no_power: rng.some(20, |_| false),
                        battery_level: Some(i16::try_from(10 + sample + rng.below(5)).expect("small")),
                        usable_battery_level: rng.some(80, |_| i16::try_from(10 + sample).expect("small")),
                        charge_energy_added_e2: Fixed::Finite(sample * 100 + rng.below(50)),
                        charger_actual_current: Some(i16::try_from(16 + rng.below(16)).expect("small")),
                        charger_phases: rng.some(80, |_| 3),
                        charger_pilot_current: Some(32),
                        charger_power: i16::try_from(11 + rng.below(5)).expect("small"),
                        charger_voltage: Some(i16::try_from(225 + rng.below(15)).expect("small")),
                        conn_charge_cable: rng.some(70, |_| "IEC".to_owned()),
                        fast_charger_present: Some(false),
                        fast_charger_brand: rng.some(10, |_| "<invalid>".to_owned()),
                        fast_charger_type: rng.some(70, |_| "ACSingleWireCAN".to_owned()),
                        ideal_battery_range_km_e2: Fixed::Finite(10_000 + sample * 500 + rng.below(100)),
                        rated_battery_range_km_e2: rng.some(80, |_| Fixed::Finite(10_000 + sample * 500)),
                        not_enough_power_to_heat: rng.some(30, |_| false),
                        outside_temp_e1: rng.some(85, |rng| Fixed::Finite(rng.below(300))),
                    },
                ));
            }
        }
        counts.processes = processes;
        counts.charges = samples.len() as u64;
        for chunk in samples.chunks(1_000) {
            stage
                .insert_page(SourceDbStageTable::Charges, chunk.iter().cloned())
                .expect("samples");
        }
        stage.seal().expect("seal");
        (stage, counts)
    }

    fn kept_dir(root: &Path) -> std::path::PathBuf {
        root.join("kept")
    }

    fn expected(receipt: &KeptArchiveReceipt) -> (String, String) {
        (receipt.logical.to_string(), receipt.digest.to_string())
    }

    fn record<'a>(receipt: &KeptArchiveReceipt, strings: &'a (String, String)) -> KeptArchiveExpected<'a> {
        KeptArchiveExpected {
            identity: IDENTITY,
            logical: &strings.0,
            digest: &strings.1,
            bytes: receipt.bytes,
            decoded_bytes: receipt.decoded_bytes,
        }
    }

    fn staging_files(root: &Path) -> Vec<String> {
        std::fs::read_dir(kept_dir(root).join(STAGING_DIRECTORY))
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn dump(stage: &SourceDbStage) -> Vec<(String, i64, String)> {
        let mut statement = stage
            .connection
            .prepare("SELECT table_name, source_id, row_json FROM stage_rows ORDER BY 1, 2")
            .expect("rows");
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("rows")
            .collect::<Result<_, _>>()
            .expect("rows")
    }

    #[test]
    fn kept_archive_round_trip_preserves_rows_digest_and_binding() {
        let root = crate::private_tempdir().expect("root");
        let (stage, _) = synthetic_stage(root.path(), 3_000);
        let receipt = stage
            .write_kept_archive(kept_dir(root.path()), "base.kept", IDENTITY)
            .expect("archive");
        assert_eq!(receipt.format, KEPT_ARCHIVE_FORMAT);
        assert_eq!(receipt.logical, stage.sealed_logical_digest().expect("digest"));
        assert!(receipt.bytes < receipt.raw_bytes, "{receipt:?}");
        assert!(receipt.decoded_bytes <= receipt.raw_bytes, "{receipt:?}");
        assert!(staging_files(root.path()).is_empty(), "the candidate goes");
        let strings = expected(&receipt);
        let mut decoded =
            SourceDbStage::decode_kept_archive(kept_dir(root.path()), "base.kept", record(&receipt, &strings))
                .expect("decoded");
        assert_eq!(decoded.sealed_content_digest().expect("checked"), receipt.logical);
        assert_eq!(dump(&decoded), dump(&stage), "every row, exactly");
        assert_eq!(decoded.stats().expect("stats"), stage.stats().expect("stats"));
        // The decoded stage is the one owned working stage, editable.
        decoded
            .edit_sealed::<SourceDbStageError>(|edit| edit.delete(SourceDbStageTable::Charges, 1))
            .expect("edited");
        assert_eq!(decoded.stats().expect("stats").row_count, stage.stats().expect("stats").row_count - 1);
        decoded.discard().expect("discard");
        // The archive is immutable: a second decode reads it again.
        SourceDbStage::decode_kept_archive(kept_dir(root.path()), "base.kept", record(&receipt, &strings))
            .expect("decoded again")
            .discard()
            .expect("discard");
    }

    #[test]
    fn kept_archive_refuses_corruption_and_records_that_differ() {
        let root = crate::private_tempdir().expect("root");
        let (stage, _) = synthetic_stage(root.path(), 1_000);
        let receipt = stage
            .write_kept_archive(kept_dir(root.path()), "base.kept", IDENTITY)
            .expect("archive");
        let path = kept_dir(root.path()).join("base.kept");
        let original = std::fs::read(&path).expect("archive");
        let strings = expected(&receipt);
        let decode = |expected: KeptArchiveExpected<'_>| {
            SourceDbStage::decode_kept_archive(kept_dir(root.path()), "base.kept", expected)
        };
        let refused = |result: Result<SourceDbStage, SourceDbStageError>, what: &str| {
            match result {
                Err(SourceDbStageError::KeptArchiveInvalid(_)) => {}
                other => panic!("{what}: {other:?}"),
            }
            assert!(staging_files(root.path()).is_empty(), "{what}: nothing is left");
        };
        // Records that differ from the archive.
        let other = (strings.0.clone(), Sha256Digest::of_bytes(b"other").to_string());
        refused(decode(record(&receipt, &other)), "digest");
        let other_logical = (Sha256Digest::of_bytes(b"other").to_string(), strings.1.clone());
        refused(decode(record(&receipt, &other_logical)), "logical");
        refused(
            decode(KeptArchiveExpected { bytes: receipt.bytes + 1, ..record(&receipt, &strings) }),
            "size",
        );
        refused(
            decode(KeptArchiveExpected {
                identity: KeptArchiveIdentity { binding: [8; 32], ..IDENTITY },
                ..record(&receipt, &strings)
            }),
            "binding",
        );
        refused(
            decode(KeptArchiveExpected {
                identity: KeptArchiveIdentity { vehicle_id: Uuid::from_u128(1), ..IDENTITY },
                ..record(&receipt, &strings)
            }),
            "vehicle",
        );
        // Damaged bytes: one flipped byte in the frame, then the header.
        for (at, what) in [(original.len() / 2, "frame"), (original.len() - 3, "checksum"), (2, "magic"), (9, "format")] {
            let mut damaged = original.clone();
            damaged[at] ^= 0x40;
            std::fs::write(&path, &damaged).expect("damage");
            refused(decode(record(&receipt, &strings)), what);
        }
        std::fs::write(&path, &original[..original.len() - 1]).expect("truncate");
        refused(
            decode(KeptArchiveExpected { bytes: receipt.bytes - 1, ..record(&receipt, &strings) }),
            "truncated",
        );
        std::fs::write(&path, &original).expect("restore");
        decode(record(&receipt, &strings)).expect("intact").discard().expect("discard");
    }

    /// Rewrites the archive's header with `edit` and returns the record the
    /// rewritten file now has.
    fn rewrite_header(
        path: &Path,
        receipt: &KeptArchiveReceipt,
        edit: impl FnOnce(&mut KeptArchiveHeader),
    ) -> (KeptArchiveReceipt, (String, String)) {
        let mut bytes = std::fs::read(path).expect("archive");
        let mut header = KeptArchiveHeader::decode(
            bytes[..HEADER_BYTES].try_into().expect("header"),
        )
        .expect("header");
        edit(&mut header);
        bytes[..HEADER_BYTES].copy_from_slice(&header.encode());
        std::fs::write(path, &bytes).expect("rewrite");
        let receipt = KeptArchiveReceipt {
            digest: Sha256Digest::of_bytes(&bytes),
            decoded_bytes: header.decoded_bytes,
            ..receipt.clone()
        };
        let strings = expected(&receipt);
        (receipt, strings)
    }

    #[test]
    fn kept_archive_decode_is_bounded_and_admitted() {
        let root = crate::private_tempdir().expect("root");
        let (stage, _) = synthetic_stage(root.path(), 1_000);
        let receipt = stage
            .write_kept_archive(kept_dir(root.path()), "base.kept", IDENTITY)
            .expect("archive");
        let path = kept_dir(root.path()).join("base.kept");
        let original = std::fs::read(&path).expect("archive");
        let decode = |receipt: &KeptArchiveReceipt, strings: &(String, String)| {
            SourceDbStage::decode_kept_archive(kept_dir(root.path()), "base.kept", record(receipt, strings))
        };
        // The frame decodes past the recorded size: stopped at the cap.
        let (short, strings) = rewrite_header(&path, &receipt, |header| header.decoded_bytes -= 4_096);
        match decode(&short, &strings) {
            Err(SourceDbStageError::KeptArchiveDecodedSizeExceeded { maximum }) => {
                assert_eq!(maximum, receipt.decoded_bytes - 4_096);
            }
            other => panic!("over the cap: {other:?}"),
        }
        assert!(staging_files(root.path()).is_empty());
        // The record claims more than the frame holds.
        std::fs::write(&path, &original).expect("restore");
        let (long, strings) = rewrite_header(&path, &receipt, |header| header.decoded_bytes += 4_096);
        assert!(matches!(decode(&long, &strings), Err(SourceDbStageError::KeptArchiveInvalid(_))));
        // A decoded size above the stage's own byte limit is refused unread.
        std::fs::write(&path, &original).expect("restore");
        let (huge, strings) = rewrite_header(&path, &receipt, |header| {
            header.decoded_bytes = header.limits.max_stage_bytes + 1;
        });
        assert!(matches!(
            decode(&huge, &strings),
            Err(SourceDbStageError::KeptArchiveDecodedSizeExceeded { .. })
        ));
        // Free space for the working stage is admitted before it is made.
        std::fs::write(&path, &original).expect("restore");
        let (unadmitted, strings) =
            rewrite_header(&path, &receipt, |header| header.limits.minimum_free_bytes = 1 << 60);
        assert!(matches!(
            decode(&unadmitted, &strings),
            Err(SourceDbStageError::InsufficientFreeSpace { .. })
        ));
        assert!(staging_files(root.path()).is_empty());
    }

    /// A newly created kept directory has its entry in the parent synced
    /// before the archive is built, so a catalogue record committed after
    /// the build cannot outlive the directory's name.
    #[test]
    fn kept_archive_syncs_the_parent_of_a_new_kept_directory() {
        let root = crate::private_tempdir().expect("root");
        let (stage, _) = synthetic_stage(root.path(), 50);
        crate::source_db_stage::PARENT_SYNCS.with(|syncs| syncs.borrow_mut().clear());
        stage
            .write_kept_archive(kept_dir(root.path()), "base.kept", IDENTITY)
            .expect("archive");
        let synced = crate::source_db_stage::PARENT_SYNCS.with(|syncs| syncs.borrow().clone());
        let parent = std::fs::canonicalize(root.path()).expect("root");
        assert!(
            synced.iter().any(|path| std::fs::canonicalize(path).ok().as_deref() == Some(parent.as_path())),
            "the parent of the new kept directory is synced: {synced:?}"
        );
        // An existing directory is not a new entry: nothing more to sync.
        crate::source_db_stage::PARENT_SYNCS.with(|syncs| syncs.borrow_mut().clear());
        stage
            .write_kept_archive(kept_dir(root.path()), "second.kept", IDENTITY)
            .expect("second archive");
        let again = crate::source_db_stage::PARENT_SYNCS.with(|syncs| syncs.borrow().clone());
        assert!(
            !again.iter().any(|path| std::fs::canonicalize(path).ok().as_deref() == Some(parent.as_path())),
            "{again:?}"
        );
    }

    #[test]
    fn kept_archive_build_crash_points_never_name_a_partial_archive() {
        for point in ["candidate", "partial"] {
            let root = crate::private_tempdir().expect("root");
            let (stage, _) = synthetic_stage(root.path(), 500);
            KEPT_ARCHIVE_CRASH_AT.with(|at| at.set(Some(point)));
            let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = stage.write_kept_archive(kept_dir(root.path()), "base.kept", IDENTITY);
            }));
            KEPT_ARCHIVE_CRASH_AT.with(|at| at.set(None));
            assert!(crashed.is_err());
            assert!(!kept_dir(root.path()).join("base.kept").exists(), "{point}");
            assert_eq!(staging_files(root.path()).len(), 1, "{point}: the candidate");
            assert_eq!(
                kept_dir(root.path()).join("base.kept.partial").exists(),
                point == "partial",
                "{point}"
            );
            // The next build over the leftovers succeeds.
            std::fs::remove_file(kept_dir(root.path()).join("base.kept.partial")).ok();
            stage
                .write_kept_archive(kept_dir(root.path()), "base.kept", IDENTITY)
                .expect("rebuilt");
        }
    }

    fn mb(bytes: u64) -> f64 {
        bytes as f64 / 1_000_000.0
    }

    /// DEC-250 size fixture: deterministic synthetic rows at two scales,
    /// counts and byte totals only, and the projected whole store. It
    /// reports `target missed` rather than failing when the projection is
    /// over 3.45 GB; it asserts the archive is smaller than the raw stage.
    #[test]
    fn storage_size_projection_reports_counts_bytes_and_the_store() {
        let mut scales = Vec::new();
        for positions in [10_000_u64, 100_000] {
            let root = crate::private_tempdir().expect("root");
            let (stage, counts) = synthetic_stage(root.path(), positions);
            let stats = stage.stats().expect("stats");
            let receipt = stage
                .write_kept_archive(kept_dir(root.path()), "base.kept", IDENTITY)
                .expect("archive");
            assert!(receipt.bytes < receipt.raw_bytes, "archive < raw: {receipt:?}");
            assert!(receipt.decoded_bytes <= receipt.raw_bytes);
            let strings = expected(&receipt);
            let decoded = SourceDbStage::decode_kept_archive(
                kept_dir(root.path()),
                "base.kept",
                record(&receipt, &strings),
            )
            .expect("decoded");
            assert_eq!(decoded.sealed_logical_digest().expect("digest"), receipt.logical);
            decoded.discard().expect("discard");
            println!(
                "storage_size_projection scale positions={} drives={} charging_processes={} charges={} rows={} payload_bytes={} raw_bytes={} vacuum_bytes={} archive_bytes={} peak_scratch_build_bytes={} decode_working_bytes={} ratio_raw_to_archive={:.2}",
                counts.positions,
                counts.drives,
                counts.processes,
                counts.charges,
                stats.row_count,
                stats.payload_bytes,
                receipt.raw_bytes,
                receipt.decoded_bytes,
                receipt.bytes,
                receipt.peak_scratch_bytes,
                receipt.decoded_bytes,
                receipt.raw_bytes as f64 / receipt.bytes as f64,
            );
            scales.push(receipt);
        }
        let (small, large) = (&scales[0], &scales[1]);
        let marginal = (large.bytes - small.bytes) as f64 / (large.raw_bytes - small.raw_bytes) as f64;
        let fixed = small.bytes as f64 - marginal * small.raw_bytes as f64;
        let vacuum_marginal =
            (large.decoded_bytes - small.decoded_bytes) as f64 / (large.raw_bytes - small.raw_bytes) as f64;
        // Supplied owner figures (GB, approximate): catalogue 2.1, packs
        // 0.47, one live raw stage 10 (the second is task 2's reclaim).
        let (catalogue, packs, raw_stage, target) = (2.1_f64, 0.47_f64, 10.0_f64, 3.45_f64);
        let archive_gb = (fixed / 1e9) + marginal * raw_stage;
        let store = catalogue + packs + archive_gb;
        let compacted = 1.35 + packs + archive_gb;
        println!(
            "storage_size_projection marginal_archive_per_raw_byte={marginal:.4} fixed_archive_bytes={fixed:.0} marginal_vacuum_per_raw_byte={vacuum_marginal:.4}"
        );
        println!(
            "storage_size_projection projected_archive_gb={archive_gb:.2} (budget 0.80, ceiling 0.88) source_json_gb=small wal=separate abandoned=0"
        );
        println!(
            "storage_size_projection projected_store_gb={store:.2} target_gb={target} {}",
            if store <= target { "target met" } else { "target missed" }
        );
        println!(
            "storage_size_projection projected_store_after_catalogue_saving_gb={compacted:.2} {}",
            if compacted <= target { "target met" } else { "target missed" }
        );
        println!(
            "storage_size_projection takeover_scenarios_gb retirement=2.57 retirement_with_pinned_base<=3.04 (design section 2)"
        );
        println!(
            "storage_size_projection projected_scratch_gb build_peak={:.2} decode_working={:.2} (not retained)",
            vacuum_marginal * raw_stage + archive_gb,
            vacuum_marginal * raw_stage,
        );
        println!(
            "storage_size_projection small_mb raw={:.2} vacuum={:.2} archive={:.2}; large_mb raw={:.2} vacuum={:.2} archive={:.2}",
            mb(small.raw_bytes), mb(small.decoded_bytes), mb(small.bytes),
            mb(large.raw_bytes), mb(large.decoded_bytes), mb(large.bytes),
        );
    }
}
