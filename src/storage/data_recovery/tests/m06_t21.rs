// SPDX-License-Identifier: AGPL-3.0-only

// M06-T21: an encrypted backup states its three space needs (catalogue,
// scratch, destination) and stops before it exceeds a configured scratch
// budget, with its scratch in an approved folder. Synthetic data only, in
// temporary stores; no network and no collector run.

use std::{cell::RefCell, rc::Rc};

use super::*;
use crate::data_recovery::{
    EncryptedBackupRequest, EncryptedBackupSpaceNeeds,
    archive_crypto::KdfParams,
    backup_admission::{VolumeTrust, test_seam},
    create_encrypted_backup, encrypted_backup_space_needs,
    encrypted_backup::{
        HookAction, clear_hook, force_default_scratch, force_scratch_above, scratch_need,
        set_hook,
    },
    restore_encrypted_backup, verify_encrypted_backup,
};
use crate::runtime::config::BackupConfig;

const PASSWORD: &str = "correct horse battery staple";
const TEST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    passes: 1,
    lanes: 1,
};
const MIB: u64 = 1024 * 1024;

struct Hub {
    temporary: tempfile::TempDir,
    store: HubStore,
    destination: PathBuf,
    scratch: PathBuf,
}

fn private_folder(path: &Path) {
    fs::create_dir(path).expect("folder");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("private");
}

fn hub() -> Hub {
    clear_hook();
    test_seam::force(None);
    let (temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    let destination = temporary.path().join("destination");
    private_folder(&destination);
    let scratch = temporary.path().join("scratch");
    private_folder(&scratch);
    Hub {
        temporary,
        store,
        destination,
        scratch,
    }
}

impl Hub {
    fn request(&self) -> EncryptedBackupRequest<'_> {
        let mut request = EncryptedBackupRequest::new(&self.destination, PASSWORD);
        request.kdf = TEST_KDF;
        request.chunk_bytes = 4096;
        request.scratch_directory = Some(&self.scratch);
        request
    }

    fn needs(&self) -> EncryptedBackupSpaceNeeds {
        encrypted_backup_space_needs(&self.store, &self.request()).expect("space needs")
    }

    fn destination_entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.destination)
            .expect("destination")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn scratch_entries(&self) -> usize {
        fs::read_dir(&self.scratch).expect("scratch folder").count()
    }

    fn pin_rows(&self) -> i64 {
        self.store
            .open()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM backup_pack_pins", [], |row| row.get(0))
            .expect("pin rows")
    }
}

/// The 2.6 x catalogue + 277 MiB rule of the design note, at the guest's
/// 2.28 GB catalogue: about 6.3 GB of scratch, packs not included.
#[test]
fn backup_scratch_need_is_about_2_6_times_the_catalogue_plus_277_mib() {
    clear_hook();
    assert_eq!(scratch_need(1024).unwrap(), 0, "the in-memory path needs no scratch");
    let catalogue = 2_280_000_000_u64;
    let need = scratch_need(catalogue).unwrap();
    let copy = catalogue + catalogue / 4;
    let volume = 2 * copy + 256 * MIB;
    assert_eq!(need, volume + volume.div_ceil(20) + 8 * MIB);
    assert!((6_270_000_000..6_280_000_000).contains(&need), "{need}");
    let rule = catalogue * 2625 / 1000 + 277 * MIB;
    assert!(need.abs_diff(rule) < MIB, "{need} against {rule}");
}

/// The needs are shown separately before a run: the catalogue, the scratch
/// (none on the in-memory path, the catalogue's alone on the scratch path),
/// and the destination, which holds the catalogue and every pack.
#[test]
fn backup_space_needs_name_catalogue_scratch_and_destination_separately() {
    let hub = hub();
    let packs: u64 = fs::read_dir(hub.store.packs_dir().join("sha256"))
        .expect("packs")
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(packs > 0, "the fixture has packs");
    let memory = hub.needs();
    assert!(memory.catalogue_bytes > 0);
    assert_eq!(memory.scratch_bytes, 0);
    assert_eq!(memory.scratch_directory, hub.scratch.canonicalize().unwrap());
    assert_eq!(memory.destination, hub.destination.canonicalize().unwrap());
    assert!(memory.destination_bytes >= memory.catalogue_bytes + packs);
    assert_eq!(memory.scratch_budget_bytes, None);
    assert!(memory.within_scratch_budget());

    force_scratch_above(Some(0));
    let mut request = hub.request();
    request.scratch_budget_bytes = Some(1);
    let scratch = encrypted_backup_space_needs(&hub.store, &request).expect("needs");
    clear_hook();
    assert_eq!(scratch.catalogue_bytes, memory.catalogue_bytes);
    assert_eq!(scratch.destination_bytes, memory.destination_bytes);
    let volume = 2 * (scratch.catalogue_bytes + scratch.catalogue_bytes / 4) + 256 * MIB;
    assert_eq!(scratch.scratch_bytes, volume + volume.div_ceil(20) + 8 * MIB);
    assert_eq!(scratch.scratch_budget_bytes, Some(1));
    assert!(!scratch.within_scratch_budget());
    let json = serde_json::to_value(&scratch).unwrap();
    for key in ["catalogueBytes", "scratchBytes", "scratchDirectory", "destinationBytes"] {
        assert!(json.get(key).is_some(), "{key} in {json}");
    }
    // Measuring writes nothing.
    assert!(hub.destination_entries().is_empty());
    assert_eq!(hub.scratch_entries(), 0);
}

