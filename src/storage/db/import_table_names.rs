// SPDX-License-Identifier: AGPL-3.0-only

//! Static SQL names shared by runtime statements and the forward migration.

macro_rules! import_projection_heads_table {
    () => {
        "import_projection_heads"
    };
}

macro_rules! import_projection_rows_table {
    () => {
        "import_projection_rows"
    };
}

macro_rules! import_projection_state_heads_table {
    () => {
        "import_projection_state_heads"
    };
}

macro_rules! import_projection_state_rows_table {
    () => {
        "import_projection_state_rows"
    };
}

macro_rules! import_projection_state_bridges_table {
    () => {
        "import_projection_state_bridges"
    };
}

macro_rules! import_state_spool_alias {
    () => {
        "import_state_spool"
    };
}

/// The SQL expression naming the legacy inventory's entity text of a state row's
/// ordinal (schema v81, M08-T30). The legacy inventory keeps its text key.
macro_rules! import_state_entity_name_sql {
    ($ordinal:literal) => {
        concat!(
            "CASE ",
            $ordinal,
            " WHEN 0 THEN 'car'
                 WHEN 1 THEN 'drive'
                 WHEN 2 THEN 'position'
                 WHEN 3 THEN 'charge'
                 WHEN 4 THEN 'charge_sample'
                 WHEN 5 THEN 'state'
                 WHEN 6 THEN 'update'
             END"
        )
    };
}

/// The SQL expression naming the ordinal of a legacy inventory entity text.
macro_rules! import_state_entity_ordinal_sql {
    ($name:literal) => {
        concat!(
            "CASE ",
            $name,
            " WHEN 'car' THEN 0
                 WHEN 'drive' THEN 1
                 WHEN 'position' THEN 2
                 WHEN 'charge' THEN 3
                 WHEN 'charge_sample' THEN 4
                 WHEN 'state' THEN 5
                 WHEN 'update' THEN 6
             END"
        )
    };
}

pub(crate) use import_projection_heads_table;
pub(crate) use import_projection_rows_table;
pub(crate) use import_projection_state_bridges_table;
pub(crate) use import_projection_state_heads_table;
pub(crate) use import_projection_state_rows_table;
pub(crate) use import_state_entity_name_sql;
pub(crate) use import_state_entity_ordinal_sql;
pub(crate) use import_state_spool_alias;
