// SPDX-License-Identifier: AGPL-3.0-only

use crate::fleet_telemetry::{FleetTelemetryAccumulator, FleetTelemetryError};

const EDGE_SOURCE_RECORD_TYPE: &str = "fleet_api_vehicle_data_v1";
const EDGE_ID_MAX_BYTES: usize = 128;

fn projection_input_is_unsupported(error: &FleetTelemetryError) -> bool {
    matches!(
        error,
        FleetTelemetryError::InvalidJson
            | FleetTelemetryError::InvalidTimestamp
            | FleetTelemetryError::TooManyFields
            | FleetTelemetryError::InvalidFieldName
            | FleetTelemetryError::InvalidFieldValue
            | FleetTelemetryError::NonFiniteNumber
            | FleetTelemetryError::InvalidCoordinates
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeBinding {
    pub installation_id: String,
    pub lineage: String,
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub vin: String,
    pub car_id: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedEdgeRecord {
    pub spool_seq: u64,
    pub stable_record_id: String,
    pub legacy_record_id: String,
    pub payload_sha256: String,
    pub edge_received_at_ms: i64,
    pub envelope_json: Vec<u8>,
    pub non_projection_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedEdgeGap {
    pub spool_seq: u64,
    pub notice_id: String,
    pub occurred_at_ms: i64,
    pub reason: String,
    pub evidence_sha256: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VerifiedEdgeItem {
    Record(VerifiedEdgeRecord),
    Gap(VerifiedEdgeGap),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeAcceptance {
    pub spool_seq: u64,
    pub item_id: String,
    pub category: String,
    pub ack_frontier: u64,
    pub observation_id: Option<i64>,
    pub accumulator_state: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeLedgerCounts {
    pub applications: u64,
    pub sequences: u64,
    pub pending_publications: u64,
    pub ack_frontier: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeDiagnosticState {
    pub state: String,
    pub detail: String,
    pub updated_at_ms: i64,
}

fn edge_error(message: impl Into<String>) -> StoreError {
    StoreError::SyncMutation(format!("edge delivery: {}", message.into()))
}

fn validate_edge_text(label: &str, value: &str, maximum: usize) -> Result<(), StoreError> {
    if value.is_empty()
        || value.len() > maximum
        || value.chars().any(char::is_control)
        || !value.is_ascii()
    {
        return Err(edge_error(format!("invalid {label}")));
    }
    Ok(())
}

fn validate_digest(label: &str, value: &str) -> Result<(), StoreError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(edge_error(format!("invalid {label}")));
    }
    Ok(())
}

fn sqlite_seq(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| edge_error("spool sequence exceeds SQLite range"))
}

fn validate_edge_binding_shape(binding: &EdgeBinding) -> Result<(), StoreError> {
    validate_edge_text(
        "installation id",
        &binding.installation_id,
        EDGE_ID_MAX_BYTES,
    )?;
    validate_edge_text("lineage", &binding.lineage, EDGE_ID_MAX_BYTES)?;
    validate_edge_text("VIN", &binding.vin, 17)?;
    if binding.vin.len() != 17
        || binding.source_id.is_nil()
        || binding.vehicle_id.is_nil()
        || binding.car_id <= 0
    {
        return Err(edge_error("invalid configured identity binding"));
    }
    Ok(())
}

fn validate_edge_binding_in_connection(
    connection: &Connection,
    binding: &EdgeBinding,
) -> Result<i64, StoreError> {
    let configured: Option<(Option<String>, i64, Option<String>, Option<i64>)> = connection
        .query_row(
            "SELECT vehicle.vin,
                    (SELECT COUNT(*) FROM vehicle_identity_aliases AS source_alias
                      WHERE source_alias.vehicle_id = vehicle.vehicle_id
                        AND source_alias.source_id = ?2),
                    (SELECT CASE WHEN COUNT(*) = 1 THEN MIN(alias_value) END
                       FROM vehicle_identity_aliases AS eid
                      WHERE eid.vehicle_id = vehicle.vehicle_id
                        AND eid.source_id = ?2
                        AND eid.alias_kind = 'tesla_eid'),
                    (SELECT selected_car_id FROM v2_base_bindings AS binding
                      WHERE binding.vehicle_id = vehicle.vehicle_id)
               FROM vehicles AS vehicle WHERE vehicle.vehicle_id = ?1",
            params![
                binding.vehicle_id.to_string(),
                binding.source_id.to_string()
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(StoreError::Query)?;
    let Some((configured_vin, source_aliases, source_eid, selected_car_id)) = configured else {
        return Err(StoreError::UnknownVehicle(binding.vehicle_id));
    };
    let source_eid = source_eid
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| edge_error("configured vehicle Tesla identity is unavailable"))?;
    let expected_car_id = selected_car_id.unwrap_or(source_eid);
    if configured_vin.as_deref() != Some(binding.vin.as_str())
        || source_aliases < 1
        || expected_car_id != binding.car_id
    {
        return Err(edge_error(
            "configured vehicle source, VIN, or car identity mismatch",
        ));
    }
    if selected_car_id.is_some() {
        let expected_source_id = binding.source_id.to_string();
        let account_id: Option<String> = connection
            .query_row(
                "SELECT account_id FROM v2_base_bindings WHERE vehicle_id = ?1",
                params![binding.vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if account_id.as_deref() != Some(expected_source_id.as_str()) {
            return Err(edge_error("configured projection source mismatch"));
        }
    }
    Ok(source_eid)
}

impl HubStore {
    pub fn validate_edge_binding(&self, binding: &EdgeBinding) -> Result<(), StoreError> {
        validate_edge_binding_shape(binding)?;
        let connection = self.open_read_only_connection()?;
        validate_edge_binding_in_connection(&connection, binding)?;
        Ok(())
    }

    pub fn accept_verified_edge_item(
        &self,
        binding: &EdgeBinding,
        item: &VerifiedEdgeItem,
        committed_at_ms: i64,
        offline_drive_timeout: Duration,
    ) -> Result<EdgeAcceptance, StoreError> {
        validate_edge_binding_shape(binding)?;
        if binding.car_id <= 0 || offline_drive_timeout.is_zero() || committed_at_ms < 0 {
            return Err(edge_error("invalid acceptance parameters"));
        }
        let (spool_seq, item_kind, item_id) = match item {
            VerifiedEdgeItem::Record(record) => {
                validate_digest("stable record id", &record.stable_record_id)?;
                validate_digest("legacy record id", &record.legacy_record_id)?;
                validate_digest("payload digest", &record.payload_sha256)?;
                if record.edge_received_at_ms < 0
                    || record.envelope_json.len() > MAX_RAW_OBSERVATION_BYTES
                {
                    return Err(edge_error("invalid record bounds"));
                }
                (record.spool_seq, "record", record.stable_record_id.as_str())
            }
            VerifiedEdgeItem::Gap(gap) => {
                validate_digest("gap notice id", &gap.notice_id)?;
                validate_digest("gap evidence digest", &gap.evidence_sha256)?;
                validate_edge_text("gap reason", &gap.reason, EDGE_ID_MAX_BYTES)?;
                if gap.occurred_at_ms < 0 {
                    return Err(edge_error("invalid gap occurrence time"));
                }
                (gap.spool_seq, "gap", gap.notice_id.as_str())
            }
        };
        let spool_seq_i64 = sqlite_seq(spool_seq)?;
        if spool_seq == 0 {
            return Err(edge_error("spool sequence starts at one"));
        }

        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let source_vehicle_eid = validate_edge_binding_in_connection(&transaction, binding)?;

        transaction
            .execute(
                "INSERT INTO edge_lineages(
                    installation_id, lineage, source_id, vehicle_id, vin, car_id,
                    first_spool_seq, ack_frontier, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?7)
                 ON CONFLICT(installation_id, lineage) DO NOTHING",
                params![
                    binding.installation_id,
                    binding.lineage,
                    binding.source_id.to_string(),
                    binding.vehicle_id.to_string(),
                    binding.vin,
                    binding.car_id,
                    committed_at_ms,
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        let lineage: (String, String, String, Option<i64>, Option<i64>) = transaction
            .query_row(
                "SELECT source_id, vehicle_id, vin, car_id, ack_frontier
                   FROM edge_lineages WHERE installation_id = ?1 AND lineage = ?2",
                params![binding.installation_id, binding.lineage],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .map_err(StoreError::Query)?;
        if lineage.0 != binding.source_id.to_string()
            || lineage.1 != binding.vehicle_id.to_string()
            || lineage.2 != binding.vin
            || lineage.3 != Some(binding.car_id)
        {
            return Err(edge_error("lineage identity binding conflict"));
        }

        let existing: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT item_kind, item_id, category
                   FROM edge_sequence_dispositions
                  WHERE installation_id = ?1 AND lineage = ?2 AND spool_seq = ?3",
                params![binding.installation_id, binding.lineage, spool_seq_i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if let Some((stored_kind, stored_id, category)) = existing {
            if stored_kind != item_kind || stored_id != item_id {
                return Err(edge_error("spool sequence identity conflict"));
            }
            let frontier = lineage
                .4
                .and_then(|value| u64::try_from(value).ok())
                .ok_or_else(|| edge_error("stored ACK frontier is invalid"))?;
            return Ok(EdgeAcceptance {
                spool_seq,
                item_id: item_id.to_owned(),
                category,
                ack_frontier: frontier,
                observation_id: None,
                accumulator_state: None,
            });
        }
        if let Some(frontier) = lineage.4
            && spool_seq_i64
                != frontier
                    .checked_add(1)
                    .ok_or_else(|| edge_error("ACK frontier exhausted"))?
        {
            return Err(edge_error("non-contiguous spool sequence"));
        }

        let (
            category,
            reason,
            legacy_id,
            payload_hash,
            occurred_at,
            evidence,
            observation_id,
            state,
        ) = match item {
            VerifiedEdgeItem::Gap(gap) => (
                "durable_gap".to_owned(),
                gap.reason.clone(),
                None,
                None,
                Some(gap.occurred_at_ms),
                Some(gap.evidence_sha256.clone()),
                None,
                None,
            ),
            VerifiedEdgeItem::Record(record) => {
                let alias_conflict: Option<String> = transaction
                    .query_row(
                        "SELECT item_id FROM edge_sequence_dispositions
                              WHERE installation_id = ?1 AND lineage = ?2
                                AND legacy_record_id = ?3 AND item_id != ?4 LIMIT 1",
                        params![
                            binding.installation_id,
                            binding.lineage,
                            record.legacy_record_id,
                            record.stable_record_id
                        ],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(StoreError::Query)?;
                if alias_conflict.is_some() {
                    return Err(edge_error("legacy record alias conflict"));
                }
                let application: Option<(String, Option<i64>)> = transaction
                        .query_row(
                            "SELECT payload_sha256, observation_id FROM edge_applications
                              WHERE installation_id = ?1 AND lineage = ?2 AND stable_record_id = ?3",
                            params![binding.installation_id, binding.lineage, record.stable_record_id],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()
                        .map_err(StoreError::Query)?;
                if let Some((stored_payload, stored_observation)) = application {
                    if stored_payload != record.payload_sha256 {
                        return Err(edge_error("stable record payload conflict"));
                    }
                    (
                        "duplicate".to_owned(),
                        "stable_record_already_applied".to_owned(),
                        Some(record.legacy_record_id.clone()),
                        Some(record.payload_sha256.clone()),
                        None,
                        None,
                        stored_observation,
                        None,
                    )
                } else if let Some(reason) = &record.non_projection_reason {
                    validate_edge_text("non-projection reason", reason, EDGE_ID_MAX_BYTES)?;
                    transaction
                            .execute(
                                "INSERT INTO edge_applications(
                                    installation_id, lineage, stable_record_id, payload_sha256,
                                    disposition, first_spool_seq, observation_id, applied_at_ms
                                 ) VALUES (?1, ?2, ?3, ?4, 'durable_non_projection_event', ?5, NULL, ?6)",
                                params![binding.installation_id, binding.lineage, record.stable_record_id, record.payload_sha256, spool_seq_i64, committed_at_ms],
                            )
                            .map_err(StoreError::LifecycleWrite)?;
                    (
                        "durable_non_projection_event".to_owned(),
                        reason.clone(),
                        Some(record.legacy_record_id.clone()),
                        Some(record.payload_sha256.clone()),
                        None,
                        None,
                        None,
                        None,
                    )
                } else {
                    let mut accumulator = match transaction
                        .query_row(
                            "SELECT state_json FROM edge_accumulator_states
                                  WHERE installation_id = ?1 AND lineage = ?2",
                            params![binding.installation_id, binding.lineage],
                            |row| row.get::<_, Vec<u8>>(0),
                        )
                        .optional()
                        .map_err(StoreError::Query)?
                    {
                        Some(encoded) => FleetTelemetryAccumulator::restore_complete_for_vin(
                            &encoded,
                            &binding.vin,
                        ),
                        None => FleetTelemetryAccumulator::empty(&binding.vin),
                    }
                    .map_err(|error| edge_error(format!("accumulator restore failed: {error}")))?;
                    match accumulator.apply_json(&record.envelope_json) {
                        Err(error) if projection_input_is_unsupported(&error) => {
                            let reason = "projection_unsupported";
                            transaction
                                .execute(
                                    "INSERT INTO edge_applications(
                                        installation_id, lineage, stable_record_id, payload_sha256,
                                        disposition, first_spool_seq, observation_id, applied_at_ms
                                     ) VALUES (?1, ?2, ?3, ?4, 'durable_non_projection_event', ?5, NULL, ?6)",
                                    params![binding.installation_id, binding.lineage, record.stable_record_id, record.payload_sha256, spool_seq_i64, committed_at_ms],
                                )
                                .map_err(StoreError::LifecycleWrite)?;
                            (
                                "durable_non_projection_event".to_owned(),
                                reason.to_owned(),
                                Some(record.legacy_record_id.clone()),
                                Some(record.payload_sha256.clone()),
                                None,
                                None,
                                None,
                                None,
                            )
                        }
                        Err(error) => {
                            return Err(edge_error(format!(
                                "Fleet projection identity or state rejected: {error}"
                            )));
                        }
                        Ok(snapshot) => {
                            if snapshot.vin != binding.vin {
                                return Err(edge_error("projected VIN mismatch"));
                            }
                            let state = accumulator.encode_complete().map_err(|error| {
                                edge_error(format!("accumulator encode failed: {error}"))
                            })?;
                            let source_state = snapshot
                                .source_vehicle_state
                                .as_deref()
                                .filter(|value| !value.is_empty() && value.len() <= 64)
                                .unwrap_or("unknown");
                            let projection_observed_at_ms = current_observation_for_type(
                                &transaction,
                                binding.vehicle_id,
                                EDGE_SOURCE_RECORD_TYPE,
                            )?
                            .map_or(snapshot.timestamp_ms, |current| {
                                current.observed_at_ms.max(snapshot.timestamp_ms)
                            });
                            let input = ObservationInput {
                                source_id: binding.source_id,
                                vehicle_id: binding.vehicle_id,
                                observed_at_ms: projection_observed_at_ms,
                                payload: serde_json::json!({
                                    "record_type": EDGE_SOURCE_RECORD_TYPE,
                                    "source_vehicle_id": source_vehicle_eid.to_string(),
                                    "source_vehicle_state": source_state,
                                    "provider_raw_json": {"response": snapshot.owner_data},
                                }),
                            };
                            let result = self
                                .accept_owner_observation_and_lifecycle_in_transaction(
                                    &transaction,
                                    &input,
                                    record.edge_received_at_ms,
                                    binding.car_id,
                                    offline_drive_timeout,
                                )?;
                            let observation_id = result.append.observation.observation_id;
                            transaction
                                .execute(
                                    "INSERT INTO edge_applications(
                                    installation_id, lineage, stable_record_id, payload_sha256,
                                    disposition, first_spool_seq, observation_id, applied_at_ms
                                 ) VALUES (?1, ?2, ?3, ?4, 'projected_telemetry', ?5, ?6, ?7)",
                                    params![
                                        binding.installation_id,
                                        binding.lineage,
                                        record.stable_record_id,
                                        record.payload_sha256,
                                        spool_seq_i64,
                                        observation_id,
                                        committed_at_ms
                                    ],
                                )
                                .map_err(StoreError::LifecycleWrite)?;
                            transaction
                                .execute(
                                    "INSERT INTO edge_accumulator_states(
                                    installation_id, lineage, vehicle_id, state_version,
                                    state_json, through_spool_seq, updated_at_ms
                                 ) VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6)
                                 ON CONFLICT(installation_id, lineage) DO UPDATE SET
                                    state_json = excluded.state_json,
                                    through_spool_seq = excluded.through_spool_seq,
                                    updated_at_ms = excluded.updated_at_ms",
                                    params![
                                        binding.installation_id,
                                        binding.lineage,
                                        binding.vehicle_id.to_string(),
                                        state,
                                        spool_seq_i64,
                                        committed_at_ms
                                    ],
                                )
                                .map_err(StoreError::LifecycleWrite)?;
                            transaction
                                .execute(
                                    "INSERT INTO edge_pending_publications(
                                    installation_id, lineage, stable_record_id, vehicle_id,
                                    status, attempts, next_attempt_ms, last_error,
                                    created_at_ms, completed_at_ms
                                 ) VALUES (?1, ?2, ?3, ?4, 'pending', 0, ?5, NULL, ?5, NULL)",
                                    params![
                                        binding.installation_id,
                                        binding.lineage,
                                        record.stable_record_id,
                                        binding.vehicle_id.to_string(),
                                        committed_at_ms
                                    ],
                                )
                                .map_err(StoreError::LifecycleWrite)?;
                            (
                                "projected_telemetry".to_owned(),
                                "fleet_protojson_projected".to_owned(),
                                Some(record.legacy_record_id.clone()),
                                Some(record.payload_sha256.clone()),
                                None,
                                None,
                                Some(observation_id),
                                Some(state),
                            )
                        }
                    }
                }
            }
        };

        self.maybe_stream_fault(StreamFaultPoint::WatermarkUpdate)?;
        #[cfg(feature = "edge-test-faults")]
        if crate::edge_test_fault::return_error_at("receipt_insert") {
            return Err(edge_error("instrumented receipt insert failure"));
        }
        transaction
            .execute(
                "INSERT INTO edge_sequence_dispositions(
                    installation_id, lineage, spool_seq, item_kind, item_id,
                    legacy_record_id, payload_sha256, category, reason,
                    occurred_at_ms, evidence_sha256, committed_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    binding.installation_id,
                    binding.lineage,
                    spool_seq_i64,
                    item_kind,
                    item_id,
                    legacy_id,
                    payload_hash,
                    category,
                    reason,
                    occurred_at,
                    evidence,
                    committed_at_ms
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        transaction
            .execute(
                "UPDATE edge_lineages SET
                    first_spool_seq = COALESCE(first_spool_seq, ?3),
                    ack_frontier = ?3,
                    updated_at_ms = MAX(updated_at_ms, ?4)
                  WHERE installation_id = ?1 AND lineage = ?2",
                params![
                    binding.installation_id,
                    binding.lineage,
                    spool_seq_i64,
                    committed_at_ms
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        #[cfg(feature = "edge-test-faults")]
        if crate::edge_test_fault::return_error_at("frontier_update") {
            return Err(edge_error("instrumented frontier update failure"));
        }
        self.maybe_stream_fault(StreamFaultPoint::Commit)?;
        #[cfg(feature = "edge-test-faults")]
        crate::edge_test_fault::abort_at("before_accept_commit");
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(EdgeAcceptance {
            spool_seq,
            item_id: item_id.to_owned(),
            category,
            ack_frontier: spool_seq,
            observation_id,
            accumulator_state: state,
        })
    }

    pub fn edge_ledger_counts(
        &self,
        installation_id: &str,
        lineage: &str,
    ) -> Result<EdgeLedgerCounts, StoreError> {
        let connection = self.open_read_only_connection()?;
        let count = |table: &str| -> Result<u64, StoreError> {
            let sql =
                format!("SELECT COUNT(*) FROM {table} WHERE installation_id = ?1 AND lineage = ?2");
            let value: i64 = connection
                .query_row(&sql, params![installation_id, lineage], |row| row.get(0))
                .map_err(StoreError::Query)?;
            u64::try_from(value).map_err(|_| StoreError::InvalidStoredCount)
        };
        let frontier: Option<i64> = connection
            .query_row(
                "SELECT ack_frontier FROM edge_lineages WHERE installation_id = ?1 AND lineage = ?2",
                params![installation_id, lineage],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?
            .flatten();
        let pending_publications: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM edge_pending_publications
                  WHERE installation_id = ?1 AND lineage = ?2 AND status != 'complete'",
                params![installation_id, lineage],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        Ok(EdgeLedgerCounts {
            applications: count("edge_applications")?,
            sequences: count("edge_sequence_dispositions")?,
            pending_publications: u64::try_from(pending_publications)
                .map_err(|_| StoreError::InvalidStoredCount)?,
            ack_frontier: frontier
                .map(u64::try_from)
                .transpose()
                .map_err(|_| StoreError::InvalidStoredSequence)?,
        })
    }

    pub fn set_edge_diagnostic(
        &self,
        binding: &EdgeBinding,
        state: &str,
        detail: &str,
        updated_at_ms: i64,
    ) -> Result<(), StoreError> {
        if detail.len() > 256 || updated_at_ms < 0 {
            return Err(edge_error("invalid diagnostic"));
        }
        let connection = self.open()?;
        connection
            .execute(
                "INSERT INTO edge_consumer_diagnostics(installation_id, lineage, state, detail, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(installation_id, lineage) DO UPDATE SET
                    state = excluded.state, detail = excluded.detail,
                    updated_at_ms = excluded.updated_at_ms",
                params![binding.installation_id, binding.lineage, state, detail, updated_at_ms],
            )
            .map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }

    pub fn edge_diagnostic(
        &self,
        installation_id: &str,
        lineage: &str,
    ) -> Result<Option<EdgeDiagnosticState>, StoreError> {
        validate_edge_text("installation id", installation_id, EDGE_ID_MAX_BYTES)?;
        validate_edge_text("lineage", lineage, EDGE_ID_MAX_BYTES)?;
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT state, detail, updated_at_ms
                   FROM edge_consumer_diagnostics
                  WHERE installation_id = ?1 AND lineage = ?2",
                params![installation_id, lineage],
                |row| {
                    Ok(EdgeDiagnosticState {
                        state: row.get(0)?,
                        detail: row.get(1)?,
                        updated_at_ms: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::Query)
    }

    pub fn complete_edge_publications(
        &self,
        binding: &EdgeBinding,
        completed_at_ms: i64,
    ) -> Result<usize, StoreError> {
        if completed_at_ms < 0 {
            return Err(edge_error("invalid publication completion time"));
        }
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE edge_pending_publications
                    SET status = 'complete', completed_at_ms = ?3, last_error = NULL
                  WHERE installation_id = ?1 AND lineage = ?2 AND status != 'complete'",
                params![binding.installation_id, binding.lineage, completed_at_ms],
            )
            .map_err(StoreError::LifecycleWrite)
    }

    pub fn fail_edge_publications(
        &self,
        binding: &EdgeBinding,
        failed_at_ms: i64,
        detail: &str,
    ) -> Result<usize, StoreError> {
        if failed_at_ms < 0 || detail.is_empty() || detail.len() > 256 {
            return Err(edge_error("invalid publication failure"));
        }
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE edge_pending_publications
                    SET status = 'retry', attempts = attempts + 1,
                        next_attempt_ms = ?3, last_error = ?4
                  WHERE installation_id = ?1 AND lineage = ?2 AND status != 'complete'",
                params![
                    binding.installation_id,
                    binding.lineage,
                    failed_at_ms,
                    detail
                ],
            )
            .map_err(StoreError::LifecycleWrite)
    }

    pub fn edge_has_pending_publications(&self, binding: &EdgeBinding) -> Result<bool, StoreError> {
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM edge_pending_publications
                     WHERE installation_id = ?1 AND lineage = ?2 AND status != 'complete'
                 )",
                params![binding.installation_id, binding.lineage],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }
}

impl From<FleetTelemetryError> for StoreError {
    fn from(error: FleetTelemetryError) -> Self {
        edge_error(error.to_string())
    }
}
