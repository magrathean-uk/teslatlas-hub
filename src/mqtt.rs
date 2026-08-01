//! TeslaMate-compatible MQTT topic and payload projection.
//!
//! This module contains no broker connection and accepts only typed, already
//! committed summary data. Credential fields are names, never secret values.

use std::{
    fmt::Display,
    net::IpAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rumqttc::{AsyncClient, Event, EventLoop, Incoming, MqttOptions, QoS, Transport};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{task::JoinHandle, time::{sleep, timeout}};
use uuid::Uuid;

use crate::{
    config::MqttConfig,
    credentials::{CredentialDirectory, MqttCredentials},
    db::{HubStore, MqttDeliveryClaim},
};

pub const TESLAMATE_TOPIC_ROOT: &str = "teslamate";
pub const MQTT_QOS_AT_LEAST_ONCE: u8 = 1;
pub const MQTT_MAX_IN_FLIGHT: usize = 10;
const MQTT_ACK_TIMEOUT: Duration = Duration::from_millis(9_500);
const MQTT_RECONNECT_INITIAL: Duration = Duration::from_secs(1);
const MQTT_RECONNECT_MAX: Duration = Duration::from_secs(30);
const MQTT_IDLE_POLL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MqttQos {
    AtLeastOnce,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MqttPublication {
    pub topic: String,
    pub payload: String,
    pub qos: MqttQos,
    pub retain: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MqttPublishError {
    #[error("MQTT publisher unavailable")]
    Unavailable,
}

/// Broker-independent publisher boundary. A real broker adapter is deliberately
/// outside this slice; callers may execute a claimed batch concurrently, up to
/// MQTT_MAX_IN_FLIGHT, and acknowledge each field independently.
pub trait MqttPublisher {
    fn publish(&self, publication: &MqttPublication) -> Result<(), MqttPublishError>;
}

pub fn deliver_pending<P: MqttPublisher>(
    store: &HubStore,
    publisher: &P,
    now_ms: i64,
) -> Result<usize, crate::db::StoreError> {
    let claims = store.claim_mqtt_deliveries(now_ms, MQTT_MAX_IN_FLIGHT)?;
    let mut delivered = 0;
    for claim in claims {
        match publisher.publish(&claim.publication) {
            Ok(()) => {
                store.complete_mqtt_delivery(&claim)?;
                delivered += 1;
            }
            Err(_) => {
                store.fail_mqtt_delivery(&claim, "publisher_unavailable")?;
            }
        }
    }
    Ok(delivered)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MqttEndpoint {
    host: String,
    port: u16,
    tls: bool,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
enum MqttRuntimeError {
    #[error("MQTT broker endpoint is unavailable")]
    Endpoint,
    #[error("MQTT credentials are incomplete")]
    Credentials,
    #[error("MQTT broker connection failed")]
    Connection,
    #[error("MQTT broker acknowledgement timed out")]
    AckTimeout,
    #[error("MQTT durable delivery failed")]
    Store,
}

fn endpoint(config: &MqttConfig) -> Result<MqttEndpoint, MqttRuntimeError> {
    let raw = config.broker_url.as_deref().ok_or(MqttRuntimeError::Endpoint)?;
    let url = url::Url::parse(raw).map_err(|_| MqttRuntimeError::Endpoint)?;
    let host = url.host_str().ok_or(MqttRuntimeError::Endpoint)?.to_owned();
    let tls = url.scheme() == "mqtts";
    let port = url
        .port()
        .unwrap_or(if tls { 8883 } else { 1883 });
    if port == 0 || (!tls && !is_loopback_host(&host)) {
        return Err(MqttRuntimeError::Endpoint);
    }
    Ok(MqttEndpoint { host, port, tls })
}

fn options(
    config: &MqttConfig,
    credentials: &MqttCredentials,
) -> Result<MqttOptions, MqttRuntimeError> {
    let endpoint = endpoint(config)?;
    let mut options = MqttOptions::new(&config.client_id, endpoint.host, endpoint.port);
    options.set_keep_alive(Duration::from_secs(60));
    if endpoint.tls {
        options.set_transport(Transport::tls_with_default_config());
    }
    match (credentials.username(), credentials.password()) {
        (Some(username), Some(password)) => {
            options.set_credentials(username, password);
        }
        (None, None) => {}
        _ => return Err(MqttRuntimeError::Credentials),
    }
    // MqttOptions defaults to clean session and no Last-Will. Do not call a
    // will setter: TeslaMate has no MQTT LWT contract.
    Ok(options)
}

pub fn spawn_worker(store: HubStore, config: MqttConfig) -> Option<JoinHandle<()>> {
    if !config.enabled {
        return None;
    }
    Some(tokio::spawn(async move {
        let credentials = match load_credentials(&config) {
            Ok(credentials) => credentials,
            Err(_) => {
                tracing::warn!("MQTT credentials unavailable; MQTT worker disabled");
                return;
            }
        };
        run_worker(store, config, credentials).await;
    }))
}

#[cfg(test)]
pub(crate) fn spawn_worker_with_credentials(
    store: HubStore,
    config: MqttConfig,
    credentials: MqttCredentials,
) -> JoinHandle<()> {
    tokio::spawn(async move { run_worker(store, config, credentials).await })
}

fn load_credentials(config: &MqttConfig) -> Result<MqttCredentials, crate::credentials::CredentialError> {
    let names_configured = config.username_credential.is_some() || config.password_credential.is_some();
    let Some(directory) = CredentialDirectory::from_systemd_environment()? else {
        return if names_configured {
            Err(crate::credentials::CredentialError::MissingDirectory)
        } else {
            Ok(MqttCredentials { username: None, password: None })
        };
    };
    directory.mqtt_credentials(
        config.username_credential.as_deref(),
        config.password_credential.as_deref(),
    )
}

async fn run_worker(store: HubStore, config: MqttConfig, credentials: MqttCredentials) {
    crate::crypto::install_default_provider();
    let mut delay = MQTT_RECONNECT_INITIAL;
    loop {
        match run_session(&store, &config, &credentials).await {
            Ok(()) => delay = MQTT_RECONNECT_INITIAL,
            Err(error) => tracing::warn!(reason = %error, "MQTT worker disconnected"),
        }
        sleep(delay).await;
        delay = (delay * 2).min(MQTT_RECONNECT_MAX);
    }
}

async fn run_session(
    store: &HubStore,
    config: &MqttConfig,
    credentials: &MqttCredentials,
) -> Result<(), MqttRuntimeError> {
    let options = options(config, credentials)?;
    let (client, mut event_loop) = AsyncClient::new(options, MQTT_MAX_IN_FLIGHT);
    wait_for_connack(&mut event_loop).await?;
    loop {
        let now_ms = epoch_millis();
        let claims = store
            .claim_mqtt_deliveries(now_ms, MQTT_MAX_IN_FLIGHT)
            .map_err(|_| MqttRuntimeError::Store)?;
        if claims.is_empty() {
            match timeout(MQTT_IDLE_POLL, event_loop.poll()).await {
                Ok(Ok(_)) | Err(_) => continue,
                Ok(Err(_)) => return Err(MqttRuntimeError::Connection),
            }
        }
        for claim in claims {
            if let Err(_) = publish_and_wait(&client, &mut event_loop, &claim).await {
                store
                    .fail_mqtt_delivery(&claim, "transport_unavailable")
                    .map_err(|_| MqttRuntimeError::Store)?;
                return Err(MqttRuntimeError::Connection);
            }
            store
                .complete_mqtt_delivery(&claim)
                .map_err(|_| MqttRuntimeError::Store)?;
        }
    }
}

async fn wait_for_connack(
    event_loop: &mut EventLoop,
) -> Result<(), MqttRuntimeError> {
    loop {
        match timeout(MQTT_ACK_TIMEOUT, event_loop.poll()).await {
            Ok(Ok(Event::Incoming(Incoming::ConnAck(_)))) => return Ok(()),
            Ok(Ok(_)) => continue,
            Ok(Err(_)) | Err(_) => return Err(MqttRuntimeError::Connection),
        }
    }
}

async fn publish_and_wait(
    client: &AsyncClient,
    event_loop: &mut EventLoop,
    claim: &MqttDeliveryClaim,
) -> Result<(), MqttRuntimeError> {
    client
        .publish(
            claim.publication.topic.clone(),
            QoS::AtLeastOnce,
            claim.publication.retain,
            claim.publication.payload.clone(),
        )
        .await
        .map_err(|_| MqttRuntimeError::Connection)?;
    loop {
        match timeout(MQTT_ACK_TIMEOUT, event_loop.poll()).await {
            Ok(Ok(Event::Incoming(Incoming::PubAck(_)))) => return Ok(()),
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => return Err(MqttRuntimeError::Connection),
            Err(_) => return Err(MqttRuntimeError::AckTimeout),
        }
    }
}

fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

impl MqttPublication {
    fn scalar(topic: String, payload: String) -> Self {
        Self { topic, payload, qos: MqttQos::AtLeastOnce, retain: true }
    }

    pub fn clear_healthy(namespace: Option<&str>, car_id: i64) -> Result<Self, MqttProjectError> {
        Ok(Self {
            topic: topic(namespace, car_id, "healthy")?,
            payload: String::new(),
            qos: MqttQos::AtLeastOnce,
            retain: true,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct MqttSummary {
    pub vehicle_id: Uuid,
    pub car_id: i64,
    pub state: Option<String>,
    pub since: Option<OffsetDateTime>,
    pub healthy: Option<bool>,
    pub identity: MqttIdentity,
    pub position: MqttPosition,
    pub drive: MqttDrive,
    pub charge: MqttCharge,
    pub climate: MqttClimate,
    pub security: MqttSecurity,
    pub service: MqttService,
    pub hardware: MqttHardware,
    pub route: Option<MqttRoute>,
    pub geofence: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MqttIdentity {
    pub display_name: Option<String>,
    pub model: Option<String>,
    pub trim_badging: Option<String>,
    pub exterior_color: Option<String>,
    pub wheel_type: Option<String>,
    pub spoiler_type: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MqttPosition {
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub heading: Option<f64>,
    pub elevation: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct MqttDrive {
    pub speed: Option<f64>,
    pub power: Option<f64>,
    pub odometer: Option<f64>,
    pub shift_state: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MqttCharge {
    pub battery_level: Option<i64>,
    pub charging_state: Option<String>,
    pub usable_battery_level: Option<i64>,
    pub ideal_battery_range_km: Option<f64>,
    pub est_battery_range_km: Option<f64>,
    pub rated_battery_range_km: Option<f64>,
    pub charge_energy_added: Option<f64>,
    pub plugged_in: Option<bool>,
    pub scheduled_charging_start_time: Option<OffsetDateTime>,
    pub charge_limit_soc: Option<i64>,
    pub charger_power: Option<f64>,
    pub time_to_full_charge: Option<f64>,
    pub charger_phases: Option<i64>,
    pub charger_actual_current: Option<f64>,
    pub charger_voltage: Option<f64>,
    pub charge_current_request: Option<i64>,
    pub charge_current_request_max: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct MqttClimate {
    pub outside_temp: Option<f64>,
    pub inside_temp: Option<f64>,
    pub is_climate_on: Option<bool>,
    pub is_preconditioning: Option<bool>,
    pub climate_keeper_mode: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MqttSecurity {
    pub locked: Option<bool>,
    pub sentry_mode: Option<bool>,
    pub windows_open: Option<bool>,
    pub driver_front_window_open: Option<bool>,
    pub driver_rear_window_open: Option<bool>,
    pub passenger_front_window_open: Option<bool>,
    pub passenger_rear_window_open: Option<bool>,
    pub doors_open: Option<bool>,
    pub driver_front_door_open: Option<bool>,
    pub driver_rear_door_open: Option<bool>,
    pub passenger_front_door_open: Option<bool>,
    pub passenger_rear_door_open: Option<bool>,
    pub is_user_present: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct MqttService {
    pub version: Option<String>,
    pub update_available: Option<bool>,
    pub update_version: Option<String>,
    pub download_perc: Option<i64>,
    pub install_perc: Option<i64>,
    pub center_display_state: Option<String>,
    pub service_mode: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct MqttHardware {
    pub sun_roof_state: Option<String>,
    pub sun_roof_installed: Option<bool>,
    pub sun_roof_percent_open: Option<i64>,
    pub trunk_open: Option<bool>,
    pub frunk_open: Option<bool>,
    pub tpms_pressure_fl: Option<f64>,
    pub tpms_pressure_fr: Option<f64>,
    pub tpms_pressure_rl: Option<f64>,
    pub tpms_pressure_rr: Option<f64>,
    pub tpms_soft_warning_fl: Option<bool>,
    pub tpms_soft_warning_fr: Option<bool>,
    pub tpms_soft_warning_rl: Option<bool>,
    pub tpms_soft_warning_rr: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct MqttRoute {
    pub destination: Option<String>,
    pub energy_at_arrival: Option<f64>,
    pub miles_to_arrival: Option<f64>,
    pub minutes_to_arrival: Option<f64>,
    pub traffic_minutes_delay: Option<f64>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MqttProjectError {
    #[error("MQTT car ID must be positive")]
    InvalidCarId,
    #[error("MQTT namespace is invalid")]
    InvalidNamespace,
    #[error("MQTT client ID is invalid")]
    InvalidClientId,
    #[error("MQTT credential name is invalid")]
    InvalidCredentialName,
    #[error("MQTT coordinate is invalid")]
    InvalidCoordinate,
    #[error("MQTT JSON payload cannot be encoded")]
    Json,
}

pub fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

pub fn validate_namespace(namespace: Option<&str>) -> Result<(), MqttProjectError> {
    let Some(namespace) = namespace.filter(|value| !value.is_empty()) else { return Ok(()) };
    if namespace.len() > 64
        || namespace.chars().any(|character| character.is_control() || character == '/')
    {
        return Err(MqttProjectError::InvalidNamespace);
    }
    Ok(())
}

pub fn validate_client_id(client_id: &str) -> Result<(), MqttProjectError> {
    if client_id.is_empty()
        || client_id.len() > 128
        || !client_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(MqttProjectError::InvalidClientId);
    }
    Ok(())
}

pub fn validate_credential_name(name: &str) -> Result<(), MqttProjectError> {
    if name.is_empty()
        || name.len() > 128
        || name.chars().any(|character| character.is_control() || character == '/')
    {
        return Err(MqttProjectError::InvalidCredentialName);
    }
    Ok(())
}

pub fn project_summary(
    namespace: Option<&str>,
    summary: &MqttSummary,
) -> Result<Vec<MqttPublication>, MqttProjectError> {
    validate_namespace(namespace)?;
    if summary.car_id <= 0 {
        return Err(MqttProjectError::InvalidCarId);
    }
    validate_coordinates(summary.position.latitude, summary.position.longitude)?;
    let base = topic_base(namespace, summary.car_id)?;
    let mut output = Vec::new();

    macro_rules! scalar {
        ($key:literal, $value:expr) => {
            if let Some(value) = &$value {
                output.push(MqttPublication::scalar(
                    format!("{base}/{}", $key),
                    value.to_string(),
                ));
            }
        };
    }
    macro_rules! clearable {
        ($key:literal, $value:expr) => {
            if let Some(value) = &$value {
                output.push(MqttPublication::scalar(
                    format!("{base}/{}", $key),
                    value.to_string(),
                ));
            } else {
                output.push(MqttPublication::scalar(
                    format!("{base}/{}", $key),
                    String::new(),
                ));
            }
        };
    }
    macro_rules! date {
        ($key:literal, $value:expr) => {
            if let Some(value) = &$value {
                output.push(MqttPublication::scalar(
                    format!("{base}/{}", $key),
                    value.format(&Rfc3339).map_err(|_| MqttProjectError::Json)?,
                ));
            }
        };
    }

    scalar!("state", summary.state);
    date!("since", summary.since);
    scalar!("healthy", summary.healthy);

    scalar!("display_name", summary.identity.display_name);
    scalar!("model", summary.identity.model);
    clearable!("trim_badging", summary.identity.trim_badging);
    scalar!("exterior_color", summary.identity.exterior_color);
    scalar!("wheel_type", summary.identity.wheel_type);
    scalar!("spoiler_type", summary.identity.spoiler_type);

    scalar!("latitude", summary.position.latitude);
    scalar!("longitude", summary.position.longitude);
    scalar!("heading", summary.position.heading);
    scalar!("elevation", summary.position.elevation);
    if let (Some(latitude), Some(longitude)) = (summary.position.latitude, summary.position.longitude) {
        output.push(json_publication(
            format!("{base}/location"),
            json!({"latitude": latitude, "longitude": longitude}),
        )?);
    }

    scalar!("speed", summary.drive.speed);
    scalar!("power", summary.drive.power);
    scalar!("odometer", summary.drive.odometer);
    clearable!("shift_state", summary.drive.shift_state);

    scalar!("battery_level", summary.charge.battery_level);
    scalar!("charging_state", summary.charge.charging_state);
    scalar!("usable_battery_level", summary.charge.usable_battery_level);
    scalar!("ideal_battery_range_km", summary.charge.ideal_battery_range_km);
    scalar!("est_battery_range_km", summary.charge.est_battery_range_km);
    scalar!("rated_battery_range_km", summary.charge.rated_battery_range_km);
    clearable!("charge_energy_added", summary.charge.charge_energy_added);
    scalar!("plugged_in", summary.charge.plugged_in);
    date!("scheduled_charging_start_time", summary.charge.scheduled_charging_start_time);
    scalar!("charge_limit_soc", summary.charge.charge_limit_soc);
    clearable!("charger_power", summary.charge.charger_power);
    clearable!("time_to_full_charge", summary.charge.time_to_full_charge);
    clearable!("charger_phases", summary.charge.charger_phases);
    clearable!("charger_actual_current", summary.charge.charger_actual_current);
    clearable!("charger_voltage", summary.charge.charger_voltage);
    scalar!("charge_current_request", summary.charge.charge_current_request);
    scalar!("charge_current_request_max", summary.charge.charge_current_request_max);

    scalar!("outside_temp", summary.climate.outside_temp);
    scalar!("inside_temp", summary.climate.inside_temp);
    scalar!("is_climate_on", summary.climate.is_climate_on);
    scalar!("is_preconditioning", summary.climate.is_preconditioning);
    scalar!("climate_keeper_mode", summary.climate.climate_keeper_mode);

    scalar!("locked", summary.security.locked);
    scalar!("sentry_mode", summary.security.sentry_mode);
    scalar!("windows_open", summary.security.windows_open);
    scalar!("driver_front_window_open", summary.security.driver_front_window_open);
    scalar!("driver_rear_window_open", summary.security.driver_rear_window_open);
    scalar!("passenger_front_window_open", summary.security.passenger_front_window_open);
    scalar!("passenger_rear_window_open", summary.security.passenger_rear_window_open);
    scalar!("doors_open", summary.security.doors_open);
    scalar!("driver_front_door_open", summary.security.driver_front_door_open);
    scalar!("driver_rear_door_open", summary.security.driver_rear_door_open);
    scalar!("passenger_front_door_open", summary.security.passenger_front_door_open);
    scalar!("passenger_rear_door_open", summary.security.passenger_rear_door_open);
    scalar!("is_user_present", summary.security.is_user_present);

    scalar!("version", summary.service.version);
    scalar!("update_available", summary.service.update_available);
    scalar!("update_version", summary.service.update_version);
    scalar!("download_perc", summary.service.download_perc);
    scalar!("install_perc", summary.service.install_perc);
    scalar!("center_display_state", summary.service.center_display_state);
    scalar!("service_mode", summary.service.service_mode);

    scalar!("sun_roof_state", summary.hardware.sun_roof_state);
    scalar!("sun_roof_installed", summary.hardware.sun_roof_installed);
    scalar!("sun_roof_percent_open", summary.hardware.sun_roof_percent_open);
    scalar!("trunk_open", summary.hardware.trunk_open);
    scalar!("frunk_open", summary.hardware.frunk_open);
    scalar!("tpms_pressure_fl", summary.hardware.tpms_pressure_fl);
    scalar!("tpms_pressure_fr", summary.hardware.tpms_pressure_fr);
    scalar!("tpms_pressure_rl", summary.hardware.tpms_pressure_rl);
    scalar!("tpms_pressure_rr", summary.hardware.tpms_pressure_rr);
    scalar!("tpms_soft_warning_fl", summary.hardware.tpms_soft_warning_fl);
    scalar!("tpms_soft_warning_fr", summary.hardware.tpms_soft_warning_fr);
    scalar!("tpms_soft_warning_rl", summary.hardware.tpms_soft_warning_rl);
    scalar!("tpms_soft_warning_rr", summary.hardware.tpms_soft_warning_rr);

    clearable!("geofence", summary.geofence);
    project_route(&mut output, &base, summary.route.as_ref())?;
    Ok(output)
}

fn project_route(
    output: &mut Vec<MqttPublication>,
    base: &str,
    route: Option<&MqttRoute>,
) -> Result<(), MqttProjectError> {
    let (destination, destination_latitude, destination_longitude, active_route) = match route {
        Some(route) => {
            validate_coordinates(route.latitude, route.longitude)?;
            let location = match (route.latitude, route.longitude) {
                (Some(latitude), Some(longitude)) => json!({
                    "latitude": latitude,
                    "longitude": longitude,
                }),
                _ => Value::Null,
            };
            (
                route.destination.clone().unwrap_or_else(|| "nil".to_owned()),
                route.latitude.map(|value| value.to_string()).unwrap_or_else(|| "nil".to_owned()),
                route.longitude.map(|value| value.to_string()).unwrap_or_else(|| "nil".to_owned()),
                json!({
                    "destination": route.destination,
                    "energy_at_arrival": route.energy_at_arrival,
                    "miles_to_arrival": route.miles_to_arrival,
                    "minutes_to_arrival": route.minutes_to_arrival,
                    "traffic_minutes_delay": route.traffic_minutes_delay,
                    "location": location,
                    "error": Value::Null,
                }),
            )
        }
        None => (
            "nil".to_owned(),
            "nil".to_owned(),
            "nil".to_owned(),
            json!({"error": "No active route available"}),
        ),
    };
    output.push(MqttPublication::scalar(format!("{base}/destination"), destination));
    output.push(MqttPublication::scalar(
        format!("{base}/destination_latitude"),
        destination_latitude,
    ));
    output.push(MqttPublication::scalar(
        format!("{base}/destination_longitude"),
        destination_longitude,
    ));
    output.push(json_publication(format!("{base}/active_route"), active_route)?);
    Ok(())
}

fn validate_coordinates(latitude: Option<f64>, longitude: Option<f64>) -> Result<(), MqttProjectError> {
    for value in [latitude, longitude].into_iter().flatten() {
        if !value.is_finite() {
            return Err(MqttProjectError::InvalidCoordinate);
        }
    }
    if let Some(value) = latitude {
        if !(-90.0..=90.0).contains(&value) { return Err(MqttProjectError::InvalidCoordinate); }
    }
    if let Some(value) = longitude {
        if !(-180.0..=180.0).contains(&value) { return Err(MqttProjectError::InvalidCoordinate); }
    }
    Ok(())
}

fn topic_base(namespace: Option<&str>, car_id: i64) -> Result<String, MqttProjectError> {
    if car_id <= 0 { return Err(MqttProjectError::InvalidCarId); }
    validate_namespace(namespace)?;
    Ok(match namespace.filter(|value| !value.is_empty()) {
        Some(namespace) => format!("{TESLAMATE_TOPIC_ROOT}/{namespace}/cars/{car_id}"),
        None => format!("{TESLAMATE_TOPIC_ROOT}/cars/{car_id}"),
    })
}

fn topic(namespace: Option<&str>, car_id: i64, field: &str) -> Result<String, MqttProjectError> {
    Ok(format!("{}/{}", topic_base(namespace, car_id)?, field))
}

fn json_publication(topic: String, value: Value) -> Result<MqttPublication, MqttProjectError> {
    let payload = serde_json::to_string(&value).map_err(|_| MqttProjectError::Json)?;
    Ok(MqttPublication::scalar(topic, payload))
}

#[allow(dead_code)]
fn _display_is_used<T: Display>(_value: T) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn publication<'a>(values: &'a [MqttPublication], topic: &str) -> &'a MqttPublication {
        values.iter().find(|value| value.topic == topic).expect("topic")
    }

    #[test]
    fn projects_namespace_scalars_and_iso8601() {
        let summary = MqttSummary {
            car_id: 7,
            state: Some("charging".to_owned()),
            since: Some(OffsetDateTime::from_unix_timestamp(1_735_780_800).unwrap()),
            healthy: Some(true),
            identity: MqttIdentity { display_name: Some("Atlas".to_owned()), ..Default::default() },
            ..Default::default()
        };
        let values = project_summary(Some("home"), &summary).unwrap();
        assert_eq!(publication(&values, "teslamate/home/cars/7/state").payload, "charging");
        assert_eq!(publication(&values, "teslamate/home/cars/7/display_name").payload, "Atlas");
        assert_eq!(
            publication(&values, "teslamate/home/cars/7/since").payload,
            "2025-01-02T01:20:00Z"
        );
        assert_eq!(publication(&values, "teslamate/home/cars/7/state").qos, MqttQos::AtLeastOnce);
        assert!(publication(&values, "teslamate/home/cars/7/state").retain);
    }

    #[test]
    fn projects_location_and_active_route_json() {
        let summary = MqttSummary {
            car_id: 7,
            position: MqttPosition { latitude: Some(51.5), longitude: Some(-0.1), ..Default::default() },
            route: Some(MqttRoute {
                destination: Some("Home".to_owned()),
                energy_at_arrival: Some(42.0),
                latitude: Some(51.6),
                longitude: Some(-0.2),
                ..Default::default()
            }),
            ..Default::default()
        };
        let values = project_summary(None, &summary).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(
                &publication(&values, "teslamate/cars/7/location").payload
            ).unwrap(),
            json!({"latitude": 51.5, "longitude": -0.1})
        );
        let route = serde_json::from_str::<Value>(
            &publication(&values, "teslamate/cars/7/active_route").payload
        ).unwrap();
        assert_eq!(route["destination"], "Home");
        assert_eq!(route["location"]["latitude"], 51.6);
        assert_eq!(publication(&values, "teslamate/cars/7/destination").payload, "Home");
    }

    #[test]
    fn clear_fields_and_no_route_match_teslamate() {
        let summary = MqttSummary { car_id: 7, ..Default::default() };
        let values = project_summary(None, &summary).unwrap();
        assert_eq!(publication(&values, "teslamate/cars/7/geofence").payload, "");
        assert_eq!(publication(&values, "teslamate/cars/7/trim_badging").payload, "");
        assert_eq!(publication(&values, "teslamate/cars/7/shift_state").payload, "");
        assert_eq!(publication(&values, "teslamate/cars/7/destination").payload, "nil");
        assert_eq!(
            publication(&values, "teslamate/cars/7/active_route").payload,
            r#"{"error":"No active route available"}"#
        );
        let clear = MqttPublication::clear_healthy(None, 7).unwrap();
        assert_eq!(clear.payload, "");
        assert!(clear.retain);
    }

    #[test]
    fn rejects_unsafe_topic_inputs_and_redacts_no_secret_values() {
        assert_eq!(validate_namespace(Some("home/secret")), Err(MqttProjectError::InvalidNamespace));
        assert_eq!(validate_client_id("bad client"), Err(MqttProjectError::InvalidClientId));
        assert_eq!(validate_credential_name("secret/name"), Err(MqttProjectError::InvalidCredentialName));
        let summary = MqttSummary { car_id: 7, ..Default::default() };
        let debug = format!("{:?}", summary);
        assert!(!debug.contains("access_token"));
        assert!(!debug.contains("refresh_token"));
    }
}

#[cfg(test)]
mod durable_tests {
    use std::sync::{Arc, Mutex};

    use tempfile::TempDir;

    use super::*;
    use crate::{
        collector::enqueue_mqtt_after_commit,
        config::MqttConfig,
        db::{HubStore, SourceDescriptor, VehicleDescriptor},
    };

    struct MockPublisher {
        fail_topic: Arc<Mutex<Option<String>>>,
        published: Arc<Mutex<Vec<String>>>,
    }

    impl MockPublisher {
        fn new() -> Self {
            Self {
                fail_topic: Arc::new(Mutex::new(None)),
                published: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn fail_topic(&self, topic: &str) {
            *self.fail_topic.lock().expect("fail lock") = Some(topic.to_owned());
        }

        fn clear_failure(&self) {
            *self.fail_topic.lock().expect("fail lock") = None;
        }
    }

    impl MqttPublisher for MockPublisher {
        fn publish(&self, publication: &MqttPublication) -> Result<(), MqttPublishError> {
            self.published
                .lock()
                .expect("published lock")
                .push(publication.topic.clone());
            if self
                .fail_topic
                .lock()
                .expect("fail lock")
                .as_deref()
                == Some(publication.topic.as_str())
            {
                Err(MqttPublishError::Unavailable)
            } else {
                Ok(())
            }
        }
    }

    fn registered_store() -> (TempDir, HubStore, Uuid, Uuid) {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("mqtt_test", "account"), 1)
            .expect("source");
        let first = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "7")
                    .with_tesla_identity(Some(7), None),
                1,
            )
            .expect("first vehicle")
            .vehicle_id;
        let second = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "8")
                    .with_tesla_identity(Some(8), None),
                1,
            )
            .expect("second vehicle")
            .vehicle_id;
        (temporary, store, first, second)
    }

    fn summary(vehicle_id: Uuid, car_id: i64) -> MqttSummary {
        MqttSummary {
            vehicle_id,
            car_id,
            state: Some("online".to_owned()),
            healthy: Some(true),
            position: MqttPosition {
                latitude: Some(51.5 + car_id as f64 / 100.0),
                longitude: Some(-0.1),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn disabled_and_precommit_hooks_do_not_enqueue() {
        let (_temporary, store, vehicle_id, _) = registered_store();
        let summary = summary(vehicle_id, 7);
        let disabled = MqttConfig::default();
        enqueue_mqtt_after_commit(&store, &disabled, &summary, true, 10).expect("disabled hook");
        assert!(store.load_mqtt_summary(vehicle_id).expect("load").is_none());

        let enabled = MqttConfig {
            enabled: true,
            broker_url: Some("mqtts://broker.example.test:8883".to_owned()),
            ..MqttConfig::default()
        };
        enqueue_mqtt_after_commit(&store, &enabled, &summary, false, 11).expect("precommit hook");
        assert!(store.load_mqtt_summary(vehicle_id).expect("load").is_none());
        enqueue_mqtt_after_commit(&store, &enabled, &summary, true, 12).expect("postcommit hook");
        assert_eq!(store.load_mqtt_summary(vehicle_id).expect("load").unwrap().revision, 1);
    }

    #[test]
    fn two_cars_coalesce_without_cross_talk() {
        let (_temporary, store, first, second) = registered_store();
        store
            .enqueue_mqtt_summary(None, &summary(first, 7), true, 10)
            .expect("first summary");
        store
            .enqueue_mqtt_summary(None, &summary(second, 8), true, 10)
            .expect("second summary");
        let connection = store.open().expect("open");
        let first_topics: Vec<String> = connection
            .prepare("SELECT topic FROM mqtt_delivery_state WHERE vehicle_id = ?1")
            .expect("statement")
            .query_map([first.to_string()], |row| row.get(0))
            .expect("rows")
            .collect::<Result<_, _>>()
            .expect("topics");
        let second_topics: Vec<String> = connection
            .prepare("SELECT topic FROM mqtt_delivery_state WHERE vehicle_id = ?1")
            .expect("statement")
            .query_map([second.to_string()], |row| row.get(0))
            .expect("rows")
            .collect::<Result<_, _>>()
            .expect("topics");
        assert!(first_topics.iter().all(|topic| topic.contains("/cars/7/")));
        assert!(second_topics.iter().all(|topic| topic.contains("/cars/8/")));
    }

    #[test]
    fn failed_field_retries_successful_fields_stay_delivered_and_restart_recovers() {
        let (temporary, store, vehicle_id, _) = registered_store();
        store
            .enqueue_mqtt_summary(None, &summary(vehicle_id, 7), true, 10)
            .expect("summary");
        let publisher = MockPublisher::new();
        publisher.fail_topic("teslamate/cars/7/healthy");
        let delivered = deliver_pending(&store, &publisher, 20).expect("first delivery");
        assert_eq!(delivered, MQTT_MAX_IN_FLIGHT - 1);
        let connection = store.open().expect("open");
        let pending: i64 = connection
            .query_row(
                "SELECT pending FROM mqtt_delivery_state
                 WHERE vehicle_id = ?1 AND field = 'healthy'",
                [vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("healthy state");
        assert_eq!(pending, 1);
        let delivered_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM mqtt_delivery_state
                 WHERE vehicle_id = ?1 AND pending = 0",
                [vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("delivered count");
        assert_eq!(delivered_count, i64::try_from(MQTT_MAX_IN_FLIGHT - 1).unwrap());

        publisher.clear_failure();
        let restarted = HubStore::initialize(temporary.path()).expect("restart");
        for attempt in 0..8 {
            let _ = deliver_pending(&restarted, &publisher, 100 + attempt * 60_000)
                .expect("retry delivery");
        }
        let connection = restarted.open().expect("reopen");
        let remaining: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM mqtt_delivery_state
                 WHERE vehicle_id = ?1 AND pending = 1",
                [vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("remaining");
        assert_eq!(remaining, 0);
    }
}

#[cfg(test)]
mod transport_tests {
    use tempfile::tempdir;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        time::{Duration, timeout},
    };

    use super::*;
    use crate::{
        credentials::MqttCredentials,
        db::{HubStore, SourceDescriptor, VehicleDescriptor},
    };

    fn test_store() -> (tempfile::TempDir, HubStore, Uuid) {
        let temporary = tempdir().expect("temporary directory");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("mqtt_transport_test", "account"), 1)
            .expect("source");
        let vehicle = store
            .register_vehicle(
                &VehicleDescriptor::new(source.source_id, "7")
                    .with_tesla_identity(Some(7), None),
                1,
            )
            .expect("vehicle")
            .vehicle_id;
        (temporary, store, vehicle)
    }

    fn test_summary(vehicle_id: Uuid) -> MqttSummary {
        MqttSummary {
            vehicle_id,
            car_id: 7,
            state: Some("online".to_owned()),
            healthy: Some(true),
            ..Default::default()
        }
    }

    async fn read_packet(stream: &mut TcpStream) -> Vec<u8> {
        let mut packet = Vec::with_capacity(64);
        let mut header = [0_u8; 1];
        stream.read_exact(&mut header).await.expect("header");
        packet.push(header[0]);
        let mut remaining = 0_usize;
        let mut multiplier = 1_usize;
        loop {
            let mut byte = [0_u8; 1];
            stream.read_exact(&mut byte).await.expect("length");
            packet.push(byte[0]);
            remaining += usize::from(byte[0] & 0x7f) * multiplier;
            if byte[0] & 0x80 == 0 { break; }
            multiplier *= 128;
        }
        let start = packet.len();
        packet.resize(start + remaining, 0);
        stream.read_exact(&mut packet[start..]).await.expect("body");
        packet
    }

    fn body(packet: &[u8]) -> &[u8] {
        let mut index = 1;
        while packet[index] & 0x80 != 0 { index += 1; }
        &packet[index + 1..]
    }

    fn read_utf8(bytes: &[u8], cursor: &mut usize) -> String {
        let length = usize::from(u16::from_be_bytes([bytes[*cursor], bytes[*cursor + 1]]));
        *cursor += 2;
        let value = String::from_utf8(bytes[*cursor..*cursor + length].to_vec()).expect("utf8");
        *cursor += length;
        value
    }

    fn assert_connect(packet: &[u8]) {
        assert_eq!(packet[0], 0x10, "must be MQTT CONNECT");
        let bytes = body(packet);
        let mut cursor = 0;
        assert_eq!(read_utf8(bytes, &mut cursor), "MQTT");
        assert_eq!(bytes[cursor], 4);
        cursor += 1;
        let flags = bytes[cursor];
        cursor += 1;
        assert_eq!(flags & 0x3c, 0, "CONNECT must have no LWT");
        cursor += 2;
        let client_id = read_utf8(bytes, &mut cursor);
        assert!(!client_id.is_empty());
        if flags & 0x80 != 0 {
            assert_eq!(read_utf8(bytes, &mut cursor), "mqtt-user");
        }
        if flags & 0x40 != 0 {
            assert_eq!(read_utf8(bytes, &mut cursor), "mqtt-password");
        }
    }

    fn publish_topic(packet: &[u8]) -> String {
        let bytes = body(packet);
        let length = usize::from(u16::from_be_bytes([bytes[0], bytes[1]]));
        String::from_utf8(bytes[2..2 + length].to_vec()).expect("topic")
    }

    fn publish_packet_id(packet: &[u8]) -> [u8; 2] {
        let bytes = body(packet);
        let length = usize::from(u16::from_be_bytes([bytes[0], bytes[1]]));
        let cursor = 2 + length;
        [bytes[cursor], bytes[cursor + 1]]
    }

    fn config(port: u16, tls: bool) -> MqttConfig {
        MqttConfig {
            enabled: true,
            broker_url: Some(format!("mqtt{}://127.0.0.1:{port}", if tls { "s" } else { "" })),
            username_credential: Some("mqtt-user".to_owned()),
            password_credential: Some("mqtt-password".to_owned()),
            ..MqttConfig::default()
        }
    }

    #[tokio::test]
    async fn real_transport_sends_credentials_qos1_retain_and_no_lwt() {
        let (_temporary, store, vehicle_id) = test_store();
        store
            .enqueue_mqtt_summary(None, &test_summary(vehicle_id), true, 10)
            .expect("summary");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let port = listener.local_addr().expect("address").port();
        let broker = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            assert_connect(&read_packet(&mut socket).await);
            socket.write_all(&[0x20, 0x02, 0x00, 0x00]).await.expect("connack");
            let publish = read_packet(&mut socket).await;
            assert_eq!(publish[0] & 0x0f, 0x03, "QoS1 + retain");
            assert_eq!(publish_topic(&publish), "teslamate/cars/7/healthy");
            let packet_id = publish_packet_id(&publish);
            socket.write_all(&[0x40, 0x02, packet_id[0], packet_id[1]]).await.expect("puback");
        });
        let credentials = MqttCredentials::for_test(Some("mqtt-user"), Some("mqtt-password"));
        assert!(!format!("{credentials:?}").contains("mqtt-password"));
        let worker = spawn_worker_with_credentials(store, config(port, false), credentials);
        timeout(Duration::from_secs(3), broker).await.expect("broker timeout").expect("broker");
        worker.abort();
        let _ = worker.await;
    }

    #[tokio::test]
    async fn dropped_connection_reconnects_and_retries_unacked_field_only() {
        let (_temporary, store, vehicle_id) = test_store();
        store
            .enqueue_mqtt_summary(None, &test_summary(vehicle_id), true, 10)
            .expect("summary");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let port = listener.local_addr().expect("address").port();
        let broker = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.expect("first accept");
            assert_connect(&read_packet(&mut first).await);
            first.write_all(&[0x20, 0x02, 0x00, 0x00]).await.expect("connack");
            let first_publish = read_packet(&mut first).await;
            let topic = publish_topic(&first_publish);
            drop(first);
            let (mut second, _) = listener.accept().await.expect("second accept");
            assert_connect(&read_packet(&mut second).await);
            second.write_all(&[0x20, 0x02, 0x00, 0x00]).await.expect("connack");
            let retry = read_packet(&mut second).await;
            assert_eq!(publish_topic(&retry), topic);
            let packet_id = publish_packet_id(&retry);
            second.write_all(&[0x40, 0x02, packet_id[0], packet_id[1]]).await.expect("puback");
        });
        let credentials = MqttCredentials::for_test(Some("mqtt-user"), Some("mqtt-password"));
        let worker = spawn_worker_with_credentials(store, config(port, false), credentials);
        timeout(Duration::from_secs(5), broker).await.expect("broker timeout").expect("broker");
        worker.abort();
        let _ = worker.await;
    }

    #[tokio::test]
    async fn tls_refuses_plaintext_listener_and_collection_still_persists() {
        let (_temporary, store, vehicle_id) = test_store();
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let port = listener.local_addr().expect("address").port();
        let broker = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut first = [0_u8; 1];
            socket.read_exact(&mut first).await.expect("TLS byte");
            assert_ne!(first[0], 0x10, "TLS must not send plaintext CONNECT");
        });
        let credentials = MqttCredentials::for_test(Some("mqtt-user"), Some("mqtt-password"));
        let worker = spawn_worker_with_credentials(store.clone(), config(port, true), credentials);
        timeout(Duration::from_secs(3), broker).await.expect("broker timeout").expect("broker");
        let before = store.load_mqtt_summary(vehicle_id).expect("load before");
        assert!(before.is_none());
        store
            .enqueue_mqtt_summary(None, &test_summary(vehicle_id), true, 10)
            .expect("collection persistence");
        assert!(store.load_mqtt_summary(vehicle_id).expect("load after").is_some());
        worker.abort();
        let _ = worker.await;
    }
}
