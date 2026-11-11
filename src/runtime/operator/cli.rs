// SPDX-License-Identifier: AGPL-3.0-only

//! The operator command tree (moved from the binary, M02-T10). The binary
//! parses with these types and calls the admission rules in
//! `super::admission` before it reads a file, standard input or the network.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "teslatlas-hub",
    version,
    about = "Native Teslatlas Hub service"
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
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
pub enum DevelopmentServeModeArgument {
    Fixture,
    Standalone,
    PrivateLan,
}

#[cfg(target_os = "macos")]
impl DevelopmentServeModeArgument {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fixture => "fixture",
            Self::Standalone => "standalone",
            Self::PrivateLan => "private-lan",
        }
    }

    pub fn serve_mode(self) -> crate::macos_launch_agent::DevelopmentServeMode {
        match self {
            Self::Fixture => crate::macos_launch_agent::DevelopmentServeMode::Fixture,
            Self::Standalone => crate::macos_launch_agent::DevelopmentServeMode::Standalone,
            Self::PrivateLan => crate::macos_launch_agent::DevelopmentServeMode::PrivateLan,
        }
    }
}

/// What `set-charge-cost --cost` means (C2 R53). The three names reuse the
/// `--billing-type` vocabulary (`per_kwh`, `per_minute`) plus `total`, because
/// the commonest manual case is copying a total from a receipt, so a unit
/// price must be named to be used and is never mistaken for a total.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ChargeCostMode {
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

impl ChargeCostMode {
    /// The name the owner types (C2 R53).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Total => "total",
            Self::PerKwh => "per_kwh",
            Self::PerMinute => "per_minute",
        }
    }
}

/// A `--mode` value as the owner typed it: the mode and, when the value was a
/// deprecated alias of one release ago, that alias (C2 R53a), so the command
/// can say so once on stderr.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChargeCostModeChoice {
    pub mode: ChargeCostMode,
    pub deprecated_alias: Option<String>,
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
pub const DEPRECATED_CHARGE_COST_MODES: &[(&str, ChargeCostMode)] = &[];

