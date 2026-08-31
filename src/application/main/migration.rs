// SPDX-License-Identifier: AGPL-3.0-only

#[cfg(unix)]
fn command_requires_user_hub_admission(command: &Command) -> bool {
    matches!(
        command,
        Command::Init
            | Command::Bootstrap
            | Command::Setup { .. }
            | Command::SetupFleet { .. }
            | Command::ConfigureFleetTelemetry
            | Command::Serve
            | Command::Observe { .. }
            | Command::Migrate { .. }
            | Command::Pair { .. }
            | Command::Repair
            | Command::Backup { .. }
            | Command::ExportRecoveryCredentials { .. }
            | Command::RestoreRecoveryCredentials { .. }
    )
}

fn collector_can_start(
    store: &HubStore,
    config: &HubConfig,
) -> Result<bool, Box<dyn std::error::Error>> {
    let credentials_present = match config.collector.provider {
        CollectorProvider::Legacy => store.load_teslamate_legacy_tokens()?.is_some(),
        CollectorProvider::Fleet => store.load_fleet_tokens()?.is_some(),
    };
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
    postgres_password_file: &'a Path,
    encryption_key_file: Option<&'a Path>,
    access_token_file: Option<&'a Path>,
    refresh_token_file: Option<&'a Path>,
    online_snapshot: bool,
    preserve_existing_credentials: bool,
}

#[cfg(unix)]
const MAX_MIGRATION_POSTGRES_PASSWORD_BYTES: usize = 4 * 1024;
#[cfg(unix)]
const MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES: usize = MAX_MIGRATION_POSTGRES_PASSWORD_BYTES + 2;
#[cfg(unix)]
const MAX_MIGRATION_TOKEN_BYTES: usize =
    teslatlas_hub::teslamate_token::MAX_LEGACY_TOKEN_PLAINTEXT_BYTES;
#[cfg(unix)]
const MAX_MIGRATION_TOKEN_FILE_BYTES: usize = MAX_MIGRATION_TOKEN_BYTES + 2;
#[cfg(unix)]
const MAX_SETUP_TOKENS_STDIN_BYTES: usize = MAX_MIGRATION_TOKEN_BYTES * 2 + 128;
#[cfg(unix)]
const MAX_SETUP_FLEET_STDIN_BYTES: usize = MAX_MIGRATION_TOKEN_BYTES * 2 + 1_024;
#[cfg(unix)]
const MAX_MIGRATION_ENCRYPTION_KEY_BYTES: usize = 16 * 1024;

#[cfg(unix)]
#[derive(serde::Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetupTokensStdin {
    access_token: String,
    refresh_token: String,
}

#[cfg(unix)]
#[derive(serde::Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetupFleetStdin {
    access_token: String,
    refresh_token: String,
    client_id: String,
    #[zeroize(skip)]
    region: FleetRegion,
    #[zeroize(skip)]
    expires_in_seconds: u64,
}

