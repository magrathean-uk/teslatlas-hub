// SPDX-License-Identifier: AGPL-3.0-only

//! Per-device vehicle grants on every vehicle-scoped route and the pack route.
//! Real SQLite, the real paired router (device authentication on), synthetic
//! data only. A nested module of the history tests: it reuses their fixtures.

use super::*;
use crate::db::{PairingScope, VehicleScope};

#[path = "device_credentials.rs"]
mod device_credentials;

const GRANT_KEY_BYTE: u8 = 83;

fn grant_key() -> CursorKey {
    CursorKey::from_bytes([GRANT_KEY_BYTE; 32])
}

struct PublishedPack {
    digest: Sha256Digest,
    bytes: Vec<u8>,
}

/// A catalogued schema-1.0 full snapshot with one real pack for `vehicle_id`;
/// `latitude` makes each vehicle's pack bytes (and so its digest) different.
fn publish_transport_vehicle(store: &HubStore, vehicle_id: Uuid, latitude: f64) -> PublishedPack {
    let installation_id = Uuid::new_v4();
    let account_id = Uuid::new_v4();
    let snapshot_id = Uuid::new_v4();
    let rows = vec![TransportRow {
        table: MirrorTable::Position,
        entity_key: "position:1".to_owned(),
        source_sequence: 5,
        operation: TransportOperation::Upsert,
        values: BTreeMap::from([
            ("latitude".to_owned(), TransportValue::Real(latitude)),
            ("longitude".to_owned(), TransportValue::Real(0.5)),
        ]),
    }];
    let tables = [MirrorTable::Position];
    let built = TransportPackWriter::new(store.packs_dir())
        .write_pack(&TransportPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id,
            ordinal: 0,
            schema: TRANSPORT_SCHEMA_V1,
            mode: TransferMode::FullSnapshot,
            sequence: SequenceRange {
                from_exclusive: 5,
                to_inclusive: 5,
            },
            tables: &tables,
            rows: &rows,
        })
        .expect("transport pack");
    let cursor = OpaqueCursor::issue(
        &grant_key(),
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: TRANSPORT_SCHEMA_V1,
            installation_id,
            account_id,
            vehicle_id,
            generation: 1,
            sequence: 5,
        },
    )
    .expect("cursor");
    store
        .publish_manifest(&SyncManifest {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: TRANSPORT_SCHEMA_V1,
            installation_id,
            account_id,
            vehicle_id,
            generation: 1,
            snapshot_id,
            mode: TransferMode::FullSnapshot,
            base_sequence: 5,
            head_sequence: 5,
            chunk_count: 1,
            total_compressed_bytes: built.metadata.compressed_bytes,
            total_uncompressed_bytes: built.metadata.uncompressed_bytes,
            total_rows: built.metadata.row_count,
            chunks: vec![built.metadata.clone()],
            terminal_cursor: cursor,
        })
        .expect("publish manifest");
    PublishedPack {
        digest: built.metadata.sha256,
        bytes: fs::read(&built.path).expect("pack bytes"),
    }
}

/// One vehicle with a current observation, one closed drive with a position
/// and one closed charge with a sample.
fn seed_granted_vehicle(store: &HubStore, source_key: &str, eid: i64) -> Uuid {
    let source = store
        .register_source(&SourceDescriptor::new("owner_api", source_key), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, eid.to_string())
                .with_tesla_identity(Some(eid), None),
            1_000,
        )
        .expect("vehicle");
    store
        .accept_owner_observation_and_lifecycle(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                observed_at_ms: 2_000,
                payload: serde_json::json!({
                    "record_type": "owner_api_vehicle_data_v1",
                    "source_vehicle_id": eid.to_string(),
                    "source_vehicle_state": "online",
                    "vehicle_data": {
                        "charge_state": {"battery_level": 80, "charging_state": "Disconnected"}
                    }
                }),
            },
            2_001,
            1,
        )
        .expect("current observation");
    seed_public_drive(store, vehicle.vehicle_id, &public_drive_fixture(10, 1_000));
    insert_position(store, vehicle.vehicle_id, &position_fixture(101, 10, 1_000));
    insert_charge(
        store,
        vehicle.vehicle_id,
        &charge_fixture(20, 1_000, Some(1_500)),
    );
    insert_sample(store, vehicle.vehicle_id, &sample_fixture(201, 20, 1_000));
    vehicle.vehicle_id
}

/// Vehicle `x` and `y` are active, published (a real transport manifest and
/// pack each) and hold history; `z` is active but has no manifest.
struct Grants {
    _temp: tempfile::TempDir,
    store: HubStore,
    app: Router,
    x: Uuid,
    y: Uuid,
    z: Uuid,
    pack_x: PublishedPack,
    pack_y: PublishedPack,
}

impl Grants {
    fn new() -> Self {
        let temp = crate::private_tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store");
        let x = seed_granted_vehicle(&store, "grants-x", 1);
        let y = seed_granted_vehicle(&store, "grants-y", 2);
        let z = {
            let source = store
                .register_source(&SourceDescriptor::new("owner_api", "grants-z"), 1_000)
                .expect("source");
            store
                .register_vehicle(&VehicleDescriptor::new(source.source_id, "3"), 1_000)
                .expect("vehicle")
                .vehicle_id
        };
        let pack_x = publish_transport_vehicle(&store, x, 10.5);
        let pack_y = publish_transport_vehicle(&store, y, 20.5);
        assert_ne!(pack_x.digest, pack_y.digest);
        let app = paired_router(store.clone(), &grant_key());
        Self {
            _temp: temp,
            store,
            app,
            x,
            y,
            z,
            pack_x,
            pack_y,
        }
    }

    /// A paired device granted exactly `vehicles` (and none for an empty
    /// list): its id and bearer.
    fn device(&self, vehicles: &[Uuid]) -> (Uuid, String) {
        let now_ms = current_epoch_ms().expect("pairing clock");
        let scope = if vehicles.is_empty() {
            PairingScope::AllVehicles
        } else {
            PairingScope::Vehicles(vehicles.to_vec())
        };
        let invitation = self
            .store
            .create_pairing_with_scope("grants test", now_ms - 1, i64::MAX, &scope)
            .expect("invitation");
        let access = self
            .store
            .claim_pairing(invitation.pairing_id, invitation.secret(), "client", now_ms)
            .expect("claim");
        if vehicles.is_empty() {
            self.store
                .open()
                .unwrap()
                .execute(
                    "DELETE FROM device_vehicle_grants WHERE device_id = ?1",
                    [access.device_id.to_string()],
                )
                .unwrap();
        }
        (access.device_id, access.access_token.as_bearer().to_owned())
    }

