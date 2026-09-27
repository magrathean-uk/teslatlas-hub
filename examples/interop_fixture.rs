// SPDX-License-Identifier: AGPL-3.0-only
//! Explicitly invoked synthetic fixture creator, not part of the Hub product.
#[path = "../tests/interop/seed.rs"]
mod seed;

const USAGE: &str = "usage: interop_fixture (--expose-dynamic|--retire-dynamic|--restore-dynamic|--advance|--rotate-physical) OWNED_FIXTURE_DIRECTORY | --output NEW_ABSOLUTE_DIRECTORY (--port PORT [--source-id UUID --vehicle-id UUID --vin VIN --car-id ID] [--scenario viewer-r1-51-drives|physical-v3-public-513] | --scenario empty-edge-binding --source-id UUID --vehicle-id UUID --vin VIN --car-id ID --installation-id ID --lineage ID)";

const PHYSICAL_V3_FIXTURE_PORT: u16 = 21_445;
const PHYSICAL_V3_FIXTURE_SOURCE_ID: uuid::Uuid =
    uuid::Uuid::from_u128(0x51300000000040008000000000000513);

#[derive(Clone, Copy)]
enum Scenario {
    B1,
    ViewerR1,
    PhysicalV3Public513,
    EmptyEdgeBinding,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!("{}", serde_json::to_string(&run(&args)?)?);
    Ok(())
}

