// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use tempfile::TempDir;
use uuid::Uuid;

use crate::{
    hub_pack::{
        ProjectionBinding, ProjectionFixedNumericV2_2, ProjectionPackRequest, ProjectionPackWriter,
    },
    protocol::SequenceRange,
};

fn history() -> TeslaMateHistory {
    TeslaMateHistory {
        cars: vec![
            TeslaMateCar {
                id: 1,
                eid: 101,
                vid: Some(201),
                vin: Some("5YJTESTVIN1234567".into()),
                name: Some("Road car".into()),
                model: Some("Model 3".into()),
                trim_badging: None,
                marketing_name: Some("Model 3 Highland".into()),
                exterior_color: Some("Pearl White".into()),
                wheel_type: Some("Apollo".into()),
                spoiler_type: Some("None".into()),
                efficiency_wh_per_km: Some(145.0),
                settings: Default::default(),
            },
            TeslaMateCar {
                id: 2,
                eid: 102,
                vid: Some(202),
                vin: None,
                name: Some("Other".into()),
                model: Some("Model Y".into()),
                trim_badging: None,
                marketing_name: None,
                exterior_color: None,
                wheel_type: None,
                spoiler_type: None,
                efficiency_wh_per_km: None,
                settings: Default::default(),
            },
        ],
        drives: vec![
            TeslaMateDrive {
                id: 10,
                car_id: 1,
                start_date_ms: 1_700_000_000_000,
                end_date_ms: Some(1_700_000_060_000),
                start_position_id: Some(20),
                end_position_id: Some(20),
                start_address_id: Some(100),
                end_address_id: Some(100),
                start_geofence_id: Some(200),
                end_geofence_id: Some(200),
                outside_temp_avg: Some(18.0),
                inside_temp_avg: Some(21.0),
                speed_max: Some(80),
                power_max: Some(36.0),
                power_min: Some(-7.0),
                start_ideal_range_km: Some(338.8),
                end_ideal_range_km: Some(334.8),
                start_rated_range_km: Some(400.0),
                end_rated_range_km: Some(390.0),
                start_km: Some(10_000.0),
                end_km: Some(10_012.0),
                distance_km: Some(12.0),
                duration_min: Some(10),
                ascent: Some(60),
                descent: Some(30),
            },
            TeslaMateDrive {
                id: 11,
                car_id: 1,
                start_date_ms: 1_700_000_100_000,
                end_date_ms: None,
                start_position_id: None,
                end_position_id: None,
                start_address_id: None,
                end_address_id: None,
                start_geofence_id: None,
                end_geofence_id: None,
                outside_temp_avg: None,
                inside_temp_avg: None,
                speed_max: None,
                power_max: None,
                power_min: None,
                start_ideal_range_km: None,
                end_ideal_range_km: None,
                start_rated_range_km: None,
                end_rated_range_km: None,
                start_km: None,
                end_km: None,
                distance_km: None,
                duration_min: None,
                ascent: None,
                descent: None,
            },
        ],
        positions: vec![
            TeslaMatePosition {
                id: 20,
                car_id: 1,
                drive_id: Some(10),
                date_ms: 1_700_000_030_000,
                latitude: 51.5,
                longitude: -0.1,
                elevation: Some(20),
                speed: Some(50),
                power: Some(10.0),
                odometer: Some(10_006.0),
                ideal_battery_range_km: Some(390.0),
                est_battery_range_km: Some(385.0),
                rated_battery_range_km: Some(389.0),
                battery_level: Some(78),
                usable_battery_level: Some(77),
                fan_status: Some(2),
                driver_temp_setting: Some(21.5),
                passenger_temp_setting: Some(22.0),
                is_climate_on: Some(false),
                is_rear_defroster_on: Some(false),
                is_front_defroster_on: Some(true),
                outside_temp: Some(18.0),
                inside_temp: Some(20.0),
                battery_heater: Some(true),
                battery_heater_on: Some(true),
                battery_heater_no_power: Some(false),
                tpms_pressure_fl: Some(2.4),
                tpms_pressure_fr: Some(2.5),
                tpms_pressure_rl: Some(2.6),
                tpms_pressure_rr: Some(2.7),
            },
            TeslaMatePosition {
                id: 21,
                car_id: 1,
                drive_id: Some(11),
                date_ms: 1_700_000_110_000,
                latitude: 51.5,
                longitude: -0.1,
                elevation: None,
                speed: None,
                power: None,
                odometer: None,
                ideal_battery_range_km: None,
                est_battery_range_km: None,
                rated_battery_range_km: None,
                battery_level: None,
                usable_battery_level: None,
                fan_status: None,
                driver_temp_setting: None,
                passenger_temp_setting: None,
                is_climate_on: None,
                is_rear_defroster_on: None,
                is_front_defroster_on: None,
                outside_temp: None,
                inside_temp: None,
                battery_heater: None,
                battery_heater_on: None,
                battery_heater_no_power: None,
                tpms_pressure_fl: None,
                tpms_pressure_fr: None,
                tpms_pressure_rl: None,
                tpms_pressure_rr: None,
            },
        ],
        charging_processes: vec![TeslaMateChargingProcess {
            id: 30,
            car_id: 1,
            position_id: Some(20),
            address_id: Some(100),
            geofence_id: Some(200),
            start_date_ms: 1_700_001_000_000,
            end_date_ms: Some(1_700_001_360_000),
            charge_energy_added: Some(20.0),
            charge_energy_used_kwh: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            cost: Some(9.99),
            start_battery_level: Some(50),
            end_battery_level: Some(80),
            duration_min: Some(60),
            outside_temp_avg: Some(18.0),
            start_rated_range_km: Some(250.0),
            end_rated_range_km: Some(400.0),
        }],
        charges: vec![TeslaMateCharge {
            id: 40,
            charging_process_id: 30,
            date_ms: 1_700_001_100_000,
            battery_heater: Some(false),
            battery_heater_on: Some(false),
            battery_heater_no_power: Some(false),
            battery_level: Some(60),
            usable_battery_level: Some(59),
            charge_energy_added_kwh: Some(6.0),
            charger_actual_current: Some(30.0),
            charger_phases: Some(1),
            charger_pilot_current: Some(32.0),
            charger_power_kw: Some(7.0),
            charger_voltage: Some(230.0),
            charge_cable: Some("Type 2".into()),
            fast_charger_present: Some(false),
            fast_charger_brand: None,
            fast_charger_type: None,
            ideal_range_km: Some(300.0),
            rated_range_km: Some(298.0),
            not_enough_power_to_heat: Some(false),
            outside_temp_c: Some(18.0),
        }],
        addresses: vec![TeslaMateAddress {
            id: 100,
            display_name: Some("Home, London".into()),
            name: Some("Home".into()),
        }],
        geofences: vec![TeslaMateGeofence {
            id: 200,
            name: "Home".into(),
            latitude: Some(51.0),
            longitude: Some(-0.1),
            radius_m: Some(100.0),
            billing_type: Some(GeofenceBillingType::PerKwh),
            cost_per_unit: Some(0.30),
            session_fee: Some(2.0),
        }],
        states: vec![],
        updates: vec![TeslaMateUpdate {
            id: 300,
            car_id: 1,
            start_date_ms: 1_699_000_000_000,
            end_date_ms: Some(1_699_000_060_000),
            version: Some("2026.20.1".into()),
        }],
    }
}

