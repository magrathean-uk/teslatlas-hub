// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::auth::credential_envelope::{EnvelopeKeys, derive_envelope_key};
use crate::platform::private_directory::{
    ENTRY_SEAM, EntrySeam, EntryState, PrivateDirectoryError, UnsafeEntry, open_validated,
};
use rustix::{fs::FileType, io::Errno};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs,
    os::unix::{
        fs::{MetadataExt, PermissionsExt, symlink},
        net::UnixListener,
    },
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

const T0: u64 = 1_800_000_000;
const STABLE: &str = "48434b52010000000102030405060708000000006b49d200000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f85cca13591d55d181324731ad2526208da7ede7dd039c446ef79e2a3fd291813";
const ROTATING: &str = "48434b52010100002122232425262728000000006b4b2380404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f0102030405060708000000006b49d200000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f845d9f0b007176f7da9b415aa675d474d02d8210ddca8495c32b60e7b2dfc508";

fn material(second: bool) -> KeyMaterial {
    KeyMaterial {
        id: KeyId(if second {
            [0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28]
        } else {
            [1, 2, 3, 4, 5, 6, 7, 8]
        }),
        created_at_s: T0 + if second { 86_400 } else { 0 },
        key: Zeroizing::new(std::array::from_fn(|i| {
            i as u8 + if second { 0x40 } else { 0 }
        })),
    }
}

fn fixture(rotating: bool) -> Keyring {
    Keyring {
        current: material(rotating),
        previous: rotating.then(|| material(false)),
    }
}

fn assert_material(actual: &KeyMaterial, expected: &KeyMaterial) {
    assert_eq!(actual.id, expected.id);
    assert_eq!(actual.created_at_s, expected.created_at_s);
    assert_eq!(actual.key.as_ref(), expected.key.as_ref());
}

fn assert_ring(actual: &Keyring, expected: &Keyring) {
    assert_material(&actual.current, &expected.current);
    match (&actual.previous, &expected.previous) {
        (Some(actual), Some(expected)) => assert_material(actual, expected),
        (None, None) => {}
        _ => panic!("previous key presence differs"),
    }
}

fn temp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("c10-k-")
        .tempdir_in(std::env::temp_dir())
        .expect("per-test system temporary directory")
}

fn secrets(root: &tempfile::TempDir) -> PrivateDirectory {
    open_validated(&root.path().join("secrets"), true).expect("test secrets directory")
}

