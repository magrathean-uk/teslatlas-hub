// SPDX-License-Identifier: AGPL-3.0-only

//! Lineage manifest reads verify every stored pack. A stable pack is hashed
//! once per process, a modified or corrupted pack is hashed again and refused,
//! and the verification runs off the async runtime behind the bounded
//! sync-control slot. Real SQLite store, real pack files, the real router and
//! handlers; synthetic data only.

use super::*;

const LINEAGE_KEY_BYTE: u8 = 46;

fn lineage_key() -> CursorKey {
    CursorKey::from_bytes([LINEAGE_KEY_BYTE; 32])
}

/// A store with a published base and one delta, each a real file in the pack
/// directory. Returns the vehicle and the digests of the two packs.
fn lineage_store_with_one_delta(store: &HubStore) -> (Uuid, Sha256Digest, Sha256Digest) {
    let (vehicle_id, base_digest, _) = seed_v2_lineage(store, &lineage_key());
    let delta = append_v2_delta_fixture(
        store,
        &lineage_key(),
        vehicle_id,
        b"lineage-manifest-cache-delta-pack",
    );
    (vehicle_id, base_digest, delta.pack.sha256)
}

fn delta_v2_manifest_request(vehicle_id: Uuid) -> Request<Body> {
    Request::builder()
        .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
        .header(SYNC_CAPABILITY_HEADER, DELTA_V2_CAPABILITY)
        .body(Body::empty())
        .expect("delta-v2 manifest request")
}

fn bootstrap_manifest_request(vehicle_id: Uuid, profile: &str) -> Request<Body> {
    Request::builder()
        .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
        .header(SYNC_PROFILE_HEADER, profile)
        .header(SUPPORTED_SCHEMAS_HEADER, "2.1,2.2")
        .body(Body::empty())
        .expect("bootstrap manifest request")
}

async fn body_bytes(response: Response) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes()
        .to_vec()
}

fn lineage_pack_path(store: &HubStore, digest: Sha256Digest) -> std::path::PathBuf {
    store
        .packs_dir()
        .join("sha256")
        .join(format!("{digest}.sqlite.zst"))
}

fn lineage_hashes_of(store: &HubStore, digest: Sha256Digest) -> usize {
    store
        .lineage_pack_hash_log()
        .iter()
        .filter(|hashed| **hashed == digest)
        .count()
}

/// A loopback state with signing, so both manifest flavours can answer.
fn lineage_state(store: HubStore) -> AppState {
    AppState::new(
        store,
        false,
        false,
        false,
        Some(ManifestSigning::from_cursor_key(&lineage_key())),
        Some(lineage_key()),
        None,
        None,
    )
}

async fn call_manifest(state: &AppState, request: Request<Body>) -> Response {
    let (parts, _) = request.into_parts();
    manifest(State(state.clone()), parts.uri, parts.headers).await
}

#[tokio::test]
async fn two_consecutive_manifest_reads_hash_each_stable_pack_once() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (vehicle_id, base_digest, delta_digest) = lineage_store_with_one_delta(&store);
    let app = trusted_local_router(store.clone(), false, Some(lineage_key()), None);

    let first = app
        .clone()
        .oneshot(delta_v2_manifest_request(vehicle_id))
        .await
        .expect("first manifest response");
    assert_eq!(first.status(), StatusCode::OK);
    let first_body = body_bytes(first).await;
    let lineage: LineageManifestV2 = serde_json::from_slice(&first_body).expect("lineage JSON");
    lineage.validate().expect("client-valid lineage");
    assert_eq!(lineage.base.digest, base_digest);
    assert_eq!(lineage.deltas.len(), 1);
    assert_eq!(lineage.deltas[0].pack.sha256, delta_digest);
    let hashed_up_to_first_read = store.lineage_pack_hash_log();
    assert_eq!(hashed_up_to_first_read, vec![base_digest, delta_digest]);

    let second = app
        .oneshot(delta_v2_manifest_request(vehicle_id))
        .await
        .expect("second manifest response");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(body_bytes(second).await, first_body);
    assert_eq!(
        store.lineage_pack_hash_log(),
        hashed_up_to_first_read,
        "the second read hashes nothing"
    );
}

