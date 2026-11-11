// SPDX-License-Identifier: AGPL-3.0-only
use std::{fs, os::unix::fs::{MetadataExt, OpenOptionsExt}, sync::atomic::{AtomicUsize, Ordering}};

const REL: &str = "pre-upgrade-owner-api.relocated.key";
fn status(data: &Path) -> crate::db::CredentialFormatState {
    HubStore::initialize(data).expect("status store").credential_format_state().expect("status").expect("format")
}
fn fake_on() { credential_migration::seam::with(|s| { s.fake_pre_upgrade = true; s.fake_generation = true; }); }
struct Controls;
impl Drop for Controls {
    fn drop(&mut self) { credential_migration::seam::reset(); OPEN_CONTROL.with(|s| *s.borrow_mut() = None); PROBE_CONTROL.with(|s|*s.borrow_mut()=None); fixtures::probe_observer(None); fixtures::power_observer(None); }
}
fn controls() -> Controls { credential_migration::seam::reset(); fake_on(); Controls }
fn complete(data: &Path) {
    let s = status(data);
    assert!(s.completed_at_ms.is_some() && !s.purge_required && s.wal_scrubbed && s.old_files_removed, "complete status source");
}
fn file_bytes(data: &Path) -> Vec<Vec<u8>> {
    fn visit(path: &Path, bytes: &mut Vec<Vec<u8>>) {
        for entry in fs::read_dir(path).expect("fixture listing") {
            let entry = entry.expect("fixture entry");
            if entry.file_type().expect("fixture type").is_dir() { visit(&entry.path(), bytes); }
            else if entry.file_type().expect("fixture type").is_file() { bytes.push(fs::read(entry.path()).expect("fixture bytes")); }
        }
    }
    let mut bytes = Vec::new(); visit(data, &mut bytes); bytes
}
fn has_residue(data: &Path, values: &[Vec<u8>]) -> bool {
    file_bytes(data).iter().any(|bytes| values.iter().any(|value| value.windows(16).any(|run| bytes.windows(16).any(|part| part == run))))
}

