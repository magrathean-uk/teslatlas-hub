// SPDX-License-Identifier: AGPL-3.0-only

#[path = "interop/seed.rs"]
mod seed;

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use serde_json::Value;
use teslatlas_hub::{db::HubStore, server::paired_router};
use tower::ServiceExt;

#[tokio::test]
async fn fixture_has_two_vehicles_and_three_exact_drive_pages() {
    let scenario: Value = serde_json::from_str(include_str!("interop/scenario.json")).unwrap();
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("isolated");
    let prepared = seed::prepare(&root, 18443).unwrap();
    let store = HubStore::initialize(root.join("hub")).unwrap();
    let key =
        teslatlas_hub::teslamate_credentials::load_or_create_cursor_key(&root.join("hub")).unwrap();
    let app = paired_router(store, &key);
    let invitation: Value =
        serde_json::from_slice(&std::fs::read(&prepared.invitation_path).unwrap()).unwrap();
    let claim = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/v1/pairings/{}/claim",
                    invitation["pairing_id"].as_str().unwrap()
                ))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"secret":invitation["secret"],"device_name":"fixture-test"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(claim.status(), 200);
    let claim: Value =
        serde_json::from_slice(&claim.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let token = claim["access_token"].as_str().unwrap();
    let vehicles = get(&app, "/v1/vehicles", token).await;
    assert_eq!(vehicles["vehicles"], scenario["vehicles"]);
    let vehicle = "11111111-1111-4111-8111-111111111111";
    let current = get(&app, &format!("/v1/vehicles/{vehicle}/current"), token).await;
    for (field, expected) in scenario["current"].as_object().unwrap() {
        assert_eq!(&current[field], expected, "initial current field {field}");
    }
    assert_eq!(current["battery_level"], 0);
    assert_eq!(current["inside_temp"], 21.5);
    assert_eq!(current["outside_temp"], Value::Null);
    let empty = get(
        &app,
        "/v1/vehicles/22222222-2222-4222-8222-222222222222/current",
        token,
    )
    .await;
    assert_eq!(empty["observed_at_ms"], Value::Null);
    let page1 = get(
        &app,
        &format!("/v1/vehicles/{vehicle}/drives?limit=2"),
        token,
    )
    .await;
    assert_eq!(ids(&page1), vec![105, 104]);
    let page2 = get(
        &app,
        &format!(
            "/v1/vehicles/{vehicle}/drives?limit=2&cursor={}",
            page1["next_cursor"].as_str().unwrap()
        ),
        token,
    )
    .await;
    assert_eq!(ids(&page2), vec![103, 102]);
    let page3 = get(
        &app,
        &format!(
            "/v1/vehicles/{vehicle}/drives?limit=2&cursor={}",
            page2["next_cursor"].as_str().unwrap()
        ),
        token,
    )
    .await;
    assert_eq!(ids(&page3), vec![101]);
    assert_eq!(page3["next_cursor"], Value::Null);
    assert_eq!(page3["items"][0]["distance_km"], Value::Null);
    for page in [&page1, &page2, &page3] {
        for drive in page["items"].as_array().unwrap() {
            for (field, expected) in scenario["drives"][drive["id"].to_string()]
                .as_object()
                .unwrap()
            {
                assert_eq!(&drive[field], expected, "drive field {field}");
            }
        }
    }
    assert_eq!(
        page1["items"][0]["start_date_ms"],
        page1["items"][1]["start_date_ms"]
    );

    std::fs::write(
        root.join("scenario.json"),
        include_str!("interop/scenario.json"),
    )
    .unwrap();
    seed::advance(&root).unwrap();
    let updated = get(&app, &format!("/v1/vehicles/{vehicle}/current"), token).await;
    for (field, expected) in scenario["later_current"].as_object().unwrap() {
        assert_eq!(&updated[field], expected, "later current field {field}");
    }
    let receipt: Value =
        serde_json::from_slice(&std::fs::read(root.join("advance.json")).unwrap()).unwrap();
    assert_eq!(receipt["charge"], scenario["charge"]);
    assert!(
        seed::advance(&root).is_err(),
        "a later update is single use"
    );
    assert!(
        seed::prepare(&root, 18443).is_err(),
        "must never overwrite an existing fixture"
    );
}

#[test]
fn fixture_rejects_zero_port_without_creating_state() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("invalid");
    assert!(seed::prepare(&root, 0).is_err());
    assert!(!root.exists());
}

fn ids(page: &Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect()
}

async fn get(app: &axum::Router, route: &str, token: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(route)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "route {route}");
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
