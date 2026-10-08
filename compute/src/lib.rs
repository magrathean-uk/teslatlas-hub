// SPDX-License-Identifier: Apache-2.0
//! Dependency-free deterministic compute kernels shared by Teslatlas products.

mod cleaner;
mod raster;

pub use cleaner::{
    CleanMapError, MAX_RAW_MAP_POSITIONS_PER_DRIVE, RawMapPosition,
    prepare_positions_for_tile_rendering_v1,
};

use std::collections::{BTreeMap, btree_map::Entry};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use raster::{LongRangeLodTier, RdpBudget, TileSegment, visit_drive_lod_tiers};

const POSITION_PAGE_FORMAT_VERSION: u32 = 1;
const TILE_PAYLOAD_FORMAT_VERSION: u32 = 1;
const TILE_DIRECTORY_BYTES: usize = 16;
const TILE_PAYLOAD_HEADER_BYTES: usize = 8;
const SEGMENT_BYTES: usize = 8;
const POSITION_PAGE_HEADER_BYTES: usize = 8;
const POSITION_DRIVE_HEADER_BYTES: usize = 8;
const POSITION_POINT_BYTES: usize = 8;
const MAX_POSITION_DRIVES_PER_PAGE: usize = 128;
const MAX_POSITION_PAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TILE_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_TILE_PAYLOAD_TILES: usize = 4096;

/// Maximum selected segments per output tile, matching the App Metal packer.
pub const MAX_CANONICAL_SEGMENTS_PER_TILE: usize = 200_000;
/// Maximum unique raw segments accepted across a generation block.
pub const MAX_CANONICAL_RAW_WEIGHT_ENTRIES: usize = MAX_TILE_PAYLOAD_BYTES / SEGMENT_BYTES;
/// Geometry identity required by the current Hub prepared-artefact profile.
pub const TILE_GEOMETRY_VERSION: &str = "raster-v9-rounded-tile-px";
/// Semantic identity of the cleaner/generator, independent of package version.
///
/// Persisted results must match this identity before reuse or current delivery.
/// Geometry and wire-format versions are separate representation contracts.
pub const ALGORITHM_VERSION: &str = "0.1.1";

const MAX_RAW_UNIQUE_SEGMENTS_PER_TILE: usize = MAX_TILE_PAYLOAD_BYTES / SEGMENT_BYTES;
const LONG_RANGE_LOD_TIERS: [LongRangeLodTier; 4] = [
    LongRangeLodTier::new(2, 4, 120.0, 3_000.0, 240.0),
    LongRangeLodTier::new(5, 7, 30.0, 800.0, 60.0),
    LongRangeLodTier::new(8, 10, 3.0, 50.0, 6.0),
    LongRangeLodTier::new(11, 13, 2.0, 20.0, 3.0),
];

