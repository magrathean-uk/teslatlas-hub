// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn schema_2_0_pack_matches_released_client_layout() {
    let temporary = crate::private_tempdir().unwrap();
    let source = snapshot();
    let built = ProjectionPackWriter::new(temporary.path().join("packs"))
        .write_full_snapshot(&request(&source))
        .unwrap();
    assert_eq!(built.metadata.schema, HUB_PROJECTION_SCHEMA_V1);

    let sqlite = zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap();
    let inspect = temporary.path().join("inspect.sqlite");
    fs::write(&inspect, sqlite).unwrap();
    let connection = Connection::open(inspect).unwrap();
    let mut tables = connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .unwrap();
    let tables = tables
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        tables,
        vec![
            "cars",
            "charge_samples",
            "charges",
            "drives",
            "hub_pack_metadata",
            "positions",
        ]
    );
    let user_version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(user_version, HUB_PROJECTION_SCHEMA_V1.sqlite_user_version());
    assert_schema_2_0_client_layout(&connection);
}

fn delta_request<'a>(delta: &'a ProjectionDelta) -> ProjectionDeltaPackRequest<'a> {
    ProjectionDeltaPackRequest {
        pack_id: Uuid::parse_str("66666666-6666-4666-8666-666666666666").unwrap(),
        snapshot_id: Uuid::parse_str("77777777-7777-4777-8777-777777777777").unwrap(),
        ordinal: 0,
        delta,
    }
}

fn sparse_delta() -> ProjectionDelta {
    let source = snapshot();
    let mut drive = source.drives[0].clone();
    drive.end_date_ms += 60_000;
    drive.end_address = Some("New work address".into());
    let mut position = source.positions[0].clone();
    position.id = 31;
    position.date_ms += 60_000;
    let mut car = source.cars[0].clone();
    car.name = "Road car renamed".into();
    ProjectionDelta {
        binding: binding(),
        sequence: SequenceRange {
            from_exclusive: 7,
            to_inclusive: 8,
        },
        parent_digest: Sha256Digest::of_bytes(b"base-lineage"),
        cars: vec![car],
        car_settings: Vec::new(),
        drives: vec![drive],
        positions: vec![position],
        charges: Vec::new(),
        charge_samples: Vec::new(),
        states: vec![ProjectionState {
            id: 60,
            car_id: 10,
            state: "online".into(),
            start_date_ms: 1_700_002_000_000,
            end_date_ms: None,
        }],
        updates: vec![ProjectionUpdate {
            id: 70,
            car_id: 10,
            start_date_ms: 1_700_002_100_000,
            end_date_ms: 1_700_002_200_000,
            version: "2026.3".into(),
        }],
        tombstones: vec![ProjectionTombstone {
            entity: ProjectionDeltaEntity::Position,
            id: 29,
            car_id: 10,
        }],
    }
}

#[test]
fn typed_delta_rejects_blank_update_version_before_writing_a_pack() {
    let temporary = crate::private_tempdir().unwrap();
    let mut delta = sparse_delta();
    delta.updates[0].version.clear();

    let error = ProjectionPackWriter::new(temporary.path().join("packs"))
        .write_delta(&delta_request(&delta))
        .expect_err("blank update versions are not a valid typed-delta payload");
    assert!(matches!(
        error,
        ProjectionPackError::Invalid(message) if message.contains("update.version must not be empty")
    ));
    assert!(
        !temporary.path().join("packs").exists(),
        "validation must reject before creating an immutable pack directory"
    );
}

#[test]
fn typed_delta_rejects_unsupported_source_owned_tombstones_before_writing_a_pack() {
    for entity in [
        ProjectionDeltaEntity::Car,
        ProjectionDeltaEntity::CarSetting,
        ProjectionDeltaEntity::Geofence,
        ProjectionDeltaEntity::Address,
    ] {
        let temporary = crate::private_tempdir().unwrap();
        let packs = temporary.path().join("packs");
        let mut delta = sparse_delta();
        delta.tombstones = vec![ProjectionTombstone {
            entity,
            id: 999,
            car_id: delta.binding.selected_car_id,
        }];

        let error = ProjectionPackWriter::new(&packs)
            .write_delta(&delta_request(&delta))
            .expect_err("unsupported source-owned tombstones must be rejected");
        assert!(matches!(
            error,
            ProjectionPackError::Invalid(message)
                if message.contains("unsupported source-owned delta tombstone entity")
        ));
        assert!(
            !packs.exists(),
            "validation must reject before creating an immutable pack directory"
        );
    }
}

