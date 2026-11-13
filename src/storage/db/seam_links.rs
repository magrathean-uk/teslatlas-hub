// SPDX-License-Identifier: AGPL-3.0-only

/// Schema v80: the import seam's recorded decisions. One row per losing row of
/// a vehicle and entity: a skipped source id or a superseded Hub id, with the
/// row that won and the decision's sequence. A recorded decision survives time
/// corrections, relabels, restore and a new host; a superseded Hub id stays as
/// the Hub's high-water memory so it is never allocated again.
const IMPORT_SEAM_LINKS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS import_seam_links (
    vehicle_id TEXT NOT NULL,
    entity TEXT NOT NULL CHECK(entity IN (
        'drive', 'position', 'charge', 'charge_sample', 'state', 'update'
    )),
    loser_id INTEGER NOT NULL CHECK(loser_id > 0),
    winner_id INTEGER NOT NULL CHECK(winner_id > 0),
    decided_seq INTEGER NOT NULL CHECK(decided_seq >= 0),
    PRIMARY KEY(vehicle_id, entity, loser_id)
) STRICT;";

/// The recorded seam decisions of one vehicle and entity, loser id to winner
/// id.
fn seam_links_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_key: &str,
    entity: &str,
) -> Result<std::collections::BTreeMap<i64, i64>, StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT loser_id, winner_id FROM import_seam_links
              WHERE vehicle_id = ?1 AND entity = ?2 ORDER BY loser_id ASC",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_key, entity], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(StoreError::Query)?;
    rows.collect::<Result<_, _>>().map_err(StoreError::Query)
}

/// The inherited sessions of one vehicle and entity the Hub owns by a recorded
/// decision (M03-T18): self-links, whose stored copy stood over the source
/// copy of the same id.
fn seam_inherited_in_connection(
    connection: &Connection,
    vehicle_key: &str,
    entity: &str,
) -> Result<HashSet<i64>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT loser_id FROM import_seam_links
              WHERE vehicle_id = ?1 AND entity = ?2 AND loser_id = winner_id",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_key, entity], |row| row.get::<_, i64>(0))
        .map_err(StoreError::Query)?;
    rows.collect::<Result<_, _>>().map_err(StoreError::Query)
}

/// Records which inherited sessions the Hub owns by decision (M03-T18). An
/// inherited session (one id on both sides) whose stored, Hub-extended copy
/// the seam kept, and which no Hub-range child holds, would read as an earlier
/// import's row once the import lists it; the self-link keeps it the Hub's on
/// every later plan. A mark goes when the source copy covers and replaces the
/// row, or the row goes; a real link for the same id is never touched here.
fn record_seam_inherited_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_key: &str,
    entity: &str,
    kept: &[i64],
    released: &[i64],
) -> Result<(), StoreError> {
    for id in released {
        transaction
            .execute(
                "DELETE FROM import_seam_links
                  WHERE vehicle_id = ?1 AND entity = ?2 AND loser_id = ?3 AND winner_id = ?3",
                params![vehicle_key, entity, id],
            )
            .map_err(StoreError::LifecycleWrite)?;
    }
    if kept.is_empty() {
        return Ok(());
    }
    let next_seq: i64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(decided_seq), 0) + 1 FROM import_seam_links WHERE vehicle_id = ?1",
            params![vehicle_key],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    for id in kept {
        if *id <= 0 || crate::db::hub_ids::is_hub_id(*id) {
            return Err(StoreError::InvalidImportGeneration);
        }
        transaction
            .execute(
                "INSERT INTO import_seam_links(vehicle_id, entity, loser_id, winner_id, decided_seq)
                 VALUES (?1, ?2, ?3, ?3, ?4)
                 ON CONFLICT(vehicle_id, entity, loser_id) DO NOTHING",
                params![vehicle_key, entity, id, next_seq],
            )
            .map_err(StoreError::LifecycleWrite)?;
    }
    Ok(())
}

/// Writes one publication's seam decisions for a vehicle and entity: the
/// dropped links go, and each decided loser is linked to its winner. A link
/// that already names the same winner keeps its sequence; a new or changed
/// one takes the vehicle's next decision sequence, so `decided_seq` orders the
/// decisions as they were made.
fn record_seam_links_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_key: &str,
    entity: &str,
    links: &[crate::import::source_db::seam::SeamLink],
    dropped: &[i64],
) -> Result<(), StoreError> {
    for loser in dropped {
        transaction
            .execute(
                "DELETE FROM import_seam_links
                  WHERE vehicle_id = ?1 AND entity = ?2 AND loser_id = ?3",
                params![vehicle_key, entity, loser],
            )
            .map_err(StoreError::LifecycleWrite)?;
    }
    if links.is_empty() {
        return Ok(());
    }
    let next_seq: i64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(decided_seq), 0) + 1 FROM import_seam_links WHERE vehicle_id = ?1",
            params![vehicle_key],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    for link in links {
        if link.loser_id <= 0 || link.winner_id <= 0 || link.loser_id == link.winner_id {
            return Err(StoreError::InvalidImportGeneration);
        }
        transaction
            .execute(
                "INSERT INTO import_seam_links(vehicle_id, entity, loser_id, winner_id, decided_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(vehicle_id, entity, loser_id) DO UPDATE SET
                    winner_id = excluded.winner_id,
                    decided_seq = excluded.decided_seq
                 WHERE import_seam_links.winner_id IS NOT excluded.winner_id",
                params![vehicle_key, entity, link.loser_id, link.winner_id, next_seq],
            )
            .map_err(StoreError::LifecycleWrite)?;
    }
    Ok(())
}
