// SPDX-License-Identifier: AGPL-3.0-only

#[cfg(unix)]
fn collector_can_start(
    store: &HubStore,
    config: &HubConfig,
) -> Result<bool, Box<dyn std::error::Error>> {
    let credentials = teslatlas_hub::auth::credential_sign_in::credential_status(store, &config.data_dir, current_epoch_ms()?)?;
    // Only a pair admitted at load (the Hub's own login) can start collection.
    let slot = match config.collector.provider {
        CollectorProvider::Legacy => &credentials.legacy_credentials,
        CollectorProvider::Fleet => &credentials.fleet_credentials,
    };
    let credentials_present = slot.present
        && !matches!(slot.store_state, "source_credential_disabled" | "unknown_origin");
    Ok(config.collector.interval_seconds > 0
        && !store.configured_tesla_vehicles()?.is_empty()
        && credentials_present)
}

fn default_config_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Teslatlas Hub")
            .join("config.toml");
    }
    #[cfg(target_os = "linux")]
    return PathBuf::from("/etc/teslatlas-hub/config.toml");
    #[cfg(not(target_os = "linux"))]
    PathBuf::from("config.toml")
}

#[cfg(unix)]
struct MacMigrationInput<'a> {
    config_path: &'a Path,
    source_url: &'a str,
    car_id: i64,
    source_key: Option<&'a str>,
    new_import_source: bool,
    postgres_password_file: &'a Path,
    history_only: bool,
    encryption_key_file: Option<&'a Path>,
    access_token_file: Option<&'a Path>,
    refresh_token_file: Option<&'a Path>,
    online_snapshot: bool,
    preserve_existing_credentials: bool,
}

#[cfg(unix)]
fn validate_history_only_config(config: &HubConfig) -> Result<(), Box<dyn std::error::Error>> {
    if config.collector.interval_seconds != 0
        || config.collector.fleet_telemetry.is_some()
        || config.collector.legacy_auth.enabled
    {
        return Err(
            "history-only import requires every collector and Legacy auth path disabled".into(),
        );
    }
    Ok(())
}

#[cfg(unix)]
const MAX_MIGRATION_POSTGRES_PASSWORD_BYTES: usize = 4 * 1024;
#[cfg(unix)]
const MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES: usize = MAX_MIGRATION_POSTGRES_PASSWORD_BYTES + 2;
#[cfg(all(unix, test))]
const MAX_MIGRATION_TOKEN_BYTES: usize =
    teslatlas_hub::source_token_envelope::MAX_TOKEN_PLAINTEXT_BYTES;
#[cfg(all(unix, test))]
const MAX_MIGRATION_TOKEN_FILE_BYTES: usize = MAX_MIGRATION_TOKEN_BYTES + 2;
#[cfg(unix)]
#[cfg(all(unix, test))]
const MAX_MIGRATION_ENCRYPTION_KEY_BYTES: usize = 16 * 1024;

#[cfg(unix)]
fn migration_stop_confirmed(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y")
}

#[cfg(target_os = "macos")]
fn macos_migration_start_request() -> serde_json::Value {
    serde_json::json!({
        "serviceStartRequested": true,
        "next": "open Teslatlas Hub.app and choose Start Hub",
    })
}

#[cfg(unix)]
fn migration_start_requested(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y")
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationSecretReadError {
    Read,
    UnsafeFile,
    IdentityChanged,
    TooLarge,
}

#[cfg(unix)]
impl std::fmt::Display for MigrationSecretReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Read => "cannot read secret",
            Self::UnsafeFile => "secret file is unsafe",
            Self::IdentityChanged => "secret file changed while reading",
            Self::TooLarge => "secret exceeds the fixed size limit",
        })
    }
}

#[cfg(unix)]
impl std::error::Error for MigrationSecretReadError {}

#[cfg(unix)]
#[cfg(test)]
async fn run_macos_migration(
    admission: &std::sync::Arc<AdmittedUserHub>,
    input: MacMigrationInput<'_>,
) -> Result<bool, Box<dyn std::error::Error>> {
    run_macos_migration_prepared(admission, input, None, false).await
}

