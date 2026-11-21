// SPDX-License-Identifier: AGPL-3.0-only

// The frozen final physical base of a vehicle (DEC-250, DEC-172; design
// DESIGN-STORAGE-SIZE section 3): one verified, published physical head whose
// pack objects every sweep keeps after the head stops being current, so the
// Hub can later publish from it without the kept raw stage. The record is one
// `hub_metadata` row per vehicle; no schema rung. A record that is damaged,
// ambiguous or not in its canonical form owns nothing it could name: every
// sweep that reads it refuses instead of guessing, so no pack is deleted.

/// Every `hub_metadata` row whose key starts with this belongs to the frozen
/// set, whatever follows (a damaged separator or vehicle id included).
const FROZEN_PHYSICAL_BASE_MEMBER_PREFIX: &str = "physical_v3_frozen_base";
/// The `hub_metadata` key prefix of a vehicle's frozen physical base.
const FROZEN_PHYSICAL_BASE_PREFIX: &str = "physical_v3_frozen_base:";
/// The record format this build writes and reads.
const FROZEN_PHYSICAL_BASE_FORMAT: u32 = 1;
/// The provenance rule of format 1: the five dimension tables are always
/// source rows, whatever their ids (the importer keeps the source's own ids
/// there, and a Hub-range id can appear); a row of a history table whose id
/// lies in the Hub's own range (`hub_ids`, schema v80) came from the Hub,
/// every other history row from the source import. See
/// [`frozen_row_is_hub_owned`].
pub(crate) const FROZEN_PHYSICAL_BASE_PROVENANCE_RULE: &str = "hub-id-range/v2";

#[cfg(test)]
std::thread_local! {
    /// A test's hook that runs between the pack scan and the final
    /// transaction of a freeze, where another writer can change the store.
    pub(crate) static FREEZE_AFTER_SCAN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// The five physical tables that hold source-owned dimension rows: the
/// import preserves their source ids and a publisher never replaces them.
const FROZEN_DIMENSION_TABLES: [&str; 5] =
    ["global_settings", "car_settings", "cars", "addresses", "geofences"];

/// Whether a frozen row of `table` with `id` is Hub-owned under
/// [`FROZEN_PHYSICAL_BASE_PROVENANCE_RULE`]: never for a dimension table, and
/// for the six history tables exactly when `id` is in the Hub's id range.
/// The one answer for the freeze scan, the strict reader and later
/// publishers.
pub(crate) fn frozen_row_is_hub_owned(table: &str, id: i64) -> bool {
    !FROZEN_DIMENSION_TABLES.contains(&table) && hub_ids::is_hub_id(id)
}

fn frozen_physical_base_key(vehicle_id: Uuid) -> String {
    format!("{FROZEN_PHYSICAL_BASE_PREFIX}{vehicle_id}")
}

/// A vehicle's frozen physical base: the exact head (its manifest, which
/// lists every pack object by digest, size and row count), the binding its
/// packs were written for, the logical digest of every decoded row, and the
/// provenance a later publisher needs to tell source rows from Hub rows.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenPhysicalBase {
    pub(crate) format: u32,
    pub(crate) vehicle_id: Uuid,
    pub(crate) snapshot_id: Uuid,
    pub(crate) head_sequence: u64,
    pub(crate) receipt_id: String,
    pub(crate) manifest_sha256: String,
    pub(crate) ordered_chunks_sha256: String,
    /// The binding the packs were written for (installation, account,
    /// vehicle, generation and source car id).
    pub(crate) binding: ProjectionBinding,
    /// The head's manifest as it was admitted.
    pub(crate) manifest: SyncManifest,
    /// [`frozen_physical_base_scan`]'s digest of every decoded row.
    pub(crate) logical: String,
    pub(crate) rows: u64,
    pub(crate) provenance: FrozenPhysicalBaseProvenance,
    pub(crate) frozen_at_ms: i64,
    /// [`frozen_physical_base_integrity`] over every other field, so a
    /// canonical one-digit change of any field is detected.
    pub(crate) integrity: String,
}

