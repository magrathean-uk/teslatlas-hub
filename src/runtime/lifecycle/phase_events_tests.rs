// SPDX-License-Identifier: AGPL-3.0-only

//! Part P spec tests, second half (split from `phase_tests.rs` to keep each
//! source file under the repository line limit): the Hub events (R15) and the
//! blob and log checks. Shorthands and fixtures come from the parent module.

use super::*;

// ------------------------------------------------------- Hub events (R15)

#[test]
fn st82_force_close_functions_equal_the_hub_events() {
    let mut state = OpenSessionState::new();
    state.open_drive = Some(open_drive(1, T0));
    state.next_drive_id = 2;
    state.open_charge = Some(open_charge(1, T0, T0 + 5 * MIN));
    state.last_observation_id = 41;
    state.last_observed_at_ms = Some(T0 + 5 * MIN);

    let closed_at = T0 + 40 * MIN;
    let forced = force_close_open_sessions(state.clone(), 1, closed_at).expect("force close");
    let event = step(state.clone(), &e(42, closed_at, "no_data"));
    assert_eq!(forced.delta, event.delta);
    assert_eq!(forced.quarantined, event.quarantined);
    let mut expected = event.state.clone();
    expected.last_observation_id = 41;
    assert_eq!(forced.state, expected);

    let forced = force_close_for_service(state.clone(), 1, closed_at).expect("service close");
    let event = step(state.clone(), &e(42, closed_at, "service_refused"));
    assert_eq!(forced.delta, event.delta);
    let mut expected = event.state;
    expected.last_observation_id = 41;
    assert_eq!(forced.state, expected);
    assert_eq!(in_service(&forced.state), (Some(true), true));

    // (c) An event before the drive's start is applied without an error.
    let early = force_close_for_service(state.clone(), 1, T0 - HOUR).expect("early event");
    assert_eq!(in_service(&early.state), (Some(true), true));
    let early = force_close_open_sessions(state.clone(), 1, T0 - HOUR).expect("early event");
    assert_eq!(early.state.last_observation_id, 41);

    // No observation id to spend: an exhausted cursor is an error.
    let mut exhausted = state;
    exhausted.last_observation_id = i64::MAX;
    assert_eq!(
        force_close_open_sessions(exhausted, 1, closed_at),
        Err(LifecycleError::IdentifierExhausted)
    );
}

#[test]
fn st82_an_event_dated_before_the_drive_still_closes_it_at_its_last_fold() {
    // R15, R43 item 4: the `no_data` event is itself a close trigger, so an
    // event dated before the drive (the Hub clock behind) or within `T_gap` of
    // the last fold closes it, and the end is the last fold, never the event.
    let folds = |state: OpenSessionState| {
        all(
            state,
            &[
                v(
                    1,
                    T0,
                    json!({"drive_state": {"timestamp": T0, "shift_state": "D", "speed": 20,
                                           "latitude": 10.0, "longitude": 20.0}}),
                ),
                v(
                    2,
                    T0 + MIN,
                    json!({"drive_state": {"timestamp": T0 + MIN, "shift_state": "D", "speed": 20,
                                           "latitude": 10.01, "longitude": 20.0}}),
                ),
            ],
        )
        .state
    };
    for event_time in [T0 - HOUR, T0 + 2 * MIN, T0 + 5 * MIN] {
        let result = apply_sample(
            folds(OpenSessionState::new()),
            1,
            &e(3, event_time, "no_data"),
        )
        .expect("no error whatever the event time");
        assert!(result.state.open_drive.is_none(), "{event_time}");
        assert_eq!(result.delta.drives.len(), 1, "{event_time}");
        assert_eq!(result.delta.drives[0].end_date_ms, T0 + MIN, "{event_time}");
    }
    // The same through the forcing function, with the early time.
    let forced = force_close_open_sessions(folds(OpenSessionState::new()), 1, T0 - HOUR)
        .expect("force close");
    assert_eq!(forced.delta.drives.len(), 1);
    assert_eq!(forced.delta.drives[0].end_date_ms, T0 + MIN);
    // An unknown event kind leaves the drive open.
    let unknown = apply_sample(
        folds(OpenSessionState::new()),
        1,
        &e(3, T0 + 5 * MIN, "later_kind"),
    )
    .expect("unknown event");
    assert!(unknown.state.open_drive.is_some());
}

