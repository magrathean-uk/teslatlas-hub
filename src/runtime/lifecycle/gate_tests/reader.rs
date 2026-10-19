// SPDX-License-Identifier: AGPL-3.0-only

//! The strict export reader (C2 R75 (a) and (b)): `sessions.csv`, `cars.csv`
//! and `operators.csv` as PostgreSQL's client writes them, read without a
//! dependency. Test builds only.
//!
//! Grammar (RFC 4180 as PostgreSQL's CSV output writes it): fields are
//! separated by commas and records by `\n` or `\r\n`; a field holding a comma,
//! a double quote or a line break is enclosed in double quotes, a quote inside
//! it is doubled, and a quoted field may hold a line break. An unquoted empty
//! field is `None`; a quoted empty field is `Some("")`; any other field is its
//! text. Stricter than the grammar needs, the reader also refuses a double
//! quote inside an unquoted field, a bare carriage return, and text that is
//! not UTF-8, because PostgreSQL writes none of them.
//!
//! A row number counts data rows from 1; the header is not a row. The header
//! is read and compared first, so a wrong header is refused before any row.

use std::collections::HashSet;
use std::fmt;
use std::io::BufRead;

use super::fold::ChargeGateFold;
use super::report::{ProcessGateFacts, SiteOperator};
use super::*;

pub(crate) const SESSIONS_FILE: &str = "sessions.csv";
pub(crate) const CARS_FILE: &str = "cars.csv";
pub(crate) const OPERATORS_FILE: &str = "operators.csv";

/// C2 R75 (a)'s first query's column names, in order.
pub(super) const SESSIONS_HEADER: [&str; 19] = [
    "car_id",
    "charging_process_id",
    "start_ms",
    "end_ms",
    "id",
    "date_ms",
    "battery_level",
    "charge_energy_added",
    "charger_power",
    "charger_voltage",
    "charger_actual_current",
    "charger_phases",
    "rated_battery_range_km",
    "outside_temp",
    "battery_heater_on",
    "battery_heater",
    "fast_charger_present",
    "fast_charger_type",
    "fast_charger_brand",
];
const CARS_HEADER: [&str; 2] = ["car_id", "lfp_battery"];
const OPERATORS_HEADER: [&str; 2] = ["charging_process_id", "operator"];

/// A fault in the CSV text itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CsvFault {
    /// A quote left open at the end of the file.
    QuoteOpen,
    /// A character other than a comma or a line break after a closing quote.
    AfterQuote(u8),
    /// A double quote inside an unquoted field.
    QuoteInUnquoted,
    /// A carriage return not followed by a line feed.
    BareCarriageReturn,
    /// Text that is not UTF-8.
    NotUtf8,
}

/// Why a data row was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowFault {
    Csv(CsvFault),
    /// The record has a different number of fields than the header.
    FieldCount {
        expected: usize,
        found: usize,
    },
    /// A field that is missing or does not parse.
    Field {
        column: &'static str,
    },
    /// `(date_ms, id)` is below the previous row's in the same process.
    SampleOrder,
    /// A process that reappears after another.
    ProcessRepeated,
    /// `start_ms` or `end_ms` differs from the process's first row.
    ProcessBounds,
    /// An operator answer other than `tesla` or `other`.
    OperatorVocabulary,
}

/// Why an export was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GateReadError {
    Io(String),
    /// The file has no header record.
    HeaderMissing {
        file: &'static str,
    },
    /// The header record is not valid CSV.
    HeaderMalformed {
        file: &'static str,
        fault: CsvFault,
    },
    /// The first column whose name differs, or the first column past the
    /// shorter header when the names agree so far (0-based), as expected and
    /// found; `None` is a column that is absent on that side, and a column
    /// present without a name (an empty header field) is `Some("")`.
    HeaderMismatch {
        file: &'static str,
        column: usize,
        expected: Option<&'static str>,
        found: Option<String>,
    },
    Row {
        file: &'static str,
        row: u64,
        fault: RowFault,
    },
}

impl fmt::Display for GateReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "read failed: {message}"),
            Self::HeaderMissing { file } => write!(f, "{file}: no header"),
            Self::HeaderMalformed { file, fault } => {
                write!(f, "{file}: malformed header ({fault:?})")
            }
            Self::HeaderMismatch {
                file,
                column,
                expected,
                found,
            } => write!(
                f,
                "{file}: header column {} is {found:?}, expected {expected:?}",
                column + 1
            ),
            Self::Row { file, row, fault } => write!(f, "{file}: row {row} refused ({fault:?})"),
        }
    }
}

