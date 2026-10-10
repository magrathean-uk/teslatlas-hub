// SPDX-License-Identifier: AGPL-3.0-only

// Per-device vehicle grants: pairing scope, claim, access checks, listing.

struct GrantFixture {
    _temporary: tempfile::TempDir,
    store: HubStore,
    source_id: Uuid,
    /// Active, published, named "Alpha".
    alpha: Uuid,
    /// Active, published, unnamed.
    beta: Uuid,
    /// Retired.
    retired: Uuid,
    /// Active, not published (no sync manifest).
    unpublished: Uuid,
}

fn publish_vehicle_fixture(store: &HubStore, vehicle_id: Uuid) {
    store
        .open()
        .expect("open store")
        .execute(
            "INSERT INTO sync_manifests(snapshot_id, vehicle_id, head_sequence, manifest_json)
             VALUES (?1, ?2, 1, ?3)",
            params![
                Uuid::new_v4().to_string(),
                vehicle_id.to_string(),
                b"{}".as_slice()
            ],
        )
        .expect("publish vehicle fixture");
}

fn grant_fixture() -> GrantFixture {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let source_id = store
        .register_source(&SourceDescriptor::new("teslamate", "grants"), 1)
        .expect("source")
        .source_id;
    let register = |key: &str, name: Option<&str>, at_ms: i64| {
        let mut descriptor = VehicleDescriptor::new(source_id, key);
        descriptor.display_name = name.map(str::to_owned);
        store
            .register_vehicle(&descriptor, at_ms)
            .expect("vehicle")
            .vehicle_id
    };
    let alpha = register("alpha", Some("Alpha"), 10);
    let beta = register("beta", None, 20);
    let retired = register("retired", Some("Retired"), 30);
    let unpublished = register("unpublished", Some("Unpublished"), 40);
    for vehicle_id in [alpha, beta, retired] {
        publish_vehicle_fixture(&store, vehicle_id);
    }
    assert!(store.retire_vehicle(retired, 50).expect("retire"));
    GrantFixture {
        _temporary: temporary,
        store,
        source_id,
        alpha,
        beta,
        retired,
        unpublished,
    }
}

fn granted_vehicles(store: &HubStore, device_id: Uuid) -> Vec<Uuid> {
    let mut vehicles: Vec<Uuid> = store
        .open()
        .expect("open store")
        .prepare("SELECT vehicle_id FROM device_vehicle_grants WHERE device_id = ?1")
        .expect("grant query")
        .query_map([device_id.to_string()], |row| row.get::<_, String>(0))
        .expect("grant rows")
        .map(|row| Uuid::parse_str(&row.expect("grant row")).expect("vehicle UUID"))
        .collect();
    vehicles.sort();
    vehicles
}

fn sorted(mut vehicles: Vec<Uuid>) -> Vec<Uuid> {
    vehicles.sort();
    vehicles
}

fn table_count(store: &HubStore, table: &str) -> i64 {
    store
        .open()
        .expect("open store")
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("row count")
}

fn claim_with_scope(
    store: &HubStore,
    scope: &PairingScope,
    name: &str,
    claimed_at_ms: i64,
) -> PairedDeviceAccess {
    let invitation = store
        .create_pairing_with_scope(name, claimed_at_ms - 1, claimed_at_ms + 60_000, scope)
        .expect("scoped invitation");
    store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            name,
            claimed_at_ms,
        )
        .expect("claim")
}

