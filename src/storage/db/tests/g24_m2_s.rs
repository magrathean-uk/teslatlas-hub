// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::auth::credential_envelope::{
    CredentialOrigin, CredentialSlot, EnvelopeError, FixedNonces, KeyId, RefreshOwner,
    refresh_fingerprint,
};
use crate::auth::credential_keys::{KeyMaterial, Keyring, KeyringHandle};
use std::sync::{Arc, Mutex};

const T0_MS: i64 = 1_800_000_000_000;
const ENV_A: &str = "484345560101010203040506070801010101010000000100000000000040008000000000000001000102030405060708090a0b8b03111345569345953066400944965d94bc293a46884c8b0894e7";
const ENV_R: &str = "484345560101010203040506070801010201010000000100000000000040008000000000000001101112131415161718191a1bf687af998f3831c8109298587b6f51d1bb0984d2598cef5f271869f2";
const ENV_E: &str = "484345560101010203040506070801010203010000000100000000000040008000000000000001202122232425262728292a2b80107cc886bcb1ca1eb18e3eb218b866";

fn generation(n: u128) -> Uuid {
    Uuid::from_u128(0x00000000_0000_4000_8000_000000000000 | n)
}

fn key_id() -> KeyId {
    KeyId([1, 2, 3, 4, 5, 6, 7, 8])
}

fn keys() -> KeyringHandle {
    KeyringHandle::new(Keyring {
        current: KeyMaterial {
            id: key_id(),
            created_at_s: 1_800_000_000,
            key: Zeroizing::new(std::array::from_fn(|i| i as u8)),
        },
        previous: None,
    })
}

fn test_nonces() -> FixedNonces {
    FixedNonces::new(
        key_id(),
        (0..4096u64).map(|draw| {
            if draw < 2 {
                std::array::from_fn(|i| i as u8 + draw as u8 * 16)
            } else {
                let mut nonce = [0xf0; 12];
                nonce[..8].copy_from_slice(&draw.to_be_bytes());
                nonce
            }
        }),
    )
}

struct Fixture {
    _temporary: tempfile::TempDir,
    store: HubStore,
    conn: CredentialConnection,
    keys: KeyringHandle,
    nonces: FixedNonces,
    sign_in_sequence: u64,
}

impl Fixture {
    fn new() -> Self {
        let temporary = crate::private_tempdir().expect("synthetic temporary root");
        let store = HubStore::initialize(temporary.path()).expect("initialise synthetic store");
        let mut conn = store
            .take_credential_connection()
            .expect("take credential connection");
        conn.set_test_now_ms(T0_MS);
        Self {
            _temporary: temporary,
            store,
            conn,
            keys: keys(),
            nonces: test_nonces(),
            sign_in_sequence: 0,
        }
    }

    fn pair(&mut self, slot: CredentialSlot, n: u128, refresh: &str) -> SealedCredentialPair {
        self.pair_with(
            slot,
            n,
            if slot == CredentialSlot::OwnerApi {
                CredentialOrigin::DirectEntry
            } else {
                CredentialOrigin::FleetOauth
            },
            RefreshOwner::Hub,
            b"access-0001",
            refresh.as_bytes(),
        )
    }

    fn pair_with(
        &mut self,
        slot: CredentialSlot,
        n: u128,
        origin: CredentialOrigin,
        owner: RefreshOwner,
        access: &[u8],
        refresh: &[u8],
    ) -> SealedCredentialPair {
        let fleet = slot == CredentialSlot::FleetApi;
        let mut pair = SealedCredentialPair {
            account_id: 1,
            slot,
            origin: Some(origin),
            refresh_owner: owner,
            generation: generation(n),
            access_envelope: Vec::new(),
            refresh_envelope: Vec::new(),
            refresh_present: !refresh.is_empty(),
            refresh_fingerprint: None,
            expires_at: if fleet { 1_800_028_800 } else { 0 },
            next_refresh_at: if fleet { 1_800_021_600 } else { 0 },
            client_id: fleet.then(|| "client-0001".to_string()),
            region: fleet.then(|| "eu".to_string()),
            migrated: false,
            written_at_ms: T0_MS,
        };
        self.conn
            .reserve_and_seal_credential_pair(
                &mut pair,
                &self.keys,
                access,
                refresh,
                &mut self.nonces,
            )
            .expect("reserve durably before sealing synthetic pair");
        pair
    }

    fn put(
        &mut self,
        pair: &SealedCredentialPair,
        policy: ReplacePolicy,
    ) -> Result<(), StoreError> {
        if pair.origin == Some(CredentialOrigin::HubLogin) {
            self.sign_in_sequence += 1;
            let mut secret = Zeroizing::new([0xa5; 32]);
            secret[..8].copy_from_slice(&self.sign_in_sequence.to_be_bytes());
            let id = self
                .conn
                .insert_sign_in_binding(pair.slot, binding(&secret), T0_MS)?;
            self.conn.replace_credential_pair(
                pair,
                &EntryProvenance::HubGrant {
                    sign_in_id: id,
                    grant_secret: &secret,
                },
                &LegacyCandidates(Vec::new()),
                policy,
            )?;
        } else {
            self.conn.replace_credential_pair(
                pair,
                &EntryProvenance::Supplied,
                &LegacyCandidates(Vec::new()),
                policy,
            )?;
        }
        self.conn.credential_durability_barrier()
    }

    fn begin(&mut self, slot: CredentialSlot, n: u128) -> OutboundRequestReceiptId {
        let id = self
            .conn
            .begin_credential_refresh(slot, generation(n))
            .expect("begin refresh");
        self.conn
            .credential_durability_barrier()
            .expect("begin durable before request");
        id
    }

    fn complete(
        &mut self,
        id: OutboundRequestReceiptId,
        n: u128,
        pair: &SealedCredentialPair,
    ) -> RefreshCompletion {
        let result = self
            .conn
            .complete_credential_refresh(id, generation(n), pair, &LegacyCandidates(Vec::new()))
            .expect("complete refresh");
        self.conn
            .credential_durability_barrier()
            .expect("completion durable before successor use");
        result
    }

    fn count(&self, table: &str) -> i64 {
        self.conn
            .test_connection()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .expect("count synthetic table")
    }

    fn usage(&self) -> i64 {
        self.conn
            .test_connection()
            .query_row(
                "SELECT coalesce(sum(reserved_invocations),0) FROM credential_key_usage",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn fence(&self, refresh: &str, kind: &str) {
        self.conn.test_connection().execute("INSERT INTO credential_refresh_fences(fingerprint,kind,fenced_at_ms) VALUES(?1,?2,?3)", params![refresh_fingerprint(refresh.as_bytes()).as_slice(),kind,T0_MS]).expect("seed classified synthetic fence");
    }

    fn observe(&self) -> (Connection, i64) {
        let db = self
            .store
            .open()
            .expect("second connection observes data_version");
        let version = data_version(&db);
        (db, version)
    }
}

fn data_version(db: &Connection) -> i64 {
    db.query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap()
}

fn binding(secret: &[u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"teslatlas-hub/sign-in-binding/v1\0");
    hash.update(secret);
    hash.finalize().into()
}

fn assert_pair(a: &SealedCredentialPair, b: &SealedCredentialPair) {
    assert_eq!(a.account_id, b.account_id, "account");
    assert_eq!(a.slot, b.slot, "slot");
    assert_eq!(a.origin, b.origin, "origin");
    assert_eq!(a.refresh_owner, b.refresh_owner, "refresh owner");
    assert_eq!(a.generation, b.generation, "generation");
    assert_eq!(a.access_envelope, b.access_envelope, "access bytes");
    assert_eq!(a.refresh_envelope, b.refresh_envelope, "refresh bytes");
    assert_eq!(a.refresh_present, b.refresh_present, "refresh presence");
    assert_eq!(a.refresh_fingerprint, b.refresh_fingerprint, "fingerprint");
    assert_eq!(a.expires_at, b.expires_at, "expiry");
    assert_eq!(a.next_refresh_at, b.next_refresh_at, "refresh schedule");
    assert_eq!(a.client_id, b.client_id, "client");
    assert_eq!(a.region, b.region, "region");
    assert_eq!(a.migrated, b.migrated, "migration flag");
    assert_eq!(a.written_at_ms, b.written_at_ms, "written timestamp");
}

fn assert_open(f: &Fixture, pair: &SealedCredentialPair, access: &[u8], refresh: &[u8]) {
    let (a, r) = pair.open(&f.keys).expect("stored pair opens");
    assert_eq!(a.as_slice(), access);
    assert_eq!(r.as_slice(), refresh);
}

fn receipt(
    db: &Connection,
    id: OutboundRequestReceiptId,
) -> (String, Option<i64>, Option<i64>, Option<i64>, Option<i64>) {
    db.query_row("SELECT outcome,http_status,retry_after_seconds,completed_at_ms,duration_ms FROM outbound_request_receipts WHERE id=?1", [id.0], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap()
}

fn assert_refusal(result: Result<OutboundRequestReceiptId, StoreError>, reason: BeginRefusal) {
    assert!(
        matches!(result, Err(StoreError::RefreshOutcomeUnknown { reason: found }) if found == reason),
        "expected named begin refusal {reason:?}"
    );
}

// CT24 is appended with its complete schema constraint matrix below.

#[test]
fn ct025_round_trip() {
    let mut f = Fixture::new();
    let pair = f.pair_with(
        CredentialSlot::OwnerApi,
        1,
        CredentialOrigin::HubLogin,
        RefreshOwner::Hub,
        b"access-0001",
        b"refresh-0001",
    );
    assert_eq!(
        pair.access_envelope,
        hex::decode(ENV_A).unwrap(),
        "CT25 ENV-A"
    );
    assert_eq!(
        pair.refresh_envelope,
        hex::decode(ENV_R).unwrap(),
        "CT25 ENV-R"
    );
    assert_eq!(
        pair.refresh_fingerprint,
        Some(refresh_fingerprint(b"refresh-0001"))
    );
    f.put(&pair, ReplacePolicy::OnlyIfEmpty).unwrap();
    let loaded = f
        .conn
        .load_credential_pair(CredentialSlot::OwnerApi)
        .unwrap()
        .unwrap();
    assert_pair(&loaded, &pair);
    assert_eq!(loaded.generation, generation(1));
    assert_eq!((loaded.expires_at, loaded.next_refresh_at), (0, 0));
    assert_eq!(loaded.written_at_ms, T0_MS);
    assert_open(&f, &loaded, b"access-0001", b"refresh-0001");
    assert_eq!(f.usage(), 2, "CT25 exactly two invocation reservations");
    assert_eq!(
        f.count("credential_sign_ins"),
        0,
        "binding consumed atomically"
    );
    let usage_row: (Vec<u8>, i64, i64) = f
        .conn
        .test_connection()
        .query_row(
            "SELECT key_id,reserved_invocations,first_reserved_at_ms FROM credential_key_usage",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        usage_row,
        (key_id().0.to_vec(), 2, T0_MS),
        "CT25 exact reserved key and first reservation time"
    );
    let mut invalid = loaded;
    for (access, refresh) in [
        (b"".as_slice(), b"refresh-0001".as_slice()),
        (b"access-0001".as_slice(), b"".as_slice()),
    ] {
        let mut refused_nonces = FixedNonces::new(key_id(), [[0x41; 12], [0x42; 12]]);
        assert!(
            matches!(
                invalid.seal(&f.keys, access, refresh, &mut refused_nonces),
                Err(StoreError::CredentialPairInvalid)
            ),
            "S-OR-15 invalid seal input is a caller error"
        );
        assert_eq!(refused_nonces.calls(), 0, "invalid input draws no nonce");
    }
}

#[test]
fn ct026_length_first() {
    let mut f = Fixture::new();
    let pair = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&pair, ReplacePolicy::OnlyIfEmpty).unwrap();
    for (length, over_cap) in [(16_452, true), (66, false)] {
        f.conn
            .test_connection()
            .execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        f.conn
            .test_connection()
            .execute(
                "UPDATE credential_pairs SET access_envelope=?1",
                [vec![0u8; length]],
            )
            .unwrap();
        f.conn.reset_test_blob_bytes_read();
        let error = f
            .conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap_err();
        assert!(
            matches!(
                (&error, over_cap),
                (StoreError::CredentialEnvelope(EnvelopeError::OverCap), true)
                    | (
                        StoreError::CredentialEnvelope(EnvelopeError::Truncated),
                        false
                    )
            ),
            "CT26 preserves distinct length errors"
        );
        assert_eq!(
            f.conn.test_blob_bytes_read(),
            0,
            "CT26 rejects length before either blob fetch"
        );
    }
}

#[test]
fn ct027_replace_policies() {
    let mut f = Fixture::new();
    let first = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&first, ReplacePolicy::OnlyIfEmpty).unwrap();
    let second = f.pair(CredentialSlot::OwnerApi, 2, "refresh-0002");
    let (observer, before) = f.observe();
    assert!(matches!(
        f.conn.replace_credential_pair(
            &second,
            &EntryProvenance::Supplied,
            &LegacyCandidates(Vec::new()),
            ReplacePolicy::OnlyIfEmpty
        ),
        Err(StoreError::SlotOccupied)
    ));
    assert_eq!(
        data_version(&observer),
        before,
        "CT27 occupied refusal writes nothing"
    );
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &first,
    );
    f.put(&second, ReplacePolicy::ReplaceAny).unwrap();
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &second,
    );
}

