// SPDX-License-Identifier: AGPL-3.0-only

/// Capture bounded, source-shaped roots and selected-car relation rows for the
/// physical V3 source slice. Callers receive one private sealed stage and
/// must either consume or discard it explicitly.
pub async fn capture_physical_v3_to_stage(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
    imports_dir: &Path,
) -> Result<TeslaMateStage, TeslaMateReaderError> {
    let (stage, owner, selected_car_id) = begin_physical_v3_capture(
        source,
        password,
        selected_car_id,
        limits,
        imports_dir,
    )
    .await?;
    finish_physical_v3_capture(
        stage,
        owner,
        source,
        password,
        selected_car_id,
        limits,
    )
    .await
}

#[cfg(test)]
async fn capture_physical_v3_to_stage_with_post_export<F, Fut>(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
    imports_dir: &Path,
    post_export: F,
) -> Result<TeslaMateStage, TeslaMateReaderError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), TeslaMateReaderError>>,
{
    let (stage, owner, selected_car_id) = begin_physical_v3_capture(
        source,
        password,
        selected_car_id,
        limits,
        imports_dir,
    )
    .await?;
    if let Err(error) = post_export().await {
        let _ = owner.finish().await;
        return Err(discard_stage_after_error(stage, error));
    }
    finish_physical_v3_capture(
        stage,
        owner,
        source,
        password,
        selected_car_id,
        limits,
    )
    .await
}

async fn begin_physical_v3_capture(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
    imports_dir: &Path,
) -> Result<(TeslaMateStage, ExportedSnapshotLease, i16), TeslaMateReaderError> {
    limits.validate()?;
    if selected_car_id <= 0 {
        return Err(TeslaMateReaderError::InvalidSelectedCarId);
    }

    let stage = TeslaMateStage::create_physical_v3(
        imports_dir,
        TeslaMateStageLimits {
            max_rows: u64::try_from(limits.maximum_rows).expect("usize fits u64"),
            max_stage_bytes: limits.maximum_stage_bytes,
            minimum_free_bytes: limits.minimum_free_bytes,
        },
    )?;
    let (owner, selected_car_id, _schema) = match open_exported_snapshot_lease(
        source,
        password,
        selected_car_id,
        limits,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return Err(discard_stage_after_error(stage, error)),
    };
    Ok((stage, owner, selected_car_id))
}