#[test]
fn address_v2_2_physical_conversion_preserves_every_non_sensitive_field() {
    let projected: ProjectionAddressV2_2 = TeslaMateAddressPhysicalV2_2 {
        id: 100,
        display_name: Some("Home, London".into()),
        latitude_e6: Some(ProjectionFixedNumericV2_2::Finite(51_500_123)),
        longitude_e6: Some(ProjectionFixedNumericV2_2::NaN),
        name: Some("Home".into()),
        house_number: Some("1".into()),
        road: Some("Example Road".into()),
        neighbourhood: Some("Westminster".into()),
        city: Some("London".into()),
        county: Some("Greater London".into()),
        postcode: Some("SW1A 1AA".into()),
        state: Some("England".into()),
        state_district: Some("London".into()),
        country: Some("United Kingdom".into()),
        inserted_at_pg_us: 1_700_000_000_000_000,
        updated_at_pg_us: 1_700_000_100_000_000,
        osm_id: Some(-42),
        osm_type: Some("node".into()),
    }
    .into();
    assert_eq!(
        projected,
        ProjectionAddressV2_2 {
            id: 100,
            display_name: Some("Home, London".into()),
            latitude_e6: Some(ProjectionFixedNumericV2_2::Finite(51_500_123)),
            longitude_e6: Some(ProjectionFixedNumericV2_2::NaN),
            name: Some("Home".into()),
            house_number: Some("1".into()),
            road: Some("Example Road".into()),
            neighbourhood: Some("Westminster".into()),
            city: Some("London".into()),
            county: Some("Greater London".into()),
            postcode: Some("SW1A 1AA".into()),
            state: Some("England".into()),
            state_district: Some("London".into()),
            country: Some("United Kingdom".into()),
            inserted_at_pg_us: 1_700_000_000_000_000,
            updated_at_pg_us: 1_700_000_100_000_000,
            osm_id: Some(-42),
            osm_type: Some("node".into()),
        }
    );
}