#[test]
fn ct028_spent_entry() {
    let mut f = Fixture::new();
    f.fence("refresh-0001", "migrated");
    let spent = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    let (observer, before) = f.observe();
    assert!(
        matches!(
            f.put(&spent, ReplacePolicy::ReplaceAny),
            Err(StoreError::RefreshTokenSpent)
        ),
        "CT28 fingerprint spent refusal"
    );
    assert_eq!(
        data_version(&observer),
        before,
        "CT28 spent entry writes nothing"
    );
    assert_eq!(f.count("credential_pairs"), 0);
    let fresh = f.pair(CredentialSlot::OwnerApi, 2, "refresh-0002");
    f.put(&fresh, ReplacePolicy::ReplaceAny).unwrap();
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &fresh,
    );
    let candidate = generation(0xaabb);
    f.conn
        .test_connection()
        .execute(
            "INSERT INTO credential_legacy_fences VALUES(?1,'anchored',?2)",
            params![candidate.to_string().to_uppercase(), T0_MS],
        )
        .unwrap();
    let other = f.pair(CredentialSlot::OwnerApi, 3, "refresh-0003");
    let (observer, before) = f.observe();
    assert!(
        matches!(
            f.conn.replace_credential_pair(
                &other,
                &EntryProvenance::Supplied,
                &LegacyCandidates(vec![candidate]),
                ReplacePolicy::ReplaceAny
            ),
            Err(StoreError::RefreshTokenSpent)
        ),
        "R74 legacy candidate remains fenced without a fingerprint"
    );
    assert_eq!(data_version(&observer), before);
    f.conn
        .test_connection()
        .execute(
            "INSERT INTO credential_legacy_fences VALUES(?1,'anchored',?2)",
            params![generation(2).to_string().to_uppercase(), T0_MS],
        )
        .unwrap();
    let (observer, before) = f.observe();
    assert_refusal(
        f.conn
            .begin_credential_refresh(CredentialSlot::OwnerApi, generation(2)),
        BeginRefusal::TokenSpent,
    );
    assert_eq!(
        data_version(&observer),
        before,
        "R74 row's own legacy generation remains fenced at begin"
    );
}

#[test]
fn ct029_rotated_refresh() {
    let mut f = Fixture::new();
    let first = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&first, ReplacePolicy::OnlyIfEmpty).unwrap();
    let id = f.begin(CredentialSlot::OwnerApi, 1);
    assert_eq!(
        receipt(f.conn.test_connection(), id),
        ("started".into(), None, None, None, None)
    );
    let intent: (String, String, String, String, Option<i64>, i64) = f.conn.test_connection().query_row("SELECT a.input_generation,hex(f.fingerprint),f.kind,r.transport,r.vehicle_tesla_id,a.migrated FROM credential_refresh_attempts a JOIN credential_refresh_fences f ON f.fence_id=a.fence_id JOIN outbound_request_receipts r ON r.id=a.receipt_id WHERE a.receipt_id=?1", [id.0], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap();
    assert_eq!(
        intent,
        (
            generation(1).to_string(),
            hex::encode_upper(refresh_fingerprint(b"refresh-0001")),
            "attempt".into(),
            "legacy_auth".into(),
            None,
            0
        )
    );
    let metadata: (String, String, String) = f
        .conn
        .test_connection()
        .query_row(
            "SELECT operation,safety_class,precondition FROM outbound_request_receipts WHERE id=?1",
            [id.0],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        metadata,
        (
            "token_refresh".into(),
            "non_wake_endpoint".into(),
            "not_required".into()
        )
    );
    assert_refusal(
        f.conn
            .begin_credential_refresh(CredentialSlot::OwnerApi, generation(1)),
        BeginRefusal::AttemptInFlight,
    );
    let second = f.pair(CredentialSlot::OwnerApi, 2, "refresh-0002");
    assert_eq!(f.complete(id, 1, &second), RefreshCompletion::Completed);
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &second,
    );
    assert_eq!(
        receipt(f.conn.test_connection(), id),
        ("success".into(), Some(200), None, Some(T0_MS), Some(0))
    );
    let output: String = f
        .conn
        .test_connection()
        .query_row(
            "SELECT output_generation FROM credential_refresh_attempts WHERE receipt_id=?1",
            [id.0],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(output, generation(2).to_string());
    assert_eq!(f.count("credential_refresh_fences"), 1);
    assert_refusal(
        f.conn
            .begin_credential_refresh(CredentialSlot::OwnerApi, generation(1)),
        BeginRefusal::RowChanged,
    );
    let stale = f.pair(CredentialSlot::OwnerApi, 3, "refresh-0001");
    assert!(matches!(
        f.put(&stale, ReplacePolicy::ReplaceAny),
        Err(StoreError::RefreshTokenSpent)
    ));
}

#[test]
fn ct030_unrotated_refresh() {
    let mut f = Fixture::new();
    let first = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&first, ReplacePolicy::OnlyIfEmpty).unwrap();
    let id = f.begin(CredentialSlot::OwnerApi, 1);
    let second = f.pair(CredentialSlot::OwnerApi, 2, "refresh-0001");
    assert_eq!(f.complete(id, 1, &second), RefreshCompletion::Completed);
    assert_eq!(
        f.count("credential_refresh_fences"),
        0,
        "CT30 unrotated input was not consumed"
    );
    let detached: Option<i64> = f
        .conn
        .test_connection()
        .query_row(
            "SELECT fence_id FROM credential_refresh_attempts WHERE receipt_id=?1",
            [id.0],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(detached, None);
    assert_eq!(
        receipt(f.conn.test_connection(), id),
        ("success".into(), Some(200), None, Some(T0_MS), Some(0))
    );
    let output: String = f
        .conn
        .test_connection()
        .query_row(
            "SELECT output_generation FROM credential_refresh_attempts WHERE receipt_id=?1",
            [id.0],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        output,
        generation(2).to_string(),
        "CT30 attempt output generation"
    );
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &second,
    );
    let next = f.begin(CredentialSlot::OwnerApi, 2);
    let (input, fingerprint): (String, Vec<u8>) = f.conn.test_connection().query_row("SELECT a.input_generation,f.fingerprint FROM credential_refresh_attempts a JOIN credential_refresh_fences f ON a.fence_id=f.fence_id WHERE receipt_id=?1", [next.0], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(input, generation(2).to_string());
    assert_eq!(fingerprint, refresh_fingerprint(b"refresh-0001"));
    assert_eq!(f.count("credential_refresh_fences"), 1);
}

#[test]
fn ct031_spent_successor() {
    let mut f = Fixture::new();
    let first = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&first, ReplacePolicy::OnlyIfEmpty).unwrap();
    let id = f.begin(CredentialSlot::OwnerApi, 1);
    let mut changed_provenance = f.pair(CredentialSlot::OwnerApi, 2, "refresh-0002");
    changed_provenance.migrated = true;
    let (observer, before) = f.observe();
    assert!(
        matches!(
            f.conn.complete_credential_refresh(
                id,
                generation(1),
                &changed_provenance,
                &LegacyCandidates(Vec::new())
            ),
            Err(StoreError::CredentialPairInvalid)
        ),
        "R31 successor inheritance mismatch is a vault defect"
    );
    assert_eq!(
        data_version(&observer),
        before,
        "R31 invalid successor writes nothing"
    );
    assert_eq!(receipt(f.conn.test_connection(), id).0, "started");
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &first,
    );
    f.fence("refresh-0002", "migrated");
    let second = f.pair(CredentialSlot::OwnerApi, 2, "refresh-0002");
    let (observer, before) = f.observe();
    assert!(matches!(
        f.conn.complete_credential_refresh(
            id,
            generation(1),
            &second,
            &LegacyCandidates(Vec::new())
        ),
        Err(StoreError::RefreshTokenSpent)
    ));
    assert_eq!(data_version(&observer), before);
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &first,
    );
    assert_eq!(receipt(f.conn.test_connection(), id).0, "started");
    assert_eq!(f.count("credential_refresh_fences"), 2);
    let legacy = generation(0xaaaa);
    f.conn
        .test_connection()
        .execute(
            "INSERT INTO credential_legacy_fences VALUES(?1,'anchored',?2)",
            params![legacy.to_string(), T0_MS],
        )
        .unwrap();
    let third = f.pair(CredentialSlot::OwnerApi, 3, "refresh-0003");
    let (observer, before) = f.observe();
    assert!(matches!(
        f.conn.complete_credential_refresh(
            id,
            generation(1),
            &third,
            &LegacyCandidates(vec![legacy])
        ),
        Err(StoreError::RefreshTokenSpent)
    ));
    assert_eq!(
        data_version(&observer),
        before,
        "R74 successor legacy form refused without writes"
    );
}

#[test]
fn ct032_idempotent_completion() {
    let mut f = Fixture::new();
    let first = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&first, ReplacePolicy::OnlyIfEmpty).unwrap();
    let id = f.begin(CredentialSlot::OwnerApi, 1);
    let second = f.pair(CredentialSlot::OwnerApi, 2, "refresh-0002");
    assert_eq!(f.complete(id, 1, &second), RefreshCompletion::Completed);
    let (observer, before) = f.observe();
    let usage = f.usage();
    assert_eq!(
        f.complete(id, 1, &second),
        RefreshCompletion::AlreadyComplete
    );
    assert_eq!(
        data_version(&observer),
        before,
        "CT32 replay commits no row changes"
    );
    assert_eq!(f.usage(), usage);
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &second,
    );
    assert_eq!(f.count("credential_refresh_attempts"), 1);
    assert_eq!(f.count("credential_refresh_fences"), 1);
    assert_eq!(receipt(f.conn.test_connection(), id).0, "success");
}