#[test]
fn an_invitation_without_a_scope_grants_every_active_vehicle_at_claim_time() {
    let fixture = grant_fixture();
    let store = &fixture.store;
    // Claimed in the same ordinary way the 80 existing call sites do.
    let invitation = store
        .create_pairing("default", 999, 60_000)
        .expect("invitation");
    let stored: i64 = store
        .open()
        .unwrap()
        .query_row(
            "SELECT all_vehicles FROM pairing_challenges WHERE pairing_id = ?1",
            [invitation.pairing_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, 1, "the default invitation is all-vehicles");
    // A vehicle registered after the invitation but before the claim counts:
    // all-vehicles means every vehicle active at claim time.
    let late = store
        .register_vehicle(&VehicleDescriptor::new(fixture.source_id, "late"), 500)
        .expect("late vehicle")
        .vehicle_id;
    let access = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "phone", 1_000)
        .expect("claim");
    assert_eq!(
        granted_vehicles(store, access.device_id),
        sorted(vec![fixture.alpha, fixture.beta, fixture.unpublished, late]),
        "every active vehicle, published or not; never the retired one"
    );
    let granted_at: Vec<i64> = store
        .open()
        .unwrap()
        .prepare("SELECT granted_at_ms FROM device_vehicle_grants WHERE device_id = ?1")
        .unwrap()
        .query_map([access.device_id.to_string()], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(granted_at.iter().all(|at_ms| *at_ms == 1_000));
    // A vehicle registered after the claim is never granted automatically.
    let after = store
        .register_vehicle(&VehicleDescriptor::new(fixture.source_id, "after"), 2_000)
        .expect("later vehicle")
        .vehicle_id;
    assert!(!granted_vehicles(store, access.device_id).contains(&after));
    assert_eq!(table_count(store, "pairing_challenges"), 0);
    assert_eq!(table_count(store, "pairing_challenge_vehicles"), 0);
}

#[test]
fn a_scoped_invitation_grants_exactly_the_named_vehicles() {
    let fixture = grant_fixture();
    let store = &fixture.store;
    let scope = PairingScope::Vehicles(vec![fixture.beta, fixture.unpublished, fixture.beta]);
    let invitation = store
        .create_pairing_with_scope("scoped", 999, 60_000, &scope)
        .expect("scoped invitation");
    let (all_vehicles, named): (i64, i64) = store
        .open()
        .unwrap()
        .query_row(
            "SELECT (SELECT all_vehicles FROM pairing_challenges WHERE pairing_id = ?1),
                    (SELECT COUNT(*) FROM pairing_challenge_vehicles WHERE pairing_id = ?1)",
            [invitation.pairing_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (all_vehicles, named),
        (0, 2),
        "a duplicate named vehicle is stored once; an unpublished one is allowed"
    );
    let access = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "phone", 1_000)
        .expect("claim");
    assert_eq!(
        granted_vehicles(store, access.device_id),
        sorted(vec![fixture.beta, fixture.unpublished])
    );
    assert_eq!(
        table_count(store, "pairing_challenge_vehicles"),
        0,
        "claiming deletes the challenge and its vehicle rows"
    );
}

#[test]
fn claim_skips_a_vehicle_retired_after_the_invitation_was_written() {
    let fixture = grant_fixture();
    let store = &fixture.store;
    let named = store
        .create_pairing_with_scope(
            "named",
            999,
            60_000,
            &PairingScope::Vehicles(vec![fixture.alpha, fixture.beta]),
        )
        .expect("named invitation");
    let all = store
        .create_pairing("all", 999, 60_000)
        .expect("all invitation");
    assert!(store.retire_vehicle(fixture.alpha, 1_500).expect("retire"));

    let named_access = store
        .claim_pairing(named.pairing_id, named.secret(), "named phone", 2_000)
        .expect("claim named");
    let all_access = store
        .claim_pairing(all.pairing_id, all.secret(), "all phone", 2_000)
        .expect("claim all");
    assert_eq!(
        granted_vehicles(store, named_access.device_id),
        vec![fixture.beta],
        "an explicit list skips a vehicle retired since the invitation"
    );
    assert_eq!(
        granted_vehicles(store, all_access.device_id),
        sorted(vec![fixture.beta, fixture.unpublished]),
        "all-vehicles skips retired vehicles too"
    );
}

#[test]
fn claim_writes_the_180_day_lifetime_cap_and_listing_shows_the_vehicles() {
    let fixture = grant_fixture();
    let store = &fixture.store;
    let first = claim_with_scope(
        store,
        &PairingScope::Vehicles(vec![fixture.beta]),
        "first",
        1_000,
    );
    let second = claim_with_scope(store, &PairingScope::AllVehicles, "second", 2_000);
    let connection = store.open().unwrap();
    for (device_id, claimed_at_ms) in [(first.device_id, 1_000_i64), (second.device_id, 2_000)] {
        let (created, expires, cap): (i64, i64, Option<i64>) = connection
            .query_row(
                "SELECT created_at_ms, expires_at_ms, max_expires_at_ms
                   FROM paired_devices WHERE device_id = ?1",
                [device_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(created, claimed_at_ms);
        assert_eq!(expires, claimed_at_ms + PAIRED_DEVICE_TOKEN_LIFETIME_MS);
        assert_eq!(cap, Some(claimed_at_ms + 15_552_000_000));
        assert_eq!(PAIRED_DEVICE_MAX_LIFETIME_MS, 15_552_000_000);
    }
    drop(connection);

    let listing = store.list_paired_devices_with_vehicles().expect("listing");
    assert_eq!(listing.len(), 2);
    assert_eq!(listing[0].device.device_id, first.device_id);
    assert_eq!(listing[0].vehicles, vec![fixture.beta]);
    assert_eq!(listing[1].device.device_id, second.device_id);
    assert_eq!(
        listing[1].vehicles,
        sorted(vec![fixture.alpha, fixture.beta, fixture.unpublished])
    );
    // The JSON keeps every PairedDeviceRecord member and adds `vehicles`.
    let json = serde_json::to_value(&listing[0]).expect("listing JSON");
    assert_eq!(json["device_id"], first.device_id.to_string());
    assert_eq!(json["display_name"], "first");
    assert_eq!(
        json["vehicles"],
        serde_json::json!([fixture.beta.to_string()])
    );
    assert!(json.get("token_sha256").is_none());
    // PairedDeviceRecord itself (the per-request principal) is unchanged.
    let plain = store.list_paired_devices().expect("plain list");
    assert_eq!(plain[0], listing[0].device);
    assert!(
        serde_json::to_value(&plain[0])
            .unwrap()
            .get("vehicles")
            .is_none()
    );
}

#[test]
fn a_pairing_scope_that_names_an_unknown_retired_nil_or_no_vehicle_stores_nothing() {
    let fixture = grant_fixture();
    let store = &fixture.store;
    let unknown = Uuid::new_v4();
    for bad in [unknown, fixture.retired, Uuid::nil()] {
        let scope = PairingScope::Vehicles(vec![fixture.alpha, bad]);
        assert!(
            matches!(
                store.create_pairing_with_scope("bad", 999, 60_000, &scope),
                Err(StoreError::PairingVehicleUnavailable(rejected)) if rejected == bad
            ),
            "{bad} must be rejected"
        );
        assert!(matches!(
            store.pairing_scope_vehicles(&[bad]),
            Err(StoreError::PairingVehicleUnavailable(rejected)) if rejected == bad
        ));
    }
    assert!(matches!(
        store.create_pairing_with_scope("empty", 999, 60_000, &PairingScope::Vehicles(Vec::new())),
        Err(StoreError::EmptyPairingScope)
    ));
    assert!(matches!(
        store.pairing_scope_vehicles(&[]),
        Err(StoreError::EmptyPairingScope)
    ));
    assert_eq!(table_count(store, "pairing_challenges"), 0);
    assert_eq!(
        table_count(store, "pairing_challenge_vehicles"),
        0,
        "the valid first vehicle of a rejected invitation is rolled back too"
    );
}

#[test]
fn persist_pairing_rechecks_retirement_inside_its_own_transaction() {
    let fixture = grant_fixture();
    let store = &fixture.store;
    let vehicles = vec![fixture.alpha, fixture.unpublished];
    // The CLI's early check passes: both are active, one has no manifest, and
    // an unnamed vehicle resolves with a null display name.
    let resolved = store.pairing_scope_vehicles(&vehicles).expect("resolve");
    assert_eq!(
        resolved
            .iter()
            .map(|vehicle| (vehicle.vehicle_id, vehicle.display_name.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            (fixture.alpha, Some("Alpha")),
            (fixture.unpublished, Some("Unpublished"))
        ]
    );
    let unnamed = store
        .pairing_scope_vehicles(&[fixture.beta])
        .expect("unnamed");
    assert_eq!(unnamed[0].display_name, None);

    let invitation = store.prepare_pairing("race", 999, 60_000).expect("prepare");
    // The vehicle is retired after the check and before the write.
    assert!(
        store
            .retire_vehicle(fixture.unpublished, 1_000)
            .expect("retire")
    );
    let scope = PairingScope::Vehicles(vehicles);
    assert!(matches!(
        store.persist_pairing_with_scope("race", &invitation, &scope),
        Err(StoreError::PairingVehicleUnavailable(rejected)) if rejected == fixture.unpublished
    ));
    assert_eq!(table_count(store, "pairing_challenges"), 0);
    assert_eq!(table_count(store, "pairing_challenge_vehicles"), 0);
    // The invitation was never stored, so it cannot be claimed.
    assert!(matches!(
        store.claim_pairing(invitation.pairing_id, invitation.secret(), "phone", 1_500),
        Err(StoreError::PairingRejected)
    ));
}

#[test]
fn vehicle_access_follows_grants_retirement_and_publication_on_every_request() {
    let fixture = grant_fixture();
    let store = &fixture.store;
    let device = claim_with_scope(
        store,
        &PairingScope::Vehicles(vec![fixture.alpha, fixture.unpublished]),
        "device",
        1_000,
    )
    .device_id;
    let scope = VehicleScope::Device(device);
    let access = |scope: VehicleScope, vehicle: Uuid, visibility: VehicleVisibility| {
        store
            .vehicle_accessible(scope, vehicle, visibility)
            .expect("access query")
    };
    use VehicleVisibility::{Active, PublishedAndActive};

    // Granted and active.
    assert!(access(scope, fixture.alpha, Active));
    assert!(access(scope, fixture.alpha, PublishedAndActive));
    // Granted, active, but unpublished: the sync routes answer it, the
    // published routes do not.
    assert!(access(scope, fixture.unpublished, Active));
    assert!(!access(scope, fixture.unpublished, PublishedAndActive));
    // Not granted: beta exists, is active and is published.
    assert!(!access(scope, fixture.beta, Active));
    assert!(!access(scope, fixture.beta, PublishedAndActive));
    // Unknown and nil are plain `false`, never an error.
    assert!(!access(scope, Uuid::new_v4(), Active));
    assert!(!access(scope, Uuid::nil(), Active));
    assert!(!access(VehicleScope::Unrestricted, Uuid::nil(), Active));
    // The unrestricted scope sees every active vehicle, but not a retired or
    // unknown one.
    assert!(access(
        VehicleScope::Unrestricted,
        fixture.beta,
        PublishedAndActive
    ));
    assert!(access(
        VehicleScope::Unrestricted,
        fixture.unpublished,
        Active
    ));
    assert!(!access(
        VehicleScope::Unrestricted,
        fixture.unpublished,
        PublishedAndActive
    ));
    assert!(!access(VehicleScope::Unrestricted, fixture.retired, Active));
    assert!(!access(VehicleScope::Unrestricted, Uuid::new_v4(), Active));
    // A device that does not exist holds no grant.
    assert!(!access(
        VehicleScope::Device(Uuid::new_v4()),
        fixture.alpha,
        Active
    ));

    // Retirement applies to the next request; the grant survives it, so
    // reactivation restores access without re-pairing.
    assert!(store.retire_vehicle(fixture.alpha, 2_000).expect("retire"));
    assert!(!access(scope, fixture.alpha, Active));
    assert!(store.reactivate_vehicle(fixture.alpha).expect("reactivate"));
    assert!(access(scope, fixture.alpha, Active));
    // De-granting (deleting the row) applies to the next request too.
    store
        .open()
        .unwrap()
        .execute(
            "DELETE FROM device_vehicle_grants WHERE device_id = ?1 AND vehicle_id = ?2",
            params![device.to_string(), fixture.alpha.to_string()],
        )
        .unwrap();
    assert!(!access(scope, fixture.alpha, Active));
    assert!(access(scope, fixture.unpublished, Active));
}

#[test]
fn the_nil_vehicle_is_never_accessible_and_never_an_error() {
    use VehicleVisibility::{Active, PublishedAndActive};
    let fixture = grant_fixture();
    let store = &fixture.store;
    let device = claim_with_scope(store, &PairingScope::AllVehicles, "device", 1_000).device_id;
    let nil = Uuid::nil();
    // Only `register_vehicle` keeps a row with the nil id out of the
    // catalogue. Plant one by hand, published and granted: the nil UUID is
    // still not a vehicle that any scope may be served.
    let connection = store.open().expect("open store");
    connection
        .execute(
            "INSERT INTO vehicles (vehicle_id, source_id, source_vehicle_key, created_at_ms, last_seen_at_ms)
             VALUES (?1, ?2, 'planted-nil', 1, 1)",
            params![nil.to_string(), fixture.source_id.to_string()],
        )
        .expect("plant a nil vehicle row");
    publish_vehicle_fixture(store, nil);
    connection
        .execute(
            "INSERT INTO device_vehicle_grants (device_id, vehicle_id, granted_at_ms) VALUES (?1, ?2, 1)",
            params![device.to_string(), nil.to_string()],
        )
        .expect("grant the planted row");
    for scope in [VehicleScope::Unrestricted, VehicleScope::Device(device)] {
        for visibility in [Active, PublishedAndActive] {
            assert!(
                !store
                    .vehicle_accessible(scope, nil, visibility)
                    .expect("the nil UUID is a plain false"),
                "{scope:?} {visibility:?}"
            );
        }
    }
    // It is answered before any lookup: a grant table that cannot be read is
    // an error for a real vehicle and still a plain false for the nil UUID,
    // so no route can turn the nil value into a 503.
    connection
        .execute_batch("DROP TABLE device_vehicle_grants")
        .expect("make the grant table unreadable");
    assert!(matches!(
        store.vehicle_accessible(VehicleScope::Device(device), nil, Active),
        Ok(false)
    ));
    assert!(
        store
            .vehicle_accessible(VehicleScope::Device(device), fixture.alpha, Active)
            .is_err()
    );
}

#[test]
fn published_vehicles_for_a_device_are_only_its_granted_ones() {
    let fixture = grant_fixture();
    let store = &fixture.store;
    let only_beta = claim_with_scope(
        store,
        &PairingScope::Vehicles(vec![fixture.beta]),
        "beta only",
        1_000,
    )
    .device_id;
    let nothing = claim_with_scope(
        store,
        &PairingScope::Vehicles(vec![fixture.unpublished]),
        "unpublished only",
        2_000,
    )
    .device_id;
    let all = claim_with_scope(store, &PairingScope::AllVehicles, "all", 3_000).device_id;
    let ids = |vehicles: Vec<PublishedVehicle>| -> Vec<Uuid> {
        vehicles
            .into_iter()
            .map(|vehicle| vehicle.vehicle_id)
            .collect()
    };

    // The global list is the published, active vehicles, newest last_seen first.
    assert_eq!(
        ids(store.published_vehicles().unwrap()),
        vec![fixture.beta, fixture.alpha]
    );
    assert_eq!(
        ids(store.published_vehicles_for_device(only_beta).unwrap()),
        vec![fixture.beta]
    );
    assert!(
        store
            .published_vehicles_for_device(nothing)
            .unwrap()
            .is_empty(),
        "a granted but unpublished vehicle is not listed"
    );
    assert_eq!(
        ids(store.published_vehicles_for_device(all).unwrap()),
        vec![fixture.beta, fixture.alpha]
    );
    assert!(
        store
            .published_vehicles_for_device(Uuid::new_v4())
            .unwrap()
            .is_empty(),
        "an unknown device is granted nothing"
    );
    assert_eq!(
        store
            .published_vehicles_in_scope(VehicleScope::Device(only_beta))
            .unwrap()[0]
            .display_name,
        None
    );
}

#[test]
fn deleting_a_vehicle_or_a_device_removes_its_grants_and_fails_closed() {
    let fixture = grant_fixture();
    let store = &fixture.store;
    let device = claim_with_scope(store, &PairingScope::AllVehicles, "device", 1_000).device_id;
    let scoped = store
        .create_pairing_with_scope(
            "scoped",
            999,
            60_000,
            &PairingScope::Vehicles(vec![fixture.unpublished]),
        )
        .expect("scoped invitation");
    let connection = store.open().unwrap();
    // Placeholder cleanup deletes vehicle rows: the CASCADE keeps that from
    // being blocked, and the grant and challenge rows go with the vehicle.
    connection
        .execute(
            "DELETE FROM vehicles WHERE vehicle_id = ?1",
            [fixture.unpublished.to_string()],
        )
        .expect("delete vehicle");
    assert!(!granted_vehicles(store, device).contains(&fixture.unpublished));
    assert_eq!(table_count(store, "pairing_challenge_vehicles"), 0);
    // The invitation survives with no vehicle and grants nothing.
    let access = store
        .claim_pairing(scoped.pairing_id, scoped.secret(), "empty phone", 2_000)
        .expect("claim");
    assert!(granted_vehicles(store, access.device_id).is_empty());
    assert!(
        !store
            .vehicle_accessible(
                VehicleScope::Device(access.device_id),
                fixture.alpha,
                VehicleVisibility::Active
            )
            .unwrap()
    );
    // Deleting a device removes its grants.
    connection
        .execute(
            "DELETE FROM paired_devices WHERE device_id = ?1",
            [device.to_string()],
        )
        .expect("delete device");
    assert!(granted_vehicles(store, device).is_empty());
}
