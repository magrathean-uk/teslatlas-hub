// SPDX-License-Identifier: AGPL-3.0-only

// An import's publication intent and the import revision its physical head
// must carry (issue #60). Before an import's first catalogue commit it writes
// a private capture (its raw physical stage as a kept archive, its source
// sessions and its schema-2.2 source facts) and records an intent naming it.
// The commit that consumes the import's generation marks that intent
// committed and makes the run the vehicle's import revision in the same
// transaction, so every earlier physical head is refused from that commit
// on. A physical head is served again only once a publication stamps it
// with that revision. A restart finishes a committed intent from its capture
// alone; an uncommitted one is dropped.

/// What an import recorded before its first commit, so a restart can finish
/// its schema-2.2 and physical publication without the source (issue #60).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ImportPublicationIntent {
    /// The import generation whose commit makes this capture current.
    pub(crate) run_id: Uuid,
    pub(crate) binding: ProjectionBinding,
    /// The capture's kept pair: the raw physical stage as a kept archive
    /// and the source sessions, as the vehicle's kept record will name them.
    pub(crate) kept: PhysicalV3Kept,
    /// SHA-256 of the captured schema-2.2 source facts.
    pub(crate) updates: String,
    /// Set by the commit that consumed `run_id`.
    #[serde(default)]
    pub(crate) committed: bool,
    /// The vehicle's committed intent this still-staging run would replace
    /// (issue #60 review, finding 4): it stays live until this run commits,
    /// and an abort, or a restart before that commit, puts it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) superseding: Option<Box<ImportPublicationIntent>>,
}

/// The vehicle's import revision (issue #60): the last committed import with
/// a publication intent, the kept generation adopted for it, and the physical
/// head published for it. While a revision is recorded only `head` is served.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PhysicalV3ImportRevision {
    pub(crate) run_id: Uuid,
    #[serde(default)]
    pub(crate) kept: Option<Uuid>,
    #[serde(default)]
    pub(crate) head: Option<Uuid>,
}

const IMPORT_PUBLICATION_INTENT_PREFIX: &str = "import_publication_intent:";

fn import_publication_intent_key(vehicle_id: Uuid) -> String {
    format!("{IMPORT_PUBLICATION_INTENT_PREFIX}{vehicle_id}")
}

fn physical_v3_import_revision_key(vehicle_id: Uuid) -> String {
    format!("physical_v3_import_revision:{vehicle_id}")
}

