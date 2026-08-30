// SPDX-License-Identifier: AGPL-3.0-only

pub async fn read_selected_car(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateCar, TeslaMateReaderError> {
    let (session, selected_car_id_i16) =
        open_snapshot_session(source, password, selected_car_id, limits).await?;
    let mut retained_rows = 0_usize;
    let result = read_cars(
        session.client(),
        selected_car_id_i16,
        limits,
        &mut retained_rows,
    )
    .await
    .and_then(|cars| {
        cars.into_iter()
            .next()
            .ok_or(TeslaMateReaderError::SelectedCarMissing { selected_car_id })
    });
    let finish = session.finish().await;
    match (result, finish) {
        (Ok(car), Ok(())) => Ok(car),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Read active TeslaMate sessions and their attached rows from one validated,
/// read-only repeatable-read snapshot. Completed history is intentionally not
/// returned here.
pub async fn read_open_session(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateOpenSession, TeslaMateReaderError> {
    let (session, selected_car_id_i16) =
        open_snapshot_session(source, password, selected_car_id, limits).await?;
    let result = read_open_session_in_client(session.client(), selected_car_id_i16, limits).await;
    let finish = session.finish().await;
    match (result, finish) {
        (Err(error), _) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(error),
    }
}

pub(crate) async fn read_open_session_in_client(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateOpenSession, TeslaMateReaderError> {
    let mut retained_rows = 0_usize;
    let drives = read_open_drives(client, selected_car_id, limits, &mut retained_rows).await?;
    let processes =
        read_open_charging_processes(client, selected_car_id, limits, &mut retained_rows).await?;
    let states = read_open_states(client, selected_car_id, limits, &mut retained_rows).await?;
    let watermarks = read_source_watermarks(client, selected_car_id).await?;
    // TeslaMate can leave stale open rows behind after interrupted sessions.
    // Keep those rows in the immutable history capture, but do not guess which
    // of several drives or charges is live. State rows are different: the
    // newest row is the current state, even if predecessors were not closed.
    let drive = unique_open_parent(drives);
    let charge = unique_open_parent(processes);
    let state = states.into_iter().max_by_key(|state| state.id);
    let positions = read_open_positions(
        client,
        selected_car_id,
        drive.as_ref().map(|drive| drive.id),
        limits,
        &mut retained_rows,
    )
    .await?;
    let (drive_positions, standalone_positions): (Vec<_>, Vec<_>) = positions
        .into_iter()
        .partition(|position| position.drive_id.is_some());
    let charge_samples = if charge.is_some() {
        read_open_charges(client, selected_car_id, limits, &mut retained_rows).await?
    } else {
        Vec::new()
    };
    let result = TeslaMateOpenSession {
        car_id: i64::from(selected_car_id),
        drive,
        drive_positions,
        charge,
        charge_samples,
        state,
        standalone_positions,
        watermarks,
    };
    result
        .validate()
        .map_err(TeslaMateReaderError::OpenSessionProjection)?;
    Ok(result)
}

fn unique_open_parent<T>(mut rows: Vec<T>) -> Option<T> {
    if rows.len() == 1 { rows.pop() } else { None }
}

fn open_rows_sql(table: SourceTable, predicate: &str) -> String {
    let sql = projection(table).sql;
    sql.replacen(
        "WHERE \"source\".\"id\" > $1",
        &format!("WHERE {predicate} AND \"source\".\"id\" > $1"),
        1,
    )
}

async fn read_open_drives(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateDrive>, TeslaMateReaderError> {
    read_open_rows(
        client,
        SourceTable::Drives,
        "\"source\".\"end_date\" IS NULL",
        selected_car_id,
        limits,
        retained_rows,
        decode_drive,
    )
    .await
}

async fn read_open_positions(
    client: &Client,
    selected_car_id: i16,
    active_drive_id: Option<i64>,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMatePosition>, TeslaMateReaderError> {
    let mut materialized_positions = 0_usize;
    let mut positions = read_open_position_branch(
        client,
        selected_car_id,
        OpenPositionBranch::Standalone,
        limits,
        retained_rows,
        &mut materialized_positions,
    )
    .await?;
    if let Some(active_drive_id) = active_drive_id {
        positions.extend(
            read_open_position_branch(
                client,
                selected_car_id,
                OpenPositionBranch::ActiveDrive(active_drive_id),
                limits,
                retained_rows,
                &mut materialized_positions,
            )
            .await?,
        );
    }
    positions.sort_unstable_by_key(|position| position.id);
    Ok(positions)
}

async fn read_open_position_branch(
    client: &Client,
    selected_car_id: i16,
    branch: OpenPositionBranch,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    materialized_positions: &mut usize,
) -> Result<Vec<TeslaMatePosition>, TeslaMateReaderError> {
    let remaining_open = MAX_MATERIALIZED_OPEN_POSITIONS.saturating_sub(*materialized_positions);
    let remaining_rows = limits.maximum_rows.saturating_sub(*retained_rows);
    let allowed = remaining_open.min(remaining_rows);
    let query_limit = i64::try_from(allowed.saturating_add(1)).unwrap_or(i64::MAX);
    let stream = client
        .copy_out(&open_position_branch_copy_sql(
            selected_car_id,
            branch,
            query_limit,
        ))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        position_copy_types()
    ));
    let mut positions = Vec::new();
    while let Some(row) = rows.as_mut().try_next().await? {
        if *materialized_positions >= MAX_MATERIALIZED_OPEN_POSITIONS {
            return Err(
                TeslaMateReaderError::MaterializedOpenPositionLimitExceeded {
                    maximum: MAX_MATERIALIZED_OPEN_POSITIONS,
                },
            );
        }
        retain_row(retained_rows, limits.maximum_rows)?;
        *materialized_positions += 1;
        positions.push(decode_binary_position(&row)?);
    }
    Ok(positions)
}

async fn read_open_charging_processes(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateChargingProcess>, TeslaMateReaderError> {
    read_open_rows(
        client,
        SourceTable::ChargingProcesses,
        "\"source\".\"end_date\" IS NULL",
        selected_car_id,
        limits,
        retained_rows,
        decode_charging_process,
    )
    .await
}

async fn read_open_charges(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateCharge>, TeslaMateReaderError> {
    read_open_rows(
        client,
        SourceTable::Charges,
        "\"process\".\"end_date\" IS NULL",
        selected_car_id,
        limits,
        retained_rows,
        decode_charge,
    )
    .await
}

async fn read_open_states(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
) -> Result<Vec<TeslaMateState>, TeslaMateReaderError> {
    read_open_rows(
        client,
        SourceTable::States,
        "\"source\".\"end_date\" IS NULL",
        selected_car_id,
        limits,
        retained_rows,
        decode_state,
    )
    .await
}

async fn read_open_rows<T, F>(
    client: &Client,
    table: SourceTable,
    predicate: &str,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    decode: F,
) -> Result<Vec<T>, TeslaMateReaderError>
where
    F: Fn(&Row) -> Result<T, TeslaMateReaderError>,
{
    let sql = open_rows_sql(table, predicate);
    let page_size = i64::from(limits.page_size);
    let mut last_id = 0_i32;
    let mut result = Vec::new();
    loop {
        let page = client
            .query(&sql, &[&last_id, &page_size, &selected_car_id])
            .await?;
        let page_len = page.len();
        for row in page {
            let id = required_i32(&row, table.name(), "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage {
                    table: table.name(),
                });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            result.push(decode(&row)?);
        }
        if page_len < limits.page_size as usize {
            return Ok(result);
        }
    }
}

async fn read_source_watermarks(
    client: &Client,
    selected_car_id: i16,
) -> Result<TeslaMateSourceWatermarks, TeslaMateReaderError> {
    Ok(TeslaMateSourceWatermarks {
        drives: read_interval_watermark(
            client,
            "drives",
            "SELECT MAX(\"id\") AS \"max_id\", MAX(\"start_date\") AS \"max_start\", MAX(\"end_date\") AS \"max_end\" FROM \"public\".\"drives\" WHERE \"car_id\" = $1",
            selected_car_id,
        )
        .await?,
        positions: read_date_watermark(
            client,
            "positions",
            "SELECT MAX(\"id\") AS \"max_id\", MAX(\"date\") AS \"max_timestamp\" FROM \"public\".\"positions\" WHERE \"car_id\" = $1",
            selected_car_id,
        )
        .await?,
        charging_processes: read_interval_watermark(
            client,
            "charging_processes",
            "SELECT MAX(\"id\") AS \"max_id\", MAX(\"start_date\") AS \"max_start\", MAX(\"end_date\") AS \"max_end\" FROM \"public\".\"charging_processes\" WHERE \"car_id\" = $1",
            selected_car_id,
        )
        .await?,
        charges: read_date_watermark(
            client,
            "charges",
            "SELECT MAX(\"charge\".\"id\") AS \"max_id\", MAX(\"charge\".\"date\") AS \"max_timestamp\" FROM \"public\".\"charges\" AS \"charge\" JOIN \"public\".\"charging_processes\" AS \"process\" ON \"process\".\"id\" = \"charge\".\"charging_process_id\" WHERE \"process\".\"car_id\" = $1",
            selected_car_id,
        )
        .await?,
        states: read_interval_watermark(
            client,
            "states",
            "SELECT MAX(\"id\") AS \"max_id\", MAX(\"start_date\") AS \"max_start\", MAX(\"end_date\") AS \"max_end\" FROM \"public\".\"states\" WHERE \"car_id\" = $1",
            selected_car_id,
        )
        .await?,
        updates: read_interval_watermark(
            client,
            "updates",
            "SELECT MAX(\"id\") AS \"max_id\", MAX(\"start_date\") AS \"max_start\", MAX(\"end_date\") AS \"max_end\" FROM \"public\".\"updates\" WHERE \"car_id\" = $1",
            selected_car_id,
        )
        .await?,
    })
}

async fn read_interval_watermark(
    client: &Client,
    table: &'static str,
    sql: &'static str,
    selected_car_id: i16,
) -> Result<TeslaMateSourceWatermark, TeslaMateReaderError> {
    let row = client.query_one(sql, &[&selected_car_id]).await?;
    let max_id = row
        .try_get::<_, Option<i32>>("max_id")
        .map_err(|source| cell(table, "id", source))?
        .map(i64::from);
    let start = row
        .try_get::<_, Option<PrimitiveDateTime>>("max_start")
        .map_err(|source| cell(table, "start_date", source))?
        .map(|value| timestamp_ms(value, table, "start_date"))
        .transpose()?;
    let end = row
        .try_get::<_, Option<PrimitiveDateTime>>("max_end")
        .map_err(|source| cell(table, "end_date", source))?
        .map(|value| timestamp_ms(value, table, "end_date"))
        .transpose()?;
    Ok(TeslaMateSourceWatermark {
        max_id,
        max_timestamp_ms: match (start, end) {
            (Some(start), Some(end)) => Some(start.max(end)),
            (Some(start), None) => Some(start),
            (None, Some(end)) => Some(end),
            (None, None) => None,
        },
    })
}

async fn read_date_watermark(
    client: &Client,
    table: &'static str,
    sql: &'static str,
    selected_car_id: i16,
) -> Result<TeslaMateSourceWatermark, TeslaMateReaderError> {
    let row = client.query_one(sql, &[&selected_car_id]).await?;
    let max_id = row
        .try_get::<_, Option<i32>>("max_id")
        .map_err(|source| cell(table, "id", source))?
        .map(i64::from);
    let timestamp = row
        .try_get::<_, Option<PrimitiveDateTime>>("max_timestamp")
        .map_err(|source| cell(table, "date", source))?
        .map(|value| timestamp_ms(value, table, "date"))
        .transpose()?;
    Ok(TeslaMateSourceWatermark {
        max_id,
        max_timestamp_ms: timestamp,
    })
}