#[cfg(unix)]
async fn run_macos_migration_prepared(
    admission: &std::sync::Arc<AdmittedUserHub>,
    MacMigrationInput {
        config_path,
        source_url,
        car_id,
        source_key,
        new_import_source,
        postgres_password_file,
        history_only,
        encryption_key_file,
        access_token_file,
        refresh_token_file,
        online_snapshot,
        preserve_existing_credentials,
    }: MacMigrationInput<'_>,
    mut prepared_key: Option<zeroize::Zeroizing<Vec<u8>>>,
    replace_credentials: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    if car_id <= 0 {
        return Err("--car-id must be a positive source car id".into());
    }
    // Source credentials are disabled: a source key or token input is refused
    // before any path is opened, the configuration is read or the store is
    // touched. Migration imports history only and never changes the Hub's
    // own sign-in.
    if encryption_key_file.is_some()
        || access_token_file.is_some()
        || refresh_token_file.is_some()
        || prepared_key.take().is_some()
        || replace_credentials
    {
        return Err(teslatlas_hub::auth::credential_sign_in::source_credential_disabled().into());
    }
    if history_only && (!online_snapshot || preserve_existing_credentials) {
        return Err(
            "history-only import requires --online-snapshot and no Tesla credential flags".into(),
        );
    }

    let config = HubConfig::load(config_path)?;
    let import_source_key =
        migration_source_key(source_key, config.import_source.source_key.as_deref());
    teslatlas_hub::config::validate_import_source_key(import_source_key)?;
    if history_only {
        validate_history_only_config(&config)?;
    }
    admission.assert_sensitive_access()?;
    admission.assert_store_path(&config.data_dir)?;
    HubStore::preflight_import_source_key(&config.data_dir, import_source_key, new_import_source)?;
    let source = ReadOnlySource::parse(source_url)?;
    if history_only && !source.is_loopback() {
        return Err("history-only import requires a numeric loopback PostgreSQL source".into());
    }
    let postgres_password = read_migration_postgres_password(postgres_password_file)?;
    let mut limits = config.import_source.read_limits()?;
    let profile = derive_effective_import_profile(
        limits.parallel_copy_lanes,
        &config.import_source.performance_profile,
        &config.data_dir,
    )?;
    limits.parallel_copy_lanes = profile.parallel_copy_lanes;
    if history_only {
        let store = HubStore::initialize(&config.data_dir)?;
        let _entry = teslatlas_hub::open_credential_vault_for_entry(store.clone(), std::sync::Arc::clone(admission), &config.data_dir)
            .map_err(teslatlas_hub::auth::credential_sign_in::credential_entry_error)?;
        let mut catalogue_checkpoint = CatalogueCheckpointGuard::new(store.clone());
        store.check_import_source_key(import_source_key, new_import_source)?;
        let cursor_key = load_or_create_cursor_key(&config.data_dir)?;
        let progress = migration_progress_reporter(true);
        let report = import_direct_migration_snapshot(
            &store,
            &cursor_key,
            &source,
            &postgres_password,
            car_id,
            import_source_key,
            new_import_source,
            limits,
            MigrationCaptureScope::HistorySnapshot,
            progress.clone(),
        )
        .await?;
        catalogue_checkpoint.finish()?;
        progress.complete(SourceDbMigrationPhase::Complete);
        println!(
            "{}",
            serde_json::json!({
                "status": "imported",
                "captureMode": "online-snapshot",
                "credentialScope": "history-only",
                "applicationVersionStatus": "unknown",
                "schemaEvidence": "v4_2_compatible_schema",
                "selectedCarId": car_id,
                "projectedRows": report.projected_rows,
                "snapshotId": report.snapshot_id,
                "sequence": report.sequence,
                "cutoverUnsettled": report.cutover_unsettled,
                "retryRecommended": report.cutover_unsettled,
                "sourceNeverMutated": true,
                "credentialsInspected": false,
                "credentialsImported": false,
                "profileVersion": profile.version,
                "parallelCopyLanes": profile.parallel_copy_lanes,
                "profileReason": profile.reason.as_str(),
            })
        );
        return Ok(false);
    }
    let store = HubStore::initialize(&config.data_dir)?;
    let _entry = teslatlas_hub::open_credential_vault_for_entry(store.clone(), std::sync::Arc::clone(admission), &config.data_dir)
        .map_err(teslatlas_hub::auth::credential_sign_in::credential_entry_error)?;
    let mut catalogue_checkpoint = CatalogueCheckpointGuard::new(store.clone());
    store.check_import_source_key(import_source_key, new_import_source)?;
    let cursor_key = load_or_create_cursor_key(&config.data_dir)?;
    if !online_snapshot {
        let initial_progress = migration_progress_reporter(online_snapshot);
        let initial_copy_started = Instant::now();
        let initial_report = import_direct_migration_snapshot(
            &store,
            &cursor_key,
            &source,
            &postgres_password,
            car_id,
            import_source_key,
            new_import_source,
            limits,
            MigrationCaptureScope::SettledWithoutCredentials,
            initial_progress,
        )
        .await?;
        tracing::info!(
            duration_ms = elapsed_migration_millis(initial_copy_started),
            "finished initial source database migration copy"
        );

        println!(
            "{}",
            serde_json::json!({
                "status": "initial-copy-complete",
                "captureMode": "direct",
                "selectedCarId": car_id,
                "projectedRows": initial_report.projected_rows,
                "snapshotId": initial_report.snapshot_id,
                "sequence": initial_report.sequence,
                "profileVersion": profile.version,
                "parallelCopyLanes": profile.parallel_copy_lanes,
                "profileReason": profile.reason.as_str(),
            })
        );
        print!("stop the source application now. Confirm it is stopped before final copy [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !migration_stop_confirmed(&answer) {
            return Err(
                "Source application stop was not confirmed; final migration capture was not started"
                    .into(),
            );
        }
    }

    // Cutover mode re-captures after the operator stops the source application.
    // Online mode captures while it remains live. Only history is captured.
    let progress = migration_progress_reporter(online_snapshot);
    let final_copy_started = Instant::now();
    let report = import_direct_migration_snapshot(
        &store,
        &cursor_key,
        &source,
        &postgres_password,
        car_id,
        import_source_key,
        new_import_source,
        limits,
        MigrationCaptureScope::SettledWithoutCredentials,
        progress.clone(),
    )
    .await?;
    tracing::info!(
        duration_ms = elapsed_migration_millis(final_copy_started),
        "finished final source database migration copy"
    );
    progress.advance_finalizing(1, 2);

    let fleet_still_present = teslatlas_hub::auth::credential_sign_in::credential_status(&store, &config.data_dir, current_epoch_ms()?)?.fleet_credentials.present;
    progress.advance_finalizing(3, 4);
    tracing::info!(
        selected_car_id = car_id,
        projected_rows = report.projected_rows,
        fleet_tokens_preserved = fleet_still_present,
        "Source database history imported; source PostgreSQL was not written; no credential was read or changed"
    );

    let checkpoint_started = Instant::now();
    catalogue_checkpoint
        .finish()
        .map_err(|error| migration_outcome_ambiguous("checkpointing imported catalogue", error))?;
    tracing::info!(
        duration_ms = elapsed_migration_millis(checkpoint_started),
        "checkpointed imported source database catalogue"
    );
    progress.complete(SourceDbMigrationPhase::Complete);

    println!(
        "{}",
        serde_json::json!({
            "status": "imported",
            "captureMode": if online_snapshot { "online-snapshot" } else { "direct" },
            "selectedCarId": car_id,
            "projectedRows": report.projected_rows,
            "snapshotId": report.snapshot_id,
            "sequence": report.sequence,
            "cutoverUnsettled": report.cutover_unsettled,
            "retryRecommended": report.cutover_unsettled,
            "sourceNeverMutated": true,
            "fleetTokensPreserved": fleet_still_present,
            "existingCredentialsPreserved": true,
            "credentialsInspected": false,
            "credentialsImported": false,
            "profileVersion": profile.version,
            "parallelCopyLanes": profile.parallel_copy_lanes,
            "profileReason": profile.reason.as_str(),
        })
    );

    if online_snapshot {
        return Ok(false);
    }

    print!("Start Teslatlas Hub now? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let start_hub = migration_start_requested(&answer);

    Ok(start_hub)
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum MigrationCaptureScope {
    HistorySnapshot,
    SettledWithoutCredentials,
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
async fn import_direct_migration_snapshot(
    store: &HubStore,
    cursor_key: &CursorKey,
    source: &ReadOnlySource,
    postgres_password: &SourceDbPassword,
    car_id: i64,
    source_key: &str,
    new_import_source: bool,
    limits: teslatlas_hub::source_db_reader::SourceDbReadLimits,
    scope: MigrationCaptureScope,
    progress: SourceDbMigrationProgressReporter,
) -> Result<SourceDbImportReport, Box<dyn std::error::Error>> {
    tracing::info!(
        host = source.host(),
        port = source.port(),
        database = source.database_name(),
        car_id,
        "starting source database read-only snapshot import"
    );
    let imported_at_ms = current_epoch_ms()?;
    let request = SourceDbImportRequest {
        new_import_source,
        source_key: source_key.to_owned(),
        scope: SourceDbImportScope::Selected(car_id),
        imported_at_ms,
    };
    let selected = match scope {
        MigrationCaptureScope::HistorySnapshot => {
            import_selected_history_snapshot_from_postgres_with_schema_22_and_progress(
                store,
                source,
                postgres_password,
                cursor_key,
                &request,
                limits,
                progress,
            )
            .await?
        }
        MigrationCaptureScope::SettledWithoutCredentials => {
            import_selected_from_postgres_with_schema_22_and_progress(
                store,
                source,
                postgres_password,
                cursor_key,
                &request,
                limits,
                progress,
            )
            .await?
        }
    };
    Ok(selected.import)
}

#[cfg(unix)]
fn should_emit_migration_progress(online_snapshot: bool) -> bool {
    online_snapshot
}

#[cfg(unix)]
fn migration_progress_reporter(online_snapshot: bool) -> SourceDbMigrationProgressReporter {
    if !should_emit_migration_progress(online_snapshot) {
        return SourceDbMigrationProgressReporter::default();
    }
    let phase = std::sync::Mutex::new(None);
    let started = Instant::now();
    SourceDbMigrationProgressReporter::new(move |event: SourceDbMigrationProgressEvent| {
        use teslatlas_hub::runtime::development_event_log as dev;
        let mut previous = phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *previous != Some(event.phase) {
            *previous = Some(event.phase);
            let mut record = dev::Event::new(dev::Kind::Import, dev::Outcome::Progress);
            record.phase = Some(match event.phase {
                SourceDbMigrationPhase::Counting => dev::Phase::Counting,
                SourceDbMigrationPhase::Metadata => dev::Phase::Metadata,
                SourceDbMigrationPhase::RelatedPositions => dev::Phase::RelatedPositions,
                SourceDbMigrationPhase::Positions => dev::Phase::Positions,
                SourceDbMigrationPhase::Charges => dev::Phase::Charges,
                SourceDbMigrationPhase::Finalizing => dev::Phase::Finalizing,
                SourceDbMigrationPhase::Complete => dev::Phase::Complete,
            });
            record.rows = event.completed_rows;
            record.total_rows = event.total_rows;
            record.duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            dev::record(record);
        }
        drop(previous);
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        if let Err(error) = write_migration_progress_event(&mut output, &event) {
            tracing::warn!(%error, "could not write source database migration progress");
        }
    })
}

#[cfg(unix)]
fn write_migration_progress_event(
    writer: &mut impl Write,
    event: &SourceDbMigrationProgressEvent,
) -> Result<(), Box<dyn std::error::Error>> {
    serde_json::to_writer(&mut *writer, event)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(unix)]
fn elapsed_migration_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(unix)]
fn read_migration_secret(
    path: &Path,
    maximum: usize,
) -> Result<zeroize::Zeroizing<Vec<u8>>, Box<dyn std::error::Error>> {
    if path == Path::new("-") {
        return read_bounded_migration_secret(std::io::stdin(), maximum).map_err(Into::into);
    }
    read_migration_secret_file(path, maximum).map_err(Into::into)
}

#[cfg(unix)]
fn read_recovery_encryption_key(
    path: &Path,
) -> Result<zeroize::Zeroizing<[u8; RECOVERY_ENCRYPTION_KEY_BYTES]>, Box<dyn std::error::Error>> {
    let bytes = read_migration_secret(path, RECOVERY_ENCRYPTION_KEY_BYTES)?;
    if bytes.len() != RECOVERY_ENCRYPTION_KEY_BYTES {
        return Err(format!(
            "credential-recovery encryption key must be exactly {RECOVERY_ENCRYPTION_KEY_BYTES} bytes"
        )
        .into());
    }
    let mut key = zeroize::Zeroizing::new([0_u8; RECOVERY_ENCRYPTION_KEY_BYTES]);
    key.copy_from_slice(&bytes);
    Ok(key)
}

#[cfg(all(unix, test))]
fn read_migration_encryption_key(
    path: &Path,
) -> Result<zeroize::Zeroizing<Vec<u8>>, Box<dyn std::error::Error>> {
    let mut key = read_migration_secret(path, MAX_MIGRATION_ENCRYPTION_KEY_BYTES)?;
    if key.last() == Some(&b'\n') {
        key.pop();
        if key.last() == Some(&b'\r') {
            key.pop();
        }
    }
    Ok(key)
}

// TRANSITIONAL (M02-T02): the source-key file readers below are unreachable
// from any command; they remain only for the entry tests that T10 retires.
#[cfg(all(unix, test))]
#[derive(Debug)]
struct SourceKeyFileRefusal { spelling: &'static str, entry: bool }
#[cfg(all(unix, test))]
impl std::fmt::Display for SourceKeyFileRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.entry {
            write!(f, "source_key_file_location_refused: The key file given to {} is a link, is not a regular file, or changed while the Hub opened it, so the Hub did not read it. Give a regular file, or pass the key on standard input with {} -.", self.spelling, self.spelling)
        } else {
            write!(f, "source_key_file_location_refused: The key file given to {} is inside a folder the Hub manages (its data, configuration or import-session folder), so the Hub did not read it. Copy the key to another folder, or pass it on standard input with {} -.", self.spelling, self.spelling)
        }
    }
}
#[cfg(all(unix, test))]
impl std::error::Error for SourceKeyFileRefusal {}
#[cfg(all(unix, test))]
fn source_key_location_refused() -> Box<dyn std::error::Error> {
    Box::new(SourceKeyFileRefusal { spelling: "--source-token-key-file", entry: false })
}
#[cfg(all(unix, test))]
fn source_key_entry_refused() -> Box<dyn std::error::Error> {
    Box::new(SourceKeyFileRefusal { spelling: "--source-token-key-file", entry: true })
}

#[cfg(all(unix, test))]
fn prepare_source_key_files_with(
    primary: Option<&Path>, deprecated: Option<&Path>, notice: &mut impl Write,
    mut read: impl FnMut(&Path, &'static str) -> Result<zeroize::Zeroizing<Vec<u8>>, Box<dyn std::error::Error>>,
) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, Box<dyn std::error::Error>> {
    if deprecated.is_some() { writeln!(notice, "--encryption-key-file is deprecated; use --source-token-key-file")?; }
    let mut key = match (primary, deprecated) {
        (None, None) => return Ok(None),
        (Some(path), None) => read(path, "--source-token-key-file")?,
        (None, Some(path)) => read(path, "--encryption-key-file")?,
        (Some(primary), Some(deprecated)) => {
            let bytes = read(primary, "--source-token-key-file")?;
            if primary != deprecated {
                let other = read(deprecated, "--encryption-key-file")?;
                if bytes.as_slice() != other.as_slice() { return Err("conflicting_source_key_files".into()); }
            }
            bytes
        }
    };
    if key.last() == Some(&b'\n') { key.pop(); if key.last() == Some(&b'\r') { key.pop(); } }
    Ok(Some(key))
}

/// R54: the device and inode of every Hub-managed directory (the data
/// directory, its `secrets/` and the config file's parent), each opened and
/// identified at its physical location.
#[cfg(all(unix, test))]
fn managed_directory_identities(data_dir: &Path, config_path: &Path) -> Vec<(u64, u64)> {
    use rustix::fs::{Mode, OFlags, fstat, open};
    let config_parent = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    [
        data_dir.to_path_buf(),
        data_dir.join("secrets"),
        config_parent.to_path_buf(),
    ]
    .iter()
    .filter_map(|directory| {
        let descriptor = open(
            directory,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()?;
        let facts = fstat(&descriptor).ok()?;
        Some((facts.st_dev as u64, facts.st_ino as u64))
    })
    .collect()
}

/// R54: open the key path's containing directory through its path (ancestor
/// links lead to the physical directory) and refuse it when it, or any
/// directory above it reached through `..`, is Hub-managed. No I/O happens on
/// the named entry.
#[cfg(all(unix, test))]
fn open_checked_key_directory(
    path: &Path,
    data_dir: &Path,
    config_path: &Path,
) -> Result<(std::os::fd::OwnedFd, std::ffi::OsString), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    let session_root = Some(Path::new("/tmp"));
    #[cfg(not(target_os = "macos"))]
    let session_root = None;
    open_checked_key_directory_with_session_root(path, data_dir, config_path, session_root)
}

#[cfg(all(unix, test))]
fn open_checked_key_directory_with_session_root(
    path: &Path,
    data_dir: &Path,
    config_path: &Path,
    session_root: Option<&Path>,
) -> Result<(std::os::fd::OwnedFd, std::ffi::OsString), Box<dyn std::error::Error>> {
    use rustix::fs::{Mode, OFlags, fstat, open, openat};
    let mut managed = managed_directory_identities(data_dir, config_path);
    let directory_path = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(source_key_location_refused)?
        .to_owned();
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let directory =
        open(directory_path, flags, Mode::empty()).map_err(|_| source_key_location_refused())?;
    if let Some(session_root) = session_root {
        if let Some(identity) = import_session_directory_identity(directory_path, &directory, session_root)? {
            managed.push(identity);
        }
    }
    let mut current = directory
        .try_clone()
        .map_err(|_| source_key_location_refused())?;
    loop {
        let facts = fstat(&current).map_err(|_| source_key_location_refused())?;
        let identity = (facts.st_dev as u64, facts.st_ino as u64);
        if managed.contains(&identity) {
            return Err(source_key_location_refused());
        }
        let parent = openat(&current, "..", flags, Mode::empty())
            .map_err(|_| source_key_location_refused())?;
        let parent_facts = fstat(&parent).map_err(|_| source_key_location_refused())?;
        if (parent_facts.st_dev as u64, parent_facts.st_ino as u64) == identity {
            break;
        }
        current = parent;
    }
    Ok((directory, name))
}

/// The landed GUI creates `th-<UUID>` directly under its physical temporary
/// root. Identify that directory, including when the key is in a descendant,
/// without inspecting the key entry or scanning other import sessions.
#[cfg(all(unix, test))]
fn import_session_directory_identity(
    directory_path: &Path,
    directory: &std::os::fd::OwnedFd,
    session_root: &Path,
) -> Result<Option<(u64, u64)>, Box<dyn std::error::Error>> {
    let physical_root = fs::canonicalize(session_root).map_err(|_| source_key_location_refused())?;
    let physical_directory = fs::canonicalize(directory_path).map_err(|_| source_key_location_refused())?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let physical = open(&physical_directory, flags, Mode::empty()).map_err(|_| source_key_location_refused())?;
    let held = fstat(directory).map_err(|_| source_key_location_refused())?;
    let resolved = fstat(&physical).map_err(|_| source_key_location_refused())?;
    if (held.st_dev, held.st_ino) != (resolved.st_dev, resolved.st_ino) { return Err(source_key_location_refused()); }
    let Ok(relative) = physical_directory.strip_prefix(&physical_root) else { return Ok(None); };
    let Some(std::path::Component::Normal(name)) = relative.components().next() else { return Ok(None); };
    let Some(name_text) = name.to_str() else { return Ok(None); };
    let Some(identifier) = name_text.strip_prefix("th-") else { return Ok(None); };
    if identifier.len() != 36 || uuid::Uuid::parse_str(identifier).is_err() { return Ok(None); }
    let root = open(&physical_root, flags, Mode::empty()).map_err(|_| source_key_location_refused())?;
    let session = rustix::fs::openat(&root, name, flags, Mode::empty()).map_err(|_| source_key_location_refused())?;
    let facts = fstat(&session).map_err(|_| source_key_location_refused())?;
    Ok(Some((facts.st_dev as u64, facts.st_ino as u64)))
}

/// R54: an explicit key file is admitted by its physical location. The entry is
/// classified relative to the held directory without following a link, opened
/// with `O_NOFOLLOW`, its device and inode must match the classification, the
/// cap is checked on the descriptor before the read, and it is read once.
#[cfg(all(unix, test))]
fn read_source_token_key_checked(
    path: &Path,
    data_dir: &Path,
    config_path: &Path,
) -> Result<zeroize::Zeroizing<Vec<u8>>, Box<dyn std::error::Error>> {
    let mut key = read_source_token_key_raw_with(path, data_dir, config_path, || {})?;
    if key.last() == Some(&b'\n') { key.pop(); if key.last() == Some(&b'\r') { key.pop(); } }
    Ok(key)
}

#[cfg(all(unix, test))]
fn read_source_token_key_raw_with(
    path: &Path,
    data_dir: &Path,
    config_path: &Path,
    after_classification: impl FnOnce(),
) -> Result<zeroize::Zeroizing<Vec<u8>>, Box<dyn std::error::Error>> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, openat, statat};
    if path == Path::new("-") {
        return read_bounded_migration_secret(std::io::stdin(), MAX_MIGRATION_ENCRYPTION_KEY_BYTES).map_err(Into::into);
    }
    let (directory, name) = open_checked_key_directory(path, data_dir, config_path)?;
    let classified = statat(&directory, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| source_key_entry_refused())?;
    if FileType::from_raw_mode(classified.st_mode) != FileType::RegularFile {
        return Err(source_key_entry_refused());
    }
    after_classification();
    let descriptor = openat(
        &directory,
        name.as_os_str(),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| source_key_entry_refused())?;
    let opened = fstat(&descriptor).map_err(|_| source_key_entry_refused())?;
    if FileType::from_raw_mode(opened.st_mode) != FileType::RegularFile
        || opened.st_dev != classified.st_dev
        || opened.st_ino != classified.st_ino
    {
        return Err(source_key_entry_refused());
    }
    if usize::try_from(opened.st_size)
        .map_or(true, |size| size > MAX_MIGRATION_ENCRYPTION_KEY_BYTES)
    {
        return Err("the source application's ENCRYPTION_KEY is too large".into());
    }
    let mut key = zeroize::Zeroizing::new(Vec::new());
    std::io::Read::read_to_end(
        &mut std::io::Read::take(
            std::fs::File::from(descriptor),
            u64::try_from(MAX_MIGRATION_ENCRYPTION_KEY_BYTES + 1).unwrap_or(u64::MAX),
        ),
        &mut key,
    )?;
    if key.len() > MAX_MIGRATION_ENCRYPTION_KEY_BYTES {
        return Err("the source application's ENCRYPTION_KEY is too large".into());
    }
    Ok(key)
}

