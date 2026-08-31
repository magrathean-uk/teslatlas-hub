// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Debug, Parser)]
#[command(
    name = "teslatlas-hub",
    version,
    about = "Native Teslatlas Hub service"
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[cfg(unix)]
#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Print the installed service state.
    Status,
    /// Start the installed service.
    Start,
    /// Stop the installed service.
    Stop,
    /// Stop and start the installed service.
    Restart,
}

#[derive(Debug, Subcommand)]
enum ControlCommand {
    /// List paired devices without exposing bearer material.
    #[command(name = "paired-devices")]
    PairedDevices,
    /// Revoke one paired device bearer immediately.
    #[command(name = "revoke-device")]
    RevokeDevice { device_id: uuid::Uuid },
    /// Show or update the selected car's collection settings.
    Settings {
        #[arg(long)]
        enabled: Option<bool>,
        #[arg(long)]
        streaming: Option<bool>,
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        suspend_after_idle_min: Option<i64>,
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        suspend_min: Option<i64>,
        #[arg(long)]
        require_locked: Option<bool>,
        #[arg(long)]
        free_supercharging: Option<bool>,
        #[arg(long)]
        lfp_battery: Option<bool>,
    },
    /// Pause Owner API and streaming collection for the selected car.
    Pause,
    /// Resume Owner API and streaming collection for the selected car.
    Resume,
    /// Stop collection and remove the persisted Tesla account credentials.
    #[command(name = "sign-out")]
    SignOut,
    /// Explicitly wake the selected car once.
    Wake {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Start climate control once.
    #[command(name = "climate-start")]
    ClimateStart {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Stop climate control once.
    #[command(name = "climate-stop")]
    ClimateStop {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Start charging once.
    #[command(name = "charge-start")]
    ChargeStart {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Stop charging once.
    #[command(name = "charge-stop")]
    ChargeStop {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Set the charging limit percentage once.
    #[command(name = "set-charge-limit")]
    SetChargeLimit {
        #[arg(long, value_parser = clap::value_parser!(u8).range(50..=100))]
        percent: u8,
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Lock the selected car once.
    Lock {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Unlock the selected car once.
    Unlock {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Flash the selected car's lights once.
    #[command(name = "flash-lights")]
    FlashLights {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Honk the selected car's horn once.
    #[command(name = "honk-horn")]
    HonkHorn {
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// List configured geofences.
    Geofences,
    /// Create a geofence, or replace one by id.
    #[command(name = "set-geofence")]
    SetGeofence {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        id: Option<i64>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        latitude: f64,
        #[arg(long)]
        longitude: f64,
        #[arg(long, default_value_t = 20.0)]
        radius_m: f64,
        #[arg(long, default_value = "per_kwh")]
        billing_type: String,
        #[arg(long)]
        cost_per_unit: Option<f64>,
        #[arg(long)]
        session_fee: Option<f64>,
        /// Fill missing costs for matching historical charges after saving.
        #[arg(long)]
        recalculate_missing_costs: bool,
    },
    /// Delete a geofence by id.
    #[command(name = "delete-geofence")]
    DeleteGeofence {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        id: i64,
    },
    /// Replace the total cost of one completed charging session.
    #[command(name = "set-charge-cost")]
    SetChargeCost {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        charge_id: i64,
        #[arg(long)]
        cost: f64,
        #[arg(long, default_value = "total")]
        mode: String,
    },
    /// Write one completed drive as TeslaMate-compatible GPX to stdout.
    #[command(name = "export-gpx")]
    ExportGpx {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        drive_id: i64,
    },
}

#[derive(Debug, Subcommand)]
enum WriteBackCommand {
    /// Copy one total cost into a TeslaMate charging process.
    #[command(name = "charge-cost")]
    ChargeCost {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        charging_process_id: i64,
        /// Exact numeric(14,2) value, for example 12.34.
        #[arg(long)]
        cost: TeslaMateCost,
        /// Commit. Omit for a locked-row dry run and rollback.
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print licence, source, and independence notices.
    #[command(visible_aliases = ["licence", "license"])]
    Legal,
    /// Print this build's source route.
    Source,
    /// Initialize or migrate the local Hub database.
    Init,
    /// Create the configured local store for a packaged Linux installation.
    #[cfg(unix)]
    Bootstrap,
    /// Configure one or every vehicle directly from private Owner tokens.
    #[cfg(unix)]
    Setup {
        /// Legacy Owner API access-token file, or `-` for stdin.
        #[arg(
            long,
            required_unless_present = "tokens_stdin",
            requires = "refresh_token_file",
            conflicts_with = "tokens_stdin"
        )]
        access_token_file: Option<PathBuf>,
        /// Legacy Owner API refresh-token file, or `-` for stdin.
        #[arg(
            long,
            required_unless_present = "tokens_stdin",
            requires = "access_token_file",
            conflicts_with = "tokens_stdin"
        )]
        refresh_token_file: Option<PathBuf>,
        /// Read one bounded {accessToken,refreshToken} JSON object from stdin.
        #[arg(long, conflicts_with_all = ["access_token_file", "refresh_token_file"])]
        tokens_stdin: bool,
        /// Tesla Owner API vehicle id. Required only when discovery finds multiple vehicles.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..), conflicts_with = "all_vehicles")]
        vehicle_id: Option<i64>,
        /// Configure every vehicle returned by the Tesla account.
        #[arg(long)]
        all_vehicles: bool,
    },
    /// Configure one or every Fleet API vehicle from bounded JSON read only from stdin.
    #[cfg(unix)]
    #[command(name = "setup-fleet")]
    SetupFleet {
        /// Tesla Fleet API vehicle id. Required only when discovery finds multiple vehicles.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..), conflicts_with = "all_vehicles")]
        vehicle_id: Option<i64>,
        /// Configure every vehicle returned by the Tesla account.
        #[arg(long)]
        all_vehicles: bool,
    },
    /// Install the fixed low-cost native Fleet Telemetry policy on configured vehicles.
    #[cfg(unix)]
    #[command(name = "configure-fleet-telemetry")]
    ConfigureFleetTelemetry,
    /// Full read-only check of the Hub database, stored Tesla credentials, TLS, and collector readiness.
    Doctor,
    /// Print the redacted local status consumed by the native control app.
    Status,
    /// Validate one TeslaMate source, connection, and selected-car inventory without mutating TeslaMate or Hub.
    #[cfg(unix)]
    #[command(name = "teslamate-check")]
    TeslaMateCheck {
        /// Password-free PostgreSQL URL. Password is read from a file or stdin.
        #[arg(long)]
        source: String,
        /// Positive TeslaMate car id that will be imported.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        car_id: i64,
        /// PostgreSQL password file, or `-` for stdin.
        #[arg(long)]
        postgres_password_file: PathBuf,
        /// Accept that database evidence proves a TeslaMate v4.2-compatible schema, not the app version.
        #[arg(long)]
        acknowledge_v4_2_compatible_schema: bool,
    },
    /// Validate that one configured car and its credentials are ready to serve.
    #[cfg(unix)]
    Preflight,
    /// Capture the current durable observation watermark for one source car.
    #[command(name = "observation-watermark")]
    ObservationWatermark {
        /// Source/import car id selected for the cutover.
        #[arg(long)]
        car_id: i64,
    },
    /// Verify that a strictly newer observation is durably committed.
    #[command(name = "verify-observation")]
    VerifyObservation {
        /// Source/import car id selected for the cutover.
        #[arg(long)]
        car_id: i64,
        /// Observation id returned by observation-watermark.
        #[arg(long)]
        watermark: i64,
    },
    /// Start the Hub HTTP service.
    Serve,
    /// Observe one admitted car for a bounded period without installing a service.
    #[cfg(unix)]
    Observe {
        /// Observation duration in seconds.
        #[arg(long, default_value_t = 3_600, value_parser = clap::value_parser!(u64).range(1..))]
        duration_seconds: u64,
    },
    /// Install and start the minimal per-user macOS Hub LaunchAgent.
    #[cfg(target_os = "macos")]
    Install,
    /// Control the installed Hub service.
    #[cfg(unix)]
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Inspect or change configured cars while the service is running.
    Control {
        /// Hub vehicle UUID. Optional only when exactly one car is published.
        #[arg(long)]
        vehicle_id: Option<uuid::Uuid>,
        #[command(subcommand)]
        command: ControlCommand,
    },
    /// Import one TeslaMate car, its history, and its opaque legacy token pair.
    #[cfg(unix)]
    Migrate {
        /// Password-free PostgreSQL URL. Password is read from a file or stdin.
        #[arg(long)]
        source: String,
        /// Positive TeslaMate car id to import.
        #[arg(long)]
        car_id: i64,
        /// PostgreSQL password file, or `-` for stdin.
        #[arg(long)]
        postgres_password_file: PathBuf,
        /// TeslaMate ENCRYPTION_KEY file, or `-` for stdin. Omit only with both fresh-token files.
        #[arg(long)]
        encryption_key_file: Option<PathBuf>,
        /// Fresh legacy access-token file when the TeslaMate key is unavailable.
        #[arg(
            long,
            requires = "refresh_token_file",
            conflicts_with = "encryption_key_file"
        )]
        access_token_file: Option<PathBuf>,
        /// Fresh legacy refresh-token file when the TeslaMate key is unavailable.
        #[arg(
            long,
            requires = "access_token_file",
            conflicts_with = "encryption_key_file"
        )]
        refresh_token_file: Option<PathBuf>,
        /// Take one live read-only snapshot, never prompt for cutover, and leave Hub stopped.
        #[arg(long)]
        online_snapshot: bool,
        /// Preserve an existing Hub account while importing TeslaMate history.
        #[arg(long, requires = "online_snapshot", hide = true)]
        preserve_existing_credentials: bool,
        /// Confirm TeslaMate 4.2.0+ and accept that its database schema alone cannot prove the app version.
        #[arg(long, required = true)]
        acknowledge_v4_2_compatible_schema: bool,
    },
    /// Explicit allow-listed write-back to TeslaMate PostgreSQL.
    #[cfg(unix)]
    WriteBack {
        /// Password-free PostgreSQL URL. Password is read from a file or stdin.
        #[arg(long)]
        source: String,
        /// Positive TeslaMate car id owning the target row.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        car_id: i64,
        /// PostgreSQL password file, or `-` for stdin.
        #[arg(long)]
        postgres_password_file: PathBuf,
        #[command(subcommand)]
        command: WriteBackCommand,
    },
    /// Display one short-lived, single-use device pairing QR.
    #[command(alias = "create-pairing")]
    Pair {
        /// Human label shown only to the Hub owner, such as "Bolyki iPhone".
        #[arg(long, default_value = "Teslatlas iPhone")]
        label: String,
        /// Lifetime before the invitation becomes unusable.
        #[arg(long, default_value_t = 900)]
        expires_in_seconds: u64,
        /// Print the secret-bearing machine-readable payload for explicit debugging.
        #[arg(long, hide = true)]
        json: bool,
    },
    /// Validate database integrity, report quarantined sessions, and clean orphaned packs.
    Repair,
    /// Create a data-only catalogue and immutable-pack backup generation.
    Backup {
        /// New private directory that will contain the data-backup generation.
        #[arg(long)]
        destination: PathBuf,
    },
    /// Immutably verify one completed data-backup generation.
    #[command(name = "verify-backup")]
    VerifyBackup {
        /// Existing private data-backup generation to verify without mutation.
        #[arg(long)]
        source: PathBuf,
    },
    /// Restore data and the pairing database into a new private directory.
    #[command(name = "restore-data")]
    RestoreData {
        /// Existing verified data-backup generation.
        #[arg(long)]
        source: PathBuf,
        /// New Hub data directory. Credentials and collector authority stay absent.
        #[arg(long)]
        destination: PathBuf,
    },
    /// Export decryption/signing keys into a separately encrypted, secret-bearing file.
    #[command(name = "export-recovery-credentials")]
    ExportRecoveryCredentials {
        /// New encrypted recovery file. Never store beside the default data backup.
        #[arg(long)]
        destination: PathBuf,
        /// Owned mode-0600 file containing exactly 32 random bytes.
        #[arg(long)]
        recovery_key_file: PathBuf,
    },
    /// Restore a separately encrypted key export into a data-only restore.
    #[command(name = "restore-recovery-credentials")]
    RestoreRecoveryCredentials {
        /// Existing encrypted, secret-bearing recovery file.
        #[arg(long)]
        source: PathBuf,
        /// Owned mode-0600 file containing the exact 32-byte recovery key.
        #[arg(long)]
        recovery_key_file: PathBuf,
    },
}
