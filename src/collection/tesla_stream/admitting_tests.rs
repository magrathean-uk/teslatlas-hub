// SPDX-License-Identifier: AGPL-3.0-only

// Tests of the pure dispatch of the Admitting phase (C4 R27 to R29, R33): what one
// inbound message means to a socket that has no subscription yet. Text and UTF-8
// Binary messages share one decoding, so every case runs in both encodings.
// Synthetic data only.

use serde_json::{Value, json};

use super::*;
use crate::collection::tesla_stream::test_peer::TAG;

const BUSY: FailureClass = FailureClass::Busy { retry_after: None };

fn both(frame: &Value) -> [Message; 2] {
    [
        Message::text(frame.to_string()),
        Message::binary(frame.to_string().into_bytes()),
    ]
}

fn hello(members: Value) -> Value {
    let mut frame = json!({"msg_type": "control:hello"});
    frame
        .as_object_mut()
        .expect("an object")
        .extend(members.as_object().expect("an object").clone());
    frame
}

fn error(members: Value) -> Value {
    let mut frame = json!({"msg_type": "data:error"});
    frame
        .as_object_mut()
        .expect("an object")
        .extend(members.as_object().expect("an object").clone());
    frame
}

fn step(frame: &Value) -> AdmittingStep {
    let [text, binary] = both(frame);
    let (from_text, from_binary) = (admitting_step(&text, TAG), admitting_step(&binary, TAG));
    assert_eq!(from_text, from_binary, "{frame}");
    from_text
}

fn seconds(count: u64) -> AdmittingStep {
    AdmittingStep::PingInterval(Duration::from_secs(count))
}

#[test]
fn r27_r28_admitting_dispatch_preserves_classes_and_tag_rules() {
    use AdmittingStep::{Classified, Liveness};
    // A hello that is not a success is classified: an `error_type` string first, the
    // empty one included; otherwise the code.
    assert_eq!(
        step(&hello(json!({"code": 401}))),
        Classified(FailureClass::Refusal)
    );
    assert_eq!(
        step(&hello(json!({"code": 403}))),
        Classified(FailureClass::Refusal)
    );
    // Busy carries no retry-after, asserted directly.
    assert_eq!(step(&hello(json!({"code": 429}))), Classified(BUSY));
    assert_eq!(step(&hello(json!({"code": 503}))), Classified(BUSY));
    assert!(matches!(
        step(&hello(json!({"code": 503}))),
        Classified(FailureClass::Busy { retry_after: None })
    ));
    assert_eq!(
        step(&hello(json!({"code": 500}))),
        Classified(FailureClass::Unknown)
    );
    assert_eq!(
        step(&hello(json!({"code": 401, "error_type": ""}))),
        Classified(FailureClass::Unknown)
    );
    assert_eq!(
        step(&hello(
            json!({"code": 401, "error_type": "vehicle_unavailable"})
        )),
        Classified(FailureClass::Vehicle)
    );
    assert_eq!(
        step(&hello(json!({"code": 200, "error_type": "boom"}))),
        Classified(FailureClass::Unknown)
    );
    // An `error_type` of the wrong JSON type counts as absent.
    assert_eq!(
        step(&hello(json!({"code": 401, "error_type": 7}))),
        Classified(FailureClass::Refusal)
    );
    // A successful hello adopts a positive integer timeout, else only liveness.
    assert_eq!(
        step(&hello(json!({"code": 200, "connection_timeout": 30000}))),
        seconds(10)
    );
    assert_eq!(
        step(&hello(json!({"connection_timeout": 45000}))),
        seconds(15)
    );
    assert_eq!(
        step(&hello(json!({"code": 0, "connection_timeout": 6000}))),
        seconds(5)
    );
    assert_eq!(
        step(&hello(json!({"code": 204, "connection_timeout": 1}))),
        seconds(5)
    );
    for timeout in [
        json!(0),
        json!(-30000),
        json!("30000"),
        json!(30000.5),
        json!(null),
    ] {
        assert_eq!(
            step(&hello(json!({"code": 200, "connection_timeout": timeout}))),
            Liveness,
            "{timeout}"
        );
    }
    assert_eq!(step(&hello(json!({"code": 200}))), Liveness);
    // A non-success hello does not adopt its timeout: it ends the attempt.
    assert_eq!(
        step(&hello(json!({"code": 503, "connection_timeout": 30000}))),
        Classified(BUSY)
    );

    // An applicable `data:error` is classified by its `error_type` only.
    assert_eq!(
        step(&error(
            json!({"tag": TAG, "error_type": "vehicle_unavailable"})
        )),
        Classified(FailureClass::Vehicle)
    );
    assert_eq!(
        step(&error(
            json!({"error_type": "vehicle_unavailable", "value": "boom"})
        )),
        Classified(FailureClass::Vehicle)
    );
    assert_eq!(
        step(&error(
            json!({"tag": TAG, "error_type": "client_error", "value": "vehicle_x"})
        )),
        Classified(FailureClass::Unknown)
    );
    assert_eq!(step(&error(json!({}))), Classified(FailureClass::Unknown));
    assert_eq!(
        step(&error(json!({"tag": TAG, "error_type": 5}))),
        Classified(FailureClass::Unknown)
    );
    // A foreign tag, or a present tag that is not a string, is ignored.
    for tag in [json!("9999"), json!(4242), json!(null), json!(["4242"])] {
        assert_eq!(
            step(&error(
                json!({"tag": tag, "error_type": "vehicle_unavailable"})
            )),
            Liveness,
            "{tag}"
        );
    }

    // Everything else is liveness only.
    assert_eq!(
        step(&json!({"msg_type": "data:update", "tag": TAG, "value": "1,2"})),
        Liveness
    );
    for other in [
        json!({"msg_type": "data:other"}),
        json!({"msg_type": "control:other", "code": 401}),
        json!({"code": 401}),
        json!({"msg_type": 5, "code": 401}),
        json!({"msg_type": null, "error_type": "x"}),
        json!([1, 2]),
        json!(5),
        json!("control:hello"),
        json!(null),
    ] {
        assert_eq!(step(&other), Liveness, "{other}");
    }
    assert_eq!(admitting_step(&Message::text("{not json"), TAG), Liveness);
    assert_eq!(admitting_step(&Message::text(""), TAG), Liveness);
    assert_eq!(
        admitting_step(&Message::binary(vec![0xff, 0xfe, b'{']), TAG),
        Liveness
    );
    assert_eq!(
        admitting_step(&Message::Ping(Default::default()), TAG),
        Liveness
    );
    assert_eq!(
        admitting_step(&Message::Pong(Default::default()), TAG),
        Liveness
    );
}

