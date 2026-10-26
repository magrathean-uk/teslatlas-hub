// SPDX-License-Identifier: AGPL-3.0-only

// C7 part B tests (spec sections 7.2 and 7.3, T15-T54 and T56): the pure
// response-to-address mapping, on hand-written synthetic fixtures F1-F11, run
// through the entry point the request path calls (`geocoder.rs::parse_response`). The
// retained storage predicate these tests rely on is recorded in the doc comment
// of `clean_text` in `response_mapping.rs`; T53 calls the kept storage API and
// T54 the kept cache wrapper against a loopback stub.

use crate::config::GeocoderConfig;
use crate::db::{AddressCacheRecord, AddressLookupBinding, HubStore};
use crate::geocoder::response_mapping::{RAW_MAX_BYTES, parse_response as mapping_parse_response};
use crate::geocoder::{GeocodedAddress, Geocoder, GeocoderError, parse_response};
use crate::location::Wgs84Point;
use axum::{Router, http::HeaderMap, http::StatusCode, routing::any};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

pub(super) use crate::test_assertions::LabelledResult;

const LIC: &str = "Data (c) test contributors, ODbL 1.0";
/// K-RAW: the largest body kept as `raw_json`.
const RAW_CAP: usize = RAW_MAX_BYTES;

const F1: &str = r#"{"place_id":7001,"licence":"Data (c) test contributors, ODbL 1.0","osm_type":"way","osm_id":"880001","lat":"52.4100000","lon":"-1.5200000","place_rank":30,"category":"building","type":"house","addresstype":"building","name":"","display_name":"14, Fernhill Road, Larkfield, Ashcombe, West Wexshire, England, AB1 2CD, United Kingdom","address":{"house_number":"14","road":"Fernhill Road","suburb":"Larkfield","city":"Ashcombe","county":"West Wexshire","state":"England","ISO3166-2-lvl4":"GB-ENG","postcode":"AB1 2CD","country":"United Kingdom","country_code":"gb"}}"#;
const F2: &str = r#"{"osm_type":"node","osm_id":880002,"lat":"51.0","lon":"-2.0","addresstype":"building","name":null,"display_name":"Rose Cottage, Mill Lane, Little Brampton, Eastshire, England, EF3 4GH, United Kingdom","address":{"house_name":"Rose Cottage","road":"Mill Lane","village":"Little Brampton","county":"Eastshire","state":"England","postcode":"EF3 4GH","country":"United Kingdom","country_code":"gb"}}"#;
const F3: &str = r#"{"osm_type":"way","osm_id":880003,"lat":"-34.5","lon":"-58.7","category":"highway","type":"primary","addresstype":"road","name":"Ruta Ejemplo","display_name":"Ruta Ejemplo, Los Alamos, Partido de Ejemplo, Buenos Aires, B1234, Argentina","address":{"road":"Ruta Ejemplo","village":"Los Alamos","state_district":"Partido de Ejemplo","state":"Buenos Aires","postcode":"B1234","country":"Argentina","country_code":"ar"}}"#;
const F4: &str = r#"{"osm_type":"way","osm_id":880004,"lat":"59.6","lon":"10.7","addresstype":"road","name":"Skogveien","display_name":"Skogveien, Nordli, Bjorkvik, Ostfjord, 1912, Norge","address":{"road":"Skogveien","hamlet":"Nordli","municipality":"Bjorkvik","county":"Ostfjord","postcode":"1912","country":"Norge","country_code":"no"}}"#;
const F5: &str = r#"{"osm_type":"node","osm_id":880005,"lat":"52.53","lon":"13.38","addresstype":"place","name":"","display_name":"7, Beispielstrasse, Nordviertel, Mitte-Nord, Berlin, 10115, Deutschland","address":{"house_number":"7","road":"Beispielstrasse","suburb":"Nordviertel","borough":"Mitte-Nord","city":"Berlin","state":"Berlin","ISO3166-2-lvl4":"DE-BE","postcode":"10115","country":"Deutschland","country_code":"de"}}"#;
const F6: &str = r#"{"osm_type":"way","osm_id":880006,"lat":"1.3","lon":"103.8","addresstype":"road","name":"Example Road","display_name":"Example Road, Example Park, 123456, Singapore","address":{"road":"Example Road","suburb":"Example Park","postcode":"123456","country":"Singapore","country_code":"sg"}}"#;
const F7: &str = r#"{"osm_type":"way","osm_id":880007,"lat":"18.4","lon":"-66.1","addresstype":"road","name":"Calle Ejemplo","display_name":"Calle Ejemplo, Puerto Rico","address":{"road":"Calle Ejemplo","state":"Puerto Rico","country":"Puerto Rico","country_code":"pr"}}"#;
const F8: &str = r#"{"osm_type":"node","osm_id":880008,"lat":"35.6","lon":"139.7","addresstype":"place","name":"","display_name":"5, 桜台三丁目, 緑ヶ丘, 例市, 例県, 100-0001, 日本","address":{"house_number":"5","neighbourhood":"桜台三丁目","suburb":"緑ヶ丘","city":"例市","state":"例県","postcode":"100-0001","country":"日本","country_code":"jp"}}"#;
const F9: &str = r#"{"osm_type":"node","osm_id":880009,"lat":40.1,"lon":-82.9,"addresstype":"place","display_name":"2200, Example Avenue, Maple Bend, Harlow County, Ohio, 43001, United States","address":{"house_number":"2200","road":"Example Avenue","town":"Maple Bend","county":"Harlow County","state":"Ohio","postcode":"43001","country":"United States","country_code":"us"}}"#;
const F10: &str = r#"{"osm_type":"node","osm_id":880008,"lat":"35.6","lon":"139.7","addresstype":"place","name":"","display_name":"5, Sakuradai 3-chome, Midorigaoka, Example City, Example Prefecture, 100-0001, Japan","address":{"house_number":"5","neighbourhood":"Sakuradai 3-chome","suburb":"Midorigaoka","city":"Example City","state":"Example Prefecture","postcode":"100-0001","country":"Japan","country_code":"jp"}}"#;
const F11: &str = r#"{"osm_type":"way","osm_id":880011,"lat":"52.4","lon":"-1.5","addresstype":"road","name":"Example\u200E Hub\uFEFF\u2028North","display_name":"12, Example\u200E Road\u2028East, Ashcombe\uFEFF, United Kingdom","address":{"house_number":"12","road":"\uFEFFExample\u200E Road\u2028East","neighbourhood":"क्\u200Dष Nagar","city":"Ashcombe","country":"United Kingdom","country_code":"gb"}}"#;

