// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use std::cell::Cell;
use std::collections::HashSet;

const ENV_A: &str = "484345560101010203040506070801010101010000000100000000000040008000000000000001000102030405060708090a0b8b03111345569345953066400944965d94bc293a46884c8b0894e7";
const ENV_R: &str = "484345560101010203040506070801010201010000000100000000000040008000000000000001101112131415161718191a1bf687af998f3831c8109298587b6f51d1bb0984d2598cef5f271869f2";
const ENV_E: &str = "484345560101010203040506070801010203010000000100000000000040008000000000000001202122232425262728292a2b80107cc886bcb1ca1eb18e3eb218b866";
const ENV_A2: &str = "484345560101212223242526272801010101010000000100000000000040008000000000000001000102030405060708090a0b0044d612ddb7da8a6f02fd1da1e6dab8381e8f629269b7420cdd25";
const ENV_R2: &str = "484345560101212223242526272801010201010000000100000000000040008000000000000001101112131415161718191a1b78a0602c937bd90c2fc89446e90695c3572fe08223f14a8a91bb75ca";

fn bytes(hexadecimal: &str, length: usize) -> Vec<u8> {
    assert_eq!(hexadecimal.len(), length * 2, "hex transcription width");
    let decoded = hex::decode(hexadecimal).expect("synthetic hex");
    assert_eq!(decoded.len(), length, "decoded vector width");
    decoded
}

fn array<const N: usize>(hexadecimal: &str) -> [u8; N] {
    bytes(hexadecimal, N)
        .try_into()
        .expect("fixed vector width")
}

fn root(second: bool) -> [u8; 32] {
    std::array::from_fn(|i| i as u8 + if second { 0x40 } else { 0 })
}

fn kid(second: bool) -> KeyId {
    KeyId(array(if second {
        "2122232425262728"
    } else {
        "0102030405060708"
    }))
}

fn place(column: CredentialColumn) -> EnvelopePlace {
    EnvelopePlace {
        table: 1,
        slot: CredentialSlot::OwnerApi,
        column,
        origin: Some(CredentialOrigin::HubLogin),
        owner: RefreshOwner::Hub,
        account_id: 1,
        generation: Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap(),
    }
}

fn empty_place() -> EnvelopePlace {
    EnvelopePlace {
        origin: Some(CredentialOrigin::ProviderEntry),
        ..place(CredentialColumn::Refresh)
    }
}

fn nonce(start: u8) -> [u8; 12] {
    std::array::from_fn(|i| start + i as u8)
}

struct Keys {
    entries: Vec<(KeyId, Zeroizing<[u8; 32]>)>,
    lookups: Cell<usize>,
}

impl fmt::Debug for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Keys([redacted])")
    }
}

impl Keys {
    fn new(ids: &[bool]) -> Self {
        Self {
            entries: ids
                .iter()
                .map(|second| (kid(*second), derive_envelope_key(&root(*second))))
                .collect(),
            lookups: Cell::new(0),
        }
    }
}

impl EnvelopeKeys for Keys {
    fn envelope_key(&self, id: KeyId) -> Option<&Zeroizing<[u8; 32]>> {
        self.lookups.set(self.lookups.get() + 1);
        self.entries
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, key)| key)
    }
}

#[test]
fn ct001_derivations() {
    let salt: Vec<u8> = (0..=0x0c).collect();
    let ikm = [0x0b; 22];
    let info: Vec<u8> = (0xf0..=0xf9).collect();
    assert_eq!(salt.len(), 13);
    assert_eq!(info.len(), 10);
    assert_eq!(
        *hkdf_extract(&salt, &ikm),
        array::<32>("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"),
        "CT1 RFC5869 PRK"
    );
    assert_eq!(
        *hkdf_sha256_32(&salt, &ikm, &info),
        array::<32>("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf"),
        "CT1 RFC5869 OKM"
    );
    for (second, expected) in [
        (
            false,
            "5fd1704c77c7967579f0e5bb592ccd71e02aa2303158bf9aad7b27f82f72e2c6",
        ),
        (
            true,
            "3929e1e6316520ba3f6fbf3e6da3aeaf0cffbd31a3e58b5cf2f1fcd23de199bf",
        ),
    ] {
        assert_eq!(
            *derive_envelope_key(&root(second)),
            array::<32>(expected),
            "CT1 envelope key"
        );
    }
    assert_eq!(ENVELOPE_KEY_INFO.len(), 48);
    assert_eq!(
        *hkdf_sha256_32(&[], &root(false), ENVELOPE_KEY_INFO),
        *derive_envelope_key(&root(false)),
        "CT1 absent salt"
    );
}