type Record = Vec<Option<String>>;

fn io_error(error: std::io::Error) -> GateReadError {
    GateReadError::Io(error.to_string())
}

fn peek(input: &mut impl BufRead) -> std::io::Result<Option<u8>> {
    Ok(input.fill_buf()?.first().copied())
}

/// Read the next record, or `None` at the end of the input.
fn next_record(input: &mut impl BufRead) -> std::io::Result<Result<Option<Record>, CsvFault>> {
    if peek(input)?.is_none() {
        return Ok(Ok(None));
    }
    let mut fields: Record = Vec::new();
    loop {
        let mut bytes = Vec::new();
        let quoted = peek(input)? == Some(b'"');
        if quoted {
            input.consume(1);
            loop {
                let Some(byte) = peek(input)? else {
                    return Ok(Err(CsvFault::QuoteOpen));
                };
                input.consume(1);
                if byte != b'"' {
                    bytes.push(byte);
                } else if peek(input)? == Some(b'"') {
                    input.consume(1);
                    bytes.push(b'"');
                } else {
                    break;
                }
            }
            if let Some(next) = peek(input)?
                && !matches!(next, b',' | b'\n' | b'\r')
            {
                return Ok(Err(CsvFault::AfterQuote(next)));
            }
        } else {
            while let Some(byte) = peek(input)? {
                match byte {
                    b',' | b'\n' | b'\r' => break,
                    b'"' => return Ok(Err(CsvFault::QuoteInUnquoted)),
                    _ => {
                        bytes.push(byte);
                        input.consume(1);
                    }
                }
            }
        }
        if !quoted && bytes.is_empty() {
            fields.push(None);
        } else {
            match String::from_utf8(bytes) {
                Ok(text) => fields.push(Some(text)),
                Err(_) => return Ok(Err(CsvFault::NotUtf8)),
            }
        }
        match peek(input)? {
            Some(b',') => input.consume(1),
            Some(b'\r') => {
                input.consume(1);
                if peek(input)? != Some(b'\n') {
                    return Ok(Err(CsvFault::BareCarriageReturn));
                }
                input.consume(1);
                return Ok(Ok(Some(fields)));
            }
            Some(b'\n') => {
                input.consume(1);
                return Ok(Ok(Some(fields)));
            }
            _ => return Ok(Ok(Some(fields))),
        }
    }
}

/// Read and check the header record before any row.
fn read_header(
    input: &mut impl BufRead,
    file: &'static str,
    expected: &[&'static str],
) -> Result<(), GateReadError> {
    let header = match next_record(input).map_err(io_error)? {
        Ok(Some(header)) => header,
        Ok(None) => return Err(GateReadError::HeaderMissing { file }),
        Err(fault) => return Err(GateReadError::HeaderMalformed { file, fault }),
    };
    let mismatch = |column: usize| GateReadError::HeaderMismatch {
        file,
        column,
        expected: expected.get(column).copied(),
        found: header
            .get(column)
            .map(|field| field.clone().unwrap_or_default()),
    };
    // The names first, so that the first column that differs is the one named.
    let first_difference = expected
        .iter()
        .zip(&header)
        .position(|(wanted, found)| found.as_deref() != Some(*wanted));
    if let Some(column) = first_difference {
        return Err(mismatch(column));
    }
    // Then the width, so that an extra column is refused even when it has no
    // name: an unquoted empty header field reads as `None`, which is also what
    // a column past the expected width reads as. The first column past the
    // shorter side is the one named.
    if header.len() != expected.len() {
        return Err(mismatch(expected.len().min(header.len())));
    }
    Ok(())
}

/// Read the next data row, numbered from 1, with its field count checked.
fn read_row(
    input: &mut impl BufRead,
    file: &'static str,
    row: u64,
    width: usize,
) -> Result<Option<Record>, GateReadError> {
    let fault = |fault| GateReadError::Row { file, row, fault };
    match next_record(input).map_err(io_error)? {
        Err(csv) => Err(fault(RowFault::Csv(csv))),
        Ok(None) => Ok(None),
        Ok(Some(record)) if record.len() == width => Ok(Some(record)),
        Ok(Some(record)) => Err(fault(RowFault::FieldCount {
            expected: width,
            found: record.len(),
        })),
    }
}

/// A plain decimal number: an optional minus sign, digits and an optional
/// fraction. PostgreSQL writes no other form, so `NaN`, exponents and signs
/// other than minus are refused.
fn decimal(text: &str) -> Option<f64> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, "0"));
    let plain = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    (plain(whole) && plain(fraction))
        .then(|| text.parse::<f64>().ok())
        .flatten()
        .filter(|value| value.is_finite())
}

