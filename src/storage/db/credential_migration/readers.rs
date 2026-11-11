// SPDX-License-Identifier: AGPL-3.0-only

pub(crate) struct PreUpgradeRefreshBinding {
    pub(crate) receipt_id: OutboundRequestReceiptId,
    pub(crate) attempt_id: Uuid,
    pub(crate) input_generation: Uuid,
    pub(crate) output_generation: Option<Uuid>,
    pub(crate) receipt_state: PreUpgradeReceiptState,
    pub(crate) started_at_ms: i64,
}

impl std::fmt::Debug for PreUpgradeRefreshBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreUpgradeRefreshBinding")
            .field("receipt_id", &self.receipt_id)
            .field("attempt_id", &"<redacted>")
            .field("input_generation", &"<redacted>")
            .field("output_generation", &"<redacted>")
            .field("receipt_state", &self.receipt_state)
            .field("started_at_ms", &self.started_at_ms)
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PreUpgradeReceiptState {
    Started,
    Completed(OutboundRequestOutcome),
}

const CREDENTIAL_PRE_UPGRADE_LIMIT: i64 = 100_000;
const CREDENTIAL_PRE_UPGRADE_CIPHERTEXT_LIMIT: i64 = 16_424;
const CREDENTIAL_PRE_UPGRADE_TABLES: [&str; 6] = [
    "legacy_owner_tokens",
    "fleet_tokens",
    "legacy_refresh_receipt_bindings",
    "legacy_refresh_input_fences",
    "fleet_refresh_receipt_bindings",
    "fleet_refresh_input_fences",
];

