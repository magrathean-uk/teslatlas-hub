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

macro_rules! legacy_owner_tokens_table {
    () => {
        "legacy_owner_tokens"
    };
}

macro_rules! import_state_spool_alias {
    () => {
        "import_state_spool"
    };
}

pub(crate) use import_projection_heads_table;
pub(crate) use import_projection_rows_table;
pub(crate) use import_projection_state_bridges_table;
pub(crate) use import_projection_state_heads_table;
pub(crate) use import_projection_state_rows_table;
pub(crate) use import_state_spool_alias;
pub(crate) use legacy_owner_tokens_table;
