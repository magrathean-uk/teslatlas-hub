// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn owner_api_model_codes_are_normalized_like_teslamate() {
    assert_eq!(normalize_tesla_model_code("model3"), "3");
    assert_eq!(normalize_tesla_model_code("models2"), "S");
    assert_eq!(normalize_tesla_model_code("modely"), "Y");
    assert_eq!(normalize_tesla_model_code("cybertruck"), "Cybertruck");
    assert_eq!(normalize_tesla_model_code("cybertruckpremium"), "Cybertruck");
    assert_eq!(normalize_tesla_model_code("Model 3"), "3");
}

#[test]
fn model_3_base_trim_uses_the_vin_model_year() {
    assert_eq!(
        derive_tesla_marketing_name("3", Some("50"), Some("model3"), Some("5YJ3E1EA7NF000001")),
        Some("RWD".to_owned())
    );
    assert_eq!(
        derive_tesla_marketing_name("3", Some("50"), Some("model3"), Some("5YJ3E1EA7MF000001")),
        Some("SR+".to_owned())
    );
    assert_eq!(
        derive_tesla_marketing_name("3", Some("50"), Some("model3"), None),
        Some("SR+".to_owned())
    );
}

#[test]
fn teslamate_suspend_default_matches_creation_conditions() {
    assert_eq!(
        teslamate_suspend_min_default(Some("3"), Some("74D"), None),
        Some(12)
    );
    assert_eq!(
        teslamate_suspend_min_default(Some("Y"), None, None),
        Some(12)
    );
    assert_eq!(
        teslamate_suspend_min_default(Some("S"), None, None),
        Some(12)
    );
    assert_eq!(
        teslamate_suspend_min_default(Some("X"), Some("100D"), Some("LR")),
        Some(12)
    );
    assert_eq!(
        teslamate_suspend_min_default(Some("S"), Some("100D"), None),
        Some(21)
    );
    assert_eq!(
        teslamate_suspend_min_default(Some("Cybertruck"), None, None),
        Some(12)
    );
}

fn binding() -> ProjectionBinding {
    ProjectionBinding {
        installation_id: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
        account_id: Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
        vehicle_id: Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap(),
        generation: 1,
        selected_car_id: 10,
    }
}