#[test]
fn ct002_fingerprints() {
    for (token, expected) in [
        (
            b"refresh-0001".as_slice(),
            "1ba5b78e19fcaa428927abebbc334f2dbe9db01f883651e921fd6b0a3e9155b2",
        ),
        (
            b"refresh-0002".as_slice(),
            "9d7b2d5dc988d038d72af2900421c6f22d7f80810c4c2347d314c1e5d103a1eb",
        ),
        (
            b"refresh-0001\r\n".as_slice(),
            "1ba5b78e19fcaa428927abebbc334f2dbe9db01f883651e921fd6b0a3e9155b2",
        ),
        (
            b" \trefresh-0001\n".as_slice(),
            "1ba5b78e19fcaa428927abebbc334f2dbe9db01f883651e921fd6b0a3e9155b2",
        ),
        (
            b"refresh-0001 x".as_slice(),
            "0d3245718256a656844eca49036600df37740ab113b118b416ab5c2d7fa9795a",
        ),
    ] {
        assert_eq!(
            refresh_fingerprint(token),
            array::<32>(expected),
            "CT2 fingerprint"
        );
    }
    let token = b" \trefresh-0001\n";
    let canonical = canonical_token(token);
    assert_eq!(canonical, b"refresh-0001", "CT2 canonical bytes");
    assert_eq!(
        canonical.as_ptr(),
        token[2..].as_ptr(),
        "CT2 borrowed canonical slice"
    );
    assert_eq!(FINGERPRINT_LABEL.len(), 36);
    let mut input = FINGERPRINT_LABEL.to_vec();
    input.push(0);
    input.extend_from_slice(b"refresh-0001");
    assert_eq!(
        refresh_fingerprint(token),
        <[u8; 32]>::from(Sha256::digest(input)),
        "CT2 domain separation"
    );
    for token in [b"".as_slice(), b" \t\r\n", b"\r\n \t"] {
        assert!(canonical_token(token).is_empty(), "CT2 all edge whitespace");
    }
    let retained = b"\x0b\x0c\xc2\xa0x \t y\xc2\xa0\x0c\x0b";
    assert_eq!(
        canonical_token(retained),
        retained,
        "CT2 only SP HTAB CR LF are stripped"
    );
}