#[test]
fn global_settings_v2_2_physical_conversion_preserves_singleton_values() {
    let projected: ProjectionGlobalSettingsV2_2 = TeslaMateSettingsPhysicalV2_2 {
        id: i64::MIN,
        unit_of_length: ProjectionUnitOfLengthV2_2::Miles,
        unit_of_temperature: ProjectionUnitOfTemperatureV2_2::Fahrenheit,
        unit_of_pressure: ProjectionUnitOfPressureV2_2::Psi,
        preferred_range: ProjectionPreferredRangeV2_2::Ideal,
        base_url: Some("é".repeat(255)),
        grafana_url: None,
        language: String::new(),
        theme_mode: String::new(),
        inserted_at_pg_us: i64::MIN,
        updated_at_pg_us: i64::MAX,
    }
    .into();
    assert_eq!(projected.id, i64::MIN);
    assert_eq!(projected.unit_of_length, ProjectionUnitOfLengthV2_2::Miles);
    assert_eq!(
        projected.unit_of_temperature,
        ProjectionUnitOfTemperatureV2_2::Fahrenheit
    );
    assert_eq!(
        projected.unit_of_pressure,
        ProjectionUnitOfPressureV2_2::Psi
    );
    assert_eq!(
        projected.preferred_range,
        ProjectionPreferredRangeV2_2::Ideal
    );
    assert_eq!(projected.base_url.as_deref().unwrap().chars().count(), 255);
    assert_eq!(projected.grafana_url, None);
    assert!(projected.language.is_empty());
    assert!(projected.theme_mode.is_empty());
    assert_eq!(projected.inserted_at_pg_us, i64::MIN);
    assert_eq!(projected.updated_at_pg_us, i64::MAX);
}

#[test]
fn cars_and_car_settings_v2_2_physical_conversion_preserves_source_widths() {
    let settings: ProjectionCarSettingsV2_2 = TeslaMateCarSettingsPhysicalV2_2 {
        id: i64::MIN,
        suspend_min: i32::MIN,
        suspend_after_idle_min: i32::MAX,
        req_not_unlocked: true,
        free_supercharging: false,
        use_streaming_api: true,
        enabled: true,
        lfp_battery: false,
    }
    .into();
    assert_eq!(
        settings,
        ProjectionCarSettingsV2_2 {
            id: i64::MIN,
            suspend_min: i32::MIN,
            suspend_after_idle_min: i32::MAX,
            req_not_unlocked: true,
            free_supercharging: false,
            use_streaming_api: true,
            enabled: true,
            lfp_battery: false,
        }
    );

    let car: ProjectionCarV2_2 = TeslaMateCarPhysicalV2_2 {
        id: i16::MIN,
        eid: i64::MIN,
        vid: i64::MAX,
        vin: None,
        name: None,
        model: Some("Model 3".into()),
        efficiency: Some(-0.145),
        trim_badging: Some("74D".into()),
        marketing_name: Some("LR AWD".into()),
        exterior_color: Some("Pearl White".into()),
        wheel_type: Some("Apollo".into()),
        spoiler_type: Some("None".into()),
        display_priority: i16::MAX,
        inserted_at_pg_us: 1_700_000_000_000_000,
        updated_at_pg_us: 1_700_000_100_000_000,
        settings_id: i64::MIN,
    }
    .into();
    assert_eq!(car.id, i16::MIN);
    assert_eq!(car.eid, i64::MIN);
    assert_eq!(car.vid, i64::MAX);
    assert_eq!(car.efficiency, Some(-0.145));
    assert_eq!(car.display_priority, i16::MAX);
    assert_eq!(car.settings_id, i64::MIN);
}