#[cfg(unix)]
fn read_migration_postgres_password(
    path: &Path,
) -> Result<SourceDbPassword, Box<dyn std::error::Error>> {
    let bytes = read_migration_secret(path, MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES)?;
    SourceDbPassword::from_bytes(&bytes).map_err(Into::into)
}

#[cfg(unix)]
fn read_bounded_migration_secret(
    reader: impl Read,
    maximum: usize,
) -> Result<zeroize::Zeroizing<Vec<u8>>, MigrationSecretReadError> {
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(maximum + 1));
    reader
        .take(u64::try_from(maximum + 1).expect("secret cap fits u64"))
        .read_to_end(&mut bytes)
        .map_err(|_| MigrationSecretReadError::Read)?;
    if bytes.len() > maximum {
        return Err(MigrationSecretReadError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn read_migration_secret_file(
    path: &Path,
    maximum: usize,
) -> Result<zeroize::Zeroizing<Vec<u8>>, MigrationSecretReadError> {
    read_migration_secret_file_with_hooks(path, maximum, || {}, || {})
}

#[cfg(unix)]
fn read_migration_secret_file_with_hooks(
    path: &Path,
    maximum: usize,
    after_open: impl FnOnce(),
    after_read: impl FnOnce(),
) -> Result<zeroize::Zeroizing<Vec<u8>>, MigrationSecretReadError> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| {
        if error == Errno::LOOP {
            MigrationSecretReadError::UnsafeFile
        } else {
            MigrationSecretReadError::Read
        }
    })?;
    let initial = fstat(&descriptor).map_err(|_| MigrationSecretReadError::Read)?;
    if !safe_migration_secret_stat(&initial) {
        return Err(MigrationSecretReadError::UnsafeFile);
    }
    let flags = fcntl_getfl(&descriptor).map_err(|_| MigrationSecretReadError::Read)?;
    fcntl_setfl(&descriptor, flags & !OFlags::NONBLOCK)
        .map_err(|_| MigrationSecretReadError::Read)?;
    after_open();

    let file: fs::File = descriptor.into();
    let bytes = read_bounded_migration_secret(&file, maximum)?;
    after_read();
    let final_descriptor = fstat(&file).map_err(|_| MigrationSecretReadError::Read)?;
    if !same_migration_secret_stat(&initial, &final_descriptor) {
        return Err(MigrationSecretReadError::IdentityChanged);
    }
    let current =
        fs::symlink_metadata(path).map_err(|_| MigrationSecretReadError::IdentityChanged)?;
    if current.file_type().is_symlink()
        || !current.file_type().is_file()
        || current.uid() != initial.st_uid
        || current.dev() != initial.st_dev as u64
        || current.ino() != initial.st_ino
        || current.mode() != initial.st_mode as u32
        || current.len() != initial.st_size as u64
        || current.mtime() != initial.st_mtime
        || current.mtime_nsec() != initial.st_mtime_nsec as i64
        || !safe_migration_secret_metadata(&current)
    {
        return Err(MigrationSecretReadError::IdentityChanged);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn safe_migration_secret_stat(stat: &rustix::fs::Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode).is_file()
        && stat.st_uid == getuid().as_raw()
        && (stat.st_mode & 0o077) == 0
        && (stat.st_mode & 0o400) != 0
}

#[cfg(unix)]
fn same_migration_secret_stat(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_uid == right.st_uid
        && left.st_mode == right.st_mode
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
}

#[cfg(unix)]
fn safe_migration_secret_metadata(metadata: &fs::Metadata) -> bool {
    metadata.uid() == getuid().as_raw()
        && (metadata.mode() & 0o077) == 0
        && (metadata.mode() & 0o400) != 0
}

#[cfg(unix)]
fn migration_source_key<'a>(explicit: Option<&'a str>, configured: Option<&'a str>) -> &'a str {
    explicit
        .or(configured)
        .unwrap_or(teslatlas_hub::db::DEFAULT_IMPORT_SOURCE_KEY)
}