#[test]
fn ct003_seal_vectors() {
    let mut first = FixedNonces::new(kid(false), [nonce(0), nonce(0x10), nonce(0x20)]);
    let mut second_key = FixedNonces::new(kid(true), [nonce(0), nonce(0x10)]);
    for (second, p, start, plaintext, expected, length) in [
        (
            false,
            place(CredentialColumn::Access),
            0,
            b"access-0001".as_slice(),
            ENV_A,
            78,
        ),
        (
            false,
            place(CredentialColumn::Refresh),
            0x10,
            b"refresh-0001".as_slice(),
            ENV_R,
            79,
        ),
        (false, empty_place(), 0x20, b"".as_slice(), ENV_E, 67),
        (
            true,
            place(CredentialColumn::Access),
            0,
            b"access-0001".as_slice(),
            ENV_A2,
            78,
        ),
        (
            true,
            place(CredentialColumn::Refresh),
            0x10,
            b"refresh-0001".as_slice(),
            ENV_R2,
            79,
        ),
    ] {
        let nonces = if second { &mut second_key } else { &mut first };
        let calls_before = nonces.calls();
        let sealed = seal_value(
            &derive_envelope_key(&root(second)),
            kid(second),
            &p,
            plaintext,
            nonces,
        )
        .unwrap();
        assert_eq!(&sealed[39..51], &nonce(start), "CT3 ordered nonce");
        assert_eq!(sealed.len(), length, "CT3 envelope length");
        assert_eq!(sealed, bytes(expected, length), "CT3 exact envelope");
        assert_eq!(nonces.calls(), calls_before + 1, "CT3 one nonce per seal");
    }
    assert_eq!(
        (
            CredentialSlot::OwnerApi as u8,
            CredentialSlot::OwnerApi.as_str()
        ),
        (1, "owner_api")
    );
    assert_eq!(
        (
            CredentialSlot::FleetApi as u8,
            CredentialSlot::FleetApi.as_str()
        ),
        (2, "fleet_api")
    );
    assert_eq!(
        (
            CredentialColumn::Access as u8,
            CredentialColumn::Access.as_str()
        ),
        (1, "access")
    );
    assert_eq!(
        (
            CredentialColumn::Refresh as u8,
            CredentialColumn::Refresh.as_str()
        ),
        (2, "refresh")
    );
    for (origin, code, text) in [
        (CredentialOrigin::HubLogin, 1, "hub_login"),
        (CredentialOrigin::DirectEntry, 2, "direct_entry"),
        (CredentialOrigin::ProviderEntry, 3, "provider_entry"),
        (CredentialOrigin::FleetOauth, 4, "fleet_oauth"),
        (CredentialOrigin::SourceToken, 5, "source_token"),
    ] {
        assert_eq!(
            (origin as u8, origin.as_str()),
            (code, text),
            "CT3 origin codes"
        );
    }
    assert_eq!(
        (RefreshOwner::Hub as u8, RefreshOwner::Hub.as_str()),
        (1, "hub")
    );
    assert_eq!(
        (
            RefreshOwner::External as u8,
            RefreshOwner::External.as_str()
        ),
        (2, "external")
    );
    let p = EnvelopePlace {
        origin: None,
        ..place(CredentialColumn::Access)
    };
    assert_eq!(encode_header(kid(false), &p)[17], 0, "CT3 NULL origin");
    let header = encode_header(kid(false), &p);
    check_place(&parse_header(&header).unwrap(), &p).unwrap();
}

#[test]
fn ct004_open_vectors() {
    let stable = Keys::new(&[false]);
    for (value, length, p, expected) in [
        (
            ENV_A,
            78,
            place(CredentialColumn::Access),
            b"access-0001".as_slice(),
        ),
        (
            ENV_R,
            79,
            place(CredentialColumn::Refresh),
            b"refresh-0001".as_slice(),
        ),
        (ENV_E, 67, empty_place(), b"".as_slice()),
    ] {
        let opened = open_value(&stable, &p, &bytes(value, length)).unwrap();
        assert_eq!(opened.as_slice(), expected, "CT4 stable plaintext");
        assert_eq!(
            opened.capacity(),
            expected.len(),
            "CT4 exactly sized output"
        );
    }
    let rotating = Keys::new(&[true, false]);
    for value in [ENV_A2, ENV_A] {
        assert_eq!(
            open_value(
                &rotating,
                &place(CredentialColumn::Access),
                &bytes(value, 78)
            )
            .unwrap()
            .as_slice(),
            b"access-0001",
            "CT4 rotating current and previous"
        );
    }
}