#[test]
fn states_and_updates_v2_2_physical_conversion_preserves_raw_postgres_timestamps() {
    let state: ProjectionStateV2_2 = TeslaMateStatePhysicalV2_2 {
        id: i32::MIN,
        car_id: i16::MAX,
        state: ProjectionStateStatusV2_2::Asleep,
        start_date_pg_us: i64::MIN,
        end_date_pg_us: Some(i64::MAX),
    }
    .into();
    assert_eq!(
        state,
        ProjectionStateV2_2 {
            id: i32::MIN,
            car_id: i16::MAX,
            state: ProjectionStateStatusV2_2::Asleep,
            start_date_pg_us: i64::MIN,
            end_date_pg_us: Some(i64::MAX),
        }
    );

    let update: ProjectionUpdateV2_2 = TeslaMateUpdatePhysicalV2_2 {
        id: i32::MAX,
        car_id: i16::MIN,
        start_date_pg_us: i64::MAX,
        end_date_pg_us: None,
        version: Some(String::new()),
    }
    .into();
    assert_eq!(
        update,
        ProjectionUpdateV2_2 {
            id: i32::MAX,
            car_id: i16::MIN,
            start_date_pg_us: i64::MAX,
            end_date_pg_us: None,
            version: Some(String::new()),
        }
    );
}

#[test]
fn drives_v2_2_physical_conversion_preserves_every_raw_field() {
    let physical = TeslaMateDrivePhysicalV2_2 {
        id: i32::MIN,
        car_id: i16::MIN,
        start_date_pg_us: i64::MAX,
        end_date_pg_us: Some(i64::MIN),
        start_position_id: Some(i32::MIN),
        end_position_id: Some(i32::MAX),
        start_address_id: Some(-1),
        end_address_id: Some(i32::MAX),
        start_geofence_id: Some(i32::MIN),
        end_geofence_id: None,
        outside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::NaN),
        inside_temp_avg_e1: None,
        speed_max: Some(i16::MIN),
        power_max: Some(i16::MAX),
        power_min: Some(i16::MIN),
        start_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
        end_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(-999_999)),
        start_rated_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
        end_rated_range_km_e2: None,
        start_km: Some(ProjectionFloat64BitsV2_2((-0.0_f64).to_bits())),
        end_km: Some(ProjectionFloat64BitsV2_2(f64::NEG_INFINITY.to_bits())),
        distance: Some(ProjectionFloat64BitsV2_2(0x7ff8_0000_0000_0042)),
        duration_min: Some(i16::MIN),
        ascent: Some(i16::MAX),
        descent: Some(i16::MIN),
    };
    let projected: ProjectionDriveV2_2 = physical.clone().into();
    assert_eq!(projected.id, physical.id);
    assert_eq!(projected.car_id, physical.car_id);
    assert_eq!(projected.start_date_pg_us, physical.start_date_pg_us);
    assert_eq!(projected.end_date_pg_us, physical.end_date_pg_us);
    assert_eq!(projected.start_position_id, physical.start_position_id);
    assert_eq!(projected.end_position_id, physical.end_position_id);
    assert_eq!(projected.start_address_id, physical.start_address_id);
    assert_eq!(projected.end_address_id, physical.end_address_id);
    assert_eq!(projected.start_geofence_id, physical.start_geofence_id);
    assert_eq!(projected.end_geofence_id, physical.end_geofence_id);
    assert_eq!(projected.outside_temp_avg_e1, physical.outside_temp_avg_e1);
    assert_eq!(projected.inside_temp_avg_e1, physical.inside_temp_avg_e1);
    assert_eq!(projected.speed_max, physical.speed_max);
    assert_eq!(projected.power_max, physical.power_max);
    assert_eq!(projected.power_min, physical.power_min);
    assert_eq!(
        projected.start_ideal_range_km_e2,
        physical.start_ideal_range_km_e2
    );
    assert_eq!(
        projected.end_ideal_range_km_e2,
        physical.end_ideal_range_km_e2
    );
    assert_eq!(
        projected.start_rated_range_km_e2,
        physical.start_rated_range_km_e2
    );
    assert_eq!(
        projected.end_rated_range_km_e2,
        physical.end_rated_range_km_e2
    );
    assert_eq!(projected.start_km, physical.start_km);
    assert_eq!(projected.end_km, physical.end_km);
    assert_eq!(projected.distance, physical.distance);
    assert_eq!(projected.duration_min, physical.duration_min);
    assert_eq!(projected.ascent, physical.ascent);
    assert_eq!(projected.descent, physical.descent);
}

