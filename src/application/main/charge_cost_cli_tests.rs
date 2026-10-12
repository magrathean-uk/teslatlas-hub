// SPDX-License-Identifier: AGPL-3.0-only

//! `control set-charge-cost` (C2-P4, R52 to R54, R53a): the `--mode` values,
//! the deprecated aliases and the required-mode rule, the store call each mode
//! makes and the document the command prints. Spec tests T37, T38 and T40 (as
//! the command makes them), T41 (the usage error), T47 and T35 (the
//! `--recalculate-missing-costs` wiring). Synthetic data only.

use clap::Parser;
use teslatlas_hub::db::{HubStore, SourceDescriptor, VehicleDescriptor};
use uuid::Uuid;

use crate::{
    ChargeCostMode, ChargeCostModeChoice, Cli, Command, ControlCommand,
    deprecated_charge_cost_mode_line, parse_charge_cost_mode_in, run_set_charge_cost,
    run_set_geofence,
};

fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
    let mut all = vec!["teslatlas-hub", "control"];
    all.extend_from_slice(arguments);
    Cli::try_parse_from(all)
}

fn set_charge_cost(arguments: &[&str]) -> Result<(i64, f64, ChargeCostModeChoice), clap::Error> {
    let mut all = vec!["set-charge-cost"];
    all.extend_from_slice(arguments);
    let cli = parse(&all)?;
    let Command::Control {
        command:
            ControlCommand::SetChargeCost {
                charge_id,
                cost,
                mode,
            },
        ..
    } = cli.command
    else {
        panic!("not a set-charge-cost command");
    };
    Ok((charge_id, cost, mode))
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: HubStore,
    vehicle: Uuid,
}

/// A store with one vehicle and one materialised charge, id 7: AC, 20.00 kWh
/// used, a tariff copy with a fee, 95.5 minutes long with samples every five.
fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("temporary Hub");
    let store = HubStore::initialize(directory.path().join("hub")).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api", "charge-cost-cli"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "car"), 1_001)
        .expect("vehicle")
        .vehicle_id;
    let start = 1_760_000_000_000_i64;
    let end = start + 95 * 60_000 + 30_000;
    let row = serde_json::json!({
        "id": 7, "car_id": 1, "start_date_ms": start, "end_date_ms": end,
        "charge_energy_added": 18.0, "charge_energy_used_kwh": 20.0,
        "billing_type": "per_kwh", "cost_per_unit": 0.3, "session_fee": 1.0,
        "is_dc": false
    });
    let connection = store.open().expect("connection");
    connection
        .execute(
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
             VALUES (?1, 7, 1, ?2)",
            rusqlite::params![vehicle.to_string(), row.to_string()],
        )
        .expect("row");
    let timestamps = (0..=19)
        .map(|step| start + step * 5 * 60_000)
        .chain(std::iter::once(end));
    for (index, timestamp) in timestamps.enumerate() {
        let sample = serde_json::json!({
            "id": 700 + index as i64, "charge_process_id": 7, "timestamp_ms": timestamp
        });
        connection
            .execute(
                "INSERT INTO materialised_charge_samples(
                    vehicle_id, sample_id, charge_id, sample_json
                 ) VALUES (?1, ?2, 7, ?3)",
                rusqlite::params![vehicle.to_string(), 700 + index as i64, sample.to_string()],
            )
            .expect("sample");
    }
    Fixture {
        _directory: directory,
        store,
        vehicle,
    }
}

fn run(fixture: &Fixture, arguments: &[&str]) -> (Result<serde_json::Value, String>, String) {
    let (charge_id, cost, mode) = set_charge_cost(arguments).expect("arguments parse");
    let mut diagnostics = Vec::new();
    let result = run_set_charge_cost(
        &fixture.store,
        fixture.vehicle,
        charge_id,
        cost,
        &mode,
        &mut diagnostics,
    )
    .map_err(|error| error.to_string());
    (
        result,
        String::from_utf8(diagnostics).expect("diagnostics are UTF-8"),
    )
}