#[test]
fn ct005_aead_vectors() {
    // Published GCM test cases 13, 14 and 16, via the codec's AEAD seam.
    for (plaintext, ciphertext, tag) in [
        (vec![], vec![], "530f8afbc74536b9a963b4f1c4cb738b"),
        (
            vec![0; 16],
            bytes("cea7403d4d606b6e074ec5d3baf39d18", 16),
            "d0d1c8a799996bf0265b98b5d48ab919",
        ),
    ] {
        let mut working = plaintext;
        assert_eq!(
            aead_seal(&[0; 32], &[0; 12], &[], &mut working).unwrap(),
            array::<16>(tag),
            "CT5 GCM13/14 tag"
        );
        assert_eq!(working, ciphertext, "CT5 GCM14 ciphertext");
    }
    let key = array::<32>("feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308");
    let nonce = array::<12>("cafebabefacedbaddecaf888");
    let aad = bytes("feedfacedeadbeeffeedfacedeadbeefabaddad2", 20);
    let plain = bytes(
        "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
        60,
    );
    let expected = bytes(
        "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662",
        60,
    );
    let mut working = plain.clone();
    let tag = aead_seal(&key, &nonce, &aad, &mut working).unwrap();
    assert_eq!(
        tag,
        array::<16>("76fc6ece0f4e1768cddf8853bb2d551b"),
        "CT5 GCM16 tag"
    );
    assert_eq!(working, expected, "CT5 GCM16 ciphertext");
    let cipher = Aes256Gcm::new((&key).into());
    let mut bad_tag = tag;
    bad_tag[0] ^= 1;
    assert!(
        cipher
            .decrypt_inout_detached(
                &Nonce::from(nonce),
                &aad,
                working.as_mut_slice().into(),
                &Tag::from(bad_tag)
            )
            .is_err(),
        "CT5 bad tag refused"
    );
    assert_eq!(working, expected, "CT5 failed tag leaves ciphertext intact");
    cipher
        .decrypt_inout_detached(
            &Nonce::from(nonce),
            &aad,
            working.as_mut_slice().into(),
            &Tag::from(tag),
        )
        .unwrap();
    assert_eq!(working, plain, "CT5 verified decrypt");
}

fn refusal(
    keys: &Keys,
    p: &EnvelopePlace,
    value: &[u8],
    error: EnvelopeError,
    lookups: usize,
    decryptions: usize,
) {
    keys.lookups.set(0);
    DECRYPTIONS.with(|count| count.set(0));
    assert_eq!(
        open_value(keys, p, value).err(),
        Some(error),
        "CT6 first error"
    );
    assert_eq!(keys.lookups.get(), lookups, "CT6 key lookup count");
    DECRYPTIONS.with(|count| assert_eq!(count.get(), decryptions, "CT6 decryption count"));
}

