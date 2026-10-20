// SPDX-License-Identifier: AGPL-3.0-only

// C6 Part I, pure descriptor tests (module `b3_c6_i1`). Synthetic VINs and
// texts only; no store, no clock, no network.

use super::{
    IDENTITY_TEXT_MAX_BYTES, live_config_text, vehicle_model_code, vehicle_model_name,
    vehicle_model_year,
};

const V3: &str = "5YJ3E1EA0KFTEST01";
const VS: &str = "5YJSA1E20HFTEST02";
const VX: &str = "5YJXCAE20LFTEST03";
const VY: &str = "7SAYGDEE0PATEST04";
const VC: &str = "7G2CEHED0RATEST05";
const VR: &str = "5YJRE11B08FTEST06";
const VL: &str = "LRW3E7EK0MCTEST07";
const VB: &str = "XP7YGCEK0SBTEST08";

const HUB_CODES: [&str; 6] = ["S", "X", "3", "Y", "R", "C"];

fn code(vin: Option<&str>, text: Option<&str>) -> Option<String> {
    vehicle_model_code(vin, text)
}

fn code_text(text: &str) -> Option<String> {
    vehicle_model_code(None, Some(text))
}

fn some(value: &str) -> Option<String> {
    Some(value.to_owned())
}

/// Replace the character at 1-based VIN position `position`.
fn with_position(vin: &str, position: usize, replacement: char) -> String {
    vin.chars()
        .enumerate()
        .map(|(index, c)| {
            if index + 1 == position {
                replacement
            } else {
                c
            }
        })
        .collect()
}