fn integer(text: &str) -> Option<i64> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// One row's fields, parsed by column.
struct Cells<'a> {
    record: &'a Record,
}

impl Cells<'_> {
    fn text(&self, index: usize) -> Option<&str> {
        self.record[index].as_deref()
    }

    fn required_i64(&self, index: usize, column: &'static str) -> Result<i64, RowFault> {
        self.text(index)
            .and_then(integer)
            .ok_or(RowFault::Field { column })
    }

    fn optional_i64(&self, index: usize, column: &'static str) -> Result<Option<i64>, RowFault> {
        self.text(index)
            .map(|text| integer(text).ok_or(RowFault::Field { column }))
            .transpose()
    }

    /// A decimal at its column scale (C1 R36), rounded half away from zero.
    fn scaled(
        &self,
        index: usize,
        column: &'static str,
        decimals: u32,
    ) -> Result<Option<f64>, RowFault> {
        self.text(index)
            .map(|text| {
                decimal(text)
                    .map(|value| round_to_precision(value, decimals))
                    .ok_or(RowFault::Field { column })
            })
            .transpose()
    }

    fn boolean(&self, index: usize, column: &'static str) -> Result<Option<bool>, RowFault> {
        match self.text(index) {
            None => Ok(None),
            Some("t") => Ok(Some(true)),
            Some("f") => Ok(Some(false)),
            Some(_) => Err(RowFault::Field { column }),
        }
    }

    fn owned_text(&self, index: usize) -> Option<String> {
        self.text(index).map(str::to_owned)
    }
}

/// One `sessions.csv` row as a sample at C1 R36's column scale (C2 R75 (b) 3);
/// every field the export does not select is `None`.
fn sample_of(cells: &Cells<'_>) -> Result<ProjectionChargeSample, RowFault> {
    Ok(ProjectionChargeSample {
        id: cells.required_i64(4, "id")?,
        charge_process_id: cells.required_i64(1, "charging_process_id")?,
        timestamp_ms: cells.required_i64(5, "date_ms")?,
        battery_level: cells.optional_i64(6, "battery_level")?,
        usable_battery_level: None,
        charge_energy_added_kwh: cells.scaled(7, "charge_energy_added", 2)?,
        charger_power_kw: cells.scaled(8, "charger_power", 0)?,
        charger_voltage: cells.scaled(9, "charger_voltage", 0)?,
        charger_actual_current: cells.scaled(10, "charger_actual_current", 0)?,
        charger_pilot_current: None,
        charger_phases: cells.optional_i64(11, "charger_phases")?,
        ideal_range_km: None,
        rated_range_km: cells.scaled(12, "rated_battery_range_km", 2)?,
        outside_temp_c: cells.scaled(13, "outside_temp", 1)?,
        battery_heater_on: cells.boolean(14, "battery_heater_on")?,
        battery_heater: cells.boolean(15, "battery_heater")?,
        battery_heater_no_power: None,
        not_enough_power_to_heat: None,
        fast_charger_present: cells.boolean(16, "fast_charger_present")?,
        fast_charger_brand: cells.owned_text(18),
        fast_charger_type: cells.owned_text(17),
        charge_cable: None,
    })
}

/// The sessions of an export, folded as their rows arrive, with the number of
/// data rows read.
pub(crate) struct SessionsRead {
    pub processes: Vec<ProcessGateFacts>,
    pub rows: u64,
}

/// The process whose rows are arriving: its identity, the previous row's
/// `(date_ms, id)` and its fold.
struct OpenProcess {
    car_id: i64,
    process_id: i64,
    start_ms: i64,
    end_ms: i64,
    last_key: (i64, i64),
    fold: ChargeGateFold,
}

impl OpenProcess {
    fn finish(self) -> ProcessGateFacts {
        ProcessGateFacts {
            car_id: self.car_id,
            process_id: self.process_id,
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            facts: self.fold.finish(),
        }
    }
}

