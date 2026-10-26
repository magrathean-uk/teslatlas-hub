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

#[cfg(target_os = "linux")]
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
    PrivateLan,
}

#[cfg(target_os = "macos")]
impl DevelopmentServeModeArgument {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fixture => "fixture",
            Self::Standalone => "standalone",
            Self::PrivateLan => "private-lan",
        }
    }

    fn serve_mode(self) -> teslatlas_hub::macos_launch_agent::DevelopmentServeMode {
        match self {
            Self::Fixture => teslatlas_hub::macos_launch_agent::DevelopmentServeMode::Fixture,
            Self::Standalone => teslatlas_hub::macos_launch_agent::DevelopmentServeMode::Standalone,
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

/// The old `set-charge-cost --mode` values that still work for one release but
/// are deprecated, each naming a total, a per-kWh rate or a per-minute rate
/// (C2 R53a, R72). The previous release accepted exactly `total`, `per_kwh` and
/// `per_minute`, rejected every other value and meant `total` when `--mode` was
/// absent, so every old value is a current name and this table is empty: no old
/// invocation needs a deprecation line. A line added here later (for a rename
/// of a mode) is accepted with exactly one deprecation line on stderr before
/// the store call and makes the same store call as its mode; a current name
/// always wins over an alias of the same text. An unlisted value is a usage
/// error. Reason: an invocation that worked must keep its meaning, and a value
/// or default that stays current is not deprecated.
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
    /// Stop the service and remove the persisted Tesla account credentials. On macOS
    /// the service stays stopped across logins until Start Hub in the app.
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
    /// Create a geofence, or edit one by id; omitted options keep the stored values.
    #[command(name = "set-geofence")]
    SetGeofence {
        #[arg(
            long,
            value_parser = clap::value_parser!(i64).range(1..),
            help = "Id of an existing fence to edit; omit to create a new fence."
        )]
        id: Option<i64>,
        #[arg(long)]
        name: String,
        // Negative numbers are values, so `--latitude -33.5` parses and a typed
        // negative amount reaches the store and its own refusal text.
        #[arg(long, allow_negative_numbers = true)]
        latitude: f64,
        #[arg(long, allow_negative_numbers = true)]
        longitude: f64,
        // C8 R34: no clap default, so the store can tell an omitted radius
        // (a new fence takes the product default, an edit keeps its own) from
        // a typed one.
        #[arg(long, allow_negative_numbers = true, help = geofence_radius_help())]
        radius_m: Option<f64>,
        // C8 R35: the exact stored vocabulary; another value is a usage error
        // before the store is opened.
        #[arg(long, value_parser = ["per_kwh", "per_minute"], help = geofence_billing_type_help())]
        billing_type: Option<String>,
        #[arg(long, allow_negative_numbers = true, help = geofence_cost_per_unit_help())]
        cost_per_unit: Option<f64>,
        #[arg(long, allow_negative_numbers = true, help = geofence_session_fee_help())]
        session_fee: Option<f64>,
        #[arg(
            long,
            help = "Remove the stored billing type, cost per unit and session fee; tariff options given with this flag are applied afterwards."
        )]
        clear_tariff: bool,
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
        /// What `--cost` means: `total` (the default), `per_kwh` or `per_minute`
        /// (C2 R53, R72).
        #[arg(
            long,
            default_value = "total",
            value_name = "total|per_kwh|per_minute",
            value_parser = parse_charge_cost_mode
        )]
        mode: ChargeCostModeChoice,
    },
    /// Write one completed drive as GPX 1.1 to stdout.
    #[command(name = "export-gpx")]
    ExportGpx {
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        drive_id: i64,
    },
}

/// Actions on the collector (C3 R65, R66). Each records a request in the Hub's
/// database, which the running collector applies within a few seconds in its next
/// tick; the CLI sends nothing to Tesla and needs no running collector to record it.
#[derive(Debug, Subcommand)]
enum CollectorCommand {
    /// Retry collection now: clear a refusal or suspension of the account, every
    /// wait and every paused car, and check the Tesla account once.
    Retry,
    /// Clear one paused car's pause so collection starts again for it.
    Resume {
        /// Hub vehicle UUID of the paused car.
        vehicle: uuid::Uuid,
    },
    /// Collection gaps: the stretches in which the Hub could not record the car.
    Gaps {
        #[command(subcommand)]
        command: CollectorGapsCommand,
    },
}