fn metadata_value_in(connection: &Connection, key: &str) -> Result<Option<String>, StoreError> {
    connection
        .query_row(
            "SELECT value FROM hub_metadata WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::Query)
}

fn put_metadata_in(connection: &Connection, key: &str, value: &str) -> Result<(), StoreError> {
    connection
        .execute(
            "INSERT INTO hub_metadata(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(StoreError::Query)?;
    Ok(())
}

/// Every recorded intent, by vehicle; one that does not read back is `None`.
fn import_publication_intents_in(
    connection: &Connection,
) -> Result<Vec<(Uuid, Option<ImportPublicationIntent>)>, StoreError> {
    let mut statement = connection
        .prepare("SELECT key, value FROM hub_metadata WHERE key LIKE 'import_publication_intent:%' ORDER BY key")
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)?;
    Ok(rows
        .into_iter()
        .filter_map(|(key, value)| {
            let vehicle_id = key.strip_prefix(IMPORT_PUBLICATION_INTENT_PREFIX)?.parse().ok()?;
            Some((vehicle_id, serde_json::from_str(&value).ok()))
        })
        .collect())
}

fn physical_v3_import_revision_in(
    connection: &Connection,
    vehicle_id: Uuid,
) -> Result<Option<PhysicalV3ImportRevision>, StoreError> {
    metadata_value_in(connection, &physical_v3_import_revision_key(vehicle_id))?
        .map(|value| {
            serde_json::from_str(&value).map_err(|_| StoreError::PhysicalV3AdmissionConflict)
        })
        .transpose()
}

fn put_physical_v3_import_revision_in(
    connection: &Connection,
    vehicle_id: Uuid,
    revision: &PhysicalV3ImportRevision,
) -> Result<(), StoreError> {
    let value = serde_json::to_string(revision).map_err(StoreError::SerializeLifecycleRow)?;
    put_metadata_in(connection, &physical_v3_import_revision_key(vehicle_id), &value)
}

/// In the transaction that consumes import generation `run_id`: an intent
/// naming the run is marked committed and the run becomes its vehicle's
/// import revision, so every earlier physical head is stale from this commit.
fn commit_import_publication_revision_in(
    connection: &Connection,
    run_id: Uuid,
) -> Result<(), StoreError> {
    for (vehicle_id, intent) in import_publication_intents_in(connection)? {
        let Some(mut intent) = intent.filter(|intent| intent.run_id == run_id) else {
            continue;
        };
        intent.committed = true;
        intent.superseding = None;
        let value = serde_json::to_string(&intent).map_err(StoreError::SerializeLifecycleRow)?;
        put_metadata_in(connection, &import_publication_intent_key(vehicle_id), &value)?;
        put_physical_v3_import_revision_in(
            connection,
            vehicle_id,
            &PhysicalV3ImportRevision {
                run_id,
                kept: None,
                head: None,
            },
        )?;
    }
    Ok(())
}

/// In the transaction that aborts import generation `run_id`: its intent, if
/// it never committed, goes with it, and the committed intent it would have
/// replaced is put back.
fn drop_uncommitted_import_publication_intent_in(
    connection: &Connection,
    run_id: Uuid,
) -> Result<(), StoreError> {
    for (vehicle_id, intent) in import_publication_intents_in(connection)? {
        if let Some(intent) = intent.filter(|intent| intent.run_id == run_id && !intent.committed) {
            restore_superseded_import_publication_intent_in(connection, vehicle_id, intent)?;
        }
    }
    Ok(())
}

/// Replaces the vehicle's uncommitted `intent` with the committed intent it
/// superseded, or removes it when it superseded none. Returns the intent put
/// back.
fn restore_superseded_import_publication_intent_in(
    connection: &Connection,
    vehicle_id: Uuid,
    intent: ImportPublicationIntent,
) -> Result<Option<ImportPublicationIntent>, StoreError> {
    let key = import_publication_intent_key(vehicle_id);
    match intent.superseding {
        Some(previous) => {
            let value =
                serde_json::to_string(&previous).map_err(StoreError::SerializeLifecycleRow)?;
            put_metadata_in(connection, &key, &value)?;
            Ok(Some(*previous))
        }
        None => {
            connection
                .execute("DELETE FROM hub_metadata WHERE key = ?1", params![key])
                .map_err(StoreError::Query)?;
            Ok(None)
        }
    }
}

/// Whether the vehicle's import revision admits `snapshot_id` (issue #60):
/// with no revision recorded every head is; with one, only the head
/// published for it. A revision that does not read back admits none.
fn physical_v3_import_revision_serves_in(
    connection: &Connection,
    vehicle_id: Uuid,
    snapshot_id: Uuid,
) -> bool {
    match physical_v3_import_revision_in(connection, vehicle_id) {
        Ok(None) => true,
        Ok(Some(revision)) => revision.head == Some(snapshot_id),
        Err(_) => false,
    }
}

/// The files a pending import publication still needs (issue #60): by
/// vehicle and capture generation (after adoption the archive and source
/// sessions keep that generation in the kept directory), and the vehicles
/// whose intent does not read back, every capture and kept file of which is
/// held. An intent row that names no vehicle holds every file.
#[derive(Debug, Default)]
pub(crate) struct ImportPublicationHolds {
    generations: std::collections::BTreeSet<(Uuid, Uuid)>,
    vehicles: std::collections::BTreeSet<Uuid>,
    all: bool,
}

impl ImportPublicationHolds {
    /// Whether every file of `vehicle_id` is held.
    pub(crate) fn holds_vehicle(&self, vehicle_id: Uuid) -> bool {
        self.all || self.vehicles.contains(&vehicle_id)
    }

    /// Whether the files of capture `generation` of `vehicle_id` are held.
    pub(crate) fn holds(&self, vehicle_id: Uuid, generation: Uuid) -> bool {
        self.holds_vehicle(vehicle_id) || self.generations.contains(&(vehicle_id, generation))
    }

    /// Whether any publication of `vehicle_id` is pending.
    pub(crate) fn pending(&self, vehicle_id: Uuid) -> bool {
        self.holds_vehicle(vehicle_id)
            || self.generations.iter().any(|(vehicle, _)| *vehicle == vehicle_id)
    }

    fn hold(&mut self, vehicle_id: Uuid, intent: &ImportPublicationIntent) {
        match &intent.kept.archive {
            Some(archive) => {
                self.generations.insert((vehicle_id, archive.generation));
            }
            None => {
                self.vehicles.insert(vehicle_id);
            }
        }
        if let Some(previous) = &intent.superseding {
            self.hold(vehicle_id, previous);
        }
    }
}

/// What every pending import publication of `connection`'s catalogue holds.
pub(crate) fn import_publication_holds_in(
    connection: &Connection,
) -> Result<ImportPublicationHolds, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT key, value FROM hub_metadata WHERE key LIKE 'import_publication_intent:%' \
             ORDER BY key",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)?;
    let mut holds = ImportPublicationHolds::default();
    for (key, value) in rows {
        let Some(vehicle_id) = key
            .strip_prefix(IMPORT_PUBLICATION_INTENT_PREFIX)
            .filter(|id| id.len() == 36)
            .and_then(|id| id.parse::<Uuid>().ok())
        else {
            holds.all = true;
            continue;
        };
        match serde_json::from_str::<ImportPublicationIntent>(&value) {
            Ok(intent) => holds.hold(vehicle_id, &intent),
            Err(_) => {
                holds.vehicles.insert(vehicle_id);
            }
        }
    }
    Ok(holds)
}