#[test]
fn t37_total_mode_prints_the_updated_row_and_keeps_the_copy() {
    let fixture = fixture();
    let (result, diagnostics) = run(
        &fixture,
        &["--charge-id", "7", "--cost", "18.4", "--mode", "total"],
    );
    let document = result.expect("total");
    assert_eq!(diagnostics, "", "no deprecation line for a current mode");
    assert_eq!(document["status"], "updated");
    assert_eq!(document["vehicleId"], fixture.vehicle.to_string());
    assert_eq!(document["charge"]["id"], 7);
    assert_eq!(document["charge"]["cost"], 18.4);
    assert_eq!(document["charge"]["billing_type"], "per_kwh");
    assert_eq!(document["charge"]["cost_per_unit"], 0.3);
    assert_eq!(document["charge"]["session_fee"], 1.0);
    assert_eq!(
        document["charge"],
        serde_json::to_value(
            fixture
                .store
                .materialised_charge_for_vehicle(fixture.vehicle, 7)
                .expect("read")
                .expect("row")
        )
        .expect("json")
    );
    // Repeating it adds no sync mutation and still prints the row.
    let (again, _) = run(
        &fixture,
        &["--charge-id", "7", "--cost", "18.4", "--mode", "total"],
    );
    assert_eq!(again.expect("again"), document);
    assert_eq!(mutation_count(&fixture), 1);
}

fn mutation_count(fixture: &Fixture) -> i64 {
    fixture
        .store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations WHERE vehicle_id = ?1 AND entity = 'charge'",
            rusqlite::params![fixture.vehicle.to_string()],
            |row| row.get(0),
        )
        .expect("count")
}

#[test]
fn t38_per_kwh_mode_calls_the_rate_entry_with_the_per_kwh_type() {
    let fixture = fixture();
    let (result, _) = run(
        &fixture,
        &["--charge-id", "7", "--cost", "0.25", "--mode", "per_kwh"],
    );
    let charge = &result.expect("per kWh")["charge"];
    assert_eq!(charge["cost"], 5.0);
    assert_eq!(charge["billing_type"], "per_kwh");
    assert_eq!(charge["cost_per_unit"], 0.25);
    assert!(charge["session_fee"].is_null());
}

#[test]
fn t40_per_minute_mode_calls_the_rate_entry_with_the_per_minute_type() {
    let fixture = fixture();
    let (result, _) = run(
        &fixture,
        &["--charge-id", "7", "--cost", "0.25", "--mode", "per_minute"],
    );
    let charge = &result.expect("per minute")["charge"];
    assert_eq!(charge["cost"], 23.88);
    assert_eq!(charge["billing_type"], "per_minute");
    assert_eq!(charge["cost_per_unit"], 0.25);
}

#[test]
fn t41_store_errors_reach_the_owner_and_write_nothing() {
    let fixture = fixture();
    let (result, _) = run(
        &fixture,
        &["--charge-id", "99", "--cost", "1", "--mode", "total"],
    );
    let message = result.expect_err("unknown charge");
    assert!(message.contains("99"), "{message}");
    let (result, _) = run(
        &fixture,
        &["--charge-id", "7", "--cost", "-0.05", "--mode", "per_kwh"],
    );
    result.expect_err("negative rate");
    let (result, _) = run(
        &fixture,
        &[
            "--charge-id",
            "7",
            "--cost",
            "100000",
            "--mode",
            "per_minute",
        ],
    );
    result.expect_err("rate above the range");
    assert_eq!(mutation_count(&fixture), 0);
}

#[test]
fn t41_an_unknown_mode_is_a_usage_error_naming_the_three_modes() {
    let error = set_charge_cost(&["--charge-id", "7", "--cost", "1", "--mode", "hourly"])
        .expect_err("hourly is not a mode");
    let message = error.to_string();
    for name in ["total", "per_kwh", "per_minute"] {
        assert!(message.contains(name), "{message}");
    }
    assert!(message.contains("hourly"), "{message}");
}

#[test]
fn r53_the_three_modes_parse_exactly() {
    for (text, mode) in [
        ("total", ChargeCostMode::Total),
        ("per_kwh", ChargeCostMode::PerKwh),
        ("per_minute", ChargeCostMode::PerMinute),
    ] {
        let (charge_id, cost, choice) =
            set_charge_cost(&["--charge-id", "7", "--cost", "1.5", "--mode", text])
                .expect("mode parses");
        assert_eq!((charge_id, cost), (7, 1.5));
        assert_eq!(
            choice,
            ChargeCostModeChoice {
                mode,
                deprecated_alias: None
            }
        );
    }
    // Names are exact: a different case or spelling is not a mode.
    for text in ["TOTAL", "per-kwh", "perkwh", "per_hour", ""] {
        assert!(
            set_charge_cost(&["--charge-id", "7", "--cost", "1", "--mode", text]).is_err(),
            "{text:?}"
        );
    }
}