#[derive(Debug, Subcommand)]
enum CollectorGapsCommand {
    /// Mark every alert gap as seen, so readiness stops reporting it as unnotified.
    Acknowledge,
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
    /// Validate one source database, connection, and selected-car inventory without mutating source database or Hub.
    #[cfg(unix)]
    #[command(name = "source-check")]
    SourceCheck {
        /// Password-free PostgreSQL URL. Password is read from a file or stdin.
        #[arg(long)]
        source: String,
        /// Positive source car id that will be imported.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        car_id: i64,
        /// PostgreSQL password file, or `-` for stdin.
        #[arg(long)]
        postgres_password_file: PathBuf,
        /// Check history compatibility without inspecting the source application's private token schema.
        #[arg(long)]
        history_only: bool,
        /// Accept that database evidence proves a recognised schema set, while the source application release remains unverified.
        #[arg(long, alias = "acknowledge-v4-2-compatible-schema")]
        accept_schema_evidence: bool,
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
        /// Source car id selected for the cutover.
        #[arg(long)]
        car_id: i64,
    },
    /// Verify that a strictly newer observation is durably committed.
    #[command(name = "verify-observation")]
    VerifyObservation {
        /// Source car id selected for the cutover.
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
    /// Control the installed Linux service.
    #[cfg(target_os = "linux")]
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
    /// Retry collection, resume a paused car or acknowledge collection gaps while the
    /// service runs.
    Collector {
        #[command(subcommand)]
        command: CollectorCommand,
    },
    /// Import one source car, its history, and its opaque legacy token pair.
    #[cfg(unix)]
    Migrate {
        /// Password-free PostgreSQL URL. Password is read from a file or stdin.
        #[arg(long)]
        source: String,
        /// Stable owner-chosen import source key; omitted means the configured key or default.
        #[arg(long)]
        source_key: Option<String>,
        /// Explicitly allow registration of another import source.
        #[arg(long)]
        new_import_source: bool,
        /// Positive source car id to import.
        #[arg(long)]
        car_id: i64,
        /// PostgreSQL password file, or `-` for stdin.
        #[arg(long)]
        postgres_password_file: PathBuf,
        /// Import and publish history without reading or storing Tesla credentials.
        #[arg(long, requires = "online_snapshot", conflicts_with_all = ["encryption_key_file", "access_token_file", "refresh_token_file", "preserve_existing_credentials"])]
        history_only: bool,
        /// Source application's ENCRYPTION_KEY file, or `-` for stdin. Omit only with both fresh-token files.
        #[arg(long)]
        encryption_key_file: Option<PathBuf>,
        /// Fresh legacy access-token file when the source application's encryption key is unavailable.
        #[arg(
            long,
            requires = "refresh_token_file",
            conflicts_with = "encryption_key_file"
        )]
        access_token_file: Option<PathBuf>,
        /// Fresh legacy refresh-token file when the source application's encryption key is unavailable.
        #[arg(
            long,
            requires = "access_token_file",
            conflicts_with = "encryption_key_file"
        )]
        refresh_token_file: Option<PathBuf>,
        /// Take one live read-only snapshot, never prompt for cutover, and leave Hub stopped.
        #[arg(long)]
        online_snapshot: bool,
        /// Preserve an existing Hub account while importing source database history.
        #[arg(long, requires = "online_snapshot", hide = true)]
        preserve_existing_credentials: bool,
        /// Confirm source application release 4.2.0 or later; database evidence alone cannot prove the application release.
        #[arg(long, required = true, alias = "acknowledge-v4-2-compatible-schema")]
        accept_schema_evidence: bool,
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

/// Help for `set-geofence --radius-m` (C8 R39), formatted from the C8 R19
/// constants so the text and the validation never disagree.
fn geofence_radius_help() -> String {
    use teslatlas_hub::location::{
        DEFAULT_GEOFENCE_RADIUS_METRES, MAX_GEOFENCE_RADIUS_METRES, MIN_SAVED_GEOFENCE_RADIUS_METRES,
    };
    format!(
        "Radius in whole metres, {MIN_SAVED_GEOFENCE_RADIUS_METRES} to {MAX_GEOFENCE_RADIUS_METRES}. Default {DEFAULT_GEOFENCE_RADIUS_METRES} for a new fence; an edit keeps the stored radius."
    )
}

/// Help for `set-geofence --billing-type` (C8 R39).
fn geofence_billing_type_help() -> String {
    format!(
        "Unit of --cost-per-unit: {} or {}. Default {} when a cost is given and no type is stored; an edit keeps the stored type unless this is given.",
        GeofenceBillingType::PerKwh.as_str(),
        GeofenceBillingType::PerMinute.as_str(),
        GeofenceBillingType::PerKwh.as_str(),
    )
}

/// Help for `set-geofence --cost-per-unit` (C8 R39).
fn geofence_cost_per_unit_help() -> String {
    format!(
        "Price per kWh or per minute, 0 to {}. An edit keeps the stored price unless this is given.",
        teslatlas_hub::location::MAX_GEOFENCE_COST_PER_UNIT
    )
}

/// Help for `set-geofence --session-fee` (C8 R39).
fn geofence_session_fee_help() -> String {
    format!(
        "Flat fee per charging session, 0 to {}. An edit keeps the stored fee unless this is given.",
        teslatlas_hub::location::MAX_GEOFENCE_SESSION_FEE
    )
}

const IMPORT_COMMAND_HINT: &str = "Import checks use source-check; see docs/guides/cli.md.";
const SCHEMA_FLAG_DEPRECATION: &str = "--acknowledge-v4-2-compatible-schema is deprecated; use --accept-schema-evidence. The alias will be removed in a later version.";

impl Cli {
    fn try_parse_from<I, T>(arguments: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        <Self as clap::Parser>::try_parse_from(arguments).map_err(|error| {
            if error.kind() == clap::error::ErrorKind::InvalidSubcommand {
                let text = error.to_string();
                let message = text.strip_prefix("error: ").unwrap_or(&text);
                clap::Error::raw(error.kind(), format!("{message}\n{IMPORT_COMMAND_HINT}\n"))
            } else {
                error
            }
        })
    }

    fn parse_with_schema_warning<I, T>(
        arguments: I,
        stderr: &mut impl std::io::Write,
    ) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let arguments = arguments
            .into_iter()
            .map(Into::into)
            .collect::<Vec<std::ffi::OsString>>();
        let deprecated = arguments
            .iter()
            .any(|argument| argument == "--acknowledge-v4-2-compatible-schema");
        let cli = Self::try_parse_from(arguments)?;
        if deprecated {
            writeln!(stderr, "{SCHEMA_FLAG_DEPRECATION}")
                .map_err(|error| clap::Error::raw(clap::error::ErrorKind::Io, error))?;
        }
        Ok(cli)
    }

    fn parse() -> Self {
        Self::parse_with_schema_warning(std::env::args_os(), &mut std::io::stderr())
            .unwrap_or_else(|error| error.exit())
    }
}