    async fn send(&self, route: &Route, vehicle: &str, bearer: Option<&str>) -> Response {
        send(
            &self.app,
            route.method,
            &route.uri(vehicle),
            bearer,
            route.body(),
        )
        .await
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    bearer: Option<&str>,
    body: Option<serde_json::Value>,
) -> Response {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let body = match body {
        Some(value) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).expect("request JSON"))
        }
        None => Body::empty(),
    };
    app.clone()
        .oneshot(request.body(body).expect("request"))
        .await
        .expect("in-memory response")
}

/// What a client can observe of a response: status, every header but the
/// per-request id, and the body.
#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    status: StatusCode,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}

async fn fingerprint(response: Response) -> Fingerprint {
    let status = response.status();
    let mut headers: Vec<(String, Vec<u8>)> = response
        .headers()
        .iter()
        .filter(|(name, _)| name.as_str() != "x-request-id")
        .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
        .collect();
    headers.sort();
    Fingerprint {
        status,
        headers,
        body: body_bytes(response).await,
    }
}

/// One vehicle-scoped route.
struct Route {
    label: &'static str,
    method: &'static str,
    /// The path with `{v}` where the vehicle id goes.
    template: &'static str,
    /// What the vehicle's own granted device sees.
    own_status: StatusCode,
}

impl Route {
    fn uri(&self, vehicle: &str) -> String {
        self.template.replace("{v}", vehicle)
    }

    fn body(&self) -> Option<serde_json::Value> {
        (self.method == "POST").then(|| {
            serde_json::json!({
                "base_receipt_id": "0".repeat(64),
                "base_manifest_schema": "2.2",
                "from_sequence": 0,
                "schema_version_range": {"minimum": "2.2", "maximum": "2.2"}
            })
        })
    }
}

fn vehicle_routes() -> Vec<Route> {
    let ok = StatusCode::OK;
    vec![
        Route {
            label: "current",
            method: "GET",
            template: "/v1/vehicles/{v}/current",
            own_status: ok,
        },
        Route {
            label: "drives",
            method: "GET",
            template: "/v1/vehicles/{v}/drives",
            own_status: ok,
        },
        Route {
            label: "drive",
            method: "GET",
            template: "/v1/vehicles/{v}/drives/10",
            own_status: ok,
        },
        Route {
            label: "positions",
            method: "GET",
            template: "/v1/vehicles/{v}/drives/10/positions",
            own_status: ok,
        },
        Route {
            label: "charges",
            method: "GET",
            template: "/v1/vehicles/{v}/charges",
            own_status: ok,
        },
        Route {
            label: "charge",
            method: "GET",
            template: "/v1/vehicles/{v}/charges/20",
            own_status: ok,
        },
        Route {
            label: "samples",
            method: "GET",
            template: "/v1/vehicles/{v}/charges/20/samples",
            own_status: ok,
        },
        Route {
            label: "manifest",
            method: "GET",
            template: "/v1/vehicles/{v}/sync/manifest",
            own_status: ok,
        },
        Route {
            label: "noop",
            method: "GET",
            template: "/v1/vehicles/{v}/sync/noop",
            own_status: StatusCode::NOT_ACCEPTABLE,
        },
        Route {
            label: "signing keys",
            method: "GET",
            template: "/v1/vehicles/{v}/sync/signing-keys",
            own_status: ok,
        },
        Route {
            label: "prepared artefact",
            method: "GET",
            // A well-formed artefact id that names nothing: a granted vehicle
            // answers the empty 404 of an unknown artefact.
            template: "/v1/vehicles/{v}/sync/prepared-artefacts/map-month-v1.00000000000000000000000000000000000000000000000000000000000000aa",
            own_status: StatusCode::NOT_FOUND,
        },
        Route {
            label: "changes since",
            method: "POST",
            template: "/v1/vehicles/{v}/sync/changes-since",
            own_status: StatusCode::NOT_ACCEPTABLE,
        },
    ]
}

fn pack_uri(digest: impl std::fmt::Display) -> String {
    format!("/v1/packs/sha256/{digest}.sqlite.zst")
}

#[tokio::test]
async fn an_ungranted_vehicle_is_answered_exactly_as_an_unknown_one_on_every_route() {
    let grants = Grants::new();
    let (_, bearer_x) = grants.device(&[grants.x]);
    for route in vehicle_routes() {
        let own = fingerprint(
            grants
                .send(&route, &grants.x.to_string(), Some(&bearer_x))
                .await,
        )
        .await;
        assert_eq!(
            own.status, route.own_status,
            "{}: granted vehicle",
            route.label
        );

        let ungranted = fingerprint(
            grants
                .send(&route, &grants.y.to_string(), Some(&bearer_x))
                .await,
        )
        .await;
        let unknown = fingerprint(
            grants
                .send(&route, &Uuid::new_v4().to_string(), Some(&bearer_x))
                .await,
        )
        .await;
        let nil = fingerprint(
            grants
                .send(&route, &Uuid::nil().to_string(), Some(&bearer_x))
                .await,
        )
        .await;
        // z is active but has no published snapshot, and is not granted to
        // this device. A granted device gets the empty `no_full_snapshot` 404
        // from the sync routes (not the unknown vehicle's JSON error), so an
        // ungranted device must not be able to tell it from an unknown one.
        let ungranted_unpublished = fingerprint(
            grants
                .send(&route, &grants.z.to_string(), Some(&bearer_x))
                .await,
        )
        .await;
        assert_ne!(
            own, unknown,
            "{}: a granted vehicle is told apart from an unknown one",
            route.label
        );
        assert_eq!(ungranted.status, StatusCode::NOT_FOUND, "{}", route.label);
        assert_eq!(
            ungranted, unknown,
            "{}: an ungranted vehicle differs from an unknown one",
            route.label
        );
        assert_eq!(
            ungranted_unpublished, unknown,
            "{}: an ungranted, active, unpublished vehicle differs from an unknown one",
            route.label
        );
        assert_eq!(
            nil, unknown,
            "{}: the nil vehicle is an unknown vehicle, never a 503",
            route.label
        );
    }
}

