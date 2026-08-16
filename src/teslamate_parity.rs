//! Honest TeslaMate source-parity accounting.
//!
//! THP1 schema 2.1 is a selected telemetry projection, not a byte-for-byte
//! TeslaMate backup.  This module makes the currently reviewed loss boundary
//! machine-readable and fail-closed.  It also binds the source facts that are
//! already decoded but intentionally remain outside the phone pack to the
//! persisted source fingerprint.

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    protocol::Sha256Digest,
    teslamate_projection::{TeslaMateDrive, TeslaMateState},
};

pub const TESLAMATE_SOURCE_PARITY_LEDGER_VERSION: u16 = 1;
pub const TESLAMATE_SOURCE_PARITY_REVIEWED_DENOMINATOR: u16 = 17;

/// What survives for one reviewed source field or value domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TeslaMateSourceFactDisposition {
    /// The exact fact is represented by THP1 and accepted by the app.
    PreservedInPack,
    /// The value is not in THP1, but a typed capture binds it to duplicate
    /// detection so a source change cannot be reported as unchanged.
    FingerprintOnly,
    /// The value is proposed for sensitive exclusion, but remains blocked
    /// until an explicit future approval path exists.
    ProposedSensitiveExclusionBlocked,
    /// The current adapter drops, does not read, or rejects the value.
    UnsupportedOrLost,
}

/// Where the reviewed fact reaches before THP1 projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TeslaMateSourceCaptureStatus {
    CapturedTyped,
    SelectedButDiscardedByDecoder,
    NotRead,
    CapturedButUnsupportedValuesFailClosed,
}

/// Whether a successful publication's persisted source fingerprint covers the
/// reviewed fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TeslaMateSourceFingerprintStatus {
    Bound,
    NotBound,
    DeliberatelyNotHashed,
    NoPublishedDigestForUnsupportedValues,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeslaMateSourceParityEntry {
    pub source_relation: &'static str,
    pub source_field_or_domain: &'static str,
    pub disposition: TeslaMateSourceFactDisposition,
    pub capture: TeslaMateSourceCaptureStatus,
    pub fingerprint: TeslaMateSourceFingerprintStatus,
    pub current_app_impact: &'static str,
}

const UNSUPPORTED_DECODER: TeslaMateSourceFactDisposition =
    TeslaMateSourceFactDisposition::UnsupportedOrLost;
const DECODER_DROP: TeslaMateSourceCaptureStatus =
    TeslaMateSourceCaptureStatus::SelectedButDiscardedByDecoder;
const NOT_BOUND: TeslaMateSourceFingerprintStatus = TeslaMateSourceFingerprintStatus::NotBound;
const NOT_READ: TeslaMateSourceCaptureStatus = TeslaMateSourceCaptureStatus::NotRead;