async fn finish_physical_v3_capture(
    mut stage: TeslaMateStage,
    owner: ExportedSnapshotLease,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateStage, TeslaMateReaderError> {
    let capture = capture_physical_v3_from_exported_snapshot(
        &owner,
        source,
        password,
        selected_car_id,
        limits,
        &mut stage,
    )
    .await;
    let owner_result = owner.finish().await;
    if let Err(error) = capture {
        return Err(discard_stage_after_error(stage, error));
    }
    if let Err(error) = owner_result {
        return Err(discard_stage_after_error(stage, error));
    }
    if let Err(error) = stage.seal() {
        return Err(discard_stage_after_error(
            stage,
            TeslaMateReaderError::Stage(error),
        ));
    }
    Ok(stage)
}

/// Retain the full physical image from the direct import's existing snapshot.
/// Keeping the lease shared also binds the legacy cutover and token capture
/// to precisely the rows that the phone will activate.
pub(crate) async fn capture_physical_v3_from_lease(
    owner: &ExportedSnapshotLease,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    imports_dir: &Path,
) -> Result<crate::teslamate_stage::OwnedTeslaMateStage, TeslaMateReaderError> {
    let stage = TeslaMateStage::create_physical_v3(
        imports_dir,
        TeslaMateStageLimits {
            max_rows: u64::try_from(limits.maximum_rows).expect("usize fits u64"),
            max_stage_bytes: limits.maximum_stage_bytes,
            minimum_free_bytes: limits.minimum_free_bytes,
        },
    )?;
    let mut owned = crate::teslamate_stage::OwnedTeslaMateStage::new(stage);
    capture_physical_v3_from_exported_snapshot(
        owner, source, password, selected_car_id, limits, owned.stage_mut(),
    ).await?;
    owned.stage_mut().seal()?;
    Ok(owned)
}

async fn capture_physical_v3_from_exported_snapshot(
    owner: &ExportedSnapshotLease,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    stage: &mut TeslaMateStage,
) -> Result<(), TeslaMateReaderError> {
    let mut retained_rows = 0_usize;
    let global_settings =
        read_settings_v2_2(owner.client(), limits, &mut retained_rows).await?;
    require_positive_physical_id("settings", global_settings.id)?;
    let car = read_car_v2_2(
        owner.client(),
        selected_car_id,
        limits,
        &mut retained_rows,
    )
    .await?;
    let car_settings = read_car_settings_v2_2(
        owner.client(),
        car.settings_id,
        limits,
        &mut retained_rows,
    )
    .await?;

    stage.insert(
        TeslaMateStageTable::GlobalSettings,
        global_settings.id,
        &global_settings,
    )?;
    stage.insert(TeslaMateStageTable::Cars, i64::from(car.id), &car)?;
    stage.insert(
        TeslaMateStageTable::CarSettings,
        car_settings.id,
        &car_settings,
    )?;

    let lane = open_snapshot_capture_lane(
        source,
        password,
        owner.snapshot_id(),
        limits,
    )
    .await?;
    let drives = capture_physical_drive_pages(
        lane.client(),
        selected_car_id,
        limits,
        &mut retained_rows,
        stage,
    )
    .await;
    let positions = match drives {
        Ok(()) => {
            capture_physical_position_pages(
                lane.client(),
                selected_car_id,
                limits,
                &mut retained_rows,
                stage,
            )
            .await
        }
        Err(error) => Err(error),
    };
    let charging_processes = match positions {
        Ok(()) => {
            capture_physical_charging_process_pages(
                lane.client(),
                selected_car_id,
                limits,
                &mut retained_rows,
                stage,
            )
            .await
        }
        Err(error) => Err(error),
    };
    let charges = match charging_processes {
        Ok(()) => {
            capture_physical_charge_pages(
                lane.client(),
                selected_car_id,
                limits,
                &mut retained_rows,
                stage,
            )
            .await
        }
        Err(error) => Err(error),
    };
    let addresses = match charges {
        Ok(()) => {
            capture_physical_address_pages(
                lane.client(),
                selected_car_id,
                limits,
                &mut retained_rows,
                stage,
            )
            .await
        }
        Err(error) => Err(error),
    };
    let geofences = match addresses {
        Ok(()) => {
            capture_physical_geofence_pages(
                lane.client(),
                selected_car_id,
                limits,
                &mut retained_rows,
                stage,
            )
            .await
        }
        Err(error) => Err(error),
    };
    let states = match geofences {
        Ok(()) => {
            capture_physical_state_pages(
                lane.client(),
                selected_car_id,
                limits,
                &mut retained_rows,
                stage,
            )
            .await
        }
        Err(error) => Err(error),
    };
    let capture = match states {
        Ok(()) => {
            capture_physical_update_pages(
                lane.client(),
                selected_car_id,
                limits,
                &mut retained_rows,
                stage,
            )
            .await
        }
        Err(error) => Err(error),
    };
    let lane_result = lane.finish().await;
    match (capture, lane_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(error),
    }
}

async fn capture_physical_address_pages(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<(), TeslaMateReaderError> {
    let page_size = i64::from(limits.page_size);
    let mut last_id = None::<i32>;
    loop {
        let rows = client
            .query(
                ADDRESSES_V2_2_SQL,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = rows.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in rows {
            let id = required_i32(&row, "addresses", "id")?;
            last_id = advance_signed_v2_2_cursor(last_id, id, "addresses")?;
            require_positive_physical_id("addresses", i64::from(id))?;
            retain_row(retained_rows, limits.maximum_rows)?;
            decoded.push((i64::from(id), decode_address_v2_2(&row)?));
        }
        stage.insert_page_parallel(TeslaMateStageTable::Addresses, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(());
        }
    }
}

async fn capture_physical_geofence_pages(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<(), TeslaMateReaderError> {
    let page_size = i64::from(limits.page_size);
    let mut last_id = None::<i32>;
    loop {
        let rows = client
            .query(
                GEOFENCES_V2_2_SQL,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = rows.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in rows {
            let id = required_i32(&row, "geofences", "id")?;
            last_id = advance_signed_v2_2_cursor(last_id, id, "geofences")?;
            require_positive_physical_id("geofences", i64::from(id))?;
            retain_row(retained_rows, limits.maximum_rows)?;
            decoded.push((i64::from(id), decode_geofence_v2_2(&row)?));
        }
        stage.insert_page_parallel(TeslaMateStageTable::Geofences, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(());
        }
    }
}

async fn capture_physical_charge_pages(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<(), TeslaMateReaderError> {
    let page_size = i64::from(limits.page_size);
    let mut last_id = None::<i32>;
    loop {
        let rows = client
            .query(CHARGES_V2_2_SQL, &[&last_id, &page_size, &selected_car_id])
            .await?;
        let page_len = rows.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in rows {
            let id = required_i32(&row, "charges", "id")?;
            last_id = advance_signed_v2_2_cursor(last_id, id, "charges")?;
            require_positive_physical_id("charges", i64::from(id))?;
            retain_row(retained_rows, limits.maximum_rows)?;
            decoded.push((i64::from(id), decode_charge_v2_2(&row)?));
        }
        stage.insert_page_parallel(TeslaMateStageTable::Charges, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(());
        }
    }
}

async fn capture_physical_charging_process_pages(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<(), TeslaMateReaderError> {
    let page_size = i64::from(limits.page_size);
    let mut last_id = None::<i32>;
    loop {
        let rows = client
            .query(
                CHARGING_PROCESSES_V2_2_SQL,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = rows.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in rows {
            let id = required_i32(&row, "charging_processes", "id")?;
            last_id = advance_signed_v2_2_cursor(last_id, id, "charging_processes")?;
            require_positive_physical_id("charging_processes", i64::from(id))?;
            retain_row(retained_rows, limits.maximum_rows)?;
            let process = decode_charging_process_v2_2(&row)?;
            if process.car_id != selected_car_id {
                return Err(TeslaMateReaderError::NonProgressingPage {
                    table: "charging_processes",
                });
            }
            decoded.push((i64::from(id), process));
        }
        stage.insert_page_parallel(TeslaMateStageTable::ChargingProcesses, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(());
        }
    }
}

async fn capture_physical_drive_pages(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<(), TeslaMateReaderError> {
    let page_size = i64::from(limits.page_size);
    let mut last_id = None::<i32>;
    loop {
        let rows = client
            .query(DRIVES_V2_2_SQL, &[&last_id, &page_size, &selected_car_id])
            .await?;
        let page_len = rows.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in rows {
            let id = required_i32(&row, "drives", "id")?;
            last_id = advance_signed_v2_2_cursor(last_id, id, "drives")?;
            require_positive_physical_id("drives", i64::from(id))?;
            retain_row(retained_rows, limits.maximum_rows)?;
            let drive = decode_drive_v2_2(&row)?;
            if drive.car_id != selected_car_id {
                return Err(TeslaMateReaderError::NonProgressingPage { table: "drives" });
            }
            decoded.push((i64::from(id), drive));
        }
        stage.insert_page_parallel(TeslaMateStageTable::Drives, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(());
        }
    }
}

async fn capture_physical_position_pages(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<(), TeslaMateReaderError> {
    let page_size = i64::from(limits.page_size);
    let mut last_id = None::<i32>;
    loop {
        let rows = client
            .query(
                POSITIONS_V2_2_SQL,
                &[&last_id, &page_size, &selected_car_id],
            )
            .await?;
        let page_len = rows.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in rows {
            let id = required_i32(&row, "positions", "id")?;
            last_id = advance_signed_v2_2_cursor(last_id, id, "positions")?;
            require_positive_physical_id("positions", i64::from(id))?;
            retain_row(retained_rows, limits.maximum_rows)?;
            let position = decode_position_v2_2(&row)?;
            if position.car_id != selected_car_id {
                return Err(TeslaMateReaderError::NonProgressingPage { table: "positions" });
            }
            decoded.push((i64::from(id), position));
        }
        stage.insert_page_parallel(TeslaMateStageTable::Positions, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(());
        }
    }
}

async fn capture_physical_state_pages(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<(), TeslaMateReaderError> {
    let page_size = i64::from(limits.page_size);
    let mut last_id = None::<i32>;
    loop {
        let rows = client
            .query(STATES_V2_2_SQL, &[&last_id, &page_size, &selected_car_id])
            .await?;
        let page_len = rows.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in rows {
            let id = required_i32(&row, "states", "id")?;
            last_id = advance_signed_v2_2_cursor(last_id, id, "states")?;
            require_positive_physical_id("states", i64::from(id))?;
            retain_row(retained_rows, limits.maximum_rows)?;
            let state = decode_state_v2_2(&row)?;
            if state.car_id != selected_car_id {
                return Err(TeslaMateReaderError::NonProgressingPage { table: "states" });
            }
            decoded.push((i64::from(id), state));
        }
        stage.insert_page_parallel(TeslaMateStageTable::States, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(());
        }
    }
}

async fn capture_physical_update_pages(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    stage: &mut TeslaMateStage,
) -> Result<(), TeslaMateReaderError> {
    let page_size = i64::from(limits.page_size);
    let mut last_id = None::<i32>;
    loop {
        let rows = client
            .query(UPDATES_V2_2_SQL, &[&last_id, &page_size, &selected_car_id])
            .await?;
        let page_len = rows.len();
        let mut decoded = Vec::with_capacity(page_len);
        for row in rows {
            let id = required_i32(&row, "updates", "id")?;
            last_id = advance_signed_v2_2_cursor(last_id, id, "updates")?;
            require_positive_physical_id("updates", i64::from(id))?;
            retain_row(retained_rows, limits.maximum_rows)?;
            let update = decode_update_v2_2(&row)?;
            if update.car_id != selected_car_id {
                return Err(TeslaMateReaderError::NonProgressingPage { table: "updates" });
            }
            decoded.push((i64::from(id), update));
        }
        stage.insert_page_parallel(TeslaMateStageTable::Updates, decoded)?;
        if page_len < limits.page_size as usize {
            return Ok(());
        }
    }
}
