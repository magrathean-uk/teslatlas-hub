// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn unbounded_full_snapshot_capacity_uses_the_protocol_ceiling_without_overflow() {
    let temp = crate::private_tempdir().unwrap();
    let limits = ProtocolLimits {
        max_chunks: 1,
        max_compressed_pack_bytes: 1024,
        max_uncompressed_pack_bytes: 4096,
        ..Default::default()
    };
    ProjectionPackWriter::with_limits(temp.path(), limits)
        .ensure_full_snapshot_capacity(0)
        .expect("unbounded capture clamps to finite protocol capacity");
}

#[test]
fn owner_api_model_codes_are_normalized_like_teslamate() {
    assert_eq!(normalize_tesla_model_code("model3"), "3");
    assert_eq!(normalize_tesla_model_code("models2"), "S");
    assert_eq!(normalize_tesla_model_code("modely"), "Y");
    assert_eq!(normalize_tesla_model_code("cybertruck"), "Cybertruck");
    assert_eq!(
        normalize_tesla_model_code("cybertruckpremium"),
        "Cybertruck"
    );
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
fn ct4_live_car_defaults_are_one_set_of_values() {
    let live = ProjectionCarSettings::new_live();
    assert_eq!(
        live,
        ProjectionCarSettings {
            enabled: true,
            use_streaming_api: true,
            suspend_after_idle_min: 15,
            suspend_min: 30,
            suspend_min_resolved: true,
            req_not_unlocked: false,
            free_supercharging: false,
            lfp_battery: false,
        }
    );
    // The storage fallbacks (`unwrap_or_default`, `#[serde(default)]`) agree
    // with live discovery.
    assert_eq!(live, ProjectionCarSettings::default());
    validate_car_settings(&live).expect("live defaults are valid");
    assert!(default_suspend_min_resolved());
}

#[test]
fn ct4_the_importer_keeps_the_source_column_defaults_for_absent_values() {
    // C3 R63: imported history is copied, never recomputed, so the importer's
    // fallback is not the live default.
    let source = ProjectionCarSettings::source_column_defaults();
    assert_eq!(
        source,
        ProjectionCarSettings {
            enabled: true,
            use_streaming_api: true,
            suspend_after_idle_min: 15,
            suspend_min: 21,
            suspend_min_resolved: true,
            req_not_unlocked: false,
            free_supercharging: false,
            lfp_battery: false,
        }
    );
    assert_ne!(source, ProjectionCarSettings::new_live());
    validate_car_settings(&source).expect("source defaults are valid");
}

#[test]
fn r61_effective_suspend_values_are_clamped_to_one_minute_to_one_day() {
    let with = |idle, suspend| ProjectionCarSettings {
        suspend_after_idle_min: idle,
        suspend_min: suspend,
        ..ProjectionCarSettings::new_live()
    };
    let settings = with(15, 30);
    assert_eq!(settings.effective_suspend_after_idle_min(), 15);
    assert_eq!(settings.effective_suspend_min(), 30);
    for (stored, effective) in [
        (i64::MIN, 1),
        (-5, 1),
        (0, 1),
        (1, 1),
        (1_440, 1_440),
        (1_441, 1_440),
        (i64::MAX, 1_440),
    ] {
        assert_eq!(
            with(stored, stored).effective_suspend_after_idle_min(),
            effective
        );
        assert_eq!(with(stored, stored).effective_suspend_min(), effective);
    }
}

#[test]
fn ct5_a_stored_settings_object_without_the_resolution_flag_decodes_as_resolved() {
    let old: ProjectionCarSettings = serde_json::from_value(serde_json::json!({
        "enabled": false,
        "use_streaming_api": true,
        "suspend_after_idle_min": 20,
        "suspend_min": 21,
        "req_not_unlocked": true,
        "free_supercharging": true,
        "lfp_battery": true
    }))
    .expect("blob written before the flag existed");
    assert_eq!(
        old,
        ProjectionCarSettings {
            enabled: false,
            use_streaming_api: true,
            suspend_after_idle_min: 20,
            suspend_min: 21,
            suspend_min_resolved: true,
            req_not_unlocked: true,
            free_supercharging: true,
            lfp_battery: true,
        }
    );
    // An object with no settings at all takes the live defaults.
    let empty: ProjectionCarSettings = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(empty, ProjectionCarSettings::new_live());
    // An explicit flag is kept.
    let unresolved: ProjectionCarSettings =
        serde_json::from_value(serde_json::json!({"suspend_min_resolved": false})).unwrap();
    assert!(!unresolved.suspend_min_resolved);
    assert_eq!(unresolved.suspend_min, 30);
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
            // Frozen fixture packs were generated with the source column
            // defaults, which are not the live defaults (C3 R63).
            settings: ProjectionCarSettings::source_column_defaults(),
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
    let physical = ["physical-admission", "physical-scan"].map(|kind| {
        let path = staging.join(format!("{}.{kind}.sqlite.tmp", Uuid::new_v4()));
        fs::write(&path, b"decoded").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    });
    let lookalike = staging.join("not-a-uuid.physical-scan.sqlite.tmp");
    fs::write(&lookalike, b"keep").unwrap();
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

    assert_eq!(removed, 4);
    assert_eq!(freed_bytes, 21);
    assert!(physical.iter().all(|path| !path.exists()));
    assert_eq!(fs::read(lookalike).unwrap(), b"keep");
    assert!(!private.exists());
    assert!(!linked.exists());
    assert_eq!(fs::read(final_link).unwrap(), b"linked");
    assert_eq!(fs::read(unrelated).unwrap(), b"keep");
}

#[test]
fn startup_cleanup_rejects_unsafe_physical_scratch_entries() {
    use std::os::unix::fs::symlink;
    for suffix in ["physical-admission", "physical-scan"] {
        for unsafe_kind in ["symlink", "public-file", "directory"] {
            let temporary = crate::private_tempdir().expect("pack root");
            let staging = temporary.path().join(".staging");
            ensure_private_staging_directory(&staging).expect("private staging");
            let path = staging.join(format!("{}.{suffix}.sqlite.tmp", Uuid::new_v4()));
            let target = temporary.path().join("target");
            fs::write(&target, b"keep").unwrap();
            match unsafe_kind {
                "symlink" => symlink(&target, &path).unwrap(),
                "public-file" => {
                    fs::write(&path, b"keep").unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                }
                "directory" => fs::create_dir(&path).unwrap(),
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    cleanup_stale_pack_staging(temporary.path()),
                    Err(ProjectionPackError::UnsafeStaging(_))
                ),
                "{suffix} {unsafe_kind}"
            );
            assert!(fs::symlink_metadata(path).is_ok());
            assert_eq!(fs::read(target).unwrap(), b"keep");
        }
    }
}