#[test]
fn c6_st_i01_to_i10_descriptors() {
    // C6 ST-I1 filed VINs: code, year and name.
    let filed = [
        (V3, "3", 2019, "Model 3"),
        (VS, "S", 2017, "Model S"),
        (VX, "X", 2020, "Model X"),
        (VY, "Y", 2023, "Model Y"),
        (VC, "C", 2024, "Cybertruck"),
        (VR, "R", 2008, "Roadster"),
    ];
    for (vin, expected_code, year, name) in filed {
        assert_eq!(
            code(Some(vin), None),
            some(expected_code),
            "{expected_code}"
        );
        assert_eq!(vehicle_model_year(Some(vin)), Some(year), "{expected_code}");
        assert_eq!(vehicle_model_name(expected_code), Some(name));
    }

    // C6 ST-I2 a filed VIN wins over the text.
    assert_eq!(code(Some(VS), Some("model3")), some("S"));
    assert_eq!(code(Some(V3), Some("lychee")), some("3"));

    // C6 ST-I3 not-filed WMIs: recognised text wins, the VIN is the fallback,
    // and no year is decoded.
    assert_eq!(code(Some(VL), Some("modely")), some("Y"));
    assert_eq!(code(Some(VL), None), some("3"));
    assert_eq!(code(Some(VL), Some("lychee")), some("3"));
    assert_eq!(vehicle_model_year(Some(VL)), None);
    assert_eq!(code(Some(VB), None), some("Y"));
    assert_eq!(vehicle_model_year(Some(VB)), None);

    // C6 ST-I4 text spellings without a VIN.
    let spellings = [
        ("Model 3", Some("3")),
        (" models ", Some("S")),
        ("MODEL_X", Some("X")),
        ("model-y", Some("Y")),
        ("Tesla Model S", Some("S")),
        ("CarTypeModel3", Some("3")),
        ("models2", Some("S")),
        ("modelx2", Some("X")),
        ("roadster", Some("R")),
        ("Cybertruck", Some("C")),
        ("s", Some("S")),
        ("3", Some("3")),
        ("CarTypeUnknown", None),
        ("model31", Some("model31")),
    ];
    for (text, expected) in spellings {
        assert_eq!(code_text(text), expected.map(str::to_owned), "{text:?}");
    }

    // C6 ST-I5 pass-through of unrecognised texts, and the 256-byte bound.
    assert_eq!(code_text("lychee"), some("lychee"));
    assert_eq!(vehicle_model_name("lychee"), None);
    assert_eq!(code_text("  New Thing "), some("New Thing"));
    assert_eq!(code_text("CarTypeSemiTruck"), some("CarTypeSemiTruck"));
    assert_eq!(vehicle_model_name("CarTypeSemiTruck"), None);
    assert_eq!(code_text("Modèle Ω"), some("Modèle Ω"));
    let edge = "é".repeat(128);
    assert_eq!(edge.len(), 256);
    assert_eq!(code_text(&edge), Some(edge.clone()));
    let over = "é".repeat(129);
    assert_eq!(over.len(), 258);
    assert_eq!(code_text(&over), None);
    assert_eq!(code_text(&"a".repeat(257)), None);
    assert_eq!(code_text("   "), None);
    assert_eq!(code(None, None), None);

    // C6 ST-I6 a Tesla VIN with an unknown position 4.
    let unknown_line = "5YJTE1EA0RFTEST10";
    assert_eq!(code(Some(unknown_line), None), None);
    assert_eq!(vehicle_model_year(Some(unknown_line)), Some(2024));
    assert_eq!(code(Some(unknown_line), Some("semi")), some("semi"));

    // C6 ST-I7 invalid and foreign VINs give no VIN facts.
    assert_eq!(code(Some("5YJ3E1EA0KFTEST0"), None), None);
    assert_eq!(vehicle_model_year(Some("5YJ3E1EA0KFTEST0")), None);
    assert_eq!(code(Some("5YJ3E1EA0KFTESTO1"), None), None);
    assert_eq!(vehicle_model_year(Some("5YJ3E1EA0KFTESTO1")), None);
    assert_eq!(code(Some("WVWZZZ1KZ6WTEST11"), None), None);
    assert_eq!(vehicle_model_year(Some("WVWZZZ1KZ6WTEST11")), None);
    assert_eq!(code(Some(" 5yj3e1ea0kftest01 "), None), some("3"));
    assert_eq!(vehicle_model_year(Some(" 5yj3e1ea0kftest01 ")), Some(2019));

    // C6 ST-I8 year codes at position 10; the model stays "3".
    for (year_code, year) in [
        ('A', 2010),
        ('9', 2009),
        ('1', 2031),
        ('7', 2037),
        ('8', 2008),
    ] {
        let vin = with_position(V3, 10, year_code);
        assert_eq!(vehicle_model_year(Some(&vin)), Some(year), "{year_code}");
        assert_eq!(code(Some(&vin), None), some("3"));
    }
    for none_code in ['U', 'Z', '0'] {
        let vin = with_position(V3, 10, none_code);
        assert_eq!(vehicle_model_year(Some(&vin)), None, "{none_code}");
        assert_eq!(code(Some(&vin), None), some("3"));
    }

    // C6 ST-I9 the window: all 30 codes map into 2008..=2037, each to one year.
    let year_codes = "89ABCDEFGHJKLMNPRSTVWXY1234567";
    assert_eq!(year_codes.chars().count(), 30);
    let mut years: Vec<i32> = year_codes
        .chars()
        .map(|c| {
            vehicle_model_year(Some(&with_position(V3, 10, c)))
                .unwrap_or_else(|| panic!("code {c} has no year"))
        })
        .collect();
    assert!(years.iter().all(|year| (2008..=2037).contains(year)));
    years.sort_unstable();
    years.dedup();
    assert_eq!(years.len(), 30);

    // C6 ST-I10 live configuration text.
    let live = [
        (Some("74D"), Some("74D")),
        (Some(" p100d "), Some("p100d")),
        (Some(" Rouge métallisé "), Some("Rouge métallisé")),
        (Some(""), None),
        (Some("  "), None),
        (None, None),
    ];
    for (raw, expected) in live {
        assert_eq!(
            live_config_text(raw),
            expected.map(str::to_owned),
            "{raw:?}"
        );
    }
    assert_eq!(live_config_text(Some(&"a".repeat(257))), None);
    assert_eq!(
        live_config_text(Some(&"é".repeat(128))),
        Some("é".repeat(128))
    );
    assert_eq!(live_config_text(Some(&"é".repeat(129))), None);
}

