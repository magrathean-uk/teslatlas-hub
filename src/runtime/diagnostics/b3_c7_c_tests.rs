// SPDX-License-Identifier: AGPL-3.0-only

// C7 part C doctor test (spec section 7.5, C7 T73): the `collector.geocoder`
// member. Temporary stores, hand-written records, no network. The member is
// checked through `inspect_hub` with a record that is far from its boundary,
// and at fixed evaluation times through `geocoder_diagnostics`, which takes the
// time explicitly.

use super::{doctor_config, *};
use serde_json::{Value, json};

/// The record of C7 T73 (b): two refusals of the public host.
fn public_record(since_ms: i64, until_ms: i64) -> String {
    format!(
        r#"{{"v":1,"endpoint":"https://nominatim.openstreetmap.org:443/","host":"nominatim.openstreetmap.org","status":429,"refusals":2,"since_ms":{since_ms},"until_ms":{until_ms}}}"#
    )
}

fn geocoder_member(report: &HubDoctorReport) -> Value {
    serde_json::to_value(&report.collector.geocoder).expect("JSON")
}

#[test]
fn c7_t73_doctor() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let init_only = "interval_seconds = 0";
    let default_config = doctor_config(temporary.path(), init_only, "");

    // (a) The default configuration and no record.
    let plain = inspect_hub(&store, &default_config).expect("doctor report");
    assert_eq!(
        geocoder_member(&plain),
        json!({
            "enabled": true,
            "endpointHost": "nominatim.openstreetmap.org",
            "publicHost": true,
            "policyUrl": "https://operations.osmfoundation.org/policies/nominatim/",
            "state": "on",
            "refusedStatus": null,
            "refusals": null,
            "backingOffSinceMs": null,
            "retryAtMs": null,
        })
    );
    let whole = serde_json::to_value(&plain).expect("JSON");
    assert_eq!(whole["collector"]["geocoderEnabled"], json!(true));
    assert_eq!(whole["collector"]["geocoder"], geocoder_member(&plain));
    assert_eq!(stored(&store), None);

    // (b) A record for the public host that ends an hour from now.
    let now = diagnostic_epoch_ms();
    let written = public_record(now - 60_000, now + 3_600_000);
    store
        .set_geocoder_provider_state(Some(&written))
        .expect("record");
    let refused = inspect_hub(&store, &default_config).expect("doctor report");
    assert_eq!(
        geocoder_member(&refused),
        json!({
            "enabled": true,
            "endpointHost": "nominatim.openstreetmap.org",
            "publicHost": true,
            "policyUrl": "https://operations.osmfoundation.org/policies/nominatim/",
            "state": "backingOff",
            "refusedStatus": 429,
            "refusals": 2,
            "backingOffSinceMs": now - 60_000,
            "retryAtMs": now + 3_600_000,
        })
    );
    // The back-off changes neither the success of the report, nor its checks,
    // nor the collector's readiness.
    assert_eq!(refused.status, plain.status);
    assert_eq!(refused.checks, plain.checks);
    assert_eq!(refused.collector.readiness, plain.collector.readiness);
    assert_eq!(
        refused.collector.readiness_detail,
        plain.collector.readiness_detail
    );
    assert_eq!(
        refused.collector.collection_attention,
        plain.collector.collection_attention
    );
    assert_eq!(stored(&store).as_deref(), Some(written.as_str()));

    // (c) The same record with lookups switched off.
    let off = doctor_config(temporary.path(), init_only, "[geocoder]\nenabled = false\n");
    let report = inspect_hub(&store, &off).expect("doctor report");
    let member = geocoder_member(&report);
    assert_eq!(member["state"], json!("off"));
    assert_eq!(member["enabled"], json!(false));
    assert_eq!(member["endpointHost"], json!("nominatim.openstreetmap.org"));
    assert_eq!(member["refusedStatus"], json!(429));
    assert_eq!(member["refusals"], json!(2));
    assert_eq!(member["backingOffSinceMs"], json!(now - 60_000));
    assert_eq!(member["retryAtMs"], json!(now + 3_600_000));
    assert_eq!(report.status, plain.status);
    assert_eq!(stored(&store).as_deref(), Some(written.as_str()));

    // (d) Another endpoint is not paused by the public host's record.
    let other = doctor_config(
        temporary.path(),
        init_only,
        "[geocoder]\nendpoint = \"https://geo.example.test/\"\n",
    );
    let report = inspect_hub(&store, &other).expect("doctor report");
    assert_eq!(
        geocoder_member(&report),
        json!({
            "enabled": true,
            "endpointHost": "geo.example.test",
            "publicHost": false,
            "policyUrl": null,
            "state": "on",
            "refusedStatus": null,
            "refusals": null,
            "backingOffSinceMs": null,
            "retryAtMs": null,
        })
    );
    assert_eq!(report.status, plain.status);
    assert_eq!(stored(&store).as_deref(), Some(written.as_str()));

    // The exact boundaries at fixed evaluation times: the pause is in force one
    // millisecond before its end, and not at its end, when the record is still
    // shown; a pause longer than 24 h past the time counts as damaged.
    let fixed = 1_000_000;
    for (until_ms, at_ms, state) in [
        (fixed + 1, fixed, "backingOff"),
        (fixed, fixed, "on"),
        (fixed + 86_400_000, fixed, "backingOff"),
        (fixed + 86_400_001, fixed, "on"),
    ] {
        let text = public_record(fixed - 60_000, until_ms);
        store
            .set_geocoder_provider_state(Some(&text))
            .expect("record");
        let member = geocoder_diagnostics(&store, &default_config, at_ms);
        assert_eq!(member.state, state, "until {until_ms} at {at_ms}");
        assert_eq!(member.retry_at_ms, Some(until_ms));
        assert_eq!(stored(&store).as_deref(), Some(text.as_str()));
    }
    // An invalid record shows as no record.
    store
        .set_geocoder_provider_state(Some("not json"))
        .expect("record");
    let member = geocoder_diagnostics(&store, &default_config, fixed);
    assert_eq!((member.state.as_str(), member.refusals), ("on", None));
    assert_eq!(stored(&store).as_deref(), Some("not json"));
}

/// The stored provider-state text.
fn stored(store: &HubStore) -> Option<String> {
    store.geocoder_provider_state().expect("the row reads")
}