/// Refuses a new backup while an import's publication is pending (issue #60):
/// an intent its commit made current, or one that does not read back, names
/// a capture under `packs/import-publication` that no backup carries, so a
/// restored Hub could neither finish that publication nor serve the older
/// head. An intent of a run that never committed changes no route and is
/// dropped at the restored Hub's next start.
pub(crate) fn require_no_pending_import_publication_in(
    connection: &Connection,
) -> Result<(), StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT key, value FROM hub_metadata WHERE key LIKE 'import_publication_intent:%' \
             ORDER BY key",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)?;
    for (key, value) in rows {
        let staging_only = matches!(
            serde_json::from_str::<ImportPublicationIntent>(&value),
            Ok(intent) if !intent.committed && intent.superseding.is_none()
        );
        if !staging_only {
            let vehicle_id = key
                .strip_prefix(IMPORT_PUBLICATION_INTENT_PREFIX)
                .and_then(|id| id.parse().ok())
                .unwrap_or_else(Uuid::nil);
            return Err(StoreError::BackupImportPublicationPending(vehicle_id));
        }
    }
    Ok(())
}

impl HubStore {
    /// Every recorded import publication intent, by vehicle; one that does
    /// not read back is `None`.
    pub(crate) fn import_publication_intents(
        &self,
    ) -> Result<Vec<(Uuid, Option<ImportPublicationIntent>)>, StoreError> {
        import_publication_intents_in(&self.open_read_only_connection()?)
    }

    /// What every pending import publication holds back from the sweeps.
    pub(crate) fn import_publication_holds(&self) -> Result<ImportPublicationHolds, StoreError> {
        import_publication_holds_in(&self.open_read_only_connection()?)
    }

    /// The vehicle's recorded intent, if any; one that does not read back
    /// is refused.
    pub(crate) fn import_publication_intent(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<ImportPublicationIntent>, StoreError> {
        metadata_value_in(
            &self.open_read_only_connection()?,
            &import_publication_intent_key(vehicle_id),
        )?
        .map(|value| {
            serde_json::from_str(&value).map_err(|_| StoreError::PhysicalV3AdmissionConflict)
        })
        .transpose()
    }

    /// Records `intent` for its still-staging import generation. A committed
    /// intent of the vehicle that is still recorded (its publication was
    /// refused) is kept inside it until this run commits; any other earlier
    /// intent is replaced: the caller resolved it first.
    pub(crate) fn record_import_publication_intent(
        &self,
        _publication_gate: &PublicationGate,
        intent: &ImportPublicationIntent,
    ) -> Result<(), StoreError> {
        if intent.committed || intent.superseding.is_some() {
            return Err(StoreError::ImportGenerationConflict);
        }
        let vehicle_id = intent.binding.vehicle_id;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let staging: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM import_generations
                                WHERE run_id = ?1 AND vehicle_id = ?2 AND status = 'staging')",
                params![intent.run_id.to_string(), vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        if !staging {
            return Err(StoreError::ImportGenerationNotFound);
        }
        let key = import_publication_intent_key(vehicle_id);
        let earlier = metadata_value_in(&transaction, &key)?
            .and_then(|value| serde_json::from_str::<ImportPublicationIntent>(&value).ok());
        let superseding = earlier.and_then(|earlier| {
            if earlier.committed {
                Some(Box::new(earlier))
            } else {
                earlier.superseding
            }
        });
        let recorded = ImportPublicationIntent {
            superseding,
            ..intent.clone()
        };
        let value = serde_json::to_string(&recorded).map_err(StoreError::SerializeLifecycleRow)?;
        put_metadata_in(&transaction, &key, &value)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)
    }

