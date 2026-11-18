// SPDX-License-Identifier: AGPL-3.0-only

//! Start's `preflight`: would the service start, and would it collect. The
//! check reads the data folder and the catalogue only, so it is compiled on
//! every Unix platform; the parts that are the macOS serve admission's own
//! rules stay behind `cfg(target_os = "macos")` (M08-T25-LINUX).

use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::Path,
};

/// The command-line `preflight` (not `--history-only`), whole: the check the Mac
/// app's Start runs before it starts the service, with a 30-second limit; the
/// app's diagnostics report shows it, and the imported-history sign-in requires
/// its `awaiting_sign_in` answer. The answer is the JSON line `preflight` prints;
/// a refusal is the error it reports.
///
/// It answers one question: would the service start, and would it collect
/// (M08-T25). It reads what the production start reads before its listener
/// opens, read-only and bounded by the number of cars and by what interrupted
/// work left behind, never by the history or the packs: the set-up marker, the
/// catalogue through the live read-only reader (`HubStore::open_read_only`,
/// SQLite's own consistent read; no immutable snapshot, so no hash of the whole
/// file), `readiness_check`, the clock, the configured cars, the start's own
/// pack staging and import spool folders, and the selected pair's credential
/// rows, decided as the `doctor` decides
/// ([`crate::diagnostics::selected_credential_decision`]). No `PRAGMA
/// quick_check` runs and no pack is opened. Damage that only those find is the
/// `doctor`'s, the app's diagnostics report's (it runs `doctor`), backup's,
/// restore's and repair's to report.
///
/// Platforms. Everything above is the same reads on every Unix platform. The
/// idle answer and the `awaiting_sign_in` answer are the macOS production serve
/// admission's two rules (`preflight_hub_for_serve` in the LaunchAgent module);
/// a serve on another platform applies neither, so there `preflight` leaves
/// both out, never answers `idle` or `awaiting_sign_in`, and checks every Hub
/// as a configured one, as the verb did before M08-T25. The unfinished Fleet
/// set-up states (`awaiting_sign_in`, `awaiting_vehicles`,
/// `awaiting_activation`, M09-T19 ruling 3) are that admission's third rule,
/// decided on the same bounded read, and are macOS-only in the same way.
pub fn start_preflight(config: &crate::config::HubConfig) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    // A Hub that was set up and lost its catalogue is refused, never idle and
    // never new (M02-T21, DEC-116).
    crate::storage::setup_marker::check_catalogue(&config.data_dir)?;
    // A Hub that was never configured starts idle (M02-T21, DEC-115): the
    // store is neither opened nor created here, and the serve admission
    // applies the same rule. Anything else is checked below.
    #[cfg(target_os = "macos")]
    if super::macos_launch_agent::hub_is_never_configured(config)? {
        return Ok(serde_json::json!({
            "status": "ready",
            "state": "idle",
            "version": crate::BUILD_VERSION,
            "provider": config.collector.provider,
            "collectorEnabled": false,
        }));
    }
    // A configured Hub without the Hub's own login starts for the owner's
    // sign-in, the serve admission's same rule (M09-T16, B2). The clock is read
    // where the whole-store check read it, with the same refusal.
    #[cfg(target_os = "macos")]
    if super::macos_launch_agent::awaits_owner_sign_in(config, || preflight_clock_ms().map(drop))? {
        start_folders_usable(&config.data_dir)?;
        return Ok(serde_json::json!({
            "status": "ready",
            "state": "awaiting_sign_in",
            "version": crate::BUILD_VERSION,
            "provider": config.collector.provider,
            "collectorEnabled": false,
        }));
    }
    // An unfinished Fleet set-up starts for the owner's next step, the serve
    // admission's same rule on the same bounded read (M09-T19 ruling 3,
    // M08-T22). The clock is read once that state is found, with the full
    // check's refusal; a Fleet Hub that is not waiting reads it below.
    #[cfg(target_os = "macos")]
    if let Some(pending) = super::macos_launch_agent::awaits_fleet_setup(
        config,
        super::macos_launch_agent::AdmissionStoreCheck::Bounded,
    )? {
        preflight_clock_ms()?;
        start_folders_usable(&config.data_dir)?;
        return Ok(serde_json::json!({
            "status": "ready",
            "state": pending.state(),
            "version": crate::BUILD_VERSION,
            "provider": config.collector.provider,
            "collectorEnabled": false,
        }));
    }
    // The catalogue file as the immutable snapshot's open first inspected it,
    // with its refusals: present, and a regular file.
    let catalogue = config.data_dir.join("hub.sqlite");
    let metadata = fs::symlink_metadata(&catalogue).map_err(crate::db::StoreError::InspectCatalogue)?;
    if !metadata.file_type().is_file() {
        return Err(crate::db::StoreError::InvalidCataloguePath.into());
    }
    let store = crate::db::HubStore::open_read_only(&config.data_dir)?;
    store.readiness_check()?;
    let now_ms = preflight_clock_ms()?;
    if store.configured_tesla_vehicles()?.is_empty() {
        return Err("at least one configured vehicle is required".into());
    }
    start_folders_usable(&config.data_dir)?;
    let status = crate::auth::credential_sign_in::credential_status(&store, &config.data_dir, now_ms)?;
    if !crate::diagnostics::selected_credential_decision(&store, config, &status, now_ms)?.selected_admitted {
        return Err(crate::auth::credential_sign_in::diagnostic_admission_error(&status, config.collector.provider == crate::config::CollectorProvider::Fleet).into());
    }
    Ok(serde_json::json!({
        "status": "ready",
        "version": crate::BUILD_VERSION,
        "provider": config.collector.provider,
    }))
}

