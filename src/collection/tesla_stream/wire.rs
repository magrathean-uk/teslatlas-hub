// SPDX-License-Identifier: AGPL-3.0-only

//! Pure wire model of the legacy stream (C4 P1): the subscribed column set, the
//! telemetry record, the subscribe and unsubscribe frames, the override
//! endpoint check and the parser of one `data:update` frame. Nothing here reads
//! a clock, a socket or storage; the same text always gives the same result
//! (R53).

use serde::Serialize;
use serde_json::Value;
use url::Url;
use zeroize::Zeroizing;

use super::StreamError;

/// The subscribed columns, in the order the server returns their values (R15).
///
/// Each column is read by the Hub's data model (C1 R37, C3 R36, C6 R27) and no
/// other column is requested: a column nobody reads costs bandwidth on every
/// frame and widens what a misplaced value could corrupt. `est_heading` is not
/// requested because no Hub reader uses it. The order is alphabetical: the
/// server answers by position, so any fixed order works, and alphabetical is
/// Hub-owned and checkable without reference to any other product.
pub const STREAM_COLUMNS: [&str; 11] = [
    "elevation",
    "est_lat",
    "est_lng",
    "est_range",
    "heading",
    "odometer",
    "power",
    "range",
    "shift_state",
    "soc",
    "speed",
];

/// Number of comma-separated elements in a `data:update` `value`: the sample
/// time, then one value per [`STREAM_COLUMNS`] entry (R19). A different count
/// means the server's column list differs from the request, so any assignment
/// would be a guess that could put an odometer reading into the speed field.
const VALUE_ELEMENTS: usize = 1 + STREAM_COLUMNS.len();

/// Longest decimal text of one numeric element, in bytes (R21). A real reading
/// needs a dozen characters; 32 bounds memory and the work per frame.
const NUMBER_TEXT_MAX: usize = 32;

/// Longest `shift_state` text, in bytes (R24). Gear tokens are short ASCII.
const SHIFT_TEXT_MAX: usize = 32;

/// Most digits in a sample time (R20): the largest `i64` has 19.
const TIME_DIGITS_MAX: usize = 19;

/// Most digits in a tag (R8): the largest `u64` has 20 decimal digits, and the
/// tag is the stream vehicle id's decimal text.
const TAG_DIGITS_MAX: usize = 20;

/// Largest access token the subscribe frame accepts, in bytes (R8). A token in
/// JSON Web Token form (RFC 7519) with a handful of claims is a few KiB at
/// most, so 8,192 bytes bounds the outbound frame with headroom.
const TOKEN_BYTES_MAX: usize = 8_192;

/// Longest endpoint override, in bytes (R3): ample room for any host plus path,
/// and a bound on a configuration value.
const ENDPOINT_BYTES_MAX: usize = 2_048;

/// The telemetry record of one accepted frame (R48). Units are those the
/// server delivers (mph, miles, degrees, kW, percent); C1 converts. `None`
/// means the car did not report the value in this sample, or the element was
/// invalid (R21 to R24); it is never zero and never "unchanged" (R22).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamUpdate {
    /// The session's stream vehicle id text, as carried by the frame.
    pub tag: String,
    /// Sample time, epoch milliseconds, from element 0 of `value` (R19, R20).
    pub timestamp_ms: i64,
    /// Metres (C1 R38).
    pub elevation: Option<f64>,
    /// Not requested (R15), so always `None`; kept so stored rows keep their
    /// `drive_state.est_heading` key.
    pub est_heading: Option<i64>,
    /// Degrees.
    pub est_lat: Option<f64>,
    /// Degrees.
    pub est_lng: Option<f64>,
    /// Miles.
    pub est_range: Option<f64>,
    /// Degrees.
    pub heading: Option<f64>,
    /// Miles.
    pub odometer: Option<f64>,
    /// Kilowatts, signed. Whole kilowatts only: the C3 seams take an integer
    /// (R16), so a fraction is surfaced as invalid.
    pub power: Option<i64>,
    /// Miles.
    pub range: Option<f64>,
    /// Gear text as delivered; C1 R40a normalises it.
    pub shift_state: Option<String>,
    /// Percent.
    pub soc: Option<f64>,
    /// Miles per hour.
    pub speed: Option<f64>,
}

