// SPDX-License-Identifier: AGPL-3.0-only

use crate::*;
use teslatlas_hub::source_db_reader::{
    SourceDbConnectionDiagnostics, SourceDbOpenSessionCounts, SourceDbSchemaInfo,
    SourceDbSelectedCarCounts, SourceDbSelectedCarDiagnostics, SourceDbTotals,
};

fn synthetic_source_check_snapshot() -> SourceDbCheckSnapshot {
    SourceDbCheckSnapshot {
        schema: SourceDbSchemaInfo {
            observed_migration_version: MAX_VALIDATED_MIGRATION,
            observed_migration_count: 105,
            minimum_supported_migration_version: MAX_VALIDATED_MIGRATION,
            maximum_validated_migration_version: MAX_VALIDATED_MIGRATION,
            pinned_source_revision: SOURCE_DB_V4_SOURCE_REVISION,
            pinned_migration_set_sha256:
                teslatlas_hub::source_db_schema::SOURCE_DB_V4_MIGRATION_SET_SHA256,
            fingerprint: "synthetic-schema-fingerprint".to_owned(),
        },
        connection: SourceDbConnectionDiagnostics {
            current_user: "history_reader".to_owned(),
            database: "sourcedb".to_owned(),
            server_address: "192.0.2.10".to_owned(),
            server_port: 5432,
            postmaster_start_epoch_seconds: 1_700_000_000,
            transaction_read_only: true,
            private_schema_usage: false,
        },
        selected_car: SourceDbSelectedCarDiagnostics {
            id: 42,
            name: Some("Example car".to_owned()),
            model: Some("3".to_owned()),
            vin_present: false,
        },
        open_sessions: SourceDbOpenSessionCounts {
            drives: 0,
            charging_processes: 0,
            states: 0,
        },
        selected_car_counts: SourceDbSelectedCarCounts {
            drives: 2,
            positions: 8,
            charging_processes: 1,
            charges: 3,
            states: 4,
            updates: 1,
        },
        source_totals: SourceDbTotals {
            cars: 1,
            drives: 2,
            positions: 8,
            charging_processes: 1,
            charges: 3,
            states: 4,
            updates: 1,
            schema_migrations: 105,
        },
        source_tokens_relation_present: None,
        legacy_token_pair: None,
    }
}

#[test]
fn source_check_reports_neutral_keys_and_schema_recognised() {
    let actual = source_check_success_report(42, &synthetic_source_check_snapshot(), true, true);
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../macos/TeslatlasHubApp/TeslatlasHubAppTests/Fixtures/source-check-recognised.json"
    ))
    .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(actual["sourceSchemaRecognised"], true);
    assert_eq!(actual["schemaSetLabel"], "ea850d1b038c");
    let forbidden = ["tesl", "amate"].concat();
    assert!(
        !serde_json::to_string(&actual)
            .unwrap()
            .to_lowercase()
            .contains(&forbidden)
    );
    let pending = source_check_success_report(42, &synthetic_source_check_snapshot(), true, false);
    assert_eq!(pending["status"], "confirmation_required");
    assert_eq!(pending["sourceSchemaRecognised"], true);
    assert_eq!(pending["schemaSetLabel"], "ea850d1b038c");
}

#[test]
fn source_check_not_recognised_report_is_false_without_label() {
    let (_, _, observed, guidance) =
        source_check_failure_details(&SourceDbReaderError::MissingMigrationVersion);
    let actual =
        source_check_failure_report(42, "incompatible", "schema_mismatch", observed, guidance);
    let expected:serde_json::Value=serde_json::from_str(include_str!("../../../macos/TeslatlasHubApp/TeslatlasHubAppTests/Fixtures/source-check-not-recognised.json")).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(actual["sourceSchemaRecognised"], false);
    for key in [
        "schemaSetLabel",
        "credentialScope",
        "applicationVersionStatus",
    ] {
        assert!(actual.get(key).is_none());
    }
}

#[test]
fn source_check_unreached_comparison_reports_null() {
    for reason in [
        "invalid_source",
        "source_unavailable",
        "legacy_token_pair_invalid",
        "selected_car_missing",
    ] {
        let actual = source_check_failure_report(
            42,
            "unavailable",
            reason,
            None,
            "Check the source database connection.",
        );
        assert!(actual["sourceSchemaRecognised"].is_null());
        assert!(actual.get("schemaSetLabel").is_none());
    }
    for reason in [
        "older_than_v4_2_compatible_schema",
        "newer_than_v4_2_compatible_schema",
        "schema_mismatch",
    ] {
        assert_eq!(
            source_check_failure_report(42, "incompatible", reason, None, "Check the schema.")["sourceSchemaRecognised"],
            false
        );
    }
    assert_eq!(schema_set_label("abc"), None);
    assert_eq!(schema_set_label(&"A".repeat(64)), None);
}

#[test]
fn unknown_subcommand_names_source_check() {
    for command in [
        "unlisted-action".to_owned(),
        ["tesl", "amate-check"].concat(),
    ] {
        let error = Cli::try_parse_from(["teslatlas-hub", &command])
            .unwrap_err()
            .to_string();
        assert!(!error.contains("error: error:"), "{error}");
        assert!(
            error.contains("Import checks use source-check; see docs/guides/cli.md."),
            "{error}"
        );
    }
}

