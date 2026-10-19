// SPDX-License-Identifier: AGPL-3.0-only

//! HG3 spec tests (C2 section 17.1, C2 R75 (a) and (b); C2 T77): the strict export
//! reader. Synthetic data only; the files are built in memory. A row number
//! counts data rows from 1 (the header is not a row).

use super::reader::{
    CARS_FILE, CsvFault, GateReadError, OPERATORS_FILE, RowFault, read_cars, read_operators,
    read_sessions,
};
use super::report::{ProcessGateFacts, SiteOperator};
use super::support::*;
use super::*;

const HEADER: &str = "car_id,charging_process_id,start_ms,end_ms,id,date_ms,battery_level,\
charge_energy_added,charger_power,charger_voltage,charger_actual_current,charger_phases,\
rated_battery_range_km,outside_temp,battery_heater_on,battery_heater,fast_charger_present,\
fast_charger_type,fast_charger_brand";

/// Column positions of the export.
const COL_ID: usize = 4;
const COL_DATE: usize = 5;
const COL_POWER: usize = 8;
const COL_TYPE: usize = 17;
const COL_BRAND: usize = 18;

type Row = [String; 19];

/// A DC-looking row of process `pid` (start `T0`, end `T0 + 1 h`) with the
/// given sample id, minutes after `T0`, counter and power; every other column
/// is plain.
fn row(pid: i64, id: i64, minutes: i64, c: &str, power: &str) -> Row {
    [
        "1".to_owned(),
        pid.to_string(),
        T0.to_string(),
        (T0 + 60 * MIN).to_string(),
        id.to_string(),
        (T0 + minutes * MIN).to_string(),
        "50".to_owned(),
        c.to_owned(),
        power.to_owned(),
        "2".to_owned(),
        "0".to_owned(),
        String::new(),
        "200.00".to_owned(),
        "12.5".to_owned(),
        "f".to_owned(),
        String::new(),
        "t".to_owned(),
        "Supercharger".to_owned(),
        "Tesla".to_owned(),
    ]
}

fn csv(header: &str, rows: &[Row]) -> String {
    let mut text = format!("{header}\n");
    for row in rows {
        text.push_str(&row.join(","));
        text.push('\n');
    }
    text
}

fn read(text: &str) -> Result<Vec<ProcessGateFacts>, GateReadError> {
    read_sessions(text.as_bytes()).map(|read| read.processes)
}

fn refused_row(result: Result<Vec<ProcessGateFacts>, GateReadError>) -> (u64, RowFault) {
    match result {
        Err(GateReadError::Row { row, fault, .. }) => (row, fault),
        other => panic!("expected a refused row, got {other:?}"),
    }
}

/// A sample literal at the export's scale.
fn lit(
    id: i64,
    at_ms: i64,
    level: i64,
    c: f64,
    power: f64,
    rated: f64,
    temp: Option<f64>,
    heaters: (Option<bool>, Option<bool>),
    kind: Option<&str>,
) -> ProjectionChargeSample {
    let mut sample = b_ms(id, at_ms)
        .level(level)
        .c(c)
        .power(power)
        .rated(rated)
        .s();
    sample.charger_voltage = Some(2.0);
    sample.charger_actual_current = Some(0.0);
    sample.outside_temp_c = temp;
    sample.battery_heater_on = heaters.0;
    sample.battery_heater = heaters.1;
    sample.fast_charger_present = Some(true);
    sample.fast_charger_type = kind.map(str::to_owned);
    sample.fast_charger_brand = Some("Tesla".to_owned());
    sample
}