#[tokio::test]
async fn each_device_reads_only_its_own_vehicle() {
    let grants = Grants::new();
    let (_, bearer_x) = grants.device(&[grants.x]);
    let (_, bearer_y) = grants.device(&[grants.y]);
    let (_, bearer_all) = grants.device(&[grants.x, grants.y]);
    for route in vehicle_routes() {
        for (bearer, own, other) in [
            (&bearer_x, grants.x, grants.y),
            (&bearer_y, grants.y, grants.x),
        ] {
            let own_response = grants.send(&route, &own.to_string(), Some(bearer)).await;
            assert_eq!(
                own_response.status(),
                route.own_status,
                "{} own",
                route.label
            );
            let other_response = grants.send(&route, &other.to_string(), Some(bearer)).await;
            assert_eq!(
                other_response.status(),
                StatusCode::NOT_FOUND,
                "{} other",
                route.label
            );
        }
        // A device granted both vehicles reads both.
        for vehicle in [grants.x, grants.y] {
            let response = grants
                .send(&route, &vehicle.to_string(), Some(&bearer_all))
                .await;
            assert_eq!(response.status(), route.own_status, "{} all", route.label);
        }
    }
    // The vehicle list is the granted, published vehicles.
    let ids = |response: Response| async move {
        response_json(response).await["vehicles"]
            .as_array()
            .expect("vehicles array")
            .iter()
            .map(|vehicle| vehicle["vehicle_id"].as_str().expect("id").to_owned())
            .collect::<BTreeSet<String>>()
    };
    for (bearer, expected) in [
        (&bearer_x, vec![grants.x]),
        (&bearer_y, vec![grants.y]),
        (&bearer_all, vec![grants.x, grants.y]),
    ] {
        let listed = ids(send(&grants.app, "GET", "/v1/vehicles", Some(bearer), None).await).await;
        assert_eq!(
            listed,
            expected
                .iter()
                .map(Uuid::to_string)
                .collect::<BTreeSet<_>>()
        );
    }
}