/// A run whose scratch need exceeds the budget is refused before any scratch
/// is attached or anything copied; the last good archive stays.
#[test]
fn backup_scratch_budget_below_the_need_refuses_before_attaching_or_copying() {
    let hub = hub();
    let first = create_encrypted_backup(&hub.store, &hub.request()).expect("first backup");
    let before = hub.destination_entries();
    force_scratch_above(Some(0));
    let need = hub.needs().scratch_bytes;
    let stages = Rc::new(RefCell::new(Vec::<String>::new()));
    let seen = stages.clone();
    set_hook(move |stage, _| {
        seen.borrow_mut().push(stage.to_owned());
        HookAction::Continue
    });
    let mut request = hub.request();
    request.scratch_budget_bytes = Some(need - 1);
    let error = create_encrypted_backup(&hub.store, &request).expect_err("over budget");
    clear_hook();
    match &error {
        DataRecoveryError::ScratchBudgetExceeded {
            required, budget, ..
        } => {
            assert_eq!((*required, *budget), (need, need - 1));
        }
        other => panic!("unexpected refusal: {other}"),
    }
    assert!(error.to_string().starts_with("scratch_budget_exceeded:"));
    assert!(stages.borrow().is_empty(), "nothing ran: {:?}", stages.borrow());
    assert_eq!(hub.scratch_entries(), 0, "no scratch was created");
    assert_eq!(hub.destination_entries(), before);
    assert_eq!(hub.pin_rows(), 0);
    verify_encrypted_backup(&first.path, PASSWORD).expect("the last good archive stays");
}

/// A catalogue that grows past the budget between the measurement and the
/// gate is refused under the gate, before any scratch is attached or anything
/// copied (REV-CX-B3-STORAGE F4).
#[cfg(target_os = "macos")]
#[test]
fn backup_catalogue_growing_past_the_budget_under_the_gate_refuses_before_copying() {
    let hub = hub();
    force_scratch_above(Some(0));
    let need = hub.needs().scratch_bytes;
    let stages = Rc::new(RefCell::new(Vec::<String>::new()));
    let seen = stages.clone();
    let store = hub.store.clone();
    set_hook(move |stage, _| {
        seen.borrow_mut().push(stage.to_owned());
        if stage == "space_measured" {
            for index in 0..400 {
                store
                    .register_source(
                        &SourceDescriptor::new("tesla_owner_api", format!("m06-t21-grow-{index}")),
                        1_000,
                    )
                    .expect("growth");
            }
        }
        HookAction::Continue
    });
    let mut request = hub.request();
    request.scratch_budget_bytes = Some(need);
    let error = create_encrypted_backup(&hub.store, &request).expect_err("grew past budget");
    clear_hook();
    assert!(
        matches!(error, DataRecoveryError::ScratchBudgetExceeded { budget, required, .. } if budget == need && required > need),
        "{error}"
    );
    assert_eq!(stages.borrow().as_slice(), ["space_measured"], "nothing attached or copied");
    assert_eq!(hub.scratch_entries(), 0, "no scratch was created");
    assert!(hub.destination_entries().is_empty());
    assert_eq!(hub.pin_rows(), 0);
}

/// The default scratch location (the temporary folder) passes the same
/// admission as a configured one: inside the live data or the destination,
/// or reached through a link into either, it is refused before any scratch
/// is created there (REV-CX-B3-STORAGE F2).
#[cfg(target_os = "macos")]
#[test]
fn backup_default_scratch_inside_data_or_destination_is_refused_before_creating_scratch() {
    let hub = hub();
    let data = hub.store.database_path().parent().unwrap().to_path_buf();
    let inside_data = data.join("m06-t21-temporary");
    private_folder(&inside_data);
    let inside_destination = hub.destination.join("temporary");
    private_folder(&inside_destination);
    let link = hub.temporary.path().join("temporary-link");
    std::os::unix::fs::symlink(&inside_data, &link).unwrap();
    for (folder, refusal) in [
        (&inside_data, "live Hub data directory"),
        (&inside_destination, "inside the backup destination"),
        (&link, "live Hub data directory"),
    ] {
        force_scratch_above(Some(0));
        force_default_scratch(Some(folder.clone()));
        let mut request = hub.request();
        request.scratch_directory = None;
        let error = create_encrypted_backup(&hub.store, &request).expect_err("refused");
        clear_hook();
        assert!(error.to_string().contains(refusal), "{}: {error}", folder.display());
        assert_eq!(fs::read_dir(folder).unwrap().count(), 0, "no scratch in {}", folder.display());
    }
    assert_eq!(hub.pin_rows(), 0);
}

