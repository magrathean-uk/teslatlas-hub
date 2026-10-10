// SPDX-License-Identifier: AGPL-3.0-only

//! The pairing scope flags of `pair` and `control pair`, the scope in the
//! presentation, and `control pair` beside a running Hub.

use clap::Parser;
use teslatlas_hub::{
    config::TlsListenerConfig,
    db::{HubStore, PairingScope, PublishedVehicle, SourceDescriptor, VehicleDescriptor},
};
use uuid::Uuid;

use super::{io, write_test_identity};
use crate::{
    Cli, Command, ControlCommand, PairingCommandError, PairingCommandInput,
    command_requires_user_hub_admission, execute_pairing_at, pair_with_configured_tls,
    paired_devices_json, pairing_grants_text, pairing_scope_from_flags, run_control,
};

const ALPHA: &str = "11111111-1111-4111-8111-111111111111";
const BETA: &str = "22222222-2222-4222-8222-222222222222";

fn uuid(value: &str) -> Uuid {
    Uuid::parse_str(value).expect("fixture UUID")
}

fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
    let mut all = vec!["teslatlas-hub"];
    all.extend_from_slice(arguments);
    Cli::try_parse_from(all)
}

struct Fixture {
    temporary: tempfile::TempDir,
    store: HubStore,
    named: Uuid,
    unnamed: Uuid,
    retired: Uuid,
    certificate: std::path::PathBuf,
    private_key: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().expect("temporary root");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("Hub store");
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", "pairing-scope"), 1_000)
        .expect("source");
    let register = |key: &str, name: Option<&str>| {
        let mut descriptor = VehicleDescriptor::new(source.source_id, key);
        descriptor.display_name = name.map(str::to_owned);
        store
            .register_vehicle(&descriptor, 1_000)
            .expect("vehicle")
            .vehicle_id
    };
    let named = register("named", Some("Daily driver"));
    let unnamed = register("unnamed", None);
    let retired = register("retired", Some("Old car"));
    assert!(store.retire_vehicle(retired, 2_000).expect("retire"));
    let certificate = temporary.path().join("leaf.pem");
    let private_key = temporary.path().join("private-key.pem");
    write_test_identity(&certificate, &private_key);
    Fixture {
        temporary,
        store,
        named,
        unnamed,
        retired,
        certificate,
        private_key,
    }
}