#[test]
fn typed_delta_rejects_upsert_tombstone_overlap_before_writing_a_pack() {
    for entity in [
        ProjectionDeltaEntity::Drive,
        ProjectionDeltaEntity::Position,
        ProjectionDeltaEntity::Charge,
        ProjectionDeltaEntity::ChargeSample,
        ProjectionDeltaEntity::State,
        ProjectionDeltaEntity::Update,
    ] {
        let temporary = crate::private_tempdir().unwrap();
        let packs = temporary.path().join("packs");
        let mut delta = sparse_delta();
        let id = match entity {
            ProjectionDeltaEntity::Drive => delta.drives[0].id,
            ProjectionDeltaEntity::Position => delta.positions[0].id,
            ProjectionDeltaEntity::Charge => {
                let charge = snapshot().charges.into_iter().next().unwrap();
                let id = charge.id;
                delta.charges.push(charge);
                id
            }
            ProjectionDeltaEntity::ChargeSample => {
                let sample = snapshot().charge_samples.into_iter().next().unwrap();
                let id = sample.id;
                delta.charge_samples.push(sample);
                id
            }
            ProjectionDeltaEntity::State => delta.states[0].id,
            ProjectionDeltaEntity::Update => delta.updates[0].id,
            ProjectionDeltaEntity::Car
            | ProjectionDeltaEntity::CarSetting
            | ProjectionDeltaEntity::Geofence
            | ProjectionDeltaEntity::Address => unreachable!("supported tombstone entity"),
        };
        delta.tombstones = vec![ProjectionTombstone {
            entity,
            id,
            car_id: delta.binding.selected_car_id,
        }];

        let error = ProjectionPackWriter::new(&packs)
            .write_delta(&delta_request(&delta))
            .expect_err("a typed row cannot be upserted and tombstoned together");
        assert!(matches!(
            error,
            ProjectionPackError::Invalid(message)
                if message == "typed delta upsert and tombstone overlap"
        ));
        assert!(
            !packs.exists(),
            "validation must reject before creating an immutable pack directory"
        );
    }
}

#[test]
fn source_owned_tombstone_canonical_order_is_child_first() {
    let tombstones = vec![
        ProjectionTombstone {
            entity: ProjectionDeltaEntity::Drive,
            id: 20,
            car_id: 10,
        },
        ProjectionTombstone {
            entity: ProjectionDeltaEntity::Charge,
            id: 40,
            car_id: 10,
        },
        ProjectionTombstone {
            entity: ProjectionDeltaEntity::Position,
            id: 30,
            car_id: 10,
        },
        ProjectionTombstone {
            entity: ProjectionDeltaEntity::ChargeSample,
            id: 50,
            car_id: 10,
        },
        ProjectionTombstone {
            entity: ProjectionDeltaEntity::Update,
            id: 70,
            car_id: 10,
        },
        ProjectionTombstone {
            entity: ProjectionDeltaEntity::State,
            id: 60,
            car_id: 10,
        },
    ];

    let entities = source_owned_tombstones_in_canonical_order(&tombstones)
        .into_iter()
        .map(|row| row.entity)
        .collect::<Vec<_>>();
    assert_eq!(
        entities,
        vec![
            ProjectionDeltaEntity::ChargeSample,
            ProjectionDeltaEntity::Position,
            ProjectionDeltaEntity::Charge,
            ProjectionDeltaEntity::Drive,
            ProjectionDeltaEntity::State,
            ProjectionDeltaEntity::Update,
        ]
    );
}

