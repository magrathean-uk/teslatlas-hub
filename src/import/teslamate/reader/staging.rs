// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Debug, Clone, Copy)]
enum CaptureJob {
    Table(TeslaMateStageTable),
    IdRange {
        table: TeslaMateStageTable,
        start_id: i64,
        end_id: i64,
    },
}

fn distribute_capture_jobs(
    lane_count: usize,
    position_max_id: i64,
    charge_max_id: i64,
) -> Vec<Vec<CaptureJob>> {
    let mut lane_jobs = vec![Vec::new(); lane_count];
    let regular_tables = [
        TeslaMateStageTable::Cars,
        TeslaMateStageTable::Drives,
        TeslaMateStageTable::ChargingProcesses,
        TeslaMateStageTable::Addresses,
        TeslaMateStageTable::Geofences,
        TeslaMateStageTable::States,
        TeslaMateStageTable::Updates,
    ];
    let mut jobs = regular_tables
        .into_iter()
        .map(CaptureJob::Table)
        .collect::<Vec<_>>();
    jobs.extend(shard_id_ranges(
        TeslaMateStageTable::Positions,
        position_max_id,
        lane_count,
    ));
    jobs.extend(shard_id_ranges(
        TeslaMateStageTable::Charges,
        charge_max_id,
        lane_count,
    ));
    for (index, job) in jobs.into_iter().enumerate() {
        lane_jobs[index % lane_count].push(job);
    }
    lane_jobs
}

fn shard_id_ranges(
    table: TeslaMateStageTable,
    max_id: i64,
    maximum_shards: usize,
) -> Vec<CaptureJob> {
    if max_id <= 0 {
        return Vec::new();
    }
    let shard_count = maximum_shards.min(usize::try_from(max_id).unwrap_or(maximum_shards));
    (0..shard_count)
        .map(|index| {
            let start_id = ((i128::from(max_id) * i128::from(index as u64))
                / i128::from(shard_count as u64))
                + 1;
            let end_id = (i128::from(max_id) * i128::from((index + 1) as u64))
                / i128::from(shard_count as u64);
            CaptureJob::IdRange {
                table,
                start_id: i64::try_from(start_id).expect("source id fits i64"),
                end_id: i64::try_from(end_id).expect("source id fits i64"),
            }
        })
        .collect()
}

struct RawStagePage {
    table: TeslaMateStageTable,
    rows: Vec<(i64, String)>,
}

async fn capture_snapshot_lane(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    snapshot_id: &str,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    jobs: Vec<CaptureJob>,
    sender: mpsc::Sender<RawStagePage>,
) -> Result<(), TeslaMateReaderError> {
    let session = open_snapshot_capture_lane(source, password, snapshot_id, limits).await?;
    let result = async {
        for job in jobs {
            capture_raw_table_pages(session.client(), job, selected_car_id, limits, &sender)
                .await?;
        }
        Ok::<(), TeslaMateReaderError>(())
    }
    .await;
    let finish = session.finish().await;
    match (result, finish) {
        (Err(error), _) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(error),
    }
}