/// F1 with the members of the object and of its address in reverse order.
const F1_REVERSED: &str = r#"{"address":{"country_code":"gb","country":"United Kingdom","postcode":"AB1 2CD","ISO3166-2-lvl4":"GB-ENG","state":"England","county":"West Wexshire","city":"Ashcombe","suburb":"Larkfield","road":"Fernhill Road","house_number":"14"},"display_name":"14, Fernhill Road, Larkfield, Ashcombe, West Wexshire, England, AB1 2CD, United Kingdom","name":"","addresstype":"building","type":"house","category":"building","place_rank":30,"lon":"-1.5200000","lat":"52.4100000","osm_id":"880001","osm_type":"way","licence":"Data (c) test contributors, ODbL 1.0","place_id":7001}"#;

fn parsed(body: &str) -> GeocodedAddress {
    parse_response(body.as_bytes()).expect("the body maps to an address")
}

fn refused(body: &[u8]) -> GeocoderError {
    parse_response(body).expect_err("the body is refused")
}

/// A refusal as its variant name, so tests compare it without `PartialEq`.
fn variant_name(error: &GeocoderError) -> &'static str {
    match error {
        GeocoderError::ResponseRead => "ResponseRead",
        GeocoderError::MalformedResponse => "MalformedResponse",
        GeocoderError::NoResult => "NoResult",
        _ => "other",
    }
}

/// The refusal of `body` as its variant name.
fn refusal(body: &[u8]) -> &'static str {
    variant_name(&refused(body))
}

/// An address with every optional field absent.
fn bare_address(osm_type: &str, osm_id: i64, display_name: &str) -> GeocodedAddress {
    GeocodedAddress {
        osm_type: osm_type.to_owned(),
        osm_id,
        display_name: display_name.to_owned(),
        name: None,
        latitude: None,
        longitude: None,
        house_number: None,
        road: None,
        neighbourhood: None,
        city: None,
        county: None,
        postcode: None,
        state: None,
        state_district: None,
        country: None,
        raw_json: None,
        licence: None,
    }
}

/// The text columns in one array: name, house number, road, neighbourhood,
/// city, county, state district, state, postcode, country.
fn columns(address: &GeocodedAddress) -> [Option<&str>; 10] {
    [
        address.name.as_deref(),
        address.house_number.as_deref(),
        address.road.as_deref(),
        address.neighbourhood.as_deref(),
        address.city.as_deref(),
        address.county.as_deref(),
        address.state_district.as_deref(),
        address.state.as_deref(),
        address.postcode.as_deref(),
        address.country.as_deref(),
    ]
}

/// The body that the tests of T24-T47 extend: identity 1 and display text "X".
fn with_members(extra: Value) -> String {
    let mut body = json!({"osm_type": "node", "osm_id": 1, "display_name": "X"});
    for (key, value) in extra.as_object().expect("extra members").iter() {
        body[key] = value.clone();
    }
    body.to_string()
}

fn with_address(address: Value) -> String {
    with_members(json!({ "address": address }))
}

fn fixture_text(body: &str, member: &str) -> String {
    let document: Value = serde_json::from_str(body).expect("fixture is JSON");
    document[member]
        .as_str()
        .expect("fixture member is text")
        .to_owned()
}

/// What every fixture of T15-T23 and T56 shares: the identity members, the
/// provider's display text, the whole body as `raw_json` and the centroid.
fn check_common(body: &str, address: &GeocodedAddress, osm_type: &str, osm_id: i64) {
    assert_eq!(address.osm_type, osm_type);
    assert_eq!(address.osm_id, osm_id);
    assert_eq!(address.display_name, fixture_text(body, "display_name"));
    assert_eq!(address.raw_json.as_deref(), Some(body));
}

fn f1_expected() -> GeocodedAddress {
    let mut expected = bare_address(
        "way",
        880001,
        "14, Fernhill Road, Larkfield, Ashcombe, West Wexshire, England, AB1 2CD, United Kingdom",
    );
    expected.latitude = Some(52.41);
    expected.longitude = Some(-1.52);
    expected.house_number = Some("14".to_owned());
    expected.road = Some("Fernhill Road".to_owned());
    expected.neighbourhood = Some("Larkfield".to_owned());
    expected.city = Some("Ashcombe".to_owned());
    expected.county = Some("West Wexshire".to_owned());
    expected.postcode = Some("AB1 2CD".to_owned());
    expected.state = Some("England".to_owned());
    expected.country = Some("United Kingdom".to_owned());
    expected.raw_json = Some(F1.to_owned());
    expected.licence = Some(LIC.to_owned());
    expected
}