#[test]
fn invalid_car_settings_reject_before_pack_output_in_every_writer_path() {
    let temporary = crate::private_tempdir().unwrap();
    let packs = temporary.path().join("full-v1");
    let mut source = snapshot();
    source.cars[0].settings.suspend_after_idle_min = 0;
    let error = ProjectionPackWriter::new(&packs)
        .write_full_snapshot(&request(&source))
        .expect_err("full snapshots reject invalid embedded car settings");
    assert!(matches!(
        error,
        ProjectionPackError::Invalid(message) if message == "car settings durations must be positive"
    ));
    assert!(!packs.exists());

    let temporary = crate::private_tempdir().unwrap();
    let packs = temporary.path().join("full-v2");
    let mut source = snapshot();
    source.cars[0].settings.suspend_min = 0;
    let error = ProjectionPackWriter::new(&packs)
        .write_full_snapshot_with_states(&request(&source), &[])
        .expect_err("stateful full snapshots reject invalid embedded car settings");
    assert!(matches!(
        error,
        ProjectionPackError::Invalid(message) if message == "car settings durations must be positive"
    ));
    assert!(!packs.exists());

    let temporary = crate::private_tempdir().unwrap();
    let packs = temporary.path().join("delta-car");
    let mut delta = sparse_delta();
    delta.cars[0].settings.suspend_min = 0;
    let error = ProjectionPackWriter::new(&packs)
        .write_delta(&delta_request(&delta))
        .expect_err("car delta upserts reject invalid embedded settings");
    assert!(matches!(
        error,
        ProjectionPackError::Invalid(message) if message == "car settings durations must be positive"
    ));
    assert!(!packs.exists());

    let temporary = crate::private_tempdir().unwrap();
    let packs = temporary.path().join("delta-patch");
    let mut delta = sparse_delta();
    delta.cars.clear();
    delta.car_settings = vec![ProjectionCarSettingsPatch {
        car_id: delta.binding.selected_car_id,
        settings: ProjectionCarSettings {
            suspend_after_idle_min: 0,
            ..ProjectionCarSettings::default()
        },
    }];
    let error = ProjectionPackWriter::new(&packs)
        .write_delta(&delta_request(&delta))
        .expect_err("settings-only delta patches reject invalid settings");
    assert!(matches!(
        error,
        ProjectionPackError::Invalid(message) if message == "car settings durations must be positive"
    ));
    assert!(!packs.exists());
}

