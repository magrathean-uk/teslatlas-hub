// SPDX-License-Identifier: AGPL-3.0-only

// M06-T08: the shipped `backup`, `verify-backup` and `restore-data` commands
// and the encrypted bundle (REV-M06-T06 finding 2). Each test drives the real
// dispatcher (`run`) on a small synthetic Hub in its own temporary root, with
// the operator home inside that root; no real backup destination, no network.
// The recovery password reaches the dispatcher through the test seam that
// stands in for standard input, parsed exactly as standard input is.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use tesatlas_hub::{
    data_recovery::{EncryptedBackupRequest, create_encrypted_backup},
    db::HubStore,
};

use crate::{Cli, Command, run, set_operator_home, set_password_input, take_password_input};

const PASSWORD: &str = "correct horse battery staple";
const WRONG_PASSWORD: &str = "incorrect horse battery staple";

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    config: PathBuf,
    destination: PathBuf,
    installation_id: uuid::Uuid,
    database_file: PathBuf,
}

fn private_directory(path: &Path) {
    fs::create_dir_all(path).expect("directory");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("private directory");
}

fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().expect("temporary root");
    let root = temporary.path().canonicalize().expect("canonical root");
    private_directory(&root);
    let data = root.join("data");
    private_directory(&data);
    let (installation_id, database_file) = {
        let store = HubStore::initialize(&data).expect("store");
        (
            store.installation_id().expect("installation"),
            PathBuf::from(store.database_path().file_name().expect("database file")),
        )
    };
    let config = root.join("config.toml");
    fs::write(&config, format!("data_dir = {data:?}\nbind = '127.0.0.1:18443'\n")).expect("config");
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).expect("private config");
    let destination = root.join("backups");
    private_directory(&destination);
    private_directory(&root.join("home"));
    Fixture {
        _temporary: temporary,
        root,
        data,
        config,
        destination,
        installation_id,
        database_file,
    }
}

impl Fixture {
    /// Runs one command through the dispatcher with `password` offered as
    /// standard input (`None`: an empty standard input).
    async fn run(
        &self,
        command: Command,
        password: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        set_password_input(Some(password.map_or_else(Vec::new, |text| format!("{text}\n").into_bytes())));
        set_operator_home(Some(self.root.join("home")));
        let outcome = run(Cli {
            config: Some(self.config.clone()),
            command,
        })
        .await;
        set_operator_home(None);
        outcome
    }

    fn bundles(&self) -> Vec<PathBuf> {
        let mut bundles: Vec<PathBuf> = fs::read_dir(&self.destination)
            .expect("destination listing")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "tlbk"))
            .collect();
        bundles.sort();
        bundles
    }

    fn root_listing(&self) -> Vec<std::ffi::OsString> {
        let mut names: Vec<_> = fs::read_dir(&self.root)
            .expect("root listing")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        names.sort();
        names
    }

    /// One encrypted bundle made by the shipped `backup` command.
    async fn shipped_bundle(&self) -> PathBuf {
        self.run(
            Command::Backup {
                destination: self.destination.clone(),
            },
            Some(PASSWORD),
        )
        .await
        .expect("the shipped backup command");
        let bundles = self.bundles();
        assert_eq!(bundles.len(), 1, "one encrypted bundle: {bundles:?}");
        bundles[0].clone()
    }
}

fn verify(source: &Path, plaintext_directory: bool) -> Command {
    Command::VerifyBackup {
        source: source.to_path_buf(),
        plaintext_directory,
    }
}

fn restore(source: &Path, destination: &Path, plaintext_directory: bool) -> Command {
    Command::RestoreData {
        source: source.to_path_buf(),
        destination: destination.to_path_buf(),
        plaintext_directory,
    }
}

/// REV-M06-T06 finding 2, confirmed at the base commit (bfa0f08) through the
/// shipped command: `verify-backup` refused the encrypted bundle the Hub's own
/// encrypted operation produces.
#[tokio::test]
async fn backup_finding_2_shipped_verify_accepts_the_encrypted_bundle() {
    let hub = fixture();
    let report = {
        let store = HubStore::initialize(&hub.data).expect("store");
        create_encrypted_backup(&store, &EncryptedBackupRequest::new(&hub.destination, PASSWORD))
            .expect("encrypted backup")
    };
    let outcome = hub.run(verify(&report.path, false), Some(PASSWORD)).await;
    assert!(
        outcome.is_ok(),
        "the shipped verify-backup must accept the encrypted bundle: {outcome:?}"
    );
}

