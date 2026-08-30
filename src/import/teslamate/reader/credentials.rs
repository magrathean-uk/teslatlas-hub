// SPDX-License-Identifier: AGPL-3.0-only

/// Read exactly one encrypted legacy OAuth pair from TeslaMate's private
/// schema. The fixed query executes in the same repeatable-read, read-only
/// session as history migration; it never asks TeslaMate to refresh or write
/// credentials.
pub async fn read_legacy_token_ciphertexts(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateLegacyTokenCiphertexts, TeslaMateReaderError> {
    limits.validate()?;
    let (client, connection_task) = connect_source(source, password, limits).await?;
    let session = TeslaMateSnapshotSession::new(client, connection_task);
    let result = async {
        prepare_read_only_snapshot(session.client(), source, limits).await?;
        read_legacy_token_ciphertexts_in_client(session.client()).await
    }
    .await;
    let finish = session.finish().await;
    match (result, finish) {
        (Err(error), _) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Read the opaque exact-4.1.1 legacy pair without changing the caller's
/// transaction. `private.tokens` is part of the pinned source contract.
pub(crate) async fn read_legacy_token_ciphertexts_in_client(
    client: &Client,
) -> Result<TeslaMateLegacyTokenCiphertexts, TeslaMateReaderError> {
    inspect_legacy_token_pair_in_client(client).await?;
    let (_, query, relation) = exact_legacy_token_queries(true)?;
    let rows = client.query(query, &[]).await?;
    if rows.is_empty() {
        return Err(TeslaMateReaderError::LegacyTokenPairMissing);
    }
    if rows.len() != 1 {
        return Err(TeslaMateReaderError::LegacyTokenPairAmbiguous);
    }
    let row = &rows[0];
    let access: Vec<u8> = row
        .try_get("access")
        .map_err(|source| cell(relation, "access", source))?;
    let refresh: Vec<u8> = row
        .try_get("refresh")
        .map_err(|source| cell(relation, "refresh", source))?;
    if access.is_empty() || refresh.is_empty() {
        return Err(TeslaMateReaderError::LegacyTokenPairEmpty);
    }
    Ok(TeslaMateLegacyTokenCiphertexts { access, refresh })
}

async fn inspect_legacy_token_pair_in_client(
    client: &Client,
) -> Result<TeslaMateLegacyTokenPairDiagnostics, TeslaMateReaderError> {
    let private_tokens_exists: bool = client
        .query_one(PRIVATE_LEGACY_TOKENS_EXISTS_SQL, &[])
        .await?
        .try_get("private_tokens_exists")?;
    let (length_query, _, relation) = exact_legacy_token_queries(private_tokens_exists)?;
    let length_rows = client.query(length_query, &[]).await?;
    let lengths = length_rows
        .iter()
        .map(|row| {
            Ok((
                row.try_get("access_length")
                    .map_err(|source| cell(relation, "access", source))?,
                row.try_get("refresh_length")
                    .map_err(|source| cell(relation, "refresh", source))?,
            ))
        })
        .collect::<Result<Vec<_>, TeslaMateReaderError>>()?;
    validate_legacy_token_pair_lengths(relation, &lengths)
}

fn exact_legacy_token_queries(
    private_tokens_exists: bool,
) -> Result<(&'static str, &'static str, &'static str), TeslaMateReaderError> {
    private_tokens_exists
        .then_some((
            PRIVATE_LEGACY_TOKEN_LENGTHS_SQL,
            PRIVATE_LEGACY_TOKENS_SQL,
            "private.tokens",
        ))
        .ok_or(TeslaMateReaderError::LegacyTokenPairMissing)
}

fn validate_legacy_ciphertext_length(
    relation: &'static str,
    column: &'static str,
    actual: i64,
) -> Result<(), TeslaMateReaderError> {
    if actual == 0 {
        return Err(TeslaMateReaderError::LegacyTokenPairEmpty);
    }
    if !(1..=MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES_I64).contains(&actual) {
        return Err(TeslaMateReaderError::LegacyTokenCiphertextTooLarge {
            relation,
            column,
            maximum: MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES_I64,
            actual,
        });
    }
    Ok(())
}

fn validate_legacy_token_pair_lengths(
    relation: &'static str,
    lengths: &[(i64, i64)],
) -> Result<TeslaMateLegacyTokenPairDiagnostics, TeslaMateReaderError> {
    let [(access_length, refresh_length)] = lengths else {
        return if lengths.is_empty() {
            Err(TeslaMateReaderError::LegacyTokenPairMissing)
        } else {
            Err(TeslaMateReaderError::LegacyTokenPairAmbiguous)
        };
    };
    validate_legacy_ciphertext_length(relation, "access", *access_length)?;
    validate_legacy_ciphertext_length(relation, "refresh", *refresh_length)?;
    Ok(TeslaMateLegacyTokenPairDiagnostics {
        relation: relation.to_owned(),
        access_ciphertext_bytes: u64::try_from(*access_length)
            .expect("validated token ciphertext length is positive"),
        refresh_ciphertext_bytes: u64::try_from(*refresh_length)
            .expect("validated token ciphertext length is positive"),
    })
}

pub(crate) async fn open_snapshot_session(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<(TeslaMateSnapshotSession, i16), TeslaMateReaderError> {
    let (session, selected_car_id, _) =
        open_snapshot_session_with_schema(source, password, selected_car_id, limits).await?;
    Ok((session, selected_car_id))
}

pub(crate) async fn open_snapshot_session_with_schema(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<(TeslaMateSnapshotSession, i16, TeslaMateSchemaInfo), TeslaMateReaderError> {
    limits.validate()?;
    if selected_car_id <= 0 {
        return Err(TeslaMateReaderError::InvalidSelectedCarId);
    }
    let selected_car_id = selected_source_car_id(selected_car_id)?;
    let (client, connection_task) = connect_source(source, password, limits).await?;
    let session = TeslaMateSnapshotSession::new(client, connection_task);
    let schema = match prepare_read_only_snapshot(session.client(), source, limits).await {
        Ok(schema) => schema,
        Err(error) => {
            let _ = session.finish().await;
            return Err(error);
        }
    };
    Ok((session, selected_car_id, schema))
}

/// Open and validate the owner transaction for a future parallel capture.
/// PostgreSQL invalidates exported snapshots as soon as this lease ends, so a
/// caller cannot accidentally continue capture after its consistent source view
/// is gone.
pub(crate) async fn open_exported_snapshot_lease(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<(ExportedSnapshotLease, i16, TeslaMateSchemaInfo), TeslaMateReaderError> {
    let (session, selected_car_id, schema) =
        open_snapshot_session_with_schema(source, password, selected_car_id, limits).await?;
    let exported = session
        .client()
        .query_one("SELECT pg_export_snapshot() AS snapshot_id", &[])
        .await
        .and_then(|row| row.try_get::<_, String>("snapshot_id"));
    let snapshot_id = match exported {
        Ok(snapshot_id) => match validate_exported_snapshot_id(snapshot_id) {
            Ok(snapshot_id) => snapshot_id,
            Err(error) => {
                let _ = session.finish().await;
                return Err(error);
            }
        },
        Err(error) => {
            let _ = session.finish().await;
            return Err(error.into());
        }
    };
    Ok((
        ExportedSnapshotLease {
            session,
            snapshot_id,
        },
        selected_car_id,
        schema,
    ))
}

pub(crate) fn validate_exported_snapshot_id(
    snapshot_id: String,
) -> Result<String, TeslaMateReaderError> {
    let valid = snapshot_id.len() <= 64
        && snapshot_id.contains('-')
        && snapshot_id.split('-').all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_hexdigit())
        });
    valid
        .then_some(snapshot_id)
        .ok_or(TeslaMateReaderError::InvalidExportedSnapshot)
}

/// Open one bounded capture connection on an already-exported source view.
/// The owner lease must outlive this returned session.
pub(crate) async fn open_snapshot_capture_lane(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    snapshot_id: &str,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateSnapshotSession, TeslaMateReaderError> {
    limits.validate()?;
    let snapshot_sql = snapshot_import_sql(snapshot_id)?;
    let (client, connection_task) = connect_source(source, password, limits).await?;
    let session = TeslaMateSnapshotSession::new(client, connection_task);
    let prepared = async {
        session
            .client()
            .batch_execute(source.session_sql()[0])
            .await?;
        session
            .client()
            .batch_execute(source.session_sql()[1])
            .await?;
        session.client().batch_execute(&snapshot_sql).await?;
        for statement in &source.session_sql()[2..] {
            session.client().batch_execute(statement).await?;
        }
        session
            .client()
            .batch_execute(&copy_statement_timeout_sql(limits.copy_statement_timeout))
            .await?;
        validate_source_schema(session.client()).await
    }
    .await;
    if let Err(error) = prepared {
        let _ = session.finish().await;
        return Err(error);
    }
    Ok(session)
}

pub(crate) fn snapshot_import_sql(snapshot_id: &str) -> Result<String, TeslaMateReaderError> {
    let snapshot_id = validate_exported_snapshot_id(snapshot_id.to_owned())?;
    Ok(format!("SET TRANSACTION SNAPSHOT '{snapshot_id}'"))
}

/// Build a source-safe binary `COPY TO STDOUT` statement for one reviewed
/// projection. PostgreSQL does not permit query parameters in `COPY`; both the
/// table and every SQL fragment are fixed and `selected_car_id` is already an
/// `i16` from the validated source domain.
pub(crate) fn binary_copy_sql(table: SourceTable, selected_car_id: i16) -> String {
    let query = render_streaming_projection_query(table, selected_car_id);
    format!("COPY ({query}) TO STDOUT WITH (FORMAT BINARY)")
}

/// Render the canonical reviewed projection with typed, integer-only bindings.
/// Unknown placeholder tokens remain unchanged so a future schema-template
/// change fails at PostgreSQL prepare time instead of being silently rewritten.
fn render_streaming_projection_query(table: SourceTable, selected_car_id: i16) -> String {
    render_projection_template(projection(table).sql, 0, "ALL", selected_car_id)
}

fn render_bounded_projection_query(table: SourceTable, selected_car_id: i16, limit: i64) -> String {
    render_projection_template(
        projection(table).sql,
        0,
        &limit.to_string(),
        selected_car_id,
    )
}

fn render_projection_template(
    template: &str,
    last_id: i32,
    limit: &str,
    selected_car_id: i16,
) -> String {
    let mut rendered = String::with_capacity(template.len() + 16);
    let bytes = template.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' || index + 1 >= bytes.len() || !bytes[index + 1].is_ascii_digit() {
            rendered.push(bytes[index] as char);
            index += 1;
            continue;
        }

        let start = index;
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let token = &template[start..index];
        match token {
            "$1" => rendered.push_str(&last_id.to_string()),
            "$2" => rendered.push_str(limit),
            "$3" => rendered.push_str(&selected_car_id.to_string()),
            _ => rendered.push_str(token),
        }
    }
    rendered
}

/// Build a source-safe binary `COPY TO STDOUT` statement for a bounded set of
/// reviewed position IDs. The inner query is the canonical positions
/// projection, so changes to its columns or casts stay coupled to every binary
/// position decoder. The caller supplies only validated `int4` identifiers.
pub(crate) fn related_positions_binary_copy_sql(
    selected_car_id: i16,
    position_ids: &[i32],
) -> String {
    debug_assert!(!position_ids.is_empty());
    let ids = position_ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let positions = render_streaming_projection_query(SourceTable::Positions, selected_car_id);
    let query = format!(
        "SELECT \"related\".* FROM ({positions}) AS \"related\" \
         WHERE \"related\".\"id\" = ANY(ARRAY[{ids}]::int4[]) \
         ORDER BY \"related\".\"id\" ASC"
    );
    format!("COPY ({query}) TO STDOUT WITH (FORMAT BINARY)")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenPositionBranch {
    Standalone,
    ActiveDrive(i64),
}

fn open_position_projection_template(branch: OpenPositionBranch) -> String {
    let predicate = match branch {
        OpenPositionBranch::Standalone => "\"source\".\"drive_id\" IS NULL".to_owned(),
        OpenPositionBranch::ActiveDrive(drive_id) => {
            format!("\"source\".\"drive_id\" = {drive_id}")
        }
    };
    const ORDERING: &str = "ORDER BY \"source\".\"id\" ASC";
    let template = projection(SourceTable::Positions).sql;
    let (before_ordering, after_ordering) = template
        .split_once(ORDERING)
        .expect("reviewed positions projection must retain its fixed ordering");
    assert!(
        !after_ordering.contains(ORDERING),
        "reviewed positions projection must contain one fixed ordering"
    );
    format!("{before_ordering}  AND {predicate}\n{ORDERING}{after_ordering}")
}

fn open_position_branch_copy_sql(
    selected_car_id: i16,
    branch: OpenPositionBranch,
    limit: i64,
) -> String {
    let template = open_position_projection_template(branch);
    let query = render_projection_template(&template, 0, &limit.to_string(), selected_car_id);
    format!("COPY ({query}) TO STDOUT WITH (FORMAT BINARY)")
}

fn bounded_position_binary_copy_sql(selected_car_id: i16, limit: i64) -> String {
    let query = render_bounded_projection_query(SourceTable::Positions, selected_car_id, limit);
    format!("COPY ({query}) TO STDOUT WITH (FORMAT BINARY)")
}