/// The admitted scratch folder is held: replaced by a link into the live
/// data after admission, the run still creates, uses and removes its scratch
/// in the folder it admitted, never through the link (REV-CX-B3-STORAGE F3).
#[cfg(target_os = "macos")]
#[test]
fn backup_scratch_folder_replaced_after_admission_is_not_followed() {
    let hub = hub();
    let data = hub.store.database_path().parent().unwrap().to_path_buf();
    let forbidden = data.join("m06-t21-forbidden");
    private_folder(&forbidden);
    let moved = hub.temporary.path().join("scratch-moved");
    force_scratch_above(Some(0));
    let (scratch, moved_seen, forbidden_seen) =
        (hub.scratch.clone(), moved.clone(), forbidden.clone());
    let counts = Rc::new(RefCell::new(Vec::<(String, usize, usize)>::new()));
    let seen = counts.clone();
    set_hook(move |stage, _| {
        if stage == "space_measured" {
            fs::rename(&scratch, &moved_seen).expect("move the admitted folder");
            std::os::unix::fs::symlink(&forbidden_seen, &scratch).expect("plant a link");
        }
        if matches!(stage, "scratch_attached" | "scratch_copied") {
            seen.borrow_mut().push((
                stage.to_owned(),
                fs::read_dir(&moved_seen).unwrap().count(),
                fs::read_dir(&forbidden_seen).unwrap().count(),
            ));
        }
        HookAction::Continue
    });
    let result = create_encrypted_backup(&hub.store, &hub.request());
    clear_hook();
    result.expect("the run uses the folder it admitted");
    assert_eq!(counts.borrow().as_slice(), [
        ("scratch_attached".to_owned(), 1, 0),
        ("scratch_copied".to_owned(), 1, 0),
    ]);
    assert_eq!(fs::read_dir(&forbidden).unwrap().count(), 0, "nothing in the live data");
    assert_eq!(fs::read_dir(&moved).unwrap().count(), 0, "the scratch is removed");
}

/// Without a budget, a catalogue that outgrows the room its scratch volume
/// was prepared with is refused under the gate instead of filling the volume.
#[cfg(target_os = "macos")]
#[test]
fn backup_catalogue_outgrowing_its_scratch_room_under_the_gate_stops_before_copying() {
    let hub = hub();
    force_scratch_above(Some(0));
    let catalogue = hub.needs().catalogue_bytes;
    let store = hub.store.clone();
    let mut grown = false;
    set_hook(move |stage, _| {
        if stage == "space_measured" && !grown {
            grown = true;
            let mut index = 0;
            while live_bytes(&store) <= catalogue + catalogue / 4 {
                store
                    .register_source(
                        &SourceDescriptor::new(
                            "tesla_owner_api",
                            format!("m06-t21-room-{index}-{}", "y".repeat(200)),
                        ),
                        1_000,
                    )
                    .expect("growth");
                index += 1;
            }
        }
        HookAction::Continue
    });
    let error = create_encrypted_backup(&hub.store, &hub.request()).expect_err("outgrew the room");
    clear_hook();
    assert!(error.to_string().contains("grew past"), "{error}");
    assert_eq!(hub.scratch_entries(), 0);
    assert!(hub.destination_entries().is_empty());
    assert_eq!(hub.pin_rows(), 0);
}

#[cfg(target_os = "macos")]
fn live_bytes(store: &HubStore) -> u64 {
    let connection = store.open().unwrap();
    let pages: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0)).unwrap();
    let size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0)).unwrap();
    u64::try_from(pages * size).unwrap()
}

