// SPDX-License-Identifier: AGPL-3.0-only

// Paired-device credential lifetime: the absolute cap, coalesced authentication
// writes, the sealed rotation successor and rotation at the cap. Real SQLite,
// synthetic bearers and device names.

const CRED_DAY_MS: i64 = 24 * 60 * 60 * 1_000;
const CRED_GRACE_MS: i64 = PAIRED_DEVICE_ROTATION_GRACE_MS;
const CRED_WRITE_MS: i64 = PAIRED_DEVICE_AUTH_WRITE_INTERVAL_MS;
const CRED_DEVICE: &str = "11111111-1111-4111-8111-111111111111";
const CRED_BEARER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn credential_store() -> (tempfile::TempDir, HubStore) {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    (temporary, store)
}

fn credential_claim(
    store: &HubStore,
    claimed_at_ms: i64,
    max_lifetime_ms: i64,
) -> PairedDeviceAccess {
    let invitation = store
        .create_pairing("credentials", claimed_at_ms - 1, claimed_at_ms + 60_000)
        .expect("pairing creates");
    store
        .claim_pairing_with_max_lifetime(
            invitation.pairing_id,
            invitation.secret(),
            "phone",
            claimed_at_ms,
            max_lifetime_ms,
        )
        .expect("claim succeeds")
}