#[test]
fn explicit_and_configured_import_source_keys_share_validation() {
    let oversized = "a".repeat(257);
    for invalid in [
        "postgres://private",
        "user@host",
        "line\nbreak",
        "\t",
        "",
        oversized.as_str(),
    ] {
        let explicit = migration_source_key(Some(invalid), Some("home-source"));
        let configured = migration_source_key(None, Some(invalid));
        assert!(teslatlas_hub::config::validate_import_source_key(explicit).is_err());
        assert!(teslatlas_hub::config::validate_import_source_key(configured).is_err());
    }
    assert!(
        teslatlas_hub::config::validate_import_source_key(migration_source_key(None, None)).is_ok()
    );
    for key in ["a".repeat(256), "é".repeat(128)] {
        for selected in [
            migration_source_key(Some(&key), None),
            migration_source_key(None, Some(&key)),
        ] {
            assert!(teslatlas_hub::config::validate_import_source_key(selected).is_ok());
        }
    }
}

fn schema_arguments(command: &str, flag: &str) -> Vec<String> {
    let mut args = [
        "teslatlas-hub",
        command,
        "--source",
        "postgresql://reader@source.example/sourcedb",
        "--car-id",
        "42",
        "--postgres-password-file",
        "fictional-password",
        "--history-only",
        flag,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if command == "migrate" {
        args.push("--online-snapshot".to_owned());
    }
    args
}

#[test]
fn old_schema_flag_alias_works_warns_and_is_hidden() {
    for command in ["source-check", "migrate"] {
        let mut warnings = Vec::new();
        let cli = Cli::parse_with_schema_warning(
            schema_arguments(command, "--acknowledge-v4-2-compatible-schema"),
            &mut warnings,
        )
        .unwrap();
        match cli.command {
            Command::SourceCheck {
                accept_schema_evidence,
                ..
            }
            | Command::Migrate {
                accept_schema_evidence,
                ..
            } => assert!(accept_schema_evidence),
            _ => panic!("wrong command"),
        }
        let warnings = String::from_utf8(warnings).unwrap();
        assert_eq!(warnings.lines().count(), 1);
        assert!(warnings.contains("--accept-schema-evidence"));
        assert!(warnings.contains("a later version"));
        let help = Cli::try_parse_from(["teslatlas-hub", command, "--help"])
            .unwrap_err()
            .to_string();
        assert!(help.contains("--accept-schema-evidence"));
        assert!(!help.contains("--acknowledge-v4-2-compatible-schema"));
    }
}

#[test]
fn accept_schema_evidence_flag_works_for_check_and_migrate() {
    for command in ["source-check", "migrate"] {
        let mut warnings = Vec::new();
        let cli = Cli::parse_with_schema_warning(
            schema_arguments(command, "--accept-schema-evidence"),
            &mut warnings,
        )
        .unwrap();
        assert!(warnings.is_empty());
        match cli.command {
            Command::SourceCheck {
                accept_schema_evidence,
                ..
            }
            | Command::Migrate {
                accept_schema_evidence,
                ..
            } => assert!(accept_schema_evidence),
            _ => panic!("wrong command"),
        }
    }
}

#[test]
fn mt4_3_unknown_key_is_refused_unless_new_import_source() {
    let temp = tempfile::tempdir().unwrap();
    let store = HubStore::initialize(temp.path()).unwrap();
    let first = store
        .register_import_source("home-source", 1000, false)
        .unwrap();
    let before = store.runtime_inventory().unwrap();
    let error = store
        .register_import_source("unmatched-private-key", 2000, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("stored keys: home-source"));
    assert!(error.contains("--new-import-source"));
    assert!(!error.contains("unmatched-private-key"));
    assert_eq!(
        store
            .register_import_source("home-source", 2000, false)
            .unwrap()
            .source_id,
        first.source_id
    );
    let second = store
        .register_import_source("garage-source", 2000, true)
        .unwrap();
    assert_ne!(first.source_id, second.source_id);
    assert_eq!(store.runtime_inventory().unwrap().vehicles, before.vehicles);
    let mut args = schema_arguments("migrate", "--accept-schema-evidence");
    args.extend([
        "--source-key".to_owned(),
        "garage-source".to_owned(),
        "--new-import-source".to_owned(),
    ]);
    assert!(
        matches!(Cli::try_parse_from(args).unwrap().command,Command::Migrate{new_import_source:true,source_key:Some(key),..} if key=="garage-source")
    );
}

fn preceding_import_fixture() -> (tempfile::TempDir, HubStore) {
    use std::os::unix::fs::PermissionsExt;
    let temporary = tempfile::tempdir().unwrap();
    let bytes = zstd::stream::decode_all(
        include_bytes!("../../../fixtures/n1/v77-catalogue.sqlite.zst").as_slice(),
    )
    .unwrap();
    let path = temporary.path().join("hub.sqlite");
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    (temporary, store)
}

fn assert_migration_argument_identity(arguments: Vec<String>) {
    let (_temporary, store) = preceding_import_fixture();
    let cli = Cli::try_parse_from(arguments).unwrap();
    let Command::Migrate {
        source_key,
        new_import_source,
        history_only,
        online_snapshot,
        encryption_key_file,
        access_token_file,
        refresh_token_file,
        ..
    } = cli.command
    else {
        panic!("migration command required")
    };
    assert!(history_only && online_snapshot);
    assert!(
        encryption_key_file.is_none()
            && access_token_file.is_none()
            && refresh_token_file.is_none()
    );
    let key = migration_source_key(source_key.as_deref(), None);
    assert_eq!(key, "default");
    store
        .check_import_source_key(key, new_import_source)
        .unwrap();
    let source = store
        .register_import_source(key, 3000, new_import_source)
        .unwrap();
    assert_eq!(
        source.source_id.to_string(),
        "10000000-0000-4000-8000-000000000001"
    );
    assert_eq!(source.kind, "source_db");
    let vehicle = store
        .register_vehicle(
            &teslatlas_hub::db::VehicleDescriptor::new(source.source_id, "car-1"),
            3000,
        )
        .unwrap();
    assert_eq!(
        vehicle.vehicle_id.to_string(),
        "20000000-0000-4000-8000-000000000001"
    );
    assert_eq!(store.runtime_inventory().unwrap().vehicles, 66);
}

#[test]
fn mt4_1_keyless_migrate_reuses_the_existing_source() {
    assert_migration_argument_identity(schema_arguments("migrate", "--accept-schema-evidence"));
}

#[test]
fn mt4_2_gui_argument_vector_reuses_the_existing_source() {
    let arguments: Vec<String> = serde_json::from_str(include_str!(
        "../../../macos/TeslatlasHubApp/TeslatlasHubAppTests/Fixtures/import-history-argv.json"
    ))
    .unwrap();
    assert_migration_argument_identity(arguments);
    assert_eq!(
        migration_source_key(Some("garage-source"), Some("home-source")),
        "garage-source"
    );
    assert_eq!(
        migration_source_key(None, Some("home-source")),
        "home-source"
    );
}

#[tokio::test]
async fn mt4_3_cli_refusal_preserves_abandoned_generation_before_initialisation() {
    use std::os::unix::fs::PermissionsExt;
    for history_only in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let store = HubStore::initialize(&data_dir).unwrap();
        let source = store
            .register_import_source("home-source", 1000, false)
            .unwrap();
        let vehicle = store
            .register_vehicle(
                &teslatlas_hub::db::VehicleDescriptor::new(source.source_id, "car-1"),
                1000,
            )
            .unwrap();
        let connection = store.open().unwrap();
        connection.execute("INSERT INTO import_generations(run_id,source_id,vehicle_id,car_id,status,created_at_ms) VALUES (?1,?2,?3,42,'staging',1000)", rusqlite::params!["60000000-0000-4000-8000-000000000001",source.source_id.to_string(),vehicle.vehicle_id.to_string()]).unwrap();
        drop(connection);
        store.checkpoint_catalogue_for_immutable_read().unwrap();
        let staging = store.packs_dir().join(".staging");
        fs::create_dir_all(&staging).unwrap();
        let artefact = staging.join("synthetic-import-evidence");
        fs::write(&artefact, b"preserve refused import evidence").unwrap();
        let config_path = temp.path().join("config.toml");
        fs::write(&config_path, format!("data_dir = {data_dir:?}\nbind = '127.0.0.1:18443'\n[collector]\ninterval_seconds = 0\n[collector.legacy_auth]\nenabled = false\n")).unwrap();
        let password_path = temp.path().join("password");
        fs::write(&password_path, "synthetic-password").unwrap();
        fs::set_permissions(&password_path, fs::Permissions::from_mode(0o600)).unwrap();
        let key_path = temp.path().join("unread-source-key");
        let admission = AdmittedUserHub::admit(&data_dir).unwrap();
        let before = fs::read(store.database_path()).unwrap();
        let error = run_macos_migration(
            &admission,
            MacMigrationInput {
                config_path: &config_path,
                source_url: "postgresql://reader@127.0.0.1:9/sourcedb",
                car_id: 42,
                source_key: Some("unmatched-private-key"),
                new_import_source: false,
                postgres_password_file: &password_path,
                history_only,
                encryption_key_file: if history_only { None } else { Some(&key_path) },
                access_token_file: None,
                refresh_token_file: None,
                online_snapshot: true,
                preserve_existing_credentials: false,
            },
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("stored keys: home-source"), "{error}");
        assert!(!error.contains("unmatched-private-key"));
        let connection = rusqlite::Connection::open_with_flags(
            store.database_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let count: i64 = connection
            .query_row("SELECT count(*) FROM import_generations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "refusal must precede startup generation cleanup");
        assert_eq!(fs::read(store.database_path()).unwrap(), before);
        assert_eq!(
            fs::read(&artefact).unwrap(),
            b"preserve refused import evidence"
        );
    }
}
