// SPDX-License-Identifier: AGPL-3.0-only

//! `control set-geofence` and `control delete-geofence` options (C8 Part K, C8 R34
//! to C8 R41; C8 section 8, C8 T33 to C8 T36): the parsed options with no
//! implicit radius or billing type, the help text built from the shared
//! constants, the end-to-end save through a temporary configuration and store,
//! and the safe, non-zero rejection of an invalid radius or amount. Synthetic data only.

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use clap::{ArgAction, CommandFactory, Parser, error::ErrorKind};
use rusqlite::params;
use teslatlas_hub::db::{HubStore, SourceDescriptor, VehicleDescriptor};
use teslatlas_hub::location::{
    DEFAULT_GEOFENCE_RADIUS_METRES, FIRST_HUB_GEOFENCE_ID, MAX_GEOFENCE_COST_PER_UNIT,
    MAX_GEOFENCE_RADIUS_METRES, MAX_GEOFENCE_SESSION_FEE, MIN_SAVED_GEOFENCE_RADIUS_METRES,
};
use uuid::Uuid;

use crate::{Cli, Command, ControlCommand, report_outcome, run, run_set_geofence};

fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
    let mut all = vec!["teslatlas-hub", "control"];
    all.extend_from_slice(arguments);
    Cli::try_parse_from(all)
}

/// The options of one parsed `set-geofence`, as the control arm receives them.
#[derive(Debug, PartialEq)]
struct SetOptions {
    id: Option<i64>,
    name: String,
    latitude: f64,
    longitude: f64,
    radius_m: Option<f64>,
    billing_type: Option<String>,
    cost_per_unit: Option<f64>,
    session_fee: Option<f64>,
    clear_tariff: bool,
    recalculate_missing_costs: bool,
}

fn set_options(extra: &[&str]) -> Result<SetOptions, clap::Error> {
    let mut arguments = vec![
        "set-geofence",
        "--name",
        "Home",
        "--latitude",
        "10",
        "--longitude",
        "20",
    ];
    arguments.extend_from_slice(extra);
    set_options_of(&arguments)
}

/// The parsed options of exactly these `set-geofence` arguments.
fn set_options_of(arguments: &[&str]) -> Result<SetOptions, clap::Error> {
    let Command::Control {
        command:
            ControlCommand::SetGeofence {
                id,
                name,
                latitude,
                longitude,
                radius_m,
                billing_type,
                cost_per_unit,
                session_fee,
                clear_tariff,
                recalculate_missing_costs,
            },
        ..
    } = parse(arguments)?.command
    else {
        panic!("not a set-geofence command");
    };
    Ok(SetOptions {
        id,
        name,
        latitude,
        longitude,
        radius_m,
        billing_type,
        cost_per_unit,
        session_fee,
        clear_tariff,
        recalculate_missing_costs,
    })
}

fn delete_id(arguments: &[&str]) -> Result<i64, clap::Error> {
    let mut all = vec!["delete-geofence"];
    all.extend_from_slice(arguments);
    let Command::Control {
        command: ControlCommand::DeleteGeofence { id },
        ..
    } = parse(&all)?.command
    else {
        panic!("not a delete-geofence command");
    };
    Ok(id)
}