#[cfg(unix)]
fn migration_stop_confirmed(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y")
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
async fn run_macos_migration(
    admission: &AdmittedUserHub,
    MacMigrationInput {
        config_path,
        source_url,
        car_id,
        postgres_password_file,
        encryption_key_file,
        access_token_file,
        refresh_token_file,
        online_snapshot,
        preserve_existing_credentials,
    }: MacMigrationInput<'_>,
) -> Result<bool, Box<dyn std::error::Error>> {
    if car_id <= 0 {
        return Err("--car-id must be a positive TeslaMate car id".into());
    }
    let secret_paths = std::iter::once(Some(postgres_password_file))
        .chain([encryption_key_file, access_token_file, refresh_token_file])
        .flatten()
        .collect::<Vec<_>>();
    if secret_paths
        .iter()
        .filter(|path| **path == Path::new("-"))
        .count()
        > 1
    {
        return Err("only one migration secret may be read from stdin".into());
    }

    let config = HubConfig::load(config_path)?;
    admission.assert_sensitive_access()?;
    admission.assert_store_path(&config.data_dir)?;
    let source = ReadOnlySource::parse(source_url)?;
    let postgres_password = read_migration_postgres_password(postgres_password_file)?;
    let mut limits = config.teslamate.read_limits()?;
    let profile = derive_effective_import_profile(
        limits.parallel_copy_lanes,
        &config.teslamate.performance_profile,
        &config.data_dir,
    )?;
    limits.parallel_copy_lanes = profile.parallel_copy_lanes;
    let copy_teslamate_ciphertext = match (
        encryption_key_file,
        access_token_file,
        refresh_token_file,
    ) {
        (Some(_), None, None) => true,
        (None, Some(_), Some(_)) => false,
        _ => {
            return Err(
                "provide --encryption-key-file, or both --access-token-file and --refresh-token-file"
                    .into(),
            );
        }
    };

    let store = HubStore::initialize(&config.data_dir)?;
    let preserve_existing_credentials = should_preserve_existing_migration_credentials(
        preserve_existing_credentials,
        store.load_teslamate_legacy_tokens()?.is_some(),
    );
    let capture_teslamate_ciphertext =
        copy_teslamate_ciphertext && !preserve_existing_credentials;
    let mut catalogue_checkpoint = CatalogueCheckpointGuard::new(store.clone());
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
            limits,
            false,
            initial_progress,
        )
        .await?;
        tracing::info!(
            duration_ms = elapsed_migration_millis(initial_copy_started),
            "finished initial TeslaMate migration copy"
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
        print!("Stop TeslaMate now. Confirm it is stopped before final copy [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !migration_stop_confirmed(&answer) {
            return Err(
                "TeslaMate stop was not confirmed; final migration capture was not started".into(),
            );
        }
    }

    // Cutover mode re-captures after the operator stops TeslaMate. Online mode
    // performs this once while TeslaMate remains live. In both modes history
    // and source ciphertext, when selected, share this exact source snapshot.
    let progress = migration_progress_reporter(online_snapshot);
    let final_copy_started = Instant::now();
    let (report, captured_ciphertexts) = import_direct_migration_snapshot(
        &store,
        &cursor_key,
        &source,
        &postgres_password,
        car_id,
        limits,
        capture_teslamate_ciphertext,
        progress.clone(),
    )
    .await?;
    tracing::info!(
        duration_ms = elapsed_migration_millis(final_copy_started),
        "finished final TeslaMate migration copy"
    );
    progress.advance_finalizing(1, 2);

    let fleet_still_present = store.load_fleet_tokens()?.is_some();
    let imported_ciphertext_bytes = if preserve_existing_credentials {
        if captured_ciphertexts.is_some() {
            return Err("history-only migration unexpectedly retained source credentials".into());
        }
        tracing::info!("preserved existing Hub credentials during TeslaMate history import");
        None
    } else {
        // The encrypted source pair came from the same final snapshot as history.
        let (encryption_key, access_ciphertext, refresh_ciphertext) =
            if copy_teslamate_ciphertext {
                let key_path = encryption_key_file.expect("validated encrypted-token input");
                let key = read_migration_encryption_key(key_path)?;
                if key.is_empty() {
                    return Err("TeslaMate ENCRYPTION_KEY is empty".into());
                }
                let ciphertexts = captured_ciphertexts
                    .ok_or("final migration snapshot did not retain TeslaMate credentials")?;
                // Validate compatibility without exposing either plaintext token.
                drop(decrypt_legacy_owner_tokens(
                    &key,
                    &ciphertexts.access,
                    &ciphertexts.refresh,
                )?);
                let (access, refresh) = ciphertexts.into_parts();
                (key, access, refresh)
            } else {
                let access_path = access_token_file.expect("validated access-token input");
                let refresh_path = refresh_token_file.expect("validated refresh-token input");
                let key = random_encryption_key()?;
                let (access, refresh) = encrypt_legacy_owner_token_files(
                    &key,
                    read_migration_secret(access_path, MAX_MIGRATION_TOKEN_FILE_BYTES)?,
                    read_migration_secret(refresh_path, MAX_MIGRATION_TOKEN_FILE_BYTES)?,
                )?;
                (key, access, refresh)
            };
        let stored = TeslaMateLegacyTokenStore::imported(access_ciphertext, refresh_ciphertext)?;
        persist_migrated_legacy_tokens(&config.data_dir, &store, &encryption_key, &stored).map_err(
            |error| migration_outcome_ambiguous("persisting imported credentials", error),
        )?;
        Some((stored.access().len(), stored.refresh().len()))
    };
    progress.advance_finalizing(3, 4);
    tracing::info!(
        selected_car_id = car_id,
        projected_rows = report.projected_rows,
        fleet_tokens_preserved = fleet_still_present,
        existing_legacy_credentials_preserved = preserve_existing_credentials,
        "TeslaMate history imported; source PostgreSQL was not written; Fleet tokens were not deleted"
    );

    let checkpoint_started = Instant::now();
    catalogue_checkpoint
        .finish()
        .map_err(|error| migration_outcome_ambiguous("checkpointing imported catalogue", error))?;
    tracing::info!(
        duration_ms = elapsed_migration_millis(checkpoint_started),
        "checkpointed imported TeslaMate catalogue"
    );
    progress.complete(TeslaMateMigrationPhase::Complete);

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
            "existingCredentialsPreserved": preserve_existing_credentials,
            "accessCiphertextBytes": imported_ciphertext_bytes.map_or(0, |value| value.0),
            "refreshCiphertextBytes": imported_ciphertext_bytes.map_or(0, |value| value.1),
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
#[allow(clippy::too_many_arguments)]
async fn import_direct_migration_snapshot(
    store: &HubStore,
    cursor_key: &CursorKey,
    source: &ReadOnlySource,
    postgres_password: &TeslaMatePostgresPassword,
    car_id: i64,
    limits: teslatlas_hub::teslamate_reader::TeslaMateReadLimits,
    include_legacy_token: bool,
    progress: TeslaMateMigrationProgressReporter,
) -> Result<
    (
        TeslaMateImportReport,
        Option<TeslaMateLegacyTokenCiphertexts>,
    ),
    Box<dyn std::error::Error>,
> {
    tracing::info!(
        host = source.host(),
        port = source.port(),
        database = source.database_name(),
        car_id,
        include_legacy_token,
        "starting TeslaMate read-only snapshot import"
    );
    let imported_at_ms = current_epoch_ms()?;
    let request = TeslaMateImportRequest {
        source_key: "teslamate".to_owned(),
        scope: TeslaMateImportScope::Selected(car_id),
        imported_at_ms,
    };
    if include_legacy_token {
        let (selected, tokens) =
            import_selected_from_postgres_with_schema_22_and_legacy_token_and_progress(
            store,
            source,
            postgres_password,
            cursor_key,
            &request,
            limits,
            progress,
        )
        .await?;
        Ok((selected.import, Some(tokens)))
    } else {
        let selected = import_selected_from_postgres_with_schema_22_and_progress(
            store,
            source,
            postgres_password,
            cursor_key,
            &request,
            limits,
            progress,
        )
        .await?;
        Ok((selected.import, None))
    }
}

#[cfg(unix)]
fn should_emit_migration_progress(online_snapshot: bool) -> bool {
    online_snapshot
}

#[cfg(unix)]
fn should_preserve_existing_migration_credentials(
    preserve_requested: bool,
    existing_credentials_present: bool,
) -> bool {
    preserve_requested && existing_credentials_present
}

#[cfg(unix)]
fn migration_progress_reporter(online_snapshot: bool) -> TeslaMateMigrationProgressReporter {
    if !should_emit_migration_progress(online_snapshot) {
        return TeslaMateMigrationProgressReporter::default();
    }
    TeslaMateMigrationProgressReporter::new(|event: TeslaMateMigrationProgressEvent| {
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        if let Err(error) = write_migration_progress_event(&mut output, &event) {
            tracing::warn!(%error, "could not write TeslaMate migration progress");
        }
    })
}

#[cfg(unix)]
fn write_migration_progress_event(
    writer: &mut impl Write,
    event: &TeslaMateMigrationProgressEvent,
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
fn read_setup_tokens_from_stdin() -> Result<OwnerTokens, Box<dyn std::error::Error>> {
    let bytes = read_bounded_migration_secret(std::io::stdin(), MAX_SETUP_TOKENS_STDIN_BYTES)?;
    let mut input: SetupTokensStdin = serde_json::from_slice(&bytes)?;
    OwnerTokens::from_file_bytes(
        zeroize::Zeroizing::new(std::mem::take(&mut input.access_token).into_bytes()),
        zeroize::Zeroizing::new(std::mem::take(&mut input.refresh_token).into_bytes()),
    )
    .map_err(Into::into)
}

#[cfg(unix)]
fn decode_setup_fleet_stdin(
    bytes: &[u8],
) -> Result<FleetSetupCredentials, Box<dyn std::error::Error>> {
    let mut input: SetupFleetStdin = serde_json::from_slice(bytes)?;
    let credentials = FleetSetupCredentials::new(
        std::mem::take(&mut input.access_token),
        std::mem::take(&mut input.refresh_token),
        std::mem::take(&mut input.client_id),
        input.region,
        input.expires_in_seconds,
    )?;
    credentials.require_collection_scopes()?;
    Ok(credentials)
}

#[cfg(unix)]
fn read_setup_fleet_from_stdin() -> Result<FleetSetupCredentials, Box<dyn std::error::Error>> {
    let bytes = read_bounded_migration_secret(std::io::stdin(), MAX_SETUP_FLEET_STDIN_BYTES)?;
    decode_setup_fleet_stdin(&bytes)
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

#[cfg(unix)]
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
) -> Result<TeslaMatePostgresPassword, Box<dyn std::error::Error>> {
    let bytes = read_migration_secret(path, MAX_MIGRATION_POSTGRES_PASSWORD_FILE_BYTES)?;
    TeslaMatePostgresPassword::from_bytes(&bytes).map_err(Into::into)
}

#[cfg(unix)]
fn read_bounded_migration_secret(
    reader: impl Read,
    maximum: usize,
) -> Result<zeroize::Zeroizing<Vec<u8>>, MigrationSecretReadError> {
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(maximum.min(8 * 1024)));
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