/// Parse a `--mode` value: the three names, then the deprecated aliases of
/// `aliases`. Anything else is a usage error that names the three modes, so
/// no store call is made (C2 R53, R53a).
pub fn parse_charge_cost_mode_in(
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

pub fn parse_charge_cost_mode(value: &str) -> Result<ChargeCostModeChoice, String> {
    parse_charge_cost_mode_in(value, DEPRECATED_CHARGE_COST_MODES)
}

#[derive(Debug, Subcommand)]
pub enum ControlCommand {
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
    /// Rotate the credential key while the service is stopped.
    #[command(name = "rotate-credential-key")]
    RotateCredentialKey,
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
pub enum CollectorCommand {
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
pub enum CollectorGapsCommand {
    /// Mark every alert gap as seen, so readiness stops reporting it as unnotified.
    Acknowledge,
}

#[derive(Debug, Subcommand)]
pub enum Command {
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
    /// Sign the Hub in with its own login and choose its vehicles. Reads one
    /// bounded {"code","codeVerifier","redirectUri"} JSON object from stdin and
    /// forwards it to the running service, which performs the code exchange and
    /// vehicle discovery inside an open collection window. Never accepts tokens.
    #[cfg(unix)]
    Setup {
        /// Tesla Owner API vehicle id. Required only when discovery finds multiple vehicles.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..), conflicts_with = "all_vehicles")]
        vehicle_id: Option<i64>,
        /// Configure every vehicle returned by the Tesla account.
        #[arg(long)]
        all_vehicles: bool,
    },
    /// Fleet API setup. Fleet is parked: always refused with `provider_parked`.
    #[cfg(unix)]
    #[command(name = "setup-fleet")]
    SetupFleet,
    /// Fleet Telemetry configuration. Fleet is parked: always refused with `provider_parked`.
    #[cfg(unix)]
    #[command(name = "configure-fleet-telemetry")]
    ConfigureFleetTelemetry,
    /// Full read-only check of the Hub database, stored Tesla credentials, TLS, and collector readiness.
    Doctor,
    /// Print the redacted local status consumed by the native control app.
    Status,
    /// Validate one source database, connection, and selected-car inventory without mutating source database or Hub.
    #[cfg(unix)]
    #[command(
        name = "source-check",
        alias = crate::runtime::legacy_names::DEPRECATED_VERB_SOURCE_CHECK
    )]
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
    /// Import one source car and its history (read-only; never a Tesla credential).
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
        /// Import and publish history only; requires --online-snapshot.
        #[arg(long, requires = "online_snapshot")]
        history_only: bool,
        /// Take one live read-only snapshot, never prompt for cutover, and leave Hub stopped.
        #[arg(long)]
        online_snapshot: bool,
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
    /// Export decryption/signing keys into a separately encrypted, secret-bearing file
    /// (forwarded to the running service).
    #[command(name = "export-recovery-credentials")]
    ExportRecoveryCredentials {
        /// New encrypted recovery file. Never store beside the default data backup.
        #[arg(long)]
        destination: PathBuf,
        /// Owned mode-0600 file containing exactly 32 random bytes.
        #[arg(long)]
        recovery_key_file: PathBuf,
    },
    /// Restore a separately encrypted key export (forwarded to the running service).
    #[command(name = "restore-recovery-credentials")]
    RestoreRecoveryCredentials {
        /// Existing encrypted, secret-bearing recovery file.
        #[arg(long)]
        source: PathBuf,
        /// Owned mode-0600 file containing the exact 32-byte recovery key.
        #[arg(long)]
        recovery_key_file: PathBuf,
    },
    /// List the credentials the Hub refuses and keeps (pre-upgrade and Fleet
    /// rows, other pairs its own login did not make), their old key files and
    /// old tables, naming slots and origins only, never a secret value; with
    /// --confirm, remove them. The Hub's own login is never touched. Forwarded
    /// to the running service; refused while a collection window is open or a
    /// refresh is unresolved.
    #[command(name = "purge-refused-credentials")]
    PurgeRefusedCredentials {
        /// Remove what the listing names. Without it nothing is deleted.
        #[arg(long)]
        confirm: bool,
    },
    /// Run one encrypted backup now, the same operation the service's schedule
    /// runs (forwarded to the running service). The destination and the
    /// password file default to the `[backup]` configuration; the password
    /// itself is never an argument. Refused while another backup runs.
    #[command(name = "backup-now")]
    BackupNow {
        /// Backup folder (default: `backup.destination`).
        #[arg(long)]
        destination: Option<PathBuf>,
        /// Owned mode-0600 file holding the recovery password (default: `backup.password_file`).
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    /// Collection windows: status, trust, open and close are forwarded to the
    /// running service; the issuer verbs run on the issuer Mac only.
    Window {
        #[command(subcommand)]
        command: WindowCommand,
    },
}

/// Collection-window verbs (docs/operations/collection-windows.md).
#[derive(Debug, Subcommand)]
pub enum WindowCommand {
    /// Print the window state the running service holds.
    Status,
    /// Print this host's id, for the issuer to name in a grant.
    #[command(name = "host-id")]
    HostId,
    /// Trust the issuer named in a trust document; starts a new trust epoch.
    Trust {
        /// Trust document written by `window issuer-init` or `window issuer-trust`.
        file: PathBuf,
    },
    /// Open (or pend) a window with a signed grant.
    Open {
        /// Signed grant file written by `window issue`.
        #[arg(long)]
        grant: PathBuf,
    },
    /// Close the open or pending window now (reason `operator_close`).
    Close,
    /// Issuer: create the issuer key pair and ledger once.
    #[command(name = "issuer-init")]
    IssuerInit {
        /// New private-key file (0600); store it in 1Password and discard the local copy.
        #[arg(long)]
        key_out: PathBuf,
        /// New trust document for `window trust` on each Hub.
        #[arg(long)]
        trust_out: PathBuf,
    },
    /// Issuer: write the trust document again, optionally with a new trust epoch.
    #[command(name = "issuer-trust")]
    IssuerTrust {
        /// Issuer private-key file (0600).
        #[arg(long)]
        key_file: PathBuf,
        /// Trust document to write.
        #[arg(long)]
        trust_out: PathBuf,
        /// Start a new trust epoch, so every earlier grant is refused.
        #[arg(long)]
        new_epoch: bool,
    },
    /// Issuer: sign one grant for one host.
    Issue {
        /// Issuer private-key file (0600).
        #[arg(long)]
        key_file: PathBuf,
        /// Host id of the target Hub (`window host-id` on that host).
        #[arg(long)]
        host_id: String,
        /// Start, UTC (RFC 3339) or Unix seconds.
        #[arg(long)]
        starts_at: String,
        /// Hard expiry, UTC (RFC 3339) or Unix seconds.
        #[arg(long)]
        expires_at: String,
        /// Approver; must be exactly `G`.
        #[arg(long)]
        approver: String,
        /// Why the window is needed (at most 200 characters).
        #[arg(long)]
        reason: String,
        /// Reference to the record of the approval.
        #[arg(long)]
        approval_ref: String,
        /// Grant file to write.
        #[arg(long)]
        out: PathBuf,
    },
    /// Issuer: void a reservation whose grant was never published.
    Void {
        /// Issuer private-key file (0600).
        #[arg(long)]
        key_file: PathBuf,
        /// Ledger sequence number of the reservation.
        #[arg(long)]
        seq: u64,
        /// Why it is voided.
        #[arg(long)]
        reason: String,
    },
}

/// Help for `set-geofence --radius-m` (C8 R39), formatted from the C8 R19
/// constants so the text and the validation never disagree.
pub fn geofence_radius_help() -> String {
    use crate::location::{
        DEFAULT_GEOFENCE_RADIUS_METRES, MAX_GEOFENCE_RADIUS_METRES, MIN_SAVED_GEOFENCE_RADIUS_METRES,
    };
    format!(
        "Radius in whole metres, {MIN_SAVED_GEOFENCE_RADIUS_METRES} to {MAX_GEOFENCE_RADIUS_METRES}. Default {DEFAULT_GEOFENCE_RADIUS_METRES} for a new fence; an edit keeps the stored radius."
    )
}

/// Help for `set-geofence --billing-type` (C8 R39).
pub fn geofence_billing_type_help() -> String {
    format!(
        "Unit of --cost-per-unit: {} or {}. Default {} when a cost is given and no type is stored; an edit keeps the stored type unless this is given.",
        crate::hub_pack::GeofenceBillingType::PerKwh.as_str(),
        crate::hub_pack::GeofenceBillingType::PerMinute.as_str(),
        crate::hub_pack::GeofenceBillingType::PerKwh.as_str(),
    )
}

/// Help for `set-geofence --cost-per-unit` (C8 R39).
pub fn geofence_cost_per_unit_help() -> String {
    format!(
        "Price per kWh or per minute, 0 to {}. An edit keeps the stored price unless this is given.",
        crate::location::MAX_GEOFENCE_COST_PER_UNIT
    )
}

/// Help for `set-geofence --session-fee` (C8 R39).
pub fn geofence_session_fee_help() -> String {
    format!(
        "Flat fee per charging session, 0 to {}. An edit keeps the stored fee unless this is given.",
        crate::location::MAX_GEOFENCE_SESSION_FEE
    )
}

pub const IMPORT_COMMAND_HINT: &str = "Import checks use source-check; see docs/guides/cli.md.";
pub const SCHEMA_FLAG_DEPRECATION: &str = "--acknowledge-v4-2-compatible-schema is deprecated; use --accept-schema-evidence. The alias will be removed in a later version.";

/// The one-line warning for the earlier name of `source-check`.
pub fn verb_deprecation() -> String {
    format!(
        "{} is deprecated; use source-check. The alias will be removed in a later version.",
        crate::runtime::legacy_names::DEPRECATED_VERB_SOURCE_CHECK
    )
}

/// The subcommand word of an argument list: the first word after the program
/// name that is neither an option nor the value of `--config`.
fn subcommand_token(arguments: &[std::ffi::OsString]) -> Option<&std::ffi::OsStr> {
    let mut words = arguments.iter().skip(1);
    while let Some(word) = words.next() {
        if word == "--config" {
            words.next();
        } else if !word.to_string_lossy().starts_with('-') {
            return Some(word.as_os_str());
        }
    }
    None
}

impl Cli {
    pub fn try_parse_from<I, T>(arguments: I) -> Result<Self, clap::Error>
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

    pub fn parse_with_schema_warning<I, T>(
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
        let schema_flag_used = arguments
            .iter()
            .any(|argument| argument == "--acknowledge-v4-2-compatible-schema");
        let verb_used = subcommand_token(&arguments).is_some_and(|token| {
            token == crate::runtime::legacy_names::DEPRECATED_VERB_SOURCE_CHECK
        });
        let cli = Self::try_parse_from(arguments)?;
        let mut warnings = Vec::new();
        if verb_used {
            warnings.push(verb_deprecation());
        }
        if schema_flag_used {
            warnings.push(SCHEMA_FLAG_DEPRECATION.to_owned());
        }
        for warning in warnings {
            writeln!(stderr, "{warning}")
                .map_err(|error| clap::Error::raw(clap::error::ErrorKind::Io, error))?;
        }
        Ok(cli)
    }

    pub fn parse() -> Self {
        Self::parse_with_schema_warning(std::env::args_os(), &mut std::io::stderr())
            .unwrap_or_else(|error| error.exit())
    }
}
