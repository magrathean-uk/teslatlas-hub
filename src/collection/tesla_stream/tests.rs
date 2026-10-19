// SPDX-License-Identifier: AGPL-3.0-only

// Spec tests of C4 part P1 (wire model and pure policy): SC1 to SC5, SC22 to SC30, SC37,
// SC39, SC41, SC42, SC46, SC48, SC49, SC65, SC67 to SC70, SC72 and SC78, plus tests of
// the rules the part owns (R3, R8, R21, R33, R35, R37, R42, R50). Synthetic data only; no
// test touches the network.

use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{Value, json};
use url::Url;

use crate::db::StreamSessionTerminalOutcome;

use super::policy::*;
use super::wire::*;
use super::*;

const TAG: &str = "4242";
/// Spec notation T0: the sample time of every frame below, epoch milliseconds.
const T0: i64 = 1_760_000_000_000;
/// Spec notation W: the wall clock the policy tests read.
const W: i64 = T0 + 5_000;

/// Row V1 of the spec: elevation 25, est_lat 10.5, est_lng 20.25, est_range 230.5,
/// heading 182, odometer 12345.6, power 15, range 241.25, shift_state D, soc 80, speed 30.
const V1: &str = "25,10.5,20.25,230.5,182,12345.6,15,241.25,D,80,30";
/// Row PK of the spec: a parked frame (power 0, gear P, speed 0).
const PK: &str = "25,10.5,20.25,230.5,182,12345.6,0,241.25,P,80,0";
/// Row E of the spec: eleven empty values.
const E: &str = ",,,,,,,,,,";

/// `U(t, row)` of the spec: one `data:update` frame for the session's tag. The JSON is
/// built, not formatted, so a control character in a value is escaped as the wire would.
fn u(time: impl std::fmt::Display, row: &str) -> String {
    json!({"msg_type": "data:update", "tag": TAG, "value": format!("{time},{row}")}).to_string()
}

/// V1 with one column replaced; `index` counts columns from 0 in `STREAM_COLUMNS` order.
fn v1_with(index: usize, text: &str) -> String {
    let mut elements: Vec<&str> = V1.split(',').collect();
    elements[index] = text;
    elements.join(",")
}

const EST_LAT: usize = 1;
const ODOMETER: usize = 5;
const POWER: usize = 6;
const SHIFT: usize = 8;
const SPEED: usize = 10;

fn parsed(row: &str) -> StreamUpdate {
    parse_data_update(&u(T0, row)).expect("a valid frame")
}

fn v1_update() -> StreamUpdate {
    parsed(V1)
}

// ------------------------------------------------------------------ SC1 to SC5

#[test]
fn sc1_endpoints_follow_the_region_and_differ() {
    assert_eq!(
        streaming_endpoint(StreamRegion::Global),
        GLOBAL_STREAM_ENDPOINT
    );
    assert_eq!(
        streaming_endpoint(StreamRegion::China),
        CHINA_STREAM_ENDPOINT
    );
    assert!(GLOBAL_STREAM_ENDPOINT.starts_with("wss://"));
    assert!(CHINA_STREAM_ENDPOINT.starts_with("wss://"));
    assert_ne!(GLOBAL_STREAM_ENDPOINT, CHINA_STREAM_ENDPOINT);
    // Tesla's own two hosts are not overrides; anything else is (0.2).
    assert!(!is_override_endpoint(GLOBAL_STREAM_ENDPOINT));
    assert!(!is_override_endpoint(CHINA_STREAM_ENDPOINT));
    assert!(is_override_endpoint("wss://stream.example.test/streaming/"));
    // Both Tesla hosts satisfy the override rule too, so a supervisor built with
    // either passes R3.
    assert!(validate_endpoint_override(GLOBAL_STREAM_ENDPOINT).is_ok());
    assert!(validate_endpoint_override(CHINA_STREAM_ENDPOINT).is_ok());
}

#[test]
fn sc2_override_validation() {
    for accepted in [
        "wss://stream.example.test/streaming/",
        "wss://stream.example.test:8443/x?y=1",
        "wss://127.0.0.1:9443/",
        "wss://[::1]:9443/",
        "WSS://Stream.Example.Test/",
    ] {
        assert_eq!(validate_endpoint_override(accepted), Ok(()), "{accepted}");
    }
    let too_long = format!("wss://{}", "a".repeat(2_045));
    assert_eq!(too_long.len(), 2_051);
    let at_limit = format!("wss://stream.example.test/{}", "a".repeat(2_048 - 26));
    assert_eq!(at_limit.len(), 2_048);
    for rejected in [
        "ws://stream.example.test/",
        "https://stream.example.test/",
        "wss://user:pw@stream.example.test/",
        "wss://user@stream.example.test/",
        "wss://stream.example.test/#f",
        "wss://stream.example.test/#",
        "wss:///streaming/",
        "wss://",
        "wss://?x=1",
        "stream.example.test",
        "",
        too_long.as_str(),
        // Whitespace, control characters and backslashes are stripped or rewritten by
        // URL parsers, so they are refused before parsing.
        "wss://stream.example.test/ x",
        "wss://stream.example\\.test/",
        "wss://stream.example.test/\u{7}",
        "wss://stream.example.test:notaport/",
    ] {
        assert_eq!(
            validate_endpoint_override(rejected),
            Err(StreamError::InvalidEndpoint),
            "{rejected:?}"
        );
    }
    // The length bound is the only thing that separates these two.
    assert_eq!(validate_endpoint_override(&at_limit), Ok(()));
    // Userinfo may not hide behind a path-looking prefix, but an `@` after the
    // authority is just path text.
    assert_eq!(
        validate_endpoint_override("wss://stream.example.test/a@b?k=@"),
        Ok(())
    );
}

#[test]
fn r3_the_test_constructor_rule_adds_plaintext_loopback_only() {
    for accepted in [
        "ws://127.0.0.1:9000/",
        "ws://[::1]:9000/",
        "ws://localhost:9000/",
        "wss://stream.example.test/",
    ] {
        assert_eq!(
            validate_endpoint_for_test_constructor(accepted),
            Ok(()),
            "{accepted}"
        );
    }
    for rejected in [
        "ws://stream.example:9000/",
        "ws://192.168.1.2:9000/",
        "ws://user@127.0.0.1:9000/",
        "ws://127.0.0.1:9000/#f",
        "ws:///x",
    ] {
        assert_eq!(
            validate_endpoint_for_test_constructor(rejected),
            Err(StreamError::InvalidEndpoint),
            "{rejected}"
        );
    }
    // The production rule never accepts plaintext.
    assert_eq!(
        validate_endpoint_override("ws://127.0.0.1:9000/"),
        Err(StreamError::InvalidEndpoint)
    );
}