#[test]
fn positions_v2_2_physical_conversion_preserves_every_raw_field() {
    let physical = TeslaMatePositionPhysicalV2_2 {
        id: i32::MIN,
        car_id: i16::MIN,
        drive_id: Some(i32::MAX),
        date_pg_us: i64::MIN,
        latitude_e6: ProjectionFixedNumericV2_2::NaN,
        longitude_e6: ProjectionFixedNumericV2_2::Finite(-999_999_999),
        elevation: Some(i16::MIN),
        speed: Some(i16::MAX),
        power: Some(i16::MIN),
        odometer: Some(ProjectionFloat64BitsV2_2(0x7ff8_0000_0000_0042)),
        ideal_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
        est_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(-999_999)),
        rated_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
        battery_level: Some(i16::MIN),
        usable_battery_level: Some(i16::MAX),
        battery_heater: Some(false),
        battery_heater_on: Some(true),
        battery_heater_no_power: None,
        outside_temp_e1: Some(ProjectionFixedNumericV2_2::NaN),
        inside_temp_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
        fan_status: Some(i32::MIN),
        driver_temp_setting_e1: None,
        passenger_temp_setting_e1: Some(ProjectionFixedNumericV2_2::Finite(9_999)),
        is_climate_on: Some(true),
        is_rear_defroster_on: Some(false),
        is_front_defroster_on: None,
        tpms_pressure_fl_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
        tpms_pressure_fr_e1: Some(ProjectionFixedNumericV2_2::NaN),
        tpms_pressure_rl_e1: None,
        tpms_pressure_rr_e1: Some(ProjectionFixedNumericV2_2::Finite(9_999)),
    };
    let projected: ProjectionPositionV2_2 = physical.clone().into();
    assert_eq!(projected.id, physical.id);
    assert_eq!(projected.car_id, physical.car_id);
    assert_eq!(projected.drive_id, physical.drive_id);
    assert_eq!(projected.date_pg_us, physical.date_pg_us);
    assert_eq!(projected.latitude_e6, physical.latitude_e6);
    assert_eq!(projected.longitude_e6, physical.longitude_e6);
    assert_eq!(projected.elevation, physical.elevation);
    assert_eq!(projected.speed, physical.speed);
    assert_eq!(projected.power, physical.power);
    assert_eq!(projected.odometer, physical.odometer);
    assert_eq!(
        projected.ideal_battery_range_km_e2,
        physical.ideal_battery_range_km_e2
    );
    assert_eq!(
        projected.est_battery_range_km_e2,
        physical.est_battery_range_km_e2
    );
    assert_eq!(
        projected.rated_battery_range_km_e2,
        physical.rated_battery_range_km_e2
    );
    assert_eq!(projected.battery_level, physical.battery_level);
    assert_eq!(
        projected.usable_battery_level,
        physical.usable_battery_level
    );
    assert_eq!(projected.battery_heater, physical.battery_heater);
    assert_eq!(projected.battery_heater_on, physical.battery_heater_on);
    assert_eq!(
        projected.battery_heater_no_power,
        physical.battery_heater_no_power
    );
    assert_eq!(projected.outside_temp_e1, physical.outside_temp_e1);
    assert_eq!(projected.inside_temp_e1, physical.inside_temp_e1);
    assert_eq!(projected.fan_status, physical.fan_status);
    assert_eq!(
        projected.driver_temp_setting_e1,
        physical.driver_temp_setting_e1
    );
    assert_eq!(
        projected.passenger_temp_setting_e1,
        physical.passenger_temp_setting_e1
    );
    assert_eq!(projected.is_climate_on, physical.is_climate_on);
    assert_eq!(
        projected.is_rear_defroster_on,
        physical.is_rear_defroster_on
    );
    assert_eq!(
        projected.is_front_defroster_on,
        physical.is_front_defroster_on
    );
    assert_eq!(projected.tpms_pressure_fl_e1, physical.tpms_pressure_fl_e1);
    assert_eq!(projected.tpms_pressure_fr_e1, physical.tpms_pressure_fr_e1);
    assert_eq!(projected.tpms_pressure_rl_e1, physical.tpms_pressure_rl_e1);
    assert_eq!(projected.tpms_pressure_rr_e1, physical.tpms_pressure_rr_e1);
}