#[test]
fn ct033_cancel_unsent() {
    let mut f = Fixture::new();
    let pair = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&pair, ReplacePolicy::OnlyIfEmpty).unwrap();
    let id = f.begin(CredentialSlot::OwnerApi, 1);
    f.conn
        .cancel_unsent_credential_refresh(id, generation(1))
        .unwrap();
    f.conn.credential_durability_barrier().unwrap();
    assert_eq!(f.count("credential_refresh_attempts"), 0);
    assert_eq!(f.count("credential_refresh_fences"), 0);
    assert_eq!(
        receipt(f.conn.test_connection(), id),
        ("cancelled".into(), None, None, Some(T0_MS), Some(0))
    );
    let migrated = f.begin(CredentialSlot::OwnerApi, 1);
    f.conn
        .test_connection()
        .execute(
            "UPDATE credential_refresh_attempts SET migrated=1,fence_id=NULL WHERE receipt_id=?1",
            [migrated.0],
        )
        .unwrap();
    f.conn
        .test_connection()
        .execute("UPDATE credential_refresh_fences SET kind='migrated'", [])
        .unwrap();
    f.conn
        .cancel_unsent_credential_refresh(migrated, generation(1))
        .unwrap();
    assert_eq!(
        f.count("credential_refresh_attempts"),
        1,
        "migrated attempt preserved"
    );
    assert_eq!(
        f.count("credential_refresh_fences"),
        1,
        "migrated fence never released"
    );
    assert_eq!(receipt(f.conn.test_connection(), migrated).0, "cancelled");
}

#[test]
fn ct034_close_release_classes() {
    for (outcome, status, retry, release, label) in [
        (
            OutboundRequestOutcome::Success,
            Some(200),
            None,
            true,
            "success",
        ),
        (OutboundRequestOutcome::Timeout, None, None, true, "timeout"),
        (
            OutboundRequestOutcome::HttpError,
            Some(503),
            Some(120),
            true,
            "http_error",
        ),
        (
            OutboundRequestOutcome::AuthenticationRejected,
            Some(400),
            None,
            false,
            "authentication_rejected",
        ),
        (
            OutboundRequestOutcome::TransportError,
            None,
            None,
            true,
            "transport_error",
        ),
    ] {
        let mut f = Fixture::new();
        let pair = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
        f.put(&pair, ReplacePolicy::OnlyIfEmpty).unwrap();
        let id = f.begin(CredentialSlot::OwnerApi, 1);
        f.conn
            .close_credential_refresh_attempt(id, generation(1), outcome, status, retry, release)
            .unwrap();
        f.conn.credential_durability_barrier().unwrap();
        assert_eq!(
            receipt(f.conn.test_connection(), id),
            (
                label.into(),
                status.map(i64::from),
                retry.map(|v| v as i64),
                Some(T0_MS),
                Some(0)
            )
        );
        assert_eq!(f.count("credential_refresh_attempts"), i64::from(!release));
        assert_eq!(f.count("credential_refresh_fences"), i64::from(!release));
        if release {
            f.begin(CredentialSlot::OwnerApi, 1);
        } else {
            assert_refusal(
                f.conn
                    .begin_credential_refresh(CredentialSlot::OwnerApi, generation(1)),
                BeginRefusal::TokenSpent,
            );
            let same = f.pair(CredentialSlot::OwnerApi, 2, "refresh-0001");
            assert!(matches!(
                f.put(&same, ReplacePolicy::ReplaceAny),
                Err(StoreError::RefreshTokenSpent)
            ));
        }
    }
    for outcome in [
        OutboundRequestOutcome::Cancelled,
        OutboundRequestOutcome::ProtocolError,
        OutboundRequestOutcome::ResponseTooLarge,
    ] {
        let mut refused = Fixture::new();
        let pair = refused.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
        refused.put(&pair, ReplacePolicy::OnlyIfEmpty).unwrap();
        let id = refused.begin(CredentialSlot::OwnerApi, 1);
        let (observer, before) = refused.observe();
        assert!(
            refused
                .conn
                .close_credential_refresh_attempt(id, generation(1), outcome, None, None, true)
                .is_err(),
            "R33 exactly C5 R51 refuses non-sent-request close outcomes"
        );
        assert_eq!(data_version(&observer), before);
        assert!(
            refused
                .conn
                .has_unresolved_credential_refresh(CredentialSlot::OwnerApi)
                .unwrap()
        );
    }
    let mut f = Fixture::new();
    let pair = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&pair, ReplacePolicy::OnlyIfEmpty).unwrap();
    let id = f.begin(CredentialSlot::OwnerApi, 1);
    f.conn
        .test_connection()
        .execute(
            "UPDATE credential_refresh_attempts SET migrated=1,fence_id=NULL WHERE receipt_id=?1",
            [id.0],
        )
        .unwrap();
    f.conn
        .test_connection()
        .execute("UPDATE credential_refresh_fences SET kind='migrated'", [])
        .unwrap();
    f.conn
        .close_credential_refresh_attempt(
            id,
            generation(1),
            OutboundRequestOutcome::TransportError,
            None,
            None,
            true,
        )
        .unwrap();
    assert_eq!(f.count("credential_refresh_attempts"), 1);
    assert_eq!(
        f.count("credential_refresh_fences"),
        1,
        "R33 release never removes migrated fence"
    );
}

#[test]
fn ct035_unresolved_external_write() {
    let mut f = Fixture::new();
    let first = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&first, ReplacePolicy::OnlyIfEmpty).unwrap();
    let id = f.begin(CredentialSlot::OwnerApi, 1);
    assert!(
        f.conn
            .has_unresolved_credential_refresh(CredentialSlot::OwnerApi)
            .unwrap()
    );
    assert!(
        !f.conn
            .has_unresolved_credential_refresh(CredentialSlot::FleetApi)
            .unwrap()
    );
    let third = f.pair(CredentialSlot::OwnerApi, 3, "refresh-0002");
    f.put(&third, ReplacePolicy::ReplaceAny).unwrap();
    assert_eq!(receipt(f.conn.test_connection(), id).0, "cancelled");
    assert_eq!(f.count("credential_refresh_attempts"), 1);
    assert_eq!(f.count("credential_refresh_fences"), 1);
    let output: Option<String> = f
        .conn
        .test_connection()
        .query_row(
            "SELECT output_generation FROM credential_refresh_attempts WHERE receipt_id=?1",
            [id.0],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(output, None);
    assert!(
        !f.conn
            .has_unresolved_credential_refresh(CredentialSlot::OwnerApi)
            .unwrap()
    );
    let spent = f.pair(CredentialSlot::OwnerApi, 4, "refresh-0001");
    assert!(matches!(
        f.put(&spent, ReplacePolicy::ReplaceAny),
        Err(StoreError::RefreshTokenSpent)
    ));
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &third,
    );
}

#[test]
fn ct036_cross_slot_spent() {
    let mut f = Fixture::new();
    let first = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&first, ReplacePolicy::OnlyIfEmpty).unwrap();
    let id = f.begin(CredentialSlot::OwnerApi, 1);
    let fleet = f.pair(CredentialSlot::FleetApi, 2, "refresh-0001");
    assert!(
        matches!(
            f.put(&fleet, ReplacePolicy::ReplaceAny),
            Err(StoreError::RefreshTokenSpent)
        ),
        "CT36 shared spent set applies while in flight"
    );
    let successor = f.pair(CredentialSlot::OwnerApi, 2, "refresh-0002");
    f.complete(id, 1, &successor);
    assert!(
        matches!(
            f.put(&fleet, ReplacePolicy::ReplaceAny),
            Err(StoreError::RefreshTokenSpent)
        ),
        "CT36 shared spent set survives completion"
    );
    assert!(
        f.conn
            .load_credential_pair(CredentialSlot::FleetApi)
            .unwrap()
            .is_none()
    );
    assert_eq!(f.count("credential_refresh_fences"), 1);
}

#[test]
fn ct037_parallel_slots() {
    let mut f = Fixture::new();
    for (slot, token) in [
        (CredentialSlot::OwnerApi, "owner-chain-0000"),
        (CredentialSlot::FleetApi, "fleet-chain-0000"),
    ] {
        let pair = f.pair(slot, 1, token);
        f.put(&pair, ReplacePolicy::OnlyIfEmpty).unwrap();
    }
    let writer = f.store.open().expect("ordinary second SQLite writer");
    writer
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold real SQLite writer lock");
    let shared = Arc::new(Mutex::new(f));
    let (pending_tx, pending_rx) = std::sync::mpsc::channel();
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    fn chain(
        shared: Arc<Mutex<Fixture>>,
        slot: CredentialSlot,
        name: &str,
        signals: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Sender<()>)>,
    ) {
        for i in 0..200u128 {
            let id = {
                let mut f = shared.lock().unwrap();
                if i == 0
                    && let Some((pending, _)) = &signals
                {
                    let timeout: i64 = f
                        .conn
                        .test_connection()
                        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
                        .unwrap();
                    assert!(
                        timeout > 0,
                        "CT37 production credential connection has a busy timeout"
                    );
                    pending
                        .send(())
                        .expect("acknowledge pending begin before SQLite call");
                }
                let id = f.begin(slot, i + 1);
                if i == 0
                    && let Some((_, completed)) = &signals
                {
                    completed
                        .send(())
                        .expect("acknowledge successful begin after writer release");
                }
                id
            };
            let mut f = shared.lock().unwrap();
            let token = format!("{name}-chain-{:04}", i + 1);
            let successor = f.pair(slot, i + 2, &token);
            assert_eq!(
                f.complete(id, i + 1, &successor),
                RefreshCompletion::Completed
            );
        }
    }
    let owner = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            chain(
                shared,
                CredentialSlot::OwnerApi,
                "owner",
                Some((pending_tx, completed_tx)),
            )
        })
    };
    pending_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("owner thread acknowledges first pending begin");
    let fleet = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || chain(shared, CredentialSlot::FleetApi, "fleet", None))
    };
    assert!(
        matches!(
            completed_rx.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "CT37 pending begin waits while ordinary writer holds SQLite lock"
    );
    writer
        .execute_batch("COMMIT")
        .expect("release ordinary SQLite writer lock");
    completed_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("CT37 configured busy timeout permits begin after writer releases lock");
    owner.join().expect("owner refresh chain finishes");
    fleet.join().expect("Fleet refresh chain finishes");
    let mut f = shared.lock().unwrap();
    assert_eq!(
        f.count("credential_refresh_fences"),
        400,
        "CT37 400 consumed inputs"
    );
    assert_eq!(f.count("credential_refresh_attempts"), 400);
    for (slot, token) in [
        (CredentialSlot::OwnerApi, b"owner-chain-0200".as_slice()),
        (CredentialSlot::FleetApi, b"fleet-chain-0200".as_slice()),
    ] {
        let pair = f.conn.load_credential_pair(slot).unwrap().unwrap();
        assert_eq!(pair.generation, generation(201));
        assert_open(&f, &pair, b"access-0001", token);
        assert!(!f.conn.has_unresolved_credential_refresh(slot).unwrap());
    }
}

#[test]
fn ct038_fence_capacity() {
    let mut f = Fixture::new();
    let pair = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&pair, ReplacePolicy::OnlyIfEmpty).unwrap();
    f.conn.set_test_fence_capacity(3);
    f.fence("capacity-1", "migrated");
    f.fence("capacity-2", "migrated");
    f.conn
        .test_connection()
        .execute(
            "INSERT INTO credential_legacy_fences VALUES(?1,'anchored',?2)",
            params![generation(33).to_string(), T0_MS],
        )
        .unwrap();
    let (observer, before) = f.observe();
    assert_refusal(
        f.conn
            .begin_credential_refresh(CredentialSlot::OwnerApi, generation(1)),
        BeginRefusal::FenceFull,
    );
    assert_eq!(data_version(&observer), before);
    assert_eq!(
        f.count("credential_refresh_fences") + f.count("credential_legacy_fences"),
        3,
        "capacity includes both fence tables"
    );
    assert_eq!(f.count("credential_refresh_attempts"), 0);
    assert_eq!(
        MAX_CREDENTIAL_REFRESH_FENCES,
        MAX_OUTBOUND_REQUEST_RECEIPTS as usize
    );
}