/// The six-row file of C2 T77 (a): two processes of car 1, three rows each, in
/// order, with `t`, `f` and empty fields among them and a type with spaces.
fn six_rows() -> Vec<Row> {
    let mut rows = Vec::new();
    for (k, (c, p, rated)) in [
        ("0.00", "120", "200.00"),
        ("4.00", "100", "220.00"),
        ("8.00", "80", "240.00"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut r = row(11, k as i64 + 1, 2 * k as i64, c, p);
        r[2] = T0.to_string();
        r[3] = (T0 + 4 * MIN).to_string();
        r[6] = (50 + 5 * k).to_string();
        r[12] = rated.to_owned();
        r[COL_TYPE] = " Supercharger ".to_owned();
        match k {
            0 => {}
            1 => r[15] = "t".to_owned(),
            _ => {
                r[13] = String::new();
                r[14] = String::new();
                r[15] = "f".to_owned();
            }
        }
        rows.push(r);
    }
    for k in 0..3_i64 {
        let r: Row = [
            "1".to_owned(),
            "12".to_owned(),
            (T0 + DAY).to_string(),
            (T0 + DAY + 20 * MIN).to_string(),
            (k + 1).to_string(),
            (T0 + DAY + 10 * k * MIN).to_string(),
            (40 + k).to_string(),
            format!("{:.2}", 1.08 * k as f64),
            "7".to_owned(),
            "240".to_owned(),
            "30".to_owned(),
            "1".to_owned(),
            format!("{}.00", 150 + k),
            "8.0".to_owned(),
            "f".to_owned(),
            String::new(),
            "f".to_owned(),
            String::new(),
            String::new(),
        ];
        rows.push(r);
    }
    rows
}

fn six_expected() -> Vec<ProcessGateFacts> {
    let dc = [
        lit(
            1,
            T0,
            50,
            0.00,
            120.0,
            200.00,
            Some(12.5),
            (Some(false), None),
            Some(" Supercharger "),
        ),
        lit(
            2,
            T0 + 2 * MIN,
            55,
            4.00,
            100.0,
            220.00,
            Some(12.5),
            (Some(false), Some(true)),
            Some(" Supercharger "),
        ),
        lit(
            3,
            T0 + 4 * MIN,
            60,
            8.00,
            80.0,
            240.00,
            None,
            (None, Some(false)),
            Some(" Supercharger "),
        ),
    ];
    let ac = (0..3_i64)
        .map(|k| {
            let mut sample = b_ms(k + 1, T0 + DAY + 10 * k * MIN)
                .level(40 + k)
                .c(format!("{:.2}", 1.08 * k as f64).parse().unwrap())
                .a7()
                .rated(150.0 + k as f64)
                .temp(8.0)
                .present(false)
                .s();
            sample.battery_heater_on = Some(false);
            sample
        })
        .collect::<Vec<_>>();
    vec![
        ProcessGateFacts {
            car_id: 1,
            process_id: 11,
            start_ms: T0,
            end_ms: T0 + 4 * MIN,
            facts: fold_all(&dc),
        },
        ProcessGateFacts {
            car_id: 1,
            process_id: 12,
            start_ms: T0 + DAY,
            end_ms: T0 + DAY + 20 * MIN,
            facts: fold_all(&ac),
        },
    ]
}

#[test]
fn c2_t77_reader_roundtrip() {
    // (a) Six rows read into two processes equal to folding the literals.
    let text = csv(HEADER, &six_rows());
    let read_back = read_sessions(text.as_bytes()).expect("a valid export");
    assert_eq!(read_back.rows, 6);
    assert_eq!(read_back.processes, six_expected());
    // The padded type classifies as Supercharger, and is the trimmed session type.
    let facts = &read_back.processes[0].facts;
    assert_eq!(facts.session_type.as_deref(), Some("Supercharger"));
    assert_eq!(facts.dc_name_samples, 3);
    assert_eq!(facts.type_values, vec![("Supercharger".to_owned(), 3)]);
    assert_eq!(facts.is_dc, Some(true));
    // CRLF line endings, and no final line break, read the same.
    let crlf = text.replace('\n', "\r\n");
    assert_eq!(read(&crlf).expect("CRLF"), six_expected());
    assert_eq!(
        read(text.trim_end()).expect("no final break"),
        six_expected()
    );
    // Values are at C1 R36's column scale: 2 decimals for the counter, whole
    // numbers for power, 1 decimal for the temperature.
    let mut scaled = row(1, 1, 0, "0.126", "7.4");
    scaled[13] = "12.34".to_owned();
    let facts = &read(&csv(HEADER, &[scaled]))
        .expect("scaled")
        .remove(0)
        .facts;
    assert_eq!(facts.max_power_kw, Some(7.0));
    assert_eq!(facts.last_counter.expect("counter").counter_kwh, 0.13);
    assert_eq!(facts.outside_temp_mean_c, Some(12.3));
    // cars.csv: `lfp_battery` t, f or unknown.
    let cars = read_cars("car_id,lfp_battery\n1,t\n2,f\n3,\n".as_bytes()).expect("cars");
    assert_eq!(cars, vec![(1, Some(true)), (2, Some(false)), (3, None)]);
    assert_eq!(read_cars("car_id,lfp_battery\n".as_bytes()), Ok(vec![]));
    let (row_number, fault) = match read_cars("car_id,lfp_battery\n1,yes\n".as_bytes()) {
        Err(GateReadError::Row { row, fault, .. }) => (row, fault),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        (row_number, fault),
        (
            1,
            RowFault::Field {
                column: "lfp_battery"
            }
        )
    );
    // An empty file has no header.
    assert!(matches!(read(""), Err(GateReadError::HeaderMissing { .. })));
}

#[test]
fn c2_t77_header_refusal() {
    let columns = HEADER.split(',').collect::<Vec<_>>();
    // Rows that would be refused themselves, to show the header is read first.
    let body = "abc,def\n";
    let refuse = |header: String| match read(&format!("{header}\n{body}")) {
        Err(GateReadError::HeaderMismatch {
            column,
            expected,
            found,
            ..
        }) => (column, expected, found),
        other => panic!("expected a header refusal, got {other:?}"),
    };
    // An extra column `latitude`, anywhere, names the first column that differs.
    let mut extra = columns.clone();
    extra.insert(2, "latitude");
    assert_eq!(
        refuse(extra.join(",")),
        (2, Some("start_ms"), Some("latitude".to_owned()))
    );
    let at_end = format!("{HEADER},latitude");
    assert_eq!(refuse(at_end), (19, None, Some("latitude".to_owned())));
    let mut at_start = columns.clone();
    at_start.insert(0, "latitude");
    assert_eq!(
        refuse(at_start.join(",")),
        (0, Some("car_id"), Some("latitude".to_owned()))
    );
    // A missing `outside_temp`: the next column stands in its place.
    let mut missing = columns.clone();
    missing.remove(13);
    assert_eq!(
        refuse(missing.join(",")),
        (
            13,
            Some("outside_temp"),
            Some("battery_heater_on".to_owned())
        )
    );
    // A missing last column.
    let short = columns[..18].join(",");
    assert_eq!(refuse(short), (18, Some("fast_charger_brand"), None));
    // Two columns swapped.
    let mut swapped = columns.clone();
    swapped.swap(8, 9);
    assert_eq!(
        refuse(swapped.join(",")),
        (8, Some("charger_power"), Some("charger_voltage".to_owned()))
    );
    // A quoted header name is the same text; a quoted empty one is not a name.
    let mut quoted = columns.clone();
    quoted[0] = "\"car_id\"";
    assert!(read(&format!("{}\n", quoted.join(","))).is_ok());
    let mut blank = columns.clone();
    blank[0] = "\"\"";
    assert_eq!(
        refuse(blank.join(",")),
        (0, Some("car_id"), Some(String::new()))
    );
    // An empty name inside the width is a name that differs (an empty one), not
    // an absent column.
    let mut nameless = columns.clone();
    nameless[5] = "";
    assert_eq!(
        refuse(nameless.join(",")),
        (5, Some("date_ms"), Some(String::new()))
    );
    // An extra column with no name is still an extra column (C2 R75 (b) 1:
    // "an extra column included"): a trailing comma gives an empty last header
    // field, unquoted, quoted, or before a CRLF, refused before any row and
    // naming the first column past the expected width, found with an empty name.
    let unnamed = |column: usize| (column, None, Some(String::new()));
    assert_eq!(refuse(format!("{HEADER},")), unnamed(19));
    assert_eq!(refuse(format!("{HEADER},\"\"")), unnamed(19));
    assert_eq!(refuse(format!("{HEADER},\r")), unnamed(19));
    // Under such a header the rows of C2 T77 (a) are not read at all, though
    // each has the expected number of fields.
    let widened = csv(&format!("{HEADER},"), &six_rows());
    assert!(matches!(
        read(&widened),
        Err(GateReadError::HeaderMismatch {
            column: 19,
            expected: None,
            ..
        })
    ));
    // The other two files check their headers the same way.
    assert!(matches!(
        read_cars("car_id,lfp\n1,t\n".as_bytes()),
        Err(GateReadError::HeaderMismatch { column: 1, .. })
    ));
    assert!(matches!(
        read_operators("charging_process_id,operator,extra\n7,tesla\n".as_bytes()),
        Err(GateReadError::HeaderMismatch {
            column: 2,
            expected: None,
            ..
        })
    ));
    // A trailing empty header column of either file: unquoted, quoted, and
    // before a CRLF. Their rows have the expected width, so a header that read
    // as accepted would read them.
    let unnamed_third = |file: &'static str| GateReadError::HeaderMismatch {
        file,
        column: 2,
        expected: None,
        found: Some(String::new()),
    };
    for tail in [",", ",\"\"", ",\r"] {
        let cars = format!("car_id,lfp_battery{tail}\n1,t\n");
        let operators = format!("charging_process_id,operator{tail}\n7,tesla\n");
        assert_eq!(
            read_cars(cars.as_bytes()),
            Err(unnamed_third(CARS_FILE)),
            "cars.csv, header tail {tail:?}"
        );
        assert_eq!(
            read_operators(operators.as_bytes()),
            Err(unnamed_third(OPERATORS_FILE)),
            "operators.csv, header tail {tail:?}"
        );
    }
}