#[test]
fn charging_v2_2_physical_conversion_preserves_every_raw_field() {
    let process = TeslaMateChargingProcessPhysicalV2_2 {
        id: i32::MIN,
        car_id: i16::MIN,
        position_id: i32::MAX,
        address_id: Some(i32::MIN),
        geofence_id: Some(i32::MAX),
        start_date_pg_us: i64::MIN,
        end_date_pg_us: Some(i64::MAX),
        charge_energy_added_e2: Some(ProjectionFixedNumericV2_2::NaN),
        charge_energy_used_e2: None,
        start_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
        end_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(-999_999)),
        start_rated_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
        end_rated_range_km_e2: None,
        start_battery_level: Some(i16::MIN),
        end_battery_level: Some(i16::MAX),
        duration_min: Some(i16::MIN),
        outside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::Finite(-9_999)),
        cost_e2: Some(ProjectionFixedNumericV2_2::Finite(999_999)),
    };
    let projected_process: ProjectionChargingProcessV2_2 = process.clone().into();
    assert_eq!(projected_process.id, process.id);
    assert_eq!(projected_process.car_id, process.car_id);
    assert_eq!(projected_process.position_id, process.position_id);
    assert_eq!(projected_process.address_id, process.address_id);
    assert_eq!(projected_process.geofence_id, process.geofence_id);
    assert_eq!(projected_process.start_date_pg_us, process.start_date_pg_us);
    assert_eq!(projected_process.end_date_pg_us, process.end_date_pg_us);
    assert_eq!(
        projected_process.charge_energy_added_e2,
        process.charge_energy_added_e2
    );
    assert_eq!(
        projected_process.charge_energy_used_e2,
        process.charge_energy_used_e2
    );
    assert_eq!(
        projected_process.start_ideal_range_km_e2,
        process.start_ideal_range_km_e2
    );
    assert_eq!(
        projected_process.end_ideal_range_km_e2,
        process.end_ideal_range_km_e2
    );
    assert_eq!(
        projected_process.start_rated_range_km_e2,
        process.start_rated_range_km_e2
    );
    assert_eq!(
        projected_process.end_rated_range_km_e2,
        process.end_rated_range_km_e2
    );
    assert_eq!(
        projected_process.start_battery_level,
        process.start_battery_level
    );
    assert_eq!(
        projected_process.end_battery_level,
        process.end_battery_level
    );
    assert_eq!(projected_process.duration_min, process.duration_min);
    assert_eq!(
        projected_process.outside_temp_avg_e1,
        process.outside_temp_avg_e1
    );
    assert_eq!(projected_process.cost_e2, process.cost_e2);

    let charge = TeslaMateChargePhysicalV2_2 {
        id: i32::MAX,
        charging_process_id: i32::MIN,
        date_pg_us: i64::MAX,
        battery_heater: Some(false),
        battery_heater_on: Some(true),
        battery_heater_no_power: None,
        battery_level: Some(i16::MIN),
        usable_battery_level: Some(i16::MAX),
        charge_energy_added_e2: ProjectionFixedNumericV2_2::NaN,
        charger_actual_current: Some(i16::MIN),
        charger_phases: Some(i16::MAX),
        charger_pilot_current: Some(i16::MIN),
        charger_power: i16::MAX,
        charger_voltage: Some(i16::MIN),
        conn_charge_cable: Some("Type 2".into()),
        fast_charger_present: Some(false),
        fast_charger_brand: Some("Tesla".into()),
        fast_charger_type: Some("Supercharger".into()),
        ideal_battery_range_km_e2: ProjectionFixedNumericV2_2::Finite(-999_999),
        rated_battery_range_km_e2: Some(ProjectionFixedNumericV2_2::NaN),
        not_enough_power_to_heat: Some(false),
        outside_temp_e1: Some(ProjectionFixedNumericV2_2::Finite(9_999)),
    };
    let projected_charge: ProjectionChargeV2_2 = charge.clone().into();
    assert_eq!(projected_charge.id, charge.id);
    assert_eq!(
        projected_charge.charging_process_id,
        charge.charging_process_id
    );
    assert_eq!(projected_charge.date_pg_us, charge.date_pg_us);
    assert_eq!(projected_charge.battery_heater, charge.battery_heater);
    assert_eq!(projected_charge.battery_heater_on, charge.battery_heater_on);
    assert_eq!(
        projected_charge.battery_heater_no_power,
        charge.battery_heater_no_power
    );
    assert_eq!(projected_charge.battery_level, charge.battery_level);
    assert_eq!(
        projected_charge.usable_battery_level,
        charge.usable_battery_level
    );
    assert_eq!(
        projected_charge.charge_energy_added_e2,
        charge.charge_energy_added_e2
    );
    assert_eq!(
        projected_charge.charger_actual_current,
        charge.charger_actual_current
    );
    assert_eq!(projected_charge.charger_phases, charge.charger_phases);
    assert_eq!(
        projected_charge.charger_pilot_current,
        charge.charger_pilot_current
    );
    assert_eq!(projected_charge.charger_power, charge.charger_power);
    assert_eq!(projected_charge.charger_voltage, charge.charger_voltage);
    assert_eq!(projected_charge.conn_charge_cable, charge.conn_charge_cable);
    assert_eq!(
        projected_charge.fast_charger_present,
        charge.fast_charger_present
    );
    assert_eq!(
        projected_charge.fast_charger_brand,
        charge.fast_charger_brand
    );
    assert_eq!(projected_charge.fast_charger_type, charge.fast_charger_type);
    assert_eq!(
        projected_charge.ideal_battery_range_km_e2,
        charge.ideal_battery_range_km_e2
    );
    assert_eq!(
        projected_charge.rated_battery_range_km_e2,
        charge.rated_battery_range_km_e2
    );
    assert_eq!(
        projected_charge.not_enough_power_to_heat,
        charge.not_enough_power_to_heat
    );
    assert_eq!(projected_charge.outside_temp_e1, charge.outside_temp_e1);
}