#[test]
fn c8_t33_parse() {
    // C8 R34, C8 R37: nothing typed means nothing set, in particular no implicit
    // radius and no implicit billing type.
    assert_eq!(
        set_options(&[]).expect("minimal arguments"),
        SetOptions {
            id: None,
            name: "Home".to_owned(),
            latitude: 10.0,
            longitude: 20.0,
            radius_m: None,
            billing_type: None,
            cost_per_unit: None,
            session_fee: None,
            clear_tariff: false,
            recalculate_missing_costs: false,
        }
    );
    // The declared options themselves carry no default (C8 R34, C8 R35) and the
    // billing type admits exactly the stored vocabulary (C8 R35).
    let command = Cli::command();
    let control = command.find_subcommand("control").expect("control");
    let set = control
        .find_subcommand("set-geofence")
        .expect("set-geofence");
    for option in ["radius-m", "billing-type", "cost-per-unit", "session-fee"] {
        let argument = set
            .get_arguments()
            .find(|argument| argument.get_long() == Some(option))
            .unwrap_or_else(|| panic!("--{option} is declared"));
        assert!(
            argument.get_default_values().is_empty(),
            "--{option} must have no clap default"
        );
    }
    let billing = set
        .get_arguments()
        .find(|argument| argument.get_long() == Some("billing-type"))
        .expect("--billing-type");
    let vocabulary: Vec<String> = billing
        .get_possible_values()
        .iter()
        .map(|value| value.get_name().to_owned())
        .collect();
    assert_eq!(vocabulary, ["per_kwh", "per_minute"]);
    let clear = set
        .get_arguments()
        .find(|argument| argument.get_long() == Some("clear-tariff"))
        .expect("--clear-tariff is declared");
    assert!(matches!(clear.get_action(), ArgAction::SetTrue));

    // C8 R37: the clear flag is a plain switch.
    assert!(
        set_options(&["--clear-tariff"])
            .expect("clear flag")
            .clear_tariff
    );
    // C8 R35: the vocabulary is exact and lower case, refused before any store opens.
    assert_eq!(
        set_options(&["--billing-type", "per_minute"])
            .expect("per_minute")
            .billing_type
            .as_deref(),
        Some("per_minute")
    );
    assert_eq!(
        set_options(&["--billing-type", "per_kwh"])
            .expect("per_kwh")
            .billing_type
            .as_deref(),
        Some("per_kwh")
    );
    for refused in ["PER_KWH", "kwh", "per-kwh", ""] {
        let error = set_options(&["--billing-type", refused]).expect_err("outside the vocabulary");
        assert_eq!(error.kind(), ErrorKind::InvalidValue, "{refused:?}");
        // C8 R35: a clap usage error, exit status 2, before the store is opened.
        assert_eq!(error.exit_code(), 2, "{refused:?}");
    }
    // C8 R36: ids are positive i64 values; Hub ids start above the 32-bit range.
    assert_eq!(
        set_options(&["--id", "2147483648"])
            .expect("first Hub id")
            .id,
        Some(FIRST_HUB_GEOFENCE_ID)
    );
    assert_eq!(
        set_options(&["--id", "1"]).expect("smallest id").id,
        Some(1)
    );
    assert_eq!(
        set_options(&["--id", "9223372036854775807"])
            .expect("largest id")
            .id,
        Some(i64::MAX)
    );
    for refused in ["0", "-1", "9223372036854775808", "1.5", "abc"] {
        assert!(
            set_options(&["--id", refused]).is_err(),
            "set-geofence --id {refused}"
        );
    }
    assert_eq!(
        delete_id(&["--id", "9223372036854775807"]).expect("largest delete id"),
        i64::MAX
    );
    assert_eq!(
        delete_id(&["--id", "2147483648"]).expect("Hub delete id"),
        FIRST_HUB_GEOFENCE_ID
    );
    for refused in ["0", "-1", "9223372036854775808", "1.5"] {
        assert!(
            delete_id(&["--id", refused]).is_err(),
            "delete-geofence --id {refused}"
        );
    }
    // C8 R34: a fractional radius parses as typed and is refused later by the
    // store; a typed radius never turns into the default.
    assert_eq!(
        set_options(&["--radius-m", "25.5"])
            .expect("fractional radius")
            .radius_m,
        Some(25.5)
    );
    assert_eq!(
        set_options(&["--radius-m", "9"])
            .expect("radius 9")
            .radius_m,
        Some(9.0)
    );
    assert!(set_options(&["--radius-m", "abc"]).is_err());
    // C8 R5: the whole WGS84 range is writable in the `--option VALUE` form, so
    // a negative centre is a value and not an unknown short flag.
    let west_south = set_options_of(&[
        "set-geofence",
        "--name",
        "Home",
        "--latitude",
        "-33.5",
        "--longitude",
        "-0.127758",
    ])
    .expect("negative centre");
    assert_eq!(west_south.latitude, -33.5);
    assert_eq!(west_south.longitude, -0.127758);
    // C8 R40, C8 T4, C8 T8: negative numbers parse as typed and are refused by
    // the store with its own text, not by clap's unknown-argument error.
    let negative = set_options(&[
        "--radius-m",
        "-5",
        "--cost-per-unit",
        "-0.05",
        "--session-fee",
        "-1",
    ])
    .expect("negative amounts");
    assert_eq!(negative.radius_m, Some(-5.0));
    assert_eq!(negative.cost_per_unit, Some(-0.05));
    assert_eq!(negative.session_fee, Some(-1.0));
    // Tariff amounts are carried as typed (C8 R37).
    let tariff = set_options(&["--cost-per-unit", "0.3", "--session-fee", "1.5"]).expect("tariff");
    assert_eq!(tariff.cost_per_unit, Some(0.3));
    assert_eq!(tariff.session_fee, Some(1.5));
    assert!(
        !set_options(&["--cost-per-unit", "0.3"])
            .expect("cost only")
            .clear_tariff
    );
}