#[test]
fn c2_t77_order_parse_refusal() {
    // (c) A row whose (date_ms, id) is below the previous row's in the same process.
    let rows = [
        row(11, 1, 0, "0.00", "7"),
        row(11, 2, 2, "1.00", "7"),
        row(11, 3, 1, "2.00", "7"),
    ];
    assert_eq!(
        refused_row(read(&csv(HEADER, &rows))),
        (3, RowFault::SampleOrder)
    );
    // The same time with a lower id is below too.
    let mut tie = [row(11, 5, 0, "0.00", "7"), row(11, 4, 0, "1.00", "7")];
    assert_eq!(
        refused_row(read(&csv(HEADER, &tie))),
        (2, RowFault::SampleOrder)
    );
    tie[1][COL_ID] = "6".to_owned();
    assert!(
        read(&csv(HEADER, &tie)).is_ok(),
        "the same time with a higher id is in order"
    );
    // A process that reappears after another.
    let rows = [
        row(11, 1, 0, "0.00", "7"),
        row(12, 1, 0, "0.00", "7"),
        row(11, 2, 1, "1.00", "7"),
    ];
    assert_eq!(
        refused_row(read(&csv(HEADER, &rows))),
        (3, RowFault::ProcessRepeated)
    );
    // The same process id under another car is another process.
    let mut other_car = [row(11, 1, 0, "0.00", "7"), row(11, 1, 0, "0.00", "7")];
    other_car[1][0] = "2".to_owned();
    assert_eq!(read(&csv(HEADER, &other_car)).expect("two cars").len(), 2);
    // A number that does not parse: the row number counts data rows.
    let mut bad = [row(11, 1, 0, "0.00", "7"), row(11, 2, 1, "1.00", "abc")];
    assert_eq!(
        refused_row(read(&csv(HEADER, &bad))),
        (
            2,
            RowFault::Field {
                column: "charger_power"
            }
        )
    );
    for (text, column) in [
        ("1e3", "charger_power"),
        ("NaN", "charger_power"),
        ("+7", "charger_power"),
        ("7.", "charger_power"),
        ("1.5.2", "charger_power"),
        ("\"\"", "charger_power"),
    ] {
        bad[1][COL_POWER] = text.to_owned();
        assert_eq!(
            refused_row(read(&csv(HEADER, &bad))),
            (2, RowFault::Field { column }),
            "{text}"
        );
    }
    // A required key that is empty, a heater that is not t or f, a decimal in
    // an integer column, and a process whose bounds change.
    let mut key = [row(11, 1, 0, "0.00", "7")];
    key[0][COL_DATE] = String::new();
    assert_eq!(
        refused_row(read(&csv(HEADER, &key))),
        (1, RowFault::Field { column: "date_ms" })
    );
    let mut heater = [row(11, 1, 0, "0.00", "7")];
    heater[0][14] = "yes".to_owned();
    assert_eq!(
        refused_row(read(&csv(HEADER, &heater))),
        (
            1,
            RowFault::Field {
                column: "battery_heater_on"
            }
        )
    );
    let mut level = [row(11, 1, 0, "0.00", "7")];
    level[0][6] = "50.5".to_owned();
    assert_eq!(
        refused_row(read(&csv(HEADER, &level))),
        (
            1,
            RowFault::Field {
                column: "battery_level"
            }
        )
    );
    let mut bounds = [row(11, 1, 0, "0.00", "7"), row(11, 2, 1, "1.00", "7")];
    bounds[1][3] = (T0 + 61 * MIN).to_string();
    assert_eq!(
        refused_row(read(&csv(HEADER, &bounds))),
        (2, RowFault::ProcessBounds)
    );
    // A negative value parses (an outside temperature below zero).
    let mut cold = [row(11, 1, 0, "0.00", "7")];
    cold[0][13] = "-3.5".to_owned();
    let facts = read(&csv(HEADER, &cold)).expect("cold").remove(0).facts;
    assert_eq!(facts.outside_temp_mean_c, Some(-3.5));
}