/// Exact field/value-domain denominator requested by the 2026-08-08 parity
/// review. This is not a denominator for every column in TeslaMate.
pub const TESLAMATE_SOURCE_PARITY_ENTRIES: [TeslaMateSourceParityEntry; 17] = [
    TeslaMateSourceParityEntry {
        source_relation: "cars",
        source_field_or_domain: "display_priority",
        disposition: UNSUPPORTED_DECODER,
        capture: DECODER_DROP,
        fingerprint: NOT_BOUND,
        current_app_impact: "not visible while Hub and app bind one selected car; required for future multi-car ordering provenance",
    },
    TeslaMateSourceParityEntry {
        source_relation: "cars",
        source_field_or_domain: "inserted_at",
        disposition: UNSUPPORTED_DECODER,
        capture: DECODER_DROP,
        fingerprint: NOT_BOUND,
        current_app_impact: "source provenance only; not represented by the app",
    },
    TeslaMateSourceParityEntry {
        source_relation: "cars",
        source_field_or_domain: "updated_at",
        disposition: UNSUPPORTED_DECODER,
        capture: DECODER_DROP,
        fingerprint: NOT_BOUND,
        current_app_impact: "source provenance only; not represented by the app",
    },
    TeslaMateSourceParityEntry {
        source_relation: "drives",
        source_field_or_domain: "start_km",
        disposition: TeslaMateSourceFactDisposition::FingerprintOnly,
        capture: TeslaMateSourceCaptureStatus::CapturedTyped,
        fingerprint: TeslaMateSourceFingerprintStatus::Bound,
        current_app_impact: "not directly displayed; THP1 carries distance_km and position odometer instead",
    },
    TeslaMateSourceParityEntry {
        source_relation: "drives",
        source_field_or_domain: "end_km",
        disposition: TeslaMateSourceFactDisposition::FingerprintOnly,
        capture: TeslaMateSourceCaptureStatus::CapturedTyped,
        fingerprint: TeslaMateSourceFingerprintStatus::Bound,
        current_app_impact: "not directly displayed; THP1 carries distance_km and position odometer instead",
    },
    TeslaMateSourceParityEntry {
        source_relation: "settings",
        source_field_or_domain: "unit_of_length",
        disposition: UNSUPPORTED_DECODER,
        capture: NOT_READ,
        fingerprint: NOT_BOUND,
        current_app_impact: "presentation provenance; app unit preference remains locally owned",
    },
    TeslaMateSourceParityEntry {
        source_relation: "settings",
        source_field_or_domain: "unit_of_temperature",
        disposition: UNSUPPORTED_DECODER,
        capture: NOT_READ,
        fingerprint: NOT_BOUND,
        current_app_impact: "presentation provenance; app unit preference remains locally owned",
    },
    TeslaMateSourceParityEntry {
        source_relation: "settings",
        source_field_or_domain: "unit_of_pressure",
        disposition: UNSUPPORTED_DECODER,
        capture: NOT_READ,
        fingerprint: NOT_BOUND,
        current_app_impact: "presentation provenance; app unit preference remains locally owned",
    },
    TeslaMateSourceParityEntry {
        source_relation: "settings",
        source_field_or_domain: "preferred_range",
        disposition: UNSUPPORTED_DECODER,
        capture: NOT_READ,
        fingerprint: NOT_BOUND,
        current_app_impact: "presentation provenance; app range preference remains locally owned",
    },
    TeslaMateSourceParityEntry {
        source_relation: "settings",
        source_field_or_domain: "base_url",
        disposition: UNSUPPORTED_DECODER,
        capture: NOT_READ,
        fingerprint: TeslaMateSourceFingerprintStatus::DeliberatelyNotHashed,
        current_app_impact: "must not become a Hub endpoint or app control",
    },
    TeslaMateSourceParityEntry {
        source_relation: "settings",
        source_field_or_domain: "grafana_url",
        disposition: UNSUPPORTED_DECODER,
        capture: NOT_READ,
        fingerprint: TeslaMateSourceFingerprintStatus::DeliberatelyNotHashed,
        current_app_impact: "must not become a Hub endpoint or app control",
    },
    TeslaMateSourceParityEntry {
        source_relation: "settings",
        source_field_or_domain: "language",
        disposition: UNSUPPORTED_DECODER,
        capture: NOT_READ,
        fingerprint: NOT_BOUND,
        current_app_impact: "presentation provenance; app language remains platform owned",
    },
    TeslaMateSourceParityEntry {
        source_relation: "settings",
        source_field_or_domain: "theme_mode",
        disposition: UNSUPPORTED_DECODER,
        capture: NOT_READ,
        fingerprint: NOT_BOUND,
        current_app_impact: "presentation provenance; app appearance remains locally owned",
    },
    TeslaMateSourceParityEntry {
        source_relation: "settings",
        source_field_or_domain: "inserted_at",
        disposition: UNSUPPORTED_DECODER,
        capture: NOT_READ,
        fingerprint: NOT_BOUND,
        current_app_impact: "source provenance only; not represented by the app",
    },
    TeslaMateSourceParityEntry {
        source_relation: "settings",
        source_field_or_domain: "updated_at",
        disposition: UNSUPPORTED_DECODER,
        capture: NOT_READ,
        fingerprint: NOT_BOUND,
        current_app_impact: "source provenance only; not represented by the app",
    },
    TeslaMateSourceParityEntry {
        source_relation: "addresses",
        source_field_or_domain: "raw",
        disposition: TeslaMateSourceFactDisposition::ProposedSensitiveExclusionBlocked,
        capture: NOT_READ,
        fingerprint: TeslaMateSourceFingerprintStatus::DeliberatelyNotHashed,
        current_app_impact: "raw geocoder response is not app-visible and may contain unrelated sensitive data",
    },
    TeslaMateSourceParityEntry {
        source_relation: "states",
        source_field_or_domain: "state values outside online/offline/asleep",
        disposition: TeslaMateSourceFactDisposition::UnsupportedOrLost,
        capture: TeslaMateSourceCaptureStatus::CapturedButUnsupportedValuesFailClosed,
        fingerprint: TeslaMateSourceFingerprintStatus::NoPublishedDigestForUnsupportedValues,
        current_app_impact: "state history is app-visible, but THP1 schema 2.1 rejects unknown values before publication",
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeslaMateSourceParityCounts {
    pub reviewed_field_or_value_domains: u16,
    pub preserved_in_pack: u16,
    pub fingerprint_only: u16,
    pub proposed_sensitive_exclusion_blocked: u16,
    pub unsupported_or_lost: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeslaMateSourceParityReport {
    pub ledger_version: u16,
    pub scope: &'static str,
    pub thp1_schema: &'static str,
    pub schema_upgrade_required_for_remaining_fields: &'static str,
    pub all_teslamate_fields_preserved: bool,
    pub counts: TeslaMateSourceParityCounts,
    pub entries: &'static [TeslaMateSourceParityEntry],
}

impl TeslaMateSourceParityReport {
    pub const fn current() -> Self {
        Self {
            ledger_version: TESLAMATE_SOURCE_PARITY_LEDGER_VERSION,
            scope: "reviewed-discarded-source-facts",
            thp1_schema: "2.1",
            schema_upgrade_required_for_remaining_fields: "2.2",
            all_teslamate_fields_preserved: false,
            counts: TeslaMateSourceParityCounts {
                reviewed_field_or_value_domains: TESLAMATE_SOURCE_PARITY_REVIEWED_DENOMINATOR,
                preserved_in_pack: 0,
                fingerprint_only: 2,
                proposed_sensitive_exclusion_blocked: 1,
                unsupported_or_lost: 14,
            },
            entries: &TESLAMATE_SOURCE_PARITY_ENTRIES,
        }
    }

    /// A caller asking for all TeslaMate fields can never accidentally treat
    /// the selected THP1 projection as full parity.
    pub fn require_all_teslamate_fields(self) -> Result<(), TeslaMateFullParityUnavailable> {
        if self.all_teslamate_fields_preserved {
            Ok(())
        } else {
            Err(TeslaMateFullParityUnavailable {
                reviewed_denominator: self.counts.reviewed_field_or_value_domains,
                schema_upgrade_required: self.schema_upgrade_required_for_remaining_fields,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "all TeslaMate fields are not preserved: {reviewed_denominator} reviewed field/value domains remain bounded by the parity ledger; THP1 schema {schema_upgrade_required} work is required"
)]
pub struct TeslaMateFullParityUnavailable {
    pub reviewed_denominator: u16,
    pub schema_upgrade_required: &'static str,
}

const EVIDENCE_DOMAIN: &[u8] = b"teslatlas-hub/teslamate-source-evidence/v1";
const DRIVE_EVIDENCE_DOMAIN: &[u8] = b"teslatlas-hub/teslamate-drive-odometer-evidence/v1";
const STATE_EVIDENCE_DOMAIN: &[u8] = b"teslatlas-hub/teslamate-state-text-evidence/v1";

/// Bounded, fixed-order evidence for already-decoded, non-secret facts that do
/// not fit THP1 schema 2.1. Each source kind owns an independent ordered stream
/// and finalization combines kinds in a fixed order, so capture-lane scheduling
/// cannot affect the result.
#[derive(Debug)]
pub(crate) struct TeslaMateSourceEvidenceFingerprint {
    drives: EvidenceStream,
    states: EvidenceStream,
}

impl TeslaMateSourceEvidenceFingerprint {
    pub(crate) fn new() -> Self {
        Self {
            drives: EvidenceStream::new(DRIVE_EVIDENCE_DOMAIN),
            states: EvidenceStream::new(STATE_EVIDENCE_DOMAIN),
        }
    }

    pub(crate) fn record_drive(
        &mut self,
        drive: &TeslaMateDrive,
    ) -> Result<(), TeslaMateSourceEvidenceError> {
        self.record_drive_odometer(drive.id, drive.start_km, drive.end_km)
    }

    fn record_drive_odometer(
        &mut self,
        source_id: i64,
        start_km: Option<f64>,
        end_km: Option<f64>,
    ) -> Result<(), TeslaMateSourceEvidenceError> {
        self.drives.begin_record("drives", source_id)?;
        record_optional_f64(&mut self.drives.digest, start_km);
        record_optional_f64(&mut self.drives.digest, end_km);
        Ok(())
    }

    pub(crate) fn record_state(
        &mut self,
        state: &TeslaMateState,
    ) -> Result<(), TeslaMateSourceEvidenceError> {
        self.record_state_text(state.id, &state.state)
    }

    fn record_state_text(
        &mut self,
        source_id: i64,
        state: &str,
    ) -> Result<(), TeslaMateSourceEvidenceError> {
        self.states.begin_record("states", source_id)?;
        let length = u64::try_from(state.len())
            .map_err(|_| TeslaMateSourceEvidenceError::StateTextTooLarge { source_id })?;
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
    ) -> Result<(), TeslaMateSourceEvidenceError> {
        if source_id <= 0 || self.last_source_id.is_some_and(|last| source_id <= last) {
            return Err(TeslaMateSourceEvidenceError::NonIncreasingSourceId {
                relation,
                previous: self.last_source_id,
                current: source_id,
            });
        }
        self.rows = self
            .rows
            .checked_add(1)
            .ok_or(TeslaMateSourceEvidenceError::RowCountOverflow { relation })?;
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
pub enum TeslaMateSourceEvidenceError {
    #[error(
        "TeslaMate {relation} source evidence IDs must be positive and strictly increasing (previous {previous:?}, current {current})"
    )]
    NonIncreasingSourceId {
        relation: &'static str,
        previous: Option<i64>,
        current: i64,
    },
    #[error("TeslaMate {relation} source evidence row count overflowed")]
    RowCountOverflow { relation: &'static str },
    #[error("TeslaMate state {source_id} text is too large to fingerprint")]
    StateTextTooLarge { source_id: i64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_loss_ledger_has_an_exact_fail_closed_denominator() {
        let report = TeslaMateSourceParityReport::current();
        assert_eq!(report.entries.len(), 17);
        assert_eq!(report.counts.reviewed_field_or_value_domains, 17);
        assert_eq!(report.counts.preserved_in_pack, 0);
        assert_eq!(report.counts.fingerprint_only, 2);
        assert_eq!(report.counts.proposed_sensitive_exclusion_blocked, 1);
        assert_eq!(report.counts.unsupported_or_lost, 14);
        assert!(!report.all_teslamate_fields_preserved);
        assert_eq!(
            report.require_all_teslamate_fields(),
            Err(TeslaMateFullParityUnavailable {
                reviewed_denominator: 17,
                schema_upgrade_required: "2.2",
            })
        );

        let disposition_total = report.entries.iter().fold([0_u16; 4], |mut counts, entry| {
            let index = match entry.disposition {
                TeslaMateSourceFactDisposition::PreservedInPack => 0,
                TeslaMateSourceFactDisposition::FingerprintOnly => 1,
                TeslaMateSourceFactDisposition::ProposedSensitiveExclusionBlocked => 2,
                TeslaMateSourceFactDisposition::UnsupportedOrLost => 3,
            };
            counts[index] += 1;
            counts
        });
        assert_eq!(disposition_total, [0, 2, 1, 14]);

        let json = serde_json::to_value(report).expect("serialize public parity report");
        assert_eq!(json["allTeslamateFieldsPreserved"], false);
        assert_eq!(json["schemaUpgradeRequiredForRemainingFields"], "2.2");
        assert_eq!(json["counts"]["fingerprintOnly"], 2);
    }

    #[test]
    fn evidence_digest_binds_each_drive_endpoint_and_state_text() {
        fn digest(start_km: Option<f64>, end_km: Option<f64>, state: &str) -> Sha256Digest {
            let mut evidence = TeslaMateSourceEvidenceFingerprint::new();
            evidence
                .record_drive(&TeslaMateDrive {
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
                .record_state(&TeslaMateState {
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
        let mut drive_then_state = TeslaMateSourceEvidenceFingerprint::new();
        drive_then_state
            .record_drive_odometer(10, Some(10.0), Some(11.0))
            .unwrap();
        drive_then_state.record_state_text(20, "online").unwrap();

        let mut state_then_drive = TeslaMateSourceEvidenceFingerprint::new();
        state_then_drive.record_state_text(20, "online").unwrap();
        state_then_drive
            .record_drive_odometer(10, Some(10.0), Some(11.0))
            .unwrap();

        assert_eq!(drive_then_state.finish(), state_then_drive.finish());
    }

    #[test]
    fn evidence_rejects_non_canonical_source_order() {
        let mut evidence = TeslaMateSourceEvidenceFingerprint::new();
        evidence
            .record_drive_odometer(10, Some(10.0), Some(11.0))
            .unwrap();
        assert!(matches!(
            evidence.record_drive_odometer(10, Some(10.0), Some(11.0)),
            Err(TeslaMateSourceEvidenceError::NonIncreasingSourceId {
                relation: "drives",
                previous: Some(10),
                current: 10,
            })
        ));
    }
}