/// The `value` member of the subscribe frame: [`STREAM_COLUMNS`] joined with a
/// comma and no spaces (R8).
pub(crate) fn subscribe_value() -> String {
    STREAM_COLUMNS.join(",")
}

#[derive(Serialize)]
struct SubscribeWire<'a> {
    msg_type: &'static str,
    token: &'a str,
    value: &'a str,
    tag: &'a str,
}

#[derive(Serialize)]
struct UnsubscribeWire<'a> {
    msg_type: &'static str,
    tag: &'a str,
}

/// Checks the tag grammar of R8: 1 to 20 ASCII digits.
pub(crate) fn validate_tag(tag: &str) -> Result<(), StreamError> {
    if tag.is_empty() || tag.len() > TAG_DIGITS_MAX || !tag.bytes().all(|b| b.is_ascii_digit()) {
        return Err(StreamError::InvalidTag);
    }
    Ok(())
}

/// Builds the subscribe frame (R8): exactly the members `msg_type`, `token`,
/// `value` and `tag`. The tag must be 1 to 20 ASCII digits; the token must be
/// non-empty, at most 8,192 bytes and free of ASCII control characters (a
/// control character cannot occur in a token, so its presence means a
/// corrupted credential). No error value mentions the token or the frame.
///
/// The text is returned as an ordinary `String` because the signature is
/// public; the supervisor builds its frame with [`subscribe_frame_zeroizing`].
pub fn subscribe_frame(tag: &str, token: &str) -> Result<String, StreamError> {
    let mut frame = subscribe_frame_zeroizing(tag, token)?;
    // Moves the buffer out; nothing is copied, and the emptied wrapper wipes
    // nothing of value.
    String::from_utf8(std::mem::take(&mut *frame)).map_err(|_| StreamError::FrameSerialization)
}

/// The subscribe frame of [`subscribe_frame`] as bytes in a buffer that is
/// wiped when it is dropped, and that never reallocates (R6 step 6). The frame
/// holds the access token, and a growing buffer leaves its earlier, smaller
/// allocations behind unwiped, each holding part of the token. So the length is
/// measured first by serialising into a counter that keeps nothing, and the
/// buffer is then allocated once at exactly that length.
pub(crate) fn subscribe_frame_zeroizing(
    tag: &str,
    token: &str,
) -> Result<Zeroizing<Vec<u8>>, StreamError> {
    validate_tag(tag)?;
    if token.is_empty()
        || token.len() > TOKEN_BYTES_MAX
        || token.bytes().any(|b| b.is_ascii_control())
    {
        return Err(StreamError::InvalidToken);
    }
    let value = subscribe_value();
    let wire = SubscribeWire {
        msg_type: "data:subscribe_oauth",
        token,
        value: &value,
        tag,
    };
    let mut counter = ByteCounter(0);
    serde_json::to_writer(&mut counter, &wire).map_err(|_| StreamError::FrameSerialization)?;
    let mut frame = Zeroizing::new(Vec::with_capacity(counter.0));
    serde_json::to_writer(&mut *frame, &wire).map_err(|_| StreamError::FrameSerialization)?;
    Ok(frame)
}

/// A writer that only counts the bytes it is given.
struct ByteCounter(usize);

impl std::io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Builds the unsubscribe frame (R9): exactly `msg_type` and `tag`.
pub fn unsubscribe_frame(tag: &str) -> Result<String, StreamError> {
    validate_tag(tag)?;
    serde_json::to_string(&UnsubscribeWire {
        msg_type: "data:unsubscribe",
        tag,
    })
    .map_err(|_| StreamError::FrameSerialization)
}