#[test]
fn ct039_reserved_receipts() {
    let mut f = Fixture::new();
    let pair = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&pair, ReplacePolicy::OnlyIfEmpty).unwrap();
    let id = f.begin(CredentialSlot::OwnerApi, 1);
    let (observer, before) = f.observe();
    assert!(matches!(
        f.store.complete_outbound_request(
            id,
            &OutboundRequestCompletion {
                outcome: OutboundRequestOutcome::Success,
                http_status: Some(200),
                retry_after_seconds: None
            }
        ),
        Err(StoreError::ReservedCredentialRefreshReceipt)
    ));
    assert_eq!(data_version(&observer), before);
    assert_eq!(receipt(f.conn.test_connection(), id).0, "started");
    for transport in [
        OutboundRequestTransport::FleetApi,
        OutboundRequestTransport::LegacyAuth,
    ] {
        let start = OutboundRequestStart {
            correlation_id: generation(22),
            vehicle_tesla_id: None,
            transport,
            operation: OutboundRequestOperation::TokenRefresh,
            safety_class: OutboundRequestSafetyClass::NonWakeEndpoint,
            precondition: OutboundRequestPrecondition::NotRequired,
        };
        assert!(
            matches!(
                start.validate(),
                Err(StoreError::ReservedCredentialRefreshReceipt)
            ),
            "CT39 both refresh transports reserved"
        );
    }
}

#[test]
fn ct040_secure_delete() {
    let mut f = Fixture::new();
    let mut pair = f.pair_with(
        CredentialSlot::OwnerApi,
        1,
        CredentialOrigin::DirectEntry,
        RefreshOwner::Hub,
        &[b'a'; 128],
        b"refresh-0001",
    );
    let mut run = [0u8; 64];
    getrandom::fill(&mut run).expect("synthetic distinctive ciphertext run");
    pair.access_envelope[51..115].copy_from_slice(&run);
    f.put(&pair, ReplacePolicy::OnlyIfEmpty).unwrap();
    let database = f.store.database_path().to_path_buf();
    let wal = PathBuf::from(format!("{}-wal", database.display()));
    let contains = |bytes: &[u8]| bytes.windows(run.len()).any(|window| window == run);
    assert!(
        contains(&fs::read(&database).unwrap()) || contains(&fs::read(&wal).unwrap_or_default()),
        "CT40 positive control finds run before replacement"
    );
    let replacement = f.pair(CredentialSlot::OwnerApi, 2, "refresh-0002");
    f.put(&replacement, ReplacePolicy::ReplaceAny).unwrap();
    assert!(
        f.conn.scrub_credential_wal().unwrap(),
        "CT40 truncation acknowledged and stat confirms empty WAL"
    );
    assert!(
        !contains(&fs::read(&database).unwrap()),
        "CT40 secure delete removes superseded run from database"
    );
    assert_eq!(
        fs::metadata(&wal).map(|m| m.len()).unwrap_or(0),
        0,
        "CT40 WAL empty after scrub"
    );
}

#[test]
fn ct041_reservations() {
    let mut f = Fixture::new();
    let first = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    f.put(&first, ReplacePolicy::OnlyIfEmpty).unwrap();
    assert_eq!(f.usage(), 2, "replace reserved two");
    let id = f.begin(CredentialSlot::OwnerApi, 1);
    let second = f.pair(CredentialSlot::OwnerApi, 2, "refresh-0002");
    assert_eq!(f.usage(), 4, "successor reserved two");
    f.conn.set_test_rollback_completion(true);
    assert!(
        f.conn
            .complete_credential_refresh(id, generation(1), &second, &LegacyCandidates(Vec::new()))
            .is_err(),
        "CT41 forced transaction rollback"
    );
    assert_eq!(f.usage(), 4, "rollback never returns reservation");
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &first,
    );
    assert_eq!(receipt(f.conn.test_connection(), id).0, "started");
    f.conn.set_test_rollback_completion(false);
    let retry = f.pair(CredentialSlot::OwnerApi, 2, "refresh-0002");
    assert_eq!(f.usage(), 6, "completion retry reserves again");
    f.complete(id, 1, &retry);
    let owner = f.pair_with(
        CredentialSlot::OwnerApi,
        2,
        CredentialOrigin::DirectEntry,
        RefreshOwner::External,
        b"access-0001",
        b"refresh-0002",
    );
    f.conn
        .set_credential_refresh_owner(&owner, generation(2))
        .unwrap();
    f.conn.credential_durability_barrier().unwrap();
    assert_eq!(f.usage(), 8, "owner reseal reserves two");
    let spent = f.pair(CredentialSlot::OwnerApi, 3, "refresh-0001");
    let row_before_refusal = f
        .conn
        .load_credential_pair(CredentialSlot::OwnerApi)
        .unwrap()
        .unwrap();
    let (observer, before) = f.observe();
    assert!(matches!(
        f.put(&spent, ReplacePolicy::ReplaceAny),
        Err(StoreError::RefreshTokenSpent)
    ));
    assert_eq!(
        f.usage(),
        10,
        "spent refusal after seal keeps both reservations"
    );
    assert_eq!(
        f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap()
            .generation,
        generation(2)
    );
    assert_eq!(
        data_version(&observer),
        before,
        "CT41 spent refusal after sealing writes no pair or receipt"
    );
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &row_before_refusal,
    );
    f.conn
        .clear_credential_pair(CredentialSlot::OwnerApi)
        .unwrap();
    f.conn.credential_durability_barrier().unwrap();
    assert_eq!(f.usage(), 10, "clear reserves zero");
    let empty = f.pair_with(
        CredentialSlot::OwnerApi,
        4,
        CredentialOrigin::ProviderEntry,
        RefreshOwner::Hub,
        b"provider-access",
        b"",
    );
    f.put(&empty, ReplacePolicy::OnlyIfEmpty).unwrap();
    assert_eq!(f.usage(), 12, "absent refresh still seals and reserves two");
    let events = f.conn.test_events();
    let commit = events
        .iter()
        .position(|event| *event == "reservation_commit")
        .expect("reservation commit event");
    let barrier = events
        .iter()
        .position(|event| *event == "reservation_barrier")
        .expect("reservation barrier event");
    assert!(commit < barrier, "CT41 reservation commit precedes barrier");
    let mut nonces = FixedNonces::new(key_id(), [[0x81; 12], [0x82; 12]]);
    assert_eq!(nonces.calls(), 0);
    let mut resealed = empty;
    f.conn
        .reserve_and_seal_credential_pair(
            &mut resealed,
            &f.keys,
            b"provider-access",
            b"",
            &mut nonces,
        )
        .expect("R22 real orchestration reserves durably before sealing");
    assert_eq!(
        nonces.calls(),
        2,
        "CT41 real helper draws exactly two nonces after durable reservation"
    );
    let events = f.conn.test_events();
    let commit = events
        .iter()
        .rposition(|e| *e == "reservation_commit")
        .expect("latest reservation commit");
    let barrier = events
        .iter()
        .rposition(|e| *e == "reservation_barrier")
        .expect("latest reservation barrier");
    let seal_start = events
        .iter()
        .rposition(|e| *e == "seal_start")
        .expect("real helper seal start");
    assert!(
        commit < barrier && barrier < seal_start,
        "CT41 reservation commit and barrier precede first nonce draw"
    );
    assert_eq!(f.usage(), 14);
    f.conn.set_test_barrier_failure(true);
    let mut refused_nonces = FixedNonces::new(key_id(), [[0x83; 12], [0x84; 12]]);
    let events_before = f.conn.test_events().len();
    assert!(
        matches!(
            f.conn.reserve_and_seal_credential_pair(
                &mut resealed,
                &f.keys,
                b"provider-access",
                b"",
                &mut refused_nonces
            ),
            Err(StoreError::CatalogueDurability(_))
        ),
        "CT41 failed reservation barrier refuses sealing before nonce draw"
    );
    assert_eq!(
        refused_nonces.calls(),
        0,
        "CT41 real helper never draws a nonce after reservation barrier failure"
    );
    assert!(
        !f.conn.test_events()[events_before..].contains(&"seal_start"),
        "CT41 failed reservation never enters sealing"
    );
    assert_eq!(
        f.usage(),
        16,
        "failed barrier cannot refund committed reservation"
    );
    f.conn.set_test_barrier_failure(false);
    let first_reserved: i64 = f
        .conn
        .test_connection()
        .query_row(
            "SELECT first_reserved_at_ms FROM credential_key_usage WHERE key_id=?1",
            [key_id().0.as_slice()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        first_reserved, T0_MS,
        "reservations preserve first reservation time"
    );
    let limit = i64::try_from(crate::auth::credential_keys::FORCED_ROTATION_INVOCATIONS).unwrap();
    f.conn
        .test_connection()
        .execute(
            "UPDATE credential_key_usage SET reserved_invocations=?1 WHERE key_id=?2",
            params![limit - 2, key_id().0.as_slice()],
        )
        .unwrap();
    f.conn
        .reserve_credential_invocations(key_id(), 2)
        .expect("reservation exactly at invocation limit is allowed");
    assert_eq!(f.usage(), limit, "R22 exact upper boundary");
    let (observer, before) = f.observe();
    assert!(
        matches!(
            f.conn.reserve_credential_invocations(key_id(), 1),
            Err(StoreError::WriteRefused("credential_invocation_limit"))
        ),
        "R22 store refuses above limit until V1 rotates"
    );
    assert_eq!(f.usage(), limit);
    assert_eq!(
        data_version(&observer),
        before,
        "above-limit reservation writes nothing"
    );
}

