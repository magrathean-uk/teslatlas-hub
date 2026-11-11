// SPDX-License-Identifier: AGPL-3.0-only

pub(crate) async fn open_snapshot_session(
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    selected_car_id: i64,
    limits: SourceDbReadLimits,
) -> Result<(SourceDbSnapshotSession, i16), SourceDbReaderError> {
    let (session, selected_car_id, _) =
        open_snapshot_session_with_schema(source, password, selected_car_id, limits).await?;
    Ok((session, selected_car_id))
}

pub(crate) async fn open_snapshot_session_with_schema(
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    selected_car_id: i64,
    limits: SourceDbReadLimits,
) -> Result<(SourceDbSnapshotSession, i16, SourceDbSchemaInfo), SourceDbReaderError> {
    limits.validate()?;
    if selected_car_id <= 0 {
        return Err(SourceDbReaderError::InvalidSelectedCarId);
    }
    let selected_car_id = selected_source_car_id(selected_car_id)?;
    let (client, connection_task) = connect_source(source, password, limits).await?;
    let session = SourceDbSnapshotSession::new(client, connection_task);
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
    password: &SourceDbPassword,
    selected_car_id: i64,
    limits: SourceDbReadLimits,
) -> Result<(ExportedSnapshotLease, i16, SourceDbSchemaInfo), SourceDbReaderError> {
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
) -> Result<String, SourceDbReaderError> {
    let valid = snapshot_id.len() <= 64
        && snapshot_id.contains('-')
        && snapshot_id.split('-').all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_hexdigit())
        });
    valid
        .then_some(snapshot_id)
        .ok_or(SourceDbReaderError::InvalidExportedSnapshot)
}

/// Open one bounded capture connection on an already-exported source view.
/// The owner lease must outlive this returned session.
pub(crate) async fn open_snapshot_capture_lane(
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    snapshot_id: &str,
    limits: SourceDbReadLimits,
) -> Result<SourceDbSnapshotSession, SourceDbReaderError> {
    limits.validate()?;
    let snapshot_sql = snapshot_import_sql(snapshot_id)?;
    let (client, connection_task) = connect_source(source, password, limits).await?;
    let session = SourceDbSnapshotSession::new(client, connection_task);
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

pub(crate) fn snapshot_import_sql(snapshot_id: &str) -> Result<String, SourceDbReaderError> {
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

/// Like [`binary_copy_sql`], but ending in `LIMIT limit` instead of `LIMIT ALL`,
/// so a COPY reader that fails one row past its cap also stops the server there.
pub(crate) fn capped_binary_copy_sql(
    table: SourceTable,
    selected_car_id: i16,
    limit: i64,
) -> String {
    let query = render_bounded_projection_query(table, selected_car_id, limit);
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
    const ORDERING: &str = "ORDER BY \"source\".\"id\" ASC";
    let (before_ordering, after_ordering) = positions
        .split_once(ORDERING)
        .expect("reviewed positions projection must retain its fixed ordering");
    assert!(
        !after_ordering.contains(ORDERING),
        "reviewed positions projection must contain one fixed ordering"
    );
    let query = format!(
        "{before_ordering}  AND \"source\".\"id\" = ANY(ARRAY[{ids}]::int4[])\n\
         {ORDERING}{after_ordering}"
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
