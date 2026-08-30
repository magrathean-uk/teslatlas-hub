// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Debug, Error)]
pub enum StreamTaskOutcome {
    #[error("completed normally")]
    CompletedNormally,
    #[error("supervisor failed: {0}")]
    Supervisor(#[source] crate::tesla_stream::StreamSupervisorError),
    #[error("task panicked")]
    Panicked,
    #[error("task was cancelled")]
    Cancelled,
}

fn classify_stream_task_result(
    result: Result<Result<(), crate::tesla_stream::StreamSupervisorError>, JoinError>,
) -> CollectorError {
    let outcome = match result {
        Ok(Ok(())) => StreamTaskOutcome::CompletedNormally,
        Ok(Err(error)) => StreamTaskOutcome::Supervisor(error),
        Err(error) if error.is_panic() => StreamTaskOutcome::Panicked,
        Err(error) => {
            debug_assert!(error.is_cancelled());
            StreamTaskOutcome::Cancelled
        }
    };
    CollectorError::StreamTask(outcome)
}

#[derive(Debug, Error)]
pub enum CollectorError {
    #[error("Serve requires one configured vehicle")]
    SelectedVehicleMissing,
    #[error("native setup store does not match the configured Hub data directory")]
    NativeSetupStoreMismatch,
    #[error("native setup requires legacy Owner API authentication to be enabled")]
    NativeSetupLegacyAuthRequired,
    #[error("Fleet setup requires collector.provider = \"fleet\"")]
    NativeSetupFleetProviderRequired,
    #[error("native setup found no vehicles")]
    NativeSetupNoVehicles,
    #[error("native setup found {discovered} vehicles; select one with --vehicle-id")]
    NativeSetupVehicleSelectionRequired { discovered: usize },
    #[error("native setup vehicle id must be positive")]
    NativeSetupVehicleIdInvalid,
    #[error("native setup vehicle {0} was not found")]
    NativeSetupVehicleNotFound(i64),
    #[error("Fleet account does not contain every already-configured vehicle")]
    FleetSetupInventoryMismatch,
    #[error("vehicle command target is not configured")]
    CommandVehicleMissing,
    #[error("resident vehicle-control socket is unavailable")]
    ResidentControlSocket,
    #[error(
        "Hub is already configured for vehicle {existing}; refusing requested vehicle {requested}"
    )]
    NativeSetupVehicleConflict { existing: i64, requested: i64 },
    #[error("supervised collector heartbeat task stopped unexpectedly")]
    SupervisedHeartbeatTask,
    #[error("terrain worker stopped unexpectedly")]
    TerrainWorkerTask,
    #[error("export publication worker stopped unexpectedly")]
    ExportPublicationTask,
    #[error("vehicle stream task stopped unexpectedly: {0}")]
    StreamTask(StreamTaskOutcome),
    #[error("terrain worker failed during local startup")]
    TerrainWorkerStartup,
    #[error("runtime sensitive-access admission is unavailable")]
    SensitiveAccessUnavailable,
    #[error("supervised collector startup receiver closed")]
    SupervisedStartupReadyDropped,
    #[cfg(unix)]
    #[error("admitted collector store does not match the selected Hub store")]
    AdmittedStoreMismatch,
    #[cfg(unix)]
    #[error("admitted collection requires legacy authentication")]
    AdmittedLegacyAuthRequired,
    #[cfg(unix)]
    #[error(transparent)]
    UserAdmission(#[from] crate::hub_user_process::UserLifetimeLockError),
    #[error("manual collection receipt timestamp is invalid")]
    InvalidReceiptTimestamp,
    #[error("manual collection received data for a vehicle absent from discovery")]
    SnapshotWithoutListedVehicle,
    #[error("system clock is before the Unix epoch")]
    SystemClockBeforeEpoch,
    #[error("system clock is outside the supported timestamp range")]
    SystemClockOutOfRange,
    #[error("cannot serialize compatibility snapshot")]
    SerializeSnapshot(serde_json::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Credential(#[from] CredentialError),
    #[error(transparent)]
    OwnerApiConfig(#[from] OwnerApiConfigError),
    #[error(transparent)]
    OwnerApi(#[from] OwnerApiError),
    #[error(transparent)]
    OwnerApiAuth(#[from] OwnerApiAuthError),
    #[error(transparent)]
    LegacyAuthManager(#[from] LegacyAuthManagerError),
    #[error(transparent)]
    LegacyAuth(#[from] LegacyAuthError),
    #[error(transparent)]
    FleetApiConfig(#[from] FleetApiConfigError),
    #[error(transparent)]
    FleetApi(#[from] FleetApiError),
    #[error(transparent)]
    FleetCredential(#[from] FleetCredentialError),
    #[error(transparent)]
    Projection(#[from] ProjectionPackError),
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
    #[error(transparent)]
    Geocoder(#[from] GeocoderError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Default)]
struct TerrainFuse {
    failures: Vec<Instant>,
    blown_until: Option<Instant>,
}