#[tokio::test]
async fn a_pack_modified_after_a_served_manifest_fails_the_next_read_until_restored() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (vehicle_id, base_digest, delta_digest) = lineage_store_with_one_delta(&store);
    let app = trusted_local_router(store.clone(), false, Some(lineage_key()), None);
    let read = |app: Router| async move {
        let response = app
            .oneshot(delta_v2_manifest_request(vehicle_id))
            .await
            .expect("manifest response");
        (response.status(), body_bytes(response).await)
    };

    let (status, served) = read(app.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read(app.clone()).await, (StatusCode::OK, served.clone()));
    assert_eq!(lineage_hashes_of(&store, base_digest), 1);

    let base_path = lineage_pack_path(&store, base_digest);
    let original = fs::read(&base_path).expect("base pack bytes");
    let mut corrupt = original.clone();
    corrupt[0] ^= 0xff;
    fs::write(&base_path, &corrupt).expect("same-length corruption");
    for attempt in 1..=2 {
        assert_eq!(
            read(app.clone()).await,
            (StatusCode::SERVICE_UNAVAILABLE, Vec::new()),
            "a corrupted pack is refused with an empty 503"
        );
        assert_eq!(lineage_hashes_of(&store, base_digest), 1 + attempt);
    }
    assert_eq!(lineage_hashes_of(&store, delta_digest), 1);

    let mut longer = original.clone();
    longer.push(0);
    fs::write(&base_path, &longer).expect("append one byte");
    let hashed = store.lineage_pack_hash_log();
    assert_eq!(
        read(app.clone()).await,
        (StatusCode::SERVICE_UNAVAILABLE, Vec::new()),
        "a pack of another length is refused"
    );
    assert_eq!(store.lineage_pack_hash_log(), hashed, "without hashing it");

    fs::write(&base_path, &original).expect("restore the pack");
    assert_eq!(read(app.clone()).await, (StatusCode::OK, served.clone()));
    assert_eq!(lineage_hashes_of(&store, base_digest), 4);
    assert_eq!(read(app).await, (StatusCode::OK, served));
    assert_eq!(lineage_hashes_of(&store, base_digest), 4);
    assert_eq!(lineage_hashes_of(&store, delta_digest), 1);
}

#[tokio::test]
async fn a_delta_v2_manifest_read_waits_for_the_sync_control_slot_before_verifying() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let vehicle_id = seed_lineage_without_hashing(&store).0;
    let state = lineage_state(store.clone());
    let slot = state
        .sync_control_slots
        .clone()
        .acquire_owned()
        .await
        .expect("hold the sync-control slot");

    let waiting_state = state.clone();
    let mut pending = tokio::spawn(async move {
        call_manifest(&waiting_state, delta_v2_manifest_request(vehicle_id)).await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err(),
        "the manifest read queues behind the slot"
    );
    assert!(
        store.lineage_pack_hash_log().is_empty(),
        "nothing is verified before the slot is held"
    );

    drop(slot);
    let response = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("the read completes once the slot is free")
        .expect("manifest task");
    assert_eq!(response.status(), StatusCode::OK);
    let lineage: LineageManifestV2 =
        serde_json::from_slice(&body_bytes(response).await).expect("lineage JSON");
    lineage.validate().expect("client-valid lineage");
    assert_eq!(
        store.lineage_pack_hash_log().len(),
        1,
        "the base was hashed"
    );
    assert_eq!(state.sync_control_slots.available_permits(), 1);
}

#[tokio::test]
async fn the_bootstrap_manifest_waits_for_the_slot_only_for_its_lineage_tail() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (vehicle_id, base_digest) = seed_lineage_without_hashing(&store);
    let state = lineage_state(store.clone());
    let slot = state
        .sync_control_slots
        .clone()
        .acquire_owned()
        .await
        .expect("hold the sync-control slot");

    // The physical-only selection never verifies lineage packs, so it never
    // queues for the slot: it answers while the slot is held.
    let physical = tokio::time::timeout(
        Duration::from_secs(2),
        call_manifest(
            &state,
            bootstrap_manifest_request(vehicle_id, hub_sync_v1::HUB_SYNC_PROFILE_14),
        ),
    )
    .await
    .expect("the physical bootstrap does not wait for the slot");
    assert_eq!(physical.status(), StatusCode::NOT_ACCEPTABLE);
    assert!(store.lineage_pack_hash_log().is_empty());

    let waiting_state = state.clone();
    let mut pending = tokio::spawn(async move {
        call_manifest(
            &waiting_state,
            bootstrap_manifest_request(vehicle_id, HUB_SYNC_PROFILE_ID),
        )
        .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err(),
        "the lineage tail queues behind the slot"
    );
    assert!(store.lineage_pack_hash_log().is_empty());

    drop(slot);
    let response = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("the bootstrap completes once the slot is free")
        .expect("bootstrap task");
    assert_eq!(response.status(), StatusCode::OK);
    let bootstrap = response_json(response).await;
    assert_eq!(bootstrap["schema_version"], "2.1");
    assert_eq!(bootstrap["receipt_id"], base_digest.to_string());
    assert_eq!(bootstrap["vehicle_id"], vehicle_id.to_string());
    assert_eq!(store.lineage_pack_hash_log(), vec![base_digest]);
    assert_eq!(state.sync_control_slots.available_permits(), 1);
}

/// A published base only, whose pack has not been hashed yet in this store
/// (seeding writes the catalogue and the file without verifying either).
fn seed_lineage_without_hashing(store: &HubStore) -> (Uuid, Sha256Digest) {
    let (vehicle_id, base_digest, _) = seed_v2_lineage(store, &lineage_key());
    assert!(store.lineage_pack_hash_log().is_empty());
    (vehicle_id, base_digest)
}
