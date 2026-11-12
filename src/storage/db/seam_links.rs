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
