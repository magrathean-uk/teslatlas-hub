// SPDX-License-Identifier: AGPL-3.0-only

// C1 ST85: the unrounded odometer values the lifecycle stores for a closed
// drive survive the hub-sync physical pack writer and reader bit for bit
// (the `*_f64_be` columns). Synthetic data only.

fn st85_closed_drive() -> (ProjectionDrive, Vec<ProjectionPosition>) {
    use crate::lifecycle::{LifecycleSample, OpenSessionState, apply_samples};
    const T0: i64 = 1_760_000_000_000;
    let observation = |id: i64, offset_ms: i64, gear: &str, latitude: f64, odometer: f64| {
        let t = T0 + offset_ms;
        LifecycleSample {
            observation_id: id,
            observed_at_ms: t,
            vehicle_state: "online".to_owned(),
            payload: serde_json::json!({
                "record_type": "owner_api_vehicle_data_v1",
                "source_vehicle_id": "9",
                "source_vehicle_state": "online",
                "provider_raw_json": {"response": {
                    "drive_state": {"timestamp": t, "shift_state": gear, "speed": 30,
                                    "latitude": latitude, "longitude": 20.0},
                    "vehicle_state": {"timestamp": t, "odometer": odometer},
                }},
            }),
        }
    };
    let step = apply_samples(
        OpenSessionState::new(),
        10,
        &[
            observation(1, 0, "D", 10.0, 1000.123456789),
            observation(2, 60_000, "D", 10.01, 1002.0),
            observation(3, 120_000, "P", 10.02, 1003.987654321),
        ],
    )
    .expect("the lifecycle closes the drive");
    assert_eq!(step.delta.drives.len(), 1);
    (
        step.delta.drives[0].clone(),
        step.delta.open_drive_positions,
    )
}

#[test]
fn st85_unrounded_odometer_values_survive_the_physical_pack_bit_for_bit() {
    let (drive, positions) = st85_closed_drive();
    assert_eq!(positions.len(), 3);
    let start_km = positions[0].odometer.expect("first odometer");
    let end_km = positions[2].odometer.expect("last odometer");
    let distance = drive.distance_km.expect("distance");
    assert_eq!(start_km, 1000.123456789 * 1.609_344);
    assert_eq!(end_km, 1003.987654321 * 1.609_344);
    assert_eq!(distance, end_km - start_km);

    let mut source = snapshot_v2_2();
    let bits = |value: f64| ProjectionFloat64BitsV2_2(value.to_bits());
    let template = source.positions[0].clone();
    source.positions = positions
        .iter()
        .enumerate()
        .map(|(index, position)| ProjectionPositionV2_2 {
            id: 30 + index as i32,
            drive_id: Some(20),
            date_pg_us: 1_700_000_030_000_000 + index as i64 * 60_000_000,
            odometer: position.odometer.map(bits),
            ..template.clone()
        })
        .collect();
    let drive_row = &mut source.drives[0];
    drive_row.start_date_pg_us = 1_700_000_030_000_000;
    drive_row.end_date_pg_us = Some(1_700_000_150_000_000);
    drive_row.start_position_id = Some(30);
    drive_row.end_position_id = Some(32);
    drive_row.start_km = Some(bits(start_km));
    drive_row.end_km = Some(bits(end_km));
    drive_row.distance = Some(bits(distance));
    source.charging_processes[0].position_id = 30;

    let temporary = crate::private_tempdir().unwrap();
    let request = request_v2_2(&source);
    let built = ProjectionPackWriter::new(temporary.path().join("packs"))
        .write_physical_snapshot_2_2_for_hub_sync_v1_1_3(&request)
        .expect("the physical writer accepts the lifecycle's values");
    let sqlite = zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap();
    let inspect = temporary.path().join("st85.sqlite");
    fs::write(&inspect, sqlite).unwrap();
    let connection = Connection::open(inspect).unwrap();
    let decode = |blob: Vec<u8>| {
        f64::from_bits(u64::from_be_bytes(
            blob.try_into().expect("eight-byte f64_be column"),
        ))
    };
    let (read_start, read_end, read_distance): (Vec<u8>, Vec<u8>, Vec<u8>) = connection
        .query_row(
            "SELECT start_km_f64_be, end_km_f64_be, distance_f64_be FROM drives WHERE id = 20",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(decode(read_start).to_bits(), start_km.to_bits());
    assert_eq!(decode(read_end).to_bits(), end_km.to_bits());
    assert_eq!(decode(read_distance).to_bits(), distance.to_bits());
    let read_odometers: Vec<u64> = connection
        .prepare("SELECT odometer_f64_be FROM positions ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|blob| decode(blob.unwrap()).to_bits())
        .collect();
    assert_eq!(
        read_odometers,
        positions
            .iter()
            .map(|position| position.odometer.expect("odometer").to_bits())
            .collect::<Vec<_>>()
    );
}