#[test]
fn ct042_inventory() {
    let mut f = Fixture::new();
    assert_eq!(
        f.store.owner_wire_credential_identity().unwrap(),
        "",
        "M2P-48 absent Owner identity is an empty string"
    );
    assert_eq!(
        f.store.collector_credential_identity(false).unwrap(),
        "",
        "M2P-48 absent Owner collector identity is an empty string"
    );
    assert_eq!(
        f.store.collector_credential_identity(true).unwrap(),
        "",
        "M2P-48 absent Fleet identity is an empty string"
    );
    for (slot, token) in [
        (CredentialSlot::OwnerApi, "refresh-0001"),
        (CredentialSlot::FleetApi, "refresh-0002"),
    ] {
        let pair = f.pair(slot, 1, token);
        f.put(&pair, ReplacePolicy::OnlyIfEmpty).unwrap();
    }
    let runtime = f.store.runtime_inventory().unwrap();
    assert_eq!(runtime.owner_api_credential_rows, 1);
    assert_eq!(runtime.fleet_api_credential_rows, 1);
    let catalogue = f.store.catalogue_inventory().unwrap();
    assert_eq!(catalogue.owner_api_credential_rows, 1);
    assert_eq!(catalogue.fleet_api_credential_rows, 1);
    assert_eq!(
        f.store.collector_credential_identity(true).unwrap(),
        "client-0001|eu",
        "M2P-44 Fleet configuration identity keeps its exact retained text"
    );
    let mut chain = Fixture::new();
    let root = chain.pair(CredentialSlot::OwnerApi, 0xabcd, "owner-chain-input");
    chain.put(&root, ReplacePolicy::OnlyIfEmpty).unwrap();
    let stored_root_text = root.generation.to_string().to_uppercase();
    chain
        .conn
        .test_connection()
        .execute(
            "UPDATE credential_pairs SET generation=?1 WHERE slot='owner_api'",
            [&stored_root_text],
        )
        .unwrap();
    assert_eq!(
        chain.store.owner_wire_credential_identity().unwrap(),
        root.generation.to_string(),
        "M2P-48 present Owner identity uses canonical lowercase hyphenated UUID text"
    );
    // The fixture's text-form witness is independent of R30's canonical UUID
    // input. Restore that generation before starting a real new-format refresh.
    chain
        .conn
        .test_connection()
        .execute(
            "UPDATE credential_pairs SET generation=?1 WHERE slot='owner_api'",
            [root.generation.to_string()],
        )
        .unwrap();
    let owner_receipt = chain.begin(CredentialSlot::OwnerApi, 0xabcd);
    let owner_output = chain.pair(CredentialSlot::OwnerApi, 0xabce, "owner-chain-output");
    chain.complete(owner_receipt, 0xabcd, &owner_output);
    chain.conn.test_connection().execute(
        "UPDATE credential_refresh_attempts SET input_generation=?1,migrated=1,fence_id=NULL WHERE receipt_id=?2",
        params![stored_root_text, owner_receipt.0],
    ).unwrap();
    assert_eq!(
        chain.store.collector_credential_identity(false).unwrap(),
        root.generation.to_string(),
        "M2P-48 present Owner ancestry keeps canonical lowercase hyphenated UUID text"
    );
    let fleet = chain.pair(CredentialSlot::FleetApi, 0xf001, "fleet-chain-input");
    chain.put(&fleet, ReplacePolicy::OnlyIfEmpty).unwrap();
    let fleet_receipt = chain.begin(CredentialSlot::FleetApi, 0xf001);
    let fleet_output = chain.pair(CredentialSlot::FleetApi, 0xf002, "fleet-chain-output");
    chain.complete(fleet_receipt, 0xf001, &fleet_output);
    chain.conn.test_connection().execute(
        "UPDATE outbound_request_receipts SET started_at_ms=0,completed_at_ms=1,duration_ms=1 WHERE id IN (?1,?2)",
        params![owner_receipt.0,fleet_receipt.0],
    ).unwrap();
    let tx =
        Transaction::new_unchecked(chain.conn.test_connection(), TransactionBehavior::Immediate)
            .unwrap();
    prune_expired_outbound_request_receipts(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        receipt(chain.conn.test_connection(), owner_receipt).0,
        "success",
        "M2P-45 reachable Owner receipt is retained"
    );
    let fleet_rows: i64 = chain
        .conn
        .test_connection()
        .query_row(
            "SELECT count(*) FROM outbound_request_receipts WHERE id=?1",
            [fleet_receipt.0],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        fleet_rows, 0,
        "M2P-45 expired Fleet-chain receipt is pruned"
    );
    assert_eq!(
        chain.store.owner_wire_credential_identity().unwrap(),
        root.generation.to_string(),
        "M2P-48 present Owner identity uses canonical lowercase hyphenated UUID text"
    );
    let mut failed = Fixture::new();
    let stored = failed.pair(CredentialSlot::OwnerApi, 0xaabb, "failed-output-control");
    failed.put(&stored, ReplacePolicy::OnlyIfEmpty).unwrap();
    let id = failed.begin(CredentialSlot::OwnerApi, 0xaabb);
    failed
        .conn
        .close_credential_refresh_attempt(
            id,
            stored.generation,
            OutboundRequestOutcome::TransportError,
            None,
            None,
            false,
        )
        .unwrap();
    failed.conn.test_connection().execute(
        "UPDATE credential_refresh_attempts SET input_generation=?1,output_generation=?2 WHERE receipt_id=?3",
        params![generation(0xaaba).to_string(),stored.generation.to_string(),id.0],
    ).unwrap();
    assert_eq!(
        failed.store.owner_wire_credential_identity().unwrap(),
        stored.generation.to_string(),
        "M2P-44 unsuccessful matching output is ignored by configuration identity"
    );
}

#[test]
fn ct043_owner_change() {
    let mut f = Fixture::new();
    let first = f.pair_with(
        CredentialSlot::OwnerApi,
        1,
        CredentialOrigin::DirectEntry,
        RefreshOwner::External,
        b"access-0001",
        b"refresh-0001",
    );
    f.put(&first, ReplacePolicy::OnlyIfEmpty).unwrap();
    let owner = f.pair_with(
        CredentialSlot::OwnerApi,
        1,
        CredentialOrigin::DirectEntry,
        RefreshOwner::Hub,
        b"access-0001",
        b"refresh-0001",
    );
    f.conn.set_test_now_ms(T0_MS + 5_000);
    let (observer, before) = f.observe();
    assert!(matches!(
        f.conn.set_credential_refresh_owner(&owner, generation(2)),
        Err(StoreError::RefreshOutcomeUnknown {
            reason: BeginRefusal::RowChanged
        })
    ));
    assert_eq!(data_version(&observer), before);
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &first,
    );
    f.conn
        .set_credential_refresh_owner(&owner, generation(1))
        .unwrap();
    f.conn.credential_durability_barrier().unwrap();
    let loaded = f
        .conn
        .load_credential_pair(CredentialSlot::OwnerApi)
        .unwrap()
        .unwrap();
    assert_pair(&loaded, &owner);
    assert_eq!(loaded.refresh_owner, RefreshOwner::Hub);
    assert_eq!(loaded.generation, generation(1));
    assert_eq!(loaded.access_envelope[18], 0x01);
    assert_eq!(loaded.refresh_envelope[18], 0x01);
    assert_eq!(
        (loaded.expires_at, loaded.next_refresh_at),
        (first.expires_at, first.next_refresh_at)
    );
    assert_eq!(loaded.refresh_fingerprint, first.refresh_fingerprint);
    assert_eq!(loaded.origin, first.origin);
    assert_eq!(loaded.client_id, first.client_id);
    assert_eq!(loaded.region, first.region);
    assert_eq!(loaded.written_at_ms, first.written_at_ms);
    assert_open(&f, &loaded, b"access-0001", b"refresh-0001");
}

#[test]
fn ct044_clear() {
    let mut f = Fixture::new();
    let owner = f.pair(CredentialSlot::OwnerApi, 1, "refresh-0001");
    let fleet = f.pair(CredentialSlot::FleetApi, 1, "refresh-0002");
    f.put(&owner, ReplacePolicy::OnlyIfEmpty).unwrap();
    f.put(&fleet, ReplacePolicy::OnlyIfEmpty).unwrap();
    let owner_receipt = f.begin(CredentialSlot::OwnerApi, 1);
    let fleet_receipt = f.begin(CredentialSlot::FleetApi, 1);
    let before = (
        f.count("credential_refresh_attempts"),
        f.count("credential_refresh_fences"),
        f.count("credential_legacy_fences"),
        f.usage(),
    );
    let retained_rows = |db: &Connection| -> Vec<String> {
        [
            "SELECT json_group_array(json_array(receipt_id,attempt_id,account_id,slot,input_generation,fence_id,output_generation,migrated)) FROM (SELECT * FROM credential_refresh_attempts ORDER BY receipt_id)",
            "SELECT json_group_array(json_array(fence_id,hex(fingerprint),kind,fenced_at_ms)) FROM (SELECT * FROM credential_refresh_fences ORDER BY fence_id)",
            "SELECT json_group_array(json_array(legacy_generation,identity,fenced_at_ms)) FROM (SELECT * FROM credential_legacy_fences ORDER BY legacy_generation)",
            "SELECT json_group_array(json_array(hex(key_id),reserved_invocations,first_reserved_at_ms)) FROM (SELECT * FROM credential_key_usage ORDER BY key_id)",
        ].into_iter().map(|sql| db.query_row(sql, [], |row| row.get(0)).unwrap()).collect()
    };
    let rows_before = retained_rows(f.conn.test_connection());
    for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        f.conn.clear_credential_pair(slot).unwrap();
        f.conn.credential_durability_barrier().unwrap();
        assert!(f.conn.load_credential_pair(slot).unwrap().is_none());
        assert!(!f.conn.has_unresolved_credential_refresh(slot).unwrap());
    }
    assert_eq!(f.count("credential_pairs"), 0);
    assert_eq!(
        retained_rows(f.conn.test_connection()),
        rows_before,
        "CT44 all attempt, fence and usage values unchanged"
    );
    assert_eq!(
        (
            f.count("credential_refresh_attempts"),
            f.count("credential_refresh_fences"),
            f.count("credential_legacy_fences"),
            f.usage()
        ),
        before,
        "CT44 clear preserves attempts, both fences and usage"
    );
    assert_eq!(
        receipt(f.conn.test_connection(), owner_receipt).0,
        "cancelled"
    );
    assert_eq!(
        receipt(f.conn.test_connection(), fleet_receipt).0,
        "cancelled"
    );
}

#[test]
fn ct086f_old_g_access_independence() {
    // M2P-49 installs the approved package bytes at the section-10 fixture path.
    let vectors: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/credential-pre-upgrade-vectors.json"
    ))
    .unwrap();
    let old_g = vectors["old_g"].as_array().unwrap();
    let vector = |access: &str, refresh: &str| {
        old_g
            .iter()
            .find(|row| {
                row["constructor"] == "from_secret_parts"
                    && row["access"] == access
                    && row["refresh"] == refresh
            })
            .unwrap()
    };
    let expected = Uuid::parse_str(vector("A1", "R1")["generation"].as_str().unwrap()).unwrap();
    assert_eq!(
        vector("A1", "R1")["generation"],
        vector("A2", "R1")["generation"],
        "H8/R74: installed-vector entries have identical generations"
    );
    for refresh in ["R1\n", "R1\r\n"] {
        let rejected = vector("A1", refresh);
        assert_eq!(
            rejected["outcome"], "rejected",
            "M2P-19 fixture records constructor rejection"
        );
        assert_eq!(rejected["constructor_refusal"], "InvalidTokenBytes");
        assert!(
            rejected.get("generation").is_none(),
            "rejected fixture records no fabricated generation"
        );
    }
    let a1 =
        LegacyCandidates::for_secret_parts("A1", "R1").expect("kept constructor accepts A1/R1");
    let a2 =
        LegacyCandidates::for_secret_parts("A2", "R1").expect("kept constructor accepts A2/R1");
    assert!(
        a1.0.contains(&expected),
        "H8/R74: real opaque OLD-G equals A1/R1 vector"
    );
    assert!(
        a2.0.contains(&expected),
        "H8/R74: real opaque OLD-G equals A2/R1 vector"
    );
    assert_eq!(
        a1.0, a2.0,
        "H8 and R74: OLD-G must be independent of the access token"
    );
    // The canonical fixture records raw from_secret_parts LF/CRLF refusals;
    // R74 skips each refused form and still checks its accepted canonical form.
    for refresh in ["R1\n", "R1\r\n"] {
        let candidates = LegacyCandidates::for_secret_parts("A1", refresh)
            .expect("R74 raw constructor refusal must not discard canonical candidates");
        assert!(
            candidates.0.contains(&expected),
            "R74 line-ended raw entry retains canonical legacy-generation candidate"
        );
    }
    assert!(
        LegacyCandidates::for_secret_parts("provider-access", "")
            .unwrap()
            .0
            .is_empty(),
        "R74 provider without refresh has no legacy candidates"
    );
    let fake_old_g = |_: &[u8], refresh: &[u8]| Some(Uuid::new_v5(&generation(3), refresh));
    let expected_forms: Vec<Uuid> = ["refresh-0002", "refresh-0002\n", "refresh-0002\r\n"]
        .iter()
        .map(|refresh| Uuid::new_v5(&generation(3), refresh.as_bytes()))
        .collect();
    assert_eq!(
        LegacyCandidates::for_successor_with_old_g("access-0001", "refresh-0002", fake_old_g)
            .unwrap()
            .0,
        expected_forms,
        "S-OR-09 FakeOldG boundary exposes all three canonical successor forms"
    );
    assert_eq!(
        LegacyCandidates::for_secret_parts_with_old_g("access-0002", "refresh-0002", fake_old_g)
            .unwrap()
            .0,
        expected_forms,
        "S-OR-09 FakeOldG ignores access bytes as the fixture prescribes"
    );
    let mut received_forms = vec![Uuid::new_v5(&generation(3), b" refresh-0002\n")];
    received_forms.extend(expected_forms);
    assert_eq!(
        LegacyCandidates::for_file_bytes_with_old_g(
            b"access-0001\n",
            b" refresh-0002\n",
            fake_old_g
        )
        .unwrap()
        .0,
        received_forms,
        "S-OR-09 file seam keeps raw received form before canonical candidates"
    );
}