/// Every column of every paired device and grace row, so a test can prove a
/// call changed nothing (or exactly what it should).
fn credential_rows(store: &HubStore) -> Vec<String> {
    let connection = store.open().expect("catalogue");
    let mut rows = Vec::new();
    let mut devices = connection
        .prepare(
            "SELECT device_id, hex(token_sha256), created_at_ms, expires_at_ms, revoked_at_ms,
                    last_authenticated_at_ms, max_expires_at_ms
               FROM paired_devices ORDER BY device_id",
        )
        .expect("device statement");
    rows.extend(
        devices
            .query_map([], |row| {
                Ok(format!(
                    "device {} {} {} {} {:?} {:?} {:?}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            })
            .expect("device rows")
            .map(|row| row.expect("device row")),
    );
    let mut graces = connection
        .prepare(
            "SELECT device_id, hex(token_sha256), valid_until_ms, hex(sealed_successor)
               FROM paired_device_token_grace ORDER BY device_id",
        )
        .expect("grace statement");
    rows.extend(
        graces
            .query_map([], |row| {
                Ok(format!(
                    "grace {} {} {} {:?}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .expect("grace rows")
            .map(|row| row.expect("grace row")),
    );
    rows
}

/// Log every UPDATE of `paired_devices` into a table, so a test counts the
/// writes an authentication performs (a no-op UPDATE counts too).
fn credential_log_updates(store: &HubStore) {
    store
        .open()
        .expect("catalogue")
        .execute_batch(
            "CREATE TABLE credential_update_log (
                 device_id TEXT NOT NULL,
                 expires_at_ms INTEGER NOT NULL,
                 last_authenticated_at_ms INTEGER
             );
             CREATE TRIGGER credential_update_log_trigger AFTER UPDATE ON paired_devices
             BEGIN
                 INSERT INTO credential_update_log
                     VALUES (NEW.device_id, NEW.expires_at_ms, NEW.last_authenticated_at_ms);
             END;",
        )
        .expect("update log");
}

fn credential_logged_updates(store: &HubStore) -> i64 {
    store
        .open()
        .expect("catalogue")
        .query_row("SELECT COUNT(*) FROM credential_update_log", [], |row| {
            row.get(0)
        })
        .expect("update count")
}

fn credential_sql(store: &HubStore, statement: &str, params: impl rusqlite::Params) {
    store
        .open()
        .expect("catalogue")
        .execute(statement, params)
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

fn credential_sealed(store: &HubStore, device_id: Uuid) -> Option<Vec<u8>> {
    store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT sealed_successor FROM paired_device_token_grace WHERE device_id = ?1",
            [device_id.to_string()],
            |row| row.get(0),
        )
        .expect("grace row")
}

fn credential_device_uuid() -> Uuid {
    Uuid::parse_str(CRED_DEVICE).expect("fixture device id")
}

fn credential_rejected(result: Result<PairedDeviceAccess, StoreError>) -> bool {
    matches!(result, Err(StoreError::PairingRejected))
}

// ------------------------------------------------------------ construction

#[test]
fn hkdf_sha256_matches_rfc_5869_test_case_1() {
    // RFC 5869 appendix A.1: SHA-256, IKM 22 x 0x0b, salt 000102..0c, info
    // f0f1..f9, L = 42. The helper returns the single block that is the first
    // 32 bytes of that output.
    let ikm = [0x0b_u8; 22];
    let salt: Vec<u8> = (0x00..=0x0c).collect();
    let info: Vec<u8> = (0xf0..=0xf9).collect();
    let prk = hkdf_sha256_extract(&salt, &ikm);
    assert_eq!(
        hex::encode(*prk),
        "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"
    );
    let first_block = hkdf_sha256_expand_first_block(&prk, &info);
    assert_eq!(
        hex::encode(*first_block),
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf"
    );
}

#[test]
fn the_grace_key_is_pinned_to_its_salt_bearer_and_device() {
    // Known answers computed independently (Python hmac) from the construction
    // in the decision: salt "teslatlas-hub grace successor v1", IKM = the
    // bearer as transmitted, info = the device id's text, one expand block.
    let other_bearer = "fedcba9876543210".repeat(4);
    let other_device = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
    assert_eq!(
        hex::encode(*grace_successor_key(CRED_BEARER, credential_device_uuid())),
        "39a471cbe1ec4f5c230d1102a74b6fb3da051e1c78275ecbfca6c39fc6bb4c2d"
    );
    assert_eq!(
        hex::encode(*grace_successor_key(
            &other_bearer,
            credential_device_uuid()
        )),
        "30fb6d166b748717ae1cc0370a36c9de2f6fc53e7d7d0e32958e6ec0a49bd0b3"
    );
    assert_eq!(
        hex::encode(*grace_successor_key(CRED_BEARER, other_device)),
        "fccc7dbb961bf563a839a8566f06da840f2ff0348cfb6f599fc9bc70de4f9e53"
    );
}

#[test]
fn a_sealed_successor_is_nonce_then_ciphertext_and_tag_bound_to_key_and_device() {
    let device = credential_device_uuid();
    let key = grace_successor_key(CRED_BEARER, device);
    let successor = "fedcba9876543210".repeat(4);
    let sealed = seal_successor(&key, device, &successor).expect("seal");

    // 12-byte nonce || ciphertext (as long as the 64-character token) || 16-byte tag.
    assert_eq!(sealed.len(), 12 + 64 + 16);
    let cipher = Aes256Gcm::new_from_slice(&*key).expect("cipher");
    let nonce = Nonce::try_from(&sealed[..12]).expect("nonce prefix");
    let opened = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &sealed[12..],
                aad: device.to_string().as_bytes(),
            },
        )
        .expect("the layout is nonce, then ciphertext and tag, with the device id as AAD");
    assert_eq!(opened, successor.as_bytes());
    assert!(
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &sealed[12..],
                    aad: b"",
                },
            )
            .is_err(),
        "the device id is part of what the tag authenticates"
    );
    assert!(
        !sealed
            .windows(successor.len())
            .any(|window| window == successor.as_bytes()),
        "the successor is not in the blob in clear"
    );

    // The same plaintext seals to a different blob every time: the nonce is random.
    let again = seal_successor(&key, device, &successor).expect("second seal");
    assert_ne!(sealed[..12], again[..12]);
    assert_ne!(sealed, again);

    // Opening needs the key, the device and every byte.
    assert_eq!(
        unseal_successor(&key, device, &sealed)
            .as_deref()
            .map(Vec::as_slice),
        Some(successor.as_bytes())
    );
    let other_device = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
    assert!(
        unseal_successor(&key, other_device, &sealed).is_none(),
        "another device's id is another associated data"
    );
    let other_key = grace_successor_key(&successor, device);
    assert!(unseal_successor(&other_key, device, &sealed).is_none());
    for index in [0, 11, 12, 40, 75, 76, sealed.len() - 1] {
        let mut tampered = sealed.clone();
        tampered[index] ^= 0x01;
        assert!(
            unseal_successor(&key, device, &tampered).is_none(),
            "byte {index} is authenticated"
        );
    }
    for length in [0, 11, 12, 27] {
        assert!(unseal_successor(&key, device, &sealed[..length]).is_none());
    }
    assert!(unseal_successor(&key, device, &sealed[..sealed.len() - 1]).is_none());
}

// ---------------------------------------------------------------- the grace

#[test]
fn rotation_seals_the_successor_under_a_key_only_the_replaced_bearer_derives() {
    let (_temporary, store) = credential_store();
    let access = credential_claim(&store, 15_000, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let first = access.access_token.as_bearer().to_owned();
    let rotated = store.rotate_device(&first, 16_000).expect("rotation");
    assert!(!rotated.at_maximum_lifetime);
    let second = rotated.access_token.as_bearer().to_owned();
    assert_ne!(first, second);

    let sealed = credential_sealed(&store, access.device_id).expect("a sealed successor");
    assert_eq!(sealed.len(), 12 + 64 + 16);
    let key = grace_successor_key(&first, access.device_id);
    assert_eq!(
        unseal_successor(&key, access.device_id, &sealed)
            .as_deref()
            .map(Vec::as_slice),
        Some(second.as_bytes()),
        "the replaced bearer opens exactly the successor"
    );
    assert!(
        unseal_successor(
            &grace_successor_key(&second, access.device_id),
            access.device_id,
            &sealed
        )
        .is_none(),
        "the successor does not open its own seal"
    );
    assert!(
        !sealed
            .windows(second.len())
            .any(|window| window == second.as_bytes()),
        "the stored blob never holds the successor in clear"
    );
    let connection = store.open().unwrap();
    let (grace_digest, valid_until): (Vec<u8>, i64) = connection
        .query_row(
            "SELECT token_sha256, valid_until_ms FROM paired_device_token_grace",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(grace_digest, sha256_bytes(first.as_bytes()).to_vec());
    assert_eq!(valid_until, 16_000 + CRED_GRACE_MS);
    let current_digest: Vec<u8> = connection
        .query_row("SELECT token_sha256 FROM paired_devices", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(current_digest, sha256_bytes(second.as_bytes()).to_vec());
}

#[test]
fn a_grace_retry_returns_exactly_the_sealed_successor_and_changes_nothing() {
    let (_temporary, store) = credential_store();
    let access = credential_claim(&store, 15_000, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let old = access.access_token.as_bearer().to_owned();
    let rotated = store.rotate_device(&old, 16_000).expect("rotation");
    let successor = rotated.access_token.as_bearer().to_owned();
    let before = credential_rows(&store);

    for at_ms in [
        16_001,
        16_000 + CRED_GRACE_MS / 2,
        16_000 + CRED_GRACE_MS - 1,
    ] {
        let retried = store
            .rotate_device(&old, at_ms)
            .expect("a lost-response retry inside the window");
        assert_eq!(
            retried.access_token.as_bearer(),
            successor,
            "the retry returns the successor, byte for byte"
        );
        assert_eq!(retried.device_id, access.device_id);
        assert_eq!(retried.expires_at_ms, rotated.expires_at_ms);
        assert!(
            !retried.at_maximum_lifetime,
            "a grace retry carries no reason"
        );
        assert_eq!(
            credential_rows(&store),
            before,
            "a grace retry mints nothing and writes nothing"
        );
    }

    // The window is exclusive at 24 hours, and a refusal writes nothing either.
    assert!(credential_rejected(
        store.rotate_device(&old, 16_000 + CRED_GRACE_MS)
    ));
    assert!(credential_rejected(
        store.rotate_device(&old, 16_000 + CRED_GRACE_MS + 1_000)
    ));
    assert_eq!(credential_rows(&store), before);
    assert!(
        store
            .authenticate_device_at(&old, 16_000 + CRED_GRACE_MS)
            .unwrap()
            .is_none()
    );
    // The successor the retry handed out is the live bearer and rotates on.
    assert!(
        store
            .authenticate_device_at(&successor, 16_000 + CRED_GRACE_MS)
            .unwrap()
            .is_some()
    );
    let next = store
        .rotate_device(&successor, 17_000 + CRED_GRACE_MS)
        .expect("the successor rotates");
    assert_ne!(next.access_token.as_bearer(), successor);
}

#[test]
fn a_grace_row_without_a_sealed_successor_or_with_a_bad_one_is_rejected() {
    let (_temporary, store) = credential_store();
    let access = credential_claim(&store, 15_000, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let old = access.access_token.as_bearer().to_owned();
    store.rotate_device(&old, 16_000).expect("rotation");
    let good = credential_sealed(&store, access.device_id).expect("sealed");
    assert!(store.rotate_device(&old, 16_001).is_ok());

    // A grace row from before schema 68 has nothing sealed.
    credential_sql(
        &store,
        "UPDATE paired_device_token_grace SET sealed_successor = NULL",
        [],
    );
    let before = credential_rows(&store);
    assert!(credential_rejected(store.rotate_device(&old, 16_001)));
    assert_eq!(credential_rows(&store), before, "a refusal writes nothing");
    assert!(
        store
            .authenticate_device_at(&old, 16_001)
            .unwrap()
            .is_some(),
        "the grace bearer still authenticates; only its rotation is refused"
    );

    for (label, blob) in [
        ("empty", Vec::new()),
        ("short", good[..20].to_vec()),
        ("truncated", good[..good.len() - 1].to_vec()),
        ("tampered", {
            let mut tampered = good.clone();
            tampered[30] ^= 0x80;
            tampered
        }),
        ("zeros", vec![0_u8; good.len()]),
    ] {
        credential_sql(
            &store,
            "UPDATE paired_device_token_grace SET sealed_successor = ?1",
            [blob],
        );
        assert!(
            credential_rejected(store.rotate_device(&old, 16_001)),
            "{label}"
        );
    }

    // A blob that is valid for another device does not open here (the key and
    // the associated data both name the device).
    let other_device = Uuid::new_v4();
    let successor = "a".repeat(64);
    let foreign = seal_successor(
        &grace_successor_key(&old, other_device),
        other_device,
        &successor,
    )
    .expect("foreign seal");
    credential_sql(
        &store,
        "UPDATE paired_device_token_grace SET sealed_successor = ?1",
        [foreign],
    );
    assert!(credential_rejected(store.rotate_device(&old, 16_001)));
    // Restored, the retry works again.
    credential_sql(
        &store,
        "UPDATE paired_device_token_grace SET sealed_successor = ?1",
        [good],
    );
    assert!(store.rotate_device(&old, 16_001).is_ok());
}

#[test]
fn a_successor_that_is_no_longer_the_current_bearer_is_never_returned() {
    let (_temporary, store) = credential_store();
    let access = credential_claim(&store, 15_000, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let old = access.access_token.as_bearer().to_owned();
    store.rotate_device(&old, 16_000).expect("rotation");
    assert!(store.rotate_device(&old, 16_001).is_ok());
    // The device's current bearer changes without the grace row following: the
    // sealed blob is intact and opens, but its plaintext is not current.
    credential_sql(
        &store,
        "UPDATE paired_devices SET token_sha256 = ?1",
        [sha256_bytes(b"some other bearer").to_vec()],
    );
    let before = credential_rows(&store);
    assert!(credential_rejected(store.rotate_device(&old, 16_002)));
    assert_eq!(credential_rows(&store), before);
}

#[test]
fn rotating_twice_keeps_one_grace_row_and_only_the_latest_replaced_bearer_retries() {
    let (_temporary, store) = credential_store();
    let access = credential_claim(&store, 15_000, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let first = access.access_token.as_bearer().to_owned();
    let second = store.rotate_device(&first, 16_000).expect("T1 to T2");
    let second_bearer = second.access_token.as_bearer().to_owned();
    let third = store
        .rotate_device(&second_bearer, 17_000)
        .expect("T2 to T3");
    let third_bearer = third.access_token.as_bearer().to_owned();
    assert_eq!(
        credential_rows(&store)
            .iter()
            .filter(|row| row.starts_with("grace"))
            .count(),
        1,
        "one grace row per device"
    );

    // T1 lost its grace with the second rotation.
    assert!(credential_rejected(store.rotate_device(&first, 17_001)));
    assert!(
        store
            .authenticate_device_at(&first, 17_001)
            .unwrap()
            .is_none()
    );
    // T2 is the replaced bearer now and retries to exactly T3.
    let retried = store
        .rotate_device(&second_bearer, 17_001)
        .expect("T2 retry");
    assert_eq!(retried.access_token.as_bearer(), third_bearer);
    assert!(
        store
            .authenticate_device_at(&second_bearer, 17_001)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .authenticate_device_at(&third_bearer, 17_001)
            .unwrap()
            .is_some()
    );
    // The sealed blob belongs to the replaced bearer T2 and opens to T3.
    let sealed = credential_sealed(&store, access.device_id).expect("sealed");
    assert_eq!(
        unseal_successor(
            &grace_successor_key(&second_bearer, access.device_id),
            access.device_id,
            &sealed
        )
        .as_deref()
        .map(Vec::as_slice),
        Some(third_bearer.as_bytes())
    );
}

#[test]
fn concurrent_rotations_with_one_bearer_issue_one_successor_and_agree_on_it() {
    // Several requests rotate the same current bearer at once (a client that
    // retries before the first answer arrives). Only one of them issues; the
    // others find the bearer replaced when they take the writer and are
    // answered with the successor that one sealed, so every caller holds the
    // same token and exactly that token is the device's current bearer.
    let (_temporary, store) = credential_store();
    let access = credential_claim(&store, 15_000, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let old = access.access_token.as_bearer().to_owned();
    credential_log_updates(&store);
    // Every request reads the bearer as current (and so decides to issue)
    // before any of them takes the writer: the hook is a rendezvous between
    // the read and the writer, so the in-transaction re-decision is exercised
    // exactly, not by luck of scheduling.
    let rendezvous = std::sync::Arc::new(BoundedRendezvous::new(6));
    store.set_credential_write_hook(Some(CredentialWriteHook::new({
        let rendezvous = rendezvous.clone();
        move |stage| {
            if stage == CredentialWriteStage::Rotate {
                rendezvous.wait();
            }
        }
    })));
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let (store, old) = (store.clone(), old.clone());
            std::thread::spawn(move || {
                store
                    .rotate_device(&old, 16_000)
                    .map(|access| access.access_token.as_bearer().to_owned())
            })
        })
        .collect();
    let tokens: Vec<String> = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("rotation thread")
                .expect("every concurrent rotation is answered")
        })
        .collect();
    store.set_credential_write_hook(None);
    assert!(tokens.iter().all(|token| token == &tokens[0]), "{tokens:?}");
    assert_ne!(tokens[0], old);
    assert_eq!(
        credential_logged_updates(&store),
        1,
        "one rotation moved the device's bearer; the other five only answered"
    );
    let connection = store.open().unwrap();
    let current: Vec<u8> = connection
        .query_row("SELECT token_sha256 FROM paired_devices", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(current, sha256_bytes(tokens[0].as_bytes()).to_vec());
    let (graces, grace_digest): (i64, Vec<u8>) = connection
        .query_row(
            "SELECT COUNT(*), token_sha256 FROM paired_device_token_grace",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(graces, 1);
    assert_eq!(grace_digest, sha256_bytes(old.as_bytes()).to_vec());
}

#[test]
fn rotation_refuses_a_revoked_expired_or_unknown_bearer_and_never_waits_for_the_writer() {
    let (_temporary, store) = credential_store();
    let access = credential_claim(&store, 15_000, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let old = access.access_token.as_bearer().to_owned();
    let rotated = store.rotate_device(&old, 16_000).expect("rotation");
    let current = rotated.access_token.as_bearer().to_owned();

    // Answers that need no write are answered while another connection holds
    // the writer: a grace retry, and every refusal.
    let mut writer = store.open().expect("writer");
    let transaction = writer
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("hold the writer lock");
    // A request that joined the writer queue would wait out the 5 second busy
    // timeout and then fail with SQLITE_BUSY, which none of these answers is.
    let retried = store
        .rotate_device(&old, 16_001)
        .expect("a grace retry is answered while the writer is held");
    assert_eq!(retried.access_token.as_bearer(), current);
    assert!(credential_rejected(
        store.rotate_device(&"b".repeat(64), 16_001)
    ));
    assert!(credential_rejected(store.rotate_device("short", 16_001)));
    transaction.rollback().expect("release the writer lock");
    drop(writer);

    let expiry = rotated.expires_at_ms;
    assert!(credential_rejected(store.rotate_device(&current, expiry)));
    assert!(credential_rejected(store.rotate_device(&old, expiry)));
    store
        .revoke_device_at(access.device_id, 16_500)
        .expect("revoke");
    assert!(credential_rejected(store.rotate_device(&current, 16_501)));
    assert!(
        credential_rejected(store.rotate_device(&old, 16_501)),
        "a revoked device's grace bearer cannot retry"
    );
}

/// The digest, the window end and the sealed blob of a device's grace row.
fn credential_grace(store: &HubStore, device_id: Uuid) -> (Vec<u8>, i64, Option<Vec<u8>>) {
    store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT token_sha256, valid_until_ms, sealed_successor
               FROM paired_device_token_grace WHERE device_id = ?1",
            [device_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("grace row")
}

fn credential_revoked_at(store: &HubStore, device_id: Uuid) -> Option<i64> {
    store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT revoked_at_ms FROM paired_devices WHERE device_id = ?1",
            [device_id.to_string()],
            |row| row.get(0),
        )
        .expect("device row")
}

fn assert_sealed(blob: &Option<Vec<u8>>, label: &str) {
    assert!(
        blob.as_ref()
            .is_some_and(|blob| blob.len() > SEALED_NONCE_BYTES + SEALED_TAG_BYTES),
        "{label}: a sealed successor is held"
    );
}

/// A device that claimed at `claimed_at_ms` and rotated once at
/// `rotated_at_ms`: its grace row holds the successor sealed under `old`, and
/// the window ends at `rotated_at_ms + CRED_GRACE_MS`.
struct RotatedDevice {
    device_id: Uuid,
    old: String,
    current: String,
}

fn rotated_device(store: &HubStore, claimed_at_ms: i64, rotated_at_ms: i64) -> RotatedDevice {
    let access = credential_claim(store, claimed_at_ms, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let old = access.access_token.as_bearer().to_owned();
    let rotated = store.rotate_device(&old, rotated_at_ms).expect("rotation");
    RotatedDevice {
        device_id: access.device_id,
        old,
        current: rotated.access_token.as_bearer().to_owned(),
    }
}

#[test]
fn revoking_a_device_clears_its_sealed_successor_and_nobody_elses() {
    let (_temporary, store) = credential_store();
    let first = rotated_device(&store, 15_000, 16_000);
    let second = rotated_device(&store, 15_100, 16_000);
    // Inside the window: the revocation does not wait for it to end.
    let (digest, valid_until, sealed) = credential_grace(&store, first.device_id);
    assert_sealed(&sealed, "before the revocation");
    assert!(valid_until > 16_500);
    let untouched = credential_grace(&store, second.device_id);
    assert_sealed(&untouched.2, "the other device");

    store
        .revoke_device_at(first.device_id, 16_500)
        .expect("revoke");
    assert_eq!(
        credential_grace(&store, first.device_id),
        (digest.clone(), valid_until, None),
        "only the sealed successor goes; the row, its digest and its window stay"
    );
    assert_eq!(
        credential_grace(&store, second.device_id),
        untouched,
        "another device's grace row is untouched"
    );
    assert_eq!(credential_revoked_at(&store, first.device_id), Some(16_500));
    assert!(credential_rejected(store.rotate_device(&first.old, 16_501)));

    // A revocation that finds the device already revoked keeps the first time
    // and still clears a successor an earlier revocation left behind.
    credential_sql(
        &store,
        "UPDATE paired_device_token_grace SET sealed_successor = ?1 WHERE device_id = ?2",
        params![vec![7_u8; 64], first.device_id.to_string()],
    );
    store
        .revoke_device_at(first.device_id, 17_000)
        .expect("a repeat revocation");
    assert_eq!(credential_revoked_at(&store, first.device_id), Some(16_500));
    assert_eq!(
        credential_grace(&store, first.device_id),
        (digest, valid_until, None)
    );

    // A device that is not there changes nothing.
    assert!(matches!(
        store.revoke_device_at(Uuid::new_v4(), 17_000),
        Err(StoreError::PairingRejected)
    ));
    assert_eq!(credential_grace(&store, second.device_id), untouched);
    assert_eq!(credential_revoked_at(&store, second.device_id), None);
}

#[test]
fn the_authentication_write_after_the_window_clears_the_device_sealed_successor() {
    let (_temporary, store) = credential_store();
    let first = rotated_device(&store, 15_000, 16_000);
    let second = rotated_device(&store, 15_100, 16_000);
    let window_end = 16_000 + CRED_GRACE_MS;
    let sealed = credential_grace(&store, first.device_id);
    let other = credential_grace(&store, second.device_id);
    assert_eq!(sealed.1, window_end);
    assert_sealed(&sealed.2, "first");
    assert_sealed(&other.2, "second");
    credential_log_updates(&store);

    // The grace bearer authenticates and writes nothing.
    assert!(
        store
            .authenticate_device_at(&first.old, window_end - 1)
            .unwrap()
            .is_some()
    );
    assert_eq!(credential_logged_updates(&store), 0);
    assert_eq!(credential_grace(&store, first.device_id), sealed);
    // A coalesced write inside the window keeps the successor: a retry can
    // still ask for it.
    store
        .authenticate_device_at(&first.current, window_end - 1)
        .unwrap()
        .expect("the current bearer writes");
    assert_eq!(credential_logged_updates(&store), 1);
    assert_eq!(credential_grace(&store, first.device_id), sealed);
    // The window has ended, but this request is inside the coalescing interval
    // and writes nothing, so nothing is cleared yet.
    store
        .authenticate_device_at(&first.current, window_end + 1_000)
        .unwrap()
        .expect("read-only request");
    assert_eq!(credential_logged_updates(&store), 1);
    assert_eq!(credential_grace(&store, first.device_id), sealed);
    // The next write after the window drops the successor. The row, its digest
    // and its window stay, and another device's row is not this request's.
    store
        .authenticate_device_at(&first.current, window_end + CRED_WRITE_MS + 1_000)
        .unwrap()
        .expect("the current bearer writes again");
    assert_eq!(credential_logged_updates(&store), 2);
    assert_eq!(
        credential_grace(&store, first.device_id),
        (sealed.0.clone(), window_end, None)
    );
    assert_eq!(credential_grace(&store, second.device_id), other);
    // The ended window refuses the grace bearer, as it did before.
    assert!(
        store
            .authenticate_device_at(&first.old, window_end + CRED_WRITE_MS + 2_000)
            .unwrap()
            .is_none()
    );
}

#[test]
fn the_window_end_itself_counts_as_ended_for_the_authentication_write() {
    for (at_ms, cleared) in [
        (16_000 + CRED_GRACE_MS - 1, false),
        (16_000 + CRED_GRACE_MS, true),
    ] {
        let (_temporary, store) = credential_store();
        let device = rotated_device(&store, 15_000, 16_000);
        store
            .authenticate_device_at(&device.current, at_ms)
            .unwrap()
            .expect("the current bearer writes");
        let (_, window_end, sealed) = credential_grace(&store, device.device_id);
        assert_eq!(window_end, 16_000 + CRED_GRACE_MS);
        assert_eq!(sealed.is_none(), cleared, "authenticated at {at_ms}");
    }
}

#[test]
fn the_repair_pass_clears_the_sealed_successor_of_ended_windows_only() {
    let (_temporary, store) = credential_store();
    // The second device rotated half a day after the first, so its window ends
    // half a day after the first one's.
    let first = rotated_device(&store, 15_000, 16_000);
    let second = rotated_device(&store, 15_100, 16_000 + CRED_GRACE_MS / 2);
    let first_end = 16_000 + CRED_GRACE_MS;
    let second_end = 16_000 + CRED_GRACE_MS / 2 + CRED_GRACE_MS;
    let first_before = credential_grace(&store, first.device_id);
    let second_before = credential_grace(&store, second.device_id);
    assert_eq!((first_before.1, second_before.1), (first_end, second_end));
    assert_sealed(&first_before.2, "first");
    assert_sealed(&second_before.2, "second");

    // One millisecond before the first window ends there is nothing to clear.
    store.repair_at(first_end - 1).expect("repair");
    assert_eq!(credential_grace(&store, first.device_id), first_before);
    assert_eq!(credential_grace(&store, second.device_id), second_before);

    // At the window end the first device's successor goes, and only that: the
    // row, its digest and its window stay, and the second window is open.
    store.repair_at(first_end).expect("repair");
    assert_eq!(
        credential_grace(&store, first.device_id),
        (first_before.0.clone(), first_end, None)
    );
    assert_eq!(credential_grace(&store, second.device_id), second_before);
    let rows = credential_rows(&store);
    store.repair_at(first_end).expect("a repeated repair");
    assert_eq!(credential_rows(&store), rows, "a repeat changes nothing");

    // The open window still answers its retry after the repair.
    let retried = store
        .rotate_device(&second.old, first_end + 1)
        .expect("the second device's retry");
    assert_eq!(retried.access_token.as_bearer(), second.current);
    assert!(credential_rejected(
        store.rotate_device(&first.old, first_end + 1)
    ));

    store.repair_at(second_end).expect("repair");
    assert_eq!(
        credential_grace(&store, second.device_id),
        (second_before.0, second_end, None)
    );
    assert!(credential_rejected(
        store.rotate_device(&second.old, second_end)
    ));
}

// ------------------------------------------------------------------ the cap

#[test]
fn claim_writes_the_configured_cap_and_the_first_bearer_never_outlives_it() {
    let (_temporary, store) = credential_store();
    for (days, expected_expiry_days) in [(10_i64, 10_i64), (30, 30), (60, 30), (3650, 30)] {
        let access = credential_claim(&store, 1_000, days * CRED_DAY_MS);
        assert_eq!(
            access.expires_at_ms,
            1_000 + expected_expiry_days * CRED_DAY_MS
        );
        assert!(!access.at_maximum_lifetime);
        let (expires, maximum): (i64, i64) = store
            .open()
            .unwrap()
            .query_row(
                "SELECT expires_at_ms, max_expires_at_ms FROM paired_devices WHERE device_id = ?1",
                [access.device_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(maximum, 1_000 + days * CRED_DAY_MS, "{days} days");
        assert_eq!(expires, access.expires_at_ms);
    }

    // The two-argument claim is the default span.
    let invitation = store.create_pairing("default", 1_000, 60_000).unwrap();
    let access = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "phone", 2_000)
        .unwrap();
    let maximum: i64 = store
        .open()
        .unwrap()
        .query_row(
            "SELECT max_expires_at_ms FROM paired_devices WHERE device_id = ?1",
            [access.device_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(maximum, 2_000 + PAIRED_DEVICE_MAX_LIFETIME_MS);

    // A span that is not positive is refused before the invitation is read.
    let invitation = store.create_pairing("spans", 1_000, 60_000).unwrap();
    for span in [0, -1, i64::MIN] {
        assert!(matches!(
            store.claim_pairing_with_max_lifetime(
                invitation.pairing_id,
                invitation.secret(),
                "phone",
                2_000,
                span
            ),
            Err(StoreError::InvalidDeviceMaxLifetime)
        ));
    }
    assert!(
        store
            .claim_pairing_with_max_lifetime(
                invitation.pairing_id,
                invitation.secret(),
                "phone",
                2_000,
                1
            )
            .is_ok(),
        "the refusals consumed nothing"
    );
    // A span past the end of time saturates, and the column CHECK still holds.
    let invitation = store.create_pairing("saturate", 1_000, 60_000).unwrap();
    let access = store
        .claim_pairing_with_max_lifetime(
            invitation.pairing_id,
            invitation.secret(),
            "phone",
            2_000,
            i64::MAX,
        )
        .expect("saturating claim");
    assert_eq!(
        access.expires_at_ms,
        2_000 + PAIRED_DEVICE_TOKEN_LIFETIME_MS
    );
}

#[test]
fn authentication_is_refused_at_the_absolute_maximum_and_never_slides_past_it() {
    let (_temporary, store) = credential_store();
    let claimed = 1_000;
    let maximum = claimed + 40 * CRED_DAY_MS;
    let access = credential_claim(&store, claimed, 40 * CRED_DAY_MS);
    let bearer = access.access_token.as_bearer().to_owned();

    // 20 days in, a slide to now + 30 days would be day 50: it stops at day 40.
    let used = store
        .authenticate_device_at(&bearer, claimed + 20 * CRED_DAY_MS)
        .unwrap()
        .expect("authenticated before the maximum");
    assert_eq!(used.expires_at_ms, maximum);
    let stored: i64 = store
        .open()
        .unwrap()
        .query_row("SELECT expires_at_ms FROM paired_devices", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(stored, maximum, "the stored expiry is clamped too");

    let last_good = store
        .authenticate_device_at(&bearer, maximum - 1)
        .unwrap()
        .expect("the last millisecond before the maximum");
    assert_eq!(last_good.expires_at_ms, maximum);
    for at_ms in [maximum, maximum + 1, maximum + 100 * CRED_DAY_MS] {
        assert!(
            store
                .authenticate_device_at(&bearer, at_ms)
                .unwrap()
                .is_none(),
            "401 at and after the maximum ({at_ms})"
        );
    }
    let stored_after: i64 = store
        .open()
        .unwrap()
        .query_row("SELECT expires_at_ms FROM paired_devices", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(stored_after, maximum, "refusals write nothing");

    // The maximum is a limit of its own, not only the stored expiry reaching
    // it: a row whose expiry lies beyond it (a hand-edited catalogue) is
    // served as ending at the maximum and refused from then on. With the stamp
    // cleared the request writes, and the write pulls the expiry back to the
    // maximum.
    credential_sql(
        &store,
        "UPDATE paired_devices SET expires_at_ms = ?1, last_authenticated_at_ms = NULL",
        [maximum + 10 * CRED_DAY_MS],
    );
    let clamped = store
        .authenticate_device_at(&bearer, maximum - 1)
        .unwrap()
        .expect("before the maximum");
    assert_eq!(
        clamped.expires_at_ms, maximum,
        "never reported beyond the maximum"
    );
    assert_eq!(
        store.list_paired_devices().unwrap()[0].expires_at_ms,
        maximum,
        "the write clamps the stored expiry to the maximum"
    );
    credential_sql(
        &store,
        "UPDATE paired_devices SET expires_at_ms = ?1",
        [maximum + 10 * CRED_DAY_MS],
    );
    assert!(
        store
            .authenticate_device_at(&bearer, maximum)
            .unwrap()
            .is_none(),
        "the maximum refuses a bearer whose stored expiry is later"
    );
    assert!(credential_rejected(store.rotate_device(&bearer, maximum)));
}

#[test]
fn a_grace_bearer_is_refused_at_the_maximum_even_inside_its_window() {
    let (_temporary, store) = credential_store();
    let access = credential_claim(&store, 1_000, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let old = access.access_token.as_bearer().to_owned();
    // A device two hours from its cap whose stored expiry is a little earlier.
    let maximum = 1_000 + 2 * 60 * 60 * 1_000;
    credential_sql(
        &store,
        "UPDATE paired_devices SET max_expires_at_ms = ?1, expires_at_ms = ?2",
        [maximum, maximum - 30 * 60 * 1_000],
    );
    // Rotating 10 minutes in issues a successor that expires at the maximum;
    // the grace window (24 hours) reaches far beyond it.
    let rotated = store
        .rotate_device(&old, 1_000 + 600_000)
        .expect("rotation");
    assert_eq!(rotated.expires_at_ms, maximum);
    assert!(!rotated.at_maximum_lifetime, "this rotation issued");
    assert!(
        store
            .authenticate_device_at(&old, maximum - 1)
            .unwrap()
            .is_some()
    );
    let retried = store
        .rotate_device(&old, maximum - 1)
        .expect("retry before the cap");
    assert_eq!(
        retried.access_token.as_bearer(),
        rotated.access_token.as_bearer()
    );
    assert_eq!(retried.expires_at_ms, maximum);
    assert!(
        !retried.at_maximum_lifetime,
        "a grace retry never carries the reason"
    );
    // With the stored expiry pushed beyond the maximum, only the maximum ends
    // the bearers: the grace one inside its window, and the current one.
    credential_sql(
        &store,
        "UPDATE paired_devices SET expires_at_ms = ?1",
        [maximum + 10 * CRED_DAY_MS],
    );
    let current = rotated.access_token.as_bearer().to_owned();
    // The two answers that skip the writer report the capped expiry too, never
    // the stored one beyond the maximum: the grace retry (the successor) ...
    let retried = store
        .rotate_device(&old, maximum - 1)
        .expect("grace retry over a stored expiry beyond the maximum");
    assert_eq!(
        retried.access_token.as_bearer(),
        rotated.access_token.as_bearer()
    );
    assert_eq!(retried.expires_at_ms, maximum);
    assert!(!retried.at_maximum_lifetime);
    // ... and the current bearer's answer at the maximum.
    let at_maximum = store
        .rotate_device(&current, maximum - 1)
        .expect("rotation of a bearer whose stored expiry is beyond the maximum");
    assert_eq!(at_maximum.access_token.as_bearer(), current);
    assert_eq!(at_maximum.expires_at_ms, maximum);
    assert!(at_maximum.at_maximum_lifetime);
    for token in [&old, &current] {
        let device = store
            .authenticate_device_at(token, maximum - 1)
            .unwrap()
            .expect("before the maximum");
        assert_eq!(device.expires_at_ms, maximum);
        assert!(
            store
                .authenticate_device_at(token, maximum)
                .unwrap()
                .is_none()
        );
        assert!(credential_rejected(store.rotate_device(token, maximum)));
    }
}

#[test]
fn rotation_issues_until_the_successor_reaches_the_maximum_and_then_answers_at_the_maximum() {
    let (_temporary, store) = credential_store();
    let claimed = 1_000;
    let maximum = claimed + 35 * CRED_DAY_MS;
    let access = credential_claim(&store, claimed, 35 * CRED_DAY_MS);
    let first = access.access_token.as_bearer().to_owned();

    // Ten days in the stored expiry (day 30) is still below the maximum
    // (day 35): rotation issues, and the successor's expiry is the maximum.
    let at_ms = claimed + 10 * CRED_DAY_MS;
    let issued = store.rotate_device(&first, at_ms).expect("rotation issues");
    assert!(!issued.at_maximum_lifetime);
    assert_ne!(issued.access_token.as_bearer(), first);
    assert_eq!(issued.expires_at_ms, maximum);

    // From here every rotation is at the maximum: the presented bearer comes
    // back, the expiry is unchanged, and nothing is written.
    let current = issued.access_token.as_bearer().to_owned();
    let before = credential_rows(&store);
    let at_maximum = store
        .rotate_device(&current, at_ms + 1_000)
        .expect("rotation at the maximum");
    assert!(at_maximum.at_maximum_lifetime);
    assert_eq!(at_maximum.access_token.as_bearer(), current);
    assert_eq!(at_maximum.device_id, access.device_id);
    assert_eq!(at_maximum.expires_at_ms, maximum);
    assert_eq!(credential_rows(&store), before, "nothing was written");
    // Repeated, and at the last millisecond, the same.
    let last = store
        .rotate_device(&current, maximum - 1)
        .expect("last millisecond");
    assert!(last.at_maximum_lifetime);
    assert_eq!(last.access_token.as_bearer(), current);
    assert_eq!(credential_rows(&store), before);
    // The grace bearer replaced by the issue is not at the maximum, it retries.
    let retried = store.rotate_device(&first, at_ms + 2_000).expect("retry");
    assert_eq!(retried.access_token.as_bearer(), current);
    assert!(!retried.at_maximum_lifetime);
    // After the maximum the bearer is simply expired.
    assert!(credential_rejected(store.rotate_device(&current, maximum)));
    assert!(credential_rejected(
        store.rotate_device(&current, maximum + CRED_DAY_MS)
    ));
}

#[test]
fn a_device_claimed_with_a_short_span_is_at_the_maximum_from_the_start() {
    let (_temporary, store) = credential_store();
    let access = credential_claim(&store, 1_000, 5 * CRED_DAY_MS);
    let maximum = 1_000 + 5 * CRED_DAY_MS;
    assert_eq!(access.expires_at_ms, maximum);
    let bearer = access.access_token.as_bearer().to_owned();
    let before = credential_rows(&store);
    let answered = store
        .rotate_device(&bearer, 1_000 + CRED_DAY_MS)
        .expect("rotation");
    assert!(answered.at_maximum_lifetime);
    assert_eq!(answered.access_token.as_bearer(), bearer);
    assert_eq!(answered.expires_at_ms, maximum);
    assert_eq!(credential_rows(&store), before);
    assert_eq!(
        store
            .open()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM paired_device_token_grace",
                [],
                |row| { row.get::<_, i64>(0) }
            )
            .unwrap(),
        0,
        "no grace row either"
    );
}

#[test]
fn a_null_maximum_means_no_cap() {
    let (_temporary, store) = credential_store();
    let access = credential_claim(&store, 1_000, 40 * CRED_DAY_MS);
    credential_sql(
        &store,
        "UPDATE paired_devices SET max_expires_at_ms = NULL",
        [],
    );
    let bearer = access.access_token.as_bearer().to_owned();
    // Far past the claim's 40 days the bearer still authenticates and slides.
    let day_100 = 1_000 + 100 * CRED_DAY_MS;
    let first = store
        .authenticate_device_at(&bearer, 1_000 + 29 * CRED_DAY_MS)
        .unwrap()
        .expect("authenticated");
    assert_eq!(first.expires_at_ms, 1_000 + 59 * CRED_DAY_MS);
    let used = store
        .authenticate_device_at(&bearer, 1_000 + 58 * CRED_DAY_MS)
        .unwrap()
        .expect("still valid");
    assert_eq!(used.expires_at_ms, 1_000 + 88 * CRED_DAY_MS);
    let rotated = store
        .rotate_device(&bearer, 1_000 + 87 * CRED_DAY_MS)
        .expect("rotation");
    assert!(!rotated.at_maximum_lifetime);
    assert_eq!(rotated.expires_at_ms, 1_000 + 117 * CRED_DAY_MS);
    assert!(day_100 < rotated.expires_at_ms);
}

// ----------------------------------------------------------- coalesced writes

#[test]
fn one_hundred_authentications_within_fifteen_minutes_write_once() {
    let (_temporary, store) = credential_store();
    let claimed = 1_000_000;
    let access = credential_claim(&store, claimed, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let bearer = access.access_token.as_bearer().to_owned();
    credential_log_updates(&store);
    // Every request that goes on to the writer is counted: the decision made
    // from the read-only snapshot must be the one the UPDATE confirms.
    let write_decisions = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    store.set_credential_write_hook(Some(CredentialWriteHook::new({
        let write_decisions = write_decisions.clone();
        move |stage| {
            if stage == CredentialWriteStage::Authenticate {
                write_decisions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    })));

    for second in 1..=100 {
        let device = store
            .authenticate_device_at(&bearer, claimed + second * 1_000)
            .unwrap()
            .expect("authenticated");
        assert_eq!(device.device_id, access.device_id);
        // The first request stamps the device (its stamp was empty); the rest
        // see what that write stored.
        assert_eq!(device.last_authenticated_at_ms, Some(claimed + 1_000));
        assert_eq!(
            device.expires_at_ms,
            claimed + 1_000 + PAIRED_DEVICE_TOKEN_LIFETIME_MS
        );
    }
    assert_eq!(
        credential_logged_updates(&store),
        1,
        "one UPDATE for 100 requests"
    );
    let stored = store.list_paired_devices().unwrap();
    assert_eq!(stored[0].last_authenticated_at_ms, Some(claimed + 1_000));
    assert_eq!(
        stored[0].expires_at_ms,
        claimed + 1_000 + PAIRED_DEVICE_TOKEN_LIFETIME_MS
    );

    // A clock that steps back does not write, and never moves the stamp back.
    let regressed = store
        .authenticate_device_at(&bearer, claimed + 500)
        .unwrap()
        .expect("a request from before the stamp");
    assert_eq!(regressed.last_authenticated_at_ms, Some(claimed + 1_000));
    assert_eq!(credential_logged_updates(&store), 1);
    assert_eq!(
        store.list_paired_devices().unwrap()[0].last_authenticated_at_ms,
        Some(claimed + 1_000)
    );

    // The interval is strict: exactly 15 minutes later is still read-only, one
    // millisecond more writes, and the write carries the new time forward.
    let boundary = claimed + 1_000 + CRED_WRITE_MS;
    store
        .authenticate_device_at(&bearer, boundary)
        .unwrap()
        .expect("at the interval");
    assert_eq!(credential_logged_updates(&store), 1);
    let renewed = store
        .authenticate_device_at(&bearer, boundary + 1)
        .unwrap()
        .expect("past the interval");
    assert_eq!(credential_logged_updates(&store), 2);
    assert_eq!(renewed.last_authenticated_at_ms, Some(boundary + 1));
    assert_eq!(
        renewed.expires_at_ms,
        boundary + 1 + PAIRED_DEVICE_TOKEN_LIFETIME_MS
    );
    // A refused bearer never writes.
    assert!(
        store
            .authenticate_device_at(&"c".repeat(64), boundary + 2)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .authenticate_device_at("short", boundary + 2)
            .unwrap()
            .is_none()
    );
    assert_eq!(credential_logged_updates(&store), 2);
    assert_eq!(
        write_decisions.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "a request goes to the writer exactly when it writes, never at the 15-minute boundary"
    );
}

#[test]
fn the_stamp_and_the_expiry_each_write_on_their_own_interval() {
    let (_temporary, store) = credential_store();
    let claimed = 1_000_000;
    let access = credential_claim(&store, claimed, 45 * CRED_DAY_MS);
    let bearer = access.access_token.as_bearer().to_owned();
    let maximum = claimed + 45 * CRED_DAY_MS;
    let stamped = claimed + 1_000;
    // A device at the end of its life: the stored expiry is the maximum, so
    // no request can move it, and its stamp is from `stamped`.
    credential_sql(
        &store,
        "UPDATE paired_devices SET expires_at_ms = ?1, last_authenticated_at_ms = ?2",
        [maximum, stamped],
    );
    credential_log_updates(&store);
    let stored = || store.list_paired_devices().unwrap().remove(0);

    // A stamp 10 minutes old and an expiry that cannot move: read-only.
    let ten_minutes = stamped + 10 * 60 * 1_000;
    let device = store
        .authenticate_device_at(&bearer, ten_minutes)
        .unwrap()
        .expect("authenticated");
    assert_eq!(device.last_authenticated_at_ms, Some(stamped));
    assert_eq!(credential_logged_updates(&store), 0);

    // A stamp 20 minutes old is renewed by itself, although the expiry (at
    // the maximum) has nothing to move.
    let twenty_minutes = stamped + 20 * 60 * 1_000;
    let device = store
        .authenticate_device_at(&bearer, twenty_minutes)
        .unwrap()
        .expect("authenticated");
    assert_eq!(device.last_authenticated_at_ms, Some(twenty_minutes));
    assert_eq!(device.expires_at_ms, maximum);
    assert_eq!(credential_logged_updates(&store), 1);
    let row = stored();
    assert_eq!(row.last_authenticated_at_ms, Some(twenty_minutes));
    assert_eq!(row.expires_at_ms, maximum);

    // A fresh stamp but an expiry that fell behind the sliding window (here
    // by 20 days) is renewed, to now plus 30 days.
    credential_sql(
        &store,
        "UPDATE paired_devices SET expires_at_ms = ?1",
        [twenty_minutes + 10 * CRED_DAY_MS],
    );
    let updates_before = credential_logged_updates(&store);
    let next = twenty_minutes + 60_000;
    let device = store
        .authenticate_device_at(&bearer, next)
        .unwrap()
        .expect("authenticated");
    assert_eq!(device.expires_at_ms, next + PAIRED_DEVICE_TOKEN_LIFETIME_MS);
    assert_eq!(device.last_authenticated_at_ms, Some(next));
    assert_eq!(credential_logged_updates(&store), updates_before + 1);
    let row = stored();
    assert_eq!(row.expires_at_ms, next + PAIRED_DEVICE_TOKEN_LIFETIME_MS);
    assert_eq!(row.last_authenticated_at_ms, Some(next));
}

#[test]
fn a_revocation_between_coalesced_authentications_takes_effect_on_the_next_request() {
    let (_temporary, store) = credential_store();
    let claimed = 1_000_000;
    let access = credential_claim(&store, claimed, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let bearer = access.access_token.as_bearer().to_owned();
    let rotated = store
        .rotate_device(&bearer, claimed + 500)
        .expect("rotation leaves a grace bearer");
    let current = rotated.access_token.as_bearer().to_owned();
    for second in 1..=3 {
        for token in [&bearer, &current] {
            assert!(
                store
                    .authenticate_device_at(token, claimed + second * 1_000)
                    .unwrap()
                    .is_some()
            );
        }
    }
    credential_log_updates(&store);
    // These requests are all inside the interval, so they are read-only ...
    store
        .authenticate_device_at(&current, claimed + 4_000)
        .unwrap()
        .expect("read-only request");
    assert_eq!(credential_logged_updates(&store), 0);
    // ... and a revocation in between is seen by the very next one, for the
    // current bearer and the grace bearer.
    store
        .revoke_device_at(access.device_id, claimed + 4_500)
        .expect("revoke");
    for token in [&current, &bearer] {
        assert!(
            store
                .authenticate_device_at(token, claimed + 5_000)
                .unwrap()
                .is_none(),
            "a revoked device is refused on the next request"
        );
    }
    assert_eq!(
        credential_logged_updates(&store),
        1,
        "only the revocation wrote"
    );
}

#[test]
fn expiry_and_the_maximum_still_bind_on_every_coalesced_request() {
    let (_temporary, store) = credential_store();
    let claimed = 1_000_000;
    let access = credential_claim(&store, claimed, 45 * CRED_DAY_MS);
    let bearer = access.access_token.as_bearer().to_owned();
    let stamped = store
        .authenticate_device_at(&bearer, claimed + 1_000)
        .unwrap()
        .expect("first request writes");
    let expiry = stamped.expires_at_ms;
    assert_eq!(expiry, claimed + 1_000 + PAIRED_DEVICE_TOKEN_LIFETIME_MS);
    credential_log_updates(&store);

    // Inside the interval nothing writes, yet the stored expiry still ends the bearer.
    store
        .authenticate_device_at(&bearer, claimed + 2_000)
        .unwrap()
        .expect("read-only request");
    assert_eq!(credential_logged_updates(&store), 0);
    assert!(
        store
            .authenticate_device_at(&bearer, expiry)
            .unwrap()
            .is_none(),
        "the stored expiry is judged on every request"
    );
    assert_eq!(credential_logged_updates(&store), 0);
    // Close to the maximum the slide is clamped, and past it nothing is served.
    let maximum = claimed + 45 * CRED_DAY_MS;
    let near = store
        .authenticate_device_at(&bearer, claimed + 20 * CRED_DAY_MS)
        .unwrap()
        .expect("slides to the maximum");
    assert_eq!(near.expires_at_ms, maximum);
    assert!(
        store
            .authenticate_device_at(&bearer, maximum)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_coalesced_authentication_neither_needs_nor_waits_for_the_writer() {
    let (_temporary, store) = credential_store();
    let claimed = 1_000_000;
    let access = credential_claim(&store, claimed, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let bearer = access.access_token.as_bearer().to_owned();
    store
        .authenticate_device_at(&bearer, claimed + 1_000)
        .unwrap()
        .expect("first request writes");
    let mut writer = store.open().expect("writer");
    let transaction = writer
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("hold the writer lock");
    // A request that joined the writer queue would wait out the 5 second busy
    // timeout and then fail with SQLITE_BUSY: the unwraps below are the check.
    assert!(
        store
            .authenticate_device_at(&bearer, claimed + 2_000)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .authenticate_device_at(&"d".repeat(64), claimed + 2_000)
            .unwrap()
            .is_none()
    );
    transaction.rollback().expect("release the writer lock");
}

/// A rendezvous of a fixed number of threads that gives up after a minute: a
/// thread that never arrives (a read that failed, a regression that returns
/// early) makes the waiting ones fail the test, where `std::sync::Barrier`
/// would hang the whole suite.
struct BoundedRendezvous {
    parties: usize,
    arrived: Mutex<usize>,
    all_arrived: std::sync::Condvar,
}

impl BoundedRendezvous {
    fn new(parties: usize) -> Self {
        Self {
            parties,
            arrived: Mutex::new(0),
            all_arrived: std::sync::Condvar::new(),
        }
    }

    fn wait(&self) {
        let mut arrived = self
            .arrived
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *arrived += 1;
        if *arrived >= self.parties {
            self.all_arrived.notify_all();
        }
        let (arrived, outcome) = self
            .all_arrived
            .wait_timeout_while(arrived, Duration::from_secs(60), |arrived| {
                *arrived < self.parties
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            !outcome.timed_out(),
            "only {} of {} threads reached the rendezvous",
            *arrived,
            self.parties
        );
    }
}

/// Authenticate `bearer` on another thread and, once that request has read the
/// device and decided to write (and before it asks for the writer), run
/// `between` here, then let the request go on. Only the first authentication
/// to reach the write stage is held. Returns what the held request returned.
fn credential_authenticate_across(
    store: &HubStore,
    bearer: &str,
    now_ms: i64,
    between: impl FnOnce(),
) -> Result<Option<PairedDeviceRecord>, StoreError> {
    let (reached_tx, reached_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (reached_tx, go_rx) = (Mutex::new(reached_tx), Mutex::new(go_rx));
    let held = std::sync::atomic::AtomicBool::new(false);
    store.set_credential_write_hook(Some(CredentialWriteHook::new(move |stage| {
        if stage == CredentialWriteStage::Authenticate
            && !held.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            reached_tx
                .lock()
                .expect("reached")
                .send(())
                .expect("announce");
            go_rx
                .lock()
                .expect("go")
                .recv_timeout(Duration::from_secs(60))
                .expect("released");
        }
    })));
    let worker = {
        let (store, bearer) = (store.clone(), bearer.to_owned());
        std::thread::spawn(move || store.authenticate_device_at(&bearer, now_ms))
    };
    reached_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the request decided to write");
    between();
    go_tx.send(()).expect("release the request");
    let result = worker.join().expect("authentication thread");
    store.set_credential_write_hook(None);
    result
}

#[test]
fn concurrent_authentications_of_one_stale_row_write_once_and_all_succeed() {
    // A burst of requests reads the same stale row (the claim left no stamp);
    // each decides to write, and none has the writer yet. Whoever gets it
    // first refreshes the row; the others find it fresh and must neither
    // write again nor be refused.
    let (_temporary, store) = credential_store();
    let claimed = 1_000_000;
    let access = credential_claim(&store, claimed, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let bearer = access.access_token.as_bearer().to_owned();
    credential_log_updates(&store);
    let now = claimed + 1_000;
    let rendezvous = std::sync::Arc::new(BoundedRendezvous::new(8));
    store.set_credential_write_hook(Some(CredentialWriteHook::new({
        let rendezvous = rendezvous.clone();
        move |stage| {
            if stage == CredentialWriteStage::Authenticate {
                rendezvous.wait();
            }
        }
    })));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (store, bearer) = (store.clone(), bearer.clone());
            std::thread::spawn(move || store.authenticate_device_at(&bearer, now))
        })
        .collect();
    let devices: Vec<PairedDeviceRecord> = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("authentication thread")
                .expect("authentication")
                .expect("every valid request is authenticated")
        })
        .collect();
    store.set_credential_write_hook(None);
    assert_eq!(devices.len(), 8);
    for device in &devices {
        assert_eq!(device.device_id, access.device_id);
        assert_eq!(device.last_authenticated_at_ms, Some(now));
        assert_eq!(
            device.expires_at_ms,
            now + PAIRED_DEVICE_TOKEN_LIFETIME_MS,
            "every caller sees the refreshed row"
        );
    }
    assert_eq!(
        credential_logged_updates(&store),
        1,
        "eight requests on one stale row perform one UPDATE"
    );
    let stored = &store.list_paired_devices().unwrap()[0];
    assert_eq!(stored.last_authenticated_at_ms, Some(now));
    assert_eq!(stored.expires_at_ms, now + PAIRED_DEVICE_TOKEN_LIFETIME_MS);
}

#[test]
fn a_request_whose_row_was_refreshed_after_its_read_writes_nothing_and_is_answered() {
    let (_temporary, store) = credential_store();
    let claimed = 1_000_000;
    let access = credential_claim(&store, claimed, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let bearer = access.access_token.as_bearer().to_owned();
    credential_log_updates(&store);
    // The held request is at +1 s and read the stale row. Before it asks for
    // the writer another request, 5 minutes later, refreshes it.
    let refreshed_at = claimed + 5 * 60 * 1_000;
    let answered = credential_authenticate_across(&store, &bearer, claimed + 1_000, || {
        let refreshed = store
            .authenticate_device_at(&bearer, refreshed_at)
            .unwrap()
            .expect("the other request");
        assert_eq!(refreshed.last_authenticated_at_ms, Some(refreshed_at));
    })
    .expect("authentication")
    .expect("not refused for having found the row refreshed");
    assert_eq!(
        credential_logged_updates(&store),
        1,
        "only the other request wrote"
    );
    // It is answered from the row as it is now, and the row is left alone.
    assert_eq!(answered.device_id, access.device_id);
    assert_eq!(answered.last_authenticated_at_ms, Some(refreshed_at));
    assert_eq!(
        answered.expires_at_ms,
        refreshed_at + PAIRED_DEVICE_TOKEN_LIFETIME_MS
    );
    let stored = &store.list_paired_devices().unwrap()[0];
    assert_eq!(stored.last_authenticated_at_ms, Some(refreshed_at));
    assert_eq!(
        stored.expires_at_ms,
        refreshed_at + PAIRED_DEVICE_TOKEN_LIFETIME_MS
    );
}

#[test]
fn a_rotation_after_a_requests_read_leaves_it_authenticated_as_the_grace_bearer() {
    let (_temporary, store) = credential_store();
    let claimed = 1_000_000;
    let access = credential_claim(&store, claimed, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let old = access.access_token.as_bearer().to_owned();
    let mut rotated = None;
    let mut rows_after_rotation = Vec::new();
    // The request read `old` as the current bearer; a rotation replaces it
    // before the request asks for the writer, so its write finds no row. The
    // request's clock is 20 minutes past the rotation's stamp, so the
    // successor's row is stale from its point of view: only the bearer match
    // keeps the grace bearer from sliding and stamping the successor's row.
    let request_at = claimed + 20 * 60 * 1_000;
    let answered = credential_authenticate_across(&store, &old, request_at, || {
        rotated = Some(
            store
                .rotate_device(&old, claimed + 2_000)
                .expect("rotation"),
        );
        credential_log_updates(&store);
        rows_after_rotation = credential_rows(&store);
    })
    .expect("authentication")
    .expect("the grace bearer authenticates inside its window");
    let rotated = rotated.expect("rotated");
    assert_eq!(answered.device_id, access.device_id);
    assert_eq!(answered.expires_at_ms, rotated.expires_at_ms);
    assert_eq!(
        credential_logged_updates(&store),
        0,
        "the request wrote nothing"
    );
    assert_eq!(
        credential_rows(&store),
        rows_after_rotation,
        "and changed nothing: the successor's row was neither slid nor stamped"
    );
    // The successor is the one current bearer.
    assert!(
        store
            .authenticate_device_at(rotated.access_token.as_bearer(), claimed + 3_000)
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_revocation_after_a_requests_read_refuses_it_and_leaves_the_row_alone() {
    let (_temporary, store) = credential_store();
    let claimed = 1_000_000;
    let access = credential_claim(&store, claimed, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let bearer = access.access_token.as_bearer().to_owned();
    let mut rows_after_revocation = Vec::new();
    let answered = credential_authenticate_across(&store, &bearer, claimed + 1_000, || {
        store
            .revoke_device_at(access.device_id, claimed + 1_500)
            .expect("revoke");
        credential_log_updates(&store);
        rows_after_revocation = credential_rows(&store);
    })
    .expect("authentication");
    assert!(answered.is_none(), "a revoked device is refused");
    assert_eq!(credential_logged_updates(&store), 0);
    assert_eq!(
        credential_rows(&store),
        rows_after_revocation,
        "the refused request neither stamped nor slid the revoked device"
    );
}

#[test]
fn an_expiry_after_a_requests_read_refuses_it_and_leaves_the_row_alone() {
    let (_temporary, store) = credential_store();
    let claimed = 1_000_000;
    let access = credential_claim(&store, claimed, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let bearer = access.access_token.as_bearer().to_owned();
    // The request is made 10 seconds after the claim. While it waits for the
    // writer the stored expiry (or the maximum) falls to that very moment.
    let now = claimed + 10_000;
    for column in ["expires_at_ms", "max_expires_at_ms"] {
        let mut rows_after = Vec::new();
        let answered = credential_authenticate_across(&store, &bearer, now, || {
            credential_sql(
                &store,
                &format!("UPDATE paired_devices SET {column} = ?1"),
                [now],
            );
            credential_log_updates(&store);
            rows_after = credential_rows(&store);
        })
        .expect("authentication");
        assert!(answered.is_none(), "{column}: refused");
        assert_eq!(credential_logged_updates(&store), 0, "{column}");
        assert_eq!(credential_rows(&store), rows_after, "{column}: untouched");
        // Back to a live device for the next column.
        credential_sql(&store, "DROP TRIGGER credential_update_log_trigger", ());
        credential_sql(&store, "DROP TABLE credential_update_log", ());
        credential_sql(
            &store,
            "UPDATE paired_devices SET expires_at_ms = ?1, max_expires_at_ms = ?2",
            [claimed + 30 * CRED_DAY_MS, claimed + 180 * CRED_DAY_MS],
        );
    }
}

// ---------------------------------------------------------- v68 constraints

#[test]
fn the_v68_check_constraints_reject_violating_rows() {
    let (_temporary, store) = credential_store();
    // Claimed before the vehicle exists, so the claim granted nothing.
    let access = credential_claim(&store, 10_000, PAIRED_DEVICE_MAX_LIFETIME_MS);
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", "v68-checks"), 1)
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "eid:1"), 2)
        .expect("vehicle")
        .vehicle_id;
    let connection = store.open().expect("catalogue");
    let violated = |result: rusqlite::Result<usize>, what: &str| {
        let error = result.expect_err(what);
        match error {
            rusqlite::Error::SqliteFailure(failure, message) => {
                assert_eq!(
                    failure.extended_code,
                    rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
                    "{what}: {message:?}"
                );
            }
            other => panic!("{what}: {other}"),
        }
    };

    // granted_at_ms >= 0
    let grant = |granted_at_ms: i64| {
        connection.execute(
            "INSERT INTO device_vehicle_grants (device_id, vehicle_id, granted_at_ms)
             VALUES (?1, ?2, ?3)",
            params![
                access.device_id.to_string(),
                vehicle.to_string(),
                granted_at_ms
            ],
        )
    };
    violated(grant(-1), "a negative grant time");
    violated(grant(i64::MIN), "the most negative grant time");
    assert_eq!(grant(0).expect("zero is allowed"), 1);

    // all_vehicles IN (0, 1)
    let challenge = |all_vehicles: i64| {
        connection.execute(
            "INSERT INTO pairing_challenges
                 (pairing_id, label, secret_sha256, created_at_ms, expires_at_ms, all_vehicles)
             VALUES (?1, 'checks', zeroblob(32), 1, 2, ?2)",
            params![Uuid::new_v4().to_string(), all_vehicles],
        )
    };
    violated(challenge(2), "all_vehicles = 2");
    violated(challenge(-1), "all_vehicles = -1");
    assert_eq!(challenge(0).expect("0 is allowed"), 1);
    assert_eq!(challenge(1).expect("1 is allowed"), 1);

    // max_expires_at_ms IS NULL OR max_expires_at_ms >= created_at_ms
    let device = |created_at_ms: i64, maximum: Option<i64>| {
        connection.execute(
            "INSERT INTO paired_devices
                 (device_id, display_name, token_sha256, created_at_ms, expires_at_ms,
                  max_expires_at_ms)
             VALUES (?1, 'checks', randomblob(32), ?2, ?3, ?4)",
            params![
                Uuid::new_v4().to_string(),
                created_at_ms,
                created_at_ms + 1,
                maximum
            ],
        )
    };
    violated(device(500, Some(499)), "a maximum before the creation time");
    violated(device(500, Some(-1)), "a negative maximum");
    assert_eq!(device(500, Some(500)).expect("equal is allowed"), 1);
    assert_eq!(device(500, Some(i64::MAX)).expect("the sentinel"), 1);
    assert_eq!(device(500, None).expect("NULL is allowed"), 1);
    // The same rule holds for an UPDATE of an existing device.
    violated(
        connection.execute(
            "UPDATE paired_devices SET max_expires_at_ms = created_at_ms - 1 WHERE device_id = ?1",
            [access.device_id.to_string()],
        ),
        "an update below the creation time",
    );
}