/// Whitespace-collapsed rendering of `control set-geofence --help`, so wrapping
/// at any width cannot split a text.
fn set_geofence_help() -> String {
    let error = parse(&["set-geofence", "--help"]).expect_err("help is not a parse");
    assert_eq!(error.kind(), ErrorKind::DisplayHelp);
    error
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn c8_t35_help() {
    let help = set_geofence_help();
    // C8 R39: the exact texts, built from the shared constants.
    let expected = [
        format!(
            "Radius in whole metres, {MIN_SAVED_GEOFENCE_RADIUS_METRES} to {MAX_GEOFENCE_RADIUS_METRES}. Default {DEFAULT_GEOFENCE_RADIUS_METRES} for a new fence; an edit keeps the stored radius."
        ),
        "Unit of --cost-per-unit: per_kwh or per_minute. Default per_kwh when a cost is given and no type is stored; an edit keeps the stored type unless this is given.".to_owned(),
        format!(
            "Price per kWh or per minute, 0 to {MAX_GEOFENCE_COST_PER_UNIT}. An edit keeps the stored price unless this is given."
        ),
        format!(
            "Flat fee per charging session, 0 to {MAX_GEOFENCE_SESSION_FEE}. An edit keeps the stored fee unless this is given."
        ),
        "Remove the stored billing type, cost per unit and session fee; tariff options given with this flag are applied afterwards.".to_owned(),
        "Id of an existing fence to edit; omit to create a new fence.".to_owned(),
    ];
    for text in &expected {
        assert!(help.contains(text.as_str()), "help lacks {text:?}: {help}");
    }
    // The same texts with the constant values written out, so a changed constant
    // is a visible change of the help (C8 R19).
    for literal in [
        "Radius in whole metres, 10 to 30000. Default 50 for a new fence;",
        "per_kwh or per_minute",
        "0 to 99999.9999. An edit keeps the stored price",
        "0 to 999999999999.99. An edit keeps the stored fee",
        "Remove the stored billing type",
    ] {
        assert!(help.contains(literal), "help lacks {literal:?}: {help}");
    }
    // No default is shown for the radius or the billing type (C8 R34, C8 R35).
    assert!(!help.contains("[default:"), "{help}");
    assert!(
        help.contains("--clear-tariff") && help.contains("--radius-m"),
        "{help}"
    );
}

struct Fixture {
    _directory: tempfile::TempDir,
    config: std::path::PathBuf,
    store: HubStore,
    vehicle: Uuid,
}

/// A temporary configuration and Hub store holding one published car whose
/// source key names car 1 (no lab store, no real data).
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("temporary Hub");
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
        .expect("private data directory");
    let store = HubStore::initialize(directory.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("owner_api", "c8-k"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "1"), 1_001)
        .expect("vehicle")
        .vehicle_id;
    store
        .open()
        .expect("connection")
        .execute(
            "INSERT INTO sync_manifests(snapshot_id, vehicle_id, head_sequence, manifest_json)
             VALUES (?1, ?2, 1, ?3)",
            params![
                Uuid::new_v4().to_string(),
                vehicle.to_string(),
                b"{}".as_slice()
            ],
        )
        .expect("publish the vehicle");
    let config = directory.path().join("config.toml");
    write_config(directory.path(), &config);
    Fixture {
        _directory: directory,
        config,
        store,
        vehicle,
    }
}

fn write_config(directory: &Path, path: &Path) {
    fs::write(
        path,
        format!(
            "data_dir = {directory:?}\nbind = '127.0.0.1:18443'\n\n[collector]\ninterval_seconds = 300\n"
        ),
    )
    .expect("write config");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("private config");
}

fn command_of(arguments: &[&str]) -> ControlCommand {
    let Command::Control { command, .. } = parse(arguments).expect("arguments parse").command
    else {
        panic!("not a control command");
    };
    command
}

/// Run `set-geofence` with the base arguments plus `extra` and return the
/// document it prints.
fn save(fixture: &Fixture, extra: &[&str]) -> serde_json::Value {
    let mut arguments = vec![
        "set-geofence",
        "--name",
        "Home",
        "--latitude",
        "10",
        "--longitude",
        "20",
    ];
    arguments.extend_from_slice(extra);
    run_set_geofence(&fixture.store, fixture.vehicle, &command_of(&arguments)).expect("saved")
}