fn snapshot() -> ProjectionSnapshot {
    ProjectionSnapshot {
        cars: vec![ProjectionCar {
            id: 10,
            name: "Road car".into(),
            model: "Model 3".into(),
            vin: Some("5YJTESTVIN1234567".into()),
            source_eid: Some(101),
            source_vid: Some(201),
            trim_badging: Some("74D".into()),
            marketing_name: Some("LR AWD".into()),
            exterior_color: Some("Pearl White".into()),
            wheel_type: Some("Apollo".into()),
            spoiler_type: Some("None".into()),
            firmware_version: Some("2026.1.1".into()),
            efficiency_wh_per_km: Some(145.0),
            settings: ProjectionCarSettings::default(),
        }],
        drives: vec![ProjectionDrive {
            id: 20,
            car_id: 10,
            optimized_at_ms: None,
            start_date_ms: 1_700_000_000_000,
            end_date_ms: 1_700_000_060_000,
            distance_km: Some(12.5),
            duration_min: Some(10),
            efficiency: Some(145.0),
            outside_temp_avg: Some(18.5),
            inside_temp_avg: Some(20.0),
            speed_max: Some(80),
            power_max: Some(36.0),
            power_min: Some(-7.0),
            start_ideal_range_km: Some(390.0),
            end_ideal_range_km: Some(385.0),
            start_address: Some("Home".into()),
            end_address: Some("Work".into()),
            start_geofence: None,
            end_geofence: None,
            start_latitude: Some(51.5),
            start_longitude: Some(-0.1),
            end_latitude: Some(51.51),
            end_longitude: Some(-0.11),
            start_soc: Some(80),
            end_soc: Some(75),
            start_rated_range_km: Some(400.0),
            end_rated_range_km: Some(375.0),
            ascent: Some(60),
            descent: Some(30),
        }],
        positions: vec![ProjectionPosition {
            id: 30,
            drive_id: Some(20),
            car_id: 10,
            date_ms: 1_700_000_030_000,
            latitude: 51.505,
            longitude: -0.105,
            speed: Some(40),
            power: Some(3.0),
            battery_level: Some(78),
            usable_battery_level: Some(77),
            elevation: Some(25),
            odometer: Some(10_000.5),
            ideal_battery_range_km: Some(390.0),
            est_battery_range_km: Some(385.0),
            rated_battery_range_km: Some(388.0),
            fan_status: Some(2),
            driver_temp_setting: Some(21.5),
            passenger_temp_setting: Some(22.0),
            is_climate_on: Some(false),
            is_rear_defroster_on: Some(false),
            is_front_defroster_on: Some(true),
            inside_temp: Some(20.0),
            outside_temp: Some(18.0),
            battery_heater: None,
            battery_heater_on: None,
            battery_heater_no_power: None,
            tpms_pressure_fl: Some(2.4),
            tpms_pressure_fr: Some(2.5),
            tpms_pressure_rl: Some(2.6),
            tpms_pressure_rr: Some(2.7),
        }],
        charges: vec![ProjectionCharge {
            id: 40,
            car_id: 10,
            start_date_ms: 1_700_001_000_000,
            end_date_ms: Some(1_700_001_360_000),
            charge_energy_added: Some(20.0),
            charge_energy_used_kwh: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            cost: None,
            fast_charger_type: None,
            billing_type: None,
            cost_per_unit: None,
            session_fee: None,
            start_latitude: None,
            start_longitude: None,
            start_battery_level: Some(50),
            end_battery_level: Some(80),
            duration_min: Some(60),
            address: Some("Home".into()),
            location_name: None,
            geofence: None,
            is_dc: Some(false),
            charge_rate_km_per_hour: Some(40.0),
            max_charger_power_kw: Some(7.0),
            outside_temp_avg: Some(18.0),
            start_rated_range_km: Some(250.0),
            end_rated_range_km: Some(400.0),
        }],
        charge_samples: vec![ProjectionChargeSample {
            id: 50,
            charge_process_id: 40,
            timestamp_ms: 1_700_001_100_000,
            battery_level: Some(60),
            usable_battery_level: Some(59),
            charge_energy_added_kwh: Some(6.0),
            charger_power_kw: Some(7.0),
            charger_voltage: Some(230.0),
            charger_actual_current: Some(30.0),
            charger_pilot_current: Some(32.0),
            charger_phases: Some(1),
            ideal_range_km: Some(300.0),
            rated_range_km: Some(298.0),
            outside_temp_c: Some(18.0),
            battery_heater_on: Some(false),
            battery_heater: Some(false),
            battery_heater_no_power: Some(false),
            not_enough_power_to_heat: Some(false),
            fast_charger_present: Some(false),
            fast_charger_brand: None,
            fast_charger_type: None,
            charge_cable: Some("Type 2".into()),
        }],
    }
}

fn request<'a>(snapshot: &'a ProjectionSnapshot) -> ProjectionPackRequest<'a> {
    ProjectionPackRequest {
        pack_id: Uuid::parse_str("44444444-4444-4444-8444-444444444444").unwrap(),
        snapshot_id: Uuid::parse_str("55555555-5555-4555-8555-555555555555").unwrap(),
        ordinal: 0,
        binding: binding(),
        sequence: SequenceRange {
            from_exclusive: 7,
            to_inclusive: 7,
        },
        snapshot,
    }
}