#[test]
fn st82_no_data_closes_the_drive_but_not_the_charge_part_d_and_c() {
    let mut state = OpenSessionState::new();
    for (id, t, latitude) in [(1, T0, 10.0), (2, T0 + MIN, 10.01)] {
        state = step(
            state,
            &v(
                id,
                t,
                json!({"drive_state": {"timestamp": t, "shift_state": "D", "speed": 20,
                                       "latitude": latitude, "longitude": 20.0}}),
            ),
        )
        .state;
    }
    let result = step(state, &e(3, T0 + 30 * MIN, "no_data"));
    assert_eq!(result.delta.drives.len(), 1);
    assert_eq!(result.delta.drives[0].end_date_ms, T0 + MIN);
    assert!(result.state.open_drive.is_none());
    // The state interval is unchanged by the event.
    assert_eq!(result.state.last_reach.as_deref(), Some("online"));
    // An open charge stays open: its next sample can still prove continuity.
    let mut with_charge = OpenSessionState::new();
    with_charge.open_charge = Some(open_charge(1, T0, T0 + 30 * MIN));
    let result = step(with_charge, &e(1, T0 + 2 * HOUR, "no_data"));
    assert!(result.state.open_charge.is_some());
    assert!(result.delta.charges.is_empty());
}

// --------------------------------------------------------- blob and logs

fn long_text(length: usize, filler: char) -> String {
    std::iter::repeat_n(filler, length).collect()
}

