// SPDX-License-Identifier: AGPL-3.0-only

// The geocoder's provider-state row (C7 R60). It is one row of the existing
// `hub_metadata` key-value table, so it needs no schema step and travels with
// backups. These two accessors read and write exactly this row and nothing
// else; the geocoder module owns the text, validates it and treats a value it
// cannot read as absent. This file is `include!`d into `db.rs`.

/// C7 K-STATE-KEY: the `hub_metadata` key of the provider-state row.
pub(crate) const GEOCODER_PROVIDER_STATE_KEY: &str = "geocoder_provider_state";

impl HubStore {
    /// The stored provider-state text, or `None` when there is no row. It opens
    /// the catalogue read-only and writes nothing.
    pub(crate) fn geocoder_provider_state(&self) -> Result<Option<String>, StoreError> {
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = ?1",
                params![GEOCODER_PROVIDER_STATE_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::Query)
    }

    /// Replace the provider-state row with `value` in one statement, or delete it
    /// when `value` is `None`.
    pub(crate) fn set_geocoder_provider_state(
        &self,
        value: Option<&str>,
    ) -> Result<(), StoreError> {
        let connection = self.open()?;
        match value {
            Some(value) => connection.execute(
                "INSERT INTO hub_metadata(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![GEOCODER_PROVIDER_STATE_KEY, value],
            ),
            None => connection.execute(
                "DELETE FROM hub_metadata WHERE key = ?1",
                params![GEOCODER_PROVIDER_STATE_KEY],
            ),
        }
        .map(|_| ())
        .map_err(StoreError::Query)
    }
}
