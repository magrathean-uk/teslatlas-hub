// SPDX-License-Identifier: AGPL-3.0-only

//! C6 Part C2 over HTTP: nothing of the collection minor in C6's minor
//! (C6 ST-S54, C6 R75) and the served history items against the new minor's
//! item schemas and the 1.2.0 ones (C6 ST-S46, the routes half). Synthetic data
//! in temporary stores.

use super::b3_c6_c1::{get_bytes, insert_all, integer, only_item, text};
use super::*;
use crate::api::public_query::b3_c6_c1::{
    charge_distinct, charge_null, drive_distinct, drive_null, position_distinct, position_null,
    sample_distinct, sample_null, vehicle,
};
use crate::api::public_query::b3_c6_c2::{item_problems, profile_document};
use crate::config::CollectorMode;
use crate::db::{CollectorAccountRow, CollectorRowsWrite, ReadinessContext};
use crate::diagnostics::inspect_hub;

const NOW_MS: i64 = 1_800_000_000_000;

/// A configuration file in the store directory, as the managed service reads it
/// (the doctor tests' `doctor_config`): collection required (`interval_seconds`
/// above zero) with the given collector settings.
fn managed_config(directory: &std::path::Path, collector: &str) -> HubConfig {
    let path = directory.join("config.toml");
    fs::write(
        &path,
        format!(
            "data_dir = {directory:?}\nbind = '127.0.0.1:18443'\n\n[collector]\ninterval_seconds = 300\n{collector}\n"
        ),
    )
    .expect("write config");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private config");
    HubConfig::load(&path).expect("config")
}

/// One `/readyz` answer from the router the managed service builds from its
/// loaded configuration (`serve` hands the TLS listener
/// `tls_router_for_config(.., interval_seconds > 0, .., config, ..)`): its status,
/// cache header and exact body bytes. Each call builds a fresh router, so no
/// readiness answer is cached across states.
async fn ready_bytes(store: &HubStore, config: &HubConfig) -> (StatusCode, String, Vec<u8>) {
    let app = tls_router_for_config(
        store.clone(),
        config.collector.interval_seconds > 0,
        &CursorKey::from_bytes([54; 32]),
        Sha256Digest::of_bytes(b"configuration digest"),
        config,
        None,
    );
    let (status, headers, body) = get_bytes(&app, "/readyz").await;
    (
        status,
        headers[header::CACHE_CONTROL].to_str().unwrap().to_owned(),
        body,
    )
}

fn cap_zero_account() -> CollectorAccountRow {
    CollectorAccountRow {
        mode: "legacy".to_owned(),
        config_fingerprint: "a".repeat(64),
        state: "cap_reached".to_owned(),
        reason_code: "cap_zero".to_owned(),
        since_ms: NOW_MS,
        rate_limited_until_ms: None,
        next_probe_ms: None,
    }
}