#[test]
fn a_message_above_the_size_bound_is_a_protocol_end_and_the_bound_itself_is_read() {
    let padded = |length: usize| {
        let frame = json!({"msg_type": "control:hello", "code": 401}).to_string();
        format!("{frame}{}", " ".repeat(length - frame.len()))
    };
    for over in [K_MAX_MESSAGE + 1, K_MAX_MESSAGE * 2] {
        assert_eq!(
            admitting_step(&Message::text(padded(over)), TAG),
            AdmittingStep::Oversize
        );
        assert_eq!(
            admitting_step(&Message::binary(padded(over).into_bytes()), TAG),
            AdmittingStep::Oversize
        );
        // Oversize wins even when the bytes are not UTF-8.
        assert_eq!(
            admitting_step(&Message::binary(vec![0xff; over]), TAG),
            AdmittingStep::Oversize
        );
    }
    assert_eq!(
        admitting_step(&Message::text(padded(K_MAX_MESSAGE)), TAG),
        AdmittingStep::Classified(FailureClass::Refusal)
    );
}

#[test]
fn a_server_close_is_busy_for_1013_and_transport_otherwise() {
    let close = |code: Option<CloseCode>| {
        Message::Close(code.map(|code| CloseFrame {
            code,
            reason: "".into(),
        }))
    };
    assert_eq!(
        admitting_step(&close(Some(CloseCode::Again)), TAG),
        AdmittingStep::ServerClose(BUSY)
    );
    for code in [Some(CloseCode::Normal), Some(CloseCode::Away), None] {
        assert_eq!(
            admitting_step(&close(code), TAG),
            AdmittingStep::ServerClose(FailureClass::Transport)
        );
    }
}