#[test]
fn sc3_subscribe_frame_has_exactly_four_members() {
    let frame = subscribe_frame(TAG, "tok-1").expect("frame");
    let Value::Object(members) = serde_json::from_str::<Value>(&frame).expect("JSON") else {
        panic!("not an object");
    };
    assert_eq!(members.len(), 4);
    assert_eq!(members["msg_type"], "data:subscribe_oauth");
    assert_eq!(members["token"], "tok-1");
    assert_eq!(
        members["value"],
        "elevation,est_lat,est_lng,est_range,heading,odometer,power,range,shift_state,soc,speed"
    );
    assert_eq!(members["tag"], TAG);
    assert_eq!(
        STREAM_COLUMNS,
        [
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
            "speed"
        ]
    );
    // The column list is sorted: alphabetical is the Hub's own reproducible order (R15).
    let mut sorted = STREAM_COLUMNS;
    sorted.sort_unstable();
    assert_eq!(sorted, STREAM_COLUMNS);
}

#[test]
fn sc4_subscribe_frame_validates_tag_and_token() {
    let token = "tok-1";
    let twenty_one = "1".repeat(21);
    for tag in ["", "42a", "424 2", "-1", twenty_one.as_str()] {
        assert_eq!(
            subscribe_frame(tag, token),
            Err(StreamError::InvalidTag),
            "{tag:?}"
        );
    }
    assert!(subscribe_frame(&"9".repeat(20), token).is_ok());
    let too_big = "a".repeat(8_193);
    for bad_token in ["", too_big.as_str(), "a\nb", "a\u{7f}b", "a\0b"] {
        let error = subscribe_frame(TAG, bad_token).expect_err("an invalid token");
        assert_eq!(error, StreamError::InvalidToken);
        // No error text carries the token (R8).
        if !bad_token.is_empty() {
            assert!(!error.to_string().contains(bad_token));
            assert!(!format!("{error:?}").contains(bad_token));
        }
    }
    assert!(subscribe_frame(TAG, &"a".repeat(8_192)).is_ok());
    let tag_error = subscribe_frame("", "secret-token").expect_err("tag");
    assert!(!tag_error.to_string().contains("secret-token"));
}

#[test]
fn r6_the_subscribe_frame_buffer_is_built_once_at_its_exact_length() {
    // R6 step 6: the frame holds the token, so it is serialised into one buffer that is
    // sized before it is filled and never reallocated (a growing buffer leaves earlier,
    // smaller copies of the token behind unwiped). Tokens with characters the JSON
    // writer escapes make the exact length differ from the naive one.
    for token in [
        "tok-1".to_owned(),
        "a".repeat(8_192),
        "quote\"and\\backslash".to_owned(),
        "caf\u{e9}-\u{1f697}".to_owned(),
    ] {
        let buffer = subscribe_frame_zeroizing(TAG, &token).expect("frame");
        assert_eq!(
            buffer.capacity(),
            buffer.len(),
            "no spare capacity: {token:?}"
        );
        let text = String::from_utf8(buffer.to_vec()).expect("UTF-8");
        assert_eq!(subscribe_frame(TAG, &token).expect("frame"), text);
        let value: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(value["token"], token.as_str());
    }
    assert_eq!(
        subscribe_frame_zeroizing("", "tok").expect_err("tag"),
        StreamError::InvalidTag
    );
    assert_eq!(
        subscribe_frame_zeroizing(TAG, "").expect_err("token"),
        StreamError::InvalidToken
    );
}

#[test]
fn r52_an_error_type_is_logged_only_when_it_matches_the_grammar() {
    use super::session::loggable_error_token;
    // `^[a-z0-9_]{1,32}$`: kept as it is.
    for kept in ["vehicle_x", "a", "x9_", &"a".repeat(32)] {
        assert_eq!(loggable_error_token(Some(kept)), kept);
    }
    // Anything else, including a token echoed back by a server, is `other`.
    for hidden in [
        "tok-SECRET-1",
        "Vehicle_X",
        "vehicle x",
        "",
        "vehicle\n",
        "caf\u{e9}",
        &"a".repeat(33),
    ] {
        assert_eq!(loggable_error_token(Some(hidden)), "other", "{hidden:?}");
    }
    assert_eq!(loggable_error_token(None), "other");
}

#[test]
fn sc5_unsubscribe_frame_is_exactly_two_members() {
    let frame = unsubscribe_frame(TAG).expect("frame");
    assert_eq!(
        serde_json::from_str::<Value>(&frame).expect("JSON"),
        json!({"msg_type": "data:unsubscribe", "tag": "4242"})
    );
    assert_eq!(unsubscribe_frame(""), Err(StreamError::InvalidTag));
    assert_eq!(unsubscribe_frame("4x"), Err(StreamError::InvalidTag));
}

// ----------------------------------------------------------------- SC22 to SC30

#[test]
fn sc22_a_full_frame_parses_to_every_field() {
    assert_eq!(
        parse_data_update(&u(T0, V1)),
        Ok(StreamUpdate {
            tag: "4242".to_owned(),
            timestamp_ms: T0,
            elevation: Some(25.0),
            est_heading: None,
            est_lat: Some(10.5),
            est_lng: Some(20.25),
            est_range: Some(230.5),
            heading: Some(182.0),
            odometer: Some(12345.6),
            power: Some(15),
            range: Some(241.25),
            shift_state: Some("D".to_owned()),
            soc: Some(80.0),
            speed: Some(30.0),
        })
    );
}

#[test]
fn sc23_an_all_empty_frame_is_all_none_and_counts_nothing_invalid() {
    let (update, invalid) = parse_data_update_counted(&u(T0, E)).expect("frame");
    assert_eq!(
        update,
        StreamUpdate {
            tag: "4242".to_owned(),
            timestamp_ms: T0,
            elevation: None,
            est_heading: None,
            est_lat: None,
            est_lng: None,
            est_range: None,
            heading: None,
            odometer: None,
            power: None,
            range: None,
            shift_state: None,
            soc: None,
            speed: None,
        }
    );
    // An empty element is "not reported", never an invalid one (R22).
    assert_eq!(invalid, 0);
    // Whitespace-only elements are empty after trimming.
    let spaced = parse_data_update(&u(T0, " , ,\t, , , , , , , , ")).expect("frame");
    assert_eq!(spaced.odometer, None);
    assert_eq!(spaced.shift_state, None);
}

