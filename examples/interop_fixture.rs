// SPDX-License-Identifier: AGPL-3.0-only
//! Explicitly invoked synthetic fixture creator, not part of the Hub product.
#[path = "../tests/interop/seed.rs"]
mod seed;

const USAGE: &str = "usage: interop_fixture --output NEW_ABSOLUTE_DIRECTORY (--port PORT [--source-id UUID --vehicle-id UUID --vin VIN --car-id ID] [--scenario viewer-r1-51-drives] | --scenario empty-edge-binding --source-id UUID --vehicle-id UUID --vin VIN --car-id ID --installation-id ID --lineage ID)";

#[derive(Clone, Copy)]
enum Scenario {
    B1,
    ViewerR1,
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
        Scenario::B1 | Scenario::ViewerR1 => {
            let port = port.ok_or(USAGE)?;
            let fixture_scenario = match scenario {
                Scenario::B1 => seed::FixtureScenario::B1,
                Scenario::ViewerR1 => seed::FixtureScenario::ViewerR1,
                Scenario::EmptyEdgeBinding => unreachable!(),
            };
            let prepared = match (source_id, vehicle_id, vin, car_id) {
                (None, None, None, None) => match fixture_scenario {
                    seed::FixtureScenario::B1 => seed::prepare(std::path::Path::new(output), port)?,
                    seed::FixtureScenario::ViewerR1 => {
                        seed::prepare_viewer_r1(std::path::Path::new(output), port)?
                    }
                },
                (Some(source_id), None, None, None) => seed::prepare_with_scenario_and_source_id(
                    std::path::Path::new(output),
                    port,
                    fixture_scenario,
                    source_id,
                )?,
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
}