fn run(args: &[String]) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if args.len() == 2 && args[0] == "--advance" {
        seed::advance(std::path::Path::new(&args[1]))?;
        return Ok(serde_json::json!({"status":"advanced"}));
    }
    if args.len() == 2 && args[0] == "--rotate-physical" {
        return Ok(serde_json::to_value(
            seed::rotate_physical_v3_public_admission(std::path::Path::new(&args[1]))?,
        )?);
    }
    if args.len() == 2 && args[0] == "--expose-dynamic" {
        return Ok(serde_json::to_value(seed::expose_dynamic_vehicle(
            std::path::Path::new(&args[1]),
        )?)?);
    }
    if args.len() == 2 && args[0] == "--retire-dynamic" {
        return Ok(serde_json::to_value(seed::retire_dynamic_vehicle(
            std::path::Path::new(&args[1]),
        )?)?);
    }
    if args.len() == 2 && args[0] == "--restore-dynamic" {
        return Ok(serde_json::to_value(seed::restore_dynamic_vehicle(
            std::path::Path::new(&args[1]),
        )?)?);
    }
    let mut output = None;
    let mut port = None;
    let mut source_id = None;
    let mut vehicle_id = None;
    let mut vin = None;
    let mut car_id = None;
    let mut installation_id = None;
    let mut lineage = None;
    let mut scenario = Scenario::B1;
    let mut index = 0;
    while index < args.len() {
        let value = args.get(index + 1).ok_or(USAGE)?;
        match args[index].as_str() {
            "--output" if output.is_none() => output = Some(value),
            "--port" if port.is_none() => port = Some(value.parse()?),
            "--source-id" if source_id.is_none() => source_id = Some(value.parse()?),
            "--vehicle-id" if vehicle_id.is_none() => vehicle_id = Some(value.parse()?),
            "--vin" if vin.is_none() => vin = Some(value.to_owned()),
            "--car-id" if car_id.is_none() => car_id = Some(value.parse()?),
            "--installation-id" if installation_id.is_none() => {
                installation_id = Some(value.to_owned())
            }
            "--lineage" if lineage.is_none() => lineage = Some(value.to_owned()),
            "--scenario" if value == "viewer-r1-51-drives" => scenario = Scenario::ViewerR1,
            "--scenario" if value == "physical-v3-public-513" => {
                scenario = Scenario::PhysicalV3Public513
            }
            "--scenario" if value == "empty-edge-binding" => scenario = Scenario::EmptyEdgeBinding,
            _ => return Err(USAGE.into()),
        }
        index += 2;
    }
    let output = output.ok_or(USAGE)?;
    match scenario {
        Scenario::EmptyEdgeBinding => {
            if port.is_some() {
                return Err(USAGE.into());
            }
            let prepared = seed::prepare_empty_edge_binding(
                std::path::Path::new(output),
                &seed::EmptyEdgeBinding {
                    installation_id: installation_id.ok_or(USAGE)?,
                    lineage: lineage.ok_or(USAGE)?,
                    source_id: source_id.ok_or(USAGE)?,
                    vehicle_id: vehicle_id.ok_or(USAGE)?,
                    vin: vin.ok_or(USAGE)?,
                    car_id: car_id.ok_or(USAGE)?,
                },
            )?;
            Ok(serde_json::to_value(prepared)?)
        }
        Scenario::B1 | Scenario::ViewerR1 | Scenario::PhysicalV3Public513 => {
            let port = port.ok_or(USAGE)?;
            if matches!(scenario, Scenario::PhysicalV3Public513)
                && (port != PHYSICAL_V3_FIXTURE_PORT
                    || source_id.is_some()
                    || vehicle_id.is_some()
                    || vin.is_some()
                    || car_id.is_some())
            {
                return Err(USAGE.into());
            }
            let fixture_scenario = match scenario {
                Scenario::B1 => seed::FixtureScenario::B1,
                Scenario::ViewerR1 => seed::FixtureScenario::ViewerR1,
                Scenario::PhysicalV3Public513 => seed::FixtureScenario::PhysicalV3Public513,
                Scenario::EmptyEdgeBinding => unreachable!(),
            };
            let prepared = if matches!(scenario, Scenario::PhysicalV3Public513) {
                seed::prepare_with_scenario_and_source_id(
                    std::path::Path::new(output),
                    port,
                    fixture_scenario,
                    PHYSICAL_V3_FIXTURE_SOURCE_ID,
                )?
            } else {
                match (source_id, vehicle_id, vin, car_id) {
                    (None, None, None, None) => match fixture_scenario {
                        seed::FixtureScenario::B1 => {
                            seed::prepare(std::path::Path::new(output), port)?
                        }
                        seed::FixtureScenario::ViewerR1 => {
                            seed::prepare_viewer_r1(std::path::Path::new(output), port)?
                        }
                        seed::FixtureScenario::PhysicalV3Public513 => unreachable!(),
                    },
                    (Some(source_id), None, None, None) => {
                        seed::prepare_with_scenario_and_source_id(
                            std::path::Path::new(output),
                            port,
                            fixture_scenario,
                            source_id,
                        )?
                    }
                    (Some(source_id), Some(vehicle_id), Some(vin), Some(car_id)) => {
                        seed::prepare_with_scenario_source_and_primary_vehicle(
                            std::path::Path::new(output),
                            port,
                            fixture_scenario,
                            source_id,
                            vehicle_id,
                            &vin,
                            car_id,
                        )?
                    }
                    _ => return Err(USAGE.into()),
                }
            };
            // Only paths/public synthetic identities are returned; invitation
            // material remains in an owner-only file.
            Ok(serde_json::to_value(prepared)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::run;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn empty_edge_binding_scenario_requires_and_preserves_all_sealed_identities() {
        // Removing the scenario route, accepting a partial identity tuple, or
        // sending it through a telemetry-bearing fixture must make this fail.
        let parent = tempfile::tempdir().unwrap();
        let output = parent.path().join("empty-edge-binding");
        let arguments = vec![
            "--output".into(),
            output.to_str().unwrap().into(),
            "--scenario".into(),
            "empty-edge-binding".into(),
            "--source-id".into(),
            "10a0be67-fca9-4f94-bcf5-004f82e442b8".into(),
            "--vehicle-id".into(),
            "4f414935-0523-4146-b6ef-286afe933c78".into(),
            "--vin".into(),
            "5YJ3E1EA7KF000005".into(),
            "--car-id".into(),
            "43".into(),
            "--installation-id".into(),
            "empty-edge-binding".into(),
            "--lineage".into(),
            "spool-2".into(),
        ];

        let mut missing_lineage = arguments.clone();
        missing_lineage.truncate(missing_lineage.len() - 2);
        assert!(run(&missing_lineage).is_err());

        let prepared = run(&arguments).unwrap();

        assert_eq!(prepared["schema_version"], 2);
        assert_eq!(
            prepared["source_id"],
            "10a0be67-fca9-4f94-bcf5-004f82e442b8"
        );
        assert_eq!(
            prepared["vehicle_id"],
            "4f414935-0523-4146-b6ef-286afe933c78"
        );
        assert_eq!(prepared["vin"], "5YJ3E1EA7KF000005");
        assert_eq!(prepared["car_id"], 43);
        assert_eq!(prepared["installation_id"], "empty-edge-binding");
        assert_eq!(prepared["lineage"], "spool-2");
        assert!(output.join("hub").is_dir());
    }

    #[test]
    fn physical_v3_public_fixture_is_fixed_to_loopback_21445_and_513_chunks() {
        let parent = tempfile::tempdir().unwrap();
        let rejected = parent.path().join("wrong-port");
        assert!(
            run(&[
                "--output".into(),
                rejected.to_str().unwrap().into(),
                "--port".into(),
                "21443".into(),
                "--scenario".into(),
                "physical-v3-public-513".into(),
            ])
            .is_err()
        );
        assert!(!rejected.exists());

        let output = parent.path().join("physical-v3-public-513");
        let prepared = run(&[
            "--output".into(),
            output.to_str().unwrap().into(),
            "--port".into(),
            "21445".into(),
            "--scenario".into(),
            "physical-v3-public-513".into(),
        ])
        .unwrap();

        let admission = &prepared["physical_v3_admission"];
        assert_eq!(admission["profile_id"], "hub-sync-v1@1.3.0");
        assert_eq!(
            admission["snapshot_id"],
            "51351351-5135-4135-8135-513513513513"
        );
        assert_eq!(admission["chunk_count"], 513);
        assert_eq!(admission["drive_ids"], serde_json::json!([301]));
        assert_eq!(admission["position_ids"], serde_json::json!([401, 402]));
        assert_eq!(admission["charging_process_ids"], serde_json::json!([501]));
        assert_eq!(
            admission["charge_sample_ids"],
            serde_json::json!([601, 602])
        );
        assert_eq!(admission["address_rows"], 0);
        assert_eq!(admission["geofence_rows"], 0);
        assert_eq!(admission["collector_enabled"], false);
        assert_eq!(
            prepared["source_id"],
            "51300000-0000-4000-8000-000000000513"
        );
        assert_eq!(prepared["endpoint"], "https://127.0.0.1:21445");
        let config = std::fs::read_to_string(prepared["config_path"].as_str().unwrap()).unwrap();
        assert!(config.contains("bind = \"127.0.0.1:21445\""));
        assert!(config.contains("[collector]\ninterval_seconds = 0"));
        assert!(!output.join("hub/secrets/teslamate-encryption.key").exists());

        let rotated = run(&["--rotate-physical".into(), output.to_str().unwrap().into()]).unwrap();
        assert_eq!(rotated["status"], "rotated-and-activated");
        assert_eq!(rotated["profile_id"], "hub-sync-v1@1.3.0");
        assert_eq!(rotated["vehicle_id"], admission["vehicle_id"]);
        assert_eq!(rotated["retained_snapshot_id"], admission["snapshot_id"]);
        assert_eq!(rotated["retained_receipt_id"], admission["receipt_id"]);
        assert_eq!(
            rotated["active_snapshot_id"],
            "51451451-5145-4145-8145-514514514514"
        );
        assert_eq!(rotated["active_chunk_count"], 513);
        assert!(
            rotated["active_head_sequence"].as_u64().unwrap()
                > admission["head_sequence"].as_u64().unwrap()
        );
        assert_ne!(rotated["active_receipt_id"], admission["receipt_id"]);
        assert_eq!(rotated["collector_enabled"], false);
        assert_eq!(
            std::fs::metadata(output.join("physical-rotation.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(run(&["--rotate-physical".into(), output.to_str().unwrap().into()]).is_err());
    }
}
