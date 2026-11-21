// SPDX-License-Identifier: AGPL-3.0-only

// Issues #98 and #109: finite offers. A pack named in a signed control
// response (a physical manifest, a 1.4 changed set, a rebase replacement or a
// prepared-map receipt) stays fetchable by the device it was offered to for a
// bounded lease after the head rotates. Discovery stays current-only: a lease
// never makes an old object advertised, it only lets that one device finish
// fetching exactly the digest it was given. Each fetch rechecks the device,
// its vehicle grant, the source generation and the lease window; fetching
// never extends a lease. The lease rows are part of the pack reference set,
// so reclaim, repair and backup keep a leased object exactly while its lease
// lives (plus the shared delete grace).

/// The table of offer leases, added at the end of v83 like the backup pins.
const DEVICE_PACK_OFFERS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS device_pack_offers (
    device_id TEXT NOT NULL REFERENCES paired_devices(device_id) ON DELETE CASCADE,
    vehicle_id TEXT NOT NULL CHECK(length(vehicle_id) = 36),
    sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
    kind TEXT NOT NULL CHECK(kind IN ('physical', 'delta', 'prepared')),
    generation INTEGER NOT NULL CHECK(generation BETWEEN 1 AND 9007199254740991),
    compressed_bytes INTEGER NOT NULL CHECK(compressed_bytes BETWEEN 1 AND 16777216),
    offered_at_ms INTEGER NOT NULL CHECK(offered_at_ms BETWEEN 0 AND 9007199254740991),
    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms BETWEEN 1 AND 9007199254740991),
    CHECK(expires_at_ms > offered_at_ms),
    PRIMARY KEY(device_id, vehicle_id, sha256)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS device_pack_offers_digest ON device_pack_offers(sha256);
CREATE INDEX IF NOT EXISTS device_pack_offers_expiry ON device_pack_offers(expires_at_ms);";

/// Live leases one device may hold. A full head has at most 1771 chunks and a
/// changed set 64, so this covers several rotations a day; a new offer past
/// it is refused before its signed response is sent, never by evicting an
/// unexpired lease.
const MAX_PACK_OFFERS_PER_DEVICE: i64 = 8_192;
/// Live leases across the store.
const MAX_PACK_OFFERS_PER_STORE: i64 = 65_536;

/// The lease predicate shared by the fetch lookup, the reference set and the
/// cleanup: the device is not revoked, it still holds the vehicle grant, the
/// vehicle is active and its source generation is the offered one.
macro_rules! live_pack_offer_predicate {
    () => {
        "EXISTS (SELECT 1 FROM paired_devices AS device
                  WHERE device.device_id = offer.device_id AND device.revoked_at_ms IS NULL)
         AND EXISTS (SELECT 1 FROM device_vehicle_grants AS grants
                      WHERE grants.device_id = offer.device_id
                        AND grants.vehicle_id = offer.vehicle_id)
         AND EXISTS (SELECT 1 FROM vehicles AS vehicle
                       JOIN sources AS source ON source.source_id = vehicle.source_id
                      WHERE vehicle.vehicle_id = offer.vehicle_id
                        AND vehicle.retired_at_ms IS NULL
                        AND source.generation = offer.generation)"
    };
}

/// Whether the lease table exists. A v83 store that has not had its first
/// upgraded writable start lacks it; the read-only paths (doctor, census)
/// read that as an empty lease set (issue #98 review finding 5).
fn device_pack_offers_present(connection: &Connection) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema
             WHERE type = 'table' AND name = 'device_pack_offers')",
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)
}