#[test]
fn ct089_connection_settings() {
    let mut f = Fixture::new();
    let pragma = |db: &Connection, name: &str| -> i64 {
        db.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(pragma(f.conn.test_connection(), "synchronous"), 2);
    assert_eq!(pragma(f.conn.test_connection(), "secure_delete"), 1);
    #[cfg(target_os = "macos")]
    {
        assert_eq!(pragma(f.conn.test_connection(), "fullfsync"), 1);
        assert_eq!(pragma(f.conn.test_connection(), "checkpoint_fullfsync"), 1);
        assert_eq!(pragma(&f.store.open().unwrap(), "checkpoint_fullfsync"), 1);
        assert_eq!(
            pragma(
                &f.store.open_read_only_connection().unwrap(),
                "checkpoint_fullfsync"
            ),
            1
        );
    }
    assert!(
        matches!(
            f.store.take_credential_connection(),
            Err(StoreError::CredentialConnectionTaken)
        ),
        "R73 connection taken once"
    );
    let cloned = f.store.clone();
    assert!(
        matches!(
            cloned.take_credential_connection(),
            Err(StoreError::CredentialConnectionTaken)
        ),
        "R73 clone shares once-taken state"
    );
    let durability = |db: &Connection| -> Option<String> {
        db.query_row(
            "SELECT durability FROM credential_format_state WHERE singleton_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        durability(f.conn.test_connection()),
        None,
        "R73 no probe result before admitted probe"
    );
    f.conn.set_test_barrier_failure(true);
    assert!(
        matches!(
            f.conn.probe_credential_durability(),
            Err(StoreError::CatalogueDurability(_))
        ),
        "R73 failed probe remains unavailable"
    );
    assert_eq!(
        durability(f.conn.test_connection()).as_deref(),
        Some("unavailable"),
        "R73 failed probe never records ok"
    );
    f.conn.set_test_barrier_failure(false);
    f.conn.set_test_barrier_failure_at(Some(2));
    assert!(
        matches!(
            f.conn.probe_credential_durability(),
            Err(StoreError::CatalogueDurability(_))
        ),
        "R73 barrier after ok record also fails closed"
    );
    assert_eq!(
        f.conn.test_barrier_calls(),
        2,
        "R73 unavailable diagnostic has no further barrier"
    );
    assert_eq!(
        durability(f.conn.test_connection()).as_deref(),
        Some("unavailable"),
        "R73 failed ok-record barrier never leaves ok"
    );
    let replay = f.store.open_read_only_connection().unwrap();
    assert_eq!(
        durability(&replay).as_deref(),
        Some("unavailable"),
        "R73 failure value survives a separate read-only open"
    );
    drop(replay);
    f.conn.set_test_barrier_failure_at(None);

    f.conn
        .probe_credential_durability()
        .expect("later full probe recovers durability");
    assert_eq!(
        durability(f.conn.test_connection()).as_deref(),
        Some("ok"),
        "R73 ok visible only after successful probe and record barriers"
    );
    let replay = f.store.open_read_only_connection().unwrap();
    let replay_version = data_version(&replay);
    assert_eq!(
        durability(&replay).as_deref(),
        Some("ok"),
        "R73 another connection replays last recorded durability"
    );
    assert_eq!(
        data_version(&replay),
        replay_version,
        "diagnostic replay changes nothing"
    );
    drop(replay);

    f.conn
        .test_connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let held_reader = f.store.open().unwrap();
    held_reader.execute_batch("BEGIN").unwrap();
    let _: i64 = held_reader
        .query_row("SELECT count(*) FROM credential_format_state", [], |row| {
            row.get(0)
        })
        .unwrap();
    f.conn
        .test_connection()
        .execute(
            "UPDATE credential_format_state SET durability_probed_at_ms=?1 WHERE singleton_id=1",
            [T0_MS - 1],
        )
        .unwrap();
    f.conn.set_test_now_ms(T0_MS + 1_000);
    f.conn
        .probe_credential_durability()
        .expect("partial passive checkpoint falls back to synced diagnostic commit");
    let stamp: Option<i64> = f
        .conn
        .test_connection()
        .query_row(
            "SELECT durability_probed_at_ms FROM credential_format_state WHERE singleton_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stamp,
        Some(T0_MS + 1_000),
        "R73 partial checkpoint stamps fallback commit before barrier"
    );
    assert_eq!(durability(f.conn.test_connection()).as_deref(), Some("ok"));
    held_reader.execute_batch("ROLLBACK").unwrap();
    f.conn
        .test_connection()
        .execute_batch(
            "CREATE TEMP TRIGGER deny_unavailable
         BEFORE UPDATE OF durability ON credential_format_state
         WHEN NEW.durability='unavailable'
         BEGIN SELECT RAISE(ABORT,'synthetic diagnostic denial'); END;",
        )
        .unwrap();
    f.conn.set_test_barrier_failure(true);
    assert!(
        matches!(
            f.conn.probe_credential_durability(),
            Err(StoreError::CatalogueDurability(_))
        ),
        "S-OR-19 barrier failure remains primary when diagnostic write also fails"
    );
    assert_eq!(
        durability(f.conn.test_connection()).as_deref(),
        Some("ok"),
        "failed secondary diagnostic leaves last written state"
    );
    f.conn
        .test_connection()
        .execute_batch("DROP TRIGGER deny_unavailable")
        .unwrap();
    f.conn.set_test_barrier_failure(false);
    let unsafe_directory = crate::private_tempdir().unwrap();
    let unsafe_store = HubStore::initialize(unsafe_directory.path()).unwrap();
    fs::set_permissions(unsafe_directory.path(), fs::Permissions::from_mode(0o750)).unwrap();
    assert!(
        matches!(
            unsafe_store.take_credential_connection(),
            Err(StoreError::UnsafeDataDir(_))
        ),
        "S-OR-05 credential take refuses unsafe mode without repair"
    );
    assert_eq!(
        fs::metadata(unsafe_directory.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750,
        "S-OR-05 refused take does not chmod the directory"
    );
    fs::set_permissions(unsafe_directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    unsafe_store
        .take_credential_connection()
        .expect("failed take releases once-only claim");
    let read_only = HubStore::open_read_only(f._temporary.path()).unwrap();
    assert!(
        matches!(
            read_only.take_credential_connection(),
            Err(StoreError::CredentialConnectionTaken)
        ),
        "R73 requested read-only store cannot take the credential connection"
    );
    let immutable_directory = crate::private_tempdir().unwrap();
    HubStore::initialize(immutable_directory.path()).unwrap();
    let immutable = HubStore::open_immutable_read_only(immutable_directory.path()).unwrap();
    assert!(
        matches!(
            immutable.take_credential_connection(),
            Err(StoreError::CredentialConnectionTaken)
        ),
        "R73 immutable store cannot take the credential connection"
    );
}

#[test]
fn ct094_header_only_presence() {
    // The byte seam counts the instrumented fetches, not arbitrary SQLite reads;
    // source inspection still verifies that no alternative blob query is added.
    let mut f = Fixture::new();
    let pair = f.pair_with(
        CredentialSlot::OwnerApi,
        1,
        CredentialOrigin::HubLogin,
        RefreshOwner::Hub,
        b"access-0001",
        b"refresh-0001",
    );
    assert_eq!(pair.access_envelope, hex::decode(ENV_A).unwrap());
    assert_eq!(pair.refresh_envelope, hex::decode(ENV_R).unwrap());
    f.put(&pair, ReplacePolicy::OnlyIfEmpty).unwrap();
    f.store.reset_test_credential_blob_bytes_read();
    let presence = f
        .store
        .credential_pair_presence(CredentialSlot::OwnerApi)
        .unwrap()
        .unwrap();
    assert!(presence.header_check.is_ok());
    assert_eq!(
        f.store.test_credential_blob_bytes_read(),
        78,
        "CT94 exactly39 bytes per header"
    );
    assert_eq!(presence.slot, CredentialSlot::OwnerApi);
    assert_eq!(presence.origin, Some(CredentialOrigin::HubLogin));
    assert_eq!(presence.refresh_owner, RefreshOwner::Hub);
    assert!(presence.refresh_present);
    assert_eq!(
        (
            presence.expires_at,
            presence.next_refresh_at,
            presence.written_at_ms
        ),
        (0, 0, T0_MS)
    );
    let provider = f.pair_with(
        CredentialSlot::OwnerApi,
        1,
        CredentialOrigin::ProviderEntry,
        RefreshOwner::Hub,
        b"provider-access",
        b"",
    );
    f.put(&provider, ReplacePolicy::ReplaceAny).unwrap();
    // N3 is the third vector nonce; independently install the accepted exact ENV-E.
    f.conn
        .test_connection()
        .execute(
            "UPDATE credential_pairs SET refresh_envelope=?1",
            [hex::decode(ENV_E).unwrap()],
        )
        .unwrap();
    f.store.reset_test_credential_blob_bytes_read();
    let presence = f
        .store
        .credential_pair_presence(CredentialSlot::OwnerApi)
        .unwrap()
        .unwrap();
    assert!(presence.header_check.is_ok());
    assert!(!presence.refresh_present);
    assert_eq!(presence.origin, Some(CredentialOrigin::ProviderEntry));
    assert_eq!(f.store.test_credential_blob_bytes_read(), 78);
    f.conn
        .test_connection()
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    f.conn
        .test_connection()
        .execute(
            "UPDATE credential_pairs SET access_envelope=?1",
            [vec![0u8; 66]],
        )
        .unwrap();
    f.store.reset_test_credential_blob_bytes_read();
    assert!(matches!(
        f.store
            .credential_pair_presence(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap()
            .header_check,
        Err(EnvelopeError::Truncated)
    ));
    assert_eq!(
        f.store.test_credential_blob_bytes_read(),
        0,
        "bad length refuses before header reads"
    );
    let mut bad_magic = provider.access_envelope;
    bad_magic[0] = 0x49;
    f.conn
        .test_connection()
        .execute(
            "UPDATE credential_pairs SET access_envelope=?1",
            [bad_magic],
        )
        .unwrap();
    f.store.reset_test_credential_blob_bytes_read();
    assert!(matches!(
        f.store
            .credential_pair_presence(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap()
            .header_check,
        Err(EnvelopeError::BadMagic)
    ));
    assert!(
        f.store.test_credential_blob_bytes_read() <= 78,
        "presence only reads headers even on parse refusal"
    );
    // A structurally readable view makes only the header fetch return a TEXT
    // column instead of BLOB, so its SQLite decode error must stay a store error.
    f.conn
        .test_connection()
        .execute_batch(
            "ALTER TABLE credential_pairs RENAME TO presence_rows;
         CREATE VIEW credential_pairs AS
         SELECT account_id,slot,origin,refresh_owner,generation,
                replace(hex(zeroblob(50)),'0','A') AS access_envelope,
                refresh_envelope,refresh_present,refresh_fingerprint,expires_at,
                next_refresh_at,client_id,region,migrated,written_at_ms
           FROM presence_rows;",
        )
        .unwrap();
    assert!(
        matches!(
            f.store.credential_pair_presence(CredentialSlot::OwnerApi),
            Err(StoreError::CredentialPairStore(_))
        ),
        "S-OR-15 header-fetch SQLite failure is not envelope corruption"
    );
}

#[test]
fn ct098_store_provenance_negatives() {
    let mut f = Fixture::new();
    let secret = Zeroizing::new([0x55; 32]);
    let wrong_secret = Zeroizing::new([0x56; 32]);
    let grant = f.pair_with(
        CredentialSlot::OwnerApi,
        1,
        CredentialOrigin::HubLogin,
        RefreshOwner::Hub,
        b"access-0003",
        b"refresh-0003",
    );
    let unknown = generation(0x1001);
    assert!(
        !f.store.credential_refresh_requires_sign_in().unwrap(),
        "CT98 fresh store requires no sign-in"
    );
    let wrong_slot_secret = Zeroizing::new([0x57; 32]);
    let expired_secret = Zeroizing::new([0x58; 32]);
    let mismatch = f
        .conn
        .insert_sign_in_binding(CredentialSlot::OwnerApi, binding(&secret), T0_MS)
        .unwrap();
    let wrong_slot = f
        .conn
        .insert_sign_in_binding(CredentialSlot::FleetApi, binding(&[0x57; 32]), T0_MS)
        .unwrap();
    let expired = f
        .conn
        .insert_sign_in_binding(CredentialSlot::OwnerApi, binding(&[0x58; 32]), T0_MS)
        .unwrap();
    assert_eq!(
        mismatch.get_version_num(),
        4,
        "R78 store generates UUID v4 ids"
    );
    assert_eq!(wrong_slot.get_version_num(), 4);
    assert_eq!(expired.get_version_num(), 4);
    assert_ne!(mismatch, wrong_slot, "R78 every sign-in id is fresh");
    assert_ne!(mismatch, expired);
    let stored_binding: (String, Vec<u8>, i64, i64) = f.conn.test_connection()
        .query_row(
            "SELECT slot,binding,started_at_ms,expires_at_ms FROM credential_sign_ins WHERE sign_in_id=?1",
            [mismatch.to_string()],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).unwrap();
    assert_eq!(
        stored_binding,
        (
            "owner_api".into(),
            binding(&secret).to_vec(),
            T0_MS,
            T0_MS + 600_000
        ),
        "R78 store fixes the exact 600-second window"
    );
    for (id, presented, expired_case) in [
        (unknown, &secret, false),
        (mismatch, &wrong_secret, false),
        (wrong_slot, &wrong_slot_secret, false),
        (expired, &expired_secret, true),
    ] {
        f.conn
            .set_test_now_ms(if expired_case { T0_MS + 600_000 } else { T0_MS });
        let (observer, before) = f.observe();
        let result = f.conn.replace_credential_pair(
            &grant,
            &EntryProvenance::HubGrant {
                sign_in_id: id,
                grant_secret: presented,
            },
            &LegacyCandidates(Vec::new()),
            ReplacePolicy::ReplaceAny,
        );
        if expired_case {
            assert!(
                matches!(result, Err(StoreError::SignInExpired)),
                "CT98 expired binding"
            );
        } else {
            assert!(
                matches!(result, Err(StoreError::SignInBindingInvalid)),
                "CT98 unknown, mismatch or wrong-slot binding"
            );
        }
        assert_eq!(
            data_version(&observer),
            before,
            "CT98 provenance negative writes nothing"
        );
        assert_eq!(f.count("credential_pairs"), 0);
    }
    f.conn.set_test_now_ms(T0_MS + 600_000);
    for (id, presented) in [(mismatch, &wrong_secret), (wrong_slot, &wrong_slot_secret)] {
        let (observer, before) = f.observe();
        assert!(
            matches!(
                f.conn.replace_credential_pair(
                    &grant,
                    &EntryProvenance::HubGrant {
                        sign_in_id: id,
                        grant_secret: presented
                    },
                    &LegacyCandidates(Vec::new()),
                    ReplacePolicy::ReplaceAny,
                ),
                Err(StoreError::SignInBindingInvalid)
            ),
            "R28 identity mismatch precedes expiry"
        );
        assert_eq!(data_version(&observer), before);
    }
    f.conn.set_test_now_ms(T0_MS);
    let consumed = f
        .conn
        .insert_sign_in_binding(CredentialSlot::OwnerApi, binding(&[0x59; 32]), T0_MS)
        .unwrap();
    let consumed_secret = Zeroizing::new([0x59; 32]);
    f.conn
        .replace_credential_pair(
            &grant,
            &EntryProvenance::HubGrant {
                sign_in_id: consumed,
                grant_secret: &consumed_secret,
            },
            &LegacyCandidates(Vec::new()),
            ReplacePolicy::ReplaceAny,
        )
        .unwrap();
    f.conn.credential_durability_barrier().unwrap();
    let (observer, before) = f.observe();
    assert!(
        matches!(
            f.conn.replace_credential_pair(
                &grant,
                &EntryProvenance::HubGrant {
                    sign_in_id: consumed,
                    grant_secret: &consumed_secret
                },
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny
            ),
            Err(StoreError::SignInBindingInvalid)
        ),
        "CT98 consumed binding cannot replay"
    );
    assert_eq!(data_version(&observer), before);
    let (observer, before) = f.observe();
    assert!(
        matches!(
            f.conn.replace_credential_pair(
                &grant,
                &EntryProvenance::Supplied,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny
            ),
            Err(StoreError::InvalidOrigin)
        ),
        "supplied provenance cannot claim hub_login"
    );
    assert_eq!(data_version(&observer), before);
    f.conn
        .test_connection()
        .execute(
            "INSERT INTO credential_legacy_fences VALUES(?1,'anchored',?2)",
            params![generation(0x2000).to_string(), T0_MS],
        )
        .unwrap();
    assert!(
        !f.store.credential_refresh_requires_sign_in().unwrap(),
        "CT98 anchored-only store requires no sign-in"
    );
    let supplied = f.pair(CredentialSlot::OwnerApi, 2, "anchored-control-refresh");
    f.put(&supplied, ReplacePolicy::ReplaceAny)
        .expect("CT98 anchored-only supplied refresh succeeds");
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &supplied,
    );
    f.conn
        .test_connection()
        .execute(
            "INSERT INTO credential_legacy_fences VALUES(?1,'opaque',?2)",
            params![generation(0x2001).to_string(), T0_MS],
        )
        .unwrap();
    assert!(f.store.credential_refresh_requires_sign_in().unwrap());
    let fresh_grant = f.pair_with(
        CredentialSlot::OwnerApi,
        3,
        CredentialOrigin::HubLogin,
        RefreshOwner::Hub,
        b"hub-grant-access",
        b"hub-grant-refresh",
    );
    f.put(&fresh_grant, ReplacePolicy::ReplaceAny)
        .expect("CT98 Hub grant succeeds beside opaque fence");
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &fresh_grant,
    );
    let provider = f.pair_with(
        CredentialSlot::OwnerApi,
        4,
        CredentialOrigin::ProviderEntry,
        RefreshOwner::Hub,
        b"provider-access-only",
        b"",
    );
    f.put(&provider, ReplacePolicy::ReplaceAny)
        .expect("CT98 provider without refresh succeeds beside opaque fence");
    assert_pair(
        &f.conn
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap(),
        &provider,
    );
    for (slot, origin, refresh) in [
        (
            CredentialSlot::OwnerApi,
            CredentialOrigin::DirectEntry,
            "refresh-0003",
        ),
        (
            CredentialSlot::OwnerApi,
            CredentialOrigin::SourceToken,
            "refresh-0003",
        ),
        (
            CredentialSlot::OwnerApi,
            CredentialOrigin::ProviderEntry,
            "refresh-0003",
        ),
        (
            CredentialSlot::FleetApi,
            CredentialOrigin::FleetOauth,
            "refresh-0004",
        ),
    ] {
        let pair = f.pair_with(
            slot,
            2,
            origin,
            RefreshOwner::Hub,
            b"access-0003",
            refresh.as_bytes(),
        );
        let (observer, before) = f.observe();
        assert!(
            matches!(
                f.conn.replace_credential_pair(
                    &pair,
                    &EntryProvenance::Supplied,
                    &LegacyCandidates(Vec::new()),
                    ReplacePolicy::ReplaceAny
                ),
                Err(StoreError::FreshSignInRequired)
            ),
            "R77 store continuity recheck is authoritative"
        );
        assert_eq!(data_version(&observer), before);
    }
    let live_secret = Zeroizing::new([0x60; 32]);
    let pruning_time = T0_MS + 600_001;
    let live = f
        .conn
        .insert_sign_in_binding(
            CredentialSlot::OwnerApi,
            binding(&live_secret),
            pruning_time,
        )
        .unwrap();
    assert_eq!(
        f.count("credential_sign_ins"),
        1,
        "R78 binding insert prunes expired rows"
    );
    let surviving: String = f
        .conn
        .test_connection()
        .query_row("SELECT sign_in_id FROM credential_sign_ins", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        surviving,
        live.to_string(),
        "R78 only the fresh binding survives expiry pruning"
    );
    f.fence("refresh-0003", "migrated");
    let spent = f.pair(CredentialSlot::OwnerApi, 4, "refresh-0003");
    assert!(
        matches!(
            f.put(&spent, ReplacePolicy::ReplaceAny),
            Err(StoreError::RefreshTokenSpent)
        ),
        "R77 spent refusal precedes opaque continuity refusal"
    );
}

// CT24 has 37 independent constraint refusals and positive controls.
fn ct24_pair_insert(overrides: &[(&str, &str)]) -> String {
    let mut fields = vec![
        ("account_id", "1"),
        ("slot", "'owner_api'"),
        ("origin", "'direct_entry'"),
        ("refresh_owner", "'hub'"),
        ("generation", "'00000000-0000-4000-8000-000000000001'"),
        ("access_envelope", "zeroblob(78)"),
        ("refresh_envelope", "zeroblob(79)"),
        ("refresh_present", "1"),
        ("refresh_fingerprint", "zeroblob(32)"),
        ("expires_at", "0"),
        ("next_refresh_at", "0"),
        ("client_id", "NULL"),
        ("region", "NULL"),
        ("migrated", "0"),
        ("written_at_ms", "0"),
    ];
    for (name, expression) in overrides {
        fields
            .iter_mut()
            .find(|field| field.0 == *name)
            .expect("CT24 known column")
            .1 = expression;
    }
    let names = fields
        .iter()
        .map(|field| field.0)
        .collect::<Vec<_>>()
        .join(",");
    let values = fields
        .iter()
        .map(|field| field.1)
        .collect::<Vec<_>>()
        .join(",");
    format!("INSERT INTO credential_pairs ({names}) VALUES ({values});")
}

fn ct24_constraint(db: &Connection, sql: &str, expected_extended_code: i32, label: &str) {
    db.execute_batch("SAVEPOINT ct24_case;").unwrap();
    let result = db.execute_batch(sql);
    db.execute_batch("ROLLBACK TO ct24_case; RELEASE ct24_case;")
        .unwrap();
    match result.expect_err(label) {
        rusqlite::Error::SqliteFailure(error, _) => {
            assert_eq!(
                error.code,
                rusqlite::ErrorCode::ConstraintViolation,
                "{label}"
            );
            assert_eq!(error.extended_code, expected_extended_code, "{label}");
        }
        other => panic!("{label}: expected SQLite constraint, got {other:?}"),
    }
}

fn ct24_valid(db: &Connection, sql: &str, label: &str) {
    db.execute_batch("SAVEPOINT ct24_control;").unwrap();
    let result = db.execute_batch(sql);
    db.execute_batch("ROLLBACK TO ct24_control; RELEASE ct24_control;")
        .unwrap();
    result.expect(label);
}

#[test]
fn ct024_schema_constraints() {
    let f = Fixture::new();
    let db = f.conn.test_connection();
    let check = rusqlite::ffi::SQLITE_CONSTRAINT_CHECK;
    let not_null = rusqlite::ffi::SQLITE_CONSTRAINT_NOTNULL;
    let unique = rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE;
    let primary_key = rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY;

    ct24_valid(db, &ct24_pair_insert(&[]), "CT24 valid owner pair");
    ct24_valid(
        db,
        &ct24_pair_insert(&[
            ("slot", "'fleet_api'"),
            ("origin", "'fleet_oauth'"),
            ("client_id", "'client-0001'"),
            ("region", "'eu'"),
            ("expires_at", "100"),
            ("next_refresh_at", "50"),
        ]),
        "CT24 valid Fleet pair",
    );
    ct24_valid(
        db,
        &ct24_pair_insert(&[
            ("origin", "'provider_entry'"),
            ("refresh_present", "0"),
            ("refresh_fingerprint", "NULL"),
            ("refresh_envelope", "zeroblob(67)"),
        ]),
        "CT24 valid absent refresh",
    );
    ct24_valid(
        db,
        &ct24_pair_insert(&[("origin", "NULL"), ("migrated", "1")]),
        "CT24 migrated unknown origin",
    );

    let pair_cases: &[(&str, &[(&str, &str)])] = &[
        ("account 2", &[("account_id", "2")]),
        ("unknown slot", &[("slot", "'x'")]),
        ("unknown origin", &[("origin", "'x'")]),
        (
            "Fleet hub_login",
            &[
                ("slot", "'fleet_api'"),
                ("origin", "'hub_login'"),
                ("client_id", "'client-0001'"),
                ("region", "'eu'"),
                ("expires_at", "100"),
                ("next_refresh_at", "50"),
            ],
        ),
        ("owner fleet_oauth", &[("origin", "'fleet_oauth'")]),
        ("unmigrated NULL origin", &[("origin", "NULL")]),
        (
            "Fleet NULL origin",
            &[
                ("slot", "'fleet_api'"),
                ("origin", "NULL"),
                ("migrated", "1"),
                ("client_id", "'client-0001'"),
                ("region", "'eu'"),
                ("expires_at", "100"),
                ("next_refresh_at", "50"),
            ],
        ),
        (
            "Fleet missing client",
            &[
                ("slot", "'fleet_api'"),
                ("origin", "'fleet_oauth'"),
                ("region", "'eu'"),
                ("expires_at", "100"),
                ("next_refresh_at", "50"),
            ],
        ),
        ("owner with region", &[("region", "'eu'")]),
        (
            "absent direct refresh",
            &[
                ("refresh_present", "0"),
                ("refresh_fingerprint", "NULL"),
                ("refresh_envelope", "zeroblob(67)"),
            ],
        ),
        (
            "absent refresh fingerprint",
            &[
                ("origin", "'provider_entry'"),
                ("refresh_present", "0"),
                ("refresh_envelope", "zeroblob(67)"),
            ],
        ),
        (
            "present refresh length 67",
            &[("refresh_envelope", "zeroblob(67)")],
        ),
        ("access length 67", &[("access_envelope", "zeroblob(67)")]),
        (
            "access length 16452",
            &[("access_envelope", "zeroblob(16452)")],
        ),
        (
            "Fleet schedule zero",
            &[
                ("slot", "'fleet_api'"),
                ("origin", "'fleet_oauth'"),
                ("client_id", "'client-0001'"),
                ("region", "'eu'"),
            ],
        ),
        (
            "backwards schedule",
            &[("expires_at", "100"), ("next_refresh_at", "200")],
        ),
    ];
    assert_eq!(pair_cases.len(), 16, "CT24 pair-case count");
    for (label, overrides) in pair_cases {
        ct24_constraint(db, &ct24_pair_insert(overrides), check, label);
    }

    ct24_constraint(
        db,
        "INSERT INTO credential_refresh_fences(fingerprint,kind,fenced_at_ms) VALUES(NULL,'attempt',0);",
        not_null,
        "NULL fingerprint",
    );
    ct24_constraint(
        db,
        "INSERT INTO credential_refresh_fences(fingerprint,kind,fenced_at_ms) VALUES(zeroblob(31),'attempt',0);",
        check,
        "31-byte fingerprint",
    );
    ct24_constraint(
        db,
        "INSERT INTO credential_refresh_fences(fingerprint,kind,fenced_at_ms) VALUES(zeroblob(32),'live',0);",
        check,
        "unknown fence kind",
    );
    ct24_constraint(
        db,
        "INSERT INTO credential_refresh_fences(fingerprint,kind,fenced_at_ms) VALUES(zeroblob(32),'attempt',0); INSERT INTO credential_refresh_fences(fingerprint,kind,fenced_at_ms) VALUES(zeroblob(32),'migrated',1);",
        unique,
        "duplicate fingerprint",
    );
    ct24_valid(
        db,
        "INSERT INTO credential_refresh_fences(fingerprint,kind,fenced_at_ms) VALUES(zeroblob(32),'attempt',0);",
        "CT24 valid fingerprint fence",
    );

    ct24_constraint(
        db,
        "INSERT INTO credential_legacy_fences VALUES('00000000-0000-4000-8000-00000000001','anchored',0);",
        check,
        "35-character legacy generation",
    );
    ct24_constraint(
        db,
        "INSERT INTO credential_legacy_fences VALUES('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','anchored',0); INSERT INTO credential_legacy_fences VALUES('AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA','opaque',1);",
        primary_key,
        "NOCASE legacy generation",
    );
    for identity in ["'issued'", "'x'", "NULL"] {
        ct24_constraint(
            db,
            &format!(
                "INSERT INTO credential_legacy_fences VALUES('00000000-0000-4000-8000-000000000001',{identity},0);"
            ),
            if identity == "NULL" { not_null } else { check },
            "legacy identity",
        );
    }
    for identity in ["anchored", "opaque"] {
        ct24_valid(
            db,
            &format!(
                "INSERT INTO credential_legacy_fences VALUES('00000000-0000-4000-8000-000000000001','{identity}',0);"
            ),
            "CT24 valid legacy identity",
        );
    }

    db.execute_batch(
        "INSERT INTO outbound_request_receipts(id,correlation_id,started_at_ms,transport,operation,safety_class,precondition,outcome)
         VALUES(9001,'00000000-0000-4000-8000-000000000001',0,'legacy_auth','token_refresh','non_wake_endpoint','not_required','started'),
               (9002,'00000000-0000-4000-8000-000000000002',0,'legacy_auth','token_refresh','non_wake_endpoint','not_required','started');
         INSERT INTO credential_refresh_fences(fence_id,fingerprint,kind,fenced_at_ms)
         VALUES(9001,zeroblob(32),'attempt',0),
               (9002,X'1111111111111111111111111111111111111111111111111111111111111111','attempt',0);",
    ).unwrap();

    let a1 = "INSERT INTO credential_refresh_attempts(receipt_id,attempt_id,account_id,slot,input_generation,fence_id,output_generation,migrated) VALUES(9001,'10000000-0000-4000-8000-000000000001',1,'owner_api','00000000-0000-4000-8000-000000000001',9001,NULL,0);";
    ct24_valid(db, a1, "CT24 valid attempt with linked fence");
    ct24_valid(
        db,
        "INSERT INTO credential_refresh_attempts(receipt_id,attempt_id,account_id,slot,input_generation,fence_id,output_generation,migrated) VALUES(9001,'10000000-0000-4000-8000-000000000001',1,'owner_api','00000000-0000-4000-8000-000000000001',NULL,NULL,1);",
        "CT24 valid migrated attempt without link",
    );
    ct24_constraint(
        db,
        "INSERT INTO credential_refresh_attempts(receipt_id,attempt_id,account_id,slot,input_generation,fence_id,output_generation,migrated) VALUES(9001,'10000000-0000-4000-8000-000000000001',1,'owner_api','00000000-0000-4000-8000-000000000001',9001,NULL,1);",
        check,
        "migrated attempt with fence",
    );
    ct24_constraint(
        db,
        &format!(
            "{a1} INSERT INTO credential_refresh_attempts(receipt_id,attempt_id,account_id,slot,input_generation,fence_id,output_generation,migrated) VALUES(9002,'10000000-0000-4000-8000-000000000002',1,'owner_api','00000000-0000-4000-8000-000000000003',9001,NULL,0);"
        ),
        unique,
        "two attempts one fence",
    );
    ct24_constraint(
        db,
        "INSERT INTO credential_refresh_attempts(receipt_id,attempt_id,account_id,slot,input_generation,fence_id,output_generation,migrated) VALUES(9001,'10000000-0000-4000-8000-000000000001',1,'owner_api','00000000-0000-4000-8000-000000000001',9001,'00000000-0000-4000-8000-000000000001',0);",
        check,
        "output equals input",
    );
    ct24_constraint(
        db,
        "INSERT INTO credential_refresh_attempts(receipt_id,attempt_id,account_id,slot,input_generation,fence_id,output_generation,migrated) VALUES(9001,'10000000-0000-4000-8000-000000000001',1,'owner_api','00000000-0000-4000-8000-000000000001',9001,'00000000-0000-4000-8000-000000000002',0); INSERT INTO credential_refresh_attempts(receipt_id,attempt_id,account_id,slot,input_generation,fence_id,output_generation,migrated) VALUES(9002,'10000000-0000-4000-8000-000000000002',1,'owner_api','00000000-0000-4000-8000-000000000003',9002,'00000000-0000-4000-8000-000000000002',0);",
        unique,
        "same output in slot",
    );

    // The fresh-state singleton is valid; each mutation is rolled back before the next.
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM credential_format_state WHERE singleton_id=1",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1,
        "CT24 one valid format row"
    );
    ct24_constraint(
        db,
        "UPDATE credential_format_state SET owner_api_blocked='old_pair_invalid' WHERE singleton_id=1;",
        check,
        "blocked reason on nonpending slot",
    );
    ct24_constraint(
        db,
        "UPDATE credential_format_state SET purge_required=1,wal_scrubbed=0 WHERE singleton_id=1;",
        check,
        "complete with purge required",
    );
    ct24_constraint(
        db,
        "UPDATE credential_format_state SET completed_at_ms=NULL,purge_required=1,wal_scrubbed=1 WHERE singleton_id=1;",
        check,
        "scrubbed with purge required",
    );

    let sign_in = "INSERT INTO credential_sign_ins VALUES('00000000-0000-4000-8000-000000000001','owner_api',zeroblob(32),0,600000);";
    ct24_valid(db, sign_in, "CT24 valid sign-in binding");
    ct24_constraint(
        db,
        "INSERT INTO credential_sign_ins VALUES('00000000-0000-4000-8000-000000000001','owner_api',zeroblob(31),0,600000);",
        check,
        "31-byte sign-in binding",
    );
    ct24_constraint(
        db,
        "INSERT INTO credential_sign_ins VALUES('00000000-0000-4000-8000-000000000001','x',zeroblob(32),0,600000);",
        check,
        "unknown sign-in slot",
    );
    ct24_constraint(
        db,
        "INSERT INTO credential_sign_ins VALUES('00000000-0000-4000-8000-00000000001','owner_api',zeroblob(32),0,600000);",
        check,
        "35-character sign-in id",
    );
    ct24_constraint(
        db,
        "INSERT INTO credential_sign_ins VALUES('00000000-0000-4000-8000-000000000001','owner_api',zeroblob(32),1,1);",
        check,
        "sign-in expiry not after start",
    );
    ct24_constraint(
        db,
        &format!(
            "{sign_in} INSERT INTO credential_sign_ins VALUES('00000000-0000-4000-8000-000000000002','fleet_api',zeroblob(32),1,600001);"
        ),
        unique,
        "duplicate sign-in binding",
    );
    assert_eq!(
        db.query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
            .optional()
            .unwrap(),
        None,
        "CT24 controls preserve foreign keys"
    );
}
#[path = "g24_m2_s/migration_addition.rs"]
mod migration_addition;
