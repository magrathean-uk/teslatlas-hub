// SPDX-License-Identifier: AGPL-3.0-only

// M06-T10: backup and restore have no fixed member cap (the Backups decision,
// 2026-10-08; REV-M06-T06 finding 5). A store above the old 4,096-member
// ceiling backs up, verifies and restores, and the run's peak footprint is
// measured in a child process. The streamed bundle (v5) declares its totals
// instead of listing members; a bundle written before it (v4, listed) still
// verifies and restores; crafted v5 streams that break order or totals are
// refused. Synthetic data in temporary stores only.

use std::process::Command;

use super::*;
use crate::data_recovery::{
    EncryptedBackupRequest,
    archive_crypto::KdfParams,
    backup_admission::{VolumeTrust, test_seam},
    create_encrypted_backup,
    encrypted_backup::{archive_manifest, clear_hook, write_test_archive},
    restore_encrypted_backup, verify_encrypted_backup,
};

const PASSWORD: &str = "correct horse battery staple";
const TEST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    passes: 1,
    lanes: 1,
};
/// The old fixed ceiling, which a Hub already passes after a rotation.
const OLD_CEILING: usize = 4_096;
/// Synthetic packs in the store above the ceiling: with the catalogue, the
/// schema 2.2 pack and its no-op body the bundle holds 5,003 members.
const SYNTHETIC_PACKS: usize = 5_000;
/// Tells the child test to do the run; without it the child returns at once.
const CHILD_ENV: &str = "TESLATLAS_HUB_M06_T10_CHILD_PACKS";
/// A generous ceiling for the whole test process: far below the Hub's 1 GiB
/// peak, and far above what a run holding no per-member state needs.
const PEAK_FOOTPRINT_LIMIT_BYTES: u64 = 256 * 1024 * 1024;

fn request(destination: &Path) -> EncryptedBackupRequest<'_> {
    let mut request = EncryptedBackupRequest::new(destination, PASSWORD);
    request.kdf = TEST_KDF;
    request.chunk_bytes = 64 * 1024;
    request
}

/// Write `count` small content-addressed packs into the live store.
fn write_synthetic_packs(store: &HubStore, count: usize) {
    let directory = store.packs_dir().join("sha256");
    fs::create_dir_all(&directory).expect("pack directory");
    for index in 0..count {
        let bytes = format!("m06-t10 synthetic pack {index}").into_bytes();
        let name = format!("{}.sqlite.zst", sha256_bytes_hex(&bytes));
        let path = directory.join(name);
        fs::write(&path, &bytes).expect("synthetic pack");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("pack mode");
    }
}

fn count_entries(path: &Path) -> usize {
    fs::read_dir(path).expect("directory listing").count()
}

/// The whole run above the ceiling, in the child process the parent measures.
/// Without the child variable it does nothing, so a suite run passes it by.
#[test]
fn data_recovery_m06_t10_child_run_above_the_old_ceiling() {
    let Ok(packs) = std::env::var(CHILD_ENV) else {
        return;
    };
    let packs: usize = packs.parse().expect("child pack count");
    clear_hook();
    test_seam::force(Some(VolumeTrust::Posix));
    let (temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    write_synthetic_packs(&store, packs);
    let destination = temporary.path().join("destination");
    fs::create_dir(&destination).expect("destination");
    let expected_members = packs + 3;
    assert!(expected_members > OLD_CEILING);

    let created = create_encrypted_backup(&store, &request(&destination))
        .expect("an encrypted backup above the old member ceiling");
    assert_eq!(created.member_count, expected_members);

    let verified =
        verify_encrypted_backup(&created.path, PASSWORD).expect("verify above the ceiling");
    assert_eq!(verified.member_count, expected_members);
    assert_eq!(verified.total_bytes, created.payload_bytes);

    let restored_path = temporary.path().join("restored");
    let restored = restore_encrypted_backup(&created.path, PASSWORD, &restored_path)
        .expect("restore above the ceiling");
    assert_eq!(restored.member_count, expected_members);
    assert_eq!(
        count_entries(&restored_path.join("packs").join("sha256")),
        packs + 1,
        "every pack is restored"
    );
    let sample = b"m06-t10 synthetic pack 4999".to_vec();
    let sample_path = restored_path
        .join("packs")
        .join("sha256")
        .join(format!("{}.sqlite.zst", sha256_bytes_hex(&sample)));
    assert_eq!(fs::read(sample_path).expect("restored sample pack"), sample);
    println!(
        "M06T10_CHILD members={} payload_bytes={} archive_bytes={}",
        created.member_count, created.payload_bytes, created.archive_bytes
    );
}

/// The peak resident set of a finished child, from `/usr/bin/time`.
fn peak_resident_bytes(report: &str) -> Option<u64> {
    for line in report.lines() {
        let line = line.trim();
        if cfg!(target_os = "macos") {
            if let Some(value) = line.strip_suffix("maximum resident set size") {
                return value.trim().parse().ok();
            }
        } else if let Some(value) = line.strip_prefix("Maximum resident set size (kbytes):") {
            return value.trim().parse::<u64>().ok().map(|kib| kib * 1024);
        }
    }
    None
}

#[test]
fn data_recovery_m06_t10_backup_verify_restore_above_the_old_ceiling() {
    let executable = std::env::current_exe().expect("test executable");
    let module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, rest)| rest);
    let child = format!("{module}::data_recovery_m06_t10_child_run_above_the_old_ceiling");
    let flag = if cfg!(target_os = "macos") { "-l" } else { "-v" };
    let output = Command::new("/usr/bin/time")
        .arg(flag)
        .arg(&executable)
        .args(["--exact", &child, "--test-threads=1", "--nocapture"])
        .env(CHILD_ENV, SYNTHETIC_PACKS.to_string())
        .output()
        .expect("run the measured child under /usr/bin/time");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("M06T10_CHILD members="),
        "the child run above the old ceiling failed:\n{stdout}\n{stderr}"
    );
    let peak = peak_resident_bytes(&stderr).expect("peak resident set in the time report");
    let line = format!(
        "M06T10 peak footprint: {peak} bytes ({} MiB) for {} members\n",
        peak / (1024 * 1024),
        SYNTHETIC_PACKS + 3
    );
    print!("{line}");
    // A passing test's output is captured, so the measurement is also kept
    // beside the run, in its temporary directory.
    fs::write(std::env::temp_dir().join("m06-t10-peak-footprint.txt"), &line)
        .expect("record the peak footprint");
    assert!(
        peak < PEAK_FOOTPRINT_LIMIT_BYTES,
        "peak footprint {peak} bytes is above {PEAK_FOOTPRINT_LIMIT_BYTES}"
    );
}