#[test]
fn r53_a_manual_total_may_be_negative_on_the_command_line() {
    let (_, cost, _) = set_charge_cost(&["--charge-id", "7", "--cost", "-3.20", "--mode", "total"])
        .expect("a credit parses");
    assert_eq!(cost, -3.20);
    let fixture = fixture();
    let (result, _) = run(
        &fixture,
        &["--charge-id", "7", "--cost", "-3.20", "--mode", "total"],
    );
    assert_eq!(result.expect("credit")["charge"]["cost"], -3.20);
}

#[test]
fn r53a_the_mode_is_required_in_this_release_and_the_error_names_the_modes() {
    let error =
        set_charge_cost(&["--charge-id", "7", "--cost", "18.4"]).expect_err("--mode is required");
    let message = error.to_string();
    assert!(message.contains("--mode"), "{message}");
    for name in ["total", "per_kwh", "per_minute"] {
        assert!(message.contains(name), "{message}");
    }
    // The charge id and cost keep their own checks.
    assert!(set_charge_cost(&["--charge-id", "0", "--cost", "1", "--mode", "total"]).is_err());
    assert!(set_charge_cost(&["--charge-id", "7", "--mode", "total"]).is_err());
}

#[test]
fn t47_a_deprecated_alias_means_its_new_mode_and_says_so_once() {
    // The old release's value set is the dirty side's to supply (C2 section 12
    // item 9) and the shipped table is empty until it is, so the mechanism is
    // tested with a synthetic table: one alias for each of the three modes.
    let aliases = [
        ("synthetic-total", ChargeCostMode::Total),
        ("synthetic-kwh", ChargeCostMode::PerKwh),
        ("synthetic-minute", ChargeCostMode::PerMinute),
    ];
    for (alias, mode) in aliases {
        let choice = parse_charge_cost_mode_in(alias, &aliases).expect("alias parses");
        assert_eq!(
            choice,
            ChargeCostModeChoice {
                mode,
                deprecated_alias: Some(alias.to_owned())
            }
        );
        let line = deprecated_charge_cost_mode_line(alias, mode);
        assert!(!line.contains('\n'), "one line: {line}");
        assert!(
            line.contains(alias) && line.contains("deprecated"),
            "{line}"
        );
        assert!(
            line.contains(&format!("--mode {}", mode_name(mode))),
            "{line}"
        );
    }
    // An old value with no such meaning is a usage error naming the modes.
    let error = parse_charge_cost_mode_in("synthetic-other", &aliases).expect_err("not listed");
    assert!(error.contains("total, per_kwh or per_minute"), "{error}");
    // The new names win over an alias of the same text, and need no line.
    assert_eq!(
        parse_charge_cost_mode_in("total", &[("total", ChargeCostMode::PerKwh)])
            .expect("current name"),
        ChargeCostModeChoice {
            mode: ChargeCostMode::Total,
            deprecated_alias: None
        }
    );
}

fn mode_name(mode: ChargeCostMode) -> &'static str {
    mode.as_str()
}

#[test]
fn t47_an_alias_makes_the_store_call_of_its_new_mode_with_one_stderr_line() {
    let fixture = fixture();
    let aliases = [("synthetic-kwh", ChargeCostMode::PerKwh)];
    let choice = parse_charge_cost_mode_in("synthetic-kwh", &aliases).expect("alias");
    let mut diagnostics = Vec::new();
    let document = run_set_charge_cost(
        &fixture.store,
        fixture.vehicle,
        7,
        0.25,
        &choice,
        &mut diagnostics,
    )
    .expect("alias run");
    // The same row the new mode gives.
    assert_eq!(document["charge"]["cost"], 5.0);
    assert_eq!(document["charge"]["cost_per_unit"], 0.25);
    let text = String::from_utf8(diagnostics).expect("UTF-8");
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.contains("synthetic-kwh") && text.contains("--mode per_kwh"),
        "{text}"
    );
}

#[test]
fn the_shipped_alias_table_is_empty_until_the_dirty_side_supplies_the_old_values() {
    assert!(crate::DEPRECATED_CHARGE_COST_MODES.is_empty());
    // With it empty, only the three current names parse.
    assert!(set_charge_cost(&["--charge-id", "7", "--cost", "1", "--mode", "kwh"]).is_err());
}

