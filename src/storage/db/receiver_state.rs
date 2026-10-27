// SPDX-License-Identifier: AGPL-3.0-only

fn migration_clock_ms() -> Result<i64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::MigrationClock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| StoreError::MigrationClock)
}

impl HubStore {
    /// The stored complete accumulator state of one vehicle, if any.
    pub(crate) fn load_receiver_state(
        &self,
        vehicle_id: Uuid,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT state_json FROM fleet_telemetry_accumulator_states
                  WHERE vehicle_id = ?1 AND state_version = 2",
                params![vehicle_id.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StoreError::ReceiverState)
    }

    /// Upsert one vehicle's complete accumulator state (version 2) inside the
    /// caller's transaction.
    pub(crate) fn put_receiver_state_in_transaction(
        transaction: &Transaction<'_>,
        vehicle_id: Uuid,
        state_json: &[u8],
        updated_at_ms: i64,
    ) -> Result<(), StoreError> {
        validate_timestamp("receiver state updated_at_ms", updated_at_ms)?;
        transaction
            .execute(
                "INSERT INTO fleet_telemetry_accumulator_states(
                    vehicle_id, state_version, state_json, updated_at_ms)
                 VALUES (?1, 2, ?2, ?3)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    state_version = 2,
                    state_json = excluded.state_json,
                    updated_at_ms = excluded.updated_at_ms",
                params![vehicle_id.to_string(), state_json, updated_at_ms],
            )
            .map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }
}
