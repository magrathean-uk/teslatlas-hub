// SPDX-License-Identifier: AGPL-3.0-only

//! "Set up before" (M02-T21, DEC-116): a durable marker outside the catalogue.
//!
//! `hub-setup.json` in the data folder records that this Hub was set up (the
//! Hub's own login was stored, or the full serve admission passed) and the
//! installation id of its catalogue. It lives outside `hub.sqlite`, so losing
//! or emptying the catalogue cannot also lose the record that there was one.
//! Every command that may create or initialise `hub.sqlite` checks it first:
//! with a marker, the catalogue must exist, open read-only and hold the same
//! installation id, or the command refuses and creates nothing. A Hub with a
//! marker is never treated as never configured, so it never starts idle.
//! Without a marker (a Hub from before it, or one that lost it), what
//! survives outside the catalogue tells an earlier set-up (DEC-120).

use std::{
    fs,
    io::{self, Write as _},
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
};

use uuid::Uuid;

/// The marker's file name in the data folder.
pub const SETUP_MARKER_FILE: &str = "hub-setup.json";

const MARKER_VERSION: u32 = 1;
const MARKER_MAX_BYTES: u64 = 4096;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Marker {
    version: u32,
    installation_id: Uuid,
}

/// Why a Hub that was set up before may not start.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SetupMarkerError {
    #[error(
        "this Hub was set up before (hub-setup.json), but its catalogue hub.sqlite is missing; restore the data folder from a backup (restore-data); nothing was created"
    )]
    CatalogueMissing,
    #[error(
        "this Hub was set up before (hub-setup.json), but its catalogue hub.sqlite is empty or unreadable; restore the data folder from a backup (restore-data); nothing was changed"
    )]
    CatalogueUnreadable,
    #[error(
        "this Hub was set up before (hub-setup.json), but hub.sqlite belongs to another installation; restore this Hub's data folder from a backup (restore-data); nothing was changed"
    )]
    CatalogueReplaced,
    #[error("the Hub setup marker (hub-setup.json) cannot be read or written: {0}")]
    MarkerUnavailable(String),
    #[error(
        "this Hub was set up before (its credential keyring secrets/credential-keys remains), but its catalogue hub.sqlite is missing, empty or new; restore the data folder from a backup (restore-data); nothing was created or changed"
    )]
    CatalogueLostWithoutMarker,
    #[error("whether this Hub was set up before cannot be checked (secrets/credential-keys): {0}; nothing was created or changed")]
    EarlierSetupUnknown(String),
}

pub fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SETUP_MARKER_FILE)
}

/// The installation id the marker records, `None` when there is no marker.
/// A marker that is present but unreadable or invalid is an error, never
/// "no marker".
pub fn read(data_dir: &Path) -> Result<Option<Uuid>, SetupMarkerError> {
    let path = marker_path(data_dir);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SetupMarkerError::MarkerUnavailable(error.kind().to_string())),
    };
    if !metadata.file_type().is_file() || metadata.len() > MARKER_MAX_BYTES {
        return Err(SetupMarkerError::MarkerUnavailable("not a small regular file".to_owned()));
    }
    let bytes = fs::read(&path).map_err(|error| SetupMarkerError::MarkerUnavailable(error.kind().to_string()))?;
    let marker: Marker = serde_json::from_slice(&bytes)
        .map_err(|_| SetupMarkerError::MarkerUnavailable("not a setup marker".to_owned()))?;
    if marker.version != MARKER_VERSION {
        return Err(SetupMarkerError::MarkerUnavailable("unknown marker version".to_owned()));
    }
    Ok(Some(marker.installation_id))
}

/// Records that this Hub, with catalogue `installation_id`, was set up. An
/// existing marker for the same installation is kept; one for another
/// installation is refused. Written 0600 through a synced temporary file.
pub fn record(data_dir: &Path, installation_id: Uuid) -> Result<(), SetupMarkerError> {
    match read(data_dir)? {
        Some(recorded) if recorded == installation_id => return Ok(()),
        Some(_) => return Err(SetupMarkerError::CatalogueReplaced),
        None => {}
    }
    let unavailable = |error: io::Error| SetupMarkerError::MarkerUnavailable(error.kind().to_string());
    let bytes = serde_json::to_vec(&Marker {
        version: MARKER_VERSION,
        installation_id,
    })
    .map_err(|_| SetupMarkerError::MarkerUnavailable("not encodable".to_owned()))?;
    let staged = data_dir.join(format!(".{SETUP_MARKER_FILE}.{}.tmp", Uuid::new_v4()));
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged)
        .and_then(|mut file| {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&staged, marker_path(data_dir)))
        .and_then(|()| fs::File::open(data_dir)?.sync_all());
    if let Err(error) = written {
        let _ = fs::remove_file(&staged);
        return Err(unavailable(error));
    }
    Ok(())
}

