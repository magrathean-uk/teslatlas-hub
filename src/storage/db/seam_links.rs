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