#[test]
fn st74_worst_case_blob_stays_under_half_the_storage_limit() {
    // Every optional field is set to a long value; the numbers use the longest
    // shortest-round-trip text an f64 can have.
    let real = -1.234_567_890_123_456_7e-300_f64;
    let big = i64::MIN + 1;
    let mut drive: OpenDrive = serde_json::from_value(json!({
        "id": big, "car_id": big, "start_date_ms": big,
        "outside_temp_sum": real, "outside_temp_count": u32::MAX
    }))
    .unwrap();
    drive.start_latitude = Some(real);
    drive.start_longitude = Some(real);
    drive.start_soc = Some(big);
    drive.start_rated_range_km = Some(real);
    drive.speed_max = Some(big);
    drive.position_count = u32::MAX;
    drive.last_position_date_ms = Some(big);
    drive.last_latitude = Some(real);
    drive.last_longitude = Some(real);
    drive.last_soc = Some(big);
    drive.last_rated_range_km = Some(real);
    drive.last_odometer = Some(real);
    drive.first_odometer = Some(real);
    drive.power_max = Some(real);
    drive.power_min = Some(real);
    drive.inside_temp_sum = real;
    drive.inside_temp_count = u32::MAX;
    drive.start_ideal_range_km = Some(real);
    drive.end_ideal_range_km = Some(real);
    drive.elevation_ascent = big;
    drive.elevation_descent = big;
    drive.last_elevation = Some(big);
    drive.saw_offline = true;
    drive.last_charge_energy_added = Some(real);
    drive.last_ideal_range_km = Some(real);
    drive.last_folded_at_ms = Some(big);
    drive.last_motion_at_ms = Some(big);
    drive.max_displacement_m = real;
    drive.elevation_count = u32::MAX;
    drive.odometer_coarse = true;
    let mut checkpoint = drive.clone();
    checkpoint.still_checkpoint = None;
    // R87 (ST74 amended): K_LEAD_CHECKPOINTS lead checkpoints, each a copy of
    // the drive with every optional field at its longest value and nothing
    // nested, with the longest cut.
    drive.lead_checkpoints = (0..K_LEAD_CHECKPOINTS as i64)
        .map(|k| LeadCheckpoint {
            cut_ms: big + k,
            drive: checkpoint.clone(),
        })
        .collect();
    drive.still_checkpoint = Some(Box::new(checkpoint));

    let mut charge = open_charge(big, big, big);
    charge.car_id = big;
    charge.start_battery_level = Some(big);
    charge.start_ideal_range_km = Some(real);
    charge.start_rated_range_km = Some(real);
    charge.start_latitude = Some(real);
    charge.start_longitude = Some(real);
    charge.is_dc = Some(true);
    charge.fast_charger_type = Some(long_text(255, 'f'));
    charge.max_charger_power_kw = Some(real);
    charge.outside_temp_sum = real;
    charge.outside_temp_count = u32::MAX;
    charge.first_energy_added = Some(real);
    charge.max_energy_added = Some(real);
    charge.last_energy_added = Some(real);
    charge.last_battery_level = Some(big);
    charge.last_ideal_range_km = Some(real);
    charge.last_rated_range_km = Some(real);
    charge.sample_count = u32::MAX;
    charge.energy_used_kwh = Some(real);
    charge.last_sample_power_kw = Some(real);
    charge.last_odometer_km = Some(real);

    let text = long_text(255, 'm');
    let mut state = OpenSessionState::new();
    state.last_observation_id = i64::MAX;
    state.id_cursors_seeded = true;
    state.last_observed_at_ms = Some(big);
    state.last_drive_timestamp_ms = Some(big);
    state.last_charge_timestamp_ms = Some(big);
    state.last_vehicle_timestamp_ms = Some(big);
    state.imported_drive_watermark_ms = Some(big);
    state.imported_charge_watermark_ms = Some(big);
    state.imported_state_watermark_ms = Some(big);
    state.next_drive_id = i64::MAX - 1;
    state.next_position_id = i64::MAX - 1;
    state.next_charge_id = i64::MAX - 1;
    state.next_charge_sample_id = i64::MAX - 1;
    state.next_state_id = i64::MAX - 1;
    state.next_update_id = i64::MAX - 1;
    state.last_stationary_position_at_ms = Some(big);
    state.phase = VehiclePhase::Updating;
    state.open_drive = Some(drive);
    state.pending_gained_range_charge = Some(GainedRangeChargeSeed {
        start_date_ms: big,
        start_battery_level: Some(big),
        start_ideal_range_km: Some(real),
        start_rated_range_km: Some(real),
        start_latitude: Some(real),
        start_longitude: Some(real),
        first_energy_added: real,
    });
    state.open_charge = Some(charge);
    state.open_state = Some(OpenState {
        id: big,
        car_id: big,
        state: long_text(255, 's'),
        start_date_ms: big,
    });
    state.open_update = Some(OpenUpdate {
        id: big,
        car_id: big,
        start_date_ms: big,
    });
    state.imported_open = Some(ImportedOpenSessionRefs {
        source_id: uuid::Uuid::from_u128(u128::MAX).to_string(),
        content_sha256: long_text(64, 'a'),
        drive_source_row_id: Some(big),
        drive_position_count: u64::MAX,
        charge_source_row_id: Some(big),
        charge_sample_count: u64::MAX,
        state_source_row_id: Some(big),
        standalone_position_count: u64::MAX,
    });
    state.service_mode = Some(true);
    state.car_metadata = Some(ProjectionCarPatch {
        name: Some(text.clone()),
        model: Some(text.clone()),
        vin: Some(text.clone()),
        trim_badging: Some(text.clone()),
        marketing_name: Some(text.clone()),
        exterior_color: Some(text.clone()),
        wheel_type: Some(text.clone()),
        spoiler_type: Some(text.clone()),
        firmware_version: Some(text.clone()),
    });
    state.last_position_battery_heater = Some(true);
    state.last_position_battery_heater_on = Some(true);
    state.last_position_battery_heater_no_power = Some(true);
    state.last_position_est_battery_range_km = Some(real);
    state.last_position_fan_status = Some(big);
    state.last_position_driver_temp_setting = Some(real);
    state.last_position_passenger_temp_setting = Some(real);
    state.last_position_is_rear_defroster_on = Some(true);
    state.last_position_is_front_defroster_on = Some(true);
    state.last_position_tpms_pressure_fl = Some(real);
    state.last_position_tpms_pressure_fr = Some(real);
    state.last_position_tpms_pressure_rl = Some(real);
    state.last_position_tpms_pressure_rr = Some(real);
    state.parked_snapshot = Some(ParkedSnapshot {
        at_ms: Some(big),
        seen_at_ms: big,
        battery_level: Some(big),
        ideal_range_km: Some(real),
        rated_range_km: Some(real),
        odometer_km: Some(real),
        odometer_coarse: true,
        latitude: Some(real),
        longitude: Some(real),
        energy_added_kwh: Some(real),
        after_drive: true,
        unplugged: true,
    });
    state.last_car_version = Some(text.clone());
    state.last_car_version_at_ms = Some(big);
    state.last_standalone_latitude = Some(real);
    state.last_standalone_longitude = Some(real);
    state.last_reach = Some("offline".to_owned());
    state.last_position_fact_ms = Some(big);
    state.last_battery_fact_ms = Some(big);
    state.last_climate_fact_ms = Some(big);
    state.last_odometer_fact_ms = Some(big);
    state.service_mode_refused = true;

    let bytes = state.encode().expect("worst-case blob encodes");
    println!("ST74 worst-case open-session blob: {} bytes", bytes.len());
    assert!(
        bytes.len() < 32_768,
        "worst-case blob is {} bytes, the budget is 32768",
        bytes.len()
    );
    assert_eq!(OpenSessionState::decode(&bytes).expect("decodes"), state);
}

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buffer").extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn st75_logs_contain_no_coordinates_odometer_vin_or_name() {
    const VIN: &str = "5YJSYNTHETIC00001";
    const NAME: &str = "Synthetic Name";
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(logs.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let with_identity = |id: i64, t: i64, data: Value| {
            let mut sample = v(id, t, data);
            sample.payload["vin"] = json!(VIN);
            sample.payload["display_name"] = json!(NAME);
            sample
        };
        // The ST23 drive: parked, two driving observations, parked again.
        let drive = [
            (
                1,
                0,
                json!({"shift_state": null, "speed": null, "latitude": 10.0, "longitude": 20.0}),
                1000.0,
                80,
            ),
            (
                2,
                1,
                json!({"shift_state": "D", "speed": 30, "latitude": 10.005, "longitude": 20.0}),
                1000.5,
                80,
            ),
            (
                3,
                2,
                json!({"shift_state": "D", "speed": 60, "latitude": 10.010, "longitude": 20.0}),
                1001.5,
                79,
            ),
            (
                4,
                3,
                json!({"shift_state": null, "speed": null, "latitude": 10.012, "longitude": 20.0}),
                1002.0,
                78,
            ),
        ];
        let mut samples = Vec::new();
        for (id, minute, mut drive_state, odometer, soc) in drive {
            let t = T0 + minute * MIN;
            drive_state["timestamp"] = json!(t);
            samples.push(with_identity(
                id,
                t,
                json!({
                    "drive_state": drive_state,
                    "charge_state": {"timestamp": t, "battery_level": soc},
                    "vehicle_state": {"timestamp": t, "odometer": odometer, "vehicle_name": NAME}
                }),
            ));
        }
        // The ST51 charge.
        for (k, (state, energy)) in [
            ("Disconnected", 12.0),
            ("Charging", 0.5),
            ("Charging", 4.0),
            ("Complete", 20.0),
        ]
        .into_iter()
        .enumerate()
        {
            let t = T0 + (10 + 2 * k as i64) * MIN;
            samples.push(with_identity(
                5 + k as i64,
                t,
                json!({
                    "drive_state": {"timestamp": t, "shift_state": "P", "latitude": 10.012, "longitude": 20.0},
                    "charge_state": {"timestamp": t, "charging_state": state,
                                     "charge_energy_added": energy, "battery_level": 50},
                    "vehicle_state": {"timestamp": t, "odometer": 1002.0}
                }),
            ));
        }
        let result = all(OpenSessionState::new(), &samples);
        assert!(!result.quarantined);
        let _ = all(result.state, &[e(100, T0 + 3 * HOUR, "no_data")]);
    });
    let captured = String::from_utf8(logs.0.lock().unwrap().clone()).expect("utf-8 logs");
    for forbidden in [
        "10.005", "10.01", "10.012", "20.0", "1000", "1001", "1002", VIN, NAME,
    ] {
        assert!(
            !captured.contains(forbidden),
            "log output contains a forbidden text: {forbidden}"
        );
    }
}

#[test]
fn shared_constants_have_their_specified_values() {
    assert_eq!(K_MILE_KM, 1.609_344);
    assert_eq!(K_EARLIEST_MS, 1_325_376_000_000);
    assert_eq!(K_SKEW_MS, 60_000);
    assert_eq!(DEFAULT_OFFLINE_DRIVE_TIMEOUT, Duration::from_secs(600));
    // 2012-01-01T00:00:00Z.
    assert_eq!(K_EARLIEST_MS, 15_340 * 86_400 * 1_000);
}
