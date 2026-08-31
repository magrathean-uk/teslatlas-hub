#![forbid(unsafe_code)]

use serde_json::{Value, json};
use teslatlas_edge::protocol::{MAX_RECEIVER_BODY_BYTES, ProtocolError, ReceiverEnvelope};

const VIN: &str = "5YJ3E1EA7KF000001";
const T0: i64 = 1_800_000_000_000;

fn envelope(payload: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "version": 1,
        "vin": VIN,
        "txid": "tx-42",
        "tx_type": "vehicle_data",
        "received_at_ms": T0 + 100,
        "timestamp_ms": T0,
        "payload": payload,
        "device_client_version": "1.3.0",
        "firmware_version": "2026.26.6"
    }))
    .unwrap()
}

#[test]
fn accepts_hub_compatible_receiver_envelope() {
    let parsed = ReceiverEnvelope::parse(&envelope(json!({
        "vin": VIN,
        "createdAt": "2027-01-15T08:00:00Z",
        "data": {"Soc": {"intValue": "80"}}
    })))
    .unwrap();

    assert_eq!(parsed.version, 1);
    assert_eq!(parsed.vin, VIN);
    assert_eq!(parsed.txid, "tx-42");
    assert_eq!(parsed.timestamp_ms, T0);
    assert_eq!(parsed.device_client_version.as_deref(), Some("1.3.0"));
    assert_eq!(parsed.record_id().as_str().len(), 64);
}

#[test]
fn rejects_unknown_fields_and_duplicate_keys() {
    let mut value: Value = serde_json::from_slice(&envelope(json!({}))).unwrap();
    value["unexpected"] = json!(true);
    assert_eq!(
        ReceiverEnvelope::parse(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
        ProtocolError::InvalidJson
    );

    let duplicate = format!(
        r#"{{"version":1,"version":1,"vin":"{VIN}","txid":"tx","tx_type":"vehicle_data","received_at_ms":{T0},"timestamp_ms":{T0},"payload":{{}}}}"#
    );
    assert_eq!(
        ReceiverEnvelope::parse(duplicate.as_bytes()).unwrap_err(),
        ProtocolError::DuplicateJsonKey
    );
}

#[test]
fn enforces_body_identity_and_timestamp_bounds() {
    assert_eq!(
        ReceiverEnvelope::parse(&vec![b' '; MAX_RECEIVER_BODY_BYTES + 1]).unwrap_err(),
        ProtocolError::InputTooLarge
    );

    let mut value: Value = serde_json::from_slice(&envelope(json!({}))).unwrap();
    value["vin"] = json!("invalid");
    assert_eq!(
        ReceiverEnvelope::parse(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
        ProtocolError::InvalidVin
    );

    value["vin"] = json!(VIN);
    value["txid"] = json!("x".repeat(129));
    assert_eq!(
        ReceiverEnvelope::parse(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
        ProtocolError::InvalidTransactionId
    );

    value["txid"] = json!("tx");
    value["timestamp_ms"] = json!(T0 + 300_001);
    value["received_at_ms"] = json!(T0);
    assert_eq!(
        ReceiverEnvelope::parse(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
        ProtocolError::InvalidTimestamp
    );
}

#[test]
fn record_id_is_stable_across_payload_key_order() {
    let first = format!(
        r#"{{"version":1,"vin":"{VIN}","txid":"tx","tx_type":"vehicle_data","received_at_ms":{T0},"timestamp_ms":{T0},"payload":{{"b":2,"a":1}}}}"#
    );
    let second = format!(
        r#"{{"payload":{{"a":1,"b":2}},"timestamp_ms":{T0},"received_at_ms":{T0},"tx_type":"vehicle_data","txid":"tx","vin":"{VIN}","version":1}}"#
    );

    let first = ReceiverEnvelope::parse(first.as_bytes()).unwrap();
    let second = ReceiverEnvelope::parse(second.as_bytes()).unwrap();
    assert_eq!(first.record_id(), second.record_id());
    assert_eq!(
        first.record_id().as_str(),
        "c189e16bf371ef87954f09e1f76130cff73b38ea29590b0de28c5e0e304e76f2"
    );
}

#[test]
fn stable_record_id_ignores_receiver_arrival_time_only() {
    let first = ReceiverEnvelope::parse(&envelope(json!({"value": 1}))).unwrap();
    let mut later_value: Value = serde_json::from_slice(&envelope(json!({"value": 1}))).unwrap();
    later_value["received_at_ms"] = json!(T0 + 5_000);
    let later = ReceiverEnvelope::parse(&serde_json::to_vec(&later_value).unwrap()).unwrap();

    assert_ne!(first.record_id(), later.record_id());
    assert_eq!(first.stable_record_id(), later.stable_record_id());

    later_value["payload"] = json!({"value": 2});
    let changed = ReceiverEnvelope::parse(&serde_json::to_vec(&later_value).unwrap()).unwrap();
    assert_ne!(first.stable_record_id(), changed.stable_record_id());
}

#[test]
fn accepts_every_pinned_sidecar_transaction_type() {
    for tx_type in ["V", "connectivity", "alerts", "errors"] {
        let mut value: Value = serde_json::from_slice(&envelope(json!({}))).unwrap();
        value["tx_type"] = json!(tx_type);
        ReceiverEnvelope::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    }
}

#[test]
fn clock_bounded_parse_rejects_far_future_arrival_time() {
    let mut value: Value = serde_json::from_slice(&envelope(json!({}))).unwrap();
    value["received_at_ms"] = json!(T0 + 300_001);
    value["timestamp_ms"] = json!(T0 + 300_001);

    assert_eq!(
        ReceiverEnvelope::parse_at(&serde_json::to_vec(&value).unwrap(), T0).unwrap_err(),
        ProtocolError::InvalidTimestamp
    );
}
