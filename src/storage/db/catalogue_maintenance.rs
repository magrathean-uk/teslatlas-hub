// SPDX-License-Identifier: AGPL-3.0-only

fn schema_version(connection: &Connection) -> Result<i32, StoreError> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(StoreError::Query)
}

fn read_only_count(connection: &Connection, sql: &'static str) -> Result<u64, StoreError> {
    let count: i64 = connection
        .query_row(sql, [], |row| row.get(0))
        .map_err(StoreError::Query)?;
    u64::try_from(count).map_err(|_| StoreError::InvalidStoredCount)
}

fn referenced_pack_rows_at(
    connection: &Connection,
    retired_expiry_cutoff_ms: i64,
) -> Result<Vec<(String, String, i64)>, StoreError> {
    let mut rows = {
        let mut statement = connection
            .prepare("SELECT sha256, relative_path, compressed_bytes FROM sync_packs")
            .map_err(StoreError::Query)?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?
    };
    let retired_rows = {
        let mut statement = connection
            .prepare(
                "SELECT lineage.vehicle_id, lineage.head_digest, lineage.manifest_json,
                        packs.pack_digest, packs.relative_path, packs.compressed_bytes
                   FROM sync_retired_lineage_packs AS packs
                   JOIN sync_retired_lineages AS lineage
                     ON lineage.vehicle_id = packs.vehicle_id
                    AND lineage.head_digest = packs.head_digest
                  WHERE lineage.expires_at_ms > ?1
                  ORDER BY packs.pack_digest, lineage.vehicle_id, lineage.head_digest",
            )
            .map_err(StoreError::Query)?;
        statement
            .query_map(params![retired_expiry_cutoff_ms], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?
    };
    for (vehicle_id, head_digest, manifest_json, pack_digest, relative_path, compressed_bytes) in
        retired_rows
    {
        validate_retired_lineage_pack_binding(
            &vehicle_id,
            &head_digest,
            &manifest_json,
            &pack_digest,
            &relative_path,
            compressed_bytes,
        )?;
        rows.push((pack_digest, relative_path, compressed_bytes));
    }
    rows.sort_unstable();
    let mut deduplicated: Vec<(String, String, i64)> = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(existing) = deduplicated.last()
            && existing.0 == row.0
        {
            if existing != &row {
                return Err(StoreError::LineageCatalogConflict);
            }
            continue;
        }
        deduplicated.push(row);
    }
    Ok(deduplicated)
}

fn validate_retired_lineage_pack_binding(
    vehicle_id: &str,
    head_digest: &str,
    manifest_json: &[u8],
    pack_digest: &str,
    relative_path: &str,
    compressed_bytes: i64,
) -> Result<(), StoreError> {
    let digest = pack_digest
        .parse::<Sha256Digest>()
        .map_err(|_| StoreError::LineageCatalogConflict)?;
    let manifest: LineageManifestV2 =
        serde_json::from_slice(manifest_json).map_err(StoreError::DeserializeManifest)?;
    manifest
        .validate_with_limits(ProtocolLimits::default())
        .map_err(StoreError::Manifest)?;
    let descriptor = manifest
        .base
        .packs
        .iter()
        .chain(manifest.deltas.iter().map(|delta| &delta.pack))
        .find(|pack| pack.sha256 == digest)
        .ok_or(StoreError::LineageCatalogConflict)?;
    if vehicle_id != manifest.vehicle_id.to_string()
        || head_digest != manifest.head_digest.to_string()
        || relative_path != descriptor.relative_path
        || compressed_bytes
            != i64::try_from(descriptor.compressed_bytes)
                .map_err(|_| StoreError::PackSizeTooLarge)?
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    Ok(())
}

fn outbound_request_clock_ms() -> Result<i64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(StoreError::OutboundRequestClock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| StoreError::OutboundRequestClockOverflow)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn retired_lineage_clock_ms() -> Result<i64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(StoreError::RetiredLineageClock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| StoreError::RetiredLineageClockOverflow)
}

fn prune_expired_outbound_request_receipts(
    transaction: &Transaction<'_>,
) -> Result<(), StoreError> {
    let cutoff_ms = outbound_request_clock_ms()?.saturating_sub(OUTBOUND_REQUEST_RETENTION_MS);
    transaction
        .execute(
            "DELETE FROM outbound_request_receipts
              WHERE outcome <> 'started'
                AND completed_at_ms < ?1",
            params![cutoff_ms],
        )
        .map_err(StoreError::OutboundRequestReceipt)?;
    Ok(())
}