#[test]
fn c6_st_i02_i03_precedence_matrix_pure() {
    // C6 R6 exhaustively over the six rows: {filed, not filed, none} x text.
    // Filed WMI with a known position 4: always the VIN code.
    for text in [
        None,
        Some("model3"),
        Some("lychee"),
        Some("modely"),
        Some("unknown"),
    ] {
        assert_eq!(code(Some(VS), text), some("S"), "{text:?}");
    }
    // Not-filed WMI: recognised text decides, otherwise the VIN code.
    assert_eq!(code(Some(VB), Some("models")), some("S"));
    assert_eq!(code(Some(VB), Some("Model 3")), some("3"));
    assert_eq!(code(Some(VB), Some("CarTypeUnknown")), some("Y"));
    assert_eq!(code(Some(VB), Some("  ")), some("Y"));
    assert_eq!(code(Some(VB), Some(&"x".repeat(300))), some("Y"));
    assert_eq!(code(Some(VB), Some("model31")), some("Y"));
    // No VIN facts: recognised text, then verbatim text, then nothing.
    assert_eq!(code(Some("not a vin"), Some("modelx")), some("X"));
    assert_eq!(code(Some("not a vin"), Some(" Odd ")), some("Odd"));
    assert_eq!(code(Some("not a vin"), None), None);
    assert_eq!(code(Some(""), Some("")), None);
    // A foreign WMI with a model letter in position 4 is not a Tesla fact.
    assert_eq!(code(Some("WVWSZZ1KZ6WTEST11"), None), None);
    assert_eq!(
        code(Some("WVWSZZ1KZ6WTEST11"), Some("lychee")),
        some("lychee")
    );
}

#[test]
fn c6_st_i07_vin_normalisation_edges() {
    // C6 R2: ASCII whitespace is trimmed, case folds, the alphabet excludes I, O
    // and Q anywhere, the check digit (position 9) is not verified, and the
    // length is exactly 17.
    assert_eq!(code(Some("\t5YJ3E1EA0KFTEST01\n"), None), some("3"));
    assert_eq!(code(Some("5yj3E1ea0kftest01"), None), some("3"));
    for banned in ['I', 'O', 'Q', 'i', 'o', 'q'] {
        for position in [1, 3, 4, 9, 10, 17] {
            let vin = with_position(V3, position, banned);
            assert_eq!(code(Some(&vin), None), None, "{banned} at {position}");
            assert_eq!(vehicle_model_year(Some(&vin)), None);
        }
    }
    for check in "0123456789X".chars() {
        assert_eq!(code(Some(&with_position(V3, 9, check)), None), some("3"));
    }
    // Interior whitespace, non-ASCII look-alikes and Unicode-only whitespace
    // at the edges are not trimmed, so the VIN is invalid.
    assert_eq!(code(Some("5YJ3E1EA0K FTEST01"), None), None);
    assert_eq!(code(Some("\u{a0}5YJ3E1EA0KFTEST01"), None), None);
    assert_eq!(code(Some("5YJ3E1EA0KFTEST01\u{2003}"), None), None);
    assert_eq!(code(Some("5YJ3E1EA0KFTEST01X"), None), None);
    assert_eq!(code(Some("５YJ3E1EA0KFTEST01"), None), None);
    // A character outside the C6 R2 alphabet is refused. The alphabet is ASCII, so
    // counting characters and counting bytes cannot differ for any VIN that is
    // accepted; this is one more refusal case, not a length-unit test.
    assert_eq!(code(Some("5YJ3E1EA0KFTESTé1"), None), None);
}