/// The service's start reads two of its own folders before its listener opens
/// and stops on what it finds there (`HubStore::initialize`,
/// `src/storage/db/store_opening.rs`): the pack staging folder it sweeps
/// (`cleanup_stale_pack_staging`) and the import spool it recovers
/// (`recover_stale_import_projection_state_spools`). Read-only, this refuses
/// what would stop that start: a staging folder that cannot be inspected or is
/// not the Hub's own private folder (a folder of the Hub's own that is not
/// private the start makes private, so it passes here), and an import spool
/// that cannot be inspected or listed or is not private. Where the old
/// `preflight` refused the same folder (its catalogue inventory listed the
/// staging folder), the refusal keeps its words. The leftovers inside either
/// folder are not read: deciding which of them the start would remove needs its
/// own rules (M08-T25 notes).
fn start_folders_usable(data_dir: &Path) -> Result<(), crate::db::StoreError> {
    use crate::db::StoreError;
    let own_uid = rustix::process::geteuid().as_raw();
    let staging = data_dir.join("packs").join(".staging");
    // The old `preflight`'s words for a pack folder it could not inspect.
    let inspect = |source: io::Error| StoreError::InspectCatalogPack { path: data_dir.join("packs").join(".staging"), source };
    match fs::symlink_metadata(&staging) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(inspect(source)),
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != own_uid
                || metadata.mode() & 0o022 != 0
            {
                fs::read_dir(&staging).map_err(inspect)?;
                return Err(StoreError::PackStartupRepair(crate::hub_pack::ProjectionPackError::UnsafeStaging(staging)));
            }
        }
    }
    // The import spool: the start's private folder for an interrupted import,
    // owned like the data folder, mode 0700 (`src/storage/db.rs`).
    let spool = data_dir.join("import-spool");
    let spool_metadata = match fs::symlink_metadata(&spool) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(StoreError::InspectImportSpool(error)),
        Ok(metadata) => metadata,
    };
    let root = fs::symlink_metadata(data_dir).map_err(StoreError::InspectImportSpool)?;
    if root.file_type().is_symlink() || !root.is_dir() {
        return Err(StoreError::UnsafeImportSpool(data_dir.to_path_buf()));
    }
    if spool_metadata.file_type().is_symlink()
        || !spool_metadata.is_dir()
        || spool_metadata.uid() != root.uid()
        || spool_metadata.gid() != root.gid()
        || spool_metadata.mode() & 0o777 != 0o700
    {
        return Err(StoreError::UnsafeImportSpool(spool));
    }
    match fs::read_dir(&spool) {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied && own_uid != root.uid() => Ok(()),
        Err(error) => Err(StoreError::InspectImportSpool(error)),
        Ok(_) => Ok(()),
    }
}

/// The clock the whole-store check read before M08-T25 (the retired-lineage
/// clock of `verify_referenced_packs`), with its refusals: a clock before 1970
/// or past the millisecond range refuses Start with the words it had.
pub(crate) fn preflight_clock_ms() -> Result<i64, crate::db::StoreError> {
    let elapsed = preflight_now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(crate::db::StoreError::RetiredLineageClock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| crate::db::StoreError::RetiredLineageClockOverflow)
}

#[cfg(test)]
thread_local! {
    /// A clock a test sets for Start's `preflight` on this thread.
    pub(crate) static PREFLIGHT_CLOCK: std::cell::Cell<Option<std::time::SystemTime>> = const { std::cell::Cell::new(None) };
}

fn preflight_now() -> std::time::SystemTime {
    #[cfg(test)]
    if let Some(now) = PREFLIGHT_CLOCK.with(std::cell::Cell::get) {
        return now;
    }
    std::time::SystemTime::now()
}

#[cfg(test)]
mod tests {
    use super::start_preflight;
    use crate::{config::HubConfig, db::HubStore};
    use std::path::Path;

    fn never_configured(data_dir: &Path) -> HubConfig {
        HubConfig {
            data_dir: data_dir.to_owned(),
            bind: "127.0.0.1:8080".parse().expect("loopback bind"),
            tls: None,
            collector: crate::config::CollectorConfig {
                interval_seconds: 0,
                ..Default::default()
            },
            geocoder: Default::default(),
            import_source: Default::default(),
            terrain: Default::default(),
            http: Default::default(),
            pairing: Default::default(),
            backup: Default::default(),
        }
    }

    /// A never-configured Hub, before and after its empty store exists: idle
    /// on macOS; on any other platform the idle rule is left out and the Hub
    /// is checked as a configured one, so it is refused, never called idle.
    #[test]
    fn start_preflight_claims_idle_only_where_the_serve_admits_it() {
        let temporary = crate::private_tempdir().expect("temporary directory");
        let data = temporary.path().join("data");
        let config = never_configured(&data);
        let before = start_preflight(&config);
        drop(HubStore::initialize(&data).expect("the empty store serve creates"));
        let after = start_preflight(&config);
        if cfg!(target_os = "macos") {
            for answer in [before, after] {
                assert_eq!(answer.expect("idle")["state"], "idle");
            }
        } else {
            let before = before.expect_err("no catalogue to check");
            assert!(
                matches!(before.downcast_ref(), Some(crate::db::StoreError::InspectCatalogue(_))),
                "{before}"
            );
            assert_eq!(
                after.expect_err("no configured car").to_string(),
                "at least one configured vehicle is required"
            );
        }
    }
}