#[test]
fn c2_t77_operator_answers() {
    // (d) The accepted vocabulary is exactly `tesla` and `other`.
    let answers = read_operators("charging_process_id,operator\n7,tesla\n8,other\n".as_bytes());
    assert_eq!(
        answers,
        Ok(vec![(7, SiteOperator::Tesla), (8, SiteOperator::Other)])
    );
    let refused = |text: &str| match read_operators(text.as_bytes()) {
        Err(GateReadError::Row { row, fault, .. }) => (row, fault),
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert_eq!(
        refused("charging_process_id,operator\n7,tesla\n8,other\n9,maybe\n"),
        (3, RowFault::OperatorVocabulary)
    );
    for value in ["Tesla", "OTHER", " tesla", "tesla ", "", "\"\""] {
        assert_eq!(
            refused(&format!("charging_process_id,operator\n7,{value}\n")),
            (1, RowFault::OperatorVocabulary),
            "{value:?}"
        );
    }
    assert_eq!(
        refused("charging_process_id,operator\nx,tesla\n"),
        (
            1,
            RowFault::Field {
                column: "charging_process_id"
            }
        )
    );
    assert_eq!(
        refused("charging_process_id,operator\n7\n"),
        (
            1,
            RowFault::FieldCount {
                expected: 2,
                found: 1
            }
        )
    );
}

#[test]
fn c2_t77_quoted_fields() {
    // (e) In a three-row file: a quoted type, an embedded comma with the later
    // field in its own column, a doubled quote and an empty quoted brand.
    let mut rows = [
        row(11, 1, 0, "0.00", "120"),
        row(11, 2, 2, "4.00", "100"),
        row(11, 3, 4, "8.00", "80"),
    ];
    rows[0][COL_TYPE] = "\"Combo\"".to_owned();
    rows[0][COL_BRAND] = String::new();
    rows[1][COL_TYPE] = "\"a,b\"".to_owned();
    rows[1][COL_BRAND] = "\"Brand, X\"".to_owned();
    rows[2][COL_TYPE] = "\"say \"\"x\"\"\"".to_owned();
    rows[2][COL_BRAND] = "\"\"".to_owned();
    let facts = read(&csv(HEADER, &rows))
        .expect("quoted fields")
        .remove(0)
        .facts;
    assert_eq!(
        facts.type_values,
        vec![
            ("Combo".to_owned(), 1),
            ("a,b".to_owned(), 1),
            ("say \"x\"".to_owned(), 1),
        ]
    );
    assert_eq!(facts.session_type.as_deref(), Some("Combo"));
    assert_eq!(
        facts.dc_name_samples, 1,
        "only the Combo sample is DC-named"
    );
    assert!(
        facts.other_brand,
        "\"Brand, X\" is another operator's brand"
    );
    assert!(!facts.tesla_brand);
    // The same rows built as literals fold to the same facts.
    let literal = |id: i64, minutes: i64, c: f64, p: f64, kind: &str, brand: Option<&str>| {
        let mut sample = lit(
            id,
            T0 + minutes * MIN,
            50,
            c,
            p,
            200.00,
            Some(12.5),
            (Some(false), None),
            Some(kind),
        );
        sample.fast_charger_brand = brand.map(str::to_owned);
        sample
    };
    let expected = fold_all(&[
        literal(1, 0, 0.00, 120.0, "Combo", None),
        literal(2, 2, 4.00, 100.0, "a,b", Some("Brand, X")),
        literal(3, 4, 8.00, 80.0, "say \"x\"", Some("")),
    ]);
    assert_eq!(facts, expected);
    // A quoted empty brand is `Some("")`: no brand. Quoted `Tesla` is Tesla.
    let mut one = [row(11, 1, 0, "0.00", "120")];
    one[0][COL_BRAND] = "\"\"".to_owned();
    let facts = read(&csv(HEADER, &one))
        .expect("empty brand")
        .remove(0)
        .facts;
    assert!(!facts.tesla_brand && !facts.other_brand);
    one[0][COL_BRAND] = "\"Tesla\"".to_owned();
    assert!(
        read(&csv(HEADER, &one))
            .expect("tesla")
            .remove(0)
            .facts
            .tesla_brand
    );
    // A quoted empty number is not a number; an unquoted empty one is absent.
    one[0][7] = "\"\"".to_owned();
    assert_eq!(
        refused_row(read(&csv(HEADER, &one))),
        (
            1,
            RowFault::Field {
                column: "charge_energy_added"
            }
        )
    );
    one[0][7] = String::new();
    assert!(read(&csv(HEADER, &one)).is_ok());
    // A quoted field may hold a line break, of either kind; the row count
    // counts records, not lines.
    let mut multi = [row(11, 1, 0, "0.00", "120"), row(11, 2, 1, "1.00", "120")];
    multi[0][COL_TYPE] = "\"line1\nline2\"".to_owned();
    multi[1][COL_TYPE] = "\"line3\r\nline4\"".to_owned();
    let read_back = read_sessions(csv(HEADER, &multi).as_bytes()).expect("multiline");
    assert_eq!(read_back.rows, 2);
    assert_eq!(
        read_back.processes[0].facts.type_values,
        vec![
            ("line1\nline2".to_owned(), 1),
            ("line3\r\nline4".to_owned(), 1)
        ]
    );
    // One field fewer than the header, and one field more: refused with the row.
    let mut rows = [row(11, 1, 0, "0.00", "7"), row(11, 2, 1, "1.00", "7")];
    let fewer = format!(
        "{HEADER}\n{}\n{}\n",
        rows[0].join(","),
        rows[1][..18].join(",")
    );
    assert_eq!(
        refused_row(read(&fewer)),
        (
            2,
            RowFault::FieldCount {
                expected: 19,
                found: 18
            }
        )
    );
    let more = format!(
        "{HEADER}\n{},extra\n{}\n",
        rows[0].join(","),
        rows[1].join(",")
    );
    assert_eq!(
        refused_row(read(&more)),
        (
            1,
            RowFault::FieldCount {
                expected: 19,
                found: 20
            }
        )
    );
    // A blank line in the middle is a record of one field.
    let blank = format!("{HEADER}\n{}\n\n{}\n", rows[0].join(","), rows[1].join(","));
    assert_eq!(
        refused_row(read(&blank)),
        (
            2,
            RowFault::FieldCount {
                expected: 19,
                found: 1
            }
        )
    );
    // Faults in the text itself: an open quote, text after a closing quote, a
    // quote inside an unquoted field and a bare carriage return.
    rows[1][COL_TYPE] = "\"open".to_owned();
    assert_eq!(
        refused_row(read(&csv(HEADER, &rows))),
        (2, RowFault::Csv(CsvFault::QuoteOpen))
    );
    rows[1][COL_TYPE] = "\"Combo\"x".to_owned();
    assert_eq!(
        refused_row(read(&csv(HEADER, &rows))),
        (2, RowFault::Csv(CsvFault::AfterQuote(b'x')))
    );
    rows[1][COL_TYPE] = "Com\"bo".to_owned();
    assert_eq!(
        refused_row(read(&csv(HEADER, &rows))),
        (2, RowFault::Csv(CsvFault::QuoteInUnquoted))
    );
    rows[1][COL_TYPE] = "Combo\rx".to_owned();
    assert_eq!(
        refused_row(read(&csv(HEADER, &rows))),
        (2, RowFault::Csv(CsvFault::BareCarriageReturn))
    );
    // Text that is not UTF-8.
    let mut bytes = csv(HEADER, &rows[..1]).into_bytes();
    bytes.extend_from_slice(b"1,11,0,0,2,0,,,,,,,,,,,,\xff,\n");
    assert_eq!(
        refused_row(read_sessions(bytes.as_slice()).map(|read| read.processes)),
        (2, RowFault::Csv(CsvFault::NotUtf8))
    );
}