impl Fixture {
    async fn present(
        &self,
        json: bool,
        scope: PairingScope,
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, PairingCommandError> {
        let mut output = zeroize::Zeroizing::new(Vec::new());
        execute_pairing_at(
            &self.store,
            PairingCommandInput {
                label: "scope phone",
                expires_in_seconds: 900,
                json,
                public_url: "https://hub.example/",
                certificate_path: &self.certificate,
                private_key_path: &self.private_key,
                created_at_ms: 1_000,
                scope,
            },
            &mut *output,
        )
        .await?;
        Ok(output)
    }

    fn count(&self, table: &str) -> i64 {
        self.store
            .open()
            .expect("open Hub store")
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("row count")
    }

    fn all_vehicles_flag(&self) -> i64 {
        self.store
            .open()
            .expect("open Hub store")
            .query_row("SELECT all_vehicles FROM pairing_challenges", [], |row| {
                row.get(0)
            })
            .expect("all_vehicles flag")
    }

    fn granted(&self, pairing: &serde_json::Value) -> Vec<Uuid> {
        let access = self
            .store
            .claim_pairing(
                uuid(pairing["pairingId"].as_str().expect("pairing id")),
                pairing["secret"].as_str().expect("secret"),
                "scope device",
                1_500,
            )
            .expect("claim the presented invitation");
        let mut granted: Vec<Uuid> = self
            .store
            .list_paired_devices_with_vehicles()
            .expect("listing")
            .into_iter()
            .find(|listing| listing.device.device_id == access.device_id)
            .expect("claimed device")
            .vehicles;
        granted.sort();
        granted
    }
}

#[test]
fn pair_and_control_pair_parse_the_scope_flags() {
    for prefix in [
        &["pair"][..],
        &["create-pairing"][..],
        &["control", "pair"][..],
    ] {
        let command = |extra: &[&str]| {
            let mut arguments = prefix.to_vec();
            arguments.extend_from_slice(extra);
            parse(&arguments).map(|cli| match cli.command {
                Command::Pair {
                    vehicles,
                    all_vehicles,
                    ..
                } => (vehicles, all_vehicles),
                Command::Control {
                    command:
                        ControlCommand::Pair {
                            vehicles,
                            all_vehicles,
                            ..
                        },
                    ..
                } => (vehicles, all_vehicles),
                _ => panic!("not a pairing command"),
            })
        };
        assert_eq!(command(&[]).unwrap(), (vec![], false), "{prefix:?}");
        assert_eq!(
            command(&["--vehicle", ALPHA, "--vehicle", BETA]).unwrap(),
            (vec![uuid(ALPHA), uuid(BETA)], false),
            "{prefix:?}: repeatable"
        );
        assert_eq!(
            command(&["--all-vehicles"]).unwrap(),
            (vec![], true),
            "{prefix:?}"
        );
        assert!(
            command(&["--vehicle", ALPHA, "--all-vehicles"]).is_err(),
            "{prefix:?}: the two are mutually exclusive"
        );
        assert!(command(&["--vehicle", "not-a-uuid"]).is_err(), "{prefix:?}");
        assert!(command(&["--vehicle"]).is_err(), "{prefix:?}");
    }
    // The existing options keep working beside the new ones.
    let cli = parse(&[
        "pair",
        "--label",
        "Daily phone",
        "--expires-in-seconds",
        "60",
        "--vehicle",
        ALPHA,
    ])
    .expect("pair with options");
    assert!(matches!(
        cli.command,
        Command::Pair {
            expires_in_seconds: 60,
            ..
        }
    ));
}

#[test]
fn the_flags_map_to_the_store_scope() {
    assert_eq!(
        pairing_scope_from_flags(vec![], false),
        PairingScope::AllVehicles,
        "no flag is the all-vehicles default"
    );
    assert_eq!(
        pairing_scope_from_flags(vec![], true),
        PairingScope::AllVehicles
    );
    assert_eq!(
        pairing_scope_from_flags(vec![uuid(ALPHA)], false),
        PairingScope::Vehicles(vec![uuid(ALPHA)])
    );
}

#[test]
fn control_pair_needs_no_instance_lock_admission_and_pair_still_does() {
    let pair = |vehicles: Vec<Uuid>| Command::Pair {
        label: "phone".to_owned(),
        expires_in_seconds: 900,
        json: false,
        vehicles,
        all_vehicles: false,
    };
    assert!(command_requires_user_hub_admission(&pair(vec![])));
    assert!(command_requires_user_hub_admission(&pair(vec![uuid(
        ALPHA
    )])));
    for command in [
        ControlCommand::Pair {
            label: "phone".to_owned(),
            expires_in_seconds: 900,
            json: false,
            vehicles: vec![uuid(ALPHA)],
            all_vehicles: false,
        },
        ControlCommand::PairedDevices,
    ] {
        assert!(
            !command_requires_user_hub_admission(&Command::Control {
                vehicle_id: None,
                command,
            }),
            "control commands run beside a serving Hub"
        );
    }
}

#[test]
fn the_grants_line_hides_control_characters_and_names_an_unnamed_vehicle_by_id() {
    let vehicle = |vehicle_id: Uuid, display_name: Option<&str>| PublishedVehicle {
        vehicle_id,
        display_name: display_name.map(str::to_owned),
    };
    let (alpha, beta) = (uuid(ALPHA), uuid(BETA));
    // A display name is data from a provider: a terminal escape or a newline
    // in it must not reach the operator's terminal as a control character, and
    // an empty name is no name.
    let line = pairing_grants_text(Some(&[
        vehicle(alpha, Some("Car\u{1b}[31m\nred")),
        vehicle(beta, Some("")),
    ]));
    assert_eq!(line, format!("Grants: Car?[31m?red, {beta}."));
    assert!(!line.chars().any(char::is_control));
    assert_eq!(
        pairing_grants_text(Some(&[vehicle(alpha, None)])),
        format!("Grants: {alpha}.")
    );
}

#[test]
fn the_grants_line_also_hides_bidi_and_other_format_characters() {
    let vehicle = |name: &str| PublishedVehicle {
        vehicle_id: uuid(ALPHA),
        display_name: Some(name.to_owned()),
    };
    // A bidirectional override turns "Car<RLO>evil" into text that reads
    // differently from what is stored; the marks, embeddings and isolates, the
    // zero-width and joiner characters, the soft hyphen, the byte-order mark,
    // tag characters and the line and paragraph separators are all hidden.
    for (hidden, label) in [
        ('\u{200E}', "left-to-right mark"),
        ('\u{200F}', "right-to-left mark"),
        ('\u{202A}', "left-to-right embedding"),
        ('\u{202B}', "right-to-left embedding"),
        ('\u{202C}', "pop directional formatting"),
        ('\u{202D}', "left-to-right override"),
        ('\u{202E}', "right-to-left override"),
        ('\u{2066}', "left-to-right isolate"),
        ('\u{2067}', "right-to-left isolate"),
        ('\u{2068}', "first strong isolate"),
        ('\u{2069}', "pop directional isolate"),
        ('\u{061C}', "arabic letter mark"),
        ('\u{200B}', "zero width space"),
        ('\u{200C}', "zero width non-joiner"),
        ('\u{200D}', "zero width joiner"),
        ('\u{2060}', "word joiner"),
        ('\u{206F}', "nominal digit shapes"),
        ('\u{00AD}', "soft hyphen"),
        ('\u{FEFF}', "byte order mark"),
        ('\u{E0041}', "tag latin capital letter a"),
        ('\u{E007F}', "cancel tag"),
        ('\u{2028}', "line separator"),
        ('\u{2029}', "paragraph separator"),
        ('\u{0085}', "next line (a C1 control)"),
        ('\u{001B}', "escape"),
    ] {
        let line = pairing_grants_text(Some(&[vehicle(&format!("Car{hidden}evil"))]));
        assert_eq!(
            line,
            "Grants: Car?evil.",
            "{label} (U+{:04X})",
            u32::from(hidden)
        );
    }
    let line = pairing_grants_text(Some(&[vehicle(
        "a\u{202E}b\u{200F}c\u{2066}d\u{2069}e\u{1b}f",
    )]));
    assert_eq!(line, "Grants: a?b?c?d?e?f.");
    // Printable text, including other scripts, spaces and characters next to
    // the hidden ranges, is kept as it is.
    let kept =
        "M\u{fc}ller's \u{160}koda \u{540d}\u{524d} \u{1f697} \u{2027}\u{202F}\u{2070}\u{E0080}";
    assert_eq!(
        pairing_grants_text(Some(&[vehicle(kept)])),
        format!("Grants: {kept}.")
    );
}

#[tokio::test]
async fn control_vehicle_id_is_refused_for_pair_before_anything_is_opened() {
    // clap takes the flag in front of the subcommand, so the command itself
    // refuses it: a silent ignore would hand out an invitation for every car.
    let cli = parse(&["control", "--vehicle-id", ALPHA, "pair", "--vehicle", BETA])
        .expect("the flag parses ahead of the subcommand");
    let Command::Control {
        vehicle_id,
        command,
    } = cli.command
    else {
        panic!("not a control command");
    };
    assert_eq!(vehicle_id, Some(uuid(ALPHA)));
    // The configuration does not exist: the refusal comes before it is read.
    let missing = std::path::Path::new("/nonexistent/teslatlas-hub-config.toml");
    let error = run_control(missing, vehicle_id, &command)
        .await
        .expect_err("--vehicle-id must not be ignored by pair");
    let message = error.to_string();
    assert!(message.contains("--vehicle-id"), "{message}");
    assert!(message.contains("control pair --vehicle"), "{message}");
    assert!(message.contains("--all-vehicles"), "{message}");
    // Without the flag the same command goes on to read its configuration.
    let error = run_control(missing, None, &command)
        .await
        .expect_err("the configuration is missing");
    assert!(!error.to_string().contains("--vehicle-id"), "{error}");
    // The other global controls keep ignoring a vehicle they do not use.
    let paired_devices = parse(&["control", "--vehicle-id", ALPHA, "paired-devices"])
        .expect("paired-devices with a vehicle id parses");
    assert!(matches!(
        paired_devices.command,
        Command::Control {
            command: ControlCommand::PairedDevices,
            ..
        }
    ));
}

#[tokio::test]
async fn the_text_output_names_the_scope_and_the_json_output_is_unchanged() {
    let fixture = fixture();

    // Text: the granted vehicles' names, falling back to the id; the default
    // names the all-vehicles rule.
    let text = fixture
        .present(
            false,
            PairingScope::Vehicles(vec![fixture.named, fixture.unnamed]),
        )
        .await
        .expect("scoped text presentation");
    let text = std::str::from_utf8(&text).expect("terminal UTF-8");
    assert!(
        text.contains(&format!("Grants: Daily driver, {}.", fixture.unnamed)),
        "{text}"
    );
    assert!(text.contains("Pairing link: teslatlas-hub://pair?"));
    let default_text = fixture
        .present(false, PairingScope::AllVehicles)
        .await
        .expect("default text presentation");
    assert!(
        std::str::from_utf8(&default_text)
            .unwrap()
            .contains("Grants: every vehicle that is active when this invitation is claimed.")
    );

    // JSON: the same six members whatever the scope, and no scope in the URI.
    let expected_members: std::collections::BTreeSet<&str> = [
        "pairingId",
        "secret",
        "expiresAtMs",
        "endpoint",
        "tlsPin",
        "pairingUri",
    ]
    .into_iter()
    .collect();
    for scope in [
        PairingScope::AllVehicles,
        PairingScope::Vehicles(vec![fixture.named]),
    ] {
        let json = fixture
            .present(true, scope)
            .await
            .expect("JSON presentation");
        assert!(json.ends_with(b"\n"));
        let document: serde_json::Value = serde_json::from_slice(&json).expect("pairing JSON");
        let members: std::collections::BTreeSet<&str> = document
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            members, expected_members,
            "pair --json stays byte-compatible"
        );
        let uri = document["pairingUri"].as_str().unwrap();
        assert!(
            !uri.contains(&fixture.named.to_string()),
            "scope is not in the URI"
        );
        assert!(!uri.contains("vehicle"));
    }
}