#[test]
fn c6_st_i04_fold_and_strip_edges() {
    // C6 R5 steps 2-4: the fold deletes spaces, hyphens and underscores only.
    let recognised = [
        ("MODEL S", "S"),
        ("model_3", "3"),
        ("Model-Y", "Y"),
        ("tesla model x", "X"),
        ("CarType Tesla Model S", "S"),
        ("cartypemodely", "Y"),
        ("TESLA_S", "S"),
        ("x1", "X"),
        ("y007", "Y"),
        ("s999", "S"),
        ("R", "R"),
        ("c", "C"),
        ("CyberTruck", "C"),
        ("cartypecybertruck", "C"),
        ("Tesla Roadster", "R"),
        (" \tmodel s\n", "S"),
    ];
    for (text, expected) in recognised {
        assert_eq!(code_text(text), some(expected), "{text:?}");
    }
    // Unrecognised: passed through trimmed, never altered by the fold.
    let passed = [
        "model33",
        "model3x",
        "r2",
        "c2",
        "roadster2",
        "cybertruck1",
        "model",
        "tesla",
        "cartype",
        "Tesla Tesla S",
        "sx",
        "s2x",
        "modelx-2y",
        "Model S Plaid",
        "lychee",
        "3x",
    ];
    for text in passed {
        assert_eq!(code_text(text), some(text), "{text:?}");
        assert_eq!(code_text(&format!("  {text}\t")), some(text), "{text:?}");
    }
    // "Unknown" in any spelling is no text result.
    for text in [
        "unknown",
        "UNKNOWN",
        "CarTypeUnknown",
        "Tesla Unknown",
        "model_unknown",
    ] {
        assert_eq!(code_text(text), None, "{text:?}");
    }
    // The fold lower-cases ASCII letters only (C6 R5 step 2). Under Unicode
    // lower-casing the Kelvin sign U+212A would become `k`, and the two texts
    // below would be recognised.
    assert_eq!(code_text("MODEL Ｓ"), some("MODEL Ｓ"));
    assert_eq!(code_text("ſ"), some("ſ"));
    assert_eq!(code_text("CYBERTRUC\u{212A}"), some("CYBERTRUC\u{212A}"));
    assert_eq!(code_text("UN\u{212A}NOWN"), some("UN\u{212A}NOWN"));
    // The fold deletes the space (U+0020), `-` and `_` only: any other
    // whitespace inside the text stays, so these are not recognised (a fold
    // that deleted every whitespace character would make the first one `S`).
    assert_eq!(code_text("Model\tS"), some("Model\tS"));
    assert_eq!(code_text("Model\u{a0}3"), some("Model\u{a0}3"));
    // Digits after s, x and y are ASCII digits only.
    assert_eq!(code_text("s٣"), some("s٣"));
}

#[test]
fn c6_st_i11_i14_i21_model_vectors_pure() {
    // The model-code rows of the import, live-patch and old-blob cases.
    // Imported car (C6 R13): VIN and cars.model.
    assert_eq!(code(Some(V3), Some("Model 3")), some("3"));
    assert_eq!(code(Some(V3), None), some("3"));
    assert_eq!(code(Some(V3), Some("")), some("3"));
    assert_eq!(code(None, Some("lychee")), some("lychee"));
    assert_eq!(vehicle_model_name("3"), Some("Model 3"));
    assert_eq!(vehicle_model_year(Some(V3)), Some(2019));
    assert_eq!(code(None, None), None);
    // Live parse (C6 R14): P0 car_type "model3" with VIN V3.
    assert_eq!(code(Some(V3), Some("model3")), some("3"));
    // Identity fill (C6 R15): the patch model "Y" under the stored VIN V3 still
    // reads "3", and a derived "Model 3" from an old blob lands as the code.
    assert_eq!(code(Some(V3), Some("Y")), some("3"));
    assert_eq!(code(Some(V3), Some("model3")), some("3"));
    assert_eq!(code(Some(V3), Some("Model 3")), some("3"));
    // Live descriptor texts (C6 R11): trimmed, case kept, no vocabulary.
    assert_eq!(live_config_text(Some(" 74d ")), some("74d"));
    assert_eq!(
        live_config_text(Some("MidnightSilver")),
        some("MidnightSilver")
    );
    assert_eq!(live_config_text(Some("Pinwheel18")), some("Pinwheel18"));
    assert_eq!(live_config_text(Some("None")), some("None"));
    // C6 ST-I18 a refreshed model keeps code and name; the year differs.
    let refreshed = with_position(V3, 10, 'P');
    assert_eq!(refreshed, "5YJ3E1EA0PFTEST01");
    assert_eq!(code(Some(&refreshed), None), some("3"));
    assert_eq!(vehicle_model_name("3"), Some("Model 3"));
    assert_eq!(vehicle_model_year(Some(&refreshed)), Some(2023));
    assert_eq!(live_config_text(Some("74d")), some("74d"));
    assert_eq!(live_config_text(Some("74")), some("74"));
}

