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

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum DevelopmentServeModeArgument {
    Fixture,
    Standalone,
    Edge,
    PrivateLan,
}

#[cfg(target_os = "macos")]
impl DevelopmentServeModeArgument {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fixture => "fixture",
            Self::Standalone => "standalone",
            Self::Edge => "edge",
            Self::PrivateLan => "private-lan",
        }
    }

    fn serve_mode(self) -> teslatlas_hub::macos_launch_agent::DevelopmentServeMode {
        match self {
            Self::Fixture => teslatlas_hub::macos_launch_agent::DevelopmentServeMode::Fixture,
            Self::Standalone => teslatlas_hub::macos_launch_agent::DevelopmentServeMode::Standalone,
            Self::Edge => teslatlas_hub::macos_launch_agent::DevelopmentServeMode::Edge,
            Self::PrivateLan => teslatlas_hub::macos_launch_agent::DevelopmentServeMode::PrivateLan,
        }
    }
}

/// What `set-charge-cost --cost` means (C2 R53). The three names reuse the
/// `--billing-type` vocabulary (`per_kwh`, `per_minute`) plus `total`, because
/// the commonest manual case is copying a total from a receipt, so a unit
/// price must be named to be used and is never mistaken for a total.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ChargeCostMode {
    /// The session's total cost.
    #[value(name = "total")]
    Total,
    /// A price per kWh.
    #[value(name = "per_kwh")]
    PerKwh,
    /// A price per minute of connected time.
    #[value(name = "per_minute")]
    PerMinute,
}

/// A `--mode` value as the owner typed it: the mode and, when the value was a
/// deprecated alias of one release ago, that alias (C2 R53a), so the command
/// can say so once on stderr.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ChargeCostModeChoice {
    mode: ChargeCostMode,
    deprecated_alias: Option<String>,
}

/// The old `set-charge-cost --mode` values that still work for one release, each
/// naming a total, a per-kWh rate or a per-minute rate (C2 R53a). The values of
/// the previous release are the dirty side's to supply (C2 section 12 item 9)
/// and the clean inputs do not state them, so the table is empty until the
/// coordinator provides them; each line added here is accepted with a
/// deprecation line on stderr and the same store call as its new mode. An old
/// value that is not listed is a usage error. Reason: an invocation that named
/// a mode of the old release must not silently break, and one that relied on
/// an unlisted default must not silently store its number as a total, which is
/// why `--mode` is required in this release.
const DEPRECATED_CHARGE_COST_MODES: &[(&str, ChargeCostMode)] = &[];

/// Parse a `--mode` value: the three names, then the deprecated aliases of
/// `aliases`. Anything else is a usage error that names the three modes, so
/// no store call is made (C2 R53, R53a).
fn parse_charge_cost_mode_in(
    value: &str,
    aliases: &[(&str, ChargeCostMode)],
) -> Result<ChargeCostModeChoice, String> {
    if let Ok(mode) = ChargeCostMode::from_str(value, false) {
        return Ok(ChargeCostModeChoice {
            mode,
            deprecated_alias: None,
        });
    }
    aliases
        .iter()
        .find(|(alias, _)| *alias == value)
        .map(|(alias, mode)| ChargeCostModeChoice {
            mode: *mode,
            deprecated_alias: Some((*alias).to_owned()),
        })
        .ok_or_else(|| format!("unknown mode `{value}`: use total, per_kwh or per_minute"))
}

fn parse_charge_cost_mode(value: &str) -> Result<ChargeCostModeChoice, String> {
    parse_charge_cost_mode_in(value, DEPRECATED_CHARGE_COST_MODES)
}

#[derive(Debug, Subcommand)]
enum ControlCommand {
    /// List paired devices and the vehicles each may read, without exposing bearer material.
    #[command(name = "paired-devices")]
    PairedDevices,
    /// Display one short-lived, single-use device pairing QR while the Hub is running.
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
        /// Grant the device only this vehicle (Hub vehicle UUID); repeat for several.
        #[arg(long = "vehicle", value_name = "UUID", conflicts_with = "all_vehicles")]
        vehicles: Vec<uuid::Uuid>,
        /// Grant every vehicle that is active when the invitation is claimed (the default).
        #[arg(long)]
        all_vehicles: bool,
    },
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
        /// After saving the fence, price the past Hub charges that started in
        /// it and have no cost yet, with this fence's tariff (C2 R49, R50).
        /// Charges that already have a cost, and imported charges, are never
        /// changed. Without this flag no past charge is touched.
        #[arg(long)]
        recalculate_missing_costs: bool,
    },
    /// Delete a geofence by id.
    #[command(name = "delete-geofence")]
    DeleteGeofence {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        id: i64,
    },
    /// Set the cost of one completed charging session (C2 R52, R53): the
    /// total, or a price per kWh or per minute from which the Hub works the
    /// total out.
    #[command(name = "set-charge-cost")]
    SetChargeCost {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        charge_id: i64,
        /// The total cost, or the price per kWh or per minute, as `--mode` says.
        #[arg(long, allow_negative_numbers = true)]
        cost: f64,
        /// What `--cost` means: `total`, `per_kwh` or `per_minute`. Required in
        /// this release (C2 R53a); `total` becomes the default in the next.
        #[arg(
            long,
            required = true,
            value_name = "total|per_kwh|per_minute",
            value_parser = parse_charge_cost_mode
        )]
        mode: ChargeCostModeChoice,
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
    /// Probe the container's loopback TLS health endpoint using one explicit CA bundle.
    Healthcheck {
        /// Absolute PEM CA bundle used only for the loopback health probe.
        #[arg(long)]
        ca_file: PathBuf,
        /// Lowercase DNS name present in the Hub TLS certificate and public URL.
        #[arg(long)]
        server_name: String,
    },
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
        /// Check history compatibility without inspecting TeslaMate's private token schema.
        #[arg(long)]
        history_only: bool,
        /// Accept that database evidence proves a TeslaMate v4.2-compatible schema, not the app version.
        #[arg(long)]
        acknowledge_v4_2_compatible_schema: bool,
    },
    /// Validate that one configured car and its credentials are ready to serve.
    #[cfg(unix)]
    Preflight {
        /// Validate signed published history with collection disabled, without inspecting Tesla credentials.
        #[arg(long)]
        history_only: bool,
    },
    /// Validate one explicit local source-run Serve mode without starting a listener.
    #[cfg(target_os = "macos")]
    #[command(name = "serve-preflight", hide = true)]
    ServePreflight {
        #[arg(long, value_enum)]
        mode: DevelopmentServeModeArgument,
        /// Require collection and Legacy authentication to remain disabled for a history-only Hub.
        #[arg(long)]
        history_only: bool,
    },
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
        /// Import and publish history without reading or storing Tesla credentials.
        #[arg(long, requires = "online_snapshot", conflicts_with_all = ["encryption_key_file", "access_token_file", "refresh_token_file", "preserve_existing_credentials"])]
        history_only: bool,
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
        /// Grant the device only this vehicle (Hub vehicle UUID); repeat for several.
        #[arg(long = "vehicle", value_name = "UUID", conflicts_with = "all_vehicles")]
        vehicles: Vec<uuid::Uuid>,
        /// Grant every vehicle that is active when the invitation is claimed (the default).
        #[arg(long)]
        all_vehicles: bool,
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