/// Accepts an endpoint override only when it is an absolute `wss` URI with a
/// non-empty host, no userinfo, no fragment and at most 2,048 bytes; port, path
/// and query are allowed (R3).
///
/// `wss` because the subscribe frame carries the access token and RFC 6455
/// section 11.1.2 defines `wss` as WebSocket over TLS; no userinfo because
/// credentials in a URL end up in logs; no fragment because RFC 6455 section 3
/// forbids fragments in WebSocket URIs; path and query because a provider
/// stream URL may carry its own path or key (R59).
pub fn validate_endpoint_override(value: &str) -> Result<(), StreamError> {
    validate_endpoint(value, false)
}

/// The rule of the test-only constructors (IF c1a): R3, and additionally
/// `ws://` with host `127.0.0.1`, `::1` or `localhost`.
#[cfg(test)]
pub(crate) fn validate_endpoint_for_test_constructor(value: &str) -> Result<(), StreamError> {
    validate_endpoint(value, true)
}

fn validate_endpoint(value: &str, allow_test_loopback: bool) -> Result<(), StreamError> {
    let invalid = || StreamError::InvalidEndpoint;
    if value.is_empty() || value.len() > ENDPOINT_BYTES_MAX {
        return Err(invalid());
    }
    // Whitespace, control characters and backslashes are stripped or rewritten
    // by URL parsers, which would let a malformed value pass as a valid one.
    if value
        .bytes()
        .any(|b| b <= 0x20 || b == 0x7f || b == b'\\' || b == b'#')
    {
        return Err(invalid());
    }
    let (scheme, rest) = value.split_once("://").ok_or_else(invalid)?;
    let plaintext = scheme.eq_ignore_ascii_case("ws");
    if !(scheme.eq_ignore_ascii_case("wss") || (allow_test_loopback && plaintext)) {
        return Err(invalid());
    }
    // The URL parser reads `wss:///path` as host `path`, so the authority is
    // checked on the text: it must exist and carry no userinfo.
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(invalid());
    }
    let url = Url::parse(value).map_err(|_| invalid())?;
    if url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    if plaintext && !matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost")) {
        return Err(invalid());
    }
    Ok(())
}

/// Trims ASCII whitespace only: the stream's text is ASCII, and Unicode
/// whitespace in a number or a time is a corrupted element.
fn trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_ascii_whitespace())
}

/// Reads the sample time of element 0 (R20): 1 to 19 ASCII digits whose value
/// fits `i64` and is greater than 0. The unit is epoch milliseconds. A sample
/// without a time cannot be ordered or stored.
fn parse_sample_time(element: &str) -> Result<i64, StreamError> {
    let text = trim(element);
    if text.is_empty() || text.len() > TIME_DIGITS_MAX || !text.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(StreamError::InvalidTimestamp);
    }
    match text.parse::<i64>() {
        Ok(time) if time > 0 => Ok(time),
        _ => Err(StreamError::InvalidTimestamp),
    }
}

/// One numeric or text element after trimming: `Ok(None)` when it is empty
/// ("the car did not report this value", R22), `Err(())` when it is invalid.
type Cell<T> = Result<Option<T>, ()>;

/// Turns an invalid element into `None` and counts it.
fn settle<T>(cell: Cell<T>, invalid: &mut u32) -> Option<T> {
    cell.unwrap_or_else(|()| {
        *invalid = invalid.saturating_add(1);
        None
    })
}

/// Splits a decimal text `-?[0-9]+(\.[0-9]+)?` of at most 32 bytes into its
/// integer part (with sign) and fraction digits (R21). A general float parser
/// would also accept `NaN`, `inf`, `1e3` and `+5`, none of which is valid
/// telemetry.
fn decimal_parts(text: &str) -> Option<(&str, Option<&str>)> {
    if text.len() > NUMBER_TEXT_MAX {
        return None;
    }
    let (integer, fraction) = match text.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (text, None),
    };
    let digits = integer.strip_prefix('-').unwrap_or(integer);
    let all_digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(digits) || fraction.is_some_and(|fraction| !all_digits(fraction)) {
        return None;
    }
    Some((integer, fraction))
}