fn ensure_outbound_request_capacity(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    prune_expired_outbound_request_receipts(transaction)?;
    let count = outbound_request_capacity_consumers(transaction)?;
    if count >= MAX_OUTBOUND_REQUEST_RECEIPTS {
        return Err(StoreError::OutboundRequestAuditCapacityExhausted);
    }
    Ok(())
}

/// Every receipt consumes the same bounded audit budget.
fn outbound_request_capacity_consumers(transaction: &Transaction<'_>) -> Result<i64, StoreError> {
    transaction
        .query_row(
            "SELECT COUNT(*) FROM outbound_request_receipts",
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::OutboundRequestReceipt)
}

fn prune_expired_stream_session_receipts(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    let cutoff_ms = outbound_request_clock_ms()?.saturating_sub(OUTBOUND_REQUEST_RETENTION_MS);
    transaction
        .execute(
            "DELETE FROM stream_session_receipts
             WHERE outcome <> 'started' AND completed_at_ms < ?1",
            params![cutoff_ms],
        )
        .map_err(StoreError::StreamSessionReceipt)?;
    Ok(())
}

fn ensure_stream_session_capacity(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    prune_expired_stream_session_receipts(transaction)?;
    let count: i64 = transaction
        .query_row("SELECT COUNT(*) FROM stream_session_receipts", [], |row| {
            row.get(0)
        })
        .map_err(StoreError::StreamSessionReceipt)?;
    if count >= MAX_OUTBOUND_REQUEST_RECEIPTS {
        return Err(StoreError::StreamSessionAuditCapacityExhausted);
    }
    Ok(())
}

fn invalid_outbound_request_receipt_value(index: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid outbound request receipt",
        )),
    )
}

fn receipt_from_row(row: &rusqlite::Row<'_>) -> Result<OutboundRequestReceipt, rusqlite::Error> {
    let correlation: String = row.get(1)?;
    let correlation_id =
        Uuid::parse_str(&correlation).map_err(|_| invalid_outbound_request_receipt_value(1))?;
    let transport: String = row.get(6)?;
    let operation: String = row.get(7)?;
    let safety_class: String = row.get(8)?;
    let precondition: String = row.get(9)?;
    let outcome: String = row.get(10)?;
    let http_status = row
        .get::<_, Option<i64>>(11)?
        .map(|value| u16::try_from(value).map_err(|_| invalid_outbound_request_receipt_value(11)))
        .transpose()?;
    let retry_after_seconds = row
        .get::<_, Option<i64>>(12)?
        .map(|value| u64::try_from(value).map_err(|_| invalid_outbound_request_receipt_value(12)))
        .transpose()?;
    Ok(OutboundRequestReceipt {
        id: OutboundRequestReceiptId(row.get(0)?),
        correlation_id,
        started_at_ms: row.get(2)?,
        completed_at_ms: row.get(3)?,
        duration_ms: row.get(4)?,
        vehicle_tesla_id: row.get(5)?,
        transport: OutboundRequestTransport::parse(&transport)
            .ok_or_else(|| invalid_outbound_request_receipt_value(6))?,
        operation: OutboundRequestOperation::parse(&operation)
            .ok_or_else(|| invalid_outbound_request_receipt_value(7))?,
        safety_class: OutboundRequestSafetyClass::parse(&safety_class)
            .ok_or_else(|| invalid_outbound_request_receipt_value(8))?,
        precondition: OutboundRequestPrecondition::parse(&precondition)
            .ok_or_else(|| invalid_outbound_request_receipt_value(9))?,
        outcome: if outcome == "started" {
            None
        } else {
            Some(
                OutboundRequestOutcome::parse(&outcome)
                    .ok_or_else(|| invalid_outbound_request_receipt_value(10))?,
            )
        },
        http_status,
        retry_after_seconds,
    })
}

fn cleanup_abandoned_import_generations(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute("DELETE FROM import_generations", [])
        .map_err(StoreError::ImportGeneration)?;
    Ok(())
}

fn require_positive_db(value: i64, field: &'static str) -> Result<(), StoreError> {
    if value <= 0 {
        Err(StoreError::InvalidLifecycleCarId)
    } else {
        let _ = field;
        Ok(())
    }
}