#[test]
fn ct006_reader_order() {
    let keys = Keys::new(&[false]);
    let p = place(CredentialColumn::Access);
    let value = bytes(ENV_A, 78);
    refusal(&keys, &p, &value[..66], EnvelopeError::Truncated, 0, 0);
    let mut huge = vec![0; 16_452];
    huge[..39].copy_from_slice(&value[..39]);
    refusal(&keys, &p, &huge, EnvelopeError::OverCap, 0, 0);
    for (offset, byte, error) in [
        (0, 0x49, EnvelopeError::BadMagic),
        (4, 2, EnvelopeError::UnknownVersion(2)),
        (5, 2, EnvelopeError::UnknownAlgorithm(2)),
        (5, 0, EnvelopeError::UnknownAlgorithm(0)),
        (14, 2, EnvelopeError::BindingMismatch(BindingField::Table)),
        (17, 9, EnvelopeError::BindingMismatch(BindingField::Origin)),
        (15, 9, EnvelopeError::BindingMismatch(BindingField::Slot)),
        (16, 9, EnvelopeError::BindingMismatch(BindingField::Column)),
        (18, 9, EnvelopeError::BindingMismatch(BindingField::Owner)),
    ] {
        let mut changed = value.clone();
        changed[offset] = byte;
        refusal(&keys, &p, &changed, error, 0, 0);
    }
    let mut variants = Vec::new();
    let mut q = p.clone();
    q.column = CredentialColumn::Refresh;
    variants.push((q, BindingField::Column));
    let mut q = p.clone();
    q.slot = CredentialSlot::FleetApi;
    variants.push((q, BindingField::Slot));
    let mut q = p.clone();
    q.origin = Some(CredentialOrigin::DirectEntry);
    variants.push((q, BindingField::Origin));
    let mut q = p.clone();
    q.owner = RefreshOwner::External;
    variants.push((q, BindingField::Owner));
    let mut q = p.clone();
    q.account_id = 2;
    variants.push((q, BindingField::Account));
    let mut q = p.clone();
    q.generation = Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap();
    variants.push((q, BindingField::Generation));
    for (expected, field) in variants {
        refusal(
            &keys,
            &expected,
            &value,
            EnvelopeError::BindingMismatch(field),
            0,
            0,
        );
    }
    // M2P-25: raw unknown codes are checked at their field, never during parse.
    let offsets = [14, 15, 16, 17, 18];
    let fields = [
        BindingField::Table,
        BindingField::Slot,
        BindingField::Column,
        BindingField::Origin,
        BindingField::Owner,
    ];
    for earlier in 0..offsets.len() {
        for later in earlier + 1..offsets.len() {
            let mut changed = value.clone();
            changed[offsets[earlier]] = 9;
            changed[offsets[later]] = 255;
            let header = parse_header(changed[..39].try_into().unwrap())
                .expect("CT6 parse keeps unknown bindings raw");
            assert_eq!(
                check_place(&header, &p),
                Err(EnvelopeError::BindingMismatch(fields[earlier])),
                "CT6 M2P25 earlier binding wins"
            );
            refusal(
                &keys,
                &p,
                &changed,
                EnvelopeError::BindingMismatch(fields[earlier]),
                0,
                0,
            );
        }
    }
    // E2-OR-02: a valid earlier mismatch precedes a later unknown code.
    for (earlier_offset, valid_mismatch, later_offset, field) in [
        (15, 2, 17, BindingField::Slot),
        (16, 2, 18, BindingField::Column),
        (17, 2, 18, BindingField::Origin),
    ] {
        let mut changed = value.clone();
        changed[earlier_offset] = valid_mismatch;
        changed[later_offset] = 9;
        let header = parse_header(changed[..39].try_into().unwrap()).unwrap();
        assert_eq!(
            check_place(&header, &p),
            Err(EnvelopeError::BindingMismatch(field)),
            "CT6 valid earlier mismatch beats later unknown code"
        );
        refusal(
            &keys,
            &p,
            &changed,
            EnvelopeError::BindingMismatch(field),
            0,
            0,
        );
    }
    let mut changed = value.clone();
    changed[15] = 9;
    let mut account_two = p.clone();
    account_two.account_id = 2;
    let header = parse_header(changed[..39].try_into().unwrap()).unwrap();
    assert_eq!(
        check_place(&header, &account_two),
        Err(EnvelopeError::BindingMismatch(BindingField::Slot)),
        "CT6 earlier unknown precedes account mismatch"
    );
    refusal(
        &keys,
        &account_two,
        &changed,
        EnvelopeError::BindingMismatch(BindingField::Slot),
        0,
        0,
    );
    let mut layered = value.clone();
    layered[0] = 0;
    layered[4] = 2;
    layered[5] = 2;
    refusal(&keys, &p, &layered[..66], EnvelopeError::Truncated, 0, 0);
    refusal(&keys, &p, &layered, EnvelopeError::BadMagic, 0, 0);
    layered[0] = value[0];
    refusal(&keys, &p, &layered, EnvelopeError::UnknownVersion(2), 0, 0);
    refusal(
        &Keys::new(&[true]),
        &p,
        &value,
        EnvelopeError::UnknownKey,
        1,
        0,
    );
    for (len, expected) in [
        (66, Err(EnvelopeError::Truncated)),
        (67, Ok(())),
        (16_451, Ok(())),
        (16_452, Err(EnvelopeError::OverCap)),
    ] {
        assert_eq!(
            check_envelope_length(len),
            expected,
            "CT6 standalone length"
        );
    }
    for (hexadecimal, length, p) in [
        (ENV_A, 78, p),
        (ENV_R, 79, place(CredentialColumn::Refresh)),
        (ENV_E, 67, empty_place()),
    ] {
        let value = bytes(hexadecimal, length);
        let header = parse_header(value[..39].try_into().unwrap()).unwrap();
        assert_eq!(header.version, 1, "CT6 parsed version");
        assert_eq!(header.algorithm, 1, "CT6 parsed algorithm");
        assert_eq!(header.key_id, kid(false), "CT6 parsed key id");
        check_place(&header, &p).expect("CT6 header-only check");
    }
}