async fn capture_raw_table_pages(
    client: &Client,
    job: CaptureJob,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    sender: &mpsc::Sender<RawStagePage>,
) -> Result<(), TeslaMateReaderError> {
    let (table, start_id, end_id) = match job {
        CaptureJob::Table(table) => (table, 1, None),
        CaptureJob::IdRange {
            table,
            start_id,
            end_id,
        } => (table, start_id, Some(end_id)),
    };
    let mut last_id = start_id.saturating_sub(1);
    let page_size = i64::from(limits.page_size);
    loop {
        let page = match table {
            TeslaMateStageTable::Cars => {
                let last_id = i16::try_from(last_id).expect("car cursor fits smallint");
                client
                    .query(
                        projection(SourceTable::Cars).sql,
                        &[&last_id, &page_size, &selected_car_id],
                    )
                    .await?
            }
            TeslaMateStageTable::Geofences => {
                let last_id = i32::try_from(last_id).map_err(|_| {
                    TeslaMateReaderError::NonProgressingPage {
                        table: table.as_str(),
                    }
                })?;
                client
                    .query(
                        GEOFENCE_GEOMETRY_SQL,
                        &[&last_id, &page_size, &selected_car_id],
                    )
                    .await?
            }
            _ => {
                let last_id = i32::try_from(last_id).map_err(|_| {
                    TeslaMateReaderError::NonProgressingPage {
                        table: table.as_str(),
                    }
                })?;
                match end_id {
                    Some(end_id) => {
                        let end_id = i32::try_from(end_id).map_err(|_| {
                            TeslaMateReaderError::NonProgressingPage {
                                table: table.as_str(),
                            }
                        })?;
                        client
                            .query(
                                &ranged_projection_sql(stage_table_source(table)),
                                &[&last_id, &page_size, &selected_car_id, &end_id],
                            )
                            .await?
                    }
                    None => {
                        client
                            .query(
                                projection(stage_table_source(table)).sql,
                                &[&last_id, &page_size, &selected_car_id],
                            )
                            .await?
                    }
                }
            }
        };
        let page_len = page.len();
        let mut rows = Vec::with_capacity(page_len);
        for row in page {
            let id = match table {
                TeslaMateStageTable::Cars => i64::from(required_i16(&row, "cars", "id")?),
                _ => i64::from(required_i32(&row, table.as_str(), "id")?),
            };
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage {
                    table: table.as_str(),
                });
            }
            last_id = id;
            rows.push((id, encode_stage_row(table, &row)?));
        }
        if !rows.is_empty() && sender.send(RawStagePage { table, rows }).await.is_err() {
            return Ok(());
        }
        if page_len < limits.page_size as usize {
            return Ok(());
        }
    }
}

const fn stage_table_source(table: TeslaMateStageTable) -> SourceTable {
    match table {
        TeslaMateStageTable::Cars => SourceTable::Cars,
        TeslaMateStageTable::Drives => SourceTable::Drives,
        TeslaMateStageTable::Positions => SourceTable::Positions,
        TeslaMateStageTable::ChargingProcesses => SourceTable::ChargingProcesses,
        TeslaMateStageTable::Charges => SourceTable::Charges,
        TeslaMateStageTable::Addresses => SourceTable::Addresses,
        TeslaMateStageTable::Geofences => SourceTable::Geofences,
        TeslaMateStageTable::States => SourceTable::States,
        TeslaMateStageTable::Updates => SourceTable::Updates,
    }
}

fn ranged_projection_sql(table: SourceTable) -> String {
    let template = projection(table).sql;
    let ordering = "ORDER BY \"source\".\"id\" ASC";
    let (before_ordering, after_ordering) = template
        .split_once(ordering)
        .expect("reviewed projection must retain fixed ordering");
    format!("{before_ordering}  AND \"source\".\"id\" <= $4\n{ordering}{after_ordering}")
}

async fn source_max_id(
    client: &Client,
    table: TeslaMateStageTable,
    selected_car_id: i16,
) -> Result<i64, TeslaMateReaderError> {
    let sql = match table {
        TeslaMateStageTable::Positions => {
            "SELECT COALESCE(MAX(\"source\".\"id\"), 0)::bigint AS max_id \
             FROM \"public\".\"positions\" AS \"source\" \
             WHERE \"source\".\"car_id\" = $1"
        }
        TeslaMateStageTable::Charges => {
            "SELECT COALESCE(MAX(\"source\".\"id\"), 0)::bigint AS max_id \
             FROM \"public\".\"charges\" AS \"source\" \
             JOIN \"public\".\"charging_processes\" AS \"process\" \
               ON \"process\".\"id\" = \"source\".\"charging_process_id\" \
             WHERE \"process\".\"car_id\" = $1"
        }
        _ => unreachable!("only large tables are sharded"),
    };
    Ok(client
        .query_one(sql, &[&selected_car_id])
        .await?
        .try_get("max_id")?)
}

fn encode_stage_row(table: TeslaMateStageTable, row: &Row) -> Result<String, TeslaMateReaderError> {
    let encoded = match table {
        TeslaMateStageTable::Cars => serde_json::to_string(&decode_car(row)?),
        TeslaMateStageTable::Drives => serde_json::to_string(&decode_drive(row)?),
        TeslaMateStageTable::Positions => serde_json::to_string(&decode_position(row)?),
        TeslaMateStageTable::ChargingProcesses => {
            serde_json::to_string(&decode_charging_process(row)?)
        }
        TeslaMateStageTable::Charges => serde_json::to_string(&decode_charge(row)?),
        TeslaMateStageTable::Addresses => serde_json::to_string(&decode_address(row)?),
        TeslaMateStageTable::Geofences => serde_json::to_string(&decode_geofence(row)?),
        TeslaMateStageTable::States => serde_json::to_string(&decode_state(row)?),
        TeslaMateStageTable::Updates => serde_json::to_string(&decode_update(row)?),
    }?;
    Ok(encoded)
}

