// SPDX-License-Identifier: AGPL-3.0-only

//! Optional TeslaMate current telemetry; never touches Tesla authentication or commands.
use crate::{
    config::TeslaMateCurrentConfig,
    credentials::TeslaMatePostgresPassword,
    db::{HubStore, ObservationInput},
    runtime::development_event_log::{self, Event, Kind, Outcome},
    teslamate::ReadOnlySource,
    teslamate_reader::{TeslaMateCurrentPosition, read_current_position},
};
use std::{
    fs::File,
    io::Read,
    os::unix::fs::MetadataExt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_AGE_MS: i64 = 300_000;
const POLL_SECONDS: u64 = 30;

fn observation(sample: &TeslaMateCurrentPosition, now_ms: i64) -> Option<serde_json::Value> {
    if sample.position_id <= 0
        || sample.car_id <= 0
        || sample.observed_at_ms < 0
        || sample.observed_at_ms > now_ms
        || sample.observed_at_ms > sample.source_now_ms
        || now_ms.saturating_sub(sample.observed_at_ms) > MAX_AGE_MS
        || sample.source_now_ms.saturating_sub(now_ms).unsigned_abs() > 120_000
    {
        return None;
    }
    let battery = sample
        .battery_level
        .filter(|value| (0..=100).contains(value));
    let bounded = |value: Option<f64>, maximum: f64| {
        value.filter(|value| value.is_finite() && *value >= 0.0 && *value <= maximum)
    };
    let ideal = bounded(sample.ideal_range_km, 2000.0);
    let est = bounded(sample.est_range_km, 2000.0);
    let rated = bounded(sample.rated_range_km, 2000.0);
    let odometer = bounded(sample.odometer_km, 10_000_000.0);
    if battery.is_none()
        && ideal.is_none()
        && est.is_none()
        && rated.is_none()
        && odometer.is_none()
    {
        return None;
    }
    let mut fields = serde_json::Map::new();
    fields.insert(
        "record_type".into(),
        serde_json::json!("teslamate_position_v1"),
    );
    fields.insert("position_id".into(), serde_json::json!(sample.position_id));
    if let Some(value) = battery {
        fields.insert("battery_level".into(), serde_json::json!(value));
    }
    for (key, value) in [
        ("ideal_battery_range_km", ideal),
        ("est_battery_range_km", est),
        ("rated_battery_range_km", rated),
        ("odometer_km", odometer),
    ] {
        if let Some(value) = value {
            fields.insert(key.into(), serde_json::json!(value));
        }
    }
    Some(serde_json::Value::Object(fields))
}

fn password(path: &std::path::Path) -> Result<TeslaMatePostgresPassword, ()> {
    use rustix::fs::{Mode, OFlags, openat};
    let directory =
        development_event_log::validated_directory(path.parent().ok_or(())?).map_err(|_| ())?;
    let fd = openat(
        &directory,
        path.file_name().ok_or(())?,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| ())?;
    let file = File::from(fd);
    let initial = file.metadata().map_err(|_| ())?;
    if !initial.is_file()
        || initial.uid() != rustix::process::getuid().as_raw()
        || initial.mode() & 0o777 != 0o600
        || initial.nlink() != 1
    {
        return Err(());
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    (&file)
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    let final_stat = file.metadata().map_err(|_| ())?;
    if bytes.len() > 16 * 1024
        || initial.len() != final_stat.len()
        || initial.mtime() != final_stat.mtime()
        || initial.mtime_nsec() != final_stat.mtime_nsec()
        || initial.mode() != final_stat.mode()
        || initial.nlink() != final_stat.nlink()
    {
        return Err(());
    }
    TeslaMatePostgresPassword::from_bytes(&bytes).map_err(|_| ())
}

async fn poll(
    store: &HubStore,
    config: &TeslaMateCurrentConfig,
    admission: &crate::hub_user_process::AdmittedUserHub,
) -> Result<bool, ()> {
    let source = ReadOnlySource::parse(&config.source_url).map_err(|_| ())?;
    let password = password(&config.password_file)?;
    let Some(sample) = read_current_position(&source, &password, config.car_id)
        .await
        .map_err(|_| ())?
    else {
        return Ok(false);
    };
    if sample.car_id != config.car_id {
        return Err(());
    }
    let stable_key = if sample.eid > 0 {
        format!("eid:{}", sample.eid)
    } else {
        format!(
            "vin:{}",
            sample
                .vin
                .as_deref()
                .filter(|vin| !vin.trim().is_empty())
                .ok_or(())?
        )
    };
    let source_id = store
        .teslamate_current_source(
            &config.source_key,
            config.vehicle_id,
            &stable_key,
            sample.vin.as_deref(),
        )
        .map_err(|_| ())?
        .ok_or(())?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ())?
        .as_millis()
        .try_into()
        .map_err(|_| ())?;
    let Some(payload) = observation(&sample, now_ms) else {
        return Ok(false);
    };
    admission.assert_sensitive_access().map_err(|_| ())?;
    store
        .record_teslamate_current(
            &ObservationInput {
                source_id,
                vehicle_id: config.vehicle_id,
                observed_at_ms: sample.observed_at_ms,
                payload,
            },
            now_ms,
        )
        .map_err(|_| ())
}

#[cfg(unix)]
pub(crate) async fn run<F: std::future::Future<Output = ()>>(
    store: HubStore,
    config: TeslaMateCurrentConfig,
    admission: std::sync::Arc<crate::hub_user_process::AdmittedUserHub>,
    shutdown: F,
) {
    tokio::pin!(shutdown);
    let mut failures = 0_u32;
    let mut delay = Duration::ZERO;
    loop {
        tokio::select! { () = &mut shutdown => return, () = tokio::time::sleep(delay) => {} }
        if admission.assert_sensitive_access().is_err() {
            return;
        }
        let started = std::time::Instant::now();
        let result = tokio::select! {
            () = &mut shutdown => return,
            result = tokio::time::timeout(Duration::from_secs(15), poll(&store, &config, &admission)) => result.unwrap_or(Err(())),
        };
        let outcome = match result {
            Ok(true) => Outcome::Complete,
            Ok(false) => Outcome::NoOp,
            Err(()) => Outcome::Failed,
        };
        let mut event = Event::new(Kind::Current, outcome);
        event.duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        event.rows = u64::from(matches!(result, Ok(true)));
        development_event_log::record(event);
        failures = if result.is_ok() {
            0
        } else {
            failures.saturating_add(1)
        };
        delay = Duration::from_secs((POLL_SECONDS * (1_u64 << failures.min(4))).min(300));
    }
}

#[cfg(test)]
#[path = "teslamate_current/tests.rs"]
mod tests;