/// An `f64` column (R21): the nearest `f64` of the decimal text.
fn parse_float(element: &str) -> Cell<f64> {
    let text = trim(element);
    if text.is_empty() {
        return Ok(None);
    }
    decimal_parts(text).ok_or(())?;
    text.parse::<f64>().map(Some).map_err(|_| ())
}

/// The `i64` column `power` (R21): any fraction must be all zeros (`15.0` is
/// 15) and the value must fit `i64`. A fraction is surfaced as invalid rather
/// than hidden by rounding.
fn parse_whole(element: &str) -> Cell<i64> {
    let text = trim(element);
    if text.is_empty() {
        return Ok(None);
    }
    let (integer, fraction) = decimal_parts(text).ok_or(())?;
    if fraction.is_some_and(|fraction| fraction.bytes().any(|b| b != b'0')) {
        return Err(());
    }
    integer.parse::<i64>().map(Some).map_err(|_| ())
}

/// The `shift_state` column (R24): the trimmed text when it is at most 32
/// bytes of printable ASCII. Case and any prefix are kept as delivered, since
/// C1 R40a normalises and normalising twice would split the rule.
fn parse_shift(element: &str) -> Cell<String> {
    let text = trim(element);
    if text.is_empty() {
        return Ok(None);
    }
    if text.len() > SHIFT_TEXT_MAX || !text.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Err(());
    }
    Ok(Some(text.to_owned()))
}

/// Parses one `data:update` frame (R17 to R24).
///
/// `WrongMessageType` when `msg_type` is another string; `MalformedDataUpdate`
/// when the text is not a JSON object with a string `msg_type`, when `tag` or
/// `value` is missing or not a string, or when `value` does not split into
/// exactly 12 elements; `InvalidTimestamp` for a bad element 0. A bad value
/// element gives `None` for that field and keeps the rest of the frame. An
/// envelope `timestamp` member and any other member are ignored (R19): the
/// sample time is element 0, stated with the sample's own values.
pub fn parse_data_update(frame: &str) -> Result<StreamUpdate, StreamError> {
    parse_data_update_counted(frame).map(|(update, _invalid)| update)
}

/// [`parse_data_update`] that also returns how many value elements were
/// invalid, for the per-run `fields_invalid` counter (R21).
pub(crate) fn parse_data_update_counted(frame: &str) -> Result<(StreamUpdate, u32), StreamError> {
    let malformed = || StreamError::MalformedDataUpdate;
    let Ok(Value::Object(members)) = serde_json::from_str::<Value>(frame) else {
        return Err(malformed());
    };
    let Some(Value::String(msg_type)) = members.get("msg_type") else {
        return Err(malformed());
    };
    if msg_type != "data:update" {
        return Err(StreamError::WrongMessageType);
    }
    let (Some(Value::String(tag)), Some(Value::String(value))) =
        (members.get("tag"), members.get("value"))
    else {
        return Err(malformed());
    };
    let elements: Vec<&str> = value.split(',').collect();
    if elements.len() != VALUE_ELEMENTS {
        return Err(malformed());
    }
    let timestamp_ms = parse_sample_time(elements[0])?;
    let mut invalid = 0_u32;
    let update = StreamUpdate {
        tag: tag.clone(),
        timestamp_ms,
        elevation: settle(parse_float(elements[1]), &mut invalid),
        est_heading: None,
        est_lat: settle(parse_float(elements[2]), &mut invalid),
        est_lng: settle(parse_float(elements[3]), &mut invalid),
        est_range: settle(parse_float(elements[4]), &mut invalid),
        heading: settle(parse_float(elements[5]), &mut invalid),
        odometer: settle(parse_float(elements[6]), &mut invalid),
        power: settle(parse_whole(elements[7]), &mut invalid),
        range: settle(parse_float(elements[8]), &mut invalid),
        shift_state: settle(parse_shift(elements[9]), &mut invalid),
        soc: settle(parse_float(elements[10]), &mut invalid),
        speed: settle(parse_float(elements[11]), &mut invalid),
    };
    Ok((update, invalid))
}