    /// For an intent whose run never committed: puts back the committed
    /// intent it superseded, or removes it. Returns the intent put back,
    /// which the caller then resolves.
    pub(crate) fn restore_superseded_import_publication_intent(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        run_id: Uuid,
    ) -> Result<Option<ImportPublicationIntent>, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let Some(intent) =
            metadata_value_in(&transaction, &import_publication_intent_key(vehicle_id))?
                .and_then(|value| serde_json::from_str::<ImportPublicationIntent>(&value).ok())
                .filter(|intent| intent.run_id == run_id && !intent.committed)
        else {
            return Ok(None);
        };
        let restored = restore_superseded_import_publication_intent_in(&transaction, vehicle_id, intent)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(restored)
    }

    /// Clears the vehicle's intent once its outcome is resolved: only the
    /// intent of `run_id`, or with `None` whatever is recorded (an intent
    /// that does not read back). Returns whether one was cleared.
    pub(crate) fn clear_import_publication_intent(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        run_id: Option<Uuid>,
    ) -> Result<bool, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let key = import_publication_intent_key(vehicle_id);
        let Some(value) = metadata_value_in(&transaction, &key)? else {
            return Ok(false);
        };
        if let Some(run_id) = run_id {
            let named = serde_json::from_str::<ImportPublicationIntent>(&value)
                .is_ok_and(|intent| intent.run_id == run_id);
            if !named {
                return Ok(false);
            }
        }
        transaction
            .execute("DELETE FROM hub_metadata WHERE key = ?1", params![key])
            .map_err(StoreError::Query)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(true)
    }

    /// The vehicle's import revision, if one is recorded.
    pub(crate) fn physical_v3_import_revision(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<PhysicalV3ImportRevision>, StoreError> {
        physical_v3_import_revision_in(&self.open_read_only_connection()?, vehicle_id)
    }

    /// Makes a committed import's capture the vehicle's kept record in one
    /// transaction: only while the record is `expected`, clearing the
    /// recorded plan inputs, and noting the kept generation on the import
    /// revision while that revision is still `run_id`. Returns whether the
    /// record was replaced (or already was `kept`).
    pub(crate) fn adopt_import_publication_kept(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        expected: Option<&PhysicalV3Kept>,
        kept: &PhysicalV3Kept,
        run_id: Uuid,
    ) -> Result<bool, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let current = metadata_value_in(&transaction, &physical_v3_kept_key(vehicle_id))?
            .map(|value| {
                serde_json::from_str::<PhysicalV3Kept>(&value)
                    .map_err(|_| StoreError::PhysicalV3AdmissionConflict)
            })
            .transpose()?;
        if current.as_ref() != Some(kept) {
            if current.as_ref() != expected {
                return Ok(false);
            }
            let value = serde_json::to_string(kept).map_err(StoreError::SerializeLifecycleRow)?;
            put_metadata_in(&transaction, &physical_v3_kept_key(vehicle_id), &value)?;
            transaction
                .execute(
                    "DELETE FROM hub_metadata WHERE key = ?1",
                    params![physical_v3_handback_plan_key(vehicle_id)],
                )
                .map_err(StoreError::Query)?;
        }
        if let Some(mut revision) = physical_v3_import_revision_in(&transaction, vehicle_id)?
            && revision.run_id == run_id
        {
            revision.kept = kept.archive.as_ref().map(|archive| archive.generation);
            put_physical_v3_import_revision_in(&transaction, vehicle_id, &revision)?;
        }
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(true)
    }

    /// Stamps `snapshot_id` as the physical head of import revision `run_id`,
    /// before it is published, while that is still the vehicle's revision.
    /// Returns whether it was stamped.
    pub(crate) fn stamp_physical_v3_import_head(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        snapshot_id: Uuid,
        run_id: Uuid,
    ) -> Result<bool, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let Some(mut revision) = physical_v3_import_revision_in(&transaction, vehicle_id)? else {
            return Ok(false);
        };
        if revision.run_id != run_id {
            return Ok(false);
        }
        revision.head = Some(snapshot_id);
        put_physical_v3_import_revision_in(&transaction, vehicle_id, &revision)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(true)
    }
}