fn rig() -> (tempfile::TempDir, HubStore, PathBuf) {
    clear_hook();
    test_seam::force(Some(VolumeTrust::Posix));
    let (temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    let destination = temporary.path().join("destination");
    fs::create_dir(&destination).expect("destination");
    (temporary, store, destination)
}

/// The names in `parent` that a restore would stage or publish.
fn recovery_leftovers(parent: &Path) -> Vec<String> {
    fs::read_dir(parent)
        .expect("parent listing")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".teslatlas-data-recovery-") || name == "restored")
        .collect()
}

#[test]
fn data_recovery_m06_t10_new_bundle_declares_totals_and_lists_no_member() {
    let (_temporary, store, destination) = rig();
    write_synthetic_packs(&store, 3);
    let created = create_encrypted_backup(&store, &request(&destination)).expect("backup");
    let manifest = archive_manifest(&created.path, PASSWORD).expect("authenticated manifest");
    assert_eq!(manifest.kind, STREAMED_BACKUP_KIND);
    assert!(manifest.members.is_empty(), "the manifest lists no member");
    assert_eq!(manifest.member_count, Some(created.member_count as u64));
    assert_eq!(manifest.payload_bytes, Some(created.payload_bytes));
    assert_eq!(created.member_count, 3 + 3);
}

#[test]
fn data_recovery_m06_t10_older_format_bundle_still_verifies_and_restores() {
    let (temporary, store, destination) = rig();
    // A bundle written before v5 lists its members in the manifest and
    // carries one file record per member. Build one from the exact manifest
    // and member bytes of a listed (v4) generation, in that record layout.
    let generation = temporary.path().join("listed-generation");
    create_data_backup(&store, &generation).expect("listed generation");
    let manifest_bytes = fs::read(generation.join(MANIFEST_NAME)).expect("v4 manifest bytes");
    let manifest: BackupManifest = parse_canonical_json(&manifest_bytes).expect("v4 manifest");
    assert_eq!(manifest.kind, BACKUP_KIND);
    assert!(manifest.member_count.is_none() && manifest.payload_bytes.is_none());
    assert!(manifest.members.len() >= 3, "catalogue, pack and no-op");
    let bodies: Vec<(String, Vec<u8>)> = manifest
        .members
        .iter()
        .map(|member| {
            let bytes = fs::read(generation.join(&member.path)).expect("member bytes");
            (member.path.clone(), bytes)
        })
        .collect();
    let records: Vec<(&str, &[u8])> = bodies
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    let archive =
        write_test_archive(&destination, PASSWORD, TEST_KDF, &manifest_bytes, &records, false)
            .expect("older-format bundle");
    assert_eq!(
        archive_manifest(&archive, PASSWORD).expect("older manifest").kind,
        BACKUP_KIND,
        "the format version tells the older bundle apart"
    );

    let verified = verify_encrypted_backup(&archive, PASSWORD).expect("older bundle verifies");
    assert_eq!(verified.member_count, manifest.members.len());
    let restored_path = temporary.path().join("restored");
    let restored = restore_encrypted_backup(&archive, PASSWORD, &restored_path)
        .expect("older bundle restores");
    assert_eq!(restored.member_count, manifest.members.len());
    assert_eq!(
        restored.installation_id,
        store.installation_id().expect("installation")
    );
    // Packs come back byte for byte; the catalogue and the no-op bodies are
    // re-signed under the restore's fresh cursor key (M06-T09).
    for (path, bytes) in &bodies {
        if !path.starts_with(PACK_DIRECTORY) {
            continue;
        }
        let relative = path.strip_prefix("data/").expect("data member");
        assert_eq!(
            &fs::read(restored_path.join(relative)).expect("restored member"),
            bytes,
            "restored {path}"
        );
    }

    // A listed bundle above the bound it was written under is refused: its
    // member list must stay bounded in memory.
    let mut oversized = manifest.clone();
    let pack = oversized
        .members
        .iter()
        .find(|member| member.path.starts_with(PACK_DIRECTORY))
        .expect("a pack member")
        .clone();
    oversized.members = (0..=LISTED_FORMAT_MAX_MEMBERS)
        .map(|index| {
            let digest = sha256_bytes_hex(format!("bound {index}").as_bytes());
            BackupMember {
                path: format!("{PACK_DIRECTORY}/{digest}.sqlite.zst"),
                sha256: digest,
                ..pack.clone()
            }
        })
        .collect();
    let error = validate_manifest(&oversized).expect_err("listed bound");
    assert!(error.to_string().contains("outside the supported bound"), "{error}");
    // A listed manifest never carries streamed totals, and a streamed one
    // never lists members.
    let mut mixed = manifest.clone();
    mixed.member_count = Some(mixed.members.len() as u64);
    mixed.payload_bytes = Some(1);
    assert!(validate_manifest(&mixed).is_err());
    let mut streamed_with_list = manifest;
    streamed_with_list.kind = STREAMED_BACKUP_KIND.to_owned();
    streamed_with_list.member_count = Some(streamed_with_list.members.len() as u64);
    streamed_with_list.payload_bytes = Some(1);
    assert!(validate_manifest(&streamed_with_list).is_err());
}