#[test]
fn projects_only_completed_selected_vehicle_history() {
    let projected = project_vehicle(&history(), 1).unwrap();
    assert_eq!(projected.snapshot.cars.len(), 1);
    assert_eq!(projected.snapshot.drives.len(), 1);
    assert_eq!(projected.snapshot.drives[0].inside_temp_avg, Some(21.0));
    assert_eq!(projected.snapshot.drives[0].power_max, Some(36.0));
    assert_eq!(projected.snapshot.drives[0].power_min, Some(-7.0));
    assert_eq!(
        projected.snapshot.drives[0].start_ideal_range_km,
        Some(338.8)
    );
    assert_eq!(projected.snapshot.drives[0].end_ideal_range_km, Some(334.8));
    assert_eq!(projected.snapshot.drives[0].ascent, Some(60));
    assert_eq!(projected.snapshot.charges[0].cost, Some(9.99));
    assert_eq!(
        projected.snapshot.charges[0].billing_type,
        Some(GeofenceBillingType::PerKwh)
    );
    assert_eq!(projected.snapshot.drives[0].descent, Some(30));
    assert_eq!(projected.snapshot.positions.len(), 1);
    assert_eq!(projected.snapshot.positions[0].battery_heater, Some(true));
    assert_eq!(
        projected.snapshot.positions[0].battery_heater_on,
        Some(true)
    );
    assert_eq!(
        projected.snapshot.positions[0].battery_heater_no_power,
        Some(false)
    );
    assert_eq!(projected.snapshot.charges.len(), 1);
    assert_eq!(projected.snapshot.charge_samples.len(), 1);
    assert_eq!(
        projected.snapshot.charges[0].max_charger_power_kw,
        Some(7.0)
    );
    assert_eq!(projected.snapshot.charges[0].is_dc, Some(false));
    assert_eq!(projected.snapshot.cars[0].model, "3");
    assert_eq!(
        projected.snapshot.cars[0].marketing_name.as_deref(),
        Some("Model 3 Highland")
    );
    assert_eq!(
        projected.snapshot.cars[0].firmware_version.as_deref(),
        Some("2026.20.1")
    );
    assert_eq!(
        projected.snapshot.drives[0].start_address.as_deref(),
        Some("Home, London")
    );
    assert_eq!(projected.snapshot.drives[0].start_soc, Some(78));
    assert_eq!(
        projected.snapshot.charges[0].location_name.as_deref(),
        Some("Home")
    );
    assert_eq!(
        projected.report,
        ProjectionReport {
            completed_drives: 1,
            skipped_open_drives: 1,
            skipped_unattached_positions: 1,
            projected_positions: 1,
            projected_charges: 1,
            projected_charge_samples: 1,
            projected_states: 0,
            projected_updates: 1,
            skipped_incomplete_updates: 0,
        }
    );
}

#[test]
fn projects_completed_update_history_in_canonical_order_and_skips_placeholders() {
    let mut source = history();
    source.updates.extend([
        TeslaMateUpdate {
            id: 303,
            car_id: 1,
            start_date_ms: 1_800_000_000_000,
            end_date_ms: Some(1_800_000_060_000),
            version: Some(" 2027.4.1 ".into()),
        },
        TeslaMateUpdate {
            id: 302,
            car_id: 1,
            start_date_ms: 1_750_000_000_000,
            end_date_ms: Some(1_750_000_060_000),
            version: Some("   ".into()),
        },
        TeslaMateUpdate {
            id: 301,
            car_id: 1,
            start_date_ms: 1_700_000_000_000,
            end_date_ms: None,
            version: Some("2026.30.1".into()),
        },
    ]);

    let projected = project_vehicle(&source, 1).expect("project updates");
    assert_eq!(
        projected
            .updates
            .iter()
            .map(|update| (update.id, update.version.as_str()))
            .collect::<Vec<_>>(),
        vec![(300, "2026.20.1"), (303, "2027.4.1")]
    );
    assert_eq!(projected.report.projected_updates, 2);
    assert_eq!(projected.report.skipped_incomplete_updates, 2);
    assert_eq!(
        projected.snapshot.cars[0].firmware_version.as_deref(),
        Some("2027.4.1")
    );
}

