// SPDX-License-Identifier: AGPL-3.0-only

// The geocoder's durable cache over real HTTP servers and a real SQLite
// store: a lookup is reused only for the same endpoint, language and
// coordinates, and every lookup keeps the answer it was given. Synthetic
// places and loopback endpoints only.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{Json, Router, extract::Query, http::HeaderMap, routing::get};

use super::*;

const LICENCE: &str = "Data © OpenStreetMap contributors, ODbL 1.0";

/// One request as the server saw it: latitude query value and language header.
type Seen = Arc<Mutex<Vec<(String, String)>>>;

/// A Nominatim-like server. Every place resolves to OSM way 42, so a lookup
/// near 51.5 ("Alpha") and one near -33.9 ("Beta") reach the same identity
/// under different names; the answer also names the server `label` and the
/// language the request asked for.
async fn place_server(label: &'static str) -> (String, Seen, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let seen_by_server = Arc::clone(&seen);
    let app = Router::new().route(
        "/reverse",
        get(
            move |Query(query): Query<HashMap<String, String>>, headers: HeaderMap| {
                let seen = Arc::clone(&seen_by_server);
                async move {
                    let latitude = query["lat"].clone();
                    let language = headers["accept-language"].to_str().unwrap().to_owned();
                    seen.lock()
                        .unwrap()
                        .push((latitude.clone(), language.clone()));
                    let place = if latitude.starts_with("51.5") {
                        "Alpha"
                    } else {
                        "Beta"
                    };
                    Json(serde_json::json!({
                        "osm_type": "way",
                        "osm_id": 42,
                        "licence": LICENCE,
                        "display_name": format!("{place} via {label} in {language}"),
                        "name": format!("{place} name"),
                        "lat": latitude,
                        "lon": query["lon"],
                        "address": {
                            "road": format!("{place} Road"),
                            "city": format!("{place} City"),
                            "country": "Testland"
                        }
                    }))
                }
            },
        ),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (endpoint, seen, task)
}

fn geocoder_for(endpoint: &str, language: &str) -> Geocoder {
    Geocoder::for_test(&GeocoderConfig {
        enabled: true,
        endpoint: Some(endpoint.to_owned()),
        language: language.to_owned(),
        timeout_seconds: 5,
    })
    .unwrap()
}

fn request_count(seen: &Seen) -> usize {
    seen.lock().unwrap().len()
}

/// provider, endpoint, endpoint host, language, display name, licence
type LookupRow = (String, String, String, String, String, Option<String>);

fn lookup_rows(store: &HubStore) -> Vec<LookupRow> {
    store
        .open()
        .unwrap()
        .prepare(
            "SELECT provider, endpoint, endpoint_host, language, display_name, licence
             FROM address_lookup_cache ORDER BY looked_up_at_ms, display_name",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn shared_names(store: &HubStore) -> Vec<(i64, String)> {
    store
        .open()
        .unwrap()
        .prepare("SELECT osm_id, display_name FROM address_cache ORDER BY osm_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[tokio::test]
async fn distant_lookups_reaching_one_osm_identity_keep_their_own_names_across_a_restart() {
    let (endpoint, seen, server) = place_server("one").await;
    let temp = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temp.path()).unwrap();
    let geocoder = geocoder_for(&endpoint, "en-GB");
    let near = Wgs84Point::new(51.5, -0.1).unwrap();
    let far = Wgs84Point::new(-33.9, 151.2).unwrap();

    let alpha = geocoder.reverse_cached(&store, near, 10).await.unwrap();
    let beta = geocoder.reverse_cached(&store, far, 20).await.unwrap();
    assert_eq!(alpha.display_name, "Alpha via one in en-GB");
    assert_eq!(beta.display_name, "Beta via one in en-GB");
    assert_eq!((alpha.osm_type.as_str(), alpha.osm_id), ("way", 42));
    assert_eq!((beta.osm_type.as_str(), beta.osm_id), ("way", 42));
    assert_eq!(alpha.name.as_deref(), Some("Alpha name"));
    assert_eq!(beta.name.as_deref(), Some("Beta name"));
    assert_eq!(alpha.licence.as_deref(), Some(LICENCE));
    assert_eq!(request_count(&seen), 2);

    // Both come back from the cache with their own answers, field for field.
    assert_eq!(
        geocoder.reverse_cached(&store, near, 30).await.unwrap(),
        alpha
    );
    assert_eq!(
        geocoder.reverse_cached(&store, far, 40).await.unwrap(),
        beta
    );
    assert_eq!(request_count(&seen), 2);

    // The shared identity row is the first answer's; the lookup rows carry
    // the provenance each was asked under.
    assert_eq!(shared_names(&store), vec![(42, alpha.display_name.clone())]);
    let provenance = |display_name: &str| {
        (
            "nominatim".to_owned(),
            format!("{endpoint}/"),
            "127.0.0.1".to_owned(),
            "en-GB".to_owned(),
            display_name.to_owned(),
            Some(LICENCE.to_owned()),
        )
    };
    assert_eq!(
        lookup_rows(&store),
        vec![
            provenance("Alpha via one in en-GB"),
            provenance("Beta via one in en-GB")
        ]
    );

    // Restart: the same answers, no new request.
    drop(store);
    let reopened = HubStore::initialize(temp.path()).unwrap();
    assert_eq!(
        geocoder.reverse_cached(&reopened, near, 50).await.unwrap(),
        alpha
    );
    assert_eq!(
        geocoder.reverse_cached(&reopened, far, 60).await.unwrap(),
        beta
    );
    assert_eq!(request_count(&seen), 2);
    assert_eq!(shared_names(&reopened), vec![(42, alpha.display_name)]);
    server.abort();
}

#[tokio::test]
async fn a_different_language_or_endpoint_misses_the_cache_and_never_rewrites_the_other_lookup() {
    let (endpoint_one, seen_one, server_one) = place_server("one").await;
    let (endpoint_two, seen_two, server_two) = place_server("two").await;
    let temp = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temp.path()).unwrap();
    let point = Wgs84Point::new(51.5, -0.1).unwrap();
    let english = geocoder_for(&endpoint_one, "en");
    let german = geocoder_for(&endpoint_one, "de");
    let elsewhere = geocoder_for(&endpoint_two, "en");

    let first_english = english.reverse_cached(&store, point, 10).await.unwrap();
    assert_eq!(first_english.display_name, "Alpha via one in en");
    assert_eq!(request_count(&seen_one), 1);

    // Another language at the same endpoint and place asks the provider.
    let first_german = german.reverse_cached(&store, point, 20).await.unwrap();
    assert_eq!(first_german.display_name, "Alpha via one in de");
    assert_eq!(request_count(&seen_one), 2);
    assert_eq!(seen_one.lock().unwrap()[1].1, "de");

    // So does another endpoint with the same language and place.
    let first_elsewhere = elsewhere.reverse_cached(&store, point, 30).await.unwrap();
    assert_eq!(first_elsewhere.display_name, "Alpha via two in en");
    assert_eq!(request_count(&seen_two), 1);

    // Each lookup is now cached under its own binding and still answers with
    // its own text: none of the later ones replaced an earlier one.
    assert_eq!(
        english.reverse_cached(&store, point, 40).await.unwrap(),
        first_english
    );
    assert_eq!(
        german.reverse_cached(&store, point, 50).await.unwrap(),
        first_german
    );
    assert_eq!(
        elsewhere.reverse_cached(&store, point, 60).await.unwrap(),
        first_elsewhere
    );
    assert_eq!((request_count(&seen_one), request_count(&seen_two)), (2, 1));

    // The endpoint is the normalised one, so spelling the same endpoint with
    // its trailing slash is the same lookup, not a miss.
    let slashed = geocoder_for(&format!("{endpoint_one}/"), "en");
    assert_eq!(slashed.lookup_binding(), english.lookup_binding());
    assert_eq!(
        slashed.reverse_cached(&store, point, 70).await.unwrap(),
        first_english
    );
    assert_eq!(request_count(&seen_one), 2);

    // Three lookup rows, one per binding, over the one shared identity.
    let rows = lookup_rows(&store);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter()
            .map(|row| (row.1.clone(), row.3.clone(), row.4.clone()))
            .collect::<Vec<_>>(),
        vec![
            (
                format!("{endpoint_one}/"),
                "en".to_owned(),
                "Alpha via one in en".to_owned()
            ),
            (
                format!("{endpoint_one}/"),
                "de".to_owned(),
                "Alpha via one in de".to_owned()
            ),
            (
                format!("{endpoint_two}/"),
                "en".to_owned(),
                "Alpha via two in en".to_owned()
            ),
        ]
    );
    assert_eq!(
        shared_names(&store),
        vec![(42, "Alpha via one in en".to_owned())]
    );
    server_one.abort();
    server_two.abort();
}

#[test]
fn parse_keeps_only_a_printable_bounded_licence() {
    let parse = |licence: &str| {
        let body = format!(
            r#"{{"osm_type":"node","osm_id":1,"display_name":"Place","licence":{licence}}}"#
        );
        parse_response(body.as_bytes()).unwrap().licence
    };
    assert_eq!(parse(r#""ODbL 1.0""#).as_deref(), Some("ODbL 1.0"));
    assert_eq!(parse(r#""  ODbL 1.0  ""#).as_deref(), Some("ODbL 1.0"));
    let longest = "l".repeat(512);
    assert_eq!(
        parse(&format!("\"{longest}\"")).as_deref(),
        Some(longest.as_str())
    );
    let too_long = "l".repeat(513);
    assert_eq!(parse(&format!("\"{too_long}\"")), None);
    assert_eq!(parse(r#""ODbL\n1.0""#), None);
    assert_eq!(parse(r#""ODbL\u0007""#), None);
    assert_eq!(parse(r#""   ""#), None);
    assert_eq!(parse("42"), None);
    assert_eq!(parse("null"), None);
    let absent =
        parse_response(br#"{"osm_type":"node","osm_id":1,"display_name":"Place"}"#).unwrap();
    assert_eq!(absent.licence, None);
}