fn write_stable(directory: &PrivateDirectory) -> PathBuf {
    let path = directory.path().join(KEYRING_NAME);
    fs::write(&path, hex::decode(STABLE).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

fn listing(directory: &PrivateDirectory) -> Vec<String> {
    let mut entries = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

struct Excluded;
impl BackupExclusion for Excluded {
    fn ensure_excluded(&self, _path: &Path) -> Result<ExclusionState, CredentialKeyError> {
        Ok(ExclusionState::Verified)
    }
}

struct ExclusionFailed;
impl BackupExclusion for ExclusionFailed {
    fn ensure_excluded(&self, _path: &Path) -> Result<ExclusionState, CredentialKeyError> {
        Err(CredentialKeyError::BackupExclusionFailed)
    }
}

fn checksum(bytes: &mut [u8]) {
    let length = bytes.len();
    let digest = Sha256::digest(&bytes[..length - 32]);
    bytes[length - 32..].copy_from_slice(&digest);
}

fn fifo(path: &Path) {
    let status = std::process::Command::new("/usr/bin/mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "create FIFO inside test directory");
}

#[test]
fn ct011_keyring_encoding() {
    for (rotating, expected, length) in [(false, STABLE, 88), (true, ROTATING, 136)] {
        let ring = fixture(rotating);
        let bytes = encode_keyring(&ring);
        assert_eq!(bytes.len(), length);
        assert_eq!(hex::encode(&*bytes), expected);
        assert_ring(&decode_keyring(&bytes).unwrap(), &ring);
        assert_eq!(&bytes[..4], b"HCKR");
        assert_eq!(bytes[4], 1);
        assert_eq!(bytes[5], u8::from(rotating));
        assert_eq!(&bytes[6..8], &[0, 0]);
    }
    assert_eq!(KEYRING_MAGIC, *b"HCKR");
    assert_eq!(KEYRING_VERSION, 1);
    assert_eq!(KEYRING_BYTES, 88);
    assert_eq!(ROTATING_KEYRING_BYTES, 136);
    assert_eq!(FORCED_ROTATION_INVOCATIONS, 1 << 31);
    let handle = KeyringHandle::new(fixture(true));
    for second in [false, true] {
        let key = material(second);
        assert_eq!(
            handle.envelope_key(key.id).unwrap().as_ref(),
            derive_envelope_key(&key.key).as_ref()
        );
    }
    assert!(handle.envelope_key(KeyId([0xff; 8])).is_none());
}

#[test]
fn ct012_keyring_rejections() {
    for length in [87, 89, 135, 137] {
        let mut bytes = hex::decode(if length < 100 { STABLE } else { ROTATING }).unwrap();
        bytes.resize(length, 0);
        assert!(
            matches!(
                decode_keyring(&bytes),
                Err(CredentialKeyError::KeyringInvalid)
            ),
            "length {length}"
        );
    }
    for (offset, value, fix_checksum, expected) in [
        (87, 0, false, CredentialKeyError::KeyringInvalid),
        (3, b'S', true, CredentialKeyError::KeyringInvalid),
        (4, 2, true, CredentialKeyError::KeyringUnsupportedVersion(2)),
        (5, 2, true, CredentialKeyError::KeyringInvalid),
        (7, 1, true, CredentialKeyError::KeyringInvalid),
    ] {
        let mut bytes = hex::decode(STABLE).unwrap();
        bytes[offset] = value;
        if fix_checksum {
            checksum(&mut bytes);
        }
        assert!(
            matches!(decode_keyring(&bytes), Err(error) if error == expected),
            "rejection at offset {offset}"
        );
    }
    let mut duplicate = hex::decode(ROTATING).unwrap();
    let current_id = duplicate[8..16].to_vec();
    duplicate[56..64].copy_from_slice(&current_id);
    checksum(&mut duplicate);
    assert!(matches!(
        decode_keyring(&duplicate),
        Err(CredentialKeyError::KeyringInvalid)
    ));
    for (hex_value, flag) in [(STABLE, 1), (ROTATING, 0)] {
        let mut bytes = hex::decode(hex_value).unwrap();
        bytes[5] = flag;
        checksum(&mut bytes);
        assert!(matches!(
            decode_keyring(&bytes),
            Err(CredentialKeyError::KeyringInvalid)
        ));
    }
}

#[test]
fn ct013_publication_order() {
    let first_root = temp();
    let first = secrets(&first_root);
    assert!(!first.path().join(KEYRING_NAME).exists());
    assert!(load_keyring(&first).unwrap().is_none());
    let handle = with_key_faults(KeyFaults::default(), || {
        create_keyring(&first, &Excluded, T0)
    })
    .unwrap();
    let events = take_key_events();
    let position = |name| {
        events
            .iter()
            .position(|event| *event == name)
            .expect("publication event")
    };
    assert!(
        position("exclusion") < position("create"),
        "exclusion_before_first_byte"
    );
    assert!(position("create") < position("write"));
    assert!(position("write") < position("file_barrier"));
    assert!(
        position("file_barrier") < position("rename"),
        "file_barrier_before_rename"
    );
    assert!(position("rename") < position("directory_barrier"));
    assert!(
        position("directory_barrier") < position("published"),
        "directory_barrier_before_publication"
    );
    let metadata = fs::symlink_metadata(first.path().join(KEYRING_NAME)).unwrap();
    assert_eq!(metadata.len(), 88);
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
    assert_eq!(metadata.nlink(), 1);
    assert_eq!(
        fs::metadata(first.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(fs::read(first.path().join(KEYRING_NAME)).unwrap()[5], 0);
    assert_eq!(listing(&first), [KEYRING_NAME]);
    assert_eq!(handle.keyring().current.created_at_s, T0);
    assert!(handle.keyring().previous.is_none());
    let second_root = temp();
    let second = secrets(&second_root);
    let second_handle = create_keyring(&second, &Excluded, T0).unwrap();
    assert_ne!(
        handle.keyring().current.id,
        second_handle.keyring().current.id
    );
    assert_ne!(
        handle.keyring().current.key.as_ref(),
        second_handle.keyring().current.key.as_ref()
    );

    for fail_file in [true, false] {
        let root = temp();
        let directory = secrets(&root);
        let result = with_key_faults(
            KeyFaults {
                file_barrier_fail: fail_file,
                directory_barrier_fail: !fail_file,
                ..Default::default()
            },
            || create_keyring(&directory, &Excluded, T0),
        );
        assert!(matches!(
            result,
            Err(CredentialKeyError::DurabilityUnavailable)
        ));
        let events = take_key_events();
        assert!(
            !events.contains(&"published"),
            "failed_barrier_never_publishes"
        );
        assert_eq!(
            listing(&directory),
            if fail_file {
                vec![]
            } else {
                vec![KEYRING_NAME.to_string()]
            }
        );
        if fail_file {
            assert!(!events.contains(&"rename"));
        } else {
            assert!(load_keyring(&directory).unwrap().is_some());
        }
    }
    let root = temp();
    let directory = secrets(&root);
    assert!(matches!(
        with_key_faults(
            KeyFaults {
                entropy_fail: true,
                ..Default::default()
            },
            || create_keyring(&directory, &Excluded, T0)
        ),
        Err(CredentialKeyError::EntropyUnavailable)
    ));
    assert!(listing(&directory).is_empty());
    fix_cleanup_errors();
}

#[test]
fn ct014_custody_faults_helper() {
    let mut fix_failures = Vec::new();
    let root = temp();
    let directory = secrets(&root);
    let path = write_stable(&directory);
    let clean = load_keyring(&directory).unwrap().unwrap();
    fix_expect(
        &mut fix_failures,
        "fx_clean_exposure_ct014",
        !clean.exposure_suspected,
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let handle = load_keyring(&directory).unwrap().unwrap();
    assert!(handle.exposure_suspected);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_ring(handle.keyring(), &fixture(false));

    fs::remove_file(&path).unwrap();
    let target = root.path().join("valid-keyring");
    fs::write(&target, hex::decode(STABLE).unwrap()).unwrap();
    symlink(&target, &path).unwrap();
    assert!(matches!(
        load_keyring(&directory),
        Err(CredentialKeyError::KeyringUnsafe(UnsafeReason::Symlink))
    ));
    assert_eq!(fs::read(&target).unwrap(), hex::decode(STABLE).unwrap());
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(listing(&directory), [KEYRING_NAME]);

    fs::remove_file(&path).unwrap();
    write_stable(&directory);
    fs::hard_link(&path, root.path().join("second-link")).unwrap();
    assert!(matches!(
        load_keyring(&directory),
        Err(CredentialKeyError::KeyringUnsafe(UnsafeReason::LinkCount))
    ));
    fs::remove_file(root.path().join("second-link")).unwrap();
    assert!(matches!(
        with_key_faults(
            KeyFaults {
                owner: Some(rustix::process::geteuid().as_raw().wrapping_add(1)),
                ..Default::default()
            },
            || load_keyring(&directory)
        ),
        Err(CredentialKeyError::KeyringUnsafe(UnsafeReason::Owner))
    ));
    assert_eq!(fs::read(&path).unwrap(), hex::decode(STABLE).unwrap());

    #[cfg(target_os = "macos")]
    {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        let status = std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow read"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "ACL fixture remains inside test directory"
        );
        let handle = load_keyring(&directory).unwrap().unwrap();
        assert!(handle.exposure_suspected, "extended_acl_sets_exposure");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let descriptor = match directory.inspect_entry(KEYRING_NAME).unwrap() {
            EntryState::Regular(fd) => fd,
            _ => panic!("regular ACL fixture"),
        };
        crate::platform::private_directory::verify_no_extended_acl(&descriptor, &path).unwrap();
    }

    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let reopened = open_validated(directory.path(), false).unwrap();
    assert_eq!(
        fs::metadata(reopened.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    fs::remove_file(&path).unwrap();
    fifo(&path);
    let start = Instant::now();
    assert!(matches!(
        load_keyring(&directory),
        Err(CredentialKeyError::KeyringUnsafe(UnsafeReason::NotRegular))
    ));
    assert!(start.elapsed() < Duration::from_secs(2));
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(matches!(
        load_keyring(&directory),
        Err(CredentialKeyError::KeyringUnsafe(UnsafeReason::NotRegular))
    ));
    fix_rotation_and_acl(&mut fix_failures);
    finish_fix_assertions(fix_failures);
}

#[test]
fn ct016_unreadable() {
    let root = temp();
    let directory = secrets(&root);
    let path = write_stable(&directory);
    for errno in [Errno::ACCESS, Errno::IO] {
        for fail_open in [true, false] {
            let seam = if fail_open {
                EntrySeam {
                    open_error: Some(errno),
                    ..Default::default()
                }
            } else {
                EntrySeam {
                    stat_error: Some(errno),
                    ..Default::default()
                }
            };
            ENTRY_SEAM.with(|slot| assert!(slot.replace(Some(seam)).is_none()));
            let mutations_before = directory.mutation_attempts();
            let result = with_key_faults(KeyFaults::default(), || load_keyring(&directory));
            let seam = ENTRY_SEAM.with(|slot| slot.borrow_mut().take().unwrap());
            assert!(
                matches!(result, Err(CredentialKeyError::KeyringUnreadable)),
                "entry_inspection_errno_maps_to_unreadable"
            );
            assert_eq!(seam.opens, usize::from(fail_open));
            assert_eq!(seam.stats, if fail_open { 2 } else { 1 });
            assert_eq!(directory.mutation_attempts(), mutations_before);
            assert!(!take_key_events().contains(&"read"));
            assert_eq!(listing(&directory), [KEYRING_NAME]);
            assert_eq!(fs::read(&path).unwrap(), hex::decode(STABLE).unwrap());
        }
    }
}

#[test]
fn ct017_keyring_temporary_cleanup() {
    let root = temp();
    let directory = secrets(&root);
    let path = write_stable(&directory);
    let temporary = ".credential-keys-00000000-0000-4000-8000-000000000001.tmp";
    fs::write(
        directory.path().join(temporary),
        b"interrupted-keyring-write",
    )
    .unwrap();
    fs::write(directory.path().join("unrelated"), b"retain").unwrap();
    let handle = load_keyring(&directory).unwrap().unwrap();
    assert!(!directory.path().join(temporary).exists());
    assert_eq!(fs::read(&path).unwrap(), hex::decode(STABLE).unwrap());
    assert_ring(handle.keyring(), &fixture(false));
    assert_eq!(
        fs::read(directory.path().join("unrelated")).unwrap(),
        b"retain"
    );
    assert_eq!(listing(&directory), [KEYRING_NAME, "unrelated"]);
    fix_status_checks(&handle);
}

type Runs = Rc<RefCell<Vec<(String, PathBuf, Duration)>>>;
struct RecordingTmutil {
    outputs: RefCell<VecDeque<Result<TmutilOutput, CredentialKeyError>>>,
    runs: Runs,
}
impl TmutilRunner for RecordingTmutil {
    fn run(
        &self,
        operation: &str,
        path: &Path,
        limit: Duration,
    ) -> Result<TmutilOutput, CredentialKeyError> {
        self.runs
            .borrow_mut()
            .push((operation.to_string(), path.to_path_buf(), limit));
        self.outputs
            .borrow_mut()
            .pop_front()
            .expect("unexpected tmutil operation")
    }
}
fn output(
    success: bool,
    stdout: &[u8],
    timed_out: bool,
) -> Result<TmutilOutput, CredentialKeyError> {
    Ok(TmutilOutput {
        success,
        stdout: stdout.to_vec(),
        timed_out,
    })
}
fn tmutil(
    outputs: Vec<Result<TmutilOutput, CredentialKeyError>>,
) -> (TmutilExclusion<RecordingTmutil>, Runs) {
    let runs = Rc::new(RefCell::new(vec![]));
    (
        TmutilExclusion(RecordingTmutil {
            outputs: RefCell::new(outputs.into()),
            runs: runs.clone(),
        }),
        runs,
    )
}

#[test]
fn ct018_exclusion_helper() {
    // Recording runners and scoped temporary executables exercise both paths.
    // The real tmutil executable is never selected in a test build.
    let root = temp();
    let directory = secrets(&root);
    let (exclusion, runs) = tmutil(vec![
        output(true, b"", false),
        output(true, b"[Excluded] test secrets\n", false),
    ]);
    assert_eq!(
        exclusion.ensure_excluded(directory.path()).unwrap(),
        ExclusionState::Verified
    );
    assert!(!directory.path().join(KEYRING_NAME).exists());
    assert_eq!(
        *runs.borrow(),
        vec![
            (
                "addexclusion".into(),
                directory.path().into(),
                Duration::from_secs(10)
            ),
            (
                "isexcluded".into(),
                directory.path().into(),
                Duration::from_secs(10)
            )
        ]
    );
    assert_eq!(TMUTIL_RUN_LIMIT, Duration::from_secs(10));

    for (answers, expected_calls) in [
        (vec![output(false, b"", false)], 1),
        (vec![output(true, b"", true)], 1),
        (
            vec![Err(CredentialKeyError::Io {
                operation: "test-spawn",
            })],
            1,
        ),
        (
            vec![
                output(true, b"", false),
                output(false, b"[Excluded] fixture", false),
            ],
            2,
        ),
        (
            vec![
                output(true, b"", false),
                output(true, b"[Included] fixture", false),
            ],
            2,
        ),
        (
            vec![
                output(true, b"", false),
                output(true, b"prefix [Excluded] fixture", false),
            ],
            2,
        ),
        (
            vec![
                output(true, b"", false),
                output(true, b"[Excluded] fixture", true),
            ],
            2,
        ),
    ] {
        let (exclusion, runs) = tmutil(answers);
        assert_eq!(
            exclusion.ensure_excluded(directory.path()),
            Err(CredentialKeyError::BackupExclusionFailed)
        );
        assert_eq!(runs.borrow().len(), expected_calls);
        assert!(
            runs.borrow()
                .iter()
                .all(|(_, _, limit)| *limit == Duration::from_secs(10))
        );
    }

    assert!(
        matches!(
            create_keyring(&directory, &ExclusionFailed, T0),
            Err(CredentialKeyError::BackupExclusionFailed)
        ),
        "failed_exclusion_refuses_creation"
    );
    assert!(
        listing(&directory).is_empty(),
        "failed_exclusion_creates_nothing"
    );
    write_stable(&directory);
    let handle = load_keyring(&directory).unwrap().unwrap();
    assert!(matches!(
        begin_rotation(&directory, &ExclusionFailed, &handle, T0 + 86_400),
        Err(CredentialKeyError::BackupExclusionFailed)
    ));
    assert_eq!(
        fs::read(directory.path().join(KEYRING_NAME)).unwrap(),
        hex::decode(STABLE).unwrap()
    );
    assert!(
        load_keyring(&directory).unwrap().is_some(),
        "existing_key_stays_readable"
    );
    assert_eq!(listing(&directory), [KEYRING_NAME]);
    fix_exclusion_and_runner();
}

#[test]
fn ct023_key_rendering() {
    let mut fix_failures = Vec::new();
    for rotating in [false, true] {
        let ring = fixture(rotating);
        let debug = format!("{ring:?}");
        let handle = KeyringHandle::new(ring);
        let rendering = format!("{debug} {handle:?}");
        assert!(rendering.contains(&T0.to_string()));
        if rotating {
            assert!(rendering.contains(&(T0 + 86_400).to_string()));
        }
        assert!(!rendering.contains(STABLE));
        assert!(!rendering.contains(ROTATING));
        for second in [false, true] {
            let key = material(second);
            let derived = derive_envelope_key(&key.key);
            for bytes in [key.key.as_ref(), derived.as_ref()] {
                assert!(
                    !rendering.contains(&hex::encode(bytes)),
                    "key_hex_never_rendered"
                );
                assert!(
                    !rendering.contains(&format!("{bytes:?}")),
                    "key_array_never_rendered"
                );
            }
            assert!(
                !rendering.contains(&hex::encode(key.id.0)),
                "id_bytes_never_rendered"
            );
            fix_expect(
                &mut fix_failures,
                "fx_debug_ids_opaque",
                !rendering.contains(&format!("{:?}", key.id.0)) && !rendering.contains("KeyId"),
            );
        }
    }
    finish_fix_assertions(fix_failures);
}

fn inspect_with(
    directory: &PrivateDirectory,
    name: &str,
    seam: EntrySeam,
) -> (Result<EntryState, PrivateDirectoryError>, EntrySeam) {
    ENTRY_SEAM.with(|slot| assert!(slot.replace(Some(seam)).is_none()));
    let result = directory.inspect_entry(name);
    let seam = ENTRY_SEAM.with(|slot| slot.borrow_mut().take().unwrap());
    (result, seam)
}

#[test]
fn ct093_entry_inspection() {
    let root = temp();
    let directory = secrets(&root);
    let (result, seam) = inspect_with(&directory, "missing", EntrySeam::default());
    assert!(matches!(result.unwrap(), EntryState::Absent));
    assert_eq!(seam.opens, 0);
    let path = write_stable(&directory);
    let (result, seam) = inspect_with(&directory, KEYRING_NAME, EntrySeam::default());
    assert!(matches!(result.unwrap(), EntryState::Regular(_)));
    assert_eq!(seam.opens, 1);

    for (name, target) in [
        ("link", path.clone()),
        ("dangling", directory.path().join("absent")),
    ] {
        symlink(target, directory.path().join(name)).unwrap();
        let (result, seam) = inspect_with(&directory, name, EntrySeam::default());
        assert!(matches!(
            result.unwrap(),
            EntryState::Unsafe(UnsafeEntry::Symlink)
        ));
        assert_eq!(seam.opens, 0, "symlink_never_opened");
    }
    fs::create_dir(directory.path().join("directory")).unwrap();
    fifo(&directory.path().join("fifo"));
    let _socket = UnixListener::bind(directory.path().join("socket")).unwrap();
    for name in ["directory", "fifo", "socket"] {
        let start = Instant::now();
        let (result, seam) = inspect_with(&directory, name, EntrySeam::default());
        assert!(matches!(
            result.unwrap(),
            EntryState::Unsafe(UnsafeEntry::NotRegular)
        ));
        assert_eq!(seam.opens, 0, "nonregular_never_opened");
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    for kind in [FileType::CharacterDevice, FileType::BlockDevice] {
        let (result, seam) = inspect_with(
            &directory,
            KEYRING_NAME,
            EntrySeam {
                stat_types: [kind].into(),
                ..Default::default()
            },
        );
        assert!(matches!(
            result.unwrap(),
            EntryState::Unsafe(UnsafeEntry::NotRegular)
        ));
        assert_eq!(seam.opens, 0, "device_never_opened");
    }
    let (result, seam) = inspect_with(&directory, "sub/file", EntrySeam::default());
    assert!(result.is_err());
    assert_eq!(seam.opens, 0);

    for replacement in ["socket", "symlink", "fifo", "removed"] {
        let race_path = directory.path().join("race");
        fs::write(&race_path, b"regular-before-stat").unwrap();
        let callback_path = race_path.clone();
        let target = path.clone();
        let socket_hold = Rc::new(RefCell::new(None));
        let callback_hold = socket_hold.clone();
        let (result, seam) = inspect_with(
            &directory,
            "race",
            EntrySeam {
                after_stat: Some(Box::new(move || {
                    fs::remove_file(&callback_path).unwrap();
                    match replacement {
                        "socket" => {
                            *callback_hold.borrow_mut() =
                                Some(UnixListener::bind(&callback_path).unwrap())
                        }
                        "symlink" => symlink(&target, &callback_path).unwrap(),
                        "fifo" => fifo(&callback_path),
                        "removed" => {}
                        _ => unreachable!(),
                    }
                })),
                ..Default::default()
            },
        );
        let state = result.unwrap();
        match replacement {
            "symlink" => assert!(matches!(state, EntryState::Unsafe(UnsafeEntry::Symlink))),
            "removed" => assert!(matches!(state, EntryState::Absent)),
            _ => assert!(matches!(state, EntryState::Unsafe(UnsafeEntry::NotRegular))),
        }
        assert_eq!(seam.opens, 1);
        if replacement == "fifo" {
            assert_eq!(seam.closed_nonregular, 1, "raced_fifo_descriptor_closed");
        }
        if replacement == "socket" {
            assert_eq!(seam.stats, 2, "failed_open_reclassified");
        }
        drop(socket_hold);
        if replacement != "removed" {
            fs::remove_file(race_path).unwrap();
        }
    }

    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let (result, seam) = inspect_with(
        &directory,
        KEYRING_NAME,
        EntrySeam {
            open_error: Some(Errno::ACCESS),
            ..Default::default()
        },
    );
    assert!(
        matches!(result, Err(PrivateDirectoryError::Io { source, .. }) if source.raw_os_error() == Some(Errno::ACCESS.raw_os_error()))
    );
    assert_eq!(seam.opens, 1);
    assert_eq!(seam.stats, 2);
    ENTRY_SEAM.with(|slot| {
        *slot.borrow_mut() = Some(EntrySeam {
            open_error: Some(Errno::ACCESS),
            ..Default::default()
        });
    });
    let load = with_key_faults(KeyFaults::default(), || load_keyring(&directory));
    let seam = ENTRY_SEAM.with(|slot| slot.borrow_mut().take().unwrap());
    let events = take_key_events();
    let mut failures = Vec::new();
    fix_expect(
        &mut failures,
        "fx_load_open_chain_unreadable",
        matches!(load, Err(CredentialKeyError::KeyringUnreadable))
            && seam.opens == 1
            && seam.stats == 2
            && !events.contains(&"read"),
    );
    finish_fix_assertions(failures);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(fs::read(&path).unwrap(), hex::decode(STABLE).unwrap());
}

// Collect independent contract failures so one regression cannot hide another
// assertion during failing-first proof. Fixture construction still fails fast.
fn fix_expect(failures: &mut Vec<&'static str>, name: &'static str, passed: bool) {
    if !passed {
        eprintln!("FIX_ASSERT_FAIL:{name}");
        failures.push(name);
    }
}

fn finish_fix_assertions(failures: Vec<&'static str>) {
    assert!(
        failures.is_empty(),
        "fix_round_assertions_failed:{failures:?}"
    );
}

fn fix_cleanup_errors() {
    let mut failures = Vec::new();
    for (kind, name, expected) in [
        (
            0,
            "fx_cleanup_barrier_error",
            CredentialKeyError::DurabilityUnavailable,
        ),
        (
            1,
            "fx_cleanup_write_error",
            CredentialKeyError::Io { operation: "write" },
        ),
        (
            2,
            "fx_cleanup_rename_error",
            CredentialKeyError::Io {
                operation: "rename",
            },
        ),
    ] {
        let root = temp();
        let directory = secrets(&root);
        let result = with_key_faults(
            KeyFaults {
                file_barrier_fail: kind == 0,
                write_errno: (kind == 1).then_some(Errno::NOSPC.raw_os_error()),
                rename_errno: (kind == 2).then_some(Errno::IO.raw_os_error()),
                cleanup_remove_fail: true,
                ..Default::default()
            },
            || create_keyring(&directory, &Excluded, T0),
        );
        let entries = listing(&directory);
        let leftover = entries.len() == 1
            && entries[0].starts_with(".credential-keys-")
            && entries[0].ends_with(".tmp");
        let recovered = load_keyring(&directory);
        fix_expect(
            &mut failures,
            name,
            matches!(result, Err(error) if error == expected)
                && leftover
                && matches!(recovered, Ok(None))
                && listing(&directory).is_empty(),
        );
    }
    finish_fix_assertions(failures);
}

#[cfg(target_os = "macos")]
fn add_read_acl(path: &Path) {
    let status = std::process::Command::new("/bin/chmod")
        .args(["+a", "everyone allow read"])
        .arg(path)
        .status()
        .unwrap();
    if !status.success() {
        panic!("test-owned ACL fixture unavailable");
    }
}

fn fix_rotation_and_acl(failures: &mut Vec<&'static str>) {
    let root = temp();
    let directory = secrets(&root);
    let path = write_stable(&directory);
    let current = KeyringHandle::new(fixture(false));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let mut rotating = begin_rotation(&directory, &Excluded, &current, T0 + 86_400).unwrap();
    fix_expect(
        failures,
        "fx_rotation_begin_exposure",
        rotating.exposure_suspected,
    );
    // The next reload must report its own exposure independently of this flag.
    rotating.exposure_suspected = false;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let finished = finish_rotation(&directory, &rotating).unwrap();
    fix_expect(
        failures,
        "fx_rotation_finish_exposure",
        finished.exposure_suspected,
    );

    write_stable(&directory);
    let stale = KeyringHandle::new(Keyring {
        current: material(true),
        previous: None,
    });
    let changed = begin_rotation(&directory, &Excluded, &stale, T0 + 86_400);
    fix_expect(
        failures,
        "fx_rotation_changed_is_not_invalid",
        matches!(
            changed,
            Err(CredentialKeyError::Io {
                operation: "keyring_changed"
            })
        ) && fs::read(&path).unwrap() == hex::decode(STABLE).unwrap(),
    );

    #[cfg(target_os = "macos")]
    {
        add_read_acl(&path);
        let result = with_key_faults(
            KeyFaults {
                acl_clear_fail: true,
                ..Default::default()
            },
            || load_keyring(&directory),
        );
        fix_expect(
            failures,
            "fx_file_acl_failure_is_file_io",
            matches!(
                result,
                Err(CredentialKeyError::Io {
                    operation: "clear_acl"
                })
            ) && fs::read(&path).unwrap() == hex::decode(STABLE).unwrap(),
        );
    }
}

fn fix_status_checks(clean: &KeyringHandle) {
    let mut failures = Vec::new();
    fix_expect(
        &mut failures,
        "fx_clean_exposure_ct017",
        !clean.exposure_suspected,
    );
    let mut metadata_exact = true;
    let mut unchanged = true;
    for mode_case in 0..if cfg!(target_os = "macos") { 3 } else { 2 } {
        let root = temp();
        let directory = secrets(&root);
        let path = write_stable(&directory);
        let temporary = directory
            .path()
            .join(".credential-keys-00000000-0000-4000-8000-000000000001.tmp");
        fs::write(&temporary, b"in-flight-private-temporary").unwrap();
        let mode = [0o600, 0o644, 0o400][mode_case];
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        #[cfg(target_os = "macos")]
        if mode_case == 2 {
            add_read_acl(&path);
        }
        let before = listing(&directory);
        let mutations = directory.mutation_attempts();
        let checked = with_key_faults(KeyFaults::default(), || check_keyring(&directory));
        let events = take_key_events();
        metadata_exact &= matches!(checked, Ok(Some(ref value))
            if value.current.id == KeyId([1,2,3,4,5,6,7,8])
            && value.current.created_at_s == T0 && value.previous.is_none()
            && value.exposure_suspected == (mode_case != 0));
        unchanged &= listing(&directory) == before
            && fs::read(&temporary).ok().as_deref()
                == Some(b"in-flight-private-temporary".as_slice())
            && fs::metadata(&path).unwrap().permissions().mode() & 0o777 == mode
            && fs::read(&path).unwrap() == hex::decode(STABLE).unwrap()
            && directory.mutation_attempts() == mutations
            && !events.iter().any(|event| {
                [
                    "chmod",
                    "clear_acl",
                    "file_barrier",
                    "directory_barrier",
                    "derive_handle",
                    "exclusion",
                    "cleanup_temporary",
                ]
                .contains(event)
            });
        #[cfg(target_os = "macos")]
        if mode_case == 2 {
            let fd = match directory.inspect_entry(KEYRING_NAME).unwrap() {
                EntryState::Regular(fd) => fd,
                _ => panic!("test keyring replaced"),
            };
            // The check must also leave the actual extended ACL present.
            unchanged &= matches!(
                crate::platform::private_directory::verify_no_extended_acl(&fd, &path),
                Err(PrivateDirectoryError::WritableByOthers(_))
            );
        }
    }
    fix_expect(&mut failures, "fx_status_check_metadata", metadata_exact);
    fix_expect(&mut failures, "fx_status_check_nonmutating", unchanged);
    finish_fix_assertions(failures);
}

struct NotApplicable;
impl BackupExclusion for NotApplicable {
    fn ensure_excluded(&self, _path: &Path) -> Result<ExclusionState, CredentialKeyError> {
        Ok(ExclusionState::NotApplicable)
    }
}

fn fake_tmutil(root: &tempfile::TempDir, mode: &str, body: &str) -> PathBuf {
    let path = root.path().join(mode);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn child_is_reaped(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(pid as i32).expect("spawned child pid");
    matches!(
        rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::NOHANG),
        Err(Errno::CHILD)
    )
}

fn fix_exclusion_and_runner() {
    let mut failures = Vec::new();
    let root = temp();
    let directory = secrets(&root);
    let created = create_keyring(&directory, &NotApplicable, T0);
    #[cfg(target_os = "macos")]
    let create_expected = matches!(created, Err(CredentialKeyError::BackupExclusionFailed))
        && listing(&directory).is_empty();
    #[cfg(not(target_os = "macos"))]
    let create_expected = created.is_ok() && directory.path().join(KEYRING_NAME).is_file();
    fix_expect(&mut failures, "fx_notapp_create", create_expected);
    write_stable(&directory);
    let current = KeyringHandle::new(fixture(false));
    let rotated = begin_rotation(&directory, &NotApplicable, &current, T0 + 86_400);
    #[cfg(target_os = "macos")]
    let rotate_expected = matches!(rotated, Err(CredentialKeyError::BackupExclusionFailed))
        && fs::read(directory.path().join(KEYRING_NAME)).unwrap() == hex::decode(STABLE).unwrap()
        && listing(&directory) == [KEYRING_NAME];
    #[cfg(not(target_os = "macos"))]
    let rotate_expected = rotated.is_ok();
    fix_expect(&mut failures, "fx_notapp_rotate", rotate_expected);

    let hang = fake_tmutil(
        &root,
        "hang",
        "printf '[Excluded] fixture\\n'\nexec /bin/sleep 30",
    );
    let limit = Duration::from_millis(150);
    let start = Instant::now();
    let hung = with_tmutil_test(&hang, limit, || {
        TmutilExclusion(SystemTmutil).ensure_excluded(directory.path())
    });
    let elapsed = start.elapsed();
    let observations = take_tmutil_observations();
    let timed_out = observations.len() == 1
        && observations.iter().all(|o| {
            o.program == hang.canonicalize().unwrap()
                && o.operation == "addexclusion"
                && o.timed_out
                && o.kill_attempted
                && o.kill_succeeded
                && o.waited
                && o.try_wait_exited
                && o.exit_signal == Some(9)
                && child_is_reaped(o.pid)
        });
    fix_expect(
        &mut failures,
        "fx_runner_hang_bounded_killed_reaped",
        matches!(hung, Err(CredentialKeyError::BackupExclusionFailed))
            && elapsed >= limit
            && elapsed < limit + Duration::from_secs(3)
            && timed_out,
    );

    let cap = fake_tmutil(&root, "cap", "printf '%070000d' 0");
    let capped = with_tmutil_test(&cap, Duration::from_secs(1), || {
        TmutilExclusion(SystemTmutil).ensure_excluded(directory.path())
    });
    let observations = take_tmutil_observations();
    fix_expect(
        &mut failures,
        "fx_runner_output_capped",
        matches!(capped, Err(CredentialKeyError::BackupExclusionFailed))
            && observations.len() == 1
            && observations.iter().all(|o| {
                o.output_capped
                    && !o.timed_out
                    && o.output_bytes <= TMUTIL_OUTPUT_CAP
                    && o.waited
                    && o.try_wait_exited
                    && child_is_reaped(o.pid)
            }),
    );

    let success = fake_tmutil(&root, "success", "printf '[Excluded] fixture\\n'\nexit 0");
    let accepted = with_tmutil_test(&success, Duration::from_secs(1), || {
        TmutilExclusion(SystemTmutil).ensure_excluded(directory.path())
    });
    let good = take_tmutil_observations();
    let nonzero = fake_tmutil(&root, "nonzero", "printf '[Excluded] fixture\\n'\nexit 7");
    let refused = with_tmutil_test(&nonzero, Duration::from_secs(1), || {
        TmutilExclusion(SystemTmutil).ensure_excluded(directory.path())
    });
    let bad = take_tmutil_observations();
    fix_expect(
        &mut failures,
        "fx_runner_exit_status_parsed",
        matches!(accepted, Ok(ExclusionState::Verified))
            && matches!(refused, Err(CredentialKeyError::BackupExclusionFailed))
            && good.len() == 2
            && good.iter().all(|o| {
                o.exit_code == Some(0) && o.waited && o.try_wait_exited && child_is_reaped(o.pid)
            })
            && bad.len() == 1
            && bad[0].exit_code == Some(7)
            && bad[0].waited
            && bad[0].try_wait_exited
            && child_is_reaped(bad[0].pid),
    );
    finish_fix_assertions(failures);
}