#[test]
fn charge_interval_contains_all_source_samples() {
    let mut before = history();
    before.charges[0].date_ms = before.charging_processes[0].start_date_ms - 1_000;
    let projected = project_vehicle(&before, 1).unwrap();
    assert_eq!(
        projected.snapshot.charges[0].start_date_ms,
        before.charges[0].date_ms
    );

    let mut after = history();
    after.charges[0].date_ms = after.charging_processes[0].end_date_ms.unwrap() + 1_000;
    let projected = project_vehicle(&after, 1).unwrap();
    assert_eq!(
        projected.snapshot.charges[0].end_date_ms,
        Some(after.charges[0].date_ms)
    );
}

#[test]
fn refuses_duplicate_source_identity_and_uses_safe_car_fallbacks() {
    let mut source = history();
    source.cars[1].id = 1;
    assert!(matches!(
        project_vehicle(&source, 1),
        Err(TeslaMateProjectionError::DuplicateId {
            entity: "car",
            id: 1
        })
    ));

    let mut source = history();
    source.cars[0].name = None;
    source.cars[0].model = None;
    source.cars[0].marketing_name = None;
    source.cars[0].trim_badging = None;
    let projected = project_vehicle(&source, 1).unwrap();
    assert_eq!(projected.snapshot.cars[0].name, "Car 1");
    assert_eq!(projected.snapshot.cars[0].model, "Unknown Tesla");
}

#[test]
fn refuses_cross_car_endpoint_position() {
    let mut source = history();
    source.positions[0].car_id = 2;
    assert_eq!(
        project_vehicle(&source, 1),
        Err(TeslaMateProjectionError::RelatedPositionWrongCar {
            field: "drive.start_position_id",
            id: 20,
            expected_car_id: 1,
            found_car_id: 2,
        })
    );
}

#[test]
fn validates_a_complete_open_session_fixture_and_rejects_cross_car_rows() {
    let mut source = history();
    source.drives[0].end_date_ms = None;
    source.charging_processes[0].end_date_ms = None;
    let state = TeslaMateState {
        id: 40,
        car_id: 1,
        state: "online".into(),
        start_date_ms: 1_700_000_000_000,
        end_date_ms: None,
    };
    let mut standalone = source.positions[0].clone();
    standalone.id = 99;
    standalone.drive_id = None;
    let open = TeslaMateOpenSession {
        car_id: 1,
        drive: Some(source.drives[0].clone()),
        drive_positions: vec![source.positions[0].clone()],
        charge: Some(source.charging_processes[0].clone()),
        charge_samples: source.charges.clone(),
        state: Some(state),
        standalone_positions: vec![standalone],
        watermarks: TeslaMateSourceWatermarks {
            drives: TeslaMateSourceWatermark {
                max_id: Some(10),
                max_timestamp_ms: Some(1_700_000_060_000),
            },
            ..TeslaMateSourceWatermarks::default()
        },
    };
    open.validate().expect("open fixture validates");

    let mut wrong_car = open.clone();
    wrong_car.drive_positions[0].car_id = 2;
    assert!(matches!(
        wrong_car.validate(),
        Err(TeslaMateProjectionError::SelectedCarMismatch { .. })
    ));
}

#[test]
fn mapped_history_is_accepted_by_the_typed_pack_gate() {
    let temporary = TempDir::new().unwrap();
    let projected = project_vehicle(&history(), 1).unwrap();
    let request = ProjectionPackRequest {
        pack_id: Uuid::from_u128(1),
        snapshot_id: Uuid::from_u128(2),
        ordinal: 0,
        binding: ProjectionBinding {
            installation_id: Uuid::from_u128(3),
            account_id: Uuid::from_u128(4),
            vehicle_id: Uuid::from_u128(5),
            generation: 1,
            selected_car_id: 1,
        },
        sequence: SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        snapshot: &projected.snapshot,
    };
    let built = ProjectionPackWriter::new(temporary.path())
        .write_full_snapshot(&request)
        .unwrap();
    assert_eq!(built.metadata.row_count, 5);
}