/// Read `sessions.csv` (C2 R75 (b) 1 to 5): the header is checked first; rows must
/// arrive grouped by `(car_id, charging_process_id)`, each group once, in
/// `(date_ms, id)` order within it; each group is folded as its rows arrive,
/// and only the open fold and the finished facts are held.
pub(crate) fn read_sessions(mut input: impl BufRead) -> Result<SessionsRead, GateReadError> {
    read_header(&mut input, SESSIONS_FILE, &SESSIONS_HEADER)?;
    let mut processes = Vec::new();
    let mut seen: HashSet<(i64, i64)> = HashSet::new();
    let mut open: Option<OpenProcess> = None;
    let mut rows = 0_u64;
    loop {
        let row = rows + 1;
        let fault = |fault| GateReadError::Row {
            file: SESSIONS_FILE,
            row,
            fault,
        };
        let Some(record) = read_row(&mut input, SESSIONS_FILE, row, SESSIONS_HEADER.len())? else {
            break;
        };
        rows = row;
        let cells = Cells { record: &record };
        let car_id = cells.required_i64(0, "car_id").map_err(fault)?;
        let process_id = cells
            .required_i64(1, "charging_process_id")
            .map_err(fault)?;
        let start_ms = cells.required_i64(2, "start_ms").map_err(fault)?;
        let end_ms = cells.required_i64(3, "end_ms").map_err(fault)?;
        let sample = sample_of(&cells).map_err(fault)?;
        let key = (sample.timestamp_ms, sample.id);
        match open.as_mut() {
            Some(current) if (current.car_id, current.process_id) == (car_id, process_id) => {
                if (current.start_ms, current.end_ms) != (start_ms, end_ms) {
                    return Err(fault(RowFault::ProcessBounds));
                }
                if key < current.last_key {
                    return Err(fault(RowFault::SampleOrder));
                }
                current.last_key = key;
                current.fold.observe(&sample);
            }
            _ => {
                if !seen.insert((car_id, process_id)) {
                    return Err(fault(RowFault::ProcessRepeated));
                }
                if let Some(finished) = open.take() {
                    processes.push(finished.finish());
                }
                let mut fold = ChargeGateFold::new();
                fold.observe(&sample);
                open = Some(OpenProcess {
                    car_id,
                    process_id,
                    start_ms,
                    end_ms,
                    last_key: key,
                    fold,
                });
            }
        }
    }
    processes.extend(open.map(OpenProcess::finish));
    Ok(SessionsRead { processes, rows })
}

/// Read `cars.csv`: `(car_id, lfp_battery)`, the second `None` when unknown.
pub(crate) fn read_cars(
    mut input: impl BufRead,
) -> Result<Vec<(i64, Option<bool>)>, GateReadError> {
    read_header(&mut input, CARS_FILE, &CARS_HEADER)?;
    let mut cars = Vec::new();
    let mut row = 0_u64;
    loop {
        row += 1;
        let Some(record) = read_row(&mut input, CARS_FILE, row, CARS_HEADER.len())? else {
            return Ok(cars);
        };
        let cells = Cells { record: &record };
        let fault = |fault| GateReadError::Row {
            file: CARS_FILE,
            row,
            fault,
        };
        let car_id = cells.required_i64(0, "car_id").map_err(fault)?;
        let lfp = cells.boolean(1, "lfp_battery").map_err(fault)?;
        cars.push((car_id, lfp));
    }
}

/// Read `operators.csv` (C2 R75 (b) 7): the owner's answers, `tesla` or `other`
/// exactly.
pub(crate) fn read_operators(
    mut input: impl BufRead,
) -> Result<Vec<(i64, SiteOperator)>, GateReadError> {
    read_header(&mut input, OPERATORS_FILE, &OPERATORS_HEADER)?;
    let mut answers = Vec::new();
    let mut row = 0_u64;
    loop {
        row += 1;
        let Some(record) = read_row(&mut input, OPERATORS_FILE, row, OPERATORS_HEADER.len())?
        else {
            return Ok(answers);
        };
        let cells = Cells { record: &record };
        let fault = |fault| GateReadError::Row {
            file: OPERATORS_FILE,
            row,
            fault,
        };
        let process_id = cells
            .required_i64(0, "charging_process_id")
            .map_err(fault)?;
        let operator = match cells.text(1) {
            Some("tesla") => SiteOperator::Tesla,
            Some("other") => SiteOperator::Other,
            _ => return Err(fault(RowFault::OperatorVocabulary)),
        };
        answers.push((process_id, operator));
    }
}
