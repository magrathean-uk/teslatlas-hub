// SPDX-License-Identifier: AGPL-3.0-only

#[cfg(unix)]
fn collector_can_start(
    store: &HubStore,
    config: &HubConfig,
) -> Result<bool, Box<dyn std::error::Error>> {
    let credentials = tesatlas_hub::auth::credential_sign_in::credential_status(store, &config.data_dir, current_epoch_ms()?)?;
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
            .join("Tesatlas Hub")
            .join("config.toml");
    }
    #[cfg(target_os = "linux")]
    return PathBuf::from("/etc/tesatlas-hub/config.toml");
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
    tesatlas_hub::source_token_envelope::MAX_TOKEN_PLAINTEXT_BYTES;
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
        "next": "open Tesatlas Hub.app and choose Start Hub",
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
        return Err(tesatlas_hub::auth::credential_sign_in::source_credential_disabled().into());
    }
    if history_only && (!online_snapshot || preserve_existing_credentials) {
        return Err(
            "history-only import requires --online-snapshot and no Tesla credential flags".into(),
        );
    }

    let config = HubConfig::load(config_path)?;
    let import_source_key =
        migration_source_key(source_key, config.import_source.source_key.as_deref());
    tesatlas_hub::config::validate_import_source_key(import_source_key)?;
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
    // Admit the source and the selected car before any Hub target state
    // exists: a refused source leaves no store, credential vault or cursor
    // key behind. The capture admits the source again in its own snapshot.
    check_source_history_compatibility(&source, &postgres_password, car_id, limits).await?;
    if history_only {
        let store = HubStore::initialize(&config.data_dir)?;
        let _entry = tesatlas_hub::open_credential_vault_for_entry(store.clone(), std::sync::Arc::clone(admission), &config.data_dir)
            .map_err(tesatlas_hub::auth::credential_sign_in::credential_entry_error)?;
        let mut catalogue_checkpoint = CatalogueCheckpointGuard::new(store.clone());
        store.check_import_source_key(import_source_key, new_import_source)?;
        let cursor_key = load_or_create_cursor_key(&config.data_dir)?;
        let progress = migration_progress_reporter(true);
        let (report, summary) = import_direct_migration_snapshot(
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
                "schemaEvidence": "v4_4_compatible_schema",
                "selectedCarId": car_id,
                "projectedRows": report.projected_rows,
                "brokenDrives": report.projection.broken_drives,
                "skippedOpenDrives": report.projection.skipped_open_drives,
                "heldOpenSessionPositions": report.projection.held_open_session_positions,
                "detachedPositions": report.projection.detached_positions,
                "snapshotId": report.snapshot_id,
                "sequence": report.sequence,
                "cutoverUnsettled": report.cutover_unsettled,
                "importSummary": summary.to_json(),
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
    let _entry = tesatlas_hub::open_credential_vault_for_entry(store.clone(), std::sync::Arc::clone(admission), &config.data_dir)
        .map_err(tesatlas_hub::auth::credential_sign_in::credential_entry_error)?;
    let mut catalogue_checkpoint = CatalogueCheckpointGuard::new(store.clone());
    store.check_import_source_key(import_source_key, new_import_source)?;
    let cursor_key = load_or_create_cursor_key(&config.data_dir)?;
    if !online_snapshot {
        let initial_progress = migration_progress_reporter(online_snapshot);
        let initial_copy_started = Instant::now();
        let (initial_report, _) = import_direct_migration_snapshot(
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
                "brokenDrives": initial_report.projection.broken_drives,
                "heldOpenSessionPositions": initial_report.projection.held_open_session_positions,
                "skippedOpenDrives": initial_report.projection.skipped_open_drives,
                "detachedPositions": initial_report.projection.detached_positions,
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
    let (report, summary) = import_direct_migration_snapshot(
        &store,
        &cursor_key,
        &source,
        &postgres_password,
        car_id,
        import_source_key,
        new_import_source,
        limits,
        final_migration_capture_scope(online_snapshot),
        progress.clone(),
    )
    .await?;
    tracing::info!(
        duration_ms = elapsed_migration_millis(final_copy_started),
        "finished final source database migration copy"
    );
    progress.advance_finalizing(1, 2);

    let fleet_still_present = tesatlas_hub::auth::credential_sign_in::credential_status(&store, &config.data_dir, current_epoch_ms()?)?.fleet_credentials.present;
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
                "brokenDrives": report.projection.broken_drives,
                "heldOpenSessionPositions": report.projection.held_open_session_positions,
                "skippedOpenDrives": report.projection.skipped_open_drives,
                "detachedPositions": report.projection.detached_positions,
            "snapshotId": report.snapshot_id,
            "sequence": report.sequence,
            "cutoverUnsettled": report.cutover_unsettled,
            "importSummary": summary.to_json(),
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

    print!("Start Tesatlas Hub now? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let start_hub = migration_start_requested(&answer);

    Ok(start_hub)
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationCaptureScope {
    HistorySnapshot,
    SettledWithoutCredentials,
}

/// The final copy of an online snapshot reads a source that is still
/// recording, so it accepts one coherent snapshot of an advancing source. Only
/// the stopped-source cutover requires a settled source.
#[cfg(unix)]
fn final_migration_capture_scope(online_snapshot: bool) -> MigrationCaptureScope {
    if online_snapshot {
        MigrationCaptureScope::HistorySnapshot
    } else {
        MigrationCaptureScope::SettledWithoutCredentials
    }
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
    limits: tesatlas_hub::source_db_reader::SourceDbReadLimits,
    scope: MigrationCaptureScope,
    progress: SourceDbMigrationProgressReporter,
) -> Result<(SourceDbImportReport, SourceDbImportSummary), Box<dyn std::error::Error>> {
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
    Ok((selected.import, selected.summary))
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
        use tesatlas_hub::runtime::development_event_log as dev;
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
        .unwrap_or(tesatlas_hub::db::DEFAULT_IMPORT_SOURCE_KEY)
}

#[cfg(all(unix, test))]
mod final_capture_scope_tests {
    use super::*;

    #[test]
    fn online_snapshot_final_copy_accepts_an_advancing_source() {
        assert_eq!(
            final_migration_capture_scope(true),
            MigrationCaptureScope::HistorySnapshot
        );
    }

    #[test]
    fn stopped_source_final_copy_still_requires_a_settled_source() {
        assert_eq!(
            final_migration_capture_scope(false),
            MigrationCaptureScope::SettledWithoutCredentials
        );
    }
}

#[cfg(all(unix, test))]
mod source_admission_order_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn listing(directory: &Path) -> Vec<std::ffi::OsString> {
        let mut names: Vec<_> = fs::read_dir(directory)
            .expect("listing")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        names.sort();
        names
    }

    /// A source that refuses the connection (nothing listens on the loopback
    /// discard port) is reported before the store, the credential vault or
    /// the cursor key of a fresh target is created.
    #[tokio::test]
    async fn refused_source_creates_no_hub_target_state() {
        for history_only in [true, false] {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let data_dir = temporary.path().join("data");
            fs::create_dir(&data_dir).expect("data directory");
            fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700))
                .expect("private data directory");
            let config_path = temporary.path().join("config.toml");
            fs::write(
                &config_path,
                format!(
                    "data_dir = {data_dir:?}\nbind = '127.0.0.1:18443'\n[collector]\ninterval_seconds = 0\n[collector.legacy_auth]\nenabled = false\n"
                ),
            )
            .expect("configuration");
            let password_path = temporary.path().join("password");
            fs::write(&password_path, "synthetic-password").expect("password");
            fs::set_permissions(&password_path, fs::Permissions::from_mode(0o600))
                .expect("private password");
            let admission = AdmittedUserHub::admit(&data_dir).expect("admission");
            let before = listing(&data_dir);
            run_macos_migration(
                &admission,
                MacMigrationInput {
                    config_path: &config_path,
                    source_url: "postgresql://reader@127.0.0.1:9/sourcedb",
                    car_id: 42,
                    source_key: None,
                    new_import_source: true,
                    postgres_password_file: &password_path,
                    history_only,
                    encryption_key_file: None,
                    access_token_file: None,
                    refresh_token_file: None,
                    online_snapshot: true,
                    preserve_existing_credentials: false,
                },
            )
            .await
            .expect_err("the source refuses the connection");
            assert_eq!(listing(&data_dir), before, "history_only = {history_only}");
            assert!(!data_dir.join("hub.sqlite").exists());
            assert!(!tesatlas_hub::legacy_owner_credentials::cursor_key_path(&data_dir).exists());
        }
    }
}
