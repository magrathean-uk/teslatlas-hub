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
/// it never committed, goes with it.
fn drop_uncommitted_import_publication_intent_in(
    connection: &Connection,
    run_id: Uuid,
) -> Result<(), StoreError> {
    for (vehicle_id, intent) in import_publication_intents_in(connection)? {
        if intent.is_some_and(|intent| intent.run_id == run_id && !intent.committed) {
            connection
                .execute(
                    "DELETE FROM hub_metadata WHERE key = ?1",
                    params![import_publication_intent_key(vehicle_id)],
                )
                .map_err(StoreError::Query)?;
        }
    }
    Ok(())
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

impl HubStore {
    /// Every recorded import publication intent, by vehicle; one that does
    /// not read back is `None`.
    pub(crate) fn import_publication_intents(
        &self,
    ) -> Result<Vec<(Uuid, Option<ImportPublicationIntent>)>, StoreError> {
        import_publication_intents_in(&self.open_read_only_connection()?)
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

    /// Records `intent` for its still-staging import generation, replacing
    /// any earlier intent of the vehicle: the caller resolved that one first.
    pub(crate) fn record_import_publication_intent(
        &self,
        _publication_gate: &PublicationGate,
        intent: &ImportPublicationIntent,
    ) -> Result<(), StoreError> {
        if intent.committed {
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
        let value = serde_json::to_string(intent).map_err(StoreError::SerializeLifecycleRow)?;
        put_metadata_in(&transaction, &import_publication_intent_key(vehicle_id), &value)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)
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