// Test-only observations distinguish scalar count refusal from row materialisation.
#[cfg(test)]
std::thread_local! {
    static CREDENTIAL_PRE_UPGRADE_DECODE_VISITS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
fn credential_pre_upgrade_reset_decode_visits() {
    CREDENTIAL_PRE_UPGRADE_DECODE_VISITS.with(|visits| visits.set(0));
}

#[cfg(test)]
fn credential_pre_upgrade_decode_visits() -> usize {
    CREDENTIAL_PRE_UPGRADE_DECODE_VISITS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn credential_pre_upgrade_record_decode_visit() {
    CREDENTIAL_PRE_UPGRADE_DECODE_VISITS.with(|visits| {
        visits.set(visits.get().saturating_add(1));
    });
}

struct CredentialPreUpgradeColumn {
    name: &'static str,
    declared_type: &'static str,
    required: bool,
    primary_key: i64,
}

const fn credential_pre_upgrade_column(
    name: &'static str,
    declared_type: &'static str,
    required: bool,
    primary_key: i64,
) -> CredentialPreUpgradeColumn {
    CredentialPreUpgradeColumn {
        name,
        declared_type,
        required,
        primary_key,
    }
}

fn credential_pre_upgrade_get<T: rusqlite::types::FromSql>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> Result<T, StoreError> {
    row.get(index)
        .map_err(|_| StoreError::CredentialPairInvalid)
}

fn credential_pre_upgrade_type(
    row: &rusqlite::Row<'_>,
    index: usize,
    expected: &str,
) -> Result<(), StoreError> {
    let actual: String = credential_pre_upgrade_get(row, index)?;
    if actual != expected {
        return Err(StoreError::CredentialPairInvalid);
    }
    Ok(())
}

fn credential_pre_upgrade_uuid(value: &str) -> Result<Uuid, StoreError> {
    if value.len() != 36 {
        return Err(StoreError::CredentialPairInvalid);
    }
    Uuid::parse_str(value).map_err(|_| StoreError::CredentialPairInvalid)
}

fn credential_pre_upgrade_metadata(
    connection: &Connection,
    table: &str,
    without_rowid: bool,
    expected: &[CredentialPreUpgradeColumn],
) -> Result<(), StoreError> {
    let (wr, strict): (i64, i64) = connection
        .query_row(
            "SELECT wr, strict FROM pragma_table_list
             WHERE schema='main' AND name=?1 COLLATE NOCASE",
            [table],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(StoreError::CredentialPairStore)?;
    if strict != 1 || wr != i64::from(without_rowid) {
        return Err(StoreError::CredentialPairInvalid);
    }
    let mut statement = connection
        .prepare(
            "SELECT length(CAST(name AS BLOB)), length(CAST(type AS BLOB)),
                    name, type, \"notnull\", pk, hidden
             FROM pragma_table_xinfo(?1, 'main')",
        )
        .map_err(StoreError::CredentialPairStore)?;
    let mut rows = statement
        .query([table])
        .map_err(StoreError::CredentialPairStore)?;
    let mut seen = std::collections::HashSet::new();
    while let Some(row) = rows.next().map_err(StoreError::CredentialPairStore)? {
        let name_length: i64 = credential_pre_upgrade_get(row, 0)?;
        let type_length: i64 = credential_pre_upgrade_get(row, 1)?;
        if !(1..=64).contains(&name_length) || !(1..=32).contains(&type_length) {
            return Err(StoreError::CredentialPairInvalid);
        }
        let name: String = credential_pre_upgrade_get(row, 2)?;
        let declared_type: String = credential_pre_upgrade_get(row, 3)?;
        let not_null: i64 = credential_pre_upgrade_get(row, 4)?;
        let primary_key: i64 = credential_pre_upgrade_get(row, 5)?;
        let hidden: i64 = credential_pre_upgrade_get(row, 6)?;
        let expected_column = expected
            .iter()
            .find(|column| column.name.eq_ignore_ascii_case(&name))
            .ok_or(StoreError::CredentialPairInvalid)?;
        // SQLite reports raw notnull=0 for a normal INTEGER PRIMARY KEY even
        // though its persisted rowid value is necessarily non-null.
        let implicit_not_null =
            !without_rowid && primary_key == 1 && declared_type.eq_ignore_ascii_case("INTEGER");
        let effective_not_null = not_null == 1 || implicit_not_null;
        if hidden != 0
            || !matches!(not_null, 0 | 1)
            || !declared_type.eq_ignore_ascii_case(expected_column.declared_type)
            || primary_key != expected_column.primary_key
            || effective_not_null != expected_column.required
            || !seen.insert(name.to_ascii_lowercase())
        {
            return Err(StoreError::CredentialPairInvalid);
        }
    }
    if seen.len() != expected.len() {
        return Err(StoreError::CredentialPairInvalid);
    }
    Ok(())
}

/// The A2 floor preflight. This only reads main-schema metadata, before the rung
/// creates C10 tables or infers occupancy. A proper subset is never normalised.
fn credential_pre_upgrade_presence(connection: &Connection) -> Result<bool, StoreError> {
    let mut found = 0;
    for table in CREDENTIAL_PRE_UPGRADE_TABLES {
        let mut statement = connection
            // Discover aliases too, but accept only the same case-exact stored
            // name used by the rung's occupancy check (M2P-73(b), A2).
            .prepare(
                "SELECT type, name=?1 COLLATE BINARY FROM main.sqlite_schema
                 WHERE name=?1 COLLATE NOCASE",
            )
            .map_err(StoreError::CredentialPairStore)?;
        let mut rows = statement
            .query([table])
            .map_err(StoreError::CredentialPairStore)?;
        if let Some(row) = rows.next().map_err(StoreError::CredentialPairStore)? {
            let object_type: String = credential_pre_upgrade_get(row, 0)?;
            let exact_name: bool = credential_pre_upgrade_get(row, 1)?;
            if !exact_name
                || object_type != "table"
                || rows
                    .next()
                    .map_err(StoreError::CredentialPairStore)?
                    .is_some()
            {
                return Err(StoreError::CredentialPairInvalid);
            }
            found += 1;
        }
    }
    if found == 0 {
        return Ok(false);
    }
    if found != CREDENTIAL_PRE_UPGRADE_TABLES.len() {
        return Err(StoreError::CredentialPairInvalid);
    }
    let owner = [
        credential_pre_upgrade_column("singleton_id", "INTEGER", true, 1),
        credential_pre_upgrade_column("access", "BLOB", true, 0),
        credential_pre_upgrade_column("refresh", "BLOB", true, 0),
        credential_pre_upgrade_column("expires_at", "INTEGER", true, 0),
        credential_pre_upgrade_column("next_refresh_at", "INTEGER", true, 0),
        credential_pre_upgrade_column("credential_generation", "TEXT", false, 0),
    ];
    let fleet = [
        credential_pre_upgrade_column("singleton_id", "INTEGER", true, 1),
        credential_pre_upgrade_column("access", "BLOB", true, 0),
        credential_pre_upgrade_column("refresh", "BLOB", true, 0),
        credential_pre_upgrade_column("client_id", "TEXT", true, 0),
        credential_pre_upgrade_column("region", "TEXT", true, 0),
        credential_pre_upgrade_column("expires_at", "INTEGER", true, 0),
        credential_pre_upgrade_column("next_refresh_at", "INTEGER", true, 0),
        credential_pre_upgrade_column("credential_generation", "TEXT", false, 0),
    ];
    let bindings = [
        credential_pre_upgrade_column("receipt_id", "INTEGER", true, 1),
        credential_pre_upgrade_column("attempt_id", "TEXT", true, 0),
        credential_pre_upgrade_column("input_credential_generation", "TEXT", true, 0),
        credential_pre_upgrade_column("output_credential_generation", "TEXT", false, 0),
    ];
    let fences = [credential_pre_upgrade_column(
        "input_credential_generation",
        "TEXT",
        true,
        1,
    )];
    credential_pre_upgrade_metadata(connection, CREDENTIAL_PRE_UPGRADE_TABLES[0], false, &owner)?;
    credential_pre_upgrade_metadata(connection, CREDENTIAL_PRE_UPGRADE_TABLES[1], false, &fleet)?;
    credential_pre_upgrade_metadata(
        connection,
        CREDENTIAL_PRE_UPGRADE_TABLES[2],
        false,
        &bindings,
    )?;
    credential_pre_upgrade_metadata(connection, CREDENTIAL_PRE_UPGRADE_TABLES[3], true, &fences)?;
    credential_pre_upgrade_metadata(
        connection,
        CREDENTIAL_PRE_UPGRADE_TABLES[4],
        false,
        &bindings,
    )?;
    credential_pre_upgrade_metadata(connection, CREDENTIAL_PRE_UPGRADE_TABLES[5], true, &fences)?;
    Ok(true)
}

fn credential_pre_upgrade_tables(
    slot: CredentialSlot,
) -> (&'static str, &'static str, &'static str) {
    match slot {
        CredentialSlot::OwnerApi => (
            CREDENTIAL_PRE_UPGRADE_TABLES[0],
            CREDENTIAL_PRE_UPGRADE_TABLES[2],
            CREDENTIAL_PRE_UPGRADE_TABLES[3],
        ),
        CredentialSlot::FleetApi => (
            CREDENTIAL_PRE_UPGRADE_TABLES[1],
            CREDENTIAL_PRE_UPGRADE_TABLES[4],
            CREDENTIAL_PRE_UPGRADE_TABLES[5],
        ),
    }
}

fn credential_pre_upgrade_count(
    connection: &Connection,
    table: &str,
    history: bool,
) -> Result<i64, StoreError> {
    let result = connection.query_row(&format!("SELECT count(*) FROM main.{table}"), [], |row| {
        row.get::<_, i64>(0)
    });
    if history {
        result.map_err(StoreError::CredentialRefreshReceipt)
    } else {
        result.map_err(StoreError::CredentialPairStore)
    }
}

fn credential_pre_upgrade_history_bounds(connection: &Connection) -> Result<(), StoreError> {
    let owner = credential_pre_upgrade_count(connection, CREDENTIAL_PRE_UPGRADE_TABLES[2], true)?;
    let fleet = credential_pre_upgrade_count(connection, CREDENTIAL_PRE_UPGRADE_TABLES[4], true)?;
    let owner_fences =
        credential_pre_upgrade_count(connection, CREDENTIAL_PRE_UPGRADE_TABLES[3], true)?;
    let fleet_fences =
        credential_pre_upgrade_count(connection, CREDENTIAL_PRE_UPGRADE_TABLES[5], true)?;
    if [owner, fleet, owner_fences, fleet_fences]
        .iter()
        .any(|count| !(0..=CREDENTIAL_PRE_UPGRADE_LIMIT).contains(count))
        || owner
            .checked_add(fleet)
            .is_none_or(|count| count > CREDENTIAL_PRE_UPGRADE_LIMIT)
    {
        return Err(StoreError::CredentialPairInvalid);
    }
    let collision: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM main.legacy_refresh_receipt_bindings AS owner
             JOIN main.fleet_refresh_receipt_bindings AS fleet
             ON owner.receipt_id=fleet.receipt_id)",
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::CredentialRefreshReceipt)?;
    if collision {
        return Err(StoreError::CredentialPairInvalid);
    }
    Ok(())
}

struct CredentialPreUpgradeRowMetadata {
    expires_at: i64,
    next_refresh_at: i64,
    generation: Option<Uuid>,
    client_id: Option<String>,
    region: Option<String>,
}

/// No ciphertext column is selected by this metadata read.
fn credential_pre_upgrade_row_metadata_on(
    connection: &Connection,
    slot: CredentialSlot,
) -> Result<Option<CredentialPreUpgradeRowMetadata>, StoreError> {
    let (table, _, _) = credential_pre_upgrade_tables(slot);
    let count = credential_pre_upgrade_count(connection, table, false)?;
    if count == 0 {
        return Ok(None);
    }
    if count != 1 {
        return Err(StoreError::CredentialPairInvalid);
    }
    let suffix = if slot == CredentialSlot::FleetApi {
        ", typeof(client_id), length(CAST(client_id AS BLOB)), client_id,
           typeof(region), length(CAST(region AS BLOB)), region"
    } else {
        ""
    };
    let sql = format!(
        "SELECT singleton_id, typeof(singleton_id), typeof(access), length(access),
                typeof(refresh), length(refresh), typeof(expires_at), expires_at,
                typeof(next_refresh_at), next_refresh_at, typeof(credential_generation),
                length(CAST(credential_generation AS BLOB)), credential_generation{suffix}
         FROM main.{table}"
    );
    let mut statement = connection
        .prepare(&sql)
        .map_err(StoreError::CredentialPairStore)?;
    let mut rows = statement
        .query([])
        .map_err(StoreError::CredentialPairStore)?;
    let row = rows
        .next()
        .map_err(StoreError::CredentialPairStore)?
        .ok_or(StoreError::CredentialPairInvalid)?;
    credential_pre_upgrade_type(row, 1, "integer")?;
    credential_pre_upgrade_type(row, 2, "blob")?;
    credential_pre_upgrade_type(row, 4, "blob")?;
    credential_pre_upgrade_type(row, 6, "integer")?;
    credential_pre_upgrade_type(row, 8, "integer")?;
    let singleton: i64 = credential_pre_upgrade_get(row, 0)?;
    let access_length: i64 = credential_pre_upgrade_get(row, 3)?;
    let refresh_length: i64 = credential_pre_upgrade_get(row, 5)?;
    let expires_at: i64 = credential_pre_upgrade_get(row, 7)?;
    let next_refresh_at: i64 = credential_pre_upgrade_get(row, 9)?;
    if singleton != 1
        || !(1..=CREDENTIAL_PRE_UPGRADE_CIPHERTEXT_LIMIT).contains(&access_length)
        || !(1..=CREDENTIAL_PRE_UPGRADE_CIPHERTEXT_LIMIT).contains(&refresh_length)
        || !(expires_at > next_refresh_at && next_refresh_at > 0
            || slot == CredentialSlot::OwnerApi && expires_at == 0 && next_refresh_at == 0)
    {
        return Err(StoreError::CredentialPairInvalid);
    }
    let generation_type: String = credential_pre_upgrade_get(row, 10)?;
    let generation_length: Option<i64> = credential_pre_upgrade_get(row, 11)?;
    let generation = match (generation_type.as_str(), generation_length) {
        ("null", None) => None,
        ("text", Some(36)) => {
            let text: String = credential_pre_upgrade_get(row, 12)?;
            Some(credential_pre_upgrade_uuid(&text)?)
        }
        _ => return Err(StoreError::CredentialPairInvalid),
    };
    let (client_id, region) = if slot == CredentialSlot::FleetApi {
        credential_pre_upgrade_type(row, 13, "text")?;
        credential_pre_upgrade_type(row, 16, "text")?;
        let client_length: i64 = credential_pre_upgrade_get(row, 14)?;
        let region_length: i64 = credential_pre_upgrade_get(row, 17)?;
        if !(1..=255).contains(&client_length) || region_length != 2 {
            return Err(StoreError::CredentialPairInvalid);
        }
        let client_id: String = credential_pre_upgrade_get(row, 15)?;
        let region: String = credential_pre_upgrade_get(row, 18)?;
        if !matches!(region.as_str(), "na" | "eu" | "cn") {
            return Err(StoreError::CredentialPairInvalid);
        }
        (Some(client_id), Some(region))
    } else {
        (None, None)
    };
    Ok(Some(CredentialPreUpgradeRowMetadata {
        expires_at,
        next_refresh_at,
        generation,
        client_id,
        region,
    }))
}

// Validate the required receipt schema without scanning unrelated receipt rows.
// Additional unrelated receipt columns are not a new refusal policy.
fn credential_pre_upgrade_receipt_metadata(connection: &Connection) -> Result<(), StoreError> {
    let kind: Option<String> = connection
        .query_row(
            "SELECT type FROM main.sqlite_schema WHERE name='outbound_request_receipts' COLLATE NOCASE",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::CredentialPairStore)?;
    if kind.as_deref() != Some("table") {
        return Err(StoreError::CredentialPairInvalid);
    }
    let (wr, strict): (i64, i64) = connection
        .query_row(
            "SELECT wr, strict FROM pragma_table_list
             WHERE schema='main' AND name='outbound_request_receipts' COLLATE NOCASE",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(StoreError::CredentialPairStore)?;
    if wr != 0 || strict != 1 {
        return Err(StoreError::CredentialPairInvalid);
    }
    let expected = [
        credential_pre_upgrade_column("id", "INTEGER", true, 1),
        credential_pre_upgrade_column("correlation_id", "TEXT", true, 0),
        credential_pre_upgrade_column("started_at_ms", "INTEGER", true, 0),
        credential_pre_upgrade_column("completed_at_ms", "INTEGER", false, 0),
        credential_pre_upgrade_column("duration_ms", "INTEGER", false, 0),
        credential_pre_upgrade_column("vehicle_tesla_id", "INTEGER", false, 0),
        credential_pre_upgrade_column("transport", "TEXT", true, 0),
        credential_pre_upgrade_column("operation", "TEXT", true, 0),
        credential_pre_upgrade_column("safety_class", "TEXT", true, 0),
        credential_pre_upgrade_column("precondition", "TEXT", true, 0),
        credential_pre_upgrade_column("outcome", "TEXT", true, 0),
        credential_pre_upgrade_column("http_status", "INTEGER", false, 0),
        credential_pre_upgrade_column("retry_after_seconds", "INTEGER", false, 0),
    ];
    let mut statement = connection
        .prepare(
            "SELECT length(CAST(type AS BLOB)), type, \"notnull\", pk, hidden
             FROM pragma_table_xinfo('outbound_request_receipts', 'main')
             WHERE name=?1 COLLATE NOCASE",
        )
        .map_err(StoreError::CredentialPairStore)?;
    for column in expected {
        let mut rows = statement
            .query([column.name])
            .map_err(StoreError::CredentialPairStore)?;
        let row = rows
            .next()
            .map_err(StoreError::CredentialPairStore)?
            .ok_or(StoreError::CredentialPairInvalid)?;
        let length: i64 = credential_pre_upgrade_get(row, 0)?;
        if !(1..=32).contains(&length) {
            return Err(StoreError::CredentialPairInvalid);
        }
        let declared_type: String = credential_pre_upgrade_get(row, 1)?;
        let not_null: i64 = credential_pre_upgrade_get(row, 2)?;
        let primary_key: i64 = credential_pre_upgrade_get(row, 3)?;
        let hidden: i64 = credential_pre_upgrade_get(row, 4)?;
        let effective_not_null =
            not_null == 1 || primary_key == 1 && declared_type.eq_ignore_ascii_case("INTEGER");
        if !matches!(not_null, 0 | 1)
            || !declared_type.eq_ignore_ascii_case(column.declared_type)
            || primary_key != column.primary_key
            || effective_not_null != column.required
            || hidden != 0
        {
            return Err(StoreError::CredentialPairInvalid);
        }
    }
    Ok(())
}

fn credential_pre_upgrade_bindings_on(
    connection: &Connection,
    slot: CredentialSlot,
) -> Result<Vec<PreUpgradeRefreshBinding>, StoreError> {
    credential_pre_upgrade_history_bounds(connection)?;
    let (_, table, _) = credential_pre_upgrade_tables(slot);
    let count = credential_pre_upgrade_count(connection, table, true)?;
    if count == 0 {
        return Ok(Vec::new());
    }
    credential_pre_upgrade_receipt_metadata(connection)?;
    // A LEFT JOIN makes an orphan visible; it is never silently filtered away.
    let sql = format!(
        "SELECT binding.receipt_id,
                typeof(binding.attempt_id), length(CAST(binding.attempt_id AS BLOB)),
                typeof(binding.input_credential_generation),
                length(CAST(binding.input_credential_generation AS BLOB)),
                typeof(binding.output_credential_generation),
                length(CAST(binding.output_credential_generation AS BLOB)),
                binding.attempt_id, binding.input_credential_generation,
                binding.output_credential_generation, receipt.id,
                typeof(receipt.correlation_id), length(CAST(receipt.correlation_id AS BLOB)),
                receipt.correlation_id, receipt.started_at_ms, receipt.completed_at_ms,
                receipt.duration_ms, receipt.vehicle_tesla_id,
                typeof(receipt.transport), length(CAST(receipt.transport AS BLOB)),
                receipt.transport, typeof(receipt.operation),
                length(CAST(receipt.operation AS BLOB)), receipt.operation,
                typeof(receipt.safety_class), length(CAST(receipt.safety_class AS BLOB)),
                receipt.safety_class, typeof(receipt.precondition),
                length(CAST(receipt.precondition AS BLOB)), receipt.precondition,
                typeof(receipt.outcome), length(CAST(receipt.outcome AS BLOB)), receipt.outcome,
                receipt.http_status, receipt.retry_after_seconds
         FROM main.{table} AS binding
         LEFT JOIN main.outbound_request_receipts AS receipt ON receipt.id=binding.receipt_id
         ORDER BY binding.receipt_id"
    );
    let mut statement = connection
        .prepare(&sql)
        .map_err(StoreError::CredentialRefreshReceipt)?;
    let mut rows = statement
        .query([])
        .map_err(StoreError::CredentialRefreshReceipt)?;
    let mut result = Vec::with_capacity(count as usize);
    let mut attempts = std::collections::HashSet::new();
    let mut outputs = std::collections::HashSet::new();
    while let Some(row) = rows.next().map_err(StoreError::CredentialRefreshReceipt)? {
        if result.len() >= count as usize {
            return Err(StoreError::CredentialPairInvalid);
        }
        #[cfg(test)]
        credential_pre_upgrade_record_decode_visit();
        let receipt_id: i64 = credential_pre_upgrade_get(row, 0)?;
        let joined_id: Option<i64> = credential_pre_upgrade_get(row, 10)?;
        if joined_id != Some(receipt_id) {
            return Err(StoreError::CredentialPairInvalid);
        }
        for (type_index, length_index) in [(1, 2), (3, 4), (11, 12)] {
            credential_pre_upgrade_type(row, type_index, "text")?;
            let length: i64 = credential_pre_upgrade_get(row, length_index)?;
            if length != 36 {
                return Err(StoreError::CredentialPairInvalid);
            }
        }
        let output_type: String = credential_pre_upgrade_get(row, 5)?;
        let output_length: Option<i64> = credential_pre_upgrade_get(row, 6)?;
        if !matches!(
            (output_type.as_str(), output_length),
            ("null", None) | ("text", Some(36))
        ) {
            return Err(StoreError::CredentialPairInvalid);
        }
        for (type_index, length_index) in [(18, 19), (21, 22), (24, 25), (27, 28), (30, 31)] {
            credential_pre_upgrade_type(row, type_index, "text")?;
            let length: i64 = credential_pre_upgrade_get(row, length_index)?;
            if !(1..=64).contains(&length) {
                return Err(StoreError::CredentialPairInvalid);
            }
        }
        let attempt_text: String = credential_pre_upgrade_get(row, 7)?;
        let input_text: String = credential_pre_upgrade_get(row, 8)?;
        let output_text: Option<String> = credential_pre_upgrade_get(row, 9)?;
        let correlation_text: String = credential_pre_upgrade_get(row, 13)?;
        let attempt_id = credential_pre_upgrade_uuid(&attempt_text)?;
        let input_generation = credential_pre_upgrade_uuid(&input_text)?;
        let output_generation = output_text
            .as_deref()
            .map(credential_pre_upgrade_uuid)
            .transpose()?;
        credential_pre_upgrade_uuid(&correlation_text)?;
        if !attempts.insert(attempt_text)
            || output_text
                .as_ref()
                .is_some_and(|value| value == &input_text)
            || output_text
                .as_ref()
                .is_some_and(|value| !outputs.insert(value.clone()))
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        let started_at_ms: i64 = credential_pre_upgrade_get(row, 14)?;
        let completed_at_ms: Option<i64> = credential_pre_upgrade_get(row, 15)?;
        let duration_ms: Option<i64> = credential_pre_upgrade_get(row, 16)?;
        let vehicle_tesla_id: Option<i64> = credential_pre_upgrade_get(row, 17)?;
        let transport: String = credential_pre_upgrade_get(row, 20)?;
        let operation: String = credential_pre_upgrade_get(row, 23)?;
        let safety: String = credential_pre_upgrade_get(row, 26)?;
        let precondition: String = credential_pre_upgrade_get(row, 29)?;
        let outcome: String = credential_pre_upgrade_get(row, 32)?;
        let http_status: Option<i64> = credential_pre_upgrade_get(row, 33)?;
        let retry_after_seconds: Option<i64> = credential_pre_upgrade_get(row, 34)?;
        let expected_transport = match slot {
            CredentialSlot::OwnerApi => "legacy_auth",
            CredentialSlot::FleetApi => "fleet_api",
        };
        if started_at_ms < 0
            || vehicle_tesla_id.is_some_and(|value| value <= 0)
            || transport != expected_transport
            || operation != "token_refresh"
            || !matches!(
                safety.as_str(),
                "non_wake_endpoint"
                    | "conditional_read"
                    | "direct_wake_command"
                    | "explicit_vehicle_command"
            )
            || !matches!(
                precondition.as_str(),
                "not_required" | "stream_power_confirmed"
            )
            || http_status.is_some_and(|value| !(100..=599).contains(&value))
            || retry_after_seconds.is_some_and(|value| value < 0)
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        let receipt_state = if outcome == "started" {
            if completed_at_ms.is_some()
                || duration_ms.is_some()
                || http_status.is_some()
                || retry_after_seconds.is_some()
            {
                return Err(StoreError::CredentialPairInvalid);
            }
            PreUpgradeReceiptState::Started
        } else {
            let terminal =
                OutboundRequestOutcome::parse(&outcome).ok_or(StoreError::CredentialPairInvalid)?;
            if completed_at_ms.is_none_or(|value| value < started_at_ms)
                || duration_ms.is_none_or(|value| value < 0)
            {
                return Err(StoreError::CredentialPairInvalid);
            }
            PreUpgradeReceiptState::Completed(terminal)
        };
        result.push(PreUpgradeRefreshBinding {
            receipt_id: OutboundRequestReceiptId(receipt_id),
            attempt_id,
            input_generation,
            output_generation,
            receipt_state,
            started_at_ms,
        });
    }
    if result.len() != count as usize {
        return Err(StoreError::CredentialPairInvalid);
    }
    Ok(result)
}

fn credential_pre_upgrade_fences_on(
    connection: &Connection,
    slot: CredentialSlot,
) -> Result<Vec<Uuid>, StoreError> {
    credential_pre_upgrade_history_bounds(connection)?;
    let (_, _, table) = credential_pre_upgrade_tables(slot);
    let count = credential_pre_upgrade_count(connection, table, true)?;
    let mut statement = connection
        .prepare(&format!(
            "SELECT typeof(input_credential_generation),
                    length(CAST(input_credential_generation AS BLOB)), input_credential_generation
             FROM main.{table} ORDER BY input_credential_generation COLLATE NOCASE"
        ))
        .map_err(StoreError::CredentialRefreshReceipt)?;
    let mut rows = statement
        .query([])
        .map_err(StoreError::CredentialRefreshReceipt)?;
    let mut result = Vec::with_capacity(count as usize);
    let mut seen = std::collections::HashSet::new();
    while let Some(row) = rows.next().map_err(StoreError::CredentialRefreshReceipt)? {
        if result.len() >= count as usize {
            return Err(StoreError::CredentialPairInvalid);
        }
        #[cfg(test)]
        credential_pre_upgrade_record_decode_visit();
        credential_pre_upgrade_type(row, 0, "text")?;
        let length: i64 = credential_pre_upgrade_get(row, 1)?;
        if length != 36 {
            return Err(StoreError::CredentialPairInvalid);
        }
        let text: String = credential_pre_upgrade_get(row, 2)?;
        let generation = credential_pre_upgrade_uuid(&text)?;
        // UUID parsing supplies the same case-insensitive identity as NOCASE;
        // never split case variants into distinct historical fences.
        if !seen.insert(generation) {
            return Err(StoreError::CredentialPairInvalid);
        }
        result.push(generation);
    }
    if result.len() != count as usize {
        return Err(StoreError::CredentialPairInvalid);
    }
    Ok(result)
}

/// Check the whole A2 matrix and complete source admission, without fetching any
/// ciphertext. This is shared by Phase B's state read and its immediate commit.
fn credential_pre_upgrade_validate(
    connection: &Connection,
    state: &CredentialFormatState,
) -> Result<bool, StoreError> {
    let present = credential_pre_upgrade_presence(connection)?;
    let owner_pending = state.owner_api_state == CredentialSlotFormatState::Pending;
    let fleet_pending = state.fleet_api_state == CredentialSlotFormatState::Pending;
    if state.old_tables_dropped {
        if present
            || owner_pending
            || fleet_pending
            || state.owner_api_blocked.is_some()
            || state.fleet_api_blocked.is_some()
            || state.fences_pending
            || state.converted_at_ms.is_none()
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        return Ok(false);
    }
    if !present || state.converted_at_ms.is_some() || state.completed_at_ms.is_some() {
        return Err(StoreError::CredentialPairInvalid);
    }
    credential_pre_upgrade_history_bounds(connection)?;
    let owner = credential_pre_upgrade_row_metadata_on(connection, CredentialSlot::OwnerApi)?;
    let fleet = credential_pre_upgrade_row_metadata_on(connection, CredentialSlot::FleetApi)?;
    if owner.is_some() != owner_pending || fleet.is_some() != fleet_pending {
        return Err(StoreError::CredentialPairInvalid);
    }
    // Independent fences survive deleted receipts and absent token rows. Always
    // validate both histories, even when the requested slot returns no row.
    credential_pre_upgrade_bindings_on(connection, CredentialSlot::OwnerApi)?;
    credential_pre_upgrade_bindings_on(connection, CredentialSlot::FleetApi)?;
    credential_pre_upgrade_fences_on(connection, CredentialSlot::OwnerApi)?;
    credential_pre_upgrade_fences_on(connection, CredentialSlot::FleetApi)?;
    Ok(true)
}

fn credential_pre_upgrade_reader_version(connection: &Connection) -> Result<(), StoreError> {
    let version: i32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(StoreError::CredentialPairStore)?;
    if version != CREDENTIAL_FORMAT_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema(version));
    }
    Ok(())
}

impl CredentialConnection {
    pub(crate) fn read_pre_upgrade_refresh_bindings(
        &mut self,
        slot: crate::auth::credential_envelope::CredentialSlot,
    ) -> Result<Vec<PreUpgradeRefreshBinding>, StoreError> {
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
                .map_err(StoreError::CredentialRefreshReceipt)?;
        credential_pre_upgrade_reader_version(&transaction)?;
        let state = credential_format_state_on(&transaction)?;
        if credential_pre_upgrade_validate(&transaction, &state)? {
            credential_pre_upgrade_bindings_on(&transaction, slot)
        } else {
            Ok(Vec::new())
        }
    }

    pub(crate) fn read_pre_upgrade_refresh_fences(
        &mut self,
        slot: crate::auth::credential_envelope::CredentialSlot,
    ) -> Result<Vec<Uuid>, StoreError> {
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
                .map_err(StoreError::CredentialRefreshReceipt)?;
        credential_pre_upgrade_reader_version(&transaction)?;
        let state = credential_format_state_on(&transaction)?;
        if credential_pre_upgrade_validate(&transaction, &state)? {
            credential_pre_upgrade_fences_on(&transaction, slot)
        } else {
            Ok(Vec::new())
        }
    }
}