/// How the frozen rows divide between the source import and the Hub, and
/// which import and Hub inputs the head was published from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenPhysicalBaseProvenance {
    /// [`FROZEN_PHYSICAL_BASE_PROVENANCE_RULE`].
    pub(crate) rule: String,
    pub(crate) hub_id_base: i64,
    pub(crate) hub_id_max: i64,
    /// The import run whose revision the head was published for (issue #60),
    /// when one is recorded.
    pub(crate) import_run: Option<Uuid>,
    /// The vehicle's kept replay base at freeze: the source import's stage
    /// logical digest, its source sessions digest and archive generation.
    pub(crate) kept: Option<PhysicalV3Kept>,
    /// The recorded inputs digest of the planned head (M08-T19), which binds
    /// the binding, source span and the store's Hub rows it was planned with.
    pub(crate) handback_inputs: Option<String>,
    /// Per physical table, in pack table order: rows by the rule.
    pub(crate) tables: Vec<FrozenPhysicalBaseTable>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenPhysicalBaseTable {
    pub(crate) table: String,
    pub(crate) source_rows: u64,
    pub(crate) hub_rows: u64,
}

#[cfg_attr(not(test), allow(dead_code, reason = "T3b and T3c add the production callers"))]
/// Why a freeze, or a verification of a frozen base, refused.
#[derive(Debug, Error)]
pub(crate) enum FrozenPhysicalBaseRefusal {
    #[error("vehicle {0} has no physical head to freeze")]
    NoHead(Uuid),
    #[error("vehicle {0}'s physical head is a blocked rotation, not a public head")]
    NotPublic(Uuid),
    #[error(
        "vehicle {0}'s physical head is not served (withheld, restating, or planned with other \
         coverage)"
    )]
    NotServed(Uuid),
    #[error("vehicle {0} has a pending import publication")]
    ImportPublicationPending(Uuid),
    #[error("vehicle {vehicle_id}'s physical head does not verify: {reason}")]
    Unverified { vehicle_id: Uuid, reason: String },
    #[error("vehicle {vehicle_id} already has a frozen base of head {snapshot_id}")]
    AlreadyFrozen { vehicle_id: Uuid, snapshot_id: Uuid },
    #[error("vehicle {0} has no frozen base")]
    NotFrozen(Uuid),
    #[error("vehicle {0}'s physical head changed while it was frozen")]
    HeadMoved(Uuid),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[cfg_attr(not(test), allow(dead_code, reason = "T3b and T3c add the production callers"))]
/// What a scan of a head's packs found: the logical digest, the row count
/// and the per-table provenance counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrozenPhysicalBaseScan {
    pub(crate) logical: String,
    pub(crate) rows: u64,
    pub(crate) tables: Vec<FrozenPhysicalBaseTable>,
}