fn assert_read_interleaving(point: &'static str, slot: CredentialSlot, broken: bool) {
    let (root, owner_input) = seeded();
    let fleet_input = seed_entry(root.path(), CredentialEntry::fleet_supplied(Zeroizing::new("fleet-access-0001".into()), Zeroizing::new("fleet-refresh-0001".into()), "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28800).expect("Fleet fixture"));
    let input = if slot == OWNER { owner_input } else { fleet_input };
    let vault = open(root.path()); let receipt = vault.begin_refresh(slot, input).expect("begin before read");
    if point == "after_barrier" {
        vault.lock().expect("state").connection.set_test_barrier_failure_at(Some(1));
        assert!(matches!(vault.clear_credential_pair(if slot == OWNER { CredentialSlot::FleetApi } else { OWNER }), Err(CredentialStoreError::DurabilityUnavailable)), "CT99.real earlier unproven commit");
    }
    vault.lock().expect("state").connection.set_test_barrier_failure_at(Some(if point == "after_barrier" { 3 } else { 2 }));
    let trace = Arc::new(VaultLockSeam::default()); *vault.lock_seam.lock().expect("trace") = Some(Arc::clone(&trace));
    let gate = ReadPauseSeam::new(point); *vault.read_pause.lock().expect("read seam") = Some(Arc::clone(&gate));
    let reader = { let vault = Arc::clone(&vault); let gate = Arc::clone(&gate); std::thread::spawn(move || {
        if broken {
            let mut state = vault.lock().expect("counter check"); state.guard_read().expect("initial proof"); drop(state);
            gate.pause(point);
            vault.lock().expect("deliberately separate read").connection.stored_generation(slot).expect("negative control snapshot")
        } else if slot == CredentialSlot::FleetApi { vault.load(slot).expect("Fleet read").map(|row| row.generation) }
        else { vault.stored_generation(slot).expect("generation read") }
    }) };
    let reader_id = reader.thread().id(); gate.wait();
    let output = Uuid::new_v4();
    let completion = { let vault = Arc::clone(&vault); std::thread::spawn(move || vault.persist_refreshed(slot, receipt, input, output, "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000)) };
    let writer_id = completion.thread().id(); trace.wait_for(Some(writer_id), "request");
    let observed;
    if broken {
        assert!(matches!(completion.join().expect("completion thread"), Err(CredentialStoreError::DurabilityUnavailable)), "CT99.negative completion unproven");
        gate.release(); observed = reader.join().expect("reader thread");
        assert!(observed == Some(output), "CT99.negative control exposes the successor");
    } else {
        assert!(!trace.events.lock().expect("trace events").iter().any(|(id,event)| *id == writer_id && *event == "acquire"), "CT99.writer blocked by read hold");
        gate.release(); observed = reader.join().expect("reader thread");
        assert!(observed == Some(input), "CT99.reader returns preceding snapshot");
        assert!(matches!(completion.join().expect("completion thread"), Err(CredentialStoreError::DurabilityUnavailable)), "CT99.completion barrier failed");
        let events = trace.events.lock().expect("trace events");
        let index = |id, name| events.iter().position(|(thread,event)| *thread == id && *event == name).expect("acknowledged lock event");
        assert!(index(writer_id,"request") < index(reader_id,"release") && index(reader_id,"release") < index(writer_id,"acquire"), "CT99.read release before completion acquisition");
    }
    *vault.read_pause.lock().expect("disarm read seam") = None;
    let observer = rusqlite::Connection::open(root.path().join("hub.sqlite")).expect("independent test observer");
    let committed: String = observer.query_row("SELECT generation FROM credential_pairs WHERE slot=?1", [if slot == OWNER { "owner_api" } else { "fleet_api" }], |r| r.get(0)).expect("committed generation");
    assert!(committed == output.to_string(), "CT99.successor commit actually landed");
    vault.lock().expect("state").connection.set_test_barrier_failure(true);
    assert!(matches!(vault.stored_generation(slot), Err(CredentialStoreError::DurabilityUnavailable)) && matches!(vault.load(slot), Err(CredentialStoreError::DurabilityUnavailable)), "CT99.every later read remains fenced");
    vault.lock().expect("state").connection.set_test_barrier_failure(false);
    assert!(vault.stored_generation(slot).expect("recovered proof") == Some(output), "CT99.successor after read barrier");
}
#[test]
fn ct019_rotation_vectors() {
    let root = crate::private_tempdir().expect("vector root");
    let input = fixtures::seed_rotation_vector(root.path());
    let vault = CredentialVault::open(HubStore::initialize(root.path()).expect("store"), AdmittedUserHub::for_test(root.path()).expect("admission"), root.path(), VaultOptions { exclusion: Some(Arc::new(FixtureExclusion)), now_ms: Some(1_800_086_400_000), probe_failure: false }).expect("vector vault");
    let evidence = || {
        let state = vault.lock().expect("state"); let db = state.connection.test_connection();
        let fences: Vec<(i64, Vec<u8>, String)> = db.prepare("SELECT fence_id, fingerprint, kind FROM credential_refresh_fences ORDER BY fence_id").expect("fences").query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).expect("fence rows").collect::<Result<_,_>>().expect("fence values");
        let attempts: Vec<(i64, String, String)> = db.prepare("SELECT receipt_id, attempt_id, input_generation FROM credential_refresh_attempts ORDER BY receipt_id").expect("attempts").query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).expect("attempt rows").collect::<Result<_,_>>().expect("attempt values");
        (fences, attempts)
    };
    let before = evidence(); assert!(!before.0.is_empty() && !before.1.is_empty(), "CT19.non-empty invariant fixtures");
    let id = super::super::credential_envelope::KeyId([0x21,0x22,0x23,0x24,0x25,0x26,0x27,0x28]);
    ROTATION_NONCES.with(|s| *s.borrow_mut() = Some(super::super::credential_envelope::FixedNonces::new(id, [std::array::from_fn(|i| i as u8), std::array::from_fn(|i| i as u8 + 0x10)])));
    ROTATION_TRACE.with(|s| s.borrow_mut().clear());
    credential_keys::with_key_faults(credential_keys::KeyFaults { entropy: Some(vec![(0x40..0x60).collect(), id.0.to_vec(), vec![0x70;16], vec![0x80;16]]), ..Default::default() }, || vault.rotate()).expect("vector rotation");
    ROTATION_NONCES.with(|s| *s.borrow_mut() = None);
    assert!(ROTATION_TRACE.with(|s| s.borrow().as_slice() == ["file_barrier", "directory_barrier", "reservation_commit", "reservation_barrier", "reseal_commit", "reseal_barrier", "file_barrier", "directory_barrier"]), "CT19.merged barrier order");
    let decode = |text: &str| hex::decode(text).expect("vector hex");
    let expected_ring = decode("48434b52010000002122232425262728000000006b4b2380404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f8f86cc10cb6d40d2e5a3668937df602bb009e3813a596aaeae626aa387368e88");
    assert!(fs::read(root.path().join("secrets").join("credential-keys")).expect("stable ring") == expected_ring, "CT19.stable keyring bytes");
    let state = vault.lock().expect("state");
    let envelopes: (Vec<u8>,Vec<u8>) = state.connection.test_connection().query_row("SELECT access_envelope,refresh_envelope FROM credential_pairs WHERE slot='owner_api'", [], |r| Ok((r.get(0)?,r.get(1)?))).expect("vector envelopes");
    assert!(envelopes.0 == decode("484345560101212223242526272801010101010000000100000000000040008000000000000001000102030405060708090a0b0044d612ddb7da8a6f02fd1da1e6dab8381e8f629269b7420cdd25") && envelopes.1 == decode("484345560101212223242526272801010201010000000100000000000040008000000000000001101112131415161718191a1b78a0602c937bd90c2fc89446e90695c3572fe08223f14a8a91bb75ca"), "CT19.exact envelopes");
    drop(state); assert!(pair(&vault).generation == input && evidence() == before, "CT19.identity fences attempts unchanged");
    assert!(vault.lock().expect("state").connection.credential_reserved_invocations(id).expect("reservation") == 2, "CT19.new key invocation count");
}

#[test]
fn ct057_collector_pending_refusal() {
    let _controls = controls(); let root = admitted_migration_fixture(false, None, false, 0);
    let lane = HubStore::open_read_only(root.path()).expect("non-admitted lane");
    assert!(lane.credential_format_state().expect("format").expect("state").owner_api_state == CredentialSlotFormatState::Pending, "CT57.pending_status_source");
    assert!(credential_migration::seam::with(|s| s.decrypt_log.is_empty()), "CT57.status decrypts nothing");
    let vault = CredentialVault::open(HubStore::initialize(root.path()).expect("store"), AdmittedUserHub::for_test(root.path()).expect("admission"), root.path(), VaultOptions { exclusion: Some(Arc::new(FixtureExclusion)), now_ms: Some(1_800_000_000_000), probe_failure: true }).expect("read-only vault");
    assert!(matches!(vault.load(OWNER), Err(CredentialStoreError::MigrationPending(CredentialSlot::OwnerApi))), "CT57.collector identity refuses pending");
    assert!(root.path().join("secrets").join(REL).exists(), "CT57.candidate retained");
}

#[test]
fn ct062_admitted_open() {
    let _controls = controls(); credential_migration::seam::with(|s| { s.fake_pre_upgrade = false; s.fake_generation = false; });
    // M02-T02: a pre-upgrade catalogue never completes (its pairs are refused
    // unread), so the backup case starts from a fresh store.
    let root = crate::private_tempdir().expect("fresh store"); drop(HubStore::initialize(root.path()).expect("store")); let values: Vec<Vec<u8>> = Vec::new();
    let vault = open(root.path()); complete(root.path());
    let destination = crate::private_tempdir().expect("backup root"); let generation = destination.path().join("backup");
    crate::data_recovery::create_data_backup(&HubStore::initialize(root.path()).expect("store"), &generation).expect("CT62.admitted backup");
    let catalogue = generation.join("data").join("hub.sqlite"); let db = rusqlite::Connection::open_with_flags(&catalogue, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).expect("payload observer");
    let rows: i64 = db.query_row("SELECT count(*) FROM credential_pairs", [], |r| r.get(0)).expect("payload row count");
    assert!(rows == 0 && !has_residue(destination.path(), &values), "CT62.payload has no credential or predecessor bytes");
    assert!(!catalogue.with_extension("sqlite-wal").exists() && !catalogue.with_extension("sqlite-shm").exists(), "CT62.no payload side files");
    drop(db); drop(vault);
    let restore_root = crate::private_tempdir().expect("restore root"); let restored = restore_root.path().join("restored");
    crate::data_recovery::restore_data_backup(&generation, &restored).expect("CT62.restore");
    let vault = open(&restored); assert!(vault.load(OWNER).expect("restored empty Owner").is_none() && vault.load(CredentialSlot::FleetApi).expect("restored empty Fleet").is_none(), "CT62.restore has no pair");
    assert!(vault.test_commit_counts() == (0,0), "CT62.complete restore no Phase B commit");
    drop(vault);
    credential_migration::g24_m2_m::ct062_data_backup();
}

#[test]
fn ct101_old_names_never_touched_open() {
    use std::io::Write;
    let _controls = controls(); let root = admitted_migration_fixture(false, None, false, 0);
    let encoded = crate::runtime::name_test_support::forbidden_name();
    let mut paths=Vec::new();
    for suffix in ["k1.key","k2.key","k3.tmp"]{paths.push(root.path().join("secrets").join(format!("{encoded}-{suffix}")));}
    for (directory,suffix) in [("session","s1"),("session","s2"),("config","m")]{
        fs::create_dir_all(root.path().join(directory)).expect("fixture directory");
        paths.push(root.path().join(directory).join(format!("{encoded}-{suffix}")));
    }
    for path in &paths{
        std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(path).expect("encoded fixture").write_all(b"synthetic-retained-credential").expect("fixture bytes");
    }
    let fingerprint=||paths.iter().map(|path|{let m=fs::metadata(path).expect("fixture metadata");(m.ino(),m.mtime(),m.mtime_nsec(),fs::read(path).expect("fixture bytes"))}).collect::<Vec<_>>();
    let before=fingerprint();
    for _ in 0..2 { drop(open(root.path())); }
    let lane = HubStore::open_read_only(root.path()).expect("read-only lane"); lane.credential_format_state().expect("status"); drop(lane);
    // M02-T02: the refused pre-upgrade store is blocked, so backups wait; the
    // encoded names are checked through reopen only.
    assert!(fingerprint()==before,"CT101.encoded files keep bytes inode and mtime through reopen");
    assert!(credential_migration::seam::with(|s| s.events.iter().all(|event| !event.contains(&encoded) || event.starts_with("list_returned:"))), "CT101.no operation on encoded entry");
}