#[test]
fn ct007_tampering() {
    let keys = Keys::new(&[false, true]);
    for mode in 0..4 {
        let mut value = bytes(ENV_A, 78);
        let mut p = place(CredentialColumn::Access);
        match mode {
            0 => value[51] ^= 1,
            1 => *value.last_mut().unwrap() ^= 1,
            2 => {
                value[16] = 2;
                p.column = CredentialColumn::Refresh;
            }
            3 => value[6..14].copy_from_slice(&kid(true).0),
            _ => unreachable!(),
        }
        refusal(&keys, &p, &value, EnvelopeError::Corrupt, 1, 1);
    }
}

#[test]
fn ct008_caps() {
    let key = derive_envelope_key(&root(false));
    let p = place(CredentialColumn::Access);
    let mut nonces = FixedNonces::new(kid(false), [nonce(0x30)]);
    let value = seal_value(&key, kid(false), &p, &vec![b'a'; 16_384], &mut nonces).unwrap();
    assert_eq!(value.len(), 16_451, "CT8 maximum envelope");
    assert_eq!(
        open_value(&Keys::new(&[false]), &p, &value)
            .unwrap()
            .as_slice(),
        vec![b'a'; 16_384],
        "CT8 cap roundtrip"
    );
    assert_eq!(nonces.calls(), 1);
    assert_eq!(
        seal_value(&key, kid(false), &p, &vec![b'a'; 16_385], &mut nonces),
        Err(EnvelopeError::TokenTooLarge),
        "CT8 plus-one refused"
    );
    assert_eq!(nonces.calls(), 1, "CT8 oversized token draws no nonce");
    assert_eq!(MAX_TOKEN_PLAINTEXT_BYTES, 16_384);
    assert_eq!(MAX_TOKEN_ENVELOPE_BYTES, 16_451);
}

#[test]
fn ct009_nonce_source() {
    let key = derive_envelope_key(&root(false));
    let p = place(CredentialColumn::Access);
    let mut nonces = OsNonces;
    let mut seen = HashSet::new();
    for _ in 0..10_000 {
        let value = seal_value(&key, kid(false), &p, b"access-0001", &mut nonces).unwrap();
        let nonce: [u8; 12] = value[39..51].try_into().unwrap();
        assert!(seen.insert(nonce), "CT9 unique OS nonce");
    }
    assert_eq!(seen.len(), 10_000, "CT9 bounded sample count");
    assert_eq!(
        seal_value(
            &key,
            kid(false),
            &p,
            b"access-0001",
            &mut FixedNonces::with_error(kid(false), EnvelopeError::Corrupt),
        ),
        Err(EnvelopeError::EntropyUnavailable),
        "CT9 nonce errors normalise to EntropyUnavailable"
    );
    // The single shared source is reachable at the handoff path used by K/V1.
    use crate::auth::credential_envelope::FixedNonces as SharedFixedNonces;
    let test_id = KeyId([0x61; 8]);
    let a = nonce(0x70);
    let b = nonce(0x80);
    let mut ordered = SharedFixedNonces::new(test_id, [a, b]);
    assert_eq!(ordered.next().unwrap(), a, "CT9 FixedNonces FIFO first");
    assert_eq!(ordered.next().unwrap(), b, "CT9 FixedNonces FIFO second");
    let mut repeated = SharedFixedNonces::new(test_id, [a, a]);
    assert_eq!(repeated.next().unwrap(), a, "CT9 reuse first draw");
    let reused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| repeated.next()));
    let panic = reused.expect_err("CT9 repeated key/nonce must panic");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied());
    assert_eq!(
        message,
        Some("nonce reused for key id"),
        "CT9 reuse guard panic"
    );
    let mut exhausted = SharedFixedNonces::new(test_id, []);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| exhausted.next()));
    let panic = result.expect_err("CT9 exhausted source must panic");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied());
    assert_eq!(
        message,
        Some("fixed nonce exhausted"),
        "CT9 exhaustion panic"
    );
}