/// Materialize a sealed capture for isolated compatibility tests only. Normal
/// import publication uses the staged fragment producer and never calls this
/// all-memory helper.
pub fn materialize_small_staged_history(
    stage: &TeslaMateStage,
    maximum_rows: usize,
) -> Result<TeslaMateHistory, TeslaMateReaderError> {
    let stats = stage.stats()?;
    if stats.row_count > u64::try_from(maximum_rows).expect("usize fits u64") {
        return Err(TeslaMateReaderError::MaximumRowsExceeded {
            maximum: maximum_rows,
        });
    }
    Ok(TeslaMateHistory {
        cars: collect_staged_rows(stage, TeslaMateStageTable::Cars)?,
        drives: collect_staged_rows(stage, TeslaMateStageTable::Drives)?,
        positions: collect_staged_rows(stage, TeslaMateStageTable::Positions)?,
        charging_processes: collect_staged_rows(stage, TeslaMateStageTable::ChargingProcesses)?,
        charges: collect_staged_rows(stage, TeslaMateStageTable::Charges)?,
        addresses: collect_staged_rows(stage, TeslaMateStageTable::Addresses)?,
        geofences: collect_staged_rows(stage, TeslaMateStageTable::Geofences)?,
        states: collect_staged_rows(stage, TeslaMateStageTable::States)?,
        updates: collect_staged_rows(stage, TeslaMateStageTable::Updates)?,
    })
}

fn collect_staged_rows<T: DeserializeOwned>(
    stage: &TeslaMateStage,
    table: TeslaMateStageTable,
) -> Result<Vec<T>, TeslaMateReaderError> {
    let mut after_id = 0_i64;
    let mut output = Vec::new();
    loop {
        let page = stage.page(table, after_id, 10_000)?;
        output.extend(page.rows.into_iter().map(|row| row.value));
        match page.next_after_id {
            Some(next_after_id) => after_id = next_after_id,
            None => return Ok(output),
        }
    }
}

