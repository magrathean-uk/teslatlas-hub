// SPDX-License-Identifier: AGPL-3.0-only

// The service's backup schedule (M06-T03). The schedule, the one-run slot and
// the operation live in `tesatlas_hub::runtime::backup_schedule`; this only
// builds it for `serve` from the `[backup]` configuration. The recovery
// password stays in the source `backup.password_source` names (the Mac
// Keychain, or an owned file) and is read at each run; `backup-password`
// below is how the app saves it in the Keychain (M06-T17).

/// Starts the schedule for `serve`. Scheduled runs wait while the Hub is in
/// its restored-without-keys state; `backup-now` is refused there by the
/// control channel.
#[cfg(unix)]
fn start_backup_schedule(
    config: &HubConfig,
    store: &HubStore,
    recovery: &std::sync::Arc<operator::authority::RecoveryGate>,
) -> Result<
    std::sync::Arc<tesatlas_hub::runtime::backup_schedule::BackupScheduler>,
    Box<dyn std::error::Error>,
> {
    use tesatlas_hub::runtime::backup_schedule::{
        BackupScheduler, BackupSettings, StoreBackupRunner, state::STATE_FILE,
    };
    let paused = std::sync::Arc::clone(recovery);
    let scheduler = BackupScheduler::start(
        BackupSettings::from_config(&config.backup),
        std::sync::Arc::new(StoreBackupRunner::new(store.clone())),
        config.data_dir.join(STATE_FILE),
        Box::new(move || paused.pending()),
    )?;
    Ok(std::sync::Arc::new(scheduler))
}

/// `backup-password store|compare|status --account <account>` (M06-T17,
/// DEC-123 option A): the app hands the password to this binary on standard
/// input, so the Keychain item is created and owned by the service binary the
/// LaunchAgent runs. Prints one JSON line naming the outcome, never the
/// password.
fn run_backup_password(
    action: BackupPasswordAction,
    account: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    {
        use tesatlas_hub::runtime::backup_schedule::keychain::{self, Comparison};
        let store = keychain::system_keychain();
        let outcome = match action {
            BackupPasswordAction::Store => {
                let password = keychain::password_from_input(std::io::stdin().lock())?;
                keychain::store_password(store.as_ref(), account, &password)?;
                "stored"
            }
            BackupPasswordAction::Compare => {
                let password = keychain::password_from_input(std::io::stdin().lock())?;
                match keychain::compare_password(store.as_ref(), account, &password)? {
                    Comparison::Missing => "missing",
                    Comparison::Equal => "equal",
                    Comparison::Different => "different",
                }
            }
            BackupPasswordAction::Status => {
                if keychain::password_status(store.as_ref(), account)? { "present" } else { "missing" }
            }
        };
        println!("{}", serde_json::json!({ "backupPassword": outcome }));
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (action, account);
        Err(OperatorRefusal::new(
            tesatlas_hub::runtime::backup_schedule::keychain::BACKUP_PASSWORD_INVALID,
            "the Keychain is on the Mac only; elsewhere set backup.password_source = \"file\" with backup.password_file",
        )
        .into())
    }
}