/// One complete cleaned drive in latitude/longitude order.
///
/// `drive_id` is metadata: separate objects are counted and simplified separately,
/// even when their IDs match. It does not reconnect point fragments across pages.
#[derive(Clone, Debug, PartialEq)]
pub struct Drive {
    pub drive_id: i32,
    pub points: Vec<(f32, f32)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressStage {
    ParsingPositions,
    GeneratingTier,
    PackingPayload,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Progress {
    pub stage: ProgressStage,
    pub tier_index: usize,
    pub tier_count: usize,
    pub zoom_min: u32,
    pub zoom_max: u32,
    pub drives_completed: usize,
    pub drive_count: usize,
    pub work_units: u64,
}

pub type ProgressCallback<'a> = dyn Fn(Progress) -> Result<(), Error> + Send + Sync + 'a;
pub type DrivePageConsumer<'a> = dyn FnMut(&[Drive]) -> Result<(), Error> + 'a;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Diagnostics {
    pub visitor_invocations: u32,
    pub drive_count: i64,
    pub input_point_count: i64,
    pub produced_tile_count: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TileOutput {
    ReadyEmpty {
        drive_count: i64,
        diagnostics: Diagnostics,
    },
    ReadyData {
        drive_count: i64,
        tile_count: usize,
        payload: Vec<u8>,
        diagnostics: Diagnostics,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    Cancelled,
    InvalidData(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("cancelled"),
            Self::InvalidData(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TileKey {
    zoom: u32,
    tile_x: u32,
    tile_y: u32,
}

/// Signed ordering intentionally matches the App's `SegmentValue: Comparable`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SegmentKey {
    x1: i16,
    y1: i16,
    x2: i16,
    y2: i16,
}

type SegmentWeights = BTreeMap<SegmentKey, u64>;
type TierWeights = BTreeMap<TileKey, SegmentWeights>;
type TileShard = (u32, u32, u32, Vec<u8>);

struct TierAccumulator {
    tier: LongRangeLodTier,
    weights: TierWeights,
    drive_count: usize,
}

impl TierAccumulator {
    fn new(tier: LongRangeLodTier) -> Self {
        Self {
            tier,
            weights: BTreeMap::new(),
            drive_count: 0,
        }
    }
}

#[derive(Default)]
struct WeightBudget {
    used: usize,
    tile_count: usize,
}

impl WeightBudget {
    fn reserve(&mut self) -> Result<(), Error> {
        if self.used >= MAX_CANONICAL_RAW_WEIGHT_ENTRIES {
            return Err(Error::InvalidData(
                "canonical CPU tier exceeded the global raw-weight memory limit".into(),
            ));
        }
        self.used += 1;
        Ok(())
    }
}

/// Decode the App's bounded canonical cleaned-position page v1 format.
pub fn parse_position_page_v1(
    bytes: &[u8],
    cancelled: &AtomicBool,
    progress: Option<&ProgressCallback<'_>>,
) -> Result<Vec<Drive>, Error> {
    check_cancelled(cancelled)?;
    if bytes.len() > MAX_POSITION_PAGE_BYTES {
        return Err(Error::InvalidData(
            "canonical CPU position page exceeds the bounded page limit".into(),
        ));
    }
    let mut reader = Reader::new(bytes);
    let version = reader.read_u32()?;
    if version != POSITION_PAGE_FORMAT_VERSION {
        return Err(Error::InvalidData(format!(
            "unsupported canonical position page version {version}"
        )));
    }
    let drive_count = reader.read_u32()? as usize;
    if drive_count > MAX_POSITION_DRIVES_PER_PAGE {
        return Err(Error::InvalidData(format!(
            "canonical position page declared {drive_count} drives over the {MAX_POSITION_DRIVES_PER_PAGE} cap"
        )));
    }
    let work_units = AtomicU64::new(0);
    let mut drives = Vec::with_capacity(drive_count);
    for drive_index in 0..drive_count {
        checkpoint(
            cancelled,
            &work_units,
            progress,
            Progress {
                stage: ProgressStage::ParsingPositions,
                tier_index: 0,
                tier_count: LONG_RANGE_LOD_TIERS.len(),
                zoom_min: 2,
                zoom_max: 13,
                drives_completed: drive_index,
                drive_count,
                work_units: 0,
            },
        )?;
        let drive_id = reader.read_i32()?;
        let point_count = reader.read_u32()? as usize;
        let required = point_count
            .checked_mul(POSITION_POINT_BYTES)
            .ok_or_else(|| Error::InvalidData("canonical point bytes overflow".into()))?;
        if required > reader.remaining() {
            return Err(Error::InvalidData(
                "canonical position page is truncated".into(),
            ));
        }
        let mut points = Vec::with_capacity(point_count);
        for point_index in 0..point_count {
            if point_index.is_multiple_of(1_024) {
                checkpoint(
                    cancelled,
                    &work_units,
                    progress,
                    Progress {
                        stage: ProgressStage::ParsingPositions,
                        tier_index: 0,
                        tier_count: LONG_RANGE_LOD_TIERS.len(),
                        zoom_min: 2,
                        zoom_max: 13,
                        drives_completed: drive_index,
                        drive_count,
                        work_units: 0,
                    },
                )?;
            }
            points.push((reader.read_f32()?, reader.read_f32()?));
        }
        drives.push(Drive { drive_id, points });
    }
    if reader.remaining() != 0 {
        return Err(Error::InvalidData(
            "canonical position page contains trailing bytes".into(),
        ));
    }
    Ok(drives)
}

pub fn generate_tile_payload_v1(
    drives: &[Drive],
    cancelled: &AtomicBool,
    progress: Option<&ProgressCallback<'_>>,
) -> Result<TileOutput, Error> {
    generate_internal(Some(drives.len()), cancelled, progress, None, |consume| {
        consume(drives)
    })
}

/// Generate with a finite RDP interior-point distance-evaluation budget.
///
/// The budget is shared across all tiers and drives. Zero allows no RDP
/// evaluations. Exhaustion returns `InvalidData` before the next evaluation,
/// without partial output. This does not bound guard, raster or packing work.
pub fn generate_tile_payload_v1_with_rdp_limit(
    drives: &[Drive],
    cancelled: &AtomicBool,
    progress: Option<&ProgressCallback<'_>>,
    rdp_evaluation_limit: u64,
) -> Result<TileOutput, Error> {
    generate_internal(
        Some(drives.len()),
        cancelled,
        progress,
        Some(rdp_evaluation_limit),
        |consume| consume(drives),
    )
}

/// Generate from storage-owned bounded pages, consuming every page synchronously once.
///
/// Pages group complete `Drive` objects. Reordering or regrouping the same objects
/// preserves output bytes; splitting a drive's points into separate objects does
/// not. Each object must fit one page (at most 128 drives and an 8 MiB encoded
/// equivalent); an oversized object is rejected, not continued by matching IDs.
pub fn generate_tile_payload_v1_from_pages(
    cancelled: &AtomicBool,
    progress: Option<&ProgressCallback<'_>>,
    visit_pages: impl FnMut(&mut DrivePageConsumer<'_>) -> Result<(), Error>,
) -> Result<TileOutput, Error> {
    generate_internal(None, cancelled, progress, None, visit_pages)
}

/// Generate from bounded pages with one RDP evaluation budget for the entire call.
///
/// The limit also spans page boundaries. See `generate_tile_payload_v1_with_rdp_limit`
/// for exhaustion semantics and the work excluded from this budget.
/// Pages must group complete `Drive` objects, as for
/// `generate_tile_payload_v1_from_pages`; IDs do not reconnect fragments.
pub fn generate_tile_payload_v1_from_pages_with_rdp_limit(
    cancelled: &AtomicBool,
    progress: Option<&ProgressCallback<'_>>,
    rdp_evaluation_limit: u64,
    visit_pages: impl FnMut(&mut DrivePageConsumer<'_>) -> Result<(), Error>,
) -> Result<TileOutput, Error> {
    generate_internal(
        None,
        cancelled,
        progress,
        Some(rdp_evaluation_limit),
        visit_pages,
    )
}

fn generate_internal(
    expected_drive_count: Option<usize>,
    cancelled: &AtomicBool,
    progress: Option<&ProgressCallback<'_>>,
    rdp_evaluation_limit: Option<u64>,
    mut visit_pages: impl FnMut(&mut DrivePageConsumer<'_>) -> Result<(), Error>,
) -> Result<TileOutput, Error> {
    check_cancelled(cancelled)?;
    let work_units = AtomicU64::new(0);
    let mut diagnostics = Diagnostics {
        visitor_invocations: 1,
        ..Diagnostics::default()
    };
    let mut accumulators = LONG_RANGE_LOD_TIERS.map(TierAccumulator::new);
    let mut budget = WeightBudget::default();
    let mut rdp_budget = RdpBudget::new(rdp_evaluation_limit);
    let reported_drive_count = expected_drive_count.unwrap_or(0);
    let mut first_consumer_error: Option<Error> = None;
    let visitor_result = {
        let mut consume = |drives: &[Drive]| {
            if let Some(error) = &first_consumer_error {
                return Err(error.clone());
            }
            let result: Result<(), Error> = (|| {
                check_cancelled(cancelled)?;
                validate_typed_position_page(drives)?;
                for drive in drives {
                    check_cancelled(cancelled)?;
                    diagnostics.input_point_count = diagnostics
                        .input_point_count
                        .checked_add(i64::try_from(drive.points.len()).map_err(|_| {
                            Error::InvalidData("canonical input point count exceeds i64".into())
                        })?)
                        .ok_or_else(|| {
                            Error::InvalidData("canonical input point count overflow".into())
                        })?;
                    let completed = accumulators[0].drive_count;
                    let templates: [Progress; LONG_RANGE_LOD_TIERS.len()] =
                        std::array::from_fn(|tier_index| {
                            let tier = LONG_RANGE_LOD_TIERS[tier_index];
                            Progress {
                                stage: ProgressStage::GeneratingTier,
                                tier_index,
                                tier_count: LONG_RANGE_LOD_TIERS.len(),
                                zoom_min: tier.zoom_min,
                                zoom_max: tier.zoom_max,
                                drives_completed: completed,
                                drive_count: reported_drive_count,
                                work_units: 0,
                            }
                        });
                    let mut emitted_segments = 0usize;
                    visit_drive_lod_tiers(
                        &drive.points,
                        &LONG_RANGE_LOD_TIERS,
                        cancelled,
                        &mut rdp_budget,
                        |tier_index| {
                            checkpoint(cancelled, &work_units, progress, templates[tier_index])
                        },
                        |tier_index, segment| {
                            if let Some(segment) = segment {
                                if emitted_segments.is_multiple_of(1_024) {
                                    checkpoint(
                                        cancelled,
                                        &work_units,
                                        progress,
                                        templates[tier_index],
                                    )?;
                                }
                                emitted_segments = emitted_segments.wrapping_add(1);
                                merge_raw_segment(
                                    &mut accumulators[tier_index].weights,
                                    segment,
                                    &mut budget,
                                )?;
                            } else {
                                accumulators[tier_index].drive_count = accumulators[tier_index]
                                    .drive_count
                                    .checked_add(1)
                                    .ok_or_else(|| {
                                        Error::InvalidData("canonical drive count overflow".into())
                                    })?;
                            }
                            Ok(())
                        },
                    )?;
                }
                Ok(())
            })();
            if let Err(error) = &result {
                first_consumer_error = Some(error.clone());
            }
            result
        };
        visit_pages(&mut consume)
    };
    if let Some(error) = first_consumer_error {
        return Err(error);
    }
    visitor_result?;
    check_cancelled(cancelled)?;
    let observed = accumulators[0].drive_count;
    if accumulators.iter().any(|tier| tier.drive_count != observed) {
        return Err(Error::InvalidData(
            "canonical CPU LOD drive counts diverged".into(),
        ));
    }
    if expected_drive_count.is_some_and(|expected| expected != observed) {
        return Err(Error::InvalidData(
            "canonical CPU drive pages changed during generation".into(),
        ));
    }
    let mut shards = Vec::new();
    for (tier_index, accumulator) in accumulators.into_iter().enumerate() {
        let tier = accumulator.tier;
        shards.extend(finalize_tier(
            accumulator.weights,
            cancelled,
            &work_units,
            progress,
            Progress {
                stage: ProgressStage::PackingPayload,
                tier_index,
                tier_count: LONG_RANGE_LOD_TIERS.len(),
                zoom_min: tier.zoom_min,
                zoom_max: tier.zoom_max,
                drives_completed: observed,
                drive_count: observed,
                work_units: 0,
            },
        )?);
        ensure_payload_bound(&shards)?;
    }
    let drive_count = i64::try_from(observed)
        .map_err(|_| Error::InvalidData("canonical drive count exceeds i64".into()))?;
    diagnostics.drive_count = drive_count;
    diagnostics.produced_tile_count = i64::try_from(shards.len())
        .map_err(|_| Error::InvalidData("canonical output count exceeds i64".into()))?;
    if shards.is_empty() {
        return Ok(TileOutput::ReadyEmpty {
            drive_count,
            diagnostics,
        });
    }
    shards.sort_unstable_by_key(|(zoom, x, y, _)| (*zoom, *x, *y));
    let payload = encode_payload(&shards, cancelled)?;
    Ok(TileOutput::ReadyData {
        drive_count,
        tile_count: shards.len(),
        payload,
        diagnostics,
    })
}

fn validate_typed_position_page(drives: &[Drive]) -> Result<(), Error> {
    if drives.len() > MAX_POSITION_DRIVES_PER_PAGE {
        return Err(Error::InvalidData(format!(
            "canonical position page contains {} drives over the {} cap",
            drives.len(),
            MAX_POSITION_DRIVES_PER_PAGE
        )));
    }
    let encoded_bytes =
        drives
            .iter()
            .try_fold(POSITION_PAGE_HEADER_BYTES, |page_bytes, drive| {
                drive
                    .points
                    .len()
                    .checked_mul(POSITION_POINT_BYTES)
                    .and_then(|point_bytes| {
                        page_bytes
                            .checked_add(POSITION_DRIVE_HEADER_BYTES)
                            .and_then(|bytes| bytes.checked_add(point_bytes))
                    })
                    .ok_or_else(|| {
                        Error::InvalidData("canonical position page size overflow".into())
                    })
            })?;
    if encoded_bytes > MAX_POSITION_PAGE_BYTES {
        return Err(Error::InvalidData(
            "canonical position page exceeds the bounded page limit".into(),
        ));
    }
    Ok(())
}

fn merge_raw_segment(
    target: &mut TierWeights,
    segment: TileSegment,
    budget: &mut WeightBudget,
) -> Result<(), Error> {
    let coordinate = |value| {
        i16::try_from(value).map_err(|_| {
            Error::InvalidData("canonical CPU segment coordinate exceeds signed i16".into())
        })
    };
    let key = SegmentKey {
        x1: coordinate(segment.start.0)?,
        y1: coordinate(segment.start.1)?,
        x2: coordinate(segment.end.0)?,
        y2: coordinate(segment.end.1)?,
    };
    let weights = match target.entry(TileKey {
        zoom: segment.zoom,
        tile_x: segment.tile_x,
        tile_y: segment.tile_y,
    }) {
        Entry::Vacant(entry) => {
            // Tiers have disjoint zoom ranges. Every admitted tile keeps at least
            // one selected segment, so this shared count cannot fall at packing.
            // Reject before retaining tile 4,097 or processing more input pages.
            if budget.tile_count >= MAX_TILE_PAYLOAD_TILES {
                return Err(Error::InvalidData(format!(
                    "canonical CPU output contains {} tiles over the {} cap",
                    budget.tile_count + 1,
                    MAX_TILE_PAYLOAD_TILES
                )));
            }
            budget.reserve()?;
            budget.tile_count += 1;
            entry.insert(BTreeMap::from([(key, 1)]));
            return Ok(());
        }
        Entry::Occupied(entry) => entry.into_mut(),
    };
    let unique_count = weights.len();
    match weights.entry(key) {
        Entry::Vacant(entry) => {
            if unique_count >= MAX_RAW_UNIQUE_SEGMENTS_PER_TILE {
                return Err(Error::InvalidData(
                    "canonical CPU tile exceeded the bounded raw unique-segment limit".into(),
                ));
            }
            budget.reserve()?;
            entry.insert(1);
        }
        Entry::Occupied(mut entry) => {
            let weight = entry.get_mut();
            *weight = weight.checked_add(1).ok_or_else(|| {
                Error::InvalidData("canonical CPU segment weight overflow".into())
            })?;
        }
    }
    Ok(())
}

fn finalize_tier(
    weights: TierWeights,
    cancelled: &AtomicBool,
    work_units: &AtomicU64,
    progress: Option<&ProgressCallback<'_>>,
    template: Progress,
) -> Result<Vec<TileShard>, Error> {
    let mut shards = Vec::with_capacity(weights.len());
    for (index, (tile, segments)) in weights.into_iter().enumerate() {
        if index.is_multiple_of(16) {
            checkpoint(cancelled, work_units, progress, template)?;
        }
        let selected = budgeted_segments(segments, cancelled)?;
        if selected.is_empty() {
            continue;
        }
        let mut blob = Vec::with_capacity(selected.len() * SEGMENT_BYTES);
        for segment in selected {
            segment.append_bytes(&mut blob);
        }
        shards.push((tile.zoom, tile.tile_x, tile.tile_y, blob));
    }
    Ok(shards)
}

fn budgeted_segments(
    weights: SegmentWeights,
    cancelled: &AtomicBool,
) -> Result<Vec<SegmentKey>, Error> {
    check_cancelled(cancelled)?;
    if weights.len() <= MAX_CANONICAL_SEGMENTS_PER_TILE {
        return Ok(weights.into_keys().collect());
    }
    let mut weighted = weights.into_iter().collect::<Vec<_>>();
    weighted.sort_unstable_by(|(left, left_weight), (right, right_weight)| {
        right_weight.cmp(left_weight).then_with(|| left.cmp(right))
    });
    weighted.truncate(MAX_CANONICAL_SEGMENTS_PER_TILE);
    check_cancelled(cancelled)?;
    let mut selected = weighted
        .into_iter()
        .map(|(segment, _)| segment)
        .collect::<Vec<_>>();
    selected.sort_unstable();
    Ok(selected)
}

fn ensure_payload_bound(shards: &[TileShard]) -> Result<(), Error> {
    if shards.len() > MAX_TILE_PAYLOAD_TILES {
        return Err(Error::InvalidData(format!(
            "canonical CPU output contains {} tiles over the {} cap",
            shards.len(),
            MAX_TILE_PAYLOAD_TILES
        )));
    }
    let total = shards
        .iter()
        .try_fold(TILE_PAYLOAD_HEADER_BYTES, |total, (_, _, _, blob)| {
            total
                .checked_add(TILE_DIRECTORY_BYTES)
                .and_then(|value| value.checked_add(blob.len()))
                .ok_or_else(|| {
                    Error::InvalidData("canonical CPU tile payload size overflow".into())
                })
        })?;
    if total > MAX_TILE_PAYLOAD_BYTES {
        return Err(Error::InvalidData(
            "canonical CPU tile payload exceeds the bounded publication limit".into(),
        ));
    }
    Ok(())
}

fn encode_payload(shards: &[TileShard], cancelled: &AtomicBool) -> Result<Vec<u8>, Error> {
    ensure_payload_bound(shards)?;
    let total = shards
        .iter()
        .fold(TILE_PAYLOAD_HEADER_BYTES, |total, (_, _, _, blob)| {
            total + TILE_DIRECTORY_BYTES + blob.len()
        });
    let mut payload = Vec::with_capacity(total);
    push_u32(&mut payload, TILE_PAYLOAD_FORMAT_VERSION);
    push_u32(
        &mut payload,
        u32::try_from(shards.len())
            .map_err(|_| Error::InvalidData("canonical tile count overflow".into()))?,
    );
    for (index, (zoom, tile_x, tile_y, blob)) in shards.iter().enumerate() {
        if index.is_multiple_of(64) {
            check_cancelled(cancelled)?;
        }
        if blob.is_empty() || !blob.len().is_multiple_of(SEGMENT_BYTES) {
            return Err(Error::InvalidData(
                "canonical CPU tile blob is empty or misaligned".into(),
            ));
        }
        push_u32(&mut payload, *zoom);
        push_u32(&mut payload, *tile_x);
        push_u32(&mut payload, *tile_y);
        push_u32(
            &mut payload,
            u32::try_from(blob.len())
                .map_err(|_| Error::InvalidData("canonical tile blob overflow".into()))?,
        );
        payload.extend_from_slice(blob);
    }
    Ok(payload)
}

fn checkpoint(
    cancelled: &AtomicBool,
    work_units: &AtomicU64,
    progress: Option<&ProgressCallback<'_>>,
    mut value: Progress,
) -> Result<(), Error> {
    check_cancelled(cancelled)?;
    value.work_units = work_units.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    if let Some(progress) = progress {
        progress(value)?;
    }
    check_cancelled(cancelled)
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::SeqCst) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

impl SegmentKey {
    fn append_bytes(self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.x1.to_le_bytes());
        output.extend_from_slice(&self.y1.to_le_bytes());
        output.extend_from_slice(&self.x2.to_le_bytes());
        output.extend_from_slice(&self.y2.to_le_bytes());
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }
    fn read_u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            self.read_exact(4)?.try_into().expect("four bytes"),
        ))
    }
    fn read_i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(
            self.read_exact(4)?.try_into().expect("four bytes"),
        ))
    }
    fn read_f32(&mut self) -> Result<f32, Error> {
        Ok(f32::from_bits(self.read_u32()?))
    }
    fn read_exact(&mut self, count: usize) -> Result<&'a [u8], Error> {
        let end = self
            .offset
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| Error::InvalidData("canonical position page is truncated".into()))?;
        let result = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment() -> TileSegment {
        TileSegment {
            zoom: 13,
            tile_x: 100,
            tile_y: 200,
            start: (i32::from(i16::MIN), i32::from(i16::MAX)),
            end: (513, -1),
        }
    }

    #[test]
    fn streamed_duplicates_preserve_weights_direction_and_unique_budget() {
        let mut weights = TierWeights::new();
        let mut budget = WeightBudget::default();
        for _ in 0..8_192 {
            merge_raw_segment(&mut weights, segment(), &mut budget).unwrap();
        }
        assert_eq!(budget.used, 1);
        assert_eq!(weights.len(), 1);
        let tile = weights.values().next().unwrap();
        assert_eq!(tile.len(), 1);
        assert_eq!(tile.values().next(), Some(&8_192));

        let mut reversed = segment();
        std::mem::swap(&mut reversed.start, &mut reversed.end);
        merge_raw_segment(&mut weights, reversed, &mut budget).unwrap();
        assert_eq!(budget.used, 2);
        assert_eq!(weights.values().next().unwrap().len(), 2);

        let mut other_tile = segment();
        other_tile.tile_x += 1;
        merge_raw_segment(&mut weights, other_tile, &mut budget).unwrap();
        assert_eq!(budget.used, 3);
        assert_eq!(weights.len(), 2);
    }

    #[test]
    fn full_unique_budget_allows_duplicates_but_rejects_new_entries() {
        let mut weights = TierWeights::new();
        let mut budget = WeightBudget {
            used: MAX_CANONICAL_RAW_WEIGHT_ENTRIES - 1,
            ..WeightBudget::default()
        };
        merge_raw_segment(&mut weights, segment(), &mut budget).unwrap();
        assert_eq!(budget.used, MAX_CANONICAL_RAW_WEIGHT_ENTRIES);
        merge_raw_segment(&mut weights, segment(), &mut budget).unwrap();

        let mut distinct = segment();
        distinct.end.0 += 1;
        assert!(matches!(
            merge_raw_segment(&mut weights, distinct, &mut budget),
            Err(Error::InvalidData(message)) if message.contains("global raw-weight")
        ));
        assert_eq!(budget.used, MAX_CANONICAL_RAW_WEIGHT_ENTRIES);
        let tile = weights.values().next().unwrap();
        assert_eq!(tile.len(), 1);
        assert_eq!(tile.values().next(), Some(&2));
    }

    #[test]
    fn out_of_range_coordinates_are_rejected_before_accumulation() {
        for index in 0..4 {
            for outside in [i32::from(i16::MIN) - 1, i32::from(i16::MAX) + 1] {
                let mut raw = segment();
                match index {
                    0 => raw.start.0 = outside,
                    1 => raw.start.1 = outside,
                    2 => raw.end.0 = outside,
                    _ => raw.end.1 = outside,
                }
                let mut weights = TierWeights::new();
                let mut budget = WeightBudget::default();
                assert!(matches!(
                    merge_raw_segment(&mut weights, raw, &mut budget),
                    Err(Error::InvalidData(message)) if message.contains("signed i16")
                ));
                assert!(weights.is_empty());
                assert_eq!(budget.used, 0);
            }
        }
    }

    fn ordered_key(index: usize) -> SegmentKey {
        SegmentKey {
            x1: i16::MIN + i16::try_from(index / 65_536).unwrap(),
            y1: i16::try_from(i32::try_from(index % 65_536).unwrap() - 32_768).unwrap(),
            x2: 1,
            y2: 2,
        }
    }

    #[test]
    fn overfull_tile_selects_heaviest_then_signed_lexicographic_ties() {
        let mut weights = SegmentWeights::new();
        for index in 0..=MAX_CANONICAL_SEGMENTS_PER_TILE {
            weights.insert(ordered_key(index), 1);
        }
        let promoted = ordered_key(MAX_CANONICAL_SEGMENTS_PER_TILE);
        weights.insert(promoted, 2);
        let mut sorted_keys = weights.keys().copied().collect::<Vec<_>>();
        sorted_keys.retain(|key| *key != promoted);
        let omitted = sorted_keys.pop().unwrap();
        sorted_keys.push(promoted);
        sorted_keys.sort_unstable();

        let selected = budgeted_segments(weights, &AtomicBool::new(false)).unwrap();
        assert_eq!(selected.len(), MAX_CANONICAL_SEGMENTS_PER_TILE);
        assert_eq!(selected, sorted_keys);
        assert!(selected.contains(&promoted));
        assert!(!selected.contains(&omitted));
        assert!(selected.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