/// Acceptance 1: `backup` produces the encrypted bundle by default, and the
/// shipped `verify-backup` and `restore-data` accept it with the password.
#[tokio::test]
async fn backup_encrypted_by_default_verifies_and_restores_through_the_shipped_commands() {
    let hub = fixture();
    let bundle = hub.shipped_bundle().await;
    assert!(
        fs::symlink_metadata(&bundle).expect("bundle").file_type().is_file(),
        "the default backup is one encrypted file, not a plaintext directory"
    );
    let header = fs::read(&bundle).expect("bundle bytes");
    let database = fs::read(hub.data.join(&hub.database_file)).expect("live catalogue");
    assert_ne!(&header[..16], &database[..16], "the bundle does not start with the SQLite image");

    hub.run(verify(&bundle, false), Some(PASSWORD))
        .await
        .expect("the shipped verify-backup accepts the encrypted bundle");

    let restored = hub.root.join("restored");
    hub.run(restore(&bundle, &restored, false), Some(PASSWORD))
        .await
        .expect("the shipped restore-data restores the encrypted bundle");
    let store = HubStore::initialize(&restored).expect("restored Hub opens");
    assert_eq!(store.installation_id().expect("installation"), hub.installation_id);
}

/// Acceptance 2: a wrong password is refused by verify and by restore before
/// anything is written.
#[tokio::test]
async fn backup_wrong_password_is_refused_before_anything_is_written() {
    let hub = fixture();
    let bundle = hub.shipped_bundle().await;
    let before = hub.root_listing();
    let restored = hub.root.join("restored");

    let error = hub
        .run(verify(&bundle, false), Some(WRONG_PASSWORD))
        .await
        .expect_err("verify refuses the wrong password");
    assert!(error.to_string().contains("password is wrong"), "{error}");

    let error = hub
        .run(restore(&bundle, &restored, false), Some(WRONG_PASSWORD))
        .await
        .expect_err("restore refuses the wrong password");
    assert!(error.to_string().contains("password is wrong"), "{error}");
    assert!(!restored.exists(), "nothing restored");
    assert_eq!(hub.root_listing(), before, "nothing written beside the destination");
    assert!(!error.to_string().contains(WRONG_PASSWORD), "the password is never echoed");

    let error = hub
        .run(restore(&bundle, &restored, false), None)
        .await
        .expect_err("no password offered");
    assert!(error.to_string().contains("recovery password"), "{error}");
    assert!(!restored.exists(), "nothing restored");
}

/// Acceptance 2: an older plaintext directory backup offered without the
/// explicit flag is refused, before the password is read and before anything
/// is written; named explicitly it still verifies and restores, and the flag
/// never applies to an encrypted bundle.
#[tokio::test]
async fn backup_plaintext_directory_needs_the_explicit_flag() {
    let hub = fixture();
    let plaintext = hub.root.join("plaintext");
    hub.run(
        Command::BackupPlaintextDirectory {
            destination: plaintext.clone(),
        },
        None,
    )
    .await
    .expect("the older format, named explicitly");
    assert!(plaintext.is_dir());
    let restored = hub.root.join("restored");
    let before = hub.root_listing();

    for command in [verify(&plaintext, false), restore(&plaintext, &restored, false)] {
        let error = hub
            .run(command, Some(PASSWORD))
            .await
            .expect_err("a plaintext directory without the flag is refused");
        assert!(error.to_string().starts_with("plaintext_directory_refused"), "{error}");
        assert!(take_password_input().is_some(), "refused before the password is read");
        assert!(!restored.exists(), "nothing restored");
        assert_eq!(hub.root_listing(), before, "nothing written");
    }

    hub.run(verify(&plaintext, true), None)
        .await
        .expect("named explicitly, the older format verifies");
    hub.run(restore(&plaintext, &restored, true), None)
        .await
        .expect("named explicitly, the older format restores");
    assert!(restored.join(&hub.database_file).is_file());

    let bundle = hub.shipped_bundle().await;
    let error = hub
        .run(verify(&bundle, true), Some(PASSWORD))
        .await
        .expect_err("the flag never names an encrypted bundle");
    assert!(error.to_string().contains("not a directory"), "{error}");
}

/// The password is never an argument: no command accepts one.
#[test]
fn backup_commands_take_no_password_argument() {
    for arguments in [
        ["tesatlas-hub", "backup", "--destination", "d", "--password", "x"].as_slice(),
        ["tesatlas-hub", "verify-backup", "--source", "s", "--password", "x"].as_slice(),
        ["tesatlas-hub", "restore-data", "--source", "s", "--destination", "d", "--password", "x"].as_slice(),
    ] {
        assert!(Cli::try_parse_from(arguments).is_err(), "{arguments:?}");
    }
    let parsed = Cli::try_parse_from(["tesatlas-hub", "backup", "--destination", "d"]).expect("backup");
    assert!(matches!(parsed.command, Command::Backup { .. }));
}