#[test]
fn c6_st_i22_code_fixed_points_pure() {
    // C6 ST-I22: each Hub code maps to itself; a filed VIN still decides.
    for hub_code in HUB_CODES {
        assert_eq!(code(None, Some(hub_code)), some(hub_code));
        assert!(vehicle_model_name(hub_code).is_some());
    }
    assert_eq!(code(Some(V3), Some("R")), some("3"));

    // Idempotence over a deterministic corpus: whatever the function returns,
    // re-reading it as the model text gives the same value (C6 R7, R14-R16,
    // C6 R57), with and without the VIN, and the call is repeatable (C6 R1).
    let vins: Vec<Option<String>> = [
        None,
        Some(V3),
        Some(VS),
        Some(VX),
        Some(VY),
        Some(VC),
        Some(VR),
        Some(VL),
        Some(VB),
        Some("5YJTE1EA0RFTEST10"),
        Some("WVWZZZ1KZ6WTEST11"),
        Some("short"),
    ]
    .into_iter()
    .map(|vin| vin.map(str::to_owned))
    .collect();
    let mut texts: Vec<String> = [
        "",
        " ",
        "s",
        "S",
        "x",
        "X",
        "y",
        "Y",
        "3",
        "r",
        "R",
        "c",
        "C",
        "model3",
        "Model S",
        "models2",
        "model31",
        "lychee",
        " New Thing ",
        "CarTypeSemiTruck",
        "CarTypeUnknown",
        "unknown",
        "Modèle Ω",
        "roadster",
        "cybertruck",
        "tesla",
        "model",
        "cartype",
        "r2",
        "3x",
        "model33",
        "x1",
        "\t\n",
        "ſ",
    ]
    .iter()
    .map(|text| (*text).to_owned())
    .collect();
    // A bounded deterministic pseudo-random corpus over a small alphabet that
    // exercises folding, stripping, digits and separators.
    let alphabet: Vec<&str> = vec![
        "model",
        "tesla",
        "cartype",
        "s",
        "x",
        "y",
        "3",
        "r",
        "c",
        "1",
        "9",
        " ",
        "-",
        "_",
        "unknown",
        "roadster",
        "cybertruck",
        "é",
        "\t",
    ];
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..600 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let pieces = 1 + (state >> 60) as usize % 4;
        let mut text = String::new();
        for _ in 0..pieces {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            text.push_str(alphabet[(state >> 33) as usize % alphabet.len()]);
        }
        texts.push(text);
    }
    for vin in &vins {
        for text in &texts {
            let first = code(vin.as_deref(), Some(text));
            assert_eq!(
                first,
                code(vin.as_deref(), Some(text)),
                "pure {vin:?} {text:?}"
            );
            if let Some(value) = &first {
                // Without a VIN the output is a fixed point.
                assert_eq!(
                    code(None, Some(value)),
                    first,
                    "fixed point {vin:?} {text:?}"
                );
                // With the same VIN it is unchanged too.
                assert_eq!(
                    code(vin.as_deref(), Some(value)),
                    first,
                    "stable {vin:?} {text:?}"
                );
                // The output is a Hub code or the trimmed input, never new text.
                assert!(
                    HUB_CODES.contains(&value.as_str()) || value == text.trim(),
                    "{vin:?} {text:?} -> {value:?}"
                );
                // Model names exist exactly for the Hub codes.
                assert_eq!(
                    vehicle_model_name(value).is_some(),
                    HUB_CODES.contains(&value.as_str())
                );
            }
        }
    }
}

#[test]
fn c6_st_i05_bound_applies_to_the_trimmed_text() {
    // C6 R5 step 1 and C6 R11: the byte bound is checked after trimming, in bytes.
    let padded = format!("   {}   ", "a".repeat(IDENTITY_TEXT_MAX_BYTES));
    assert_eq!(
        code_text(&padded),
        Some("a".repeat(IDENTITY_TEXT_MAX_BYTES))
    );
    assert_eq!(
        live_config_text(Some(&padded)),
        Some("a".repeat(IDENTITY_TEXT_MAX_BYTES))
    );
    let over = format!("   {}   ", "a".repeat(IDENTITY_TEXT_MAX_BYTES + 1));
    assert_eq!(code_text(&over), None);
    assert_eq!(live_config_text(Some(&over)), None);
    // An over-long text does not hide a not-filed VIN's code, and never
    // overrides a filed one.
    assert_eq!(code(Some(VB), Some(&over)), some("Y"));
    assert_eq!(code(Some(V3), Some(&over)), some("3"));
    // The bound is the specified constant.
    assert_eq!(IDENTITY_TEXT_MAX_BYTES, 256);
}
