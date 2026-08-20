//! Bounded TeslaMate-compatible GPX drive export.

use std::io::{self, Write};

use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    db::{HubStore, StoreError},
    hub_pack::ProjectionPosition,
};

const POSITION_PAGE_SIZE: u32 = 2_048;

#[derive(Debug, Error)]
pub enum GpxError {
    #[error("drive {0} was not found")]
    DriveNotFound(i64),
    #[error("drive timestamp is outside the GPX date range")]
    InvalidTimestamp,
    #[error("cannot format GPX timestamp")]
    FormatTimestamp(#[source] time::error::Format),
    #[error("cannot access drive history")]
    Store(#[from] StoreError),
    #[error("cannot write GPX")]
    Io(#[from] io::Error),
}

pub fn export_drive_gpx<W: Write>(
    store: &HubStore,
    vehicle_id: Uuid,
    drive_id: i64,
    writer: &mut W,
) -> Result<(), GpxError> {
    let drive = store
        .materialised_drive_for_vehicle(vehicle_id, drive_id)?
        .ok_or(GpxError::DriveNotFound(drive_id))?;
    let name = timestamp(drive.start_date_ms)?;
    writeln!(writer, "<?xml version=\"1.0\"?>")?;
    writeln!(
        writer,
        "<gpx xmlns=\"http://www.topografix.com/GPX/1/1\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" version=\"1.1\" creator=\"Teslatlas Hub\" xsi:schemaLocation=\"http://www.topografix.com/GPX/1/1 http://www.topografix.com/GPX/1/1/gpx.xsd\">"
    )?;
    writeln!(writer, "  <trk>")?;
    writeln!(writer, "    <name>{name}</name>")?;
    writeln!(writer, "    <trkseg>")?;

    let mut after = None;
    loop {
        let positions =
            store.drive_positions_page(vehicle_id, drive_id, after, POSITION_PAGE_SIZE)?;
        if positions.is_empty() {
            break;
        }
        for position in &positions {
            write_position(writer, position)?;
        }
        let last = positions.last().expect("nonempty position page");
        after = Some((last.date_ms, last.id));
        if positions.len() < POSITION_PAGE_SIZE as usize {
            break;
        }
    }

    writeln!(writer, "    </trkseg>")?;
    writeln!(writer, "  </trk>")?;
    writeln!(writer, "</gpx>")?;
    Ok(())
}

fn write_position<W: Write>(writer: &mut W, position: &ProjectionPosition) -> Result<(), GpxError> {
    writeln!(
        writer,
        "      <trkpt lat=\"{}\" lon=\"{}\">",
        position.latitude, position.longitude
    )?;
    if let Some(elevation) = position.elevation {
        writeln!(writer, "        <ele>{elevation}</ele>")?;
    }
    writeln!(
        writer,
        "        <time>{}</time>",
        timestamp(position.date_ms)?
    )?;
    writeln!(writer, "      </trkpt>")?;
    Ok(())
}

fn timestamp(epoch_ms: i64) -> Result<String, GpxError> {
    let nanos = i128::from(epoch_ms)
        .checked_mul(1_000_000)
        .ok_or(GpxError::InvalidTimestamp)?;
    OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .map_err(|_| GpxError::InvalidTimestamp)?
        .format(&Rfc3339)
        .map_err(GpxError::FormatTimestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(id: i64, elevation: Option<i64>) -> ProjectionPosition {
        ProjectionPosition {
            id,
            drive_id: Some(7),
            car_id: 1,
            date_ms: 1_700_000_000_000 + id,
            latitude: 47.5,
            longitude: 19.0,
            speed: None,
            power: None,
            battery_level: None,
            usable_battery_level: None,
            elevation,
            odometer: None,
            ideal_battery_range_km: None,
            est_battery_range_km: None,
            rated_battery_range_km: None,
            fan_status: None,
            driver_temp_setting: None,
            passenger_temp_setting: None,
            is_climate_on: None,
            is_rear_defroster_on: None,
            is_front_defroster_on: None,
            inside_temp: None,
            outside_temp: None,
            battery_heater: None,
            battery_heater_on: None,
            battery_heater_no_power: None,
            tpms_pressure_fl: None,
            tpms_pressure_fr: None,
            tpms_pressure_rl: None,
            tpms_pressure_rr: None,
        }
    }

    #[test]
    fn position_xml_matches_the_teslamate_gpx_surface() {
        let mut output = Vec::new();
        write_position(&mut output, &position(0, Some(123))).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("<trkpt lat=\"47.5\" lon=\"19\">"));
        assert!(output.contains("<ele>123</ele>"));
        assert!(output.contains("<time>2023-11-14T22:13:20Z</time>"));

        let mut output = Vec::new();
        write_position(&mut output, &position(1, None)).unwrap();
        assert!(!String::from_utf8(output).unwrap().contains("<ele>"));
    }
}