async fn capture_history_in_session(
    client: &Client,
    source: &ReadOnlySource,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    stage: &mut TeslaMateStage,
) -> Result<(), TeslaMateReaderError> {
    prepare_read_only_snapshot(client, source, limits).await?;

    let mut retained_rows = 0_usize;
    let cars = capture_smallint_pages(
        client,
        StageProjection {
            source_table: SourceTable::Cars,
            stage_table: TeslaMateStageTable::Cars,
            decode: decode_car,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    if cars == 0 {
        return Err(TeslaMateReaderError::SelectedCarMissing {
            selected_car_id: i64::from(selected_car_id),
        });
    }
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::Drives,
            stage_table: TeslaMateStageTable::Drives,
            decode: decode_drive,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::Positions,
            stage_table: TeslaMateStageTable::Positions,
            decode: decode_position,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::ChargingProcesses,
            stage_table: TeslaMateStageTable::ChargingProcesses,
            decode: decode_charging_process,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::Charges,
            stage_table: TeslaMateStageTable::Charges,
            decode: decode_charge,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::Addresses,
            stage_table: TeslaMateStageTable::Addresses,
            decode: decode_address,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    capture_geofence_pages(client, selected_car_id, limits, &mut retained_rows, stage).await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::States,
            stage_table: TeslaMateStageTable::States,
            decode: decode_state,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    capture_integer_pages(
        client,
        StageProjection {
            source_table: SourceTable::Updates,
            stage_table: TeslaMateStageTable::Updates,
            decode: decode_update,
        },
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await?;
    Ok(())
}

struct StageProjection<T> {
    source_table: SourceTable,
    stage_table: TeslaMateStageTable,
    decode: fn(&Row) -> Result<T, TeslaMateReaderError>,
}

async fn capture_smallint_pages<T: Serialize + Sync>(
    client: &Client,
    projection_descriptor: StageProjection<T>,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<usize, TeslaMateReaderError> {
    let mut last_id = 0_i16;
    let mut captured_rows = 0_usize;
    let page_size = i64::from(limits.page_size);
    loop {
        let page = client
            .query(
                projection(projection_descriptor.source_table).sql,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = page.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in page {
            let id = required_i16(&row, projection_descriptor.source_table.name(), "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage {
                    table: projection_descriptor.source_table.name(),
                });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            decoded.push((i64::from(id), (projection_descriptor.decode)(&row)?));
        }
        captured_rows = captured_rows.checked_add(page_len).ok_or(
            TeslaMateReaderError::MaximumRowsExceeded {
                maximum: limits.maximum_rows,
            },
        )?;
        stage.insert_page_parallel(projection_descriptor.stage_table, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(captured_rows);
        }
    }
}

async fn capture_integer_pages<T: Serialize + Sync>(
    client: &Client,
    projection_descriptor: StageProjection<T>,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<usize, TeslaMateReaderError> {
    let mut last_id = 0_i32;
    let mut captured_rows = 0_usize;
    let page_size = i64::from(limits.page_size);
    loop {
        let page = client
            .query(
                projection(projection_descriptor.source_table).sql,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = page.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in page {
            let id = required_i32(&row, projection_descriptor.source_table.name(), "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage {
                    table: projection_descriptor.source_table.name(),
                });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            decoded.push((i64::from(id), (projection_descriptor.decode)(&row)?));
        }
        captured_rows = captured_rows.checked_add(page_len).ok_or(
            TeslaMateReaderError::MaximumRowsExceeded {
                maximum: limits.maximum_rows,
            },
        )?;
        stage.insert_page_parallel(projection_descriptor.stage_table, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(captured_rows);
        }
    }
}

const GEOFENCE_GEOMETRY_SQL: &str = r#"
SELECT
  source.id,
  source.name,
  source.latitude::double precision AS latitude,
  source.longitude::double precision AS longitude,
  source.radius::double precision AS radius_m,
  source.billing_type::text AS billing_type,
  source.cost_per_unit::double precision AS cost_per_unit,
  source.session_fee::double precision AS session_fee
FROM public.geofences AS source
WHERE source.id > $1
  AND (
    EXISTS (
      SELECT 1 FROM public.drives AS drive
      WHERE drive.car_id = $3
        AND (drive.start_geofence_id = source.id OR drive.end_geofence_id = source.id)
    )
    OR EXISTS (
      SELECT 1 FROM public.charging_processes AS process
      WHERE process.car_id = $3 AND process.geofence_id = source.id
    )
  )
ORDER BY source.id ASC
LIMIT $2
"#;

// This is deliberately a sibling of the legacy geometry query.  It is the
// bounded THP2.2 local-candidate source shape only; `read_geofences` and the
// existing TeslaMate import compatibility path must keep their legacy f64
// representation and query unchanged.
// This is deliberately a separate physical query from `ADDRESSES_SQL` and
// `read_addresses`. It is bounded local-candidate work only; the existing
// compatibility reader keeps its three-column binary-copy shape unchanged.
// This is deliberately separate from the legacy binary-copy drive reader.
// It retains every selected-car physical source field without completed-row
// filtering, joins, casts, defaults, or time/numeric/float normalization.
// This is deliberately separate from the legacy binary-copy positions reader.
// It retains all selected-car physical source columns without joins, casts,
// defaults, coordinate policy, or timestamp/numeric/FLOAT8 normalization.
// Dedicated source-shaped local-candidate readers for charging history. These
// do not reuse compatibility charge/session projection: every selected-car
// process is direct, while charge rows are scoped only through an extant
// process INNER JOIN. That scope asserts selected ownership only; source
// constraint state is not re-attested by the local physical slice.
// Dedicated signed-id physical source queries for the THP2.2 local candidate.
// They deliberately do not reuse legacy COPY readers or compatibility epoch-ms
// decoders: PostgreSQL timestamp binary i64 microseconds remain raw, including
// the source infinity sentinels.
#[allow(dead_code)] // local candidate only; import/publication wiring is deliberately absent.
const UPDATES_V2_2_SQL: &str = r#"
SELECT
  source.id,
  source.car_id,
  source.start_date,
  source.end_date,
  source.version
FROM public.updates AS source
WHERE ($1::integer IS NULL OR source.id > $1)
  AND source.car_id = $3
ORDER BY source.id ASC
LIMIT $2
"#;

// Source-wide singleton settings are intentionally separate from selected-car
// history. Cast only the four PostgreSQL enum values to text so the physical
// reader can decode their reviewed labels without a global enum codec.
#[allow(dead_code)] // local candidate only; import/publication wiring is deliberately absent.
const SETTINGS_V2_2_SQL: &str = r#"
SELECT
  source.id,
  source.unit_of_length::text AS unit_of_length,
  source.unit_of_temperature::text AS unit_of_temperature,
  source.unit_of_pressure::text AS unit_of_pressure,
  source.preferred_range::text AS preferred_range,
  source.base_url,
  source.grafana_url,
  source.language,
  source.theme_mode,
  source.inserted_at,
  source.updated_at
FROM public.settings AS source
ORDER BY source.id ASC
LIMIT 2
"#;

// This is a dedicated, selected-car physical source relation for the THP2.2
// local candidate. It deliberately does not reuse the legacy `CARS` query or
// its lossy/default-resolving decoder, and it never joins global `settings`.
#[allow(dead_code)] // local candidate only; import/publication wiring is deliberately absent.
const CARS_AND_CAR_SETTINGS_V2_2_SQL: &str = r#"
SELECT
  source.id,
  source.eid,
  source.vid,
  source.vin,
  source.name,
  source.model,
  source.efficiency,
  source.trim_badging,
  source.marketing_name,
  source.exterior_color,
  source.wheel_type,
  source.spoiler_type,
  source.display_priority,
  source.inserted_at,
  source.updated_at,
  source.settings_id,
  car_settings.id AS car_settings_row_id,
  car_settings.suspend_min,
  car_settings.suspend_after_idle_min,
  car_settings.req_not_unlocked,
  car_settings.free_supercharging,
  car_settings.use_streaming_api,
  car_settings.enabled,
  car_settings.lfp_battery
FROM public.cars AS source
INNER JOIN public.car_settings AS car_settings ON car_settings.id = source.settings_id
WHERE source.id = $1
ORDER BY source.id ASC
LIMIT 1
"#;

async fn capture_geofence_pages(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<usize, TeslaMateReaderError> {
    let mut last_id = 0_i32;
    let mut captured_rows = 0_usize;
    let page_size = i64::from(limits.page_size);
    loop {
        let page = client
            .query(
                GEOFENCE_GEOMETRY_SQL,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = page.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in page {
            let id = required_i32(&row, "geofences", "id")?;
            if id <= last_id {
                return Err(TeslaMateReaderError::NonProgressingPage { table: "geofences" });
            }
            last_id = id;
            retain_row(retained_rows, limits.maximum_rows)?;
            decoded.push((i64::from(id), decode_geofence(&row)?));
        }
        captured_rows = captured_rows.checked_add(page_len).ok_or(
            TeslaMateReaderError::MaximumRowsExceeded {
                maximum: limits.maximum_rows,
            },
        )?;
        stage.insert_page_parallel(TeslaMateStageTable::Geofences, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(captured_rows);
        }
    }
}

async fn read_history_in_session(
    client: &Client,
    source: &ReadOnlySource,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateHistory, TeslaMateReaderError> {
    prepare_read_only_snapshot(client, source, limits).await?;

    let mut retained_rows = 0_usize;
    let cars = read_cars(client, selected_car_id, limits, &mut retained_rows).await?;
    if cars.is_empty() {
        return Err(TeslaMateReaderError::SelectedCarMissing {
            selected_car_id: i64::from(selected_car_id),
        });
    }
    let drives = read_drives(client, selected_car_id, limits, &mut retained_rows).await?;
    let positions = read_positions(client, selected_car_id, limits, &mut retained_rows).await?;
    let charging_processes =
        read_charging_processes(client, selected_car_id, limits, &mut retained_rows).await?;
    let charges = read_charges(client, selected_car_id, limits, &mut retained_rows).await?;
    let addresses = read_addresses(client, selected_car_id, limits, &mut retained_rows).await?;
    let geofences = read_geofences(client, selected_car_id, limits, &mut retained_rows).await?;
    let states = read_states(client, selected_car_id, limits, &mut retained_rows).await?;
    let updates = read_updates(client, selected_car_id, limits, &mut retained_rows).await?;

    Ok(TeslaMateHistory {
        cars,
        drives,
        positions,
        charging_processes,
        charges,
        addresses,
        geofences,
        states,
        updates,
    })
}