/// Two synthetic pack records, in ascending path order.
fn two_packs() -> Vec<(String, Vec<u8>)> {
    let mut packs: Vec<(String, Vec<u8>)> = ["crafted pack a", "crafted pack b"]
        .into_iter()
        .map(|text| {
            let bytes = text.as_bytes().to_vec();
            let path = format!("{PACK_DIRECTORY}/{}.sqlite.zst", sha256_bytes_hex(&bytes));
            (path, bytes)
        })
        .collect();
    packs.sort();
    packs
}

/// A v5 manifest for `records` declaring `count`, with a fresh generation.
fn crafted_manifest(base: &BackupManifest, records: &[(String, Vec<u8>)], count: u64) -> Vec<u8> {
    let mut manifest = base.clone();
    manifest.generation = Uuid::new_v4();
    manifest.member_count = Some(count);
    manifest.payload_bytes = Some(records.iter().map(|(_, bytes)| bytes.len() as u64).sum());
    canonical_json(&manifest).expect("crafted manifest")
}

#[test]
fn data_recovery_m06_t10_streamed_bundle_with_broken_order_or_totals_is_refused() {
    let (temporary, store, destination) = rig();
    let created = create_encrypted_backup(&store, &request(&destination)).expect("backup");
    let base = archive_manifest(&created.path, PASSWORD).expect("v5 manifest");
    let packs = two_packs();
    let mut reversed = packs.clone();
    reversed.reverse();
    let cases: Vec<(&str, Vec<u8>, Vec<(String, Vec<u8>)>, &str)> = vec![
        (
            "out-of-order",
            crafted_manifest(&base, &packs, 2),
            reversed,
            "not uniquely sorted",
        ),
        (
            "duplicate",
            crafted_manifest(&base, &[packs[0].clone(), packs[0].clone()], 2),
            vec![packs[0].clone(), packs[0].clone()],
            "not uniquely sorted",
        ),
        (
            "fewer-than-declared",
            crafted_manifest(&base, &packs, 3),
            packs.clone(),
            "fewer members or bytes",
        ),
        (
            "more-than-declared",
            crafted_manifest(&base, &packs, 1),
            packs.clone(),
            "more members or bytes",
        ),
    ];
    for (label, manifest_bytes, records, expected) in cases {
        let folder = temporary.path().join(label);
        fs::create_dir(&folder).expect("case folder");
        let borrowed: Vec<(&str, &[u8])> = records
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
            .collect();
        let archive =
            write_test_archive(&folder, PASSWORD, TEST_KDF, &manifest_bytes, &borrowed, true)
                .expect("crafted bundle");
        let error = verify_encrypted_backup(&archive, PASSWORD).expect_err(label);
        assert!(error.to_string().contains(expected), "{label}: {error}");
        let error = restore_encrypted_backup(&archive, PASSWORD, &folder.join("restored"))
            .expect_err(label);
        assert!(error.to_string().contains(expected), "{label}: {error}");
        assert!(
            recovery_leftovers(&folder).is_empty(),
            "{label}: nothing staged or published"
        );
    }
}
