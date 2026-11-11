// SPDX-License-Identifier: AGPL-3.0-only

// The service's backup schedule (M06-T03). The schedule, the one-run slot and
// the operation live in `teslatlas_hub::runtime::backup_schedule`; this only
// builds it for `serve` from the `[backup]` configuration. The recovery
// password stays in the file the configuration names and is read at each run.

/// Starts the schedule for `serve`. Scheduled runs wait while the Hub is in
/// its restored-without-keys state; `backup-now` is refused there by the
/// control channel.
#[cfg(unix)]
fn start_backup_schedule(
    config: &HubConfig,
    store: &HubStore,
    recovery: &std::sync::Arc<operator::authority::RecoveryGate>,
) -> Result<
    std::sync::Arc<teslatlas_hub::runtime::backup_schedule::BackupScheduler>,
    Box<dyn std::error::Error>,
> {
    use teslatlas_hub::runtime::backup_schedule::{
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