#[cfg_attr(not(test), allow(dead_code, reason = "T3b and T3c add the production callers"))]
/// Decodes every pack of `manifest` with the authenticated 2.2 decoder and
/// digests its rows: per pack in manifest order its ordinal and row count,
/// then per row (table order, id order) the table, id and row digest. The
/// digest depends on the decoded rows alone, not on pack bytes or names.
pub(crate) fn frozen_physical_base_scan(
    packs_dir: &Path,
    manifest: &SyncManifest,
    binding: &ProjectionBinding,
) -> Result<FrozenPhysicalBaseScan, crate::hub_pack::ProjectionPackError> {
    let mut hash = Sha256::new();
    hash.update(b"teslatlas-hub frozen physical base logical v1\n");
    let mut counts: Vec<(&'static str, u64, u64)> = crate::hub_pack::PHYSICAL_TABLES_2_2
        .iter()
        .map(|table| (*table, 0, 0))
        .collect();
    let mut rows = 0_u64;
    for pack in &manifest.chunks {
        hash.update(pack.ordinal.to_be_bytes());
        hash.update(pack.row_count.to_be_bytes());
        let path = packs_dir
            .join("sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        crate::hub_pack::scan_verified_physical_pack_2_2(
            pack,
            manifest,
            binding,
            &path,
            crate::hub_pack::PackCheck::Full,
            |row| {
                hash.update(row.table.as_bytes());
                hash.update([0]);
                hash.update(row.id.to_be_bytes());
                hash.update(row.digest);
                if let Some(count) = counts.iter_mut().find(|count| count.0 == row.table) {
                    if frozen_row_is_hub_owned(row.table, row.id) {
                        count.2 += 1;
                    } else {
                        count.1 += 1;
                    }
                }
                rows += 1;
                Ok(())
            },
        )?;
    }
    Ok(FrozenPhysicalBaseScan {
        logical: hex::encode(hash.finalize()),
        rows,
        tables: counts
            .into_iter()
            .map(|(table, source_rows, hub_rows)| FrozenPhysicalBaseTable {
                table: table.to_owned(),
                source_rows,
                hub_rows,
            })
            .collect(),
    })
}

/// The integrity value of a record: SHA-256 over the canonical serialisation
/// of every field but `integrity` itself (held empty while it is computed).
fn frozen_physical_base_integrity(base: &FrozenPhysicalBase) -> Result<String, serde_json::Error> {
    let mut bare = base.clone();
    bare.integrity = String::new();
    let mut hash = Sha256::new();
    hash.update(b"teslatlas-hub frozen physical base integrity v1\n");
    hash.update(serde_json::to_string(&bare)?.as_bytes());
    Ok(hex::encode(hash.finalize()))
}

fn canonical_sha256_hex(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Reads one record row strictly: the key must be the canonical spelling of
/// its vehicle, the value must parse, be in its canonical serialisation and
/// agree with itself (manifest digests, receipt and binding derived again).
/// Anything else is `FrozenPhysicalBaseInvalid`.
fn frozen_physical_base_from_row(key: &str, value: &str) -> Result<FrozenPhysicalBase, StoreError> {
    let invalid = || StoreError::FrozenPhysicalBaseInvalid { key: key.to_owned() };
    let vehicle_id = key
        .strip_prefix(FROZEN_PHYSICAL_BASE_PREFIX)
        .filter(|id| id.len() == 36)
        .and_then(|id| id.parse::<Uuid>().ok())
        .ok_or_else(invalid)?;
    if key != frozen_physical_base_key(vehicle_id) {
        return Err(invalid());
    }
    let base: FrozenPhysicalBase = serde_json::from_str(value).map_err(|_| invalid())?;
    if serde_json::to_string(&base).map_err(|_| invalid())? != value {
        return Err(invalid());
    }
    if frozen_physical_base_integrity(&base).map_err(|_| invalid())? != base.integrity {
        return Err(invalid());
    }
    let binding = &base.binding;
    if base.format != FROZEN_PHYSICAL_BASE_FORMAT
        || base.vehicle_id != vehicle_id
        || binding.vehicle_id != vehicle_id
        || base.manifest.generation != binding.generation
    {
        return Err(invalid());
    }
    validate_pending_physical_v3_manifest(&base.manifest).map_err(|_| invalid())?;
    let computed = physical_v3_admission_from_manifest(&base.manifest, binding.selected_car_id)
        .map_err(|_| invalid())?;
    if computed.vehicle_id != vehicle_id
        || computed.installation_id != binding.installation_id
        || computed.account_id != binding.account_id
        || computed.snapshot_id != base.snapshot_id
        || computed.head_sequence != base.head_sequence
        || computed.receipt_id != base.receipt_id
        || computed.manifest_sha256.to_string() != base.manifest_sha256
        || computed.ordered_chunks_sha256.to_string() != base.ordered_chunks_sha256
        || !physical_v3_admission_is_public(&computed)
    {
        return Err(invalid());
    }
    for pack in &base.manifest.chunks {
        if pack.relative_path != format!("/v1/packs/sha256/{}.sqlite.zst", pack.sha256)
            || i64::try_from(pack.compressed_bytes).is_err()
        {
            return Err(invalid());
        }
    }
    let provenance = &base.provenance;
    let tables_match = provenance.tables.len() == crate::hub_pack::PHYSICAL_TABLES_2_2.len()
        && provenance
            .tables
            .iter()
            .zip(crate::hub_pack::PHYSICAL_TABLES_2_2)
            .all(|(entry, table)| entry.table == table);
    let counted = provenance
        .tables
        .iter()
        .try_fold(0_u64, |sum, entry| sum.checked_add(entry.source_rows)?.checked_add(entry.hub_rows));
    let kept_canonical = provenance.kept.as_ref().is_none_or(|kept| {
        canonical_sha256_hex(&kept.stage)
            && canonical_sha256_hex(&kept.source)
            && kept.archive.as_ref().is_none_or(|archive| canonical_sha256_hex(&archive.digest))
    });
    if !canonical_sha256_hex(&base.logical)
        || provenance.rule != FROZEN_PHYSICAL_BASE_PROVENANCE_RULE
        || provenance.hub_id_base != hub_ids::HUB_ID_BASE
        || provenance.hub_id_max != hub_ids::HUB_ID_MAX
        || !tables_match
        || provenance
            .tables
            .iter()
            .any(|entry| FROZEN_DIMENSION_TABLES.contains(&entry.table.as_str()) && entry.hub_rows != 0)
        || counted != Some(base.rows)
        || !kept_canonical
        || provenance.handback_inputs.as_deref().is_some_and(|inputs| inputs.is_empty())
    {
        return Err(invalid());
    }
    Ok(base)
}

/// Every frozen physical base of `connection`'s catalogue, by key. One record
/// that does not read back refuses the whole set: a sweep cannot tell which
/// packs it would have named.
pub(crate) fn frozen_physical_bases_in(
    connection: &Connection,
) -> Result<Vec<FrozenPhysicalBase>, StoreError> {
    let rows = connection
        .prepare(
            // Membership never depends on the bytes that identify the
            // vehicle: any key that starts with the stem, in any case, is a
            // frozen record, and one that is not the canonical key refuses
            // the set below.
            "SELECT key, value FROM hub_metadata
              WHERE lower(substr(key, 1, length(?1))) = ?1
              ORDER BY key",
        )
        .map_err(StoreError::Query)?
        .query_map(params![FROZEN_PHYSICAL_BASE_MEMBER_PREFIX], |row| {
            // The value is read as raw bytes whatever its storage class: a
            // TEXT value that is not UTF-8 (or a BLOB or NULL) is a damaged
            // record (keyed below), not a query failure.
            let value = match row.get_ref(1)? {
                rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) => {
                    bytes.to_vec()
                }
                _ => Vec::new(),
            };
            Ok((row.get::<_, String>(0)?, value))
        })
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)?;
    rows.iter()
        .map(|(key, value)| {
            let value = std::str::from_utf8(value)
                .map_err(|_| StoreError::FrozenPhysicalBaseInvalid { key: key.clone() })?;
            frozen_physical_base_from_row(key, value)
        })
        .collect()
}