#[test]
fn writes_sparse_schema_2_1_delta_without_base_copy() {
    let temporary = crate::private_tempdir().unwrap();
    let delta = sparse_delta();
    let built = ProjectionPackWriter::new(temporary.path().join("packs"))
        .write_delta(&delta_request(&delta))
        .unwrap();
    assert_eq!(built.metadata.schema, HUB_PROJECTION_SCHEMA_V2);
    assert_eq!(built.metadata.sequence.from_exclusive, 7);
    assert_eq!(built.metadata.sequence.to_inclusive, 8);
    assert_eq!(built.metadata.row_count, 6);
    assert!(built.metadata.tables.contains(&MirrorTable::Tombstone));

    let sqlite = zstd::stream::decode_all(File::open(&built.path).unwrap()).unwrap();
    let inspect = temporary.path().join("inspect.sqlite");
    fs::write(&inspect, sqlite).unwrap();
    let connection = Connection::open(inspect).unwrap();
    let mode: String = connection
        .query_row(
            "SELECT value FROM hub_pack_metadata WHERE key = 'mode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(mode, "typed_delta");
    let parent: String = connection
        .query_row(
            "SELECT value FROM hub_pack_metadata WHERE key = 'parent_digest'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(parent, Sha256Digest::of_bytes(b"base-lineage").to_string());
    let positions: i64 = connection
        .query_row("SELECT COUNT(*) FROM positions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(positions, 1);
    let tombstone: (String, i64, i64) = connection
        .query_row(
            "SELECT entity, entity_id, car_id FROM tombstones",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(tombstone, ("position".into(), 29, 10));
}

#[test]
fn delta_output_is_deterministic_and_rejects_bad_binding_or_parent() {
    let first_dir = crate::private_tempdir().unwrap();
    let second_dir = crate::private_tempdir().unwrap();
    let delta = sparse_delta();
    let first = ProjectionPackWriter::new(first_dir.path().join("packs"))
        .write_delta(&delta_request(&delta))
        .unwrap();
    let second = ProjectionPackWriter::new(second_dir.path().join("packs"))
        .write_delta(&delta_request(&delta))
        .unwrap();
    assert_eq!(
        fs::read(first.path).unwrap(),
        fs::read(second.path).unwrap()
    );
    assert_eq!(first.metadata.sha256, second.metadata.sha256);

    let mut bad_parent = delta.clone();
    bad_parent.parent_digest = Sha256Digest::from_bytes([0; 32]);
    assert!(matches!(
        ProjectionPackWriter::new(first_dir.path().join("bad-parent"))
            .write_delta(&delta_request(&bad_parent)),
        Err(ProjectionPackError::Invalid(_))
    ));

    let mut bad_binding = delta;
    bad_binding.positions[0].car_id = 99;
    assert!(matches!(
        ProjectionPackWriter::new(first_dir.path().join("bad-binding"))
            .write_delta(&delta_request(&bad_binding)),
        Err(ProjectionPackError::Invalid(_))
    ));
}

fn fixture_delta_request<'a>(
    delta: &'a ProjectionDelta,
    pack_id: &str,
    snapshot_id: Uuid,
) -> ProjectionDeltaPackRequest<'a> {
    ProjectionDeltaPackRequest {
        pack_id: Uuid::parse_str(pack_id).unwrap(),
        snapshot_id,
        ordinal: 0,
        delta,
    }
}

fn fixture_lineage(root: &Path) -> (LineageManifestV2, Vec<(String, Vec<u8>)>) {
    let build_root = root.join("build");
    let writer = ProjectionPackWriter::new(build_root.join("packs"));
    let source = snapshot();
    let base_request = request(&source);
    let base = writer
        .write_full_snapshot_with_states_and_updates(
            &base_request,
            &[ProjectionState {
                id: 11,
                car_id: 10,
                state: "online".into(),
                start_date_ms: 1_700_000_000_000,
                end_date_ms: None,
            }],
            &[],
        )
        .unwrap();

    let mut open_drive = source.drives[0].clone();
    open_drive.end_date_ms = 1_700_000_060_000;
    let mut new_position = source.positions[0].clone();
    new_position.id = 31;
    new_position.date_ms = 1_700_000_090_000;
    let first_delta = ProjectionDelta {
        binding: binding(),
        sequence: SequenceRange {
            from_exclusive: 7,
            to_inclusive: 8,
        },
        parent_digest: base.metadata.sha256,
        cars: vec![],
        car_settings: vec![],
        drives: vec![open_drive],
        positions: vec![new_position],
        charges: vec![],
        charge_samples: vec![],
        states: vec![],
        updates: vec![],
        tombstones: vec![],
    };
    let first = writer
        .write_delta(&fixture_delta_request(
            &first_delta,
            "88888888-8888-4888-8888-888888888881",
            base_request.snapshot_id,
        ))
        .unwrap();

    let mut closed_drive = source.drives[0].clone();
    closed_drive.end_date_ms = 1_700_000_120_000;
    let sparse_car = ProjectionCar {
        id: 10,
        name: "Road car renamed".into(),
        model: "Model 3".into(),
        vin: None,
        source_eid: None,
        source_vid: None,
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        firmware_version: None,
        efficiency_wh_per_km: None,
        settings: ProjectionCarSettings::default(),
    };
    let second_delta = ProjectionDelta {
        binding: binding(),
        sequence: SequenceRange {
            from_exclusive: 8,
            to_inclusive: 9,
        },
        parent_digest: first.metadata.sha256,
        cars: vec![sparse_car],
        car_settings: vec![],
        drives: vec![closed_drive],
        positions: vec![],
        charges: vec![],
        charge_samples: vec![],
        states: vec![],
        updates: vec![],
        tombstones: vec![ProjectionTombstone {
            entity: ProjectionDeltaEntity::Position,
            id: 30,
            car_id: 10,
        }],
    };
    let second = writer
        .write_delta(&fixture_delta_request(
            &second_delta,
            "99999999-9999-4999-8999-999999999991",
            base_request.snapshot_id,
        ))
        .unwrap();

    let key = CursorKey::from_bytes([42; 32]);
    let chain_one = Sha256Digest::of_bytes(
        format!(
            "delta-v2/{}:{}",
            base.metadata.sha256, first.metadata.sha256
        )
        .as_bytes(),
    );
    let chain_two = Sha256Digest::of_bytes(
        format!("delta-v2/{}:{}", chain_one, second.metadata.sha256).as_bytes(),
    );
    let terminal_cursor = OpaqueCursor::issue(
        &key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding().installation_id,
            account_id: binding().account_id,
            vehicle_id: binding().vehicle_id,
            generation: binding().generation,
            sequence: 9,
        },
    )
    .unwrap();
    let manifest = LineageManifestV2 {
        protocol: LINEAGE_PROTOCOL_V2,
        capability: LineageCapability::ImmutableBaseOrderedDeltas,
        schema: HUB_PROJECTION_SCHEMA_V2,
        installation_id: binding().installation_id,
        account_id: binding().account_id,
        vehicle_id: binding().vehicle_id,
        generation: 1,
        base: LineageBase {
            snapshot_id: base.metadata.snapshot_id,
            sequence: 7,
            digest: base.metadata.sha256,
            packs: vec![base.metadata.clone()],
        },
        deltas: vec![
            LineageDelta {
                from_sequence: 7,
                to_sequence: 8,
                parent_chain_digest: base.metadata.sha256,
                chain_digest: chain_one,
                pack_digest: first.metadata.sha256,
                pack: first.metadata.clone(),
            },
            LineageDelta {
                from_sequence: 8,
                to_sequence: 9,
                parent_chain_digest: chain_one,
                chain_digest: chain_two,
                pack_digest: second.metadata.sha256,
                pack: second.metadata.clone(),
            },
        ],
        head_sequence: 9,
        head_digest: chain_two,
        terminal_cursor,
    };
    manifest.validate().unwrap();
    let mut files = Vec::new();
    for (name, path) in [
        ("base", base.path),
        ("delta-0001", first.path),
        ("delta-0002", second.path),
    ] {
        files.push((name.to_owned(), fs::read(path).unwrap()));
    }
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    files.push((
        "manifest.json".into(),
        [manifest_bytes, b"\n".to_vec()].concat(),
    ));
    files.sort_by(|left, right| left.0.cmp(&right.0));
    (manifest, files)
}

fn write_fixture_set(root: &Path) {
    if root.exists() {
        fs::remove_dir_all(root).unwrap();
    }
    fs::create_dir_all(root.join("v1/packs/sha256")).unwrap();
    let (manifest, files) = fixture_lineage(&root.join("work"));
    let mut claims = Vec::new();
    for pack in manifest
        .base
        .packs
        .iter()
        .chain(manifest.deltas.iter().map(|delta| &delta.pack))
    {
        let bytes = fs::read(
            root.join("work/build/packs/sha256")
                .join(format!("{}.sqlite.zst", pack.sha256)),
        )
        .unwrap();
        let destination = root
            .join("v1/packs/sha256")
            .join(format!("{}.sqlite.zst", pack.sha256));
        fs::write(&destination, &bytes).unwrap();
        claims.push(format!(
            "{}  {} {}",
            pack.sha256,
            bytes.len(),
            pack.relative_path.trim_start_matches('/')
        ));
    }
    let manifest_bytes = files
        .iter()
        .find(|(name, _)| name == "manifest.json")
        .map(|(_, bytes)| bytes.clone())
        .unwrap();
    fs::write(root.join("manifest.json"), &manifest_bytes).unwrap();
    let digest = Sha256Digest::of_bytes(&manifest_bytes);
    claims.push(format!(
        "{}  {} manifest.json",
        digest,
        manifest_bytes.len()
    ));
    claims.sort();
    fs::write(root.join("SHA256SUMS"), format!("{}\n", claims.join("\n"))).unwrap();
    fs::remove_dir_all(root.join("work")).unwrap();
}

// These frozen pack bytes were generated by the macOS SQLite/zstd toolchain.
// Linux validates the same schema and lineage through the portable pack tests.
#[cfg(target_os = "macos")]
#[test]
fn delta_v2_fixtures_regenerate_deterministically_and_validate_lineage() {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/delta-v2");
    let (manifest, expected_files) =
        fixture_lineage(&crate::private_tempdir().unwrap().path().join("work"));
    manifest.validate().unwrap();
    for (name, expected) in expected_files {
        let actual = match name.as_str() {
            "manifest.json" => fs::read(fixture_root.join("manifest.json")).unwrap(),
            _ => {
                let pack = manifest
                    .base
                    .packs
                    .iter()
                    .chain(manifest.deltas.iter().map(|delta| &delta.pack))
                    .find(|pack| match name.as_str() {
                        "base" => **pack == manifest.base.packs[0],
                        "delta-0001" => **pack == manifest.deltas[0].pack,
                        _ => **pack == manifest.deltas[1].pack,
                    })
                    .unwrap();
                fs::read(
                    fixture_root
                        .join("v1/packs/sha256")
                        .join(format!("{}.sqlite.zst", pack.sha256)),
                )
                .unwrap()
            }
        };
        assert_eq!(actual, expected, "fixture {name}");
    }
    let parsed: LineageManifestV2 =
        serde_json::from_slice(&fs::read(fixture_root.join("manifest.json")).unwrap()).unwrap();
    parsed.validate().unwrap();
}

#[test]
#[ignore = "fixture writer; run explicitly when refreshing committed golden files"]
fn write_delta_v2_fixtures() {
    let hub_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/delta-v2");
    write_fixture_set(&hub_root);
    if let Ok(client_root) = env::var("TESLATLAS_CLIENT_FIXTURES") {
        write_fixture_set(Path::new(&client_root));
    }
}
