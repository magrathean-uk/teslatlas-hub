// SPDX-License-Identifier: AGPL-3.0-only

//! Honest source database-parity accounting.
//!
//! The selected telemetry projection is not a byte-for-byte source database backup.
//! This module makes the reviewed preservation and exclusion boundary
//! machine-readable and fail-closed.

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    protocol::Sha256Digest,
    source_db_projection::{SourceDbDrive, SourceDbState},
};

pub const SOURCE_DB_PARITY_LEDGER_VERSION: u16 = 4;
pub const SOURCE_DB_PARITY_REVIEWED_DENOMINATOR: u16 = 17;

/// What survives for one reviewed source field or value domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceDbFactDisposition {
    /// The exact fact is represented by the current Hub projection pack.
    PreservedInPack,
    /// The value is not in THP1, but a typed capture binds it to duplicate
    /// detection so a source change cannot be reported as unchanged.
    FingerprintOnly,
    /// The value is deliberately outside the product boundary.
    DeliberatelyExcluded,
    /// The current adapter drops, does not read, or rejects the value.
    UnsupportedOrLost,
}

/// Where the reviewed fact reaches before THP1 projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceDbCaptureStatus {
    CapturedTyped,
    SelectedButDiscardedByDecoder,
    NotRead,
    CapturedButUnsupportedValuesFailClosed,
}

/// Whether a successful publication's persisted source fingerprint covers the
/// reviewed fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceDbFingerprintStatus {
    Bound,
    NotBound,
    DeliberatelyNotHashed,
    NoPublishedDigestForUnsupportedValues,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDbParityEntry {
    pub source_relation: &'static str,
    pub source_field_or_domain: &'static str,
    pub disposition: SourceDbFactDisposition,
    pub capture: SourceDbCaptureStatus,
    pub fingerprint: SourceDbFingerprintStatus,
    pub current_app_impact: &'static str,
}

const fn preserved(
    source_relation: &'static str,
    source_field_or_domain: &'static str,
    current_app_impact: &'static str,
) -> SourceDbParityEntry {
    SourceDbParityEntry {
        source_relation,
        source_field_or_domain,
        disposition: SourceDbFactDisposition::PreservedInPack,
        capture: SourceDbCaptureStatus::CapturedTyped,
        fingerprint: SourceDbFingerprintStatus::Bound,
        current_app_impact,
    }
}