#[tokio::test]
async fn a_presented_scope_is_what_the_claiming_device_is_granted() {
    let fixture = fixture();
    let granted = |json: Vec<u8>| -> serde_json::Value {
        serde_json::from_slice(&json).expect("pairing JSON")
    };

    let all = granted(
        fixture
            .present(true, PairingScope::AllVehicles)
            .await
            .unwrap()
            .to_vec(),
    );
    assert_eq!(fixture.all_vehicles_flag(), 1);
    assert_eq!(fixture.count("pairing_challenge_vehicles"), 0);
    let mut active = vec![fixture.named, fixture.unnamed];
    active.sort();
    assert_eq!(
        fixture.granted(&all),
        active,
        "all vehicles active at claim time"
    );

    let named = granted(
        fixture
            .present(true, PairingScope::Vehicles(vec![fixture.unnamed]))
            .await
            .unwrap()
            .to_vec(),
    );
    assert_eq!(fixture.all_vehicles_flag(), 0);
    assert_eq!(fixture.count("pairing_challenge_vehicles"), 1);
    assert_eq!(fixture.granted(&named), vec![fixture.unnamed]);

    // A vehicle retired between presentation and claim is not granted.
    let late = granted(
        fixture
            .present(
                true,
                PairingScope::Vehicles(vec![fixture.named, fixture.unnamed]),
            )
            .await
            .unwrap()
            .to_vec(),
    );
    assert!(fixture.store.retire_vehicle(fixture.named, 3_000).unwrap());
    assert_eq!(fixture.granted(&late), vec![fixture.unnamed]);
}

