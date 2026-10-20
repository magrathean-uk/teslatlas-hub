// SPDX-License-Identifier: AGPL-3.0-only

// Pure vehicle descriptor (C6 Part I, R1-R12, R20).
//
// Four public functions in the `hub_pack` namespace, all pure functions of
// their arguments: no clock, environment, storage, randomness or logging, and
// no failure. A VIN is personal data, so nothing here ever reports one (C6 R19).
//
// This file is pulled into `hub_pack.rs` by `include!`, like its siblings, so
// it carries plain `//` and `///` comments only and no inner attributes.

/// Longest descriptor text, in UTF-8 bytes, that is accepted from a live
/// source or from a model text (C6 `K_CODE_MAX`, C6 R5 step 1, C6 R11). Lifecycle
/// blobs carry these values, so the bound protects their size budget.
pub(crate) const IDENTITY_TEXT_MAX_BYTES: usize = 256;

/// A VIN has exactly this many characters (49 CFR 565.15, ISO 3779).
const VIN_LENGTH: usize = 17;

/// Tesla WMIs whose position-4 and position-10 decodings are published by the
/// regulator from Tesla's own filing (C6 R3, "filed").
const FILED_TESLA_WMIS: [&str; 4] = ["5YJ", "7SA", "7G2", "SFZ"];

/// Tesla WMIs for which no public filing confirms the decodings (C6 R3, "not
/// filed"): only the model letter is used, and only when the car's own text
/// does not state a recognised model (C6 R6).
const UNFILED_TESLA_WMIS: [&str; 2] = ["LRW", "XP7"];

/// The facts a valid Tesla VIN yields (C6 R2-R4). A VIN that is invalid or not
/// Tesla's yields no `VinFacts` at all.
struct VinFacts {
    /// The normalised VIN: trimmed of ASCII whitespace, ASCII upper case.
    vin: String,
    /// Whether the WMI is a filed one (C6 R3).
    filed: bool,
    /// The Hub model code from position 4, when that character is one of the
    /// six model-line characters (C6 R4).
    model_code: Option<&'static str>,
}

