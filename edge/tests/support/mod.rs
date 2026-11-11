#![allow(dead_code)]

pub mod hub_double;

use serde_json::json;
use teslatlas_edge::protocol::ReceiverEnvelope;

pub const VIN: &str = "5YJ3E1EA7KF000001";
pub const T0: i64 = 1_800_000_000_000;

pub fn receiver_envelope(txid: &str, timestamp_ms: i64) -> ReceiverEnvelope {
    ReceiverEnvelope::parse(
        &serde_json::to_vec(&json!({
            "version": 1,
            "vin": VIN,
            "txid": txid,
            "tx_type": "vehicle_data",
            "received_at_ms": timestamp_ms + 100,
            "timestamp_ms": timestamp_ms,
            "payload": {
                "vin": VIN,
                "createdAt": "2027-01-15T08:00:00Z",
                "data": {"Soc": {"intValue": "80"}}
            }
        }))
        .unwrap(),
    )
    .unwrap()
}