#[tokio::test]
async fn c6_st_s54_no_collection_minor() {
    // (a) Against the base minor, this minor adds no resource, route, `$defs`
    // entry or capability for the collection object or the gaps resource
    // (`collection_enabled` is a settings key, not such an entry), and every
    // entry that describes `/readyz` equals the base's as a JSON value.
    let (new_openapi, base_openapi) = (
        profile_document("1.3.0", "openapi.json"),
        profile_document("1.2.0", "openapi.json"),
    );
    assert_eq!(
        new_openapi["paths"]["/readyz"],
        base_openapi["paths"]["/readyz"]
    );
    assert_eq!(
        new_openapi["paths"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        base_openapi["paths"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        "no route is added"
    );
    let (new_resources, base_resources) = (
        profile_document("1.3.0", "resources.schema.json"),
        profile_document("1.2.0", "resources.schema.json"),
    );
    assert_eq!(
        new_resources["$defs"]["ready"],
        base_resources["$defs"]["ready"]
    );
    assert_eq!(
        new_resources["$defs"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        base_resources["$defs"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        "no $defs entry is added"
    );
    assert!(
        new_resources["$defs"]["current"]["properties"]
            .get("collection")
            .is_none(),
        "assertion failed: c6_st_s54_no_collection_minor: current schema has no collection object"
    );
    assert_eq!(
        profile_document("1.3.0", "examples/ready.json"),
        profile_document("1.2.0", "examples/ready.json")
    );
    let find = |document: &serde_json::Value, id: &str| {
        document["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["id"] == id)
            .cloned()
            .expect("case")
    };
    let (new_cases, base_cases) = (
        profile_document("1.3.0", "cases.json"),
        profile_document("1.2.0", "cases.json"),
    );
    assert_eq!(
        find(&new_cases, "ready-success"),
        find(&base_cases, "ready-success")
    );
    let (new_profile, base_profile) = (
        profile_document("1.3.0", "profile.json"),
        profile_document("1.2.0", "profile.json"),
    );
    assert_eq!(new_profile["capabilities"], base_profile["capabilities"]);
    assert_eq!(
        new_profile["routes"].as_array().unwrap().len(),
        base_profile["routes"].as_array().unwrap().len()
    );
    let ready_route = |profile: &serde_json::Value| {
        profile["routes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|route| route["kind"] == "ready")
            .cloned()
            .expect("ready route")
    };
    assert_eq!(ready_route(&new_profile), ready_route(&base_profile));
    let text = |name: &str| {
        std::fs::read_to_string(crate::api::public_query::b3_c6_c2::profile_file(
            "1.3.0", name,
        ))
        .unwrap()
    };
    for name in [
        "profile.json",
        "openapi.json",
        "resources.schema.json",
        "cases.json",
        "discovery.schema.json",
    ] {
        let file = text(name);
        for word in [
            "collectorCadenceError",
            "collector_cadence_invalid",
            "/gaps",
            "\"detail\"",
        ] {
            assert!(!file.contains(word), "{name} does not hold {word}");
        }
    }

    // (b) The four states over HTTP, from the router the managed service builds
    // from its loaded configuration. Each answer is exactly the 1.2.0 answer of
    // the Hub before this change (the same four answers were read from the
    // `hub@6aba8c4` build): the status, no-store, and the body bytes with no
    // member beyond `status` (and `reason` when not ready).
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    // A store that holds a vehicle: the status reader treats an empty store as a
    // Hub that was only initialised and requires no collection of it.
    seed_active_vehicle_identity(
        &store,
        Uuid::from_u128(0x6c54),
        Uuid::from_u128(0x6c6c_0054_0000_4000_8000_0000_0000_0001),
    );
    let valid = managed_config(temporary.path(), "");
    assert_eq!(valid.collector.cadence_violation(), None);
    assert_eq!(
        crate::config::collector_mode(&valid.collector),
        CollectorMode::Legacy
    );
    let expect_ready = |(status, cache, body): (StatusCode, String, Vec<u8>), label: &str| {
        assert_eq!(status, StatusCode::OK, "{label}");
        assert_eq!(cache, "no-store", "{label}");
        assert_eq!(
            String::from_utf8(body).expect("UTF-8 body"),
            r#"{"status":"ready"}"#,
            "{label}"
        );
    };
    let now_ms = || current_epoch_ms().expect("system clock");

    // The first state: ready, under a fresh supervised lease.
    let lease = store
        .acquire_supervised_collector_lease(now_ms())
        .expect("lease");
    // Each answer is read right after a heartbeat of the lease, so no step of this
    // test depends on how long an earlier step took (the lease lasts 30 seconds).
    let renew = || {
        store
            .heartbeat_supervised_collector_lease(lease, SupervisedCollectorState::Active, now_ms())
            .expect("heartbeat");
    };
    renew();
    expect_ready(ready_bytes(&store, &valid).await, "ready");
    let clean = inspect_hub(&store, &valid).expect("status");
    assert_eq!(clean.collector.collection_attention, None);
    assert_eq!(clean.collector.cadence_error, None);

    // The second: ready with a collection attention (a local monthly cap of 0
    // counts as reached): the body is the same, and the status reader reports
    // the attention.
    store
        .collector_write_transaction(
            |transaction| {
                let wrote = HubStore::write_collector_rows_in_transaction(
                    transaction,
                    &CollectorRowsWrite {
                        now_ms: NOW_MS,
                        account: Some(cap_zero_account()),
                        ..CollectorRowsWrite::default()
                    },
                )?;
                Ok::<_, crate::db::StoreError>(((), wrote))
            },
            || Ok(()),
        )
        .expect("cap-zero rows");
    let attention = store.collector_readiness_view(&ReadinessContext::configured(
        true,
        now_ms(),
        CollectorMode::Legacy,
        None,
    ));
    assert_eq!(
        attention.attention,
        Some(crate::db::CollectionAttentionCode::CollectorCapReached),
        "the local view does report the attention"
    );
    renew();
    expect_ready(
        ready_bytes(&store, &valid).await,
        "ready with cap-zero attention",
    );

    // The fourth: the first state's store and configuration, which are
    // otherwise eligible to collect, except one collector cadence setting. The
    // second state's account row is removed first, so the store is again the
    // first state's shape (no account row, the same lease), and the status
    // reader confirms it with the valid configuration before the cadence is
    // changed. Then an invalid cadence in the active mode, under the managed
    // service: a charging interval of 0 under Legacy, in the configuration the
    // service loads (a file that still loads; the service reports the failure
    // instead of refusing to start). First the local status, run with the same
    // configuration, reports the key, value and rule and the attention
    // `collector_cadence_invalid`; then the HTTP answer is the same ready body,
    // with no detail and no cadence member.
    store
        .open()
        .expect("catalogue")
        .execute("DELETE FROM collector_account_state", [])
        .expect("remove the second state's account row");
    renew();
    let restored = inspect_hub(&store, &valid).expect("status");
    assert_eq!(
        restored.collector.collection_attention, None,
        "the first state's shape: no attention"
    );
    assert_eq!(restored.collector.cadence_error, None);
    expect_ready(
        ready_bytes(&store, &valid).await,
        "ready again in the first state's shape",
    );
    let invalid = managed_config(temporary.path(), "charging_poll_seconds = 0");
    assert_eq!(
        crate::config::collector_mode(&invalid.collector),
        CollectorMode::Legacy
    );
    let violation = invalid.collector.cadence_violation().expect("a violation");
    assert_eq!(
        violation.to_string(),
        "collector setting collector.charging_poll_seconds = 0 must be greater than zero"
    );
    renew();
    let status = inspect_hub(&store, &invalid).expect("status");
    assert_eq!(
        status.collector.collection_attention.as_deref(),
        Some("collector_cadence_invalid")
    );
    let reported = status
        .collector
        .cadence_error
        .as_ref()
        .expect("cadence error");
    assert_eq!(
        (reported.key, reported.value, reported.rule),
        ("collector.charging_poll_seconds", 0, "greater_than_zero")
    );
    assert_eq!(
        status.collector.readiness, "ready",
        "the wait lease is fresh"
    );
    renew();
    let (status_code, cache, body) = ready_bytes(&store, &invalid).await;
    expect_ready(
        (status_code, cache, body.clone()),
        "managed invalid cadence",
    );
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed.get("detail").is_none());
    assert!(parsed.get("collectorCadenceError").is_none());
    assert_eq!(
        parsed.as_object().unwrap().len(),
        1,
        "no member beyond status"
    );

    // The third: not ready, no lease: `collector_absent`, for either
    // configuration.
    store
        .release_supervised_collector_lease(lease)
        .expect("release");
    for config in [&valid, &invalid] {
        let (status, cache, body) = ready_bytes(&store, config).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(cache, "no-store");
        assert_eq!(
            String::from_utf8(body).expect("UTF-8 body"),
            r#"{"status":"not_ready","reason":"collector_absent"}"#
        );
    }
}

#[tokio::test]
async fn c6_st_s46_served_items() {
    // The ST-S45 fixtures, the distinct non-null and the all-null row of each of
    // the four items, stored as rows and served through their routes: each served
    // item is the serialiser's golden bytes of its fixture, is valid against this
    // minor's item schema and against 1.2.0's, and holds every alias beside its
    // replacement.
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let registered = Uuid::from_u128(0x6c6c_0046_0000_4000_8000_0000_0000_0001);
    seed_active_vehicle_identity(&store, Uuid::from_u128(0x6c46), registered);
    mark_vehicle_published(&store, registered);
    let id = registered.to_string();
    fn stored(row: &impl serde::Serialize) -> rusqlite::types::Value {
        text(serde_json::to_string(row).expect("row"))
    }
    insert_all(
        &store,
        "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
         VALUES (?1, ?2, 1, ?3)",
        vec![
            vec![text(&id), integer(11), stored(&drive_distinct())],
            vec![text(&id), integer(12), stored(&drive_null())],
        ],
    );
    insert_all(
        &store,
        "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
         VALUES (?1, ?2, 1, ?3)",
        vec![
            vec![text(&id), integer(21), stored(&charge_distinct())],
            vec![text(&id), integer(22), stored(&charge_null())],
        ],
    );
    insert_all(
        &store,
        "INSERT INTO materialised_positions(
             vehicle_id, position_id, drive_id, car_id, position_json
         ) VALUES (?1, ?2, ?3, 1, ?4)",
        vec![
            vec![
                text(&id),
                integer(31),
                integer(11),
                stored(&position_distinct()),
            ],
            vec![
                text(&id),
                integer(32),
                integer(12),
                stored(&position_null()),
            ],
        ],
    );
    insert_all(
        &store,
        "INSERT INTO materialised_charge_samples(
             vehicle_id, sample_id, charge_id, sample_json
         ) VALUES (?1, ?2, ?3, ?4)",
        vec![
            vec![
                text(&id),
                integer(41),
                integer(21),
                stored(&sample_distinct()),
            ],
            vec![text(&id), integer(42), integer(22), stored(&sample_null())],
        ],
    );
    // The distinct position carries terrain provenance through its lookup row.
    insert_all(
        &store,
        "INSERT INTO terrain_elevation_provenance(
             vehicle_id, position_id, drive_id, latitude, longitude, elevation_m,
             tile_name, tile_hash, dataset_source, dataset_version, status,
             error_code, attempts, attempted_at_ms, retry_after_ms
         ) VALUES (?1, 31, 11, 47.375, 19.625, NULL, 'tile-83', NULL,
                   'Terrain dataset 81', 'v82', 'success', NULL, 1, 1000, 0)",
        vec![vec![text(&id)]],
    );
    let app = public_query_router(store, 46);
    let fixtures = crate::api::public_query::b3_c6_c1::cases();
    assert_eq!(fixtures.len(), 8, "four items, two fixtures each");
    for case in fixtures {
        // The distinct fixture rows are drive 11 and charge 21, the null ones 12 and 22.
        let (drive, charge) = match case.fixture {
            "distinct" => (11, 21),
            _ => (12, 22),
        };
        let (uri, paged) = match case.item {
            "drive" => (format!("/v1/vehicles/{registered}/drives/{drive}"), false),
            "charge" => (format!("/v1/vehicles/{registered}/charges/{charge}"), false),
            "position" => (
                format!("/v1/vehicles/{registered}/drives/{drive}/positions?limit=1"),
                true,
            ),
            _ => (
                format!("/v1/vehicles/{registered}/charges/{charge}/samples?limit=1"),
                true,
            ),
        };
        let label = format!("{} ({})", case.item, case.fixture);
        let (status, _, bytes) = get_bytes(&app, &uri).await;
        assert_eq!(status, StatusCode::OK, "{label}");
        let served = if paged { only_item(&bytes) } else { bytes };
        // The golden bytes of the fixture, with the registered vehicle's id in
        // place of the fixture's (the same length, so no other byte moves).
        let golden = String::from_utf8(case.bytes.clone())
            .expect("fixture text")
            .replace(&vehicle().to_string(), &id);
        assert_eq!(
            String::from_utf8(served.clone()).expect("served text"),
            golden,
            "{label}: the served bytes are the serialiser's golden bytes"
        );
        let item: serde_json::Value = serde_json::from_slice(&served).expect("JSON");
        for minor in ["1.3.0", "1.2.0"] {
            assert!(
                item_problems(minor, case.item, &item).is_empty(),
                "assertion failed: c6_st_s46_served_items: served item validates against the requested profile minor; item={label} minor={minor} problems={:?}",
                item_problems(minor, case.item, &item)
            );
        }
        // The aliases of this item agree with their replacements on the wire.
        for (kind, old, new) in crate::api::public_query::b3_c6_c2::history_pairs() {
            if kind == case.item {
                assert_eq!(item[old], item[new], "{label}: {old} equals {new}");
                assert!(item.get(new).is_some(), "{label}.{new} is served");
            }
        }
    }
}