/// The pack rows (digest, relative path, compressed bytes) every frozen base
/// names, for the shared reference set of every sweep and backup.
fn frozen_physical_base_pack_rows(
    connection: &Connection,
) -> Result<Vec<(String, String, i64)>, StoreError> {
    let mut rows = Vec::new();
    for base in frozen_physical_bases_in(connection)? {
        for pack in &base.manifest.chunks {
            rows.push((
                pack.sha256.to_string(),
                pack.relative_path.clone(),
                i64::try_from(pack.compressed_bytes).map_err(|_| StoreError::PackSizeTooLarge)?,
            ));
        }
    }
    Ok(rows)
}

#[cfg_attr(not(test), allow(dead_code, reason = "T3b and T3c add the production callers"))]
fn frozen_physical_base_in(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<Option<FrozenPhysicalBase>, StoreError> {
    Ok(frozen_physical_bases_in(connection)?
        .into_iter()
        .find(|base| base.vehicle_id == vehicle_id))
}

#[cfg_attr(not(test), allow(dead_code, reason = "T3b and T3c add the production callers"))]
impl HubStore {
    /// The vehicle's frozen physical base, if any. Any record that does not
    /// read back is refused.
    pub(crate) fn frozen_physical_v3_base(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<FrozenPhysicalBase>, StoreError> {
        frozen_physical_base_in(&self.open_read_only_connection()?, vehicle_id)
    }

    /// Freezes the vehicle's current physical head into its frozen-base
    /// record. Refuses when the vehicle has no head, the head is a blocked
    /// rotation, the head is not served (withheld, restating, a committed
    /// import not yet published, coverage unreconciled or changed since the
    /// plan), an import publication is pending, any pack does not verify
    /// (file digest at admission, then the authenticated decoder with a full
    /// integrity check), or another head is frozen already. Freezing the head
    /// that is frozen returns the record unchanged.
    pub(crate) fn freeze_physical_v3_base(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        frozen_at_ms: i64,
    ) -> Result<FrozenPhysicalBase, FrozenPhysicalBaseRefusal> {
        let unverified = |reason: String| FrozenPhysicalBaseRefusal::Unverified { vehicle_id, reason };
        {
            let connection = self.open_read_only_connection()?;
            if import_publication_holds_in(&connection)?.pending(vehicle_id) {
                return Err(FrozenPhysicalBaseRefusal::ImportPublicationPending(vehicle_id));
            }
        }
        let admission = match self
            .pending_physical_v3_admission_for_vehicle_with_file_digests(vehicle_id, true, true)
        {
            Ok(Some(admission)) => admission,
            Ok(None) => return Err(FrozenPhysicalBaseRefusal::NoHead(vehicle_id)),
            Err(StoreError::PhysicalV3SecondHeadUnsupported(_)) => {
                return Err(FrozenPhysicalBaseRefusal::NotPublic(vehicle_id));
            }
            Err(error) => return Err(unverified(error.to_string())),
        };
        if let Some(existing) = self.frozen_physical_v3_base(vehicle_id)? {
            if existing.snapshot_id == admission.snapshot_id
                && existing.receipt_id == admission.receipt_id
            {
                // A retry is answered only while the head is still served.
                if !self.physical_v3_admission_served(vehicle_id, admission.snapshot_id)? {
                    return Err(FrozenPhysicalBaseRefusal::NotServed(vehicle_id));
                }
                return Ok(existing);
            }
            return Err(FrozenPhysicalBaseRefusal::AlreadyFrozen {
                vehicle_id,
                snapshot_id: existing.snapshot_id,
            });
        }
        if !physical_v3_admission_is_public(&admission) {
            return Err(FrozenPhysicalBaseRefusal::NotPublic(vehicle_id));
        }
        if !self.physical_v3_admission_served(vehicle_id, admission.snapshot_id)? {
            return Err(FrozenPhysicalBaseRefusal::NotServed(vehicle_id));
        }
        let binding = ProjectionBinding {
            installation_id: admission.installation_id,
            account_id: admission.account_id,
            vehicle_id,
            generation: admission.manifest.generation,
            selected_car_id: admission.selected_car_id,
        };
        let scan = frozen_physical_base_scan(&self.packs_dir, &admission.manifest, &binding)
            .map_err(|error| unverified(error.to_string()))?;
        #[cfg(test)]
        FREEZE_AFTER_SCAN.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        let import_run = {
            let connection = self.open_read_only_connection()?;
            physical_v3_import_revision_in(&connection, vehicle_id)
                .map_err(|_| unverified("the import revision does not read back".into()))?
                .map(|revision| revision.run_id)
        };
        let kept = self
            .physical_v3_kept(vehicle_id)
            .map_err(|_| unverified("the kept record does not read back".into()))?;
        let handback_inputs = self
            .physical_v3_handback_plan(vehicle_id)?
            .filter(|plan| plan.snapshot_id == admission.snapshot_id.to_string())
            .map(|plan| plan.inputs);
        let mut base = FrozenPhysicalBase {
            format: FROZEN_PHYSICAL_BASE_FORMAT,
            vehicle_id,
            snapshot_id: admission.snapshot_id,
            head_sequence: admission.head_sequence,
            receipt_id: admission.receipt_id.clone(),
            manifest_sha256: admission.manifest_sha256.to_string(),
            ordered_chunks_sha256: admission.ordered_chunks_sha256.to_string(),
            binding,
            manifest: admission.manifest.clone(),
            logical: scan.logical,
            rows: scan.rows,
            provenance: FrozenPhysicalBaseProvenance {
                rule: FROZEN_PHYSICAL_BASE_PROVENANCE_RULE.to_owned(),
                hub_id_base: hub_ids::HUB_ID_BASE,
                hub_id_max: hub_ids::HUB_ID_MAX,
                import_run,
                kept,
                handback_inputs,
                tables: scan.tables,
            },
            frozen_at_ms,
            integrity: String::new(),
        };
        base.integrity =
            frozen_physical_base_integrity(&base).map_err(StoreError::SerializeLifecycleRow)?;
        let value = serde_json::to_string(&base).map_err(StoreError::SerializeLifecycleRow)?;
        // The record must read back as written before it may own anything.
        frozen_physical_base_from_row(&frozen_physical_base_key(vehicle_id), &value)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Query)?;
        if import_publication_holds_in(&transaction)?.pending(vehicle_id) {
            return Err(FrozenPhysicalBaseRefusal::ImportPublicationPending(vehicle_id));
        }
        let current: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT snapshot_id, receipt_id, serve_state
                   FROM pending_physical_v3_admissions WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        if current
            != Some((
                admission.snapshot_id.to_string(),
                admission.receipt_id.clone(),
                "public_first".to_owned(),
            ))
        {
            return Err(FrozenPhysicalBaseRefusal::HeadMoved(vehicle_id));
        }
        if frozen_physical_base_in(&transaction, vehicle_id)?.is_some() {
            return Err(FrozenPhysicalBaseRefusal::HeadMoved(vehicle_id));
        }
        // The scan above ran outside this transaction; the window controller
        // writes closed coverage without the publication gate, so the head
        // must still be served in the state this insert commits with.
        if !self.physical_v3_admission_served_in(&transaction, vehicle_id, admission.snapshot_id)? {
            return Err(FrozenPhysicalBaseRefusal::NotServed(vehicle_id));
        }
        transaction
            .execute(
                "INSERT INTO hub_metadata(key, value) VALUES(?1, ?2)",
                params![frozen_physical_base_key(vehicle_id), value],
            )
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(base)
    }

    /// Decodes the vehicle's frozen packs with the authenticated decoder and
    /// checks them against the record: logical digest, row count and the
    /// per-table provenance counts. Returns the record.
    pub(crate) fn verify_frozen_physical_v3_base(
        &self,
        vehicle_id: Uuid,
    ) -> Result<FrozenPhysicalBase, FrozenPhysicalBaseRefusal> {
        let base = self
            .frozen_physical_v3_base(vehicle_id)?
            .ok_or(FrozenPhysicalBaseRefusal::NotFrozen(vehicle_id))?;
        let scan = frozen_physical_base_scan(&self.packs_dir, &base.manifest, &base.binding)
            .map_err(|error| FrozenPhysicalBaseRefusal::Unverified {
                vehicle_id,
                reason: error.to_string(),
            })?;
        if scan.logical != base.logical
            || scan.rows != base.rows
            || scan.tables != base.provenance.tables
        {
            return Err(FrozenPhysicalBaseRefusal::Unverified {
                vehicle_id,
                reason: "the decoded rows differ from the frozen record".into(),
            });
        }
        Ok(base)
    }
}