#[test]
fn fault_matrix_never_leaves_a_partial_content_pack() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};

    let cases = [
        (DurabilityFaultPoint::PackSqliteCommit, false, false, false),
        (
            DurabilityFaultPoint::PackCompressedWrite,
            false,
            false,
            false,
        ),
        (
            DurabilityFaultPoint::PackCompressedFsync,
            false,
            false,
            false,
        ),
        (DurabilityFaultPoint::PackFinalInstall, false, false, false),
        (
            DurabilityFaultPoint::PackFinalDirectoryFsync,
            true,
            false,
            false,
        ),
        (DurabilityFaultPoint::PackStagingUnlink, true, true, true),
        (
            DurabilityFaultPoint::PackStagingDirectoryFsync,
            true,
            false,
            true,
        ),
    ];
    for (point, expect_final, expect_staging, expect_cleanup_pending) in cases {
        let temporary = crate::private_tempdir().expect("fault store");
        let source = snapshot();
        let _fault = inject(point);
        let result =
            ProjectionPackWriter::new(temporary.path()).write_full_snapshot(&request(&source));
        if expect_cleanup_pending {
            let built = result.expect("post-publication cleanup is a successful pack receipt");
            assert_eq!(
                built.cleanup_state(),
                ProjectionPackCleanupState::PendingStartupRepair
            );
        } else {
            let error = result.expect_err("pre-publication durability point must fail");
            assert!(
                error.to_string().contains("durability fault"),
                "typed fault for {point:?}: {error}"
            );
        }

        let content_directory = temporary.path().join("sha256");
        let content = fs::read_dir(&content_directory)
            .map(|entries| {
                entries
                    .map(|entry| entry.expect("content entry").path())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        assert_eq!(
            !content.is_empty(),
            expect_final,
            "final namespace outcome for {point:?}"
        );
        for path in content {
            let bytes = fs::read(&path).expect("read completed content object");
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .expect("UTF-8 content name");
            let digest = name
                .strip_suffix(".sqlite.zst")
                .expect("canonical content suffix");
            assert_eq!(hex::encode(Sha256::digest(&bytes)), digest);
            assert!(!bytes.is_empty());
        }

        let staging_directory = temporary.path().join(".staging");
        let staging_count = fs::read_dir(staging_directory)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(
            staging_count > 0,
            expect_staging,
            "restart-visible staging outcome for {point:?}"
        );
    }
}

#[test]
fn startup_cleanup_removes_only_owned_staging_files() {
    let temporary = crate::private_tempdir().expect("pack root");
    let staging = temporary.path().join(".staging");
    ensure_private_staging_directory(&staging).expect("private staging");
    let private = staging.join(format!("{}.projection.sqlite.tmp", Uuid::new_v4()));
    let linked = staging.join(format!("{}.projection.zst.tmp", Uuid::new_v4()));
    let final_link = temporary.path().join("linked-pack");
    let unrelated = staging.join("notes.tmp");
    fs::write(&private, b"private").unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&linked, b"linked").unwrap();
    fs::set_permissions(
        &linked,
        fs::Permissions::from_mode(SHARED_IMMUTABLE_PACK_MODE),
    )
    .unwrap();
    fs::hard_link(&linked, &final_link).unwrap();
    fs::write(&unrelated, b"keep").unwrap();

    let (removed, freed_bytes) = cleanup_stale_pack_staging(temporary.path()).expect("cleanup");

    assert_eq!(removed, 2);
    assert_eq!(freed_bytes, 7);
    assert!(!private.exists());
    assert!(!linked.exists());
    assert_eq!(fs::read(final_link).unwrap(), b"linked");
    assert_eq!(fs::read(unrelated).unwrap(), b"keep");
}

#[test]
fn startup_cleanup_rejects_an_owned_name_symlink() {
    use std::os::unix::fs::symlink;

    let temporary = crate::private_tempdir().expect("pack root");
    let staging = temporary.path().join(".staging");
    ensure_private_staging_directory(&staging).expect("private staging");
    let target = temporary.path().join("target");
    fs::write(&target, b"keep").unwrap();
    let link = staging.join(format!("{}.projection.sqlite.tmp", Uuid::new_v4()));
    symlink(&target, &link).unwrap();

    assert!(matches!(
        cleanup_stale_pack_staging(temporary.path()),
        Err(ProjectionPackError::UnsafeStaging(path)) if path == link
    ));
    assert_eq!(fs::read(target).unwrap(), b"keep");
}

#[test]
fn existing_content_retry_repeats_the_final_directory_sync() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};

    let temporary = crate::private_tempdir().expect("retry store");
    let source = snapshot();
    {
        let _fault = inject(DurabilityFaultPoint::PackFinalDirectoryFsync);
        ProjectionPackWriter::new(temporary.path())
            .write_full_snapshot(&request(&source))
            .expect_err("first directory sync fault");
    }
    {
        let _fault = inject(DurabilityFaultPoint::PackFinalDirectoryFsync);
        ProjectionPackWriter::new(temporary.path())
            .write_full_snapshot(&request(&source))
            .expect_err("verified reuse still repeats directory sync");
    }
    let built = ProjectionPackWriter::new(temporary.path())
        .write_full_snapshot(&request(&source))
        .expect("normal retry durably converges");
    assert_eq!(built.ownership(), ProjectionPackOwnership::ReusedExisting);
    assert_eq!(built.cleanup_state(), ProjectionPackCleanupState::Complete);
}