#[test]
fn ct010_rendering() {
    let p = place(CredentialColumn::Access);
    let header = parse_header(bytes(ENV_A, 78)[..39].try_into().unwrap()).unwrap();
    let mut renderings = vec![
        format!("{:?}", kid(false)),
        format!("{:?}", kid(true)),
        format!("{p:?}"),
        format!("{header:?}"),
        format!("{:?}", OsNonces),
        format!("{:?}", FixedNonces::new(kid(false), [nonce(0)])),
        format!("{:?}", Keys::new(&[false, true])),
    ];
    for v in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        renderings.push(format!("{v:?}"));
    }
    for v in [CredentialColumn::Access, CredentialColumn::Refresh] {
        renderings.push(format!("{v:?}"));
    }
    for v in [
        CredentialOrigin::HubLogin,
        CredentialOrigin::DirectEntry,
        CredentialOrigin::ProviderEntry,
        CredentialOrigin::FleetOauth,
        CredentialOrigin::SourceToken,
    ] {
        renderings.push(format!("{v:?}"));
    }
    for v in [RefreshOwner::Hub, RefreshOwner::External] {
        renderings.push(format!("{v:?}"));
    }
    for v in [
        BindingField::Table,
        BindingField::Slot,
        BindingField::Column,
        BindingField::Origin,
        BindingField::Owner,
        BindingField::Account,
        BindingField::Generation,
    ] {
        renderings.push(format!("{v:?}"));
        renderings.push(format!("{:?}", EnvelopeError::BindingMismatch(v)));
    }
    for error in [
        EnvelopeError::Truncated,
        EnvelopeError::OverCap,
        EnvelopeError::BadMagic,
        EnvelopeError::UnknownKey,
        EnvelopeError::Corrupt,
        EnvelopeError::TokenTooLarge,
        EnvelopeError::EntropyUnavailable,
    ] {
        renderings.push(format!("{error:?}"));
    }
    for code in 0..=255 {
        renderings.push(format!(
            "{:?} {:?}",
            EnvelopeError::UnknownVersion(code),
            EnvelopeError::UnknownAlgorithm(code)
        ));
    }
    let mut forbidden = vec![
        root(false).to_vec(),
        root(true).to_vec(),
        derive_envelope_key(&root(false)).to_vec(),
        derive_envelope_key(&root(true)).to_vec(),
    ];
    for token in [
        b"access-0001".as_slice(),
        b"refresh-0001",
        b"access-0002",
        b"refresh-0002",
    ] {
        forbidden.push(token.to_vec());
        forbidden.push(refresh_fingerprint(token).to_vec());
    }
    for (value, len) in [
        (ENV_A, 78),
        (ENV_R, 79),
        (ENV_E, 67),
        (ENV_A2, 78),
        (ENV_R2, 79),
    ] {
        forbidden.push(bytes(value, len));
    }
    for start in [0u8, 0x10, 0x20] {
        forbidden.push((start..start + 12).collect());
    }
    for rendered in renderings {
        for secret in &forbidden {
            assert!(
                !rendered.to_ascii_lowercase().contains(&hex::encode(secret)),
                "CT10 no hex secret in Debug"
            );
            if let Ok(text) = std::str::from_utf8(secret) {
                assert!(!rendered.contains(text), "CT10 no text secret in Debug");
            }
            assert!(
                !rendered.contains(&format!("{secret:?}")),
                "CT10 no decimal bytes in Debug"
            );
        }
    }
}