fn device_pack_offers_table(connection: &Connection) -> Result<(), StoreError> {
    let present = |connection: &Connection| -> Result<bool, StoreError> {
        device_pack_offers_present(connection).map_err(|error| match error {
            StoreError::Query(error) => StoreError::Migrate(error),
            other => other,
        })
    };
    if present(connection)? {
        return Ok(());
    }
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(StoreError::Migrate)?;
    admit_writable_catalogue(&transaction)?;
    if schema_version(&transaction)? == SCHEMA_VERSION && !present(&transaction)? {
        transaction
            .execute_batch(DEVICE_PACK_OFFERS_SCHEMA)
            .map_err(StoreError::Migrate)?;
    }
    transaction.commit().map_err(StoreError::Migrate)
}

/// What an offered digest is, which decides its media type when fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PackOfferKind {
    /// A chunk of a full physical head (manifest, rebase replacement).
    Physical,
    /// A pack of a 1.4 physical changed set.
    Delta,
    /// The pack a prepared-map receipt names.
    Prepared,
}

impl PackOfferKind {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Physical => "physical",
            Self::Delta => "delta",
            Self::Prepared => "prepared",
        }
    }

    fn from_sql(value: &str) -> Option<Self> {
        match value {
            "physical" => Some(Self::Physical),
            "delta" => Some(Self::Delta),
            "prepared" => Some(Self::Prepared),
            _ => None,
        }
    }

    /// The catalogue rows of the selected head that may name an offered
    /// digest of this kind (`?1` digest, `?2` vehicle, `?3` the head's
    /// receipt): a chunk of that head, a pack of the changed set into it, or
    /// a prepared month built from it.
    fn catalogued_sql(self) -> &'static str {
        match self {
            Self::Physical => {
                "SELECT pack.compressed_bytes FROM pending_physical_v3_packs AS pack
                   JOIN pending_physical_v3_admissions AS head
                     ON head.snapshot_id = pack.snapshot_id
                  WHERE pack.sha256 = ?1 AND head.vehicle_id = ?2 AND head.receipt_id = ?3
                  LIMIT 1"
            }
            Self::Delta => {
                "SELECT pack.compressed_bytes FROM physical_v3_delta_packs AS pack
                   JOIN physical_v3_delta_transitions AS delta
                     ON delta.target_receipt_id = pack.target_receipt_id
                  WHERE pack.sha256 = ?1 AND delta.vehicle_id = ?2
                    AND delta.target_receipt_id = ?3
                  LIMIT 1"
            }
            Self::Prepared => {
                "SELECT compressed_bytes FROM prepared_map_months
                  WHERE pack_sha256 = ?1 AND vehicle_id = ?2 AND input_receipt_id = ?3
                  LIMIT 1"
            }
        }
    }
}

/// Whether an offer was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PackOfferRecord {
    /// The leases are committed.
    Recorded,
    /// The selected head is no longer the vehicle's served head (a rotation,
    /// a generation change or an issue #60 import revision committed since it
    /// was selected): nothing was recorded, and the caller selects again.
    HeadMoved,
}

/// One digest a control response is about to offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PackOffer {
    pub(crate) digest: Sha256Digest,
    pub(crate) kind: PackOfferKind,
}