#[test]
fn sc24_a_parked_frame_keeps_zero_speed_and_has_no_power_or_gear() {
    let update = parsed("25,10.5,20.25,230.5,182,12345.6,,241.25,,80,0");
    assert_eq!(update.power, None);
    assert_eq!(update.shift_state, None);
    assert_eq!(update.speed, Some(0.0));
    assert_eq!(update.elevation, Some(25.0));
    assert_eq!(update.est_lat, Some(10.5));
    assert_eq!(update.odometer, Some(12345.6));
    assert_eq!(update.range, Some(241.25));
    assert_eq!(update.soc, Some(80.0));
}

#[test]
fn sc25_a_wrong_element_count_rejects_the_whole_frame() {
    let eleven = V1.rsplit_once(',').expect("comma").0;
    let value_frame =
        |value: &str| format!(r#"{{"msg_type":"data:update","tag":"{TAG}","value":"{value}"}}"#);
    for frame in [
        // 11 elements: the time and ten values.
        u(T0, eleven),
        // 13 elements.
        u(T0, &format!("{V1},1")),
        value_frame(""),
        value_frame(&format!("{T0}")),
        // The old 13-element layout, which also had a leading column.
        value_frame("1,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"),
    ] {
        assert_eq!(
            parse_data_update(&frame),
            Err(StreamError::MalformedDataUpdate),
            "{frame}"
        );
    }
}

#[test]
fn sc26_the_sample_time_format() {
    for bad in [
        "",
        "abc",
        "-5",
        "0",
        "00",
        "1760000000000.0",
        "99999999999999999999",
        "9999999999999999999",
        "+5",
        "1e3",
        "1 2",
    ] {
        assert_eq!(
            parse_data_update(&u(bad, V1)),
            Err(StreamError::InvalidTimestamp),
            "{bad:?}"
        );
    }
    assert_eq!(
        parse_data_update(&u(format!(" {T0} "), V1))
            .expect("padded time")
            .timestamp_ms,
        T0
    );
    // 16 digits parse here; plausibility is R20a's (SC67).
    assert_eq!(
        parse_data_update(&u("1760000000000000", V1))
            .expect("16 digits")
            .timestamp_ms,
        1_760_000_000_000_000
    );
    // The largest 19-digit value that fits `i64`.
    assert_eq!(
        parse_data_update(&u(i64::MAX, V1))
            .expect("i64::MAX")
            .timestamp_ms,
        i64::MAX
    );
    // The time is checked after the element count, so a short frame is Malformed.
    assert_eq!(
        parse_data_update(&format!(
            r#"{{"msg_type":"data:update","tag":"{TAG}","value":"abc,1"}}"#
        )),
        Err(StreamError::MalformedDataUpdate)
    );
}

#[test]
fn sc27_the_number_grammar() {
    let update_with = |index: usize, text: &str| parsed(&v1_with(index, text));

    assert_eq!(update_with(SPEED, "30.5").speed, Some(30.5));
    assert_eq!(update_with(POWER, "15.0").power, Some(15));
    assert_eq!(update_with(POWER, "15.5").power, None);
    assert_eq!(update_with(SPEED, "+30").speed, None);
    assert_eq!(update_with(SPEED, "3e1").speed, None);
    assert_eq!(update_with(ODOMETER, "NaN").odometer, None);
    assert_eq!(update_with(ODOMETER, "inf").odometer, None);
    assert_eq!(update_with(ODOMETER, " 12345.6 ").odometer, Some(12345.6));
    assert_eq!(update_with(EST_LAT, "-33.865143").est_lat, Some(-33.865143));
    assert_eq!(update_with(POWER, "-12").power, Some(-12));
    assert_eq!(update_with(7, "241").range, Some(241.0));
    assert_eq!(update_with(POWER, "9223372036854775808").power, None);
    assert_eq!(
        update_with(POWER, "9223372036854775807").power,
        Some(i64::MAX)
    );
    assert_eq!(
        update_with(POWER, "-9223372036854775808").power,
        Some(i64::MIN)
    );
    assert_eq!(update_with(ODOMETER, &"1".repeat(33)).odometer, None);
    assert_eq!(
        update_with(ODOMETER, &"1".repeat(32)).odometer,
        Some("1".repeat(32).parse::<f64>().expect("32 digits"))
    );
    // More of the grammar: a missing digit on either side of the point, a second
    // point, a bare sign, an exponent and a leading zero.
    for bad in [
        ".5", "5.", "-", "-.5", "1.2.3", "--1", "0x10", "1_0", "1e-3",
    ] {
        assert_eq!(update_with(ODOMETER, bad).odometer, None, "{bad:?}");
    }
    assert_eq!(update_with(ODOMETER, "007.50").odometer, Some(7.5));
    assert_eq!(update_with(POWER, "-0.000").power, Some(0));
    // The nearest f64: the stored text round-trips.
    assert_eq!(
        update_with(ODOMETER, "57509.856033").odometer,
        Some(57509.856033)
    );
    // None of these rejected the frame: the rest of V1 is intact.
    let bad = update_with(POWER, "15.5");
    assert_eq!(bad.speed, Some(30.0));
    assert_eq!(bad.soc, Some(80.0));
    assert_eq!(bad.shift_state.as_deref(), Some("D"));
}

#[test]
fn r21_invalid_elements_are_counted_per_frame() {
    let row = "x,10.5,20.25,230.5,182,12345.6,15.5,241.25,DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD,80,3e1";
    let (update, invalid) = parse_data_update_counted(&u(T0, row)).expect("frame");
    // elevation, power, shift_state and speed are invalid; the others stand.
    assert_eq!(invalid, 4);
    assert_eq!(update.elevation, None);
    assert_eq!(update.power, None);
    assert_eq!(update.shift_state, None);
    assert_eq!(update.speed, None);
    assert_eq!(update.est_lat, Some(10.5));
    assert_eq!(update.soc, Some(80.0));
    let (_, none_invalid) = parse_data_update_counted(&u(T0, V1)).expect("frame");
    assert_eq!(none_invalid, 0);
}

#[test]
fn sc28_the_shift_state_text() {
    let shift = |text: &str| parsed(&v1_with(SHIFT, text)).shift_state;
    assert_eq!(shift("  D "), Some("D".to_owned()));
    assert_eq!(shift("SNA"), Some("SNA".to_owned()));
    // Case and a prefix are kept as delivered (C1 R40a normalises).
    assert_eq!(shift("shiftstated"), Some("shiftstated".to_owned()));
    assert_eq!(shift("ShiftStateD"), Some("ShiftStateD".to_owned()));
    assert_eq!(shift(&"D".repeat(32)), Some("D".repeat(32)));
    assert_eq!(shift(&"D".repeat(33)), None);
    assert_eq!(shift("D\u{7}"), None);
    assert_eq!(shift("D\u{e9}"), None);
    // Printable ASCII includes an inner space.
    assert_eq!(shift("D R"), Some("D R".to_owned()));
}

#[test]
fn sc29_the_envelope() {
    let value = format!("{T0},{V1}");
    assert_eq!(
        parse_data_update(&format!(
            r#"{{"msg_type":"data:error","tag":"{TAG}","value":"{value}"}}"#
        )),
        Err(StreamError::WrongMessageType)
    );
    for malformed in [
        "not json".to_owned(),
        "".to_owned(),
        "[]".to_owned(),
        "42".to_owned(),
        "null".to_owned(),
        format!(r#"{{"tag":"{TAG}","value":"{value}"}}"#),
        format!(r#"{{"msg_type":7,"tag":"{TAG}","value":"{value}"}}"#),
        format!(r#"{{"msg_type":"data:update","value":"{value}"}}"#),
        format!(r#"{{"msg_type":"data:update","tag":4242,"value":"{value}"}}"#),
        format!(r#"{{"msg_type":"data:update","tag":null,"value":"{value}"}}"#),
        format!(r#"{{"msg_type":"data:update","tag":"{TAG}"}}"#),
        format!(r#"{{"msg_type":"data:update","tag":"{TAG}","value":5}}"#),
    ] {
        assert_eq!(
            parse_data_update(&malformed),
            Err(StreamError::MalformedDataUpdate),
            "{malformed}"
        );
    }
    // An extra member is ignored.
    let extra = format!(r#"{{"msg_type":"data:update","tag":"{TAG}","value":"{value}","foo":1}}"#);
    assert_eq!(
        parse_data_update(&extra).expect("extra member"),
        v1_update()
    );
    // An envelope time is ignored: element 0 is the sample time (R19), whatever the
    // envelope says and whatever its type.
    for envelope in ["5", r#""later""#, "null", "1.5"] {
        let frame = format!(
            r#"{{"msg_type":"data:update","tag":"{TAG}","timestamp":{envelope},"value":"{value}"}}"#
        );
        assert_eq!(
            parse_data_update(&frame)
                .expect("envelope time")
                .timestamp_ms,
            T0
        );
    }
}

#[test]
fn sc30_parsing_is_deterministic() {
    let frame = u(T0, V1);
    assert_eq!(parse_data_update(&frame), parse_data_update(&frame));
    // Re-parsing captured text gives an equal record (R53).
    let first = parse_data_update(&frame).expect("frame");
    let second = parse_data_update(&frame).expect("frame");
    assert_eq!(first, second);
    assert_eq!(first.clone(), second);
}

// ------------------------------------------------------------ SC37, SC39, SC41, SC42

#[test]
fn sc37_ping_interval() {
    let secs = Duration::from_secs;
    assert_eq!(ping_interval(Some(30_000)), secs(10));
    assert_eq!(ping_interval(Some(36_000)), secs(12));
    assert_eq!(ping_interval(Some(6_000)), secs(5));
    assert_eq!(ping_interval(Some(90_000)), secs(15));
    assert_eq!(ping_interval(Some(0)), secs(15));
    assert_eq!(ping_interval(Some(-1)), secs(15));
    assert_eq!(ping_interval(None), secs(15));
    // The bounds of the clamp: a third of 45 s is 15 s, of 15 s is 5 s.
    assert_eq!(ping_interval(Some(45_000)), secs(15));
    assert_eq!(ping_interval(Some(15_000)), secs(5));
    assert_eq!(ping_interval(Some(1)), secs(5));
    assert_eq!(ping_interval(Some(i64::MAX)), secs(15));
    assert_eq!(ping_interval(Some(36_001)), Duration::from_millis(12_000));
}

#[test]
fn sc39_hello_classification() {
    let busy = FailureClass::Busy { retry_after: None };
    for success in [
        (None, None),
        (Some(0), None),
        (Some(200), None),
        (Some(299), None),
    ] {
        assert_eq!(classify_hello(success.0, success.1), None, "{success:?}");
    }
    assert_eq!(classify_hello(Some(401), None), Some(FailureClass::Refusal));
    assert_eq!(classify_hello(Some(403), None), Some(FailureClass::Refusal));
    assert_eq!(classify_hello(Some(429), None), Some(busy));
    assert_eq!(classify_hello(Some(503), None), Some(busy));
    assert_eq!(classify_hello(Some(500), None), Some(FailureClass::Unknown));
    assert_eq!(classify_hello(Some(1), None), Some(FailureClass::Unknown));
    assert_eq!(classify_hello(Some(300), None), Some(FailureClass::Unknown));
    assert_eq!(classify_hello(Some(199), None), Some(FailureClass::Unknown));
    assert_eq!(classify_hello(Some(-1), None), Some(FailureClass::Unknown));
    assert_eq!(
        classify_hello(None, Some("vehicle_test_a")),
        Some(FailureClass::Vehicle)
    );
    assert_eq!(
        classify_hello(Some(200), Some("something_new")),
        Some(FailureClass::Unknown)
    );
    // An error type that is present decides, even an empty one and even over 401.
    assert_eq!(classify_hello(None, Some("")), Some(FailureClass::Unknown));
    assert_eq!(
        classify_hello(Some(401), Some("vehicle_x")),
        Some(FailureClass::Vehicle)
    );
}

#[test]
fn sc41_error_classification() {
    for vehicle in ["vehicle_test_a", "vehicle_x", "vehicle_0", "vehicle__"] {
        assert_eq!(
            classify_error(Some(vehicle)),
            FailureClass::Vehicle,
            "{vehicle}"
        );
    }
    for unknown in [
        "client_test_a",
        "vehicle_",
        "Vehicle_x",
        "vehicle",
        "something_new",
        "",
        "vehicle_X",
        "vehicle_a-b",
        "vehicle_a b",
        "vehicle_a\n",
        "xvehicle_a",
        " vehicle_a",
    ] {
        assert_eq!(
            classify_error(Some(unknown)),
            FailureClass::Unknown,
            "{unknown:?}"
        );
    }
    assert_eq!(classify_error(None), FailureClass::Unknown);
    // No input gives Refusal: refusal evidence comes only from statuses (R28).
    for token in [
        "refused",
        "auth_failed",
        "unauthorized",
        "client_refused",
        "token_invalid",
        "401",
        "invalid_token",
    ] {
        assert_ne!(
            classify_error(Some(token)),
            FailureClass::Refusal,
            "{token}"
        );
    }
}

#[test]
fn sc42_message_text_never_decides_the_class() {
    // `classify_error` takes the token only: the text cannot reach it. The two
    // synthetic frames of the spec, as the dispatcher will split them.
    let from_frame = |frame: &str| {
        let wire: Value = serde_json::from_str(frame).expect("JSON");
        classify_error(wire["error_type"].as_str())
    };
    assert_eq!(
        from_frame(
            r#"{"msg_type":"data:error","error_type":"vehicle_test_a","value":"token invalid"}"#
        ),
        FailureClass::Vehicle
    );
    assert_eq!(
        from_frame(
            r#"{"msg_type":"data:error","error_type":"something_new","value":"vehicle offline"}"#
        ),
        FailureClass::Unknown
    );
}

// ------------------------------------------------------------ SC46, SC70

/// Runs `f` with a `tracing` subscriber that records everything at any level and
/// returns what was recorded.
fn capture_logs(f: impl FnOnce()) -> String {
    #[derive(Clone, Default)]
    struct Sink(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("sink").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let sink = Sink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::callsite::rebuild_interest_cache();
    tracing::subscriber::with_default(subscriber, f);
    String::from_utf8(sink.0.lock().expect("sink").clone()).expect("UTF-8 log")
}

#[test]
fn sc46_the_moving_limit_is_clamped_silently() {
    let secs = Duration::from_secs;
    let cases = [
        (0, 60),
        (30, 60),
        (60, 60),
        (180, 180),
        (300, 300),
        (900, 300),
    ];
    let logs = capture_logs(|| {
        for (configured, expected) in cases {
            assert_eq!(effective_health_timeout(secs(configured)), secs(expected));
        }
    });
    assert_eq!(logs, "", "the clamp is logged at C3's load, not here (R30)");
    // The capture works: a probe line is recorded.
    let probe = capture_logs(|| tracing::info!("probe line"));
    assert!(probe.contains("probe line"), "{probe:?}");
}

#[test]
fn sc70_the_data_silence_limit() {
    let secs = Duration::from_secs;
    assert_eq!(data_silence_limit(secs(30), true), secs(60));
    assert_eq!(data_silence_limit(secs(180), true), secs(180));
    assert_eq!(data_silence_limit(secs(900), true), secs(300));
    assert_eq!(data_silence_limit(secs(0), true), secs(60));
    assert_eq!(data_silence_limit(secs(30), false), secs(300));
    assert_eq!(data_silence_limit(secs(900), false), secs(300));
    assert_eq!(data_silence_limit(secs(180), false), secs(300));
    // H_move stays above the longest dead-socket window (3 x 15 s, R29/R30).
    assert!(data_silence_limit(secs(0), true) > ping_interval(None) * 3);
    assert!(frame_shows_motion(&v1_update()));
    assert!(!frame_shows_motion(&parsed(PK)));
    assert!(!frame_shows_motion(&parsed(E)));
}

#[test]
fn r30_motion_follows_the_gear_then_the_speed() {
    let motion = |shift: &str, speed: &str| {
        frame_shows_motion(&parsed(
            &v1_with(SPEED, speed).replace(",D,", &format!(",{shift},")),
        ))
    };
    // Drive gears are motion whatever the speed, after C1 R40a normalisation.
    for gear in ["D", "R", "N", "d", "ShiftStateD", " r "] {
        assert!(motion(gear, "0"), "{gear}");
    }
    // Park is not motion whatever the speed.
    assert!(!motion("P", "3"));
    assert!(!motion("ShiftStateP", "3"));
    // With no gear the speed decides. An unrecognised gear (Tesla's `SNA`, "signal not
    // available", or any other text outside P, R, N, D) carries no information about the
    // selector, so it counts as no gear (C4 spec 0.2, B2 clarification of 2026-10-12
    // 23:25, which reads C3 R36 the same way): a stale or unavailable signal must not
    // make a stationary car look parked or moving at random.
    assert!(motion("", "3"));
    assert!(motion("", "0.4"));
    assert!(!motion("", "0"));
    assert!(!motion("", "-1"));
    assert!(motion("SNA", "3"));
    assert!(!motion("SNA", "0"));
    // An unreported speed is no motion.
    assert!(!frame_shows_motion(&parsed(
        &v1_with(SPEED, "").replace(",D,", ",,")
    )));
}

// -------------------------------------------------------------- SC48, SC49

#[test]
fn sc48_schedules() {
    let secs = Duration::from_secs;
    let transport = |n: u32| {
        nominal_delay(
            &FailureClass::Transport,
            &RetryState {
                transport_attempt: n,
                ..RetryState::default()
            },
        )
    };
    let counters = 1..=10;
    assert_eq!(
        counters.map(transport).collect::<Vec<_>>(),
        [2, 4, 8, 16, 32, 64, 128, 256, 300, 300].map(secs)
    );
    let vehicle = |n: u32| {
        nominal_delay(
            &FailureClass::Vehicle,
            &RetryState {
                vehicle_attempt: n,
                ..RetryState::default()
            },
        )
    };
    assert_eq!(
        (1..=6).map(vehicle).collect::<Vec<_>>(),
        [15, 30, 60, 120, 120, 120].map(secs)
    );
    let refusal = |n: u32| {
        nominal_delay(
            &FailureClass::Refusal,
            &RetryState {
                refusal_streak: n,
                ..RetryState::default()
            },
        )
    };
    assert_eq!([1, 2, 7].map(refusal), [5, 300, 300].map(secs));
    // Unknown, Protocol and Busy take the Transport schedule (R34).
    let state = RetryState {
        transport_attempt: 4,
        ..RetryState::default()
    };
    for class in [
        FailureClass::Unknown,
        FailureClass::Protocol,
        FailureClass::Busy { retry_after: None },
    ] {
        assert_eq!(nominal_delay(&class, &state), secs(16));
    }
    // The saturated counter and a counter of 0 stay inside the schedule.
    assert_eq!(transport(32), secs(300));
    assert_eq!(transport(u32::MAX), secs(300));
    assert_eq!(transport(0), secs(2));
    assert_eq!(vehicle(0), secs(15));
    assert_eq!(refusal(0), secs(5));
}

#[test]
fn sc49_jitter() {
    let secs = Duration::from_secs;
    assert_eq!(jittered(secs(2), 0.0), Duration::from_millis(1_000));
    assert_eq!(jittered(secs(2), 0.5), Duration::from_millis(1_500));
    assert_eq!(
        jittered(secs(300), 0.999_999),
        Duration::from_millis(299_999)
    );
    // The range is [nominal / 2, nominal): never below half, never the nominal.
    for step in 0..1_000 {
        let u = f64::from(step) / 1_000.0;
        let delay = jittered(secs(15), u);
        assert!(
            delay >= Duration::from_millis(7_500) && delay < secs(15),
            "{u} {delay:?}"
        );
    }
    // A fault in the source cannot leave the documented range.
    assert_eq!(jittered(secs(2), -3.0), Duration::from_millis(1_000));
    assert_eq!(jittered(secs(2), f64::NAN), Duration::from_millis(1_000));
    assert_eq!(jittered(secs(2), 7.0), secs(2));

    let busy = |retry_after: Option<Duration>| {
        next_delay(
            &FailureClass::Busy { retry_after },
            &RetryState {
                transport_attempt: 1,
                ..RetryState::default()
            },
            0.0,
        )
    };
    assert_eq!(busy(Some(secs(600))), secs(600));
    assert_eq!(busy(Some(secs(7_200))), secs(3_600));
    assert_eq!(busy(None), secs(1));
    // The floor is applied after jitter: a small retry-after never lowers the delay.
    assert_eq!(busy(Some(Duration::ZERO)), secs(1));
    assert_eq!(busy(Some(secs(1))), secs(1));
    let late = next_delay(
        &FailureClass::Busy {
            retry_after: Some(secs(1)),
        },
        &RetryState {
            transport_attempt: 9,
            ..RetryState::default()
        },
        0.0,
    );
    assert_eq!(late, secs(150));
    // The other classes jitter their own nominal delay.
    assert_eq!(
        next_delay(
            &FailureClass::Vehicle,
            &RetryState {
                vehicle_attempt: 1,
                ..RetryState::default()
            },
            0.0
        ),
        Duration::from_millis(7_500)
    );
    assert_eq!(
        next_delay(
            &FailureClass::Refusal,
            &RetryState {
                refusal_streak: 1,
                ..RetryState::default()
            },
            0.0
        ),
        Duration::from_millis(2_500)
    );
    assert_eq!(
        next_delay(
            &FailureClass::Refusal,
            &RetryState {
                refusal_streak: 2,
                ..RetryState::default()
            },
            0.0
        ),
        secs(150)
    );
    // R38: the smallest delay of any class is the first Transport delay at u = 0, 1 s.
    let floor = [
        FailureClass::Transport,
        FailureClass::Unknown,
        FailureClass::Protocol,
        FailureClass::Busy { retry_after: None },
        FailureClass::Vehicle,
        FailureClass::Refusal,
    ]
    .into_iter()
    .flat_map(|class| {
        (1..=40).map(move |n| {
            let mut state = RetryState::default();
            for _ in 0..n {
                state.on_session_end(&class);
            }
            next_delay(&class, &state, 0.0)
        })
    })
    .min();
    assert_eq!(floor, Some(secs(1)));
}

#[test]
fn r42_and_r6a_start_and_terminal_delays() {
    assert_eq!(start_delay(0.0), Duration::ZERO);
    assert_eq!(start_delay(0.5), Duration::from_millis(500));
    assert_eq!(start_delay(0.999_9), Duration::from_millis(999));
    assert!(start_delay(f64::NAN) <= Duration::from_secs(1));
    assert_eq!(terminal_credential_delay(0.0), Duration::from_secs(150));
    assert_eq!(terminal_credential_delay(0.5), Duration::from_secs(225));
    assert_eq!(
        terminal_credential_delay(0.999_999),
        Duration::from_millis(299_999)
    );
}

#[test]
fn r35_the_production_jitter_stays_in_range_and_varies() {
    let mut source = OsJitter;
    let draws: Vec<f64> = (0..200).map(|_| source.draw()).collect();
    assert!(draws.iter().all(|draw| (0.0..1.0).contains(draw)));
    assert!(
        draws.windows(2).any(|pair| pair[0] != pair[1]),
        "200 equal draws"
    );
}

// ------------------------------------------------------------------- SC65

#[test]
fn sc65_the_c1_fold_accepts_the_parsed_frame() {
    let payload = crate::lifecycle::stream_observation_payload(&v1_update());
    assert_eq!(
        payload,
        json!({
            "record_type": "tesla_stream_update_v1",
            "source_vehicle_state": "online",
            "fields": {
                "drive_state": {
                    "timestamp": T0,
                    "speed": 30.0,
                    "odometer": 12345.6,
                    "elevation": 25.0,
                    "heading": 182.0,
                    "est_heading": null,
                    "latitude": 10.5,
                    "longitude": 20.25,
                    "est_lat": 10.5,
                    "est_lng": 20.25,
                    "power": 15,
                    "shift_state": "D"
                },
                "charge_state": {
                    "timestamp": T0,
                    "battery_level": 80.0,
                    "ideal_battery_range": 241.25,
                    "est_battery_range": 230.5
                },
                "vehicle_state": {
                    "timestamp": T0,
                    "odometer": 12345.6
                }
            }
        })
    );
    for key in ["speed", "elevation", "heading"] {
        assert!(payload["fields"]["drive_state"][key].is_number(), "{key}");
    }
    assert!(payload["fields"]["charge_state"]["battery_level"].is_number());
    // The fold reads the stored numbers back: a float frame is an ordinary sample.
    let sample = crate::lifecycle::LifecycleSample {
        observation_id: 1,
        observed_at_ms: T0,
        vehicle_state: "online".to_owned(),
        payload,
    };
    let step =
        crate::lifecycle::apply_sample(crate::lifecycle::OpenSessionState::new(), 1, &sample)
            .expect("the fold accepts the frame");
    assert_eq!(step.state.last_reach.as_deref(), Some("online"));
}

// ------------------------------------------------------------------- SC67

#[test]
fn sc67_plausible_sample_time() {
    let earliest = crate::lifecycle::K_EARLIEST_MS;
    assert!(plausible_sample_time(T0, W));
    assert!(plausible_sample_time(earliest, W));
    assert!(plausible_sample_time(W + 60_000, W));
    assert!(!plausible_sample_time(earliest - 1, W));
    assert!(!plausible_sample_time(W + 60_001, W));
    assert!(!plausible_sample_time(1_760_000_000_000_000, W));
    assert!(!plausible_sample_time(W + 86_400_000, W));
    assert!(!plausible_sample_time(0, W));
    assert!(!plausible_sample_time(-1, W));
    // A wall clock at the top of the range cannot overflow the bound.
    assert!(plausible_sample_time(T0, i64::MAX));
    assert!(!plausible_sample_time(i64::MAX, W));
}

// ------------------------------------------------------------------- SC68

#[test]
fn sc68_counter_functions() {
    let steps = |counters: RetryState| {
        (
            counters.transport_attempt,
            counters.vehicle_attempt,
            counters.refusal_streak,
        )
    };
    let mut state = RetryState::default();
    assert_eq!(steps(state), (0, 0, 0));

    // Transport, Busy, Unknown and Protocol: transport + 1, the others unchanged.
    for class in [
        FailureClass::Transport,
        FailureClass::Busy {
            retry_after: Some(Duration::from_secs(5)),
        },
        FailureClass::Unknown,
        FailureClass::Protocol,
    ] {
        let mut after = RetryState {
            transport_attempt: 2,
            vehicle_attempt: 3,
            refusal_streak: 1,
        };
        after.on_session_end(&class);
        assert_eq!(steps(after), (3, 3, 1), "{class:?}");
    }
    // Vehicle: vehicle + 1 and the transport counter restarts.
    let mut after = RetryState {
        transport_attempt: 5,
        vehicle_attempt: 3,
        refusal_streak: 1,
    };
    after.on_session_end(&FailureClass::Vehicle);
    assert_eq!(steps(after), (0, 4, 1));
    // Refusal: streak + 1 and the transport counter restarts.
    let mut after = RetryState {
        transport_attempt: 5,
        vehicle_attempt: 3,
        refusal_streak: 1,
    };
    after.on_session_end(&FailureClass::Refusal);
    assert_eq!(steps(after), (0, 3, 2));
    // The first accepted frame clears the vehicle and refusal counters only.
    let mut after = RetryState {
        transport_attempt: 5,
        vehicle_attempt: 3,
        refusal_streak: 2,
    };
    after.on_first_accepted();
    assert_eq!(steps(after), (5, 0, 0));
    // Stability clears the transport counter only.
    let mut after = RetryState {
        transport_attempt: 5,
        vehicle_attempt: 3,
        refusal_streak: 2,
    };
    after.on_stable();
    assert_eq!(steps(after), (0, 3, 2));

    // 40 consecutive Transport ends saturate at 32, whatever the class.
    for class in [
        FailureClass::Transport,
        FailureClass::Vehicle,
        FailureClass::Refusal,
    ] {
        for _ in 0..40 {
            state.on_session_end(&class);
        }
    }
    assert_eq!(steps(state), (0, 32, 32));
    let mut transport = RetryState::default();
    for _ in 0..40 {
        transport.on_session_end(&FailureClass::Transport);
    }
    assert_eq!(transport.transport_attempt, 32);
}

// ------------------------------------------------------------------- SC69

#[test]
fn sc69_power_gate() {
    let gate = StreamPowerGate::default();
    assert!(!gate.is_confirmed_at(W), "a fresh gate is not confirmed");
    gate.observe(&v1_update(), W);
    assert!(gate.is_confirmed_at(W));
    assert!(gate.is_confirmed_at(W + 30_000));
    assert!(!gate.is_confirmed_at(W + 30_001));
    assert!(!gate.is_confirmed_at(W - 1), "a backward step fails safe");
    gate.revoke();
    assert!(!gate.is_confirmed_at(W));

    let observed = |row: &str| {
        let gate = StreamPowerGate::default();
        gate.observe(&parsed(row), W);
        gate.is_confirmed_at(W)
    };
    assert!(!observed(PK), "gear P, speed 0, power 0");
    assert!(
        observed("25,10.5,20.25,230.5,182,12345.6,-7,241.25,P,80,0"),
        "power -7 is use"
    );
    assert!(
        observed("25,10.5,20.25,230.5,182,12345.6,1,241.25,P,80,0"),
        "power at the 1 kW bound"
    );
    assert!(!observed(E), "an all-empty frame confirms nothing");
    assert!(
        observed(&v1_with(SHIFT, "ShiftStateD")),
        "a prefixed gear is motion"
    );
    assert!(
        observed("25,10.5,20.25,230.5,182,12345.6,0,241.25,,80,3"),
        "no gear with speed 3"
    );
    assert!(
        !observed("25,10.5,20.25,230.5,182,12345.6,0,241.25,P,80,3"),
        "gear P with speed 3"
    );

    // A frame that shows neither leaves the gate as it is (R50): it neither confirms
    // nor revokes, and it does not move the confirmation time.
    let gate = StreamPowerGate::default();
    gate.observe(&v1_update(), W);
    gate.observe(&parsed(PK), W + 20_000);
    assert!(gate.is_confirmed_at(W + 30_000));
    assert!(!gate.is_confirmed_at(W + 30_001));
    // A later confirming frame moves the time.
    gate.observe(&v1_update(), W + 20_000);
    assert!(gate.is_confirmed_at(W + 50_000));
    assert!(!gate.is_confirmed_at(W + 50_001));
}

#[test]
fn r50_the_gate_reads_its_own_wall_clock() {
    struct TestClock(AtomicI64);
    impl WallClock for TestClock {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }
    let clock = Arc::new(TestClock(AtomicI64::new(W)));
    let gate = StreamPowerGate::default();
    gate.set_clock(clock.clone());
    gate.observe(&v1_update(), W);
    assert!(gate.is_confirmed());
    clock.0.store(W + 30_000, Ordering::SeqCst);
    assert!(gate.is_confirmed());
    clock.0.store(W + 30_001, Ordering::SeqCst);
    assert!(!gate.is_confirmed());
    // A wall clock set back to before the confirmation reads as not confirmed.
    clock.0.store(W - 1, Ordering::SeqCst);
    assert!(!gate.is_confirmed());
    clock.0.store(W + 1, Ordering::SeqCst);
    assert!(gate.is_confirmed());
    gate.revoke();
    assert!(!gate.is_confirmed());
    // The production clock reads the real time: a gate confirmed now is fresh now
    // and a gate confirmed a minute ago is not.
    let system = StreamPowerGate::default();
    let now = SystemWallClock.now_ms();
    assert!(now > T0, "the host clock is after the fixture time");
    system.observe(&v1_update(), now);
    assert!(system.is_confirmed());
    system.observe(&v1_update(), now - 60_000);
    assert!(!system.is_confirmed());
}

// ------------------------------------------------------------------- SC72

#[test]
fn sc72_session_outcome() {
    use EndCause::*;
    use SessionOutcome::*;
    // (subscribed, cause, unsubscribe_ok, expected)
    let rows = [
        (false, SubscribeWriteFailed, false, Failed),
        (false, Shutdown, false, CancelledBeforeSubscription),
        (true, Vehicle, true, TransportEnded),
        (true, Refusal, true, Failed),
        (true, Refusal, false, Failed),
        (true, DataSilence, true, TransportEnded),
        (true, SocketEnded, false, TransportEnded),
        (true, Protocol, true, Failed),
        (true, QueueFull, true, Failed),
        (true, ReceiverClosed, true, Failed),
        (true, Shutdown, true, OrderlyShutdown),
        (true, Shutdown, false, TransportEnded),
        (true, Unknown, true, TransportEnded),
        (true, Busy, true, TransportEnded),
        (true, AuditFailed, false, Failed),
        // No subscription was established: connect failure, credential error.
        (false, ConnectFailed, false, Failed),
        (false, CredentialError, false, Failed),
        // A cause that needs a subscription never reads as a transport end without one.
        (false, SocketEnded, false, Failed),
    ];
    for (subscribed, cause, unsubscribe_ok, expected) in rows {
        assert_eq!(
            session_outcome(subscribed, cause, unsubscribe_ok),
            expected,
            "{subscribed} {cause:?} {unsubscribe_ok}"
        );
    }
    // The schema allows an unsubscribe link only for the orderly shutdown (R14): the
    // other outcomes map to a terminal outcome and this one does not.
    assert_eq!(OrderlyShutdown.terminal(), None);
    assert!(matches!(
        Failed.terminal(),
        Some(StreamSessionTerminalOutcome::Failed)
    ));
    assert!(matches!(
        TransportEnded.terminal(),
        Some(StreamSessionTerminalOutcome::TransportEnded)
    ));
    assert!(matches!(
        CancelledBeforeSubscription.terminal(),
        Some(StreamSessionTerminalOutcome::CancelledBeforeSubscription)
    ));
}

// ------------------------------------------------------------------- SC78

#[test]
fn sc78_upgrade_classification() {
    let busy = FailureClass::Busy { retry_after: None };
    assert_eq!(classify_upgrade(401, false), FailureClass::Transport);
    assert_eq!(classify_upgrade(403, false), FailureClass::Transport);
    assert_eq!(classify_upgrade(401, true), FailureClass::Refusal);
    assert_eq!(classify_upgrade(403, true), FailureClass::Refusal);
    assert_eq!(classify_upgrade(503, false), busy);
    assert_eq!(classify_upgrade(429, true), busy);
    assert_eq!(classify_upgrade(429, false), busy);
    assert_eq!(classify_upgrade(503, true), busy);
    assert_eq!(classify_upgrade(404, true), FailureClass::Transport);
    assert_eq!(classify_upgrade(404, false), FailureClass::Transport);
    assert_eq!(classify_upgrade(500, true), FailureClass::Transport);
    assert_eq!(classify_upgrade(200, true), FailureClass::Transport);
}

// ----------------------------------------------------- R33 and R3 at the socket

#[test]
fn r33_the_socket_is_bounded_to_64_kib() {
    let config = stream_socket_config();
    assert_eq!(config.max_message_size, Some(65_536));
    assert_eq!(config.max_frame_size, Some(65_536));
    assert_eq!(config.max_write_buffer_size, 65_536);
    assert_eq!(config.read_buffer_size, 65_536);
    // The library panics when the write limit is not above the flush size.
    assert!(config.write_buffer_size < config.max_write_buffer_size);
    // The Hub's own largest frame, a subscribe frame at the token limit, fits in
    // one flush and far inside the write limit.
    let largest = subscribe_frame("9".repeat(20).as_str(), &"a".repeat(8_192)).expect("frame");
    assert!(
        largest.len() < config.write_buffer_size,
        "{}",
        largest.len()
    );
}

fn legacy_supervisor(
    endpoint: String,
    production_rule: bool,
    events: mpsc::Sender<StreamEvent>,
) -> Result<TeslaStreamSupervisor, StreamSupervisorError> {
    crate::crypto::install_default_provider();
    let (expires_at, next_refresh_at) = super::test_peer::schedule_not_due();
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        Url::parse("http://127.0.0.1:9/").unwrap(),
        "test-access",
        "test-refresh",
    )
    .with_test_schedule(expires_at, next_refresh_at);
    let manager = LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
    let client = reqwest::Client::builder().build().unwrap();
    let arguments = (
        VehicleId::from_test(9),
        StreamVehicleId::from_test(9),
        Arc::new(Mutex::new(manager)),
        StreamRegion::Global,
        endpoint,
        client,
        events,
    );
    if production_rule {
        TeslaStreamSupervisor::new_legacy_auth_for_test_production_policy(
            arguments.0,
            arguments.1,
            arguments.2,
            arguments.3,
            arguments.4,
            arguments.5,
            arguments.6,
        )
    } else {
        TeslaStreamSupervisor::new_legacy_auth_for_test(
            arguments.0,
            arguments.1,
            arguments.2,
            arguments.3,
            arguments.4,
            arguments.5,
            arguments.6,
        )
    }
}

#[test]
fn r3_constructors_apply_the_override_rule() {
    let (events, _receiver) = mpsc::channel(1);
    // The test constructor takes plaintext loopback; the production rule never does.
    assert!(legacy_supervisor("ws://127.0.0.1:9000/".to_owned(), false, events.clone()).is_ok());
    assert!(matches!(
        legacy_supervisor("ws://127.0.0.1:9000/".to_owned(), true, events.clone()),
        Err(StreamSupervisorError::InvalidEndpoint(
            StreamError::InvalidEndpoint
        ))
    ));
    for endpoint in [GLOBAL_STREAM_ENDPOINT, CHINA_STREAM_ENDPOINT] {
        // R1: the endpoint is the string given; the region is not used to rewrite it.
        let supervisor = legacy_supervisor(endpoint.to_owned(), true, events.clone())
            .expect("a Tesla endpoint passes R3");
        assert_eq!(supervisor.endpoint, endpoint);
    }
    assert!(matches!(
        legacy_supervisor(
            "https://stream.example.test/".to_owned(),
            true,
            events.clone()
        ),
        Err(StreamSupervisorError::InvalidEndpoint(_))
    ));
    // R8: the tag is the stream vehicle id's decimal text.
    let supervisor =
        legacy_supervisor(GLOBAL_STREAM_ENDPOINT.to_owned(), true, events).expect("supervisor");
    assert_eq!(supervisor.tag, "9");
}