#[tokio::test]
async fn packs_are_served_only_to_a_device_granted_an_owning_vehicle() {
    let grants = Grants::new();
    let (_, bearer_x) = grants.device(&[grants.x]);
    let (_, bearer_y) = grants.device(&[grants.y]);
    let (_, bearer_none) = grants.device(&[]);
    for (bearer, own, other) in [
        (&bearer_x, &grants.pack_x, &grants.pack_y),
        (&bearer_y, &grants.pack_y, &grants.pack_x),
    ] {
        let served = send(
            &grants.app,
            "GET",
            &pack_uri(own.digest),
            Some(bearer),
            None,
        )
        .await;
        assert_eq!(served.status(), StatusCode::OK);
        assert_eq!(body_bytes(served).await, own.bytes);

        let ungranted = fingerprint(
            send(
                &grants.app,
                "GET",
                &pack_uri(other.digest),
                Some(bearer),
                None,
            )
            .await,
        )
        .await;
        let unknown = fingerprint(
            send(
                &grants.app,
                "GET",
                &pack_uri(Sha256Digest::of_bytes(b"no such pack")),
                Some(bearer),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(ungranted.status, StatusCode::NOT_FOUND);
        assert!(ungranted.body.is_empty());
        assert_eq!(
            ungranted, unknown,
            "an ungranted digest differs from an unknown one"
        );
    }
    // A device with no grant at all sees neither pack.
    for pack in [&grants.pack_x, &grants.pack_y] {
        let response = send(
            &grants.app,
            "GET",
            &pack_uri(pack.digest),
            Some(&bearer_none),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    // A byte range is still a pack read: the grant decides before the range.
    let ranged = send_with_range(&grants.app, &pack_uri(grants.pack_y.digest), &bearer_x).await;
    assert_eq!(ranged.status(), StatusCode::NOT_FOUND);
}

async fn send_with_range(app: &Router, uri: &str, bearer: &str) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .header(header::RANGE, "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("ranged response")
}

#[tokio::test]
async fn a_device_with_no_grant_sees_no_vehicle_and_no_pack() {
    let grants = Grants::new();
    let (_, bearer) = grants.device(&[]);
    let listed = send(&grants.app, "GET", "/v1/vehicles", Some(&bearer), None).await;
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(
        response_json(listed).await,
        serde_json::json!({"vehicles": []})
    );
    for route in vehicle_routes() {
        for vehicle in [grants.x, grants.y, grants.z] {
            let response = grants
                .send(&route, &vehicle.to_string(), Some(&bearer))
                .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{}", route.label);
        }
    }
    for pack in [&grants.pack_x, &grants.pack_y] {
        let response = send(
            &grants.app,
            "GET",
            &pack_uri(pack.digest),
            Some(&bearer),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn a_revoked_grant_or_vehicle_takes_effect_on_the_next_request() {
    let grants = Grants::new();
    let (device_id, bearer) = grants.device(&[grants.x, grants.y]);
    let manifest = |vehicle: Uuid| format!("/v1/vehicles/{vehicle}/sync/manifest");
    let status = |uri: String| {
        let app = grants.app.clone();
        let bearer = bearer.clone();
        async move { send(&app, "GET", &uri, Some(&bearer), None).await.status() }
    };
    assert_eq!(status(manifest(grants.x)).await, StatusCode::OK);
    assert_eq!(status(pack_uri(grants.pack_x.digest)).await, StatusCode::OK);

    // De-grant (delete the row): the same router, no restart.
    grants
        .store
        .open()
        .unwrap()
        .execute(
            "DELETE FROM device_vehicle_grants WHERE device_id = ?1 AND vehicle_id = ?2",
            [device_id.to_string(), grants.x.to_string()],
        )
        .unwrap();
    assert_eq!(status(manifest(grants.x)).await, StatusCode::NOT_FOUND);
    assert_eq!(
        status(format!("/v1/vehicles/{}/current", grants.x)).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status(pack_uri(grants.pack_x.digest)).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(status(manifest(grants.y)).await, StatusCode::OK);
    grants
        .store
        .open()
        .unwrap()
        .execute(
            "INSERT INTO device_vehicle_grants (device_id, vehicle_id, granted_at_ms) VALUES (?1, ?2, 1)",
            [device_id.to_string(), grants.x.to_string()],
        )
        .unwrap();
    assert_eq!(status(manifest(grants.x)).await, StatusCode::OK);

    // Retirement hides the vehicle from the granted device at once, and
    // reactivation brings it back without pairing again.
    assert!(grants.store.retire_vehicle(grants.y, 9_000).unwrap());
    assert_eq!(status(manifest(grants.y)).await, StatusCode::NOT_FOUND);
    assert_eq!(
        status(format!("/v1/vehicles/{}/drives", grants.y)).await,
        StatusCode::NOT_FOUND
    );
    assert!(grants.store.reactivate_vehicle(grants.y).unwrap());
    assert_eq!(status(manifest(grants.y)).await, StatusCode::OK);
}

#[tokio::test]
async fn a_revoked_device_fails_on_json_sync_and_pack_routes_even_with_grants() {
    let grants = Grants::new();
    let (device_id, bearer) = grants.device(&[grants.x]);
    assert_eq!(
        send(
            &grants.app,
            "GET",
            &pack_uri(grants.pack_x.digest),
            Some(&bearer),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    grants.store.revoke_device(device_id).expect("revoke");
    // The grant rows are still there; the revocation is what answers.
    assert!(
        grants
            .store
            .vehicle_accessible(
                VehicleScope::Device(device_id),
                grants.x,
                crate::db::VehicleVisibility::Active
            )
            .unwrap()
    );
    let mut uris: Vec<(&str, String)> = vec![
        ("GET", "/v1/vehicles".to_owned()),
        ("GET", pack_uri(grants.pack_x.digest)),
    ];
    for route in vehicle_routes() {
        uris.push((route.method, route.uri(&grants.x.to_string())));
    }
    for (method, uri) in uris {
        let response = send(&grants.app, method, &uri, Some(&bearer), None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    }
}

#[tokio::test]
async fn the_loopback_router_has_no_device_principal_and_sees_every_vehicle() {
    let grants = Grants::new();
    // The plain-loopback router (device authentication off): the nil principal
    // bypasses grants, so no bearer is needed and every active vehicle answers.
    let loopback = public_query_router(grants.store.clone(), GRANT_KEY_BYTE);
    for vehicle in [grants.x, grants.y] {
        for template in ["/v1/vehicles/{v}/current", "/v1/vehicles/{v}/drives"] {
            let uri = template.replace("{v}", &vehicle.to_string());
            assert_eq!(
                send(&loopback, "GET", &uri, None, None).await.status(),
                StatusCode::OK,
                "{uri}"
            );
        }
        assert_eq!(
            send(
                &loopback,
                "GET",
                &format!("/v1/vehicles/{vehicle}/sync/signing-keys"),
                None,
                None
            )
            .await
            .status(),
            StatusCode::OK
        );
    }
    let listed = response_json(send(&loopback, "GET", "/v1/vehicles", None, None).await).await;
    assert_eq!(listed["vehicles"].as_array().expect("vehicles").len(), 2);
    for pack in [&grants.pack_x, &grants.pack_y] {
        let served = send(&loopback, "GET", &pack_uri(pack.digest), None, None).await;
        assert_eq!(served.status(), StatusCode::OK);
        assert_eq!(body_bytes(served).await, pack.bytes);
    }
    // An unknown vehicle stays hidden from it as well.
    assert_eq!(
        send(
            &loopback,
            "GET",
            &format!("/v1/vehicles/{}/current", Uuid::new_v4()),
            None,
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    // The paired router never bypasses: the same ungranted vehicle is a 404.
    let (_, bearer_x) = grants.device(&[grants.x]);
    assert_eq!(
        send(
            &grants.app,
            "GET",
            &format!("/v1/vehicles/{}/current", grants.y),
            Some(&bearer_x),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    // A retired vehicle is hidden from the loopback router too: the bypass is
    // of the grant, not of the vehicle's own state.
    assert!(grants.store.retire_vehicle(grants.x, 9_000).unwrap());
    for template in [
        "/v1/vehicles/{v}/current",
        "/v1/vehicles/{v}/drives",
        "/v1/vehicles/{v}/sync/manifest",
    ] {
        let uri = template.replace("{v}", &grants.x.to_string());
        assert_eq!(
            send(&loopback, "GET", &uri, None, None).await.status(),
            StatusCode::NOT_FOUND,
            "{uri}: a retired vehicle is hidden from the loopback router"
        );
    }
    let listed = response_json(send(&loopback, "GET", "/v1/vehicles", None, None).await).await;
    assert_eq!(listed["vehicles"].as_array().expect("vehicles").len(), 1);
}

#[tokio::test]
async fn the_sync_routes_keep_their_not_found_shapes_per_path_segment() {
    let grants = Grants::new();
    let (_, bearer) = grants.device(&[grants.x]);
    let empty_404 = |fingerprint: &Fingerprint| {
        fingerprint.status == StatusCode::NOT_FOUND && fingerprint.body.is_empty()
    };
    let json_404 = |fingerprint: &Fingerprint| {
        fingerprint.status == StatusCode::NOT_FOUND
            && serde_json::from_slice::<serde_json::Value>(&fingerprint.body).is_ok_and(|body| {
                // The public envelope, or the sync envelope of changes-since.
                body["error"]["code"] == "vehicle_not_found" || body["code"] == "vehicle_not_found"
            })
    };
    // route label -> (malformed vehicle id answers JSON?, nil answers JSON?)
    let expected = [
        ("manifest", false, true),
        ("noop", false, true),
        ("prepared artefact", false, true),
        ("signing keys", false, false),
        ("changes since", true, true),
    ];
    let routes = vehicle_routes();
    for (label, malformed_is_json, nil_is_json) in expected {
        let route = routes.iter().find(|route| route.label == label).unwrap();
        for malformed in ["not-a-uuid", "%FF", "%C3%28"] {
            let seen = fingerprint(grants.send(route, malformed, Some(&bearer)).await).await;
            assert!(
                if malformed_is_json {
                    json_404(&seen)
                } else {
                    empty_404(&seen)
                },
                "{label} {malformed}: {seen:?}"
            );
        }
        let nil = fingerprint(
            grants
                .send(route, &Uuid::nil().to_string(), Some(&bearer))
                .await,
        )
        .await;
        assert!(
            if nil_is_json {
                json_404(&nil)
            } else {
                empty_404(&nil)
            },
            "{label} nil: {nil:?}"
        );
        // Without a valid bearer every path is a 401, whatever its segments.
        for vehicle in [
            "%FF",
            "not-a-uuid",
            &Uuid::nil().to_string(),
            &grants.x.to_string(),
        ] {
            for headers in [None, Some("not-a-valid-token")] {
                let response = grants.send(route, vehicle, headers).await;
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "{label} {vehicle}"
                );
            }
        }
    }
    // A prepared-artefact id that is not UTF-8 names no artefact.
    let bad_artefact = fingerprint(
        send(
            &grants.app,
            "GET",
            &format!("/v1/vehicles/{}/sync/prepared-artefacts/%FF", grants.x),
            Some(&bearer),
            None,
        )
        .await,
    )
    .await;
    assert!(empty_404(&bad_artefact), "{bad_artefact:?}");
    // The pack object name: not UTF-8, no suffix and a bad digest are the empty
    // 404 after the bearer check, never the framework's 400, and the very
    // answer of a well-formed digest that names no pack.
    let unknown_pack = fingerprint(
        send(
            &grants.app,
            "GET",
            &pack_uri(Sha256Digest::of_bytes(b"no such pack")),
            Some(&bearer),
            None,
        )
        .await,
    )
    .await;
    assert!(empty_404(&unknown_pack), "{unknown_pack:?}");
    for name in ["%FF.sqlite.zst", "%FF", "nodigest", "zz.sqlite.zst"] {
        let uri = format!("/v1/packs/sha256/{name}");
        let seen = fingerprint(send(&grants.app, "GET", &uri, Some(&bearer), None).await).await;
        assert!(empty_404(&seen), "{uri}: {seen:?}");
        assert_eq!(seen, unknown_pack, "{uri}");
        let unauthenticated = send(&grants.app, "GET", &uri, None, None).await;
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn sync_routes_answer_an_active_vehicle_without_a_snapshot_and_published_routes_do_not() {
    let grants = Grants::new();
    let (_, bearer) = grants.device(&[grants.z]);
    let get = |uri: String| {
        let app = grants.app.clone();
        let bearer = bearer.clone();
        async move { fingerprint(send(&app, "GET", &uri, Some(&bearer), None).await).await }
    };
    // z is active and granted but unpublished. The manifest says so with the
    // empty 404 `no_full_snapshot`, not with the unknown vehicle's JSON error,
    // and signing keys are served: both routes check the vehicle as Active.
    let manifest = get(format!("/v1/vehicles/{}/sync/manifest", grants.z)).await;
    assert_eq!(manifest.status, StatusCode::NOT_FOUND);
    assert!(manifest.body.is_empty(), "no_full_snapshot is an empty 404");
    let unknown = get(format!("/v1/vehicles/{}/sync/manifest", Uuid::new_v4())).await;
    assert_ne!(manifest, unknown, "an active vehicle is not an unknown one");
    assert_eq!(
        get(format!("/v1/vehicles/{}/sync/signing-keys", grants.z))
            .await
            .status,
        StatusCode::OK
    );
    // The published routes (/v1/vehicles, /current and the history routes)
    // require publication as well.
    let current = get(format!("/v1/vehicles/{}/current", grants.z)).await;
    assert_eq!(current.status, StatusCode::NOT_FOUND);
    assert!(current.body.is_empty());
    let drives = get(format!("/v1/vehicles/{}/drives", grants.z)).await;
    assert_eq!(drives.status, StatusCode::NOT_FOUND);
    assert!(
        serde_json::from_slice::<serde_json::Value>(&drives.body)
            .is_ok_and(|body| body["error"]["code"] == "vehicle_not_found")
    );
    let listed = get("/v1/vehicles".to_owned()).await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&listed.body).unwrap(),
        serde_json::json!({"vehicles": []})
    );
}

#[tokio::test]
async fn a_claimed_scope_and_a_rotated_bearer_keep_the_same_grants() {
    let grants = Grants::new();
    let now_ms = current_epoch_ms().expect("clock");
    let invitation = grants
        .store
        .create_pairing_with_scope(
            "scoped over HTTP",
            now_ms - 1,
            i64::MAX,
            &PairingScope::Vehicles(vec![grants.y]),
        )
        .expect("scoped invitation");
    // The production claim route; the secret reaches the store as the same
    // digest the invitation holds, and the scope is not in the request.
    let claimed = response_json(
        send(
            &grants.app,
            "POST",
            &format!("/v1/pairings/{}/claim", invitation.pairing_id),
            None,
            Some(serde_json::json!({
                "secret": invitation.secret(),
                "device_name": "http phone"
            })),
        )
        .await,
    )
    .await;
    let bearer = claimed["access_token"].as_str().expect("bearer").to_owned();
    let manifest = |vehicle: Uuid| format!("/v1/vehicles/{vehicle}/sync/manifest");
    assert_eq!(
        send(&grants.app, "GET", &manifest(grants.y), Some(&bearer), None)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        send(&grants.app, "GET", &manifest(grants.x), Some(&bearer), None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    // Rotation keeps the device identity, so the new bearer has the grants.
    let rotated = response_json(
        send(
            &grants.app,
            "POST",
            "/v1/device/rotate",
            Some(&bearer),
            None,
        )
        .await,
    )
    .await;
    let renewed = rotated["access_token"]
        .as_str()
        .expect("rotated bearer")
        .to_owned();
    assert_ne!(renewed, bearer);
    assert_eq!(
        send(
            &grants.app,
            "GET",
            &manifest(grants.y),
            Some(&renewed),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        send(
            &grants.app,
            "GET",
            &manifest(grants.x),
            Some(&renewed),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &grants.app,
            "GET",
            &pack_uri(grants.pack_x.digest),
            Some(&renewed),
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn physical_and_prepared_packs_follow_the_owning_vehicles_grant() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path().join("store")).expect("store");
    let key = grant_key();
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let stage = |name: &str, updates: u32| {
        let (binding, candidate) =
            crate::import::teslamate::physical_fragments::tests::public_admission_candidate_fixture_for_source(
                &temp.path().join(name),
                &store,
                &key,
                updates,
                name,
            );
        let admission = store
            .stage_pending_physical_v3_admission(&gate, candidate)
            .expect("public physical head");
        (binding.vehicle_id, admission)
    };
    // Different update counts give the two heads different pack bytes.
    let (vehicle_one, admission_one) = stage("grants-p1", 1);
    let (vehicle_two, admission_two) = stage("grants-p2", 2);
    drop(gate);
    let pack_one = admission_one.manifest.chunks[0].sha256;
    let pack_two = admission_two.manifest.chunks[0].sha256;
    assert_ne!(pack_one, pack_two);

    let now_ms = current_epoch_ms().expect("clock");
    let device = |vehicle: Uuid| {
        let invitation = store
            .create_pairing_with_scope(
                "physical grants",
                now_ms - 1,
                i64::MAX,
                &PairingScope::Vehicles(vec![vehicle]),
            )
            .expect("invitation");
        let access = store
            .claim_pairing(invitation.pairing_id, invitation.secret(), "client", now_ms)
            .expect("claim");
        (access.device_id, access.access_token.as_bearer().to_owned())
    };
    let (device_one, bearer_one) = device(vehicle_one);
    let (device_two, bearer_two) = device(vehicle_two);
    let app = paired_router(store.clone(), &key);

    // Pending physical heads (the second lookup family of the pack route).
    for (bearer, own, other) in [
        (&bearer_one, pack_one, pack_two),
        (&bearer_two, pack_two, pack_one),
    ] {
        let served = send(&app, "GET", &pack_uri(own), Some(bearer), None).await;
        assert_eq!(served.status(), StatusCode::OK, "own physical pack");
        let ungranted =
            fingerprint(send(&app, "GET", &pack_uri(other), Some(bearer), None).await).await;
        let unknown = fingerprint(
            send(
                &app,
                "GET",
                &pack_uri(Sha256Digest::of_bytes(b"unknown")),
                Some(bearer),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(ungranted.status, StatusCode::NOT_FOUND);
        assert_eq!(
            ungranted, unknown,
            "an ungranted physical pack is an unknown one"
        );
    }

    // The signed prepared month of vehicle one: its receipt route and its pack.
    let (artifact_id, receipt) =
        crate::import::teslamate::prepared_map::insert_signed_month_fixture(
            &store,
            &key,
            &admission_one,
            now_ms,
        );
    let receipt_uri =
        |vehicle: Uuid| format!("/v1/vehicles/{vehicle}/sync/prepared-artefacts/{artifact_id}");
    let served = send(
        &app,
        "GET",
        &receipt_uri(vehicle_one),
        Some(&bearer_one),
        None,
    )
    .await;
    assert_eq!(served.status(), StatusCode::OK);
    assert_eq!(body_bytes(served).await, receipt);
    let ungranted_receipt = fingerprint(
        send(
            &app,
            "GET",
            &receipt_uri(vehicle_one),
            Some(&bearer_two),
            None,
        )
        .await,
    )
    .await;
    let unknown_receipt = fingerprint(
        send(
            &app,
            "GET",
            &receipt_uri(Uuid::new_v4()),
            Some(&bearer_two),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(ungranted_receipt.status, StatusCode::NOT_FOUND);
    assert_eq!(ungranted_receipt, unknown_receipt);
    // The prepared pack lookup (the last family) resolves the owner as well.
    let prepared_pack = Sha256Digest::of_bytes(&[0x00]);
    assert_ne!(prepared_pack, pack_one);
    let digest_c: Sha256Digest = "c".repeat(64).parse().expect("fixture digest");
    let lookup = |scope: VehicleScope| {
        crate::import::teslamate::prepared_map::current_pack(&store, &key, digest_c, scope)
            .expect("prepared pack lookup")
    };
    assert!(lookup(VehicleScope::Unrestricted).is_some());
    assert!(lookup(VehicleScope::Device(device_one)).is_some());
    assert!(
        lookup(VehicleScope::Device(device_two)).is_none(),
        "vehicle two is not an owner of vehicle one's prepared pack"
    );
    assert!(lookup(VehicleScope::Device(Uuid::new_v4())).is_none());
    let ungranted_prepared =
        fingerprint(send(&app, "GET", &pack_uri(digest_c), Some(&bearer_two), None).await).await;
    let unknown_prepared = fingerprint(
        send(
            &app,
            "GET",
            &pack_uri(Sha256Digest::of_bytes(b"another")),
            Some(&bearer_two),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(ungranted_prepared.status, StatusCode::NOT_FOUND);
    assert_eq!(ungranted_prepared, unknown_prepared);

    // With no cursor key a prepared pack's receipt cannot be verified: the
    // device that owns the month gets the 503 of an outage, while the device
    // that is not granted it (and anyone asking for an unknown digest) still
    // gets the empty 404, so the missing key tells nothing about ownership.
    let keyless = paired_state(&store, None);
    let owner_keyless = fingerprint(call_pack(&keyless, digest_c, &bearer_one).await).await;
    assert_eq!(owner_keyless.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(owner_keyless.body.is_empty());
    let ungranted_keyless = fingerprint(call_pack(&keyless, digest_c, &bearer_two).await).await;
    let unknown_keyless =
        fingerprint(call_pack(&keyless, Sha256Digest::of_bytes(b"another"), &bearer_two).await)
            .await;
    assert_eq!(ungranted_keyless.status, StatusCode::NOT_FOUND);
    assert!(ungranted_keyless.body.is_empty());
    assert_eq!(ungranted_keyless, unknown_keyless);
    // The loopback scope (no device principal) is the owner's: unrestricted.
    assert!(
        crate::import::teslamate::prepared_map::current_pack_candidate_exists(
            &store,
            digest_c,
            VehicleScope::Unrestricted
        )
        .unwrap()
    );
    assert!(
        !crate::import::teslamate::prepared_map::current_pack_candidate_exists(
            &store,
            digest_c,
            VehicleScope::Device(device_two)
        )
        .unwrap()
    );
    assert!(
        crate::import::teslamate::prepared_map::current_pack_candidate_exists(
            &store,
            digest_c,
            VehicleScope::Device(device_one)
        )
        .unwrap()
    );
}

/// The pack handler called directly on a state whose slots and cursor key the
/// router constructors do not let a test shape.
async fn call_pack(state: &AppState, digest: impl std::fmt::Display, bearer: &str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {bearer}")).expect("bearer header"),
    );
    pack(
        State(state.clone()),
        pack_uri(digest).parse().expect("pack URI"),
        headers,
    )
    .await
}

fn paired_state(store: &HubStore, key: Option<CursorKey>) -> AppState {
    AppState::new(
        store.clone(),
        false,
        true,
        true,
        key.as_ref().map(ManifestSigning::from_cursor_key),
        key,
        None,
        None,
    )
}

#[tokio::test]
async fn an_ungranted_digest_is_not_found_before_any_stream_slot_branch() {
    let grants = Grants::new();
    let (device_x, bearer_x) = grants.device(&[grants.x]);
    let state = paired_state(&grants.store, Some(grant_key()));
    let unknown = Sha256Digest::of_bytes(b"no such pack");

    // The device's own slots are all taken: its own pack is the 503 with
    // Retry-After, and a pack of a vehicle it is not granted is still the plain
    // 404 of an unknown digest (the lookup runs before the slot is taken).
    let held: Vec<PackDeviceSlot> = (0..MAX_ACTIVE_PACK_STREAMS_PER_DEVICE)
        .map(|_| {
            state
                .try_acquire_pack_device_slot(device_x)
                .expect("device slot")
        })
        .collect();
    let busy = call_pack(&state, grants.pack_x.digest, &bearer_x).await;
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(busy.headers()[header::RETRY_AFTER], "1");
    let ungranted = fingerprint(call_pack(&state, grants.pack_y.digest, &bearer_x).await).await;
    let missing = fingerprint(call_pack(&state, unknown, &bearer_x).await).await;
    assert_eq!(ungranted.status, StatusCode::NOT_FOUND);
    assert!(ungranted.body.is_empty());
    assert_eq!(ungranted, missing, "device slots exhausted");
    drop(held);

    // The same with the global pool exhausted.
    let pool = state
        .pack_stream_slots
        .clone()
        .try_acquire_many_owned(u32::try_from(MAX_ACTIVE_PACK_STREAMS).expect("pool size"))
        .expect("every global slot");
    let busy = call_pack(&state, grants.pack_x.digest, &bearer_x).await;
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(busy.headers()[header::RETRY_AFTER], "1");
    let ungranted = fingerprint(call_pack(&state, grants.pack_y.digest, &bearer_x).await).await;
    let missing = fingerprint(call_pack(&state, unknown, &bearer_x).await).await;
    assert_eq!(ungranted.status, StatusCode::NOT_FOUND);
    assert_eq!(ungranted, missing, "global slots exhausted");
    drop(pool);
    assert_eq!(
        call_pack(&state, grants.pack_x.digest, &bearer_x)
            .await
            .status(),
        StatusCode::OK,
        "the slots were only held, not consumed"
    );
}

#[tokio::test]
async fn an_ungranted_digest_is_not_found_before_the_cursor_key_branch() {
    let grants = Grants::new();
    let (_, bearer_x) = grants.device(&[grants.x]);
    // No cursor key: a pack the device is granted is still served from the
    // catalogue, and a digest that no catalogue lists for it is the empty 404
    // of an unknown digest, whether another vehicle owns it or nobody does.
    // The missing key is an outage only for a prepared pack the device may be
    // served (see the prepared-pack test).
    let state = paired_state(&grants.store, None);
    assert_eq!(
        call_pack(&state, grants.pack_x.digest, &bearer_x)
            .await
            .status(),
        StatusCode::OK
    );
    let ungranted = fingerprint(call_pack(&state, grants.pack_y.digest, &bearer_x).await).await;
    let missing =
        fingerprint(call_pack(&state, Sha256Digest::of_bytes(b"no such pack"), &bearer_x).await)
            .await;
    assert_eq!(ungranted.status, StatusCode::NOT_FOUND);
    assert!(ungranted.body.is_empty());
    assert_eq!(ungranted, missing);
    // The same Hub with a cursor key answers the same two digests the same way.
    let keyed = paired_state(&grants.store, Some(grant_key()));
    let keyed_ungranted =
        fingerprint(call_pack(&keyed, grants.pack_y.digest, &bearer_x).await).await;
    assert_eq!(keyed_ungranted, ungranted, "the key does not change a 404");
}

#[tokio::test]
async fn the_catalogued_manifest_is_read_only_after_the_owner_is_resolved() {
    let grants = Grants::new();
    let (_, bearer_x) = grants.device(&[grants.x]);
    let (_, bearer_y) = grants.device(&[grants.y]);
    let state = paired_state(&grants.store, Some(grant_key()));
    // Vehicle y's catalogued manifest can no longer be validated: its owner
    // gets the 503 of a catalogue that cannot be read, while a device that is
    // not granted y never reaches the manifest and sees the unknown-digest 404.
    grants
        .store
        .open()
        .unwrap()
        .execute(
            "UPDATE sync_manifests SET manifest_json = x'7b7d' WHERE vehicle_id = ?1",
            [grants.y.to_string()],
        )
        .expect("corrupt the catalogued manifest");
    let owner = call_pack(&state, grants.pack_y.digest, &bearer_y).await;
    assert_eq!(owner.status(), StatusCode::SERVICE_UNAVAILABLE);
    let ungranted = fingerprint(call_pack(&state, grants.pack_y.digest, &bearer_x).await).await;
    let missing =
        fingerprint(call_pack(&state, Sha256Digest::of_bytes(b"no such pack"), &bearer_x).await)
            .await;
    assert_eq!(ungranted.status, StatusCode::NOT_FOUND);
    assert_eq!(ungranted, missing);
}

/// A paired device granted exactly the vehicles `scope` names: its id and
/// bearer.
fn claim_scoped(store: &HubStore, scope: &PairingScope) -> (Uuid, String) {
    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing_with_scope("scoped device", now_ms - 1, i64::MAX, scope)
        .expect("invitation");
    let access = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "client", now_ms)
        .expect("claim");
    (access.device_id, access.access_token.as_bearer().to_owned())
}

/// A vehicle with a v2 lineage whose prior head was retired with one replaced
/// delta pack, still within its retention: the vehicle, and the pack's digest
/// and bytes. The pack is in no active manifest, so only the retired-lineage
/// lookup of the pack route can find it.
fn seed_retired_lineage_pack(
    store: &HubStore,
    cursor_key: &CursorKey,
) -> (Uuid, Sha256Digest, &'static [u8]) {
    let (vehicle_id, _, _) = seed_v2_lineage(store, cursor_key);
    let binding = store
        .v2_projection_binding(vehicle_id)
        .expect("immutable binding");
    let mut prior = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage lookup")
        .expect("base lineage");
    let retired_bytes: &'static [u8] = b"grants-retired-delta";
    let retired_digest = Sha256Digest::of_bytes(retired_bytes);
    let parent_digest = prior.head_digest;
    let pack = TransportPack {
        pack_id: Uuid::new_v4(),
        snapshot_id: prior.base.snapshot_id,
        ordinal: 1,
        schema: HUB_PROJECTION_SCHEMA_V2,
        format: PackFormat::HubProjectionSqlite,
        compression: PackCompression::Zstd,
        relative_path: TransportPack::canonical_relative_path(retired_digest),
        sha256: retired_digest,
        compressed_bytes: u64::try_from(retired_bytes.len()).expect("retired bytes"),
        uncompressed_bytes: 100,
        row_count: 1,
        sequence: SequenceRange {
            from_exclusive: prior.head_sequence,
            to_inclusive: prior.head_sequence + 1,
        },
        tables: vec![MirrorTable::Car],
    };
    prior.deltas.push(LineageDelta {
        from_sequence: prior.head_sequence,
        to_sequence: prior.head_sequence + 1,
        parent_chain_digest: parent_digest,
        chain_digest: canonical_delta_chain_digest(parent_digest, retired_digest),
        pack_digest: retired_digest,
        pack: pack.clone(),
    });
    prior.head_sequence += 1;
    prior.head_digest = canonical_delta_chain_digest(parent_digest, retired_digest);
    prior.terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: prior.head_sequence,
        },
    )
    .expect("prior terminal cursor");
    prior.validate().expect("valid prior lineage");
    fs::write(
        store
            .packs_dir()
            .join("sha256")
            .join(format!("{retired_digest}.sqlite.zst")),
        retired_bytes,
    )
    .expect("retired pack file");
    let retired_at_ms = current_epoch_ms().expect("retirement clock");
    let connection = store.open().expect("catalogue");
    connection
        .execute(
            "INSERT INTO sync_retired_lineages(
                vehicle_id, head_digest, manifest_json, retired_at_ms, expires_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                vehicle_id.to_string(),
                prior.head_digest.to_string(),
                serde_json::to_vec(&prior).expect("prior lineage JSON"),
                retired_at_ms,
                retired_at_ms + 60_000,
            ],
        )
        .expect("retired lineage");
    connection
        .execute(
            "INSERT INTO sync_retired_lineage_packs(
                vehicle_id, head_digest, pack_digest, relative_path, compressed_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                vehicle_id.to_string(),
                prior.head_digest.to_string(),
                retired_digest.to_string(),
                pack.relative_path,
                i64::try_from(pack.compressed_bytes).expect("retired pack size"),
            ],
        )
        .expect("retired lineage pack");
    (vehicle_id, retired_digest, retired_bytes)
}

#[tokio::test]
async fn retired_lineage_packs_follow_the_owning_vehicles_grant() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let key = grant_key();
    let (vehicle, digest, bytes) = seed_retired_lineage_pack(&store, &key);
    let other = seed_granted_vehicle(&store, "retired-other", 9);
    assert_eq!(
        store
            .open()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sync_packs WHERE sha256 = ?1",
                [digest.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "the pack is in no active manifest: only the retired-lineage lookup finds it"
    );
    let (_, bearer_owner) = claim_scoped(&store, &PairingScope::Vehicles(vec![vehicle]));
    let (_, bearer_other) = claim_scoped(&store, &PairingScope::Vehicles(vec![other]));
    let app = paired_router(store.clone(), &key);

    let served = send(&app, "GET", &pack_uri(digest), Some(&bearer_owner), None).await;
    assert_eq!(served.status(), StatusCode::OK, "the owner's retired pack");
    assert_eq!(body_bytes(served).await, bytes);

    let ungranted =
        fingerprint(send(&app, "GET", &pack_uri(digest), Some(&bearer_other), None).await).await;
    let unknown = fingerprint(
        send(
            &app,
            "GET",
            &pack_uri(Sha256Digest::of_bytes(b"no such retired pack")),
            Some(&bearer_other),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(ungranted.status, StatusCode::NOT_FOUND);
    assert!(ungranted.body.is_empty());
    assert_eq!(
        ungranted, unknown,
        "an ungranted retired-lineage pack is an unknown digest"
    );

    // The unrestricted loopback scope is served it, and a grant removed from
    // the owner withdraws it on the next request.
    let loopback = public_query_router(store.clone(), GRANT_KEY_BYTE);
    assert_eq!(
        send(&loopback, "GET", &pack_uri(digest), None, None)
            .await
            .status(),
        StatusCode::OK
    );
    store
        .open()
        .unwrap()
        .execute(
            "DELETE FROM device_vehicle_grants WHERE vehicle_id = ?1",
            [vehicle.to_string()],
        )
        .unwrap();
    let withdrawn =
        fingerprint(send(&app, "GET", &pack_uri(digest), Some(&bearer_owner), None).await).await;
    assert_eq!(withdrawn, unknown, "a removed grant withdraws the pack");
}

#[tokio::test]
async fn an_unreadable_grant_table_is_a_503_and_the_nil_vehicle_is_never_one() {
    let grants = Grants::new();
    let (_, bearer) = grants.device(&[grants.x]);
    grants
        .store
        .open()
        .unwrap()
        .execute_batch("DROP TABLE device_vehicle_grants")
        .expect("make the grant table unreadable");
    // The vehicle check cannot run, so it is the outage of a catalogue that
    // cannot be read on every route (never an answer about the vehicle).
    for route in vehicle_routes() {
        let broken = grants
            .send(&route, &grants.x.to_string(), Some(&bearer))
            .await;
        assert_eq!(
            broken.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{}",
            route.label
        );
        // The nil UUID is answered before any lookup: its value alone never
        // causes a 503.
        let nil = fingerprint(
            grants
                .send(&route, &Uuid::nil().to_string(), Some(&bearer))
                .await,
        )
        .await;
        assert_eq!(nil.status, StatusCode::NOT_FOUND, "{} nil", route.label);
    }
    for uri in ["/v1/vehicles".to_owned(), pack_uri(grants.pack_x.digest)] {
        assert_eq!(
            send(&grants.app, "GET", &uri, Some(&bearer), None)
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn a_bad_query_on_an_ungranted_vehicle_is_still_an_unknown_vehicle() {
    let grants = Grants::new();
    let (_, bearer_x) = grants.device(&[grants.x]);
    for template in [
        "/v1/vehicles/{v}/drives",
        "/v1/vehicles/{v}/drives/10/positions",
        "/v1/vehicles/{v}/charges",
        "/v1/vehicles/{v}/charges/20/samples",
    ] {
        for query in [
            "limit=0",
            "limit=abc",
            "cursor=garbage",
            "unknown=1",
            "from_ms=abc",
        ] {
            let uri = |vehicle: Uuid| {
                format!("{}?{query}", template.replace("{v}", &vehicle.to_string()))
            };
            // The granted vehicle reads the query, which is invalid ...
            let own = send(&grants.app, "GET", &uri(grants.x), Some(&bearer_x), None).await;
            assert_eq!(own.status(), StatusCode::BAD_REQUEST, "{template}?{query}");
            // ... and an ungranted one is an unknown vehicle before the query
            // is read, so the 400 cannot reveal that it exists.
            let ungranted =
                fingerprint(send(&grants.app, "GET", &uri(grants.y), Some(&bearer_x), None).await)
                    .await;
            let unknown = fingerprint(
                send(
                    &grants.app,
                    "GET",
                    &uri(Uuid::new_v4()),
                    Some(&bearer_x),
                    None,
                )
                .await,
            )
            .await;
            assert_eq!(
                ungranted.status,
                StatusCode::NOT_FOUND,
                "{template}?{query}"
            );
            assert_eq!(ungranted, unknown, "{template}?{query}");
        }
    }
}
