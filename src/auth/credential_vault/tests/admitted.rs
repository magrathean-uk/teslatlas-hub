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
fn ack_at_boundaries(data: &Path) -> Arc<AtomicUsize> {
    let db = rusqlite::Connection::open(data.join("hub.sqlite")).expect("ack connection");
    db.execute_batch("CREATE TABLE IF NOT EXISTS v1_ack(n INTEGER PRIMARY KEY, boundary TEXT)").expect("ack fixture");
    let count = Arc::new(AtomicUsize::new(0)); let seen = Arc::clone(&count);
    credential_migration::seam::with(|s| s.boundary = Some(Box::new(move |name| {
        db.execute("INSERT INTO v1_ack(boundary) VALUES(?1)", [name]).expect("acknowledged commit");
        seen.fetch_add(1, Ordering::SeqCst); true
    })));
    count
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
fn ct051_admitted_open_crash_points() {
    for fleet in [false, true] {
        for candidate in 0..if fleet { 3 } else { 1 } {
            for point in ["after_keyring", "after_conversion", "after_purge_commit", "after_scrub_marker", "after_files_marker", "first_removal"] {
                let _controls = controls();
                let root = admitted_migration_fixture(fleet, None, false, candidate);
                credential_migration::seam::with(|s| s.boundary = Some(Box::new(move |name| if point=="first_removal"{!name.starts_with("after_removal:")}else{name != point})));
                let vault = open(root.path());
                assert!(vault.lock().expect("state").resume_error.is_some(), "CT51.boundary stopped");
                assert!(status(root.path()).completed_at_ms.is_none(), "CT51.only final step completes");
                if point == "after_keyring" || point == "after_conversion" {
                    let name = if fleet { ["fleet-credentials.key", "pre-upgrade-fleet-api.key", ".fleet-credentials.pending.key"][candidate] } else { REL };
                    assert!(root.path().join("secrets").join(name).exists(), "CT51.candidate retained before proof");
                }
                let id = vault.keys.read().expect("keys").as_ref().expect("published ring").keyring().current.id;
                drop(vault); credential_migration::seam::with(|s| s.boundary = None);
                let vault = open(root.path()); complete(root.path());
                let row = vault.load(if fleet { CredentialSlot::FleetApi } else { OWNER }).expect("recovered load").expect("pair");
                assert!(row.access.as_str() == "access-0001" && row.refresh.as_str() == "refresh-0001" && (row.expires_at, row.next_refresh_at) == (1_800_028_800, 1_800_021_600), "CT51.recovered metadata and plaintext");
                assert!(vault.keys.read().expect("keys").as_ref().expect("ring").keyring().current.id == id, "CT51.key reused");
            }
        }
    }
    for fleet in [false,true]{for candidate in 0..if fleet{3}else{1}{for fault in ["rollback","conversion_barrier","vacuum_interrupt"]{
        let _controls=controls();let root=admitted_migration_fixture(fleet,None,false,candidate);
        let interrupt=Arc::new(std::sync::atomic::AtomicBool::new(false));let handler=Arc::clone(&interrupt);
        OPEN_CONTROL.with(|s|*s.borrow_mut()=Some(Box::new(move|connection|match fault{
            "rollback"=>{connection.test_connection().execute_batch("CREATE TEMP TRIGGER refuse_conversion BEFORE INSERT ON credential_pairs BEGIN SELECT RAISE(ABORT,'conversion refused'); END;").expect("conversion rollback seam");},
            "conversion_barrier"=>connection.set_test_barrier_failure_at(Some(3)),
            _=>{let flag=Arc::clone(&handler);let _=connection.test_connection().progress_handler(1,Some(move||flag.load(Ordering::SeqCst)));}
        })));
        if fault=="vacuum_interrupt"{let flag=Arc::clone(&interrupt);credential_migration::seam::with(|s|s.boundary=Some(Box::new(move|point|{if point=="before_vacuum"{flag.store(true,Ordering::SeqCst);}true})));}
        let vault=open(root.path());interrupt.store(false,Ordering::SeqCst);
        let _=vault.lock().expect("state").connection.test_connection().progress_handler(1,None::<fn()->bool>);
        assert!(vault.lock().expect("state").resume_error.is_some(),"CT51.real fault stopped open work");
        let candidate_name=if fleet{["fleet-credentials.key","pre-upgrade-fleet-api.key",".fleet-credentials.pending.key"][candidate]}else{REL};
        assert!(root.path().join("secrets").join(candidate_name).exists(),"CT51.no removal before conversion proof");
        if fault=="rollback"{assert!(status(root.path()).converted_at_ms.is_none() && vault.credential_pair_presence(if fleet{CredentialSlot::FleetApi}else{OWNER}).expect("presence").is_none(),"CT51.transaction rolled back");}
        let key=vault.keys.read().expect("keys").as_ref().expect("ring").keyring().current.id;
        drop(vault);OPEN_CONTROL.with(|s|*s.borrow_mut()=None);credential_migration::seam::with(|s|s.boundary=None);
        let vault=open(root.path());complete(root.path());let row=vault.load(if fleet{CredentialSlot::FleetApi}else{OWNER}).expect("recovered load").expect("pair");
        assert!(row.access.as_str()=="access-0001" && row.refresh.as_str()=="refresh-0001" && vault.keys.read().expect("keys").as_ref().expect("ring").keyring().current.id==key,"CT51.real fault recovery retains pair and key");
    }}}
}

#[test]
fn ct052_admitted_open() {
    let _controls = controls(); let root = credential_migration::g24_m2_m::seed_admitted_large();
    let db = rusqlite::Connection::open(root.path().join("hub.sqlite")).expect("fixture observer");
    let prior: Vec<u8> = db.query_row("SELECT access FROM legacy_owner_tokens", [], |r| r.get(0)).expect("predecessor bytes");
    OPEN_CONTROL.with(|s| *s.borrow_mut() = Some(Box::new(|c| { c.set_test_max_page_count(1).expect("page cap"); })));
    let vault = open(root.path());
    assert!(matches!(&vault.lock().expect("state").resume_error, Some(CredentialStoreError::Store(StoreError::CredentialPairStore(rusqlite::Error::SqliteFailure(code, _)))) if code.code == rusqlite::ErrorCode::DiskFull), "CT52.SQLITE_FULL");
    assert!(status(root.path()).owner_api_state == CredentialSlotFormatState::Pending && root.path().join("secrets").join(REL).exists(), "CT52.pending_status_source");
    let retained: Vec<u8> = db.query_row("SELECT access FROM legacy_owner_tokens", [], |r| r.get(0)).expect("retained bytes");
    assert!(retained == prior, "CT52.byte identical predecessor");
    drop(vault); OPEN_CONTROL.with(|s| *s.borrow_mut() = None);
    let vault = open(root.path()); complete(root.path()); assert!(pair(&vault).access.len() == 16_000, "CT52.recovered pair");
}

#[test]
fn ct054_admitted_open() {
    for fleet in [false, true] { for mask in 1..8 { for authentic in 0..3 { if mask & (1 << authentic) == 0 { continue; }
        for equal in [false, true] {
            let _controls = controls();
            let root = credential_migration::g24_m2_m::seed_admitted_candidate_set(fleet, mask, authentic, equal);
            let vault = open(root.path());
            let row = vault.load(if fleet { CredentialSlot::FleetApi } else { OWNER }).expect("converted load").expect("pair");
            assert!(row.refresh.as_str() == "refresh-0001", "CT54.authentic candidate opens");
            let attempts = credential_migration::seam::with(|s| s.decrypt_log.len());
            assert!(attempts > 0 && attempts <= if equal { 1 } else { 3 }, "CT54.distinct candidate attempts");
            complete(root.path());
            assert!(fs::read_dir(root.path().join("secrets")).expect("secrets listing").count() == 1, "CT54.candidates removed after proof");
        }
    } } }
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
fn ct058_admitted_open() {
    let _controls = controls(); credential_migration::seam::with(|s| { s.fake_pre_upgrade = false; s.fake_generation = false; });
    let (root, values) = credential_migration::g24_m2_m::seed_admitted_catalogue();
    let count = ack_at_boundaries(root.path());
    credential_migration::seam::with(|s| s.free_bytes = Some((0, 0)));
    let vault = open(root.path()); assert!(status(root.path()).purge_required, "CT58.purge deferred");
    assert!(has_residue(root.path(), &values), "CT58.negative control detects residue before purge");
    drop(vault); credential_migration::seam::with(|s| s.free_bytes = None);
    let vault = open(root.path()); complete(root.path());
    assert!(vault.load(OWNER).expect("Owner load").is_some() && vault.load(CredentialSlot::FleetApi).expect("Fleet load").is_some(), "CT58.both rows open");
    assert!(count.load(Ordering::SeqCst) >= 4 && !has_residue(root.path(), &values), "CT58.acknowledged writes and complete directory scan");
}

#[test]
fn ct087_admitted_open() {
    let _controls = controls(); credential_migration::seam::with(|s| { s.fake_pre_upgrade = false; s.fake_generation = false; });
    let (root, _) = credential_migration::g24_m2_m::seed_admitted_catalogue(); let vault = open(root.path()); complete(root.path());
    let before = { let state = vault.lock().expect("state"); let db = state.connection.test_connection();
        let counts: (i64, i64, i64) = db.query_row("SELECT (SELECT count(*) FROM credential_legacy_fences), (SELECT count(*) FROM credential_refresh_attempts), (SELECT count(DISTINCT attempt_id) FROM credential_refresh_attempts)", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).expect("consolidated evidence");
        assert!(counts.0 > 0 && counts.1 > 0 && counts.1 == counts.2, "CT87.unique attempts and retained identities"); counts
    };
    vault.clear_credential_pair(OWNER).expect("clear Owner"); vault.clear_credential_pair(CredentialSlot::FleetApi).expect("clear Fleet");
    let after: i64 = vault.lock().expect("state").connection.test_connection().query_row("SELECT count(*) FROM credential_legacy_fences", [], |r| r.get(0)).expect("retained fence count");
    assert!(after == before.0, "CT87.clear retains fences");
}

#[test]
fn ct090_admitted_open() {
    let _controls = controls(); let root = admitted_migration_fixture(false, None, false, 0);
    credential_migration::seam::with(|s| s.free_bytes = Some((0, 0)));
    let vault = open(root.path()); let state = status(root.path());
    assert!(state.purge_required && state.purge_needed_bytes.is_some() && state.completed_at_ms.is_none(), "CT90.cleanup_pending_status_source");
    assert!(pair(&vault).refresh.as_str() == "refresh-0001", "CT90.converted slot usable");
    drop(vault); credential_migration::seam::with(|s| s.free_bytes = None);
    let vault = open(root.path()); complete(root.path()); assert!(pair(&vault).refresh.as_str() == "refresh-0001", "CT90.recovered slot");
    assert!(credential_migration::seam::with(|s| s.events.iter().filter(|e| e.as_str() == "vacuum").count()) == 1, "CT90.one purge");
}

#[test]
fn ct061_admitted_open() {
    let _controls = controls(); credential_migration::seam::with(|s| { s.fake_pre_upgrade = false; s.fake_generation = false; });
    let root = crate::auth::credential_recovery::tests::g24_m2_recovery::seed_admitted_restored_export();
    let vault = open(root.path()); complete(root.path());
    assert!(vault.load(OWNER).expect("restored Owner load").is_some() && vault.load(CredentialSlot::FleetApi).expect("restored Fleet load").is_some(), "CT61.restored pairs open through vault");
    for name in ["pre-upgrade-owner-api.key", "pre-upgrade-fleet-api.key"] { assert!(!root.path().join("secrets").join(name).exists(), "CT61.restored candidates removed after proof"); }
    assert!(root.path().join("secrets").join("hub-cursor.key").exists(), "CT61.cursor retained");
}

#[test]
fn ct062_admitted_open() {
    let _controls = controls(); credential_migration::seam::with(|s| { s.fake_pre_upgrade = false; s.fake_generation = false; });
    let (root, values) = credential_migration::g24_m2_m::seed_admitted_catalogue();
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
fn ct091_admitted_open() {
    let _controls = controls(); credential_migration::seam::with(|state| { state.fake_pre_upgrade = false; state.fake_generation = false; });
    let (root, values) = credential_migration::g24_m2_m::seed_admitted_catalogue();
    let keeper = rusqlite::Connection::open(root.path().join("hub.sqlite")).expect("retained WAL connection");
    keeper.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true).expect("retain WAL frames");
    assert!(has_residue(root.path(), &values), "CT91 negative control detects predecessor bytes");
    credential_migration::seam::with(|state| state.boundary = Some(Box::new(|point| point != "after_purge_commit")));
    let prepared = open(root.path()); let retained = pair(&prepared); drop(prepared);
    credential_migration::seam::with(|state| state.boundary = None);
    assert!(!status(root.path()).purge_required && !status(root.path()).wal_scrubbed, "CT91 purge committed before the pinned scrub");
    keeper.execute_batch("CREATE TABLE v1_busy_ack(n INTEGER PRIMARY KEY)").expect("busy acknowledgement table");
    let reader = rusqlite::Connection::open(root.path().join("hub.sqlite")).expect("reader");
    reader.execute_batch("BEGIN; SELECT * FROM credential_format_state;").expect("pinned snapshot");
    keeper.execute("INSERT INTO v1_busy_ack DEFAULT VALUES", []).expect("acknowledged write after reader snapshot");
    let attempts = Arc::new(AtomicUsize::new(0)); let seen = Arc::clone(&attempts);
    fixtures::power_observer(Some(Box::new(move |kind, _| { if kind == "wal_scrub_attempt" { seen.fetch_add(1, Ordering::SeqCst); } })));
    let vault = open(root.path());
    fixtures::power_observer(None);
    assert!(status(root.path()).completed_at_ms.is_none() && !status(root.path()).wal_scrubbed, "CT91 busy scrub retains cleanup_pending_status_source");
    assert_eq!(attempts.load(Ordering::SeqCst), 5, "CT91 five busy scrub attempts");
    drop(vault); reader.execute_batch("ROLLBACK").expect("release reader"); drop(reader);
    keeper.execute_batch("CREATE TABLE v1_scrub_ack(n INTEGER PRIMARY KEY)").expect("scrub acknowledgement table");
    let zeroes = Arc::new(AtomicUsize::new(0)); let seen = Arc::clone(&zeroes); let path = root.path().join("hub.sqlite-wal");
    fixtures::power_observer(Some(Box::new(move |kind, _| { if kind == "wal_scrubbed" {
        assert!(fs::metadata(&path).expect("scrub WAL").len() == 0, "CT91 proved zero WAL before marker or acknowledgement");
        keeper.execute("INSERT INTO v1_scrub_ack DEFAULT VALUES", []).expect("acknowledged post-scrub write");
        seen.fetch_add(1, Ordering::SeqCst);
    } })));
    let count = ack_at_boundaries(root.path()); let vault = open(root.path()); complete(root.path());
    fixtures::power_observer(None);
    let after = pair(&vault);
    assert!(after.generation == retained.generation && after.access.as_str() == retained.access.as_str() && after.refresh.as_str() == retained.refresh.as_str(), "CT91 cleanup preserves the catalogue vector's current pair");
    assert!(count.load(Ordering::SeqCst) >= 4, "CT91 acknowledged writes at every remaining boundary");
    assert!(zeroes.load(Ordering::SeqCst) >= 1, "CT91 zero-length WAL observed before the marker");
    assert!(fs::metadata(root.path().join("hub.sqlite-wal")).expect("active WAL").len() > 0 && !has_residue(root.path(), &values), "CT91 markers and acknowledged WAL writes contain no predecessor bytes");
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
    let backup_root=crate::private_tempdir().expect("backup root");let backup=backup_root.path().join("backup");
    crate::data_recovery::create_data_backup(&HubStore::initialize(root.path()).expect("backup store"),&backup).expect("CT101.backup");
    let destination=crate::private_tempdir().expect("restore root");let restored=destination.path().join("restored");
    crate::data_recovery::restore_data_backup(&backup,&restored).expect("CT101.restore");drop(open(&restored));
    assert!(fingerprint()==before,"CT101.encoded files keep bytes inode and mtime through backup restore");
    assert!(credential_migration::seam::with(|s| s.events.iter().all(|event| !event.contains(&encoded) || event.starts_with("list_returned:"))), "CT101.no operation on encoded entry");
}