/// Within the budget the run uses the approved folder, reports the needs it
/// measured, leaves no scratch behind and restores on a clean host with every
/// dependency; an interrupted run there keeps the last good archive.
#[cfg(target_os = "macos")]
#[test]
fn backup_within_the_scratch_budget_uses_the_approved_folder_and_restores() {
    let hub = hub();
    force_scratch_above(Some(0));
    let needs = hub.needs();
    let scratch_root = needs.scratch_directory.clone();
    let images = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
    let seen = images.clone();
    set_hook(move |stage, path| {
        if stage == "scratch_copied" {
            seen.borrow_mut().push(path.to_path_buf());
        }
        HookAction::Continue
    });
    let mut request = hub.request();
    request.scratch_budget_bytes = Some(needs.scratch_bytes);
    let report = create_encrypted_backup(&hub.store, &request).expect("backup within budget");
    assert_eq!(report.space_needs, EncryptedBackupSpaceNeeds {
        scratch_budget_bytes: Some(needs.scratch_bytes),
        ..needs.clone()
    });
    assert_eq!(images.borrow().len(), 1);
    assert!(images.borrow()[0].starts_with(&scratch_root), "{:?}", images.borrow());
    assert_eq!(hub.scratch_entries(), 0, "the scratch is removed after the run");

    // An interrupted run on the same path keeps the last good archive.
    let before = hub.destination_entries();
    set_hook(|stage, _| {
        if stage == "write" {
            HookAction::Fail(std::io::ErrorKind::Other)
        } else {
            HookAction::Continue
        }
    });
    // The first run's records may have grown the catalogue: measure again.
    request.scratch_budget_bytes = Some(hub.needs().scratch_bytes);
    let error = create_encrypted_backup(&hub.store, &request).expect_err("interrupted");
    clear_hook();
    assert!(error.to_string().contains("injected"), "{error}");
    let after = hub.destination_entries();
    let archives = |names: &[String]| names.iter().filter(|name| name.ends_with(".tlbk")).cloned().collect::<Vec<_>>();
    assert_eq!(archives(&after), archives(&before), "no new generation, the last good one kept");
    assert_eq!(hub.scratch_entries(), 0);
    verify_encrypted_backup(&report.path, PASSWORD).expect("the last good archive stays");

    let restored = hub.temporary.path().join("restored");
    restore_encrypted_backup(&report.path, PASSWORD, &restored).expect("restore on a clean host");
    let restored_packs = fs::read_dir(restored.join("packs/sha256")).expect("packs").count();
    let live_packs = fs::read_dir(hub.store.packs_dir().join("sha256")).expect("packs").count();
    assert_eq!(restored_packs, live_packs, "every pack is restored");
    assert_eq!(hub.pin_rows(), 0);
}

/// The scratch folder must be approved: private to the Hub's user, outside
/// the live data and the destination, and not a link.
#[test]
fn backup_scratch_folder_must_be_private_and_outside_data_and_destination() {
    let hub = hub();
    // As on a volume that enforces ownership, wherever the test runs.
    test_seam::force(Some(VolumeTrust::Posix));
    let refused = |folder: &Path| {
        let mut request = hub.request();
        request.scratch_directory = Some(folder);
        encrypted_backup_space_needs(&hub.store, &request)
            .expect_err("refused")
            .to_string()
    };
    let open = hub.temporary.path().join("open");
    fs::create_dir(&open).unwrap();
    fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(refused(&open).contains("private folder"), "{}", refused(&open));
    let inside_destination = hub.destination.join("scratch");
    private_folder(&inside_destination);
    assert!(refused(&inside_destination).contains("inside the backup destination"));
    assert!(refused(&hub.destination).contains("inside the backup destination"));
    let data = hub.store.database_path().parent().unwrap().to_path_buf();
    let inside_data = data.join("m06-t21-scratch");
    private_folder(&inside_data);
    assert!(refused(&inside_data).contains("live Hub data directory"));
    let link = hub.temporary.path().join("link");
    std::os::unix::fs::symlink(&hub.scratch, &link).unwrap();
    assert!(refused(&link).contains("scratch folder"));
    let missing = hub.temporary.path().join("missing");
    assert!(encrypted_backup_space_needs(&hub.store, &{
        let mut request = hub.request();
        request.scratch_directory = Some(&missing);
        request
    })
    .is_err());
    test_seam::force(None);
}

/// The configuration names the budget and the folder; both are checked.
#[test]
fn backup_scratch_configuration_parses_and_is_validated() {
    let data = Path::new("/srv/hub/data");
    let config: BackupConfig = toml::from_str(
        "scratch_budget_bytes = 8000000000\nscratch_directory = \"/Volumes/Scratch/hub\"\n",
    )
    .expect("parses");
    assert_eq!(config.scratch_budget_bytes, Some(8_000_000_000));
    assert_eq!(config.scratch_directory.as_deref(), Some(Path::new("/Volumes/Scratch/hub")));
    config.validate_on(data, true).expect("valid");
    let relative: BackupConfig = toml::from_str("scratch_directory = \"scratch\"\n").unwrap();
    assert!(relative.validate_on(data, true).is_err());
    let inside: BackupConfig =
        toml::from_str("scratch_directory = \"/srv/hub/data/scratch\"\n").unwrap();
    assert!(inside.validate_on(data, true).is_err());
    let zero: BackupConfig = toml::from_str("scratch_budget_bytes = 0\n").unwrap();
    assert!(zero.validate_on(data, true).is_err());
    assert_eq!(BackupConfig::default().scratch_budget_bytes, None);
}
