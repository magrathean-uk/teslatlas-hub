// SPDX-License-Identifier: AGPL-3.0-only

//! The historical charge-gate measurement (C2 section 11, C2 R74 and C2 R75): a
//! test-only fold of one charging session's gate facts, a pure report over one
//! car's sessions, a strict reader of the three export files, and the manual
//! runner that applies them to an export directory. Test builds only: the
//! measurement is not a product feature.
//!
//! The children are `fold` (C2 R74 (b)), `report` (C2 R75 (c)) and `reader` (C2 R75 (a)
//! and (b)); `support` holds the sample builders the tests share; `b3_hg1`,
//! `b3_hg2` and `b3_hg3` are the spec tests of the three parts, and `b3_c2_a`
//! the gate-level tests of C2 R76 (C2 T79, C2 T83 (d), C2 T84). The ignored
//! runner `g2_10_historical_charge_gates` is registered here, apart from the
//! ordinary tests.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use super::*;

#[path = "gate_tests/fold.rs"]
mod fold;
#[path = "gate_tests/reader.rs"]
mod reader;
#[path = "gate_tests/report.rs"]
mod report;
#[path = "gate_tests/support.rs"]
mod support;

#[path = "gate_tests/c2_a_tests.rs"]
mod b3_c2_a;
#[path = "gate_tests/hg1_tests.rs"]
mod b3_hg1;
#[path = "gate_tests/hg2_tests.rs"]
mod b3_hg2;
#[path = "gate_tests/hg3_tests.rs"]
mod b3_hg3;

use reader::{
    CARS_FILE, GateReadError, OPERATORS_FILE, SESSIONS_FILE, read_cars, read_operators,
    read_sessions,
};
use report::charge_gate_report;

/// The directory the runner reads (C2 R75 (b)).
const HISTORICAL_DIR_VAR: &str = "TESLATLAS_G2_10_HISTORICAL_DIR";

fn open_export(dir: &Path, file: &str) -> Result<Option<BufReader<fs::File>>, GateReadError> {
    let path = dir.join(file);
    if !path.exists() {
        return Ok(None);
    }
    fs::File::open(&path)
        .map(|handle| Some(BufReader::new(handle)))
        .map_err(|error| GateReadError::Io(format!("{file}: {error}")))
}

/// The SHA-256 of a file, as lower-case hex, read in chunks.
fn file_digest(dir: &Path, file: &str) -> Result<String, GateReadError> {
    let io = |error: std::io::Error| GateReadError::Io(format!("{file}: {error}"));
    let mut handle = fs::File::open(dir.join(file)).map_err(io)?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0_u8; 1 << 16];
    loop {
        let read = handle.read(&mut chunk).map_err(io)?;
        if read == 0 {
            break;
        }
        hasher.update(&chunk[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Read the export in `dir`, report each car and write `report-<car_id>.json`
/// beside it with the files' SHA-256 digests and row counts (C2 R75 (b), (c)).
/// Nothing is written to a Hub store and no network connection is opened (C2 R75
/// (d)). `cars.csv` and `operators.csv` are optional. Returns the paths written.
fn run_historical_gates(dir: &Path) -> Result<Vec<PathBuf>, GateReadError> {
    let sessions = open_export(dir, SESSIONS_FILE)?.ok_or_else(|| {
        GateReadError::Io(format!(
            "{SESSIONS_FILE}: not found in the export directory"
        ))
    })?;
    let sessions = read_sessions(sessions)?;
    let cars = match open_export(dir, CARS_FILE)? {
        Some(input) => Some(read_cars(input)?),
        None => None,
    };
    let answers = match open_export(dir, OPERATORS_FILE)? {
        Some(input) => Some(read_operators(input)?),
        None => None,
    };
    let export = |file: &str, rows: Option<u64>| -> Result<Value, GateReadError> {
        Ok(match rows {
            Some(rows) => json!({"sha256": file_digest(dir, file)?, "rows": rows}),
            None => Value::Null,
        })
    };
    let export = json!({
        SESSIONS_FILE: export(SESSIONS_FILE, Some(sessions.rows))?,
        CARS_FILE: export(CARS_FILE, cars.as_ref().map(|cars| cars.len() as u64))?,
        OPERATORS_FILE: export(OPERATORS_FILE, answers.as_ref().map(|answers| answers.len() as u64))?,
    });
    let car_ids = sessions
        .processes
        .iter()
        .map(|process| process.car_id)
        .collect::<BTreeSet<_>>();
    let mut written = Vec::new();
    for car_id in car_ids {
        let lfp_battery = cars
            .iter()
            .flatten()
            .find(|(id, _)| *id == car_id)
            .and_then(|(_, lfp)| *lfp);
        let report = charge_gate_report(
            &sessions.processes,
            car_id,
            lfp_battery,
            answers.as_deref().unwrap_or(&[]),
        );
        let document = json!({
            "measurement": "g2_10_historical_charge_gates",
            "fold": "C2-A fold (C2 sections 11 and 17 with C2 R76: counter tolerance, signed intervals, final declines); C2-B and C2-C amendments and C1 ST-G4 are not claimed",
            "export": export,
            "report": report.to_json(),
        });
        let path = dir.join(format!("report-{car_id}.json"));
        let text = serde_json::to_string_pretty(&document)
            .map_err(|error| GateReadError::Io(error.to_string()))?;
        fs::write(&path, text + "\n")
            .map_err(|error| GateReadError::Io(format!("{}: {error}", path.display())))?;
        written.push(path);
    }
    Ok(written)
}

/// Manual run of the historical gates (C2 R75 (b)): reads the export directory
/// named by `TESLATLAS_G2_10_HISTORICAL_DIR` and writes one report per car
/// beside it. Ignored: run by hand, under the heavy-build lock and the memory
/// guard, never in a mutation run (resource decision 2026-10-11).
#[test]
#[ignore = "manual: reads the export in TESLATLAS_G2_10_HISTORICAL_DIR; run by hand under the heavy-build lock and the memory guard, never in a mutation run"]
fn g2_10_historical_charge_gates() {
    let dir = std::env::var_os(HISTORICAL_DIR_VAR)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {HISTORICAL_DIR_VAR} to the export directory"));
    let written = run_historical_gates(&dir).unwrap_or_else(|error| panic!("{error}"));
    assert!(!written.is_empty(), "the export held no session");
    for path in written {
        println!("wrote {}", path.display());
    }
}