impl HubStore {
    /// Commit a lease for each offered digest before the signed response that
    /// names them is returned. `head_receipt_id` is the head the response was
    /// selected from; the write binds the offer to it: if it is no longer the
    /// vehicle's public head under the source's generation and the issue #60
    /// import revision, nothing is recorded and the answer is `HeadMoved`.
    /// Refused (and nothing recorded) when the device is revoked or
    /// ungranted, a digest is not catalogued for that head, or the lease caps
    /// are reached. Re-offering a digest while it is current keeps the later
    /// expiry; an old digest is never offered again, so its deadline cannot
    /// roll forward. Already issued leases are never touched here.
    pub(crate) fn record_pack_offers_at(
        &self,
        device_id: Uuid,
        vehicle_id: Uuid,
        head_receipt_id: &str,
        offers: &[PackOffer],
        now_ms: i64,
    ) -> Result<PackOfferRecord, StoreError> {
        if offers.is_empty() {
            return Ok(PackOfferRecord::Recorded);
        }
        let expires_at_ms = now_ms
            .checked_add(RETIRED_LINEAGE_PACK_RETENTION_MS)
            .filter(|expires| now_ms >= 0 && *expires <= 9_007_199_254_740_991)
            .ok_or(StoreError::PhysicalV3RetentionInvalid)?;
        let device = device_id.to_string();
        let vehicle = vehicle_id.to_string();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        delete_expired_pack_offers(
            &transaction,
            now_ms.saturating_sub(RETIRED_LINEAGE_PACK_DELETE_GRACE_MS),
        )?;
        // The selected head must still be the served head, read inside this
        // write so no rotation or import revision can commit in between.
        let head: Option<(String, i64)> = transaction
            .query_row(
                "SELECT head.snapshot_id, source.generation
                   FROM pending_physical_v3_admissions AS head
                   JOIN vehicles AS vehicle ON vehicle.vehicle_id = head.vehicle_id
                   JOIN sources AS source ON source.source_id = vehicle.source_id
                  WHERE head.vehicle_id = ?1 AND head.receipt_id = ?2
                    AND head.serve_state = 'public_first' AND vehicle.retired_at_ms IS NULL
                    AND json_extract(CAST(head.manifest_json AS TEXT), '$.generation')
                        = source.generation",
                params![vehicle, head_receipt_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let head = head.and_then(|(snapshot, generation)| {
            let snapshot = snapshot.parse::<Uuid>().ok()?;
            physical_v3_import_revision_serves_in(&transaction, vehicle_id, snapshot)
                .then_some(generation)
        });
        let Some(generation) = head else {
            tracing::info!(%vehicle_id, "pack offer refused: the selected head is no longer served");
            return Ok(PackOfferRecord::HeadMoved);
        };
        let bound: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM paired_devices AS device
                                WHERE device.device_id = ?1 AND device.revoked_at_ms IS NULL)
                    AND EXISTS(SELECT 1 FROM device_vehicle_grants
                                WHERE device_id = ?1 AND vehicle_id = ?2)",
                params![device, vehicle],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        if !bound {
            tracing::info!(%vehicle_id, "pack offer refused: the device or its grant moved");
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        for offer in offers {
            let digest = offer.digest.to_string();
            let bytes: Option<i64> = transaction
                .query_row(
                    offer.kind.catalogued_sql(),
                    params![digest, vehicle, head_receipt_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::Query)?;
            let Some(bytes) = bytes.filter(|bytes| (1..=16_777_216).contains(bytes)) else {
                tracing::info!(%vehicle_id, "pack offer refused: the object left the catalogue");
                return Err(StoreError::PhysicalV3AdmissionConflict);
            };
            transaction
                .execute(
                    "INSERT INTO device_pack_offers(device_id, vehicle_id, sha256, kind,
                         generation, compressed_bytes, offered_at_ms, expires_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(device_id, vehicle_id, sha256) DO UPDATE SET
                         kind = excluded.kind,
                         compressed_bytes = excluded.compressed_bytes,
                         offered_at_ms = CASE WHEN generation = excluded.generation
                             THEN min(offered_at_ms, excluded.offered_at_ms)
                             ELSE excluded.offered_at_ms END,
                         expires_at_ms = CASE WHEN generation = excluded.generation
                             THEN max(expires_at_ms, excluded.expires_at_ms)
                             ELSE excluded.expires_at_ms END,
                         generation = excluded.generation",
                    params![
                        device,
                        vehicle,
                        digest,
                        offer.kind.as_sql(),
                        generation,
                        bytes,
                        now_ms,
                        expires_at_ms
                    ],
                )
                .map_err(StoreError::Query)?;
        }
        let (device_count, store_count): (i64, i64) = transaction
            .query_row(
                "SELECT (SELECT count(*) FROM device_pack_offers WHERE device_id = ?1),
                        (SELECT count(*) FROM device_pack_offers)",
                [&device],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(StoreError::Query)?;
        if device_count > MAX_PACK_OFFERS_PER_DEVICE || store_count > MAX_PACK_OFFERS_PER_STORE {
            tracing::warn!(%vehicle_id, "pack offer refused: the offer lease cap is reached");
            return Err(StoreError::PhysicalV3AdmissionConflict);
        }
        transaction.commit().map_err(StoreError::Query)?;
        Ok(PackOfferRecord::Recorded)
    }

    /// The object a live lease of `device_id` names, for the pack route once
    /// every current lookup has missed. `None` for an unknown, unoffered,
    /// expired, revoked, ungranted or other-generation digest alike.
    pub(crate) fn offered_pack_for_digest_at(
        &self,
        device_id: Uuid,
        digest: Sha256Digest,
        now_ms: i64,
    ) -> Result<Option<(StoredPack, PackOfferKind)>, StoreError> {
        let row: Option<(String, i64)> = self
            .open_read_only_connection()?
            .query_row(
                concat!(
                    "SELECT offer.kind, offer.compressed_bytes FROM device_pack_offers AS offer
                      WHERE offer.device_id = ?1 AND offer.sha256 = ?2
                        AND offer.offered_at_ms <= ?3 AND offer.expires_at_ms > ?3 AND ",
                    live_pack_offer_predicate!(),
                    " ORDER BY offer.expires_at_ms DESC LIMIT 1"
                ),
                params![device_id.to_string(), digest.to_string(), now_ms],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((kind, bytes)) = row else {
            return Ok(None);
        };
        let kind = PackOfferKind::from_sql(&kind).ok_or(StoreError::PhysicalV3AdmissionConflict)?;
        let bytes = u64::try_from(bytes)
            .ok()
            .filter(|bytes| (1..=16_777_216).contains(bytes))
            .ok_or(StoreError::PhysicalV3AdmissionConflict)?;
        Ok(Some((
            StoredPack {
                digest,
                compressed_bytes: bytes,
                path: self.packs_dir.join("sha256").join(format!("{digest}.sqlite.zst")),
            },
            kind,
        )))
    }
}

/// The objects live leases keep, for the shared pack reference set: a lease
/// that expired before `expiry_cutoff_ms`, or whose device, grant or
/// generation moved, keeps nothing.
fn pack_offer_rows(
    connection: &Connection,
    expiry_cutoff_ms: i64,
) -> Result<Vec<(String, String, i64)>, StoreError> {
    if !device_pack_offers_present(connection)? {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare(concat!(
            "SELECT DISTINCT offer.sha256, offer.compressed_bytes FROM device_pack_offers AS offer
              WHERE offer.expires_at_ms > ?1 AND ",
            live_pack_offer_predicate!()
        ))
        .map_err(StoreError::Query)?;
    statement
        .query_map([expiry_cutoff_ms], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(StoreError::Query)?
        .map(|row| {
            let (digest, bytes) = row.map_err(StoreError::Query)?;
            let parsed = digest
                .parse::<Sha256Digest>()
                .map_err(|_| StoreError::LineageCatalogConflict)?;
            if !(1..=16 * 1024 * 1024).contains(&bytes) {
                return Err(StoreError::LineageCatalogConflict);
            }
            Ok((digest, format!("/v1/packs/sha256/{parsed}.sqlite.zst"), bytes))
        })
        .collect()
}

/// Remove the leases that expired before `cutoff_ms` or no longer live, with
/// the other retention rows, after the sweep's file removal.
fn delete_expired_pack_offers(
    transaction: &Transaction<'_>,
    cutoff_ms: i64,
) -> Result<(), StoreError> {
    transaction
        .execute(
            concat!(
                "DELETE FROM device_pack_offers AS offer
                  WHERE offer.expires_at_ms <= ?1 OR NOT (",
                live_pack_offer_predicate!(),
                ")"
            ),
            params![cutoff_ms],
        )
        .map_err(StoreError::LineageCatalog)?;
    Ok(())
}