/// The stored fences of the fixture's car, as the control listing prints them.
fn stored(fixture: &Fixture) -> Vec<serde_json::Value> {
    fixture
        .store
        .geofences(fixture.vehicle)
        .expect("fences")
        .iter()
        .map(|fence| serde_json::to_value(fence).expect("fence JSON"))
        .collect()
}

/// A Hub charge of car 1 that started at `place`, with no cost, 10.00 kWh used
/// and three samples.
fn put_unpriced_charge(fixture: &Fixture, id: i64, place: (f64, f64)) {
    let start = 1_760_000_000_000_i64;
    let end = start + 10 * 60_000;
    let row = serde_json::json!({
        "id": id, "car_id": 1, "start_date_ms": start, "end_date_ms": end,
        "charge_energy_added": 9.0, "charge_energy_used_kwh": 10.0,
        "is_dc": false, "start_latitude": place.0, "start_longitude": place.1
    });
    let connection = fixture.store.open().expect("connection");
    connection
        .execute(
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
             VALUES (?1, ?2, 1, ?3)",
            params![fixture.vehicle.to_string(), id, row.to_string()],
        )
        .expect("charge");
    for (position, offset) in [0_i64, 5, 10].into_iter().enumerate() {
        let sample_id = id * 100 + position as i64;
        let sample = serde_json::json!({
            "id": sample_id, "charge_process_id": id,
            "timestamp_ms": start + offset * 60_000,
            "charge_energy_added_kwh": 4.5 * position as f64,
        });
        connection
            .execute(
                "INSERT INTO materialised_charge_samples(
                    vehicle_id, sample_id, charge_id, sample_json
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    fixture.vehicle.to_string(),
                    sample_id,
                    id,
                    sample.to_string()
                ],
            )
            .expect("sample");
    }
}

fn geofence_document(
    id: i64,
    name: &str,
    radius_m: f64,
    billing_type: Option<&str>,
    cost_per_unit: Option<f64>,
    session_fee: Option<f64>,
) -> serde_json::Value {
    serde_json::json!({
        "id": id, "name": name, "latitude": 10.0, "longitude": 20.0,
        "radius_m": radius_m, "billing_type": billing_type,
        "cost_per_unit": cost_per_unit, "session_fee": session_fee,
    })
}