/// How a model text reads under C6 R5.
enum ModelText {
    /// Absent, empty after trimming, too long, or Tesla's "unknown" value:
    /// the text states nothing (C6 R5 steps 1 and 4).
    Absent,
    /// One of the six Hub codes after folding and stripping (C6 R5 step 4).
    Recognised(&'static str),
    /// Anything else; the trimmed text passes through verbatim (C6 R7).
    Unrecognised(String),
}

/// C6 R2 normalisation and validity: trim ASCII whitespace, ASCII-uppercase, and
/// require exactly 17 characters from `0-9`, `A-H`, `J-N`, `P`, `R-Z`. The
/// check digit is not verified. Anything else gives `None`, never an error.
fn normalised_vin(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches(|c: char| c.is_ascii_whitespace());
    let upper = trimmed.to_ascii_uppercase();
    let mut count = 0usize;
    for character in upper.chars() {
        count += 1;
        let allowed = matches!(
            character,
            '0'..='9' | 'A'..='H' | 'J'..='N' | 'P' | 'R'..='Z'
        );
        if !allowed || count > VIN_LENGTH {
            return None;
        }
    }
    (count == VIN_LENGTH).then_some(upper)
}

/// C6 R2-R4: the facts of a valid VIN with a Tesla WMI, else `None`.
fn tesla_vin_facts(raw: Option<&str>) -> Option<VinFacts> {
    let vin = normalised_vin(raw?)?;
    // A valid VIN is 17 ASCII characters, so byte slicing is exact.
    let wmi = &vin[..3];
    let filed = if FILED_TESLA_WMIS.contains(&wmi) {
        true
    } else if UNFILED_TESLA_WMIS.contains(&wmi) {
        false
    } else {
        return None;
    };
    let model_code = match vin.as_bytes()[3] {
        b'S' => Some("S"),
        b'X' => Some("X"),
        b'3' => Some("3"),
        b'Y' => Some("Y"),
        b'R' => Some("R"),
        b'C' => Some("C"),
        _ => None,
    };
    Some(VinFacts {
        vin,
        filed,
        model_code,
    })
}

/// C6 R5: read a model text. Trim (Unicode White_Space); empty or longer than
/// the byte bound gives no result; fold (ASCII lower case, delete spaces, `-`
/// and `_`); strip a leading `cartype`, then `tesla`, then `model`, each only
/// when something remains after it; then match the remainder.
fn read_model_text(raw: Option<&str>) -> ModelText {
    let Some(raw) = raw else {
        return ModelText::Absent;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > IDENTITY_TEXT_MAX_BYTES {
        return ModelText::Absent;
    }
    let folded: String = trimmed
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let mut rest = folded.as_str();
    for prefix in ["cartype", "tesla", "model"] {
        if let Some(stripped) = rest.strip_prefix(prefix)
            && !stripped.is_empty()
        {
            rest = stripped;
        }
    }
    match recognised_remainder(rest) {
        RemainderMatch::Code(code) => ModelText::Recognised(code),
        RemainderMatch::Unknown => ModelText::Absent,
        RemainderMatch::Other => ModelText::Unrecognised(trimmed.to_owned()),
    }
}

/// Result of matching the folded and stripped remainder of a model text.
enum RemainderMatch {
    Code(&'static str),
    /// Tesla's own "unknown" value: no text result (C6 R5 step 4).
    Unknown,
    Other,
}

/// C6 R5 step 4. `s`, `x` or `y`, each optionally followed by one or more ASCII
/// digits, give `S`, `X`, `Y`; exactly `3` gives `3` (a digit suffix after
/// `3` is ambiguous and stays unrecognised); `roadster` or `r` give `R`;
/// `cybertruck` or `c` give `C`; `unknown` is no result.
fn recognised_remainder(rest: &str) -> RemainderMatch {
    match rest {
        "3" => return RemainderMatch::Code("3"),
        "roadster" | "r" => return RemainderMatch::Code("R"),
        "cybertruck" | "c" => return RemainderMatch::Code("C"),
        "unknown" => return RemainderMatch::Unknown,
        _ => {}
    }
    let mut characters = rest.chars();
    let code = match characters.next() {
        Some('s') => "S",
        Some('x') => "X",
        Some('y') => "Y",
        _ => return RemainderMatch::Other,
    };
    if characters.all(|c| c.is_ascii_digit()) {
        RemainderMatch::Code(code)
    } else {
        RemainderMatch::Other
    }
}

/// The Hub model code for a vehicle (C6 R2-R7).
///
/// `vin` is the vehicle's VIN as stated by its source and `model_text` the
/// source's own model text (a live `car_type`, an imported `cars.model`, a
/// stored model or a lifecycle patch's model). The result is one of the six
/// Hub codes `S`, `X`, `3`, `Y`, `R`, `C`, or an unrecognised text passed
/// through trimmed and verbatim (C6 R7), or `None` when neither input states a
/// model. A filed VIN decoding decides over any text; for a not-filed Tesla
/// WMI a recognised text decides and the VIN is the fallback (C6 R6). The
/// function never fails and never logs.
pub fn vehicle_model_code(vin: Option<&str>, model_text: Option<&str>) -> Option<String> {
    let facts = tesla_vin_facts(vin);
    let text = read_model_text(model_text);
    // A VIN code exists only for a Tesla WMI with a known position 4.
    let vin_code = facts
        .as_ref()
        .and_then(|facts| facts.model_code.map(|code| (code, facts.filed)));
    match (vin_code, text) {
        // A filed decoding is the regulator-published identity and wins.
        (Some((code, true)), _) => Some(code.to_owned()),
        // Not filed: the car's own recognised statement wins, else the VIN.
        (_, ModelText::Recognised(code)) => Some(code.to_owned()),
        (Some((code, false)), _) => Some(code.to_owned()),
        (None, ModelText::Unrecognised(text)) => Some(text),
        (None, ModelText::Absent) => None,
    }
}

/// The model year of a valid VIN with a filed Tesla WMI, from position 10
/// (C6 R8). Not-filed WMIs, non-Tesla WMIs, invalid VINs and the codes `0`, `U`,
/// `Z` (and any other) give `None`. The window 2008 to 2037 maps every code to
/// exactly one year without a clock.
pub fn vehicle_model_year(vin: Option<&str>) -> Option<i32> {
    let facts = tesla_vin_facts(vin)?;
    if !facts.filed {
        return None;
    }
    let year = match facts.vin.as_bytes()[9] {
        b'8' => 2008,
        b'9' => 2009,
        b'A' => 2010,
        b'B' => 2011,
        b'C' => 2012,
        b'D' => 2013,
        b'E' => 2014,
        b'F' => 2015,
        b'G' => 2016,
        b'H' => 2017,
        b'J' => 2018,
        b'K' => 2019,
        b'L' => 2020,
        b'M' => 2021,
        b'N' => 2022,
        b'P' => 2023,
        b'R' => 2024,
        b'S' => 2025,
        b'T' => 2026,
        b'V' => 2027,
        b'W' => 2028,
        b'X' => 2029,
        b'Y' => 2030,
        b'1' => 2031,
        b'2' => 2032,
        b'3' => 2033,
        b'4' => 2034,
        b'5' => 2035,
        b'6' => 2036,
        b'7' => 2037,
        _ => return None,
    };
    Some(year)
}

/// The Hub-owned display name of a model code (C6 R9). Any other code, including
/// an unrecognised pass-through text, gives `None`.
pub fn vehicle_model_name(model_code: &str) -> Option<&'static str> {
    match model_code {
        "S" => Some("Model S"),
        "X" => Some("Model X"),
        "3" => Some("Model 3"),
        "Y" => Some("Model Y"),
        "R" => Some("Roadster"),
        "C" => Some("Cybertruck"),
        _ => None,
    }
}

/// A live configuration text such as a trim badge, exterior colour, wheel or
/// spoiler code (C6 R11): trimmed (Unicode White_Space), case unchanged, `None`
/// when absent, empty after trimming or longer than the byte bound. No
/// vocabulary is applied: Tesla's raw codes pass through. A source value that
/// is not a JSON string is the caller's `None`.
pub fn live_config_text(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() || trimmed.len() > IDENTITY_TEXT_MAX_BYTES {
        return None;
    }
    Some(trimmed.to_owned())
}