#[tokio::test]
async fn an_unknown_or_retired_vehicle_is_rejected_before_anything_is_persisted() {
    let fixture = fixture();
    for bad in [Uuid::new_v4(), fixture.retired, Uuid::nil()] {
        let error = fixture
            .present(false, PairingScope::Vehicles(vec![fixture.named, bad]))
            .await
            .expect_err("a vehicle that cannot be granted");
        assert!(
            matches!(&error, PairingCommandError::Scope(source)
                if matches!(**source, teslatlas_hub::db::StoreError::PairingVehicleUnavailable(rejected) if rejected == bad)),
            "{error:?}"
        );
        assert_eq!(fixture.count("pairing_challenges"), 0);
        assert_eq!(fixture.count("pairing_challenge_vehicles"), 0);
    }
    // Nothing is written to the terminal either.
    struct NeverWriter;
    impl io::Write for NeverWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            panic!("a rejected scope must not be presented")
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("a rejected scope must not be presented")
        }
    }
    assert!(
        execute_pairing_at(
            &fixture.store,
            PairingCommandInput {
                label: "scope phone",
                expires_in_seconds: 900,
                json: true,
                public_url: "https://hub.example/",
                certificate_path: &fixture.certificate,
                private_key_path: &fixture.private_key,
                created_at_ms: 1_000,
                scope: PairingScope::Vehicles(vec![fixture.retired]),
            },
            &mut NeverWriter,
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn control_pair_works_while_another_process_holds_the_instance_lock() {
    use teslatlas_hub::hub_user_process::AdmittedUserHub;

    let fixture = fixture();
    let data_dir = fixture.temporary.path().join("hub");
    // A serving Hub holds the lifetime lock for its whole life; `pair` needs
    // that same admission and is refused, `control pair` does not.
    let _serving = AdmittedUserHub::admit(&data_dir).expect("serve-like lock holder");
    assert!(
        AdmittedUserHub::admit(&data_dir).is_err(),
        "a second admission is refused while the Hub serves"
    );
    let control_store = HubStore::open_existing(&data_dir).expect("control store");
    let tls = TlsListenerConfig {
        certificate_path: fixture.certificate.clone(),
        private_key_path: fixture.private_key.clone(),
        public_url: "https://hub.example/".to_owned(),
    };
    let mut output = Vec::new();
    pair_with_configured_tls(
        &control_store,
        Some(&tls),
        "control phone",
        900,
        true,
        PairingScope::Vehicles(vec![fixture.named]),
        &mut output,
    )
    .await
    .expect("control pair beside a running Hub");
    let pairing: serde_json::Value = serde_json::from_slice(&output).expect("pairing JSON");
    assert_eq!(fixture.granted(&pairing), vec![fixture.named]);
    // The listing shows the claimed device with its vehicle.
    let listing: serde_json::Value =
        serde_json::from_str(&paired_devices_json(&control_store).expect("listing")).unwrap();
    assert_eq!(listing.as_array().unwrap().len(), 1);
    assert_eq!(
        listing[0]["vehicles"],
        serde_json::json!([fixture.named.to_string()])
    );
    assert_eq!(listing[0]["display_name"], "scope device");
    assert!(listing[0].get("token_sha256").is_none());

    // Without configured TLS the command says so and persists nothing.
    let error = pair_with_configured_tls(
        &control_store,
        None,
        "no tls",
        900,
        false,
        PairingScope::AllVehicles,
        &mut Vec::new(),
    )
    .await
    .expect_err("TLS is required");
    assert!(error.to_string().contains("requires configured TLS"));
    assert_eq!(fixture.count("pairing_challenges"), 0);
}