#[tokio::test]
async fn c8_t34_control_end_to_end() {
    // C8 T34 (a): a create with no radius, tariff or type takes the product
    // default and carries no tariff; the printed document is exact.
    let plain = fixture();
    let document = save(&plain, &[]);
    assert_eq!(
        document,
        serde_json::json!({
            "status": "updated",
            "vehicleId": plain.vehicle.to_string(),
            "geofence": geofence_document(
                FIRST_HUB_GEOFENCE_ID, "Home", DEFAULT_GEOFENCE_RADIUS_METRES, None, None, None
            ),
            "recalculatedChargeCosts": 0,
        })
    );
    // C8 T34 (b): a cost alone takes the default billing type (C8 table 5.2).
    let priced = fixture();
    let document = save(&priced, &["--cost-per-unit", "0.3"]);
    assert_eq!(
        document["geofence"],
        geofence_document(
            FIRST_HUB_GEOFENCE_ID,
            "Home",
            DEFAULT_GEOFENCE_RADIUS_METRES,
            Some("per_kwh"),
            Some(0.3),
            None
        )
    );

    // C8 T34 (b2): an edit of that fence that omits radius and tariff keeps the
    // default radius, per_kwh and 0.3.
    let priced_id = document["geofence"]["id"].as_i64().expect("returned id");
    let kept = run_set_geofence(
        &priced.store,
        priced.vehicle,
        &command_of(&[
            "set-geofence",
            "--id",
            &priced_id.to_string(),
            "--name",
            "Home2",
            "--latitude",
            "10",
            "--longitude",
            "20",
        ]),
    )
    .expect("edit");
    assert_eq!(
        kept["geofence"],
        geofence_document(
            priced_id,
            "Home2",
            DEFAULT_GEOFENCE_RADIUS_METRES,
            Some("per_kwh"),
            Some(0.3),
            None
        )
    );

    // C8 T34 (c): the id the create returned addresses the edits that follow;
    // an edit that omits radius and tariff keeps every stored value.
    let edited = fixture();
    let created = save(
        &edited,
        &[
            "--radius-m",
            "120",
            "--billing-type",
            "per_minute",
            "--cost-per-unit",
            "0.3",
            "--session-fee",
            "1.5",
        ],
    );
    let id = created["geofence"]["id"].as_i64().expect("returned id");
    assert_eq!(id, FIRST_HUB_GEOFENCE_ID);
    let id_text = id.to_string();
    let renamed = run_set_geofence(
        &edited.store,
        edited.vehicle,
        &command_of(&[
            "set-geofence",
            "--id",
            &id_text,
            "--name",
            "Home2",
            "--latitude",
            "10",
            "--longitude",
            "20",
        ]),
    )
    .expect("rename");
    assert_eq!(renamed["status"], "updated");
    assert_eq!(
        renamed["geofence"],
        geofence_document(id, "Home2", 120.0, Some("per_minute"), Some(0.3), Some(1.5)),
        "an omitted radius, type, cost and fee keep the stored values"
    );
    assert_eq!(stored(&edited), vec![renamed["geofence"].clone()]);

    // C8 T34 (d): --clear-tariff removes the three tariff fields and keeps the radius.
    let cleared = run_set_geofence(
        &edited.store,
        edited.vehicle,
        &command_of(&[
            "set-geofence",
            "--id",
            &id_text,
            "--name",
            "Home2",
            "--latitude",
            "10",
            "--longitude",
            "20",
            "--clear-tariff",
        ]),
    )
    .expect("clear");
    assert_eq!(
        cleared["geofence"],
        geofence_document(id, "Home2", 120.0, None, None, None)
    );
    assert_eq!(stored(&edited), vec![cleared["geofence"].clone()]);

    // C8 T34 (e): a tariff given with the clear flag replaces the old one.
    let replaced = run_set_geofence(
        &edited.store,
        edited.vehicle,
        &command_of(&[
            "set-geofence",
            "--id",
            &id_text,
            "--name",
            "Home2",
            "--latitude",
            "10",
            "--longitude",
            "20",
            "--billing-type",
            "per_minute",
            "--cost-per-unit",
            "0.5",
            "--clear-tariff",
        ]),
    )
    .expect("replace");
    assert_eq!(
        replaced["geofence"],
        geofence_document(id, "Home2", 120.0, Some("per_minute"), Some(0.5), None)
    );

    // C8 T34 (e2): with the stored type cleared, a given cost alone takes the
    // default billing type (C8 table 5.2).
    let defaulted = run_set_geofence(
        &edited.store,
        edited.vehicle,
        &command_of(&[
            "set-geofence",
            "--id",
            &id_text,
            "--name",
            "Home2",
            "--latitude",
            "10",
            "--longitude",
            "20",
            "--cost-per-unit",
            "0.7",
            "--clear-tariff",
        ]),
    )
    .expect("clear and replace");
    assert_eq!(
        defaulted["geofence"],
        geofence_document(id, "Home2", 120.0, Some("per_kwh"), Some(0.7), None)
    );

    // C8 T34 (f): the same edit through the configuration and the control entry
    // point (C8 R37) reaches the store with the typed id.
    let cli = Cli::try_parse_from([
        "teslatlas-hub",
        "--config",
        edited.config.to_str().expect("UTF-8 path"),
        "control",
        "set-geofence",
        "--id",
        &id_text,
        "--name",
        "Home3",
        "--latitude",
        "10",
        "--longitude",
        "20",
        "--clear-tariff",
    ])
    .expect("control arguments");
    run(cli).await.expect("control run");
    assert_eq!(
        stored(&edited),
        vec![geofence_document(id, "Home3", 120.0, None, None, None)]
    );

    // C8 T34 (g): the requested missing-cost fill runs after the save and uses
    // the id the save returned, not the id typed (a create types none).
    let filled = fixture();
    put_unpriced_charge(&filled, 9, (10.0005, 20.0));
    put_unpriced_charge(&filled, 10, (11.0, 20.0));
    let document = save(
        &filled,
        &[
            "--radius-m",
            "150",
            "--cost-per-unit",
            "0.30",
            "--recalculate-missing-costs",
        ],
    );
    assert_eq!(document["geofence"]["id"], FIRST_HUB_GEOFENCE_ID);
    assert_eq!(document["recalculatedChargeCosts"], 1);
    let cost_of = |charge: i64| {
        filled
            .store
            .materialised_charge_for_vehicle(filled.vehicle, charge)
            .expect("read")
            .expect("row")
            .cost
    };
    assert_eq!(cost_of(9), Some(3.0));
    assert_eq!(cost_of(10), None);
}