#[test]
fn c7_t15_to_t23_fixtures() {
    // T15 F1: every field.
    assert_eq!(parsed(F1), f1_expected());

    // T16 F2: the name comes from the house name; no licence.
    let mut expected = bare_address(
        "node",
        880002,
        "Rose Cottage, Mill Lane, Little Brampton, Eastshire, England, EF3 4GH, United Kingdom",
    );
    expected.name = Some("Rose Cottage".to_owned());
    expected.latitude = Some(51.0);
    expected.longitude = Some(-2.0);
    expected.road = Some("Mill Lane".to_owned());
    expected.city = Some("Little Brampton".to_owned());
    expected.county = Some("Eastshire".to_owned());
    expected.postcode = Some("EF3 4GH".to_owned());
    expected.state = Some("England".to_owned());
    expected.country = Some("United Kingdom".to_owned());
    expected.raw_json = Some(F2.to_owned());
    assert_eq!(parsed(F2), expected);

    // T17 F3: a road object keeps its own name; state district present.
    let f3 = parsed(F3);
    check_common(F3, &f3, "way", 880003);
    assert_eq!(f3.latitude, Some(-34.5));
    assert_eq!(f3.longitude, Some(-58.7));
    assert_eq!(f3.licence, None);
    assert_eq!(
        columns(&f3),
        [
            Some("Ruta Ejemplo"),
            None,
            Some("Ruta Ejemplo"),
            None,
            Some("Los Alamos"),
            None,
            Some("Partido de Ejemplo"),
            Some("Buenos Aires"),
            Some("B1234"),
            Some("Argentina"),
        ]
    );

    // T18 F4: the municipality outranks the hamlet.
    let f4 = parsed(F4);
    check_common(F4, &f4, "way", 880004);
    assert_eq!(
        columns(&f4),
        [
            Some("Skogveien"),
            None,
            Some("Skogveien"),
            None,
            Some("Bjorkvik"),
            Some("Ostfjord"),
            None,
            None,
            Some("1912"),
            Some("Norge"),
        ]
    );

    // T19 F5: equal city and state are both kept; the suburb outranks the borough.
    let f5 = parsed(F5);
    check_common(F5, &f5, "node", 880005);
    assert_eq!(
        columns(&f5),
        [
            None,
            Some("7"),
            Some("Beispielstrasse"),
            Some("Nordviertel"),
            Some("Berlin"),
            None,
            None,
            Some("Berlin"),
            Some("10115"),
            Some("Deutschland"),
        ]
    );

    // T20 F6: a city-state without a settlement key takes the country name.
    let f6 = parsed(F6);
    check_common(F6, &f6, "way", 880006);
    assert_eq!(
        columns(&f6),
        [
            Some("Example Road"),
            None,
            Some("Example Road"),
            Some("Example Park"),
            Some("Singapore"),
            None,
            None,
            None,
            Some("123456"),
            Some("Singapore"),
        ]
    );

    // T21 F7: a territory gets no city.
    let f7 = parsed(F7);
    check_common(F7, &f7, "way", 880007);
    assert_eq!(
        columns(&f7),
        [
            Some("Calle Ejemplo"),
            None,
            Some("Calle Ejemplo"),
            None,
            None,
            None,
            None,
            Some("Puerto Rico"),
            None,
            Some("Puerto Rico"),
        ]
    );

    // T22 F8: non-Latin text is kept as returned.
    let f8 = parsed(F8);
    check_common(F8, &f8, "node", 880008);
    assert_eq!(
        f8.display_name,
        "5, 桜台三丁目, 緑ヶ丘, 例市, 例県, 100-0001, 日本"
    );
    assert_eq!(
        columns(&f8),
        [
            None,
            Some("5"),
            None,
            Some("桜台三丁目"),
            Some("例市"),
            None,
            None,
            Some("例県"),
            Some("100-0001"),
            Some("日本"),
        ]
    );

    // T23 F9: numeric lat and lon; a town and a county.
    let f9 = parsed(F9);
    check_common(F9, &f9, "node", 880009);
    assert_eq!(f9.latitude, Some(40.1));
    assert_eq!(f9.longitude, Some(-82.9));
    assert_eq!(
        columns(&f9),
        [
            None,
            Some("2200"),
            Some("Example Avenue"),
            None,
            Some("Maple Bend"),
            Some("Harlow County"),
            None,
            Some("Ohio"),
            Some("43001"),
            Some("United States"),
        ]
    );
}