/// Exact field/value-domain denominator requested by the 2026-08-08 parity
/// review, updated for the schema-2.2 physical import. This is not a
/// denominator for every column in source database.
pub const SOURCE_DB_PARITY_ENTRIES: [SourceDbParityEntry; 17] = [
    preserved("cars", "display_priority", "exact schema-2.2 source value"),
    preserved("cars", "inserted_at", "exact schema-2.2 source timestamp"),
    preserved("cars", "updated_at", "exact schema-2.2 source timestamp"),
    preserved(
        "drives",
        "start_km",
        "exact IEEE-754 source bits in the schema-2.2 drive pack",
    ),
    preserved(
        "drives",
        "end_km",
        "exact IEEE-754 source bits in the schema-2.2 drive pack",
    ),
    preserved("settings", "unit_of_length", "exact schema-2.2 enum"),
    preserved("settings", "unit_of_temperature", "exact schema-2.2 enum"),
    preserved("settings", "unit_of_pressure", "exact schema-2.2 enum"),
    preserved("settings", "preferred_range", "exact schema-2.2 enum"),
    preserved("settings", "base_url", "opaque nullable source text"),
    preserved("settings", "grafana_url", "opaque nullable source text"),
    preserved("settings", "language", "exact schema-2.2 source text"),
    preserved("settings", "theme_mode", "exact schema-2.2 source text"),
    preserved(
        "settings",
        "inserted_at",
        "exact schema-2.2 source timestamp",
    ),
    preserved(
        "settings",
        "updated_at",
        "exact schema-2.2 source timestamp",
    ),
    SourceDbParityEntry {
        source_relation: "addresses",
        source_field_or_domain: "raw",
        disposition: SourceDbFactDisposition::DeliberatelyExcluded,
        capture: SourceDbCaptureStatus::NotRead,
        fingerprint: SourceDbFingerprintStatus::DeliberatelyNotHashed,
        current_app_impact: "deliberately excluded because it is not app-visible and may contain unrelated provider data",
    },
    preserved(
        "states",
        "state enum (online/offline/asleep)",
        "exact reviewed source database v4.4-compatible enum domain",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDbParityCounts {
    pub reviewed_field_or_value_domains: u16,
    pub preserved_in_pack: u16,
    pub fingerprint_only: u16,
    pub deliberately_excluded: u16,
    pub unsupported_or_lost: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDbParityReport {
    pub ledger_version: u16,
    pub scope: &'static str,
    pub thp1_schema: &'static str,
    pub schema_upgrade_required_for_remaining_fields: &'static str,
    #[serde(rename = "allTeslamateFieldsPreserved")]
    pub all_source_fields_preserved: bool,
    pub counts: SourceDbParityCounts,
    pub entries: &'static [SourceDbParityEntry],
}

impl SourceDbParityReport {
    pub const fn current() -> Self {
        Self {
            ledger_version: SOURCE_DB_PARITY_LEDGER_VERSION,
            scope: "reviewed-source-facts",
            thp1_schema: "2.2",
            schema_upgrade_required_for_remaining_fields: "not-applicable-addresses-raw-deliberately-excluded",
            all_source_fields_preserved: false,
            counts: SourceDbParityCounts {
                reviewed_field_or_value_domains: SOURCE_DB_PARITY_REVIEWED_DENOMINATOR,
                preserved_in_pack: 16,
                fingerprint_only: 0,
                deliberately_excluded: 1,
                unsupported_or_lost: 0,
            },
            entries: &SOURCE_DB_PARITY_ENTRIES,
        }
    }

    /// A caller asking for all source database fields can never accidentally treat
    /// the selected THP1 projection as full parity.
    pub fn require_all_source_db_fields(self) -> Result<(), SourceDbFullParityUnavailable> {
        if self.all_source_fields_preserved {
            Ok(())
        } else {
            Err(SourceDbFullParityUnavailable {
                reviewed_denominator: self.counts.reviewed_field_or_value_domains,
                schema_upgrade_required: self.schema_upgrade_required_for_remaining_fields,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "all source database fields are not preserved: {reviewed_denominator} reviewed field/value domains remain bounded by the parity ledger; remaining schema upgrade: {schema_upgrade_required}"
)]
pub struct SourceDbFullParityUnavailable {
    pub reviewed_denominator: u16,
    pub schema_upgrade_required: &'static str,
}

const EVIDENCE_DOMAIN: &[u8] = crate::runtime::legacy_names::SOURCE_EVIDENCE_V1.as_bytes();
const DRIVE_EVIDENCE_DOMAIN: &[u8] =
    crate::runtime::legacy_names::DRIVE_ODOMETER_EVIDENCE_V1.as_bytes();
const STATE_EVIDENCE_DOMAIN: &[u8] =
    crate::runtime::legacy_names::STATE_TEXT_EVIDENCE_V1.as_bytes();

/// Bounded, fixed-order evidence for already-decoded, non-secret facts that do
/// not fit THP1 schema 2.1. Each source kind owns an independent ordered stream
/// and finalization combines kinds in a fixed order, so capture-lane scheduling
/// cannot affect the result.
#[derive(Debug)]
pub(crate) struct SourceDbEvidenceFingerprint {
    drives: EvidenceStream,
    states: EvidenceStream,
}

impl SourceDbEvidenceFingerprint {
    pub(crate) fn new() -> Self {
        Self {
            drives: EvidenceStream::new(DRIVE_EVIDENCE_DOMAIN),
            states: EvidenceStream::new(STATE_EVIDENCE_DOMAIN),
        }
    }

    pub(crate) fn record_drive(
        &mut self,
        drive: &SourceDbDrive,
    ) -> Result<(), SourceDbEvidenceError> {
        self.record_drive_odometer(drive.id, drive.start_km, drive.end_km)
    }

    fn record_drive_odometer(
        &mut self,
        source_id: i64,
        start_km: Option<f64>,
        end_km: Option<f64>,
    ) -> Result<(), SourceDbEvidenceError> {
        self.drives.begin_record("drives", source_id)?;
        record_optional_f64(&mut self.drives.digest, start_km);
        record_optional_f64(&mut self.drives.digest, end_km);
        Ok(())
    }

    pub(crate) fn record_state(
        &mut self,
        state: &SourceDbState,
    ) -> Result<(), SourceDbEvidenceError> {
        self.record_state_text(state.id, &state.state)
    }

    fn record_state_text(
        &mut self,
        source_id: i64,
        state: &str,
    ) -> Result<(), SourceDbEvidenceError> {
        self.states.begin_record("states", source_id)?;
        let length = u64::try_from(state.len())
            .map_err(|_| SourceDbEvidenceError::StateTextTooLarge { source_id })?;
        self.states.digest.update(length.to_be_bytes());
        self.states.digest.update(state.as_bytes());
        Ok(())
    }

    pub(crate) fn finish(self) -> Sha256Digest {
        let mut digest = Sha256::new();
        digest.update(EVIDENCE_DOMAIN);
        finish_stream(&mut digest, 1, self.drives);
        finish_stream(&mut digest, 2, self.states);
        Sha256Digest::from_bytes(digest.finalize().into())
    }
}

struct EvidenceStream {
    digest: Sha256,
    last_source_id: Option<i64>,
    rows: u64,
}

impl std::fmt::Debug for EvidenceStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EvidenceStream")
            .field("last_source_id", &self.last_source_id)
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

impl EvidenceStream {
    fn new(domain: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update(domain);
        Self {
            digest,
            last_source_id: None,
            rows: 0,
        }
    }

    fn begin_record(
        &mut self,
        relation: &'static str,
        source_id: i64,
    ) -> Result<(), SourceDbEvidenceError> {
        if source_id <= 0 || self.last_source_id.is_some_and(|last| source_id <= last) {
            return Err(SourceDbEvidenceError::NonIncreasingSourceId {
                relation,
                previous: self.last_source_id,
                current: source_id,
            });
        }
        self.rows = self
            .rows
            .checked_add(1)
            .ok_or(SourceDbEvidenceError::RowCountOverflow { relation })?;
        self.last_source_id = Some(source_id);
        self.digest.update(source_id.to_be_bytes());
        Ok(())
    }
}

fn record_optional_f64(digest: &mut Sha256, value: Option<f64>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value.to_bits().to_be_bytes());
        }
        None => digest.update([0]),
    }
}

fn finish_stream(digest: &mut Sha256, kind: u8, stream: EvidenceStream) {
    digest.update([kind]);
    digest.update(stream.rows.to_be_bytes());
    digest.update(stream.digest.finalize());
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SourceDbEvidenceError {
    #[error(
        "Source database {relation} source evidence IDs must be positive and strictly increasing (previous {previous:?}, current {current})"
    )]
    NonIncreasingSourceId {
        relation: &'static str,
        previous: Option<i64>,
        current: i64,
    },
    #[error("Source database {relation} source evidence row count overflowed")]
    RowCountOverflow { relation: &'static str },
    #[error("Source database state {source_id} text is too large to fingerprint")]
    StateTextTooLarge { source_id: i64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_loss_ledger_has_an_exact_fail_closed_denominator() {
        let report = SourceDbParityReport::current();
        assert_eq!(report.entries.len(), 17);
        assert_eq!(report.counts.reviewed_field_or_value_domains, 17);
        assert_eq!(report.counts.preserved_in_pack, 16);
        assert_eq!(report.counts.fingerprint_only, 0);
        assert_eq!(report.counts.deliberately_excluded, 1);
        assert_eq!(report.counts.unsupported_or_lost, 0);
        assert!(!report.all_source_fields_preserved);
        assert_eq!(
            report.require_all_source_db_fields(),
            Err(SourceDbFullParityUnavailable {
                reviewed_denominator: 17,
                schema_upgrade_required: "not-applicable-addresses-raw-deliberately-excluded",
            })
        );

        let disposition_total = report.entries.iter().fold([0_u16; 4], |mut counts, entry| {
            let index = match entry.disposition {
                SourceDbFactDisposition::PreservedInPack => 0,
                SourceDbFactDisposition::FingerprintOnly => 1,
                SourceDbFactDisposition::DeliberatelyExcluded => 2,
                SourceDbFactDisposition::UnsupportedOrLost => 3,
            };
            counts[index] += 1;
            counts
        });
        assert_eq!(disposition_total, [16, 0, 1, 0]);

        let json = serde_json::to_value(report).expect("serialize public parity report");
        assert_eq!(
            json[crate::runtime::legacy_names::PARITY_PRESERVED_KEY],
            false
        );
        assert_eq!(
            json["schemaUpgradeRequiredForRemainingFields"],
            "not-applicable-addresses-raw-deliberately-excluded"
        );
        assert_eq!(json["counts"]["preservedInPack"], 16);
    }

    #[test]
    fn evidence_digest_binds_each_drive_endpoint_and_state_text() {
        fn digest(start_km: Option<f64>, end_km: Option<f64>, state: &str) -> Sha256Digest {
            let mut evidence = SourceDbEvidenceFingerprint::new();
            evidence
                .record_drive(&SourceDbDrive {
                    id: 10,
                    car_id: 1,
                    start_date_ms: 1_700_000_000_000,
                    end_date_ms: Some(1_700_000_060_000),
                    start_position_id: None,
                    end_position_id: None,
                    start_address_id: None,
                    end_address_id: None,
                    start_geofence_id: None,
                    end_geofence_id: None,
                    outside_temp_avg: None,
                    inside_temp_avg: None,
                    speed_max: None,
                    power_max: None,
                    power_min: None,
                    start_ideal_range_km: None,
                    end_ideal_range_km: None,
                    start_rated_range_km: None,
                    end_rated_range_km: None,
                    start_km,
                    end_km,
                    distance_km: None,
                    duration_min: None,
                    ascent: None,
                    descent: None,
                })
                .unwrap();
            evidence
                .record_state(&SourceDbState {
                    id: 20,
                    car_id: 1,
                    state: state.to_owned(),
                    start_date_ms: 1_700_000_000_000,
                    end_date_ms: None,
                })
                .unwrap();
            evidence.finish()
        }

        let baseline = digest(Some(10.0), Some(11.0), "online");
        assert_ne!(baseline, digest(Some(10.5), Some(11.0), "online"));
        assert_ne!(baseline, digest(Some(10.0), Some(11.5), "online"));
        assert_ne!(baseline, digest(Some(10.0), Some(11.0), "asleep"));
    }

    #[test]
    fn evidence_kind_finalization_is_stable_across_lane_scheduling() {
        let mut drive_then_state = SourceDbEvidenceFingerprint::new();
        drive_then_state
            .record_drive_odometer(10, Some(10.0), Some(11.0))
            .unwrap();
        drive_then_state.record_state_text(20, "online").unwrap();

        let mut state_then_drive = SourceDbEvidenceFingerprint::new();
        state_then_drive.record_state_text(20, "online").unwrap();
        state_then_drive
            .record_drive_odometer(10, Some(10.0), Some(11.0))
            .unwrap();

        assert_eq!(drive_then_state.finish(), state_then_drive.finish());
    }

    #[test]
    fn evidence_rejects_non_canonical_source_order() {
        let mut evidence = SourceDbEvidenceFingerprint::new();
        evidence
            .record_drive_odometer(10, Some(10.0), Some(11.0))
            .unwrap();
        assert!(matches!(
            evidence.record_drive_odometer(10, Some(10.0), Some(11.0)),
            Err(SourceDbEvidenceError::NonIncreasingSourceId {
                relation: "drives",
                previous: Some(10),
                current: 10,
            })
        ));
    }
}

#[cfg(test)]
mod n1_src_import_source_db_parity_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free, example};

    impl ErrorExample for SourceDbEvidenceError {
        fn error_example() -> Self {
            SourceDbEvidenceError::RowCountOverflow {
                relation: example(),
            }
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_source_db_evidence_error(error: SourceDbEvidenceError) {
            match &error {
                SourceDbEvidenceError::NonIncreasingSourceId { .. } => (),
                SourceDbEvidenceError::RowCountOverflow { .. } => (),
                SourceDbEvidenceError::StateTextTooLarge { .. } => (),
            }
            assert_name_free(&error);
        }
        check_source_db_evidence_error(SourceDbEvidenceError::NonIncreasingSourceId {
            relation: example(),
            previous: example(),
            current: example(),
        });
        check_source_db_evidence_error(SourceDbEvidenceError::RowCountOverflow {
            relation: example(),
        });
        check_source_db_evidence_error(SourceDbEvidenceError::StateTextTooLarge {
            source_id: example(),
        });
    }
}