/// Every table a fence save may touch, dumped row by row.
fn snapshot(store: &HubStore) -> Vec<(String, Vec<String>)> {
    let connection = store.open().expect("connection");
    [
        "geofences",
        "geofence_id_marks",
        "deleted_imported_geofences",
        "sync_mutations",
        "export_outbox",
    ]
    .into_iter()
    .map(|table| {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {table}"))
            .expect("prepare dump");
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                Ok((0..columns)
                    .map(|index| format!("{:?}", row.get_ref(index).expect("column")))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .expect("dump")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows");
        (table.to_owned(), rows)
    })
    .collect()
}

/// The error and every cause after it, as the process prints them.
fn failure_text(error: &(dyn std::error::Error + 'static)) -> Vec<String> {
    let mut texts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(cause) = source {
        texts.push(cause.to_string());
        source = cause.source();
    }
    texts
}

#[tokio::test]
async fn c8_t36_rejection_exit() {
    let fixture = fixture();
    // An existing fence to edit, so both a create and an edit are refused.
    let created = save(&fixture, &["--radius-m", "100", "--cost-per-unit", "0.3"]);
    let id_text = created["geofence"]["id"]
        .as_i64()
        .expect("returned id")
        .to_string();
    let before = snapshot(&fixture.store);
    assert!(
        before
            .iter()
            .any(|(table, rows)| table == "geofences" && rows.len() == 1),
        "the fence is stored"
    );
    let config = fixture.config.to_str().expect("UTF-8 path");
    // (typed option and value, the exact C8 R11 text it is refused with). The
    // negative amounts reach the store as typed values (no clap usage error).
    let refusals = [
        (
            "--radius-m",
            "9",
            "geofence radius must be at least 10 metres",
        ),
        (
            "--radius-m",
            "30001",
            "geofence radius must be at most 30000 metres",
        ),
        (
            "--radius-m",
            "25.5",
            "geofence radius must be a whole number of metres",
        ),
        (
            "--radius-m",
            "NaN",
            "geofence radius must be a finite number of metres",
        ),
        (
            "--radius-m",
            "-5",
            "geofence radius must be at least 10 metres",
        ),
        (
            "--cost-per-unit",
            "-0.05",
            "geofence cost per unit must be a finite number from 0 to 99999.9999",
        ),
        (
            "--session-fee",
            "-1",
            "geofence session fee must be a finite number from 0 to 999999999999.99",
        ),
    ];
    for (option, value, message) in refusals {
        for edit in [false, true] {
            let mut arguments = vec![
                "teslatlas-hub",
                "--config",
                config,
                "control",
                "set-geofence",
                "--name",
                "Private Place Alpha",
                "--latitude",
                "12.345678",
                "--longitude",
                "23.456789",
                option,
                value,
            ];
            if edit {
                arguments.extend_from_slice(&["--id", &id_text]);
            }
            let cli = Cli::try_parse_from(arguments).expect("arguments parse");
            let error = run(cli).await.expect_err("an invalid value is refused");
            let texts = failure_text(error.as_ref());
            assert_eq!(texts[0], message, "{option} {value} edit {edit}");
            for text in &texts {
                for supplied in ["Private Place Alpha", "12.345678", "23.456789", value] {
                    assert!(
                        !text.contains(supplied),
                        "the refusal repeats {supplied:?}: {text}"
                    );
                }
            }
            // C8 R40: `main` ends with a non-zero status and exactly the R11 line
            // (the error has no cause) on standard error, with no supplied value.
            let mut stderr = Vec::new();
            assert_eq!(report_outcome(Err(error), &mut stderr), 1);
            assert_eq!(
                String::from_utf8(stderr).expect("UTF-8"),
                format!("teslatlas-hub: {message}\n"),
                "{option} {value} edit {edit}"
            );
            assert_eq!(
                snapshot(&fixture.store),
                before,
                "a refused save writes nothing ({option} {value} edit {edit})"
            );
        }
    }
    // A success is status 0 and writes nothing to standard error.
    let mut stderr = Vec::new();
    assert_eq!(report_outcome(Ok(()), &mut stderr), 0);
    assert!(stderr.is_empty());
    assert_eq!(
        stored(&fixture),
        vec![geofence_document(
            FIRST_HUB_GEOFENCE_ID,
            "Home",
            100.0,
            Some("per_kwh"),
            Some(0.3),
            None
        )]
    );
}