#[test]
fn staging_directory_creation_is_private_under_a_group_writable_umask() {
    const PROBE: &str = "TESLATLAS_HUB_STAGING_UMASK_PROBE";
    if env::var_os(PROBE).is_some() {
        rustix::process::umask(rustix::fs::Mode::from_bits_retain(0o002));
        let temporary = crate::private_tempdir().expect("pack root");
        let staging = temporary.path().join(".staging");
        ensure_private_staging_directory(&staging).expect("private staging under umask 0002");
        assert_eq!(
            fs::symlink_metadata(staging).unwrap().permissions().mode() & 0o777,
            PRIVATE_STAGING_DIRECTORY_MODE,
        );
        return;
    }

    let status = std::process::Command::new(env::current_exe().expect("current test binary"))
        .arg("sync::hub_pack::tests::staging_directory_creation_is_private_under_a_group_writable_umask")
        .arg("--exact")
        .env(PROBE, "1")
        .status()
        .expect("run isolated umask probe");
    assert!(status.success(), "isolated umask probe must pass");
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

#[test]
fn pack_output_staging_and_publication_attempts_are_counted_for_the_run_guard_proofs() {
    // B2 fix round, P3b fix 4 (R-P3bF4-7): the writer's staging directory, its temporary
    // files, the hard link of the final name and the removal of the staging name are file
    // mutation attempts counted at the helpers they go through, whatever any guard says.
    let temporary = crate::private_tempdir().expect("pack store");
    let source = snapshot();
    assert_eq!(
        crate::platform::file_attempts::count_under(temporary.path()),
        0
    );
    ProjectionPackWriter::new(temporary.path())
        .write_full_snapshot(&request(&source))
        .expect("a pack");
    let written = crate::platform::file_attempts::count_under(temporary.path());
    // The staging directory (checked by the writer, by the verification and by the
    // validation: three), the content directory, the SQLite temporary file (created,
    // filled, removed on drop) and the zstd one (created, filled, permissions, removed),
    // the hard link of the final name, and the syncs of the content directory and of the
    // staging directory (fix 5, R-P3bF5-3): fourteen.
    assert_eq!(
        written,
        14,
        "{:#?}",
        crate::platform::file_attempts::entries_under(temporary.path())
    );
    // A reuse of the same bytes publishes nothing new, and still attempts: the proof that
    // an idempotent rewrite of a pack file cannot hide.
    ProjectionPackWriter::new(temporary.path())
        .write_full_snapshot(&request(&source))
        .expect("the same pack again");
    assert_eq!(
        crate::platform::file_attempts::count_under(temporary.path()),
        2 * written
    );
}

#[test]
fn a_pack_directory_sync_that_changes_nothing_moves_the_file_counter() {
    // B2 fix round, P3b fix 5 (R-P3bF5-3): the sync of a pack directory flushes what is
    // there and leaves the file image as it was; the counter still moves, once.
    let temporary = crate::private_tempdir().expect("pack root");
    let file = temporary.path().join("kept");
    fs::write(&file, b"unchanged").unwrap();
    let before = crate::platform::file_attempts::count_under(temporary.path());

    sync_parent_directory(&file).expect("sync");

    assert_eq!(fs::read(&file).unwrap(), b"unchanged");
    assert_eq!(
        crate::platform::file_attempts::count_under(temporary.path()),
        before + 1
    );
}

#[test]
fn the_stale_staging_cleanup_counts_each_removal_and_its_directory_sync() {
    // The staging directory is checked (one attempt), each owned temporary file is
    // removed (two) and the directory is synced once afterwards: four attempts.
    let temporary = crate::private_tempdir().expect("pack root");
    let staging = temporary.path().join(".staging");
    ensure_private_staging_directory(&staging).expect("private staging");
    for kind in ["projection.sqlite", "projection.zst"] {
        let path = staging.join(format!("{}.{kind}.tmp", Uuid::new_v4()));
        fs::write(&path, b"stale").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let before = crate::platform::file_attempts::count_under(temporary.path());

    let (removed, _) = cleanup_stale_pack_staging(temporary.path()).expect("cleanup");

    assert_eq!(removed, 2);
    assert_eq!(
        crate::platform::file_attempts::count_under(temporary.path()) - before,
        4,
        "{:#?}",
        crate::platform::file_attempts::entries_under(temporary.path())
    );
}