#[test]
fn c7_t24_to_t29_identity_errors() {
    let refusal = |body: &[u8]| {
        variant_name(&parse_response(body).labelled_err(
            "c7_t24_to_t29_identity_errors",
            "the invalid identity body is refused",
        ))
    };
    // T24 error member: any present non-null member means no result.
    for body in [
        r#"{"error":"Unable to geocode"}"#,
        r#"{"error":{"code":400,"message":"lat=51.5"}}"#,
        r#"{"error":"","osm_type":"node","osm_id":1,"display_name":"X"}"#,
        r#"{"error":0,"osm_type":"node","osm_id":1,"display_name":"X"}"#,
        r#"{"error":false,"osm_type":"node","osm_id":1,"display_name":"X"}"#,
    ] {
        assert_eq!(refusal(body.as_bytes()), "NoResult", "{body}");
    }
    let null_error = parsed(r#"{"error":null,"osm_type":"node","osm_id":1,"display_name":"X"}"#);
    assert_eq!(null_error.display_name, "X");

    // T25 decode failures, including a byte-order mark and invalid UTF-8.
    // Nesting beyond the parser's depth limit, inside an otherwise valid object.
    let deep = format!(
        r#"{{"osm_type":"node","osm_id":1,"display_name":"X","x":{}{}}}"#,
        "[".repeat(200),
        "]".repeat(200)
    );
    assert_eq!(parsed(&with_members(json!({"x": [[1]]}))).osm_id, 1);
    let decode_failures: [&[u8]; 11] = [
        b"",
        b"{",
        b"not json",
        b"[]",
        b"\"text\"",
        b"42",
        b"\xEF\xBB\xBF{}",
        b"\xFF",
        b"<html><body>Example</body></html>",
        deep.as_bytes(),
        b"null",
    ];
    for body in decode_failures {
        assert_eq!(refusal(body), "ResponseRead", "{body:?}");
    }

    // T26 missing identity, including the postcode shape.
    for body in [
        r#"{}"#,
        r#"{"osm_type":"node","display_name":"X"}"#,
        r#"{"osm_id":1,"display_name":"X"}"#,
        r#"{"osm_type":null,"osm_id":1,"display_name":"X"}"#,
        r#"{"osm_type":"node","osm_id":null,"display_name":"X"}"#,
        r#"{"category":"place","type":"postcode","display_name":"AB1 2CD, United Kingdom","address":{"postcode":"AB1 2CD"}}"#,
        // A place identifier never stands in for the object identity (R26).
        r#"{"place_id":7001,"osm_type":"node","display_name":"X"}"#,
        r#"{"place_id":7001,"osm_type":"node","osm_id":null,"display_name":"X"}"#,
        r#"{"place_id":7001,"osm_type":"node","osm_id":"12a","display_name":"X"}"#,
        r#"{"place_id":7001,"osm_type":"node","osm_id":0,"display_name":"X"}"#,
        r#"{"place_id":7002,"category":"place","type":"postcode","display_name":"AB1 2CD, United Kingdom","address":{"postcode":"AB1 2CD"}}"#,
    ] {
        assert_eq!(refusal(body.as_bytes()), "MalformedResponse", "{body}");
    }

    // T27 object-type forms.
    let kind_body =
        |literal: &str| format!(r#"{{"osm_type":{literal},"osm_id":1,"display_name":"X"}}"#);
    for (literal, expected) in [
        (r#""node""#, "node"),
        (r#""N""#, "node"),
        (r#"" Way ""#, "way"),
        (r#""R""#, "relation"),
        (r#""RELATION""#, "relation"),
        (r#""postcode""#, "postcode"),
        (r#""w""#, "way"),
    ] {
        assert_eq!(parsed(&kind_body(literal)).osm_type, expected, "{literal}");
    }
    let too_long = format!("\"{}\"", "x".repeat(33));
    let at_limit = format!("\"{}\"", "x".repeat(32));
    assert_eq!(parsed(&kind_body(&at_limit)).osm_type, "x".repeat(32));
    for literal in [
        r#""a b""#,
        too_long.as_str(),
        r#""""#,
        r#""  ""#,
        "7",
        r#""noé""#,
    ] {
        assert_eq!(
            refusal(kind_body(literal).as_bytes()),
            "MalformedResponse",
            "{literal}"
        );
    }

    // T28 identifier forms.
    let id_body =
        |literal: &str| format!(r#"{{"osm_type":"node","osm_id":{literal},"display_name":"X"}}"#);
    for (literal, expected) in [
        ("1", 1),
        (r#""880001""#, 880001),
        ("9223372036854775807", i64::MAX),
        (r#""007""#, 7),
        (r#""9223372036854775807""#, i64::MAX),
    ] {
        assert_eq!(parsed(&id_body(literal)).osm_id, expected, "{literal}");
    }
    for literal in [
        "0",
        r#""0""#,
        "-3",
        "9223372036854775808",
        r#""9223372036854775808""#,
        "12.0",
        "12.5",
        "1e3",
        r#""12a""#,
        r#"" 12""#,
        r#""+12""#,
        r#""""#,
        "true",
    ] {
        assert_eq!(
            refusal(id_body(literal).as_bytes()),
            "MalformedResponse",
            "{literal}"
        );
    }

    // T29 result position.
    let position = |members: &str| {
        let body = format!(r#"{{"osm_type":"node","osm_id":1,"display_name":"X"{members}}}"#);
        let address = parsed(&body);
        (address.latitude, address.longitude)
    };
    assert_eq!(
        position(r#","lat":"52.5","lon":"13.4""#),
        (Some(52.5), Some(13.4))
    );
    assert_eq!(
        position(r#","lat":52.5,"lon":13.4"#),
        (Some(52.5), Some(13.4))
    );
    assert_eq!(position(r#","lat":"95","lon":"13.4""#), (None, Some(13.4)));
    assert_eq!(
        position(r#","lat":"52.5","lon":"-181""#),
        (Some(52.5), None)
    );
    assert_eq!(position(r#","lat":"NaN","lon":"inf""#), (None, None));
    assert_eq!(position(r#","lat":"","lon":"abc""#), (None, None));
    assert_eq!(position(""), (None, None));
    assert_eq!(position(r#","lat":" 52.5 ""#), (Some(52.5), None));
    assert_eq!(
        position(r#","lat":"90","lon":"-180""#),
        (Some(90.0), Some(-180.0))
    );
    assert_eq!(position(r#","lat":true,"lon":[1]"#), (None, None));
}

#[test]
fn c7_t51_precedence() {
    // The error member is read before identity.
    assert_eq!(refusal(br#"{"error":"x","osm_type":7}"#), "NoResult");
    // Identity is checked before the display text.
    assert_eq!(
        refusal(br#"{"osm_type":7,"osm_id":0}"#),
        "MalformedResponse"
    );
    // The shape is checked before the error member.
    assert_eq!(refusal(br#"[{"error":"x"}]"#), "ResponseRead");
    // A missing display text is the last check.
    assert_eq!(
        refusal(br#"{"osm_type":"node","osm_id":1}"#),
        "MalformedResponse"
    );
    assert_eq!(
        refusal(br#"{"osm_id":1,"display_name":"X","error":{"code":1}}"#),
        "NoResult"
    );
}

fn road_of(value: &str) -> Option<String> {
    parsed(&with_address(json!({ "road": value }))).road
}

fn ten_pieces() -> String {
    (1..=10)
        .map(|index| format!("Part-{index:02}-{}", "a".repeat(22)))
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn c7_t30_to_t38_text() {
    // T30 cleaning.
    assert_eq!(
        road_of("  Mill\tLane \u{7} East  ").as_deref(),
        Some("Mill Lane East")
    );
    assert_eq!(road_of("\u{0}"), None);
    assert_eq!(road_of("   "), None);
    assert_eq!(road_of("Mill\nLane").as_deref(), Some("Mill Lane"));

    // T31 text bound, on character boundaries.
    assert_eq!(road_of(&"a".repeat(300)), Some("a".repeat(256)));
    assert_eq!(road_of(&"é".repeat(130)), Some("é".repeat(128)));
    let wide = road_of(&"道".repeat(90)).expect("bounded text");
    assert_eq!(wide, "道".repeat(85));
    assert_eq!(wide.len(), 255);
    assert_eq!(
        road_of(&format!("{} b", "a".repeat(255))),
        Some("a".repeat(255))
    );
    assert_eq!(road_of(&"a".repeat(256)), Some("a".repeat(256)));

    // T32 display bound by whole pieces: eight pieces, 254 bytes.
    let long = ten_pieces();
    assert_eq!(long.len(), 318);
    let by_pieces = parsed(&with_members(json!({ "display_name": long }))).display_name;
    let expected = ten_pieces()
        .split(", ")
        .take(8)
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(by_pieces, expected);
    assert_eq!(by_pieces.len(), 254);

    // T33 a first piece beyond the bound is cut itself.
    let first_long = format!("{}, Tail", "x".repeat(300));
    assert_eq!(
        parsed(&with_members(json!({ "display_name": first_long }))).display_name,
        "x".repeat(256)
    );

    // T34 exactly at the limit, unchanged.
    let at_limit = "x".repeat(256);
    assert_eq!(
        parsed(&with_members(json!({ "display_name": at_limit.clone() }))).display_name,
        at_limit
    );

    // T35 composed display text, in the specified order.
    let composed = parsed(
        r#"{"osm_type":"node","osm_id":5,"display_name":"  ","name":"Example Charging Hub","addresstype":"amenity","address":{"amenity":"Example Charging Hub","house_number":"3","road":"Station Road","city":"Ashcombe","postcode":"AB1 9ZZ","country":"United Kingdom","country_code":"gb"}}"#,
    );
    assert_eq!(
        composed.display_name,
        "Example Charging Hub, 3, Station Road, Ashcombe, AB1 9ZZ, United Kingdom"
    );
    assert_eq!(composed.name.as_deref(), Some("Example Charging Hub"));

    // T36 repeats are skipped; the columns keep their equal values.
    let repeats = parsed(
        r#"{"osm_type":"way","osm_id":6,"name":"Station Road","addresstype":"road","address":{"road":"Station Road","city":"Hamburg","state":"Hamburg","country":"Deutschland","country_code":"de"}}"#,
    );
    assert_eq!(repeats.display_name, "Station Road, Hamburg, Deutschland");
    assert_eq!(repeats.road.as_deref(), Some("Station Road"));
    assert_eq!(repeats.city.as_deref(), Some("Hamburg"));
    assert_eq!(repeats.state.as_deref(), Some("Hamburg"));

    // T37 a display member that is not text is composed instead.
    let not_text =
        parsed(r#"{"osm_type":"node","osm_id":8,"display_name":42,"address":{"city":"Ashcombe"}}"#);
    assert_eq!(not_text.display_name, "Ashcombe");
    assert_eq!(not_text.name, None);

    // T38 nothing to display.
    for body in [
        r#"{"osm_type":"node","osm_id":7}"#,
        r#"{"osm_type":"node","osm_id":7,"display_name":"","address":{"country_code":"gb"}}"#,
    ] {
        assert_eq!(refusal(body.as_bytes()), "MalformedResponse", "{body}");
    }

    // A composed text beyond the bound follows the same whole-piece rule.
    let wide_parts = parsed(&format!(
        r#"{{"osm_type":"node","osm_id":9,"address":{{"road":"{}","city":"{}","state":"{}"}}}}"#,
        "r".repeat(120),
        "c".repeat(120),
        "s".repeat(120),
    ));
    assert_eq!(
        wide_parts.display_name,
        format!("{}, {}", "r".repeat(120), "c".repeat(120))
    );
}

#[test]
fn c7_t56_format_chars() {
    // F11 is written with JSON escapes (spec 7.2), so no invisible code point sits in the source.
    assert!(
        !F11.contains(['\u{200E}', '\u{FEFF}', '\u{2028}', '\u{200D}']),
        "F11 holds a literal invisible code point"
    );
    let address = parsed(F11);
    assert_eq!(address.osm_type, "way");
    assert_eq!(address.osm_id, 880011);
    assert_eq!(address.raw_json.as_deref(), Some(F11));
    // Format characters are kept (storage refuses only control characters);
    // U+2028 has become one space.
    assert_eq!(
        address.name.as_deref(),
        Some("Example\u{200E} Hub\u{FEFF} North")
    );
    assert_eq!(
        address.road.as_deref(),
        Some("\u{FEFF}Example\u{200E} Road East")
    );
    assert_eq!(
        address.neighbourhood.as_deref(),
        Some("\u{0915}\u{094D}\u{200D}\u{0937} Nagar")
    );
    assert_eq!(
        address.display_name,
        "12, Example\u{200E} Road East, Ashcombe\u{FEFF}, United Kingdom"
    );
    assert_eq!(address.house_number.as_deref(), Some("12"));
    assert_eq!(address.city.as_deref(), Some("Ashcombe"));
    assert!(
        address
            .name
            .as_deref()
            .is_some_and(|text| !text.contains('\u{2028}'))
    );
}

fn name_of(extra: Value) -> Option<String> {
    parsed(&with_members(extra)).name
}

fn city_of(address: Value) -> Option<String> {
    parsed(&with_address(address)).city
}

#[test]
fn c7_t39_to_t47_names_licence() {
    // T39 name order.
    assert_eq!(
        name_of(json!({"name": "Example Charging Hub"})).as_deref(),
        Some("Example Charging Hub")
    );
    assert_eq!(
        name_of(json!({"name": "", "addresstype": "amenity", "address": {"amenity": "Ashcombe Car Park"}}))
            .as_deref(),
        Some("Ashcombe Car Park")
    );
    assert_eq!(
        name_of(json!({"name": null, "addresstype": "building", "address": {"house_name": "Rose Cottage"}}))
            .as_deref(),
        Some("Rose Cottage")
    );
    assert_eq!(
        name_of(json!({"name": 12, "addresstype": "amenity", "address": {"amenity": "Y"}}))
            .as_deref(),
        Some("Y")
    );
    assert_eq!(
        name_of(json!({"addresstype": "postcode", "address": {"postcode": "AB1 2CD"}})),
        None
    );
    assert_eq!(
        name_of(json!({"addresstype": "ISO3166-2-lvl4", "address": {"ISO3166-2-lvl4": "GB-ENG"}})),
        None
    );
    assert_eq!(name_of(json!({})), None);
    // Code labels never name a place; a listed label falls through to the house name.
    assert_eq!(
        name_of(json!({"addresstype": "house_number", "address": {"house_number": "4", "house_name": "Hill House"}}))
            .as_deref(),
        Some("Hill House")
    );
    assert_eq!(
        name_of(json!({"addresstype": "country_code", "address": {"country_code": "gb"}})),
        None
    );
    assert_eq!(
        name_of(
            json!({"addresstype": "shop", "address": {"shop": "  ", "house_name": "Corner House"}})
        )
        .as_deref(),
        Some("Corner House")
    );

    // T40 city order.
    let gb = |mut address: Value| {
        address["country_code"] = json!("gb");
        address
    };
    assert_eq!(
        city_of(gb(json!({"city": "C", "town": "T", "village": "V"}))).as_deref(),
        Some("C")
    );
    assert_eq!(
        city_of(gb(json!({"town": "T", "village": "V"}))).as_deref(),
        Some("T")
    );
    assert_eq!(
        city_of(gb(json!({"village": "V", "municipality": "M"}))).as_deref(),
        Some("V")
    );
    assert_eq!(
        city_of(gb(json!({"municipality": "M", "hamlet": "H"}))).as_deref(),
        Some("M")
    );
    assert_eq!(city_of(gb(json!({"hamlet": "H"}))).as_deref(), Some("H"));
    assert_eq!(
        city_of(gb(
            json!({"isolated_dwelling": "Hill Farm", "croft": "Low Croft"})
        )),
        None
    );

    // T41 city-states.
    assert_eq!(
        city_of(json!({"country": "Singapore", "country_code": "SG"})).as_deref(),
        Some("Singapore")
    );
    assert_eq!(
        city_of(json!({"country": "Monaco", "country_code": "mc"})).as_deref(),
        Some("Monaco")
    );
    assert_eq!(
        city_of(json!({"country": "Citta del Vaticano", "country_code": "va"})).as_deref(),
        Some("Citta del Vaticano")
    );
    assert_eq!(
        city_of(json!({"city": "Singapore City", "country": "Singapore", "country_code": "sg"}))
            .as_deref(),
        Some("Singapore City")
    );
    assert_eq!(city_of(json!({"country_code": "sg"})), None);
    assert_eq!(
        city_of(json!({"country": "Gibraltar", "country_code": "gi"})),
        None
    );
    assert_eq!(city_of(json!({"country": "Singapore"})), None);
    assert_eq!(
        city_of(json!({"country": "Singapore", "country_code": 7})),
        None
    );

    // T42 neighbourhood order.
    let neighbourhood_of = |address: Value| parsed(&with_address(address)).neighbourhood;
    assert_eq!(
        neighbourhood_of(
            json!({"neighbourhood": "A", "allotments": "D", "quarter": "B", "suburb": "C"})
        )
        .as_deref(),
        Some("A")
    );
    assert_eq!(
        neighbourhood_of(json!({"quarter": "B", "allotments": "D", "suburb": "C"})).as_deref(),
        Some("D")
    );
    assert_eq!(
        neighbourhood_of(json!({"quarter": "B", "suburb": "C"})).as_deref(),
        Some("B")
    );
    assert_eq!(
        neighbourhood_of(json!({"allotments": "D", "suburb": "C"})).as_deref(),
        Some("D")
    );
    assert_eq!(
        neighbourhood_of(json!({"suburb": "C", "city_district": "E", "borough": "F"})).as_deref(),
        Some("C")
    );
    assert_eq!(
        neighbourhood_of(json!({"city_district": "E", "borough": "F"})).as_deref(),
        Some("E")
    );
    assert_eq!(
        neighbourhood_of(json!({"borough": "F"})).as_deref(),
        Some("F")
    );
    assert_eq!(
        neighbourhood_of(
            json!({"district": "G", "subdivision": "H", "residential": "I", "city_block": "J"})
        ),
        None
    );

    // T43 state, county and state district.
    let state_of = |address: Value| parsed(&with_address(address));
    assert_eq!(
        state_of(json!({"state": "S", "region": "R"}))
            .state
            .as_deref(),
        Some("S")
    );
    assert_eq!(state_of(json!({"region": "R"})).state.as_deref(), Some("R"));
    let levels = state_of(json!({"county": "K", "district": "G", "state_district": "SD"}));
    assert_eq!(levels.county.as_deref(), Some("K"));
    assert_eq!(levels.state_district.as_deref(), Some("SD"));
    assert_eq!(state_of(json!({"district": "G"})).county, None);

    // T44 country.
    assert_eq!(state_of(json!({"country_code": "gb"})).country, None);
    assert_eq!(
        state_of(json!({"country": " United  Kingdom "}))
            .country
            .as_deref(),
        Some("United Kingdom")
    );

    // T45 non-string and odd address members.
    for odd in [json!("x"), json!([]), json!(null)] {
        let address = parsed(&with_address(odd));
        assert_eq!(address.display_name, "X");
        assert_eq!(columns(&address), [None; 10]);
    }
    let typed = parsed(&with_address(
        json!({"house_number": 14, "road": ["A"], "postcode": true}),
    ));
    assert_eq!(
        (typed.house_number, typed.road, typed.postcode),
        (None, None, None)
    );

    // T46 duplicate members: the last occurrence wins.
    let duplicate = parsed(
        r#"{"osm_type":"node","osm_id":1,"display_name":"X","address":{"road":"First Road","road":"Second Road"}}"#,
    );
    assert_eq!(duplicate.road.as_deref(), Some("Second Road"));

    // T47 licence kept verbatim or not at all.
    let licence_of = |value: Value| parsed(&with_members(json!({ "licence": value }))).licence;
    assert_eq!(licence_of(json!("ODbL")).as_deref(), Some("ODbL"));
    assert_eq!(licence_of(json!("  ODbL  ")).as_deref(), Some("  ODbL  "));
    assert_eq!(licence_of(json!("L".repeat(512))), Some("L".repeat(512)));
    assert_eq!(licence_of(json!("L".repeat(513))), None);
    assert_eq!(licence_of(json!("ODbL\u{0}")), None);
    assert_eq!(licence_of(json!("ODbL\n")), None);
    assert_eq!(licence_of(json!("")), None);
    assert_eq!(licence_of(json!("   ")), None);
    assert_eq!(licence_of(json!(42)), None);
    assert_eq!(licence_of(json!(null)), None);
    assert_eq!(parsed(&with_members(json!({}))).licence, None);
}

/// F1 with one more top-level text member, the whole body exactly `length` bytes.
fn f1_padded_to(length: usize) -> String {
    let overhead = F1.len() + r#","pad":"""#.len();
    let pad = "x".repeat(length - overhead);
    let body = format!(r#"{},"pad":"{pad}"}}"#, &F1[..F1.len() - 1]);
    assert_eq!(body.len(), length);
    body
}

#[test]
fn c7_t48_to_t52_raw_unknown() {
    // T48 raw bound: whole body at the cap, nothing one byte over.
    let at_cap = f1_padded_to(RAW_CAP);
    let kept = parsed(&at_cap);
    assert_eq!(kept.raw_json.as_deref(), Some(at_cap.as_str()));
    let over_cap = f1_padded_to(RAW_CAP + 1);
    let dropped = parsed(&over_cap);
    assert_eq!(dropped.raw_json, None);
    let mut without_raw = f1_expected();
    without_raw.raw_json = None;
    assert_eq!(dropped, without_raw);
    let mut kept_without_raw = kept;
    kept_without_raw.raw_json = None;
    assert_eq!(kept_without_raw, without_raw);

    // T49 member order does not change the mapping.
    let document: Value = serde_json::from_str(F1).expect("F1 is JSON");
    let reordered = F1_REVERSED;
    assert_ne!(reordered, F1);
    let reversed_members: Value = serde_json::from_str(reordered).expect("reversed F1 is JSON");
    assert_eq!(reversed_members, document);
    let mut from_reordered = parsed(reordered);
    assert_eq!(from_reordered.raw_json.as_deref(), Some(reordered));
    from_reordered.raw_json = Some(F1.to_owned());
    assert_eq!(from_reordered, f1_expected());

    // T50 determinism.
    assert_eq!(parsed(F1), parsed(F1));
    assert_eq!(refusal(b"[]"), refusal(b"[]"));
    assert_eq!(refusal(br#"{"error":"x"}"#), "NoResult");

    // T52 unknown members map to the same columns.
    let mut extended = document.clone();
    extended["address"]["continent"] = json!("Europe");
    extended["address"]["shop"] = json!("Example Shop");
    extended["importance"] = json!(0.3);
    extended["boundingbox"] = json!(["1", "2", "3", "4"]);
    let extended_text = extended.to_string();
    let mut mapped = parsed(&extended_text);
    assert_eq!(mapped.raw_json.as_deref(), Some(extended_text.as_str()));
    mapped.raw_json = Some(F1.to_owned());
    assert_eq!(mapped, f1_expected());
}

fn record_of(
    address: &GeocodedAddress,
    binding: &AddressLookupBinding,
    point: Wgs84Point,
    looked_up_at_ms: i64,
) -> AddressCacheRecord {
    AddressCacheRecord {
        osm_type: address.osm_type.clone(),
        osm_id: address.osm_id,
        display_name: address.display_name.clone(),
        name: address.name.clone(),
        latitude: address.latitude,
        longitude: address.longitude,
        house_number: address.house_number.clone(),
        road: address.road.clone(),
        neighbourhood: address.neighbourhood.clone(),
        city: address.city.clone(),
        county: address.county.clone(),
        postcode: address.postcode.clone(),
        state: address.state.clone(),
        state_district: address.state_district.clone(),
        country: address.country.clone(),
        raw_json: address.raw_json.clone(),
        binding: binding.clone(),
        licence: address.licence.clone(),
        lookup_latitude: point.latitude,
        lookup_longitude: point.longitude,
        looked_up_at_ms,
    }
}

fn point(latitude: f64, longitude: f64) -> Wgs84Point {
    Wgs84Point::new(latitude, longitude).expect("point in range")
}

fn binding(endpoint: &str, language: &str) -> AddressLookupBinding {
    AddressLookupBinding::nominatim(&url::Url::parse(endpoint).expect("endpoint"), language)
        .expect("binding")
}

#[test]
fn c7_t53_storage_accepts() {
    let mut bodies: Vec<(String, String)> = Vec::new();
    // T15-T23 and T56 fixtures.
    for (label, body) in [
        ("F1", F1),
        ("F2", F2),
        ("F3", F3),
        ("F4", F4),
        ("F5", F5),
        ("F6", F6),
        ("F7", F7),
        ("F8", F8),
        ("F9", F9),
        ("F11", F11),
    ] {
        bodies.push((label.to_owned(), body.to_owned()));
    }
    // T30 and T31 values in a road column.
    for (label, value) in [
        (
            "T30 mixed whitespace",
            "  Mill\tLane \u{7} East  ".to_owned(),
        ),
        ("T30 control only", "\u{0}".to_owned()),
        ("T30 blank", "   ".to_owned()),
        ("T30 newline", "Mill\nLane".to_owned()),
        ("T31 ascii", "a".repeat(300)),
        ("T31 two-byte", "é".repeat(130)),
        ("T31 three-byte", "道".repeat(90)),
        ("T31 trailing space cut", format!("{} b", "a".repeat(255))),
    ] {
        bodies.push((label.to_owned(), with_address(json!({ "road": value }))));
    }
    // T32 and T33 long display texts.
    bodies.push((
        "T32".to_owned(),
        with_members(json!({ "display_name": ten_pieces() })),
    ));
    bodies.push((
        "T33".to_owned(),
        with_members(json!({ "display_name": format!("{}, Tail", "x".repeat(300)) })),
    ));
    // T35 and T36 composed display texts.
    bodies.push((
        "T35".to_owned(),
        r#"{"osm_type":"node","osm_id":5,"display_name":"  ","name":"Example Charging Hub","addresstype":"amenity","address":{"amenity":"Example Charging Hub","house_number":"3","road":"Station Road","city":"Ashcombe","postcode":"AB1 9ZZ","country":"United Kingdom","country_code":"gb"}}"#.to_owned(),
    ));
    bodies.push((
        "T36".to_owned(),
        r#"{"osm_type":"way","osm_id":6,"name":"Station Road","addresstype":"road","address":{"road":"Station Road","city":"Hamburg","state":"Hamburg","country":"Deutschland","country_code":"de"}}"#.to_owned(),
    ));
    // Every other text column at its bound, and a bounded name and licence.
    bodies.push((
        "all columns long".to_owned(),
        with_members(json!({
            "name": "n".repeat(400),
            "licence": "L".repeat(512),
            "address": {
                "house_number": "h".repeat(400),
                "road": "r".repeat(400),
                "neighbourhood": "é".repeat(400),
                "city": "道".repeat(400),
                "county": "k".repeat(400),
                "postcode": "p".repeat(400),
                "state": "s".repeat(400),
                "state_district": "d".repeat(400),
                "country": "c".repeat(400),
            }
        })),
    ));

    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let binding = binding("http://127.0.0.1:8080/", "en-GB");
    let lookup_point = point(52.41, -1.52);
    let mut stored = 0;
    for (label, body) in &bodies {
        let address = parsed(body);
        store
            .put_address_cache(&record_of(&address, &binding, lookup_point, 1000))
            .labelled_ok(
                "c7_t53_storage_accepts",
                &format!("{label}: storage accepts a parser output"),
            );
        stored += 1;
    }
    assert_eq!(stored, bodies.len());

    // The check is live: storage refuses what the parser never produces.
    let mut control = parsed(F1);
    control.road = Some("Fern\u{7}hill".to_owned());
    assert!(
        store
            .put_address_cache(&record_of(&control, &binding, lookup_point, 1000))
            .is_err()
    );
    let mut long = parsed(F1);
    long.display_name = "d".repeat(257);
    assert!(
        store
            .put_address_cache(&record_of(&long, &binding, lookup_point, 1000))
            .is_err()
    );
}

/// A loopback stub that answers F8 to `Accept-Language: ja`, F10 to `en` and
/// 400 to anything else, counting every request it receives.
async fn language_stub() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let endpoint = format!("http://{}", listener.local_addr().expect("local address"));
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_server = Arc::clone(&count);
    let app = Router::new().route(
        "/{*path}",
        any(move |headers: HeaderMap| {
            let count = Arc::clone(&count_for_server);
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                let language = headers
                    .get("accept-language")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default();
                match language {
                    "ja" => (StatusCode::OK, F8),
                    "en" => (StatusCode::OK, F10),
                    _ => (StatusCode::BAD_REQUEST, ""),
                }
            }
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("stub server");
    });
    (endpoint, count, task)
}

/// A geocoder for the stub with its own fresh limiter, as the spec requires.
fn stub_geocoder(endpoint: &str, language: &str) -> Geocoder {
    let config = GeocoderConfig {
        enabled: true,
        endpoint: Some(endpoint.to_owned()),
        language: language.to_owned(),
        timeout_seconds: 2,
    };
    Geocoder::build(&config, true, Arc::new(Mutex::new(None))).expect("geocoder")
}

async fn language_restart() {
    let (endpoint, requests, server) = language_stub().await;
    let at = point(35.6, 139.7);
    let ja_name = fixture_text(F8, "display_name");
    let en_name = fixture_text(F10, "display_name");
    assert_ne!(ja_name, en_name);
    // F8 and F10 are one OSM object in two languages.
    assert_eq!(
        (parsed(F8).osm_type, parsed(F8).osm_id),
        (parsed(F10).osm_type, parsed(F10).osm_id)
    );

    let temporary = crate::private_tempdir().expect("temporary directory");
    {
        let store = HubStore::initialize(temporary.path()).expect("store");
        let ja = stub_geocoder(&endpoint, "ja");
        let en = stub_geocoder(&endpoint, "en");
        // The binding each geocoder derives from its configured language.
        assert_ne!(ja.lookup_binding(), en.lookup_binding());
        assert_eq!(ja.lookup_binding(), &binding(&endpoint, "ja"));
        assert_eq!(en.lookup_binding(), &binding(&endpoint, "en"));

        let first_ja = ja.reverse_cached(&store, at, 2000).await.expect("ja");
        let first_en = en.reverse_cached(&store, at, 2000).await.expect("en");
        assert_eq!(first_ja.display_name, ja_name);
        assert_eq!(first_en.display_name, en_name);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        // Both written: each language still returns its own answer, no request.
        let again_ja = ja.reverse_cached(&store, at, 3000).await.expect("ja");
        let again_en = en.reverse_cached(&store, at, 3000).await.expect("en");
        assert_eq!(again_ja.display_name, ja_name);
        assert_eq!(again_en.display_name, en_name);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    // Reopen the store from the same directory with new geocoders and repeat.
    let reopened = HubStore::initialize(temporary.path()).expect("reopened store");
    let ja = stub_geocoder(&endpoint, "ja");
    let en = stub_geocoder(&endpoint, "en");
    let reopened_ja = ja.reverse_cached(&reopened, at, 4000).await.expect("ja");
    let reopened_en = en.reverse_cached(&reopened, at, 4000).await.expect("en");
    assert_eq!(reopened_ja.display_name, ja_name);
    assert_eq!(reopened_en.display_name, en_name);
    assert_eq!(requests.load(Ordering::SeqCst), 2);

    // The stored rows carry each language's own mapped columns and body.
    let stored_ja = reopened
        .cached_address(ja.lookup_binding(), at)
        .expect("read")
        .expect("hit");
    let stored_en = reopened
        .cached_address(en.lookup_binding(), at)
        .expect("read")
        .expect("hit");
    assert_eq!(stored_ja.neighbourhood.as_deref(), Some("桜台三丁目"));
    assert_eq!(
        stored_en.neighbourhood.as_deref(),
        Some("Sakuradai 3-chome")
    );
    assert_eq!(stored_ja.raw_json.as_deref(), Some(F8));
    assert_eq!(stored_en.raw_json.as_deref(), Some(F10));
    server.abort();
}

#[tokio::test]
async fn c7_t54_language_restart() {
    // Two requests (one per fresh-limiter geocoder; the cache answers the rest): 2 x 1 s + 5 s.
    tokio::time::timeout(Duration::from_secs(7), language_restart())
        .await
        .expect("T54 finishes within 7 s");
}

/// The request path's entry point (`geocoder.rs::parse_response`) and
/// `response_mapping::parse_response` give the same outcome for F1-F11, a top-level array and an HTML body: equal
/// addresses, or the same refusal variant (matched by pattern, because the kept
/// error enum has no `PartialEq`). Every differing case is listed.
#[test]
fn c7_entry_points_agree() {
    const HTML: &str =
        "<!doctype html><html><head><title>Example</title></head><body>Maintenance</body></html>";
    fn outcome(
        result: Result<GeocodedAddress, GeocoderError>,
    ) -> Result<GeocodedAddress, &'static str> {
        result.map_err(|error| variant_name(&error))
    }
    let cases = [
        ("F1", F1),
        ("F2", F2),
        ("F3", F3),
        ("F4", F4),
        ("F5", F5),
        ("F6", F6),
        ("F7", F7),
        ("F8", F8),
        ("F9", F9),
        ("F10", F10),
        ("F11", F11),
        ("[]", "[]"),
        ("html", HTML),
    ];
    let mut differing = Vec::new();
    for (label, body) in cases {
        let entry = outcome(parse_response(body.as_bytes()));
        let mapping = outcome(mapping_parse_response(body.as_bytes()));
        if entry != mapping {
            differing.push(format!("{label}: entry {entry:?}; mapping {mapping:?}"));
        }
    }
    assert!(
        differing.is_empty(),
        "entry points differ on {} of {} cases:\n{}",
        differing.len(),
        cases.len(),
        differing.join("\n")
    );
}