/// The check before `hub.sqlite` is created or initialised: with a marker,
/// the catalogue must exist, be readable and hold the marker's installation
/// id. Without a marker, see [`check_unmarked_catalogue`]. It writes nothing
/// but a marker taken from an intact catalogue that was set up.
pub fn check_catalogue(data_dir: &Path) -> Result<(), SetupMarkerError> {
    let Some(recorded) = read(data_dir)? else {
        return check_unmarked_catalogue(data_dir);
    };
    let catalogue = data_dir.join("hub.sqlite");
    match fs::symlink_metadata(&catalogue) {
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() > 0 => {}
        Ok(_) => return Err(SetupMarkerError::CatalogueUnreadable),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(SetupMarkerError::CatalogueMissing);
        }
        Err(_) => return Err(SetupMarkerError::CatalogueUnreadable),
    }
    let found = crate::db::HubStore::open_read_only(data_dir)
        .and_then(|store| store.installation_id())
        .map_err(|_| SetupMarkerError::CatalogueUnreadable)?;
    if found != recorded {
        return Err(SetupMarkerError::CatalogueReplaced);
    }
    Ok(())
}

/// Without a marker (DEC-120): a Hub from before the marker, or one that lost
/// it. An intact catalogue that was set up (a configured car, a published
/// vehicle or a credential pair) gets its marker written from it. A missing
/// or empty catalogue, or an intact one holding no set-up, beside the
/// credential keyring (written outside the catalogue only when a credential
/// pair is stored) is refused and left as it is, and so is a SQLite file with
/// nothing in it beside the keyring (DEC-121). Anything else is a Hub never
/// set up. Any other catalogue that does not open as a current one (an older
/// schema the start migrates, or damage the start reports) is left to the
/// start.
fn check_unmarked_catalogue(data_dir: &Path) -> Result<(), SetupMarkerError> {
    let keyring = data_dir
        .join("secrets")
        .join(crate::auth::credential_keys::KEYRING_NAME);
    let lost = match fs::symlink_metadata(keyring) {
        Ok(_) => Err(SetupMarkerError::CatalogueLostWithoutMarker),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => return Err(SetupMarkerError::EarlierSetupUnknown(error.kind().to_string())),
    };
    let catalogue = data_dir.join("hub.sqlite");
    match fs::symlink_metadata(&catalogue) {
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() > 0 => {}
        _ => return lost,
    }
    let set_up = crate::db::HubStore::open_read_only(data_dir).and_then(|store| {
        let held = !store.configured_tesla_vehicles()?.is_empty()
            || !store.published_vehicles()?.is_empty()
            || store
                .credential_pair_presence(crate::auth::credential_envelope::CredentialSlot::OwnerApi)?
                .is_some()
            || store
                .credential_pair_presence(crate::auth::credential_envelope::CredentialSlot::FleetApi)?
                .is_some();
        held.then(|| store.installation_id()).transpose()
    });
    match set_up {
        Ok(Some(installation)) => record(data_dir, installation),
        Ok(None) => lost,
        // It does not open as a catalogue, but initialisation would take it
        // as a new one and give it a new installation identity (DEC-121).
        Err(_) if lost.is_err() && is_schema_empty(&catalogue) => lost,
        Err(_) => Ok(()),
    }
}

/// Whether `catalogue` is a SQLite file with nothing in it: application_id 0,
/// user_version 0 and an empty schema, the state that `HubStore::initialize`
/// admits as a new catalogue. Read-only. `false` when the file cannot be read
/// as SQLite; initialisation refuses such a file itself.
fn is_schema_empty(catalogue: &Path) -> bool {
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        catalogue,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return false;
    };
    let read = |sql: &str| connection.query_row(sql, [], |row| row.get::<_, i64>(0));
    matches!(
        (
            read("PRAGMA application_id"),
            read("PRAGMA user_version"),
            read("SELECT count(*) FROM sqlite_schema"),
        ),
        (Ok(0), Ok(0), Ok(0))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_install_journey_marker_absent_passes_and_creates_nothing() {
        let folder = tempfile::tempdir().expect("folder");
        assert_eq!(check_catalogue(folder.path()), Ok(()));
        assert_eq!(read(folder.path()), Ok(None));
        assert!(!folder.path().join("hub.sqlite").exists());
    }

    #[test]
    fn fresh_install_journey_marker_refuses_a_missing_empty_or_replaced_catalogue() {
        let folder = tempfile::tempdir().expect("folder");
        let store = crate::db::HubStore::initialize(folder.path()).expect("store");
        let id = store.installation_id().expect("id");
        drop(store);
        record(folder.path(), id).expect("marker");
        record(folder.path(), id).expect("the same marker again");
        assert_eq!(check_catalogue(folder.path()), Ok(()));
        assert_eq!(record(folder.path(), Uuid::new_v4()), Err(SetupMarkerError::CatalogueReplaced));

        let catalogue = folder.path().join("hub.sqlite");
        fs::OpenOptions::new().write(true).open(&catalogue).expect("open").set_len(0).expect("truncate");
        assert_eq!(check_catalogue(folder.path()), Err(SetupMarkerError::CatalogueUnreadable));
        assert_eq!(fs::metadata(&catalogue).expect("catalogue").len(), 0, "nothing initialised");

        for name in ["hub.sqlite", "hub.sqlite-wal", "hub.sqlite-shm"] {
            let _ = fs::remove_file(folder.path().join(name));
        }
        assert_eq!(check_catalogue(folder.path()), Err(SetupMarkerError::CatalogueMissing));
        assert!(!catalogue.exists(), "nothing created");

        drop(crate::db::HubStore::initialize(folder.path()).expect("another catalogue"));
        assert_eq!(check_catalogue(folder.path()), Err(SetupMarkerError::CatalogueReplaced));
    }
}