#[test]
fn t35_set_geofence_keeps_its_recalculate_flag() {
    let cli = parse(&[
        "set-geofence",
        "--name",
        "Home",
        "--latitude",
        "10.0",
        "--longitude",
        "20.0",
        "--cost-per-unit",
        "0.30",
        "--recalculate-missing-costs",
    ])
    .expect("flag parses");
    assert!(matches!(
        cli.command,
        Command::Control {
            command: ControlCommand::SetGeofence {
                recalculate_missing_costs: true,
                ..
            },
            ..
        }
    ));
    let without = parse(&[
        "set-geofence",
        "--name",
        "Home",
        "--latitude",
        "10.0",
        "--longitude",
        "20.0",
    ])
    .expect("no flag");
    assert!(matches!(
        without.command,
        Command::Control {
            command: ControlCommand::SetGeofence {
                recalculate_missing_costs: false,
                ..
            },
            ..
        }
    ));
}

/// Add a Hub charge of car 1 that started `place` (latitude, longitude) with
/// no cost, 10.00 kWh used and three samples, and return its id.
fn put_unpriced_row(fixture: &Fixture, id: i64, place: (f64, f64)) -> i64 {
    let start = 1_760_000_000_000_i64 + id * 86_400_000;
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
            rusqlite::params![fixture.vehicle.to_string(), id, row.to_string()],
        )
        .expect("row");
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
                rusqlite::params![
                    fixture.vehicle.to_string(),
                    sample_id,
                    id,
                    sample.to_string()
                ],
            )
            .expect("sample");
    }
    id
}

fn run_geofence(fixture: &Fixture, extra: &[&str]) -> serde_json::Value {
    let mut arguments = vec![
        "set-geofence",
        "--name",
        "Home",
        "--latitude",
        "10.0",
        "--longitude",
        "20.0",
        "--radius-m",
        "150",
        "--cost-per-unit",
        "0.30",
    ];
    arguments.extend_from_slice(extra);
    let cli = parse(&arguments).expect("arguments parse");
    let Command::Control { command, .. } = cli.command else {
        panic!("not a control command");
    };
    run_set_geofence(&fixture.store, fixture.vehicle, &command).expect("set-geofence")
}

fn cost_of(fixture: &Fixture, id: i64) -> Option<f64> {
    fixture
        .store
        .materialised_charge_for_vehicle(fixture.vehicle, id)
        .expect("read")
        .expect("row")
        .cost
}

#[test]
fn t35_the_flag_fills_unpriced_hub_rows_in_the_saved_fence_and_reports_the_count() {
    let fixture = fixture();
    // Inside the fence (55 m from its centre), and outside it (about 111 km).
    let inside = put_unpriced_row(&fixture, 9, (10.0005, 20.0));
    let outside = put_unpriced_row(&fixture, 10, (11.0, 20.0));
    let document = run_geofence(&fixture, &["--recalculate-missing-costs"]);
    assert_eq!(document["status"], "updated");
    assert_eq!(document["vehicleId"], fixture.vehicle.to_string());
    assert_eq!(document["geofence"]["name"], "Home");
    assert_eq!(document["recalculatedChargeCosts"], 1);
    // 10.00 kWh used at 0.30 per kWh, with no fee.
    assert_eq!(cost_of(&fixture, inside), Some(3.0));
    assert_eq!(cost_of(&fixture, outside), None);
    let row = fixture
        .store
        .materialised_charge_for_vehicle(fixture.vehicle, inside)
        .expect("read")
        .expect("row");
    assert_eq!(row.cost_per_unit, Some(0.30));
    // Row 7 has no start place and keeps its stored value.
    assert_eq!(cost_of(&fixture, 7), None);
    // A second call prices nothing more and reports 0.
    let again = run_geofence(&fixture, &["--recalculate-missing-costs"]);
    assert_eq!(again["recalculatedChargeCosts"], 0);
}

#[test]
fn t35_without_the_flag_no_past_charge_is_touched() {
    let fixture = fixture();
    let inside = put_unpriced_row(&fixture, 9, (10.0005, 20.0));
    let document = run_geofence(&fixture, &[]);
    assert_eq!(document["recalculatedChargeCosts"], 0);
    // Saving a fence may label the rows inside it (C8), but it prices nothing
    // and copies no tariff onto a row.
    assert_eq!(cost_of(&fixture, inside), None);
    let row = fixture
        .store
        .materialised_charge_for_vehicle(fixture.vehicle, inside)
        .expect("read")
        .expect("row");
    assert_eq!(row.billing_type, None);
    assert_eq!(row.cost_per_unit, None);
}
