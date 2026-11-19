// SPDX-License-Identifier: AGPL-3.0-only

//! The Fleet command-line grammar and settings tests (M09-T19), and the
//! vehicle actions' parse and refusal lines (DEC-246).

use super::*;

#[cfg(target_os = "macos")]
#[test]
fn fleet_sign_in_setup_fleet_grammar_routes_to_the_service() {
    use tesatlas_hub::runtime::operator::{admission::{Route, requires_user_hub_admission as command_requires_user_hub_admission, route}, cli::FleetSetupStep};
    let parse = |args: &[&str]| Cli::try_parse_from([&["tesatlas-hub"][..], args].concat()).map(|cli| cli.command);
    assert!(matches!(parse(&["setup-fleet"]).expect("redeem"), Command::SetupFleet { step: None, vehicle_id: None, all_vehicles: false }));
    assert!(matches!(parse(&["setup-fleet", "--vehicle-id", "70"]).expect("redeem"), Command::SetupFleet { step: None, vehicle_id: Some(70), all_vehicles: false }));
    assert!(matches!(parse(&["setup-fleet", "--all-vehicles"]).expect("redeem"), Command::SetupFleet { step: None, vehicle_id: None, all_vehicles: true }));
    assert!(matches!(parse(&["setup-fleet", "begin"]).expect("begin"), Command::SetupFleet { step: Some(FleetSetupStep::Begin), .. }));
    for args in [&["setup-fleet", "--sign-in"][..], &["setup-fleet", "--vehicle-id", "70", "--all-vehicles"], &["setup-fleet", "begin", "--all-vehicles"],
        &["setup-fleet", "--vehicle-id", "70", "begin"], &["setup-fleet", "--client-secret", "x"], &["setup-fleet", "--access-token-file", "x"]] {
        assert!(parse(args).is_err(), "{args:?}");
    }
    for command in [parse(&["setup-fleet"]).expect("redeem"), parse(&["setup-fleet", "begin"]).expect("begin")] {
        assert_eq!(route(&command), Route::Service);
        assert!(!command_requires_user_hub_admission(&command), "forwarded, never the lifetime lock");
    }
    assert_eq!(route(&Command::ConfigureFleetTelemetry), Route::Refused("provider_parked"));
}

#[cfg(target_os = "macos")]
#[test]
fn fleet_rejects_enabling_legacy_streaming() {
    use tesatlas_hub::config::CollectorProvider;

    assert!(validate_streaming_setting(CollectorProvider::Fleet, Some(true)).is_err());
    assert!(validate_streaming_setting(CollectorProvider::Fleet, Some(false)).is_ok());
    assert!(validate_streaming_setting(CollectorProvider::Legacy, Some(true)).is_ok());
}

/// DEC-246: the command line's lines for what the running service answers. A
/// missing service and a closed window share the window line; the Fleet route,
/// an action outside the permitted set and an unknown action carry their own
/// codes; anything else keeps the service's text (an unknown outcome says the
/// action may have been sent and not to repeat it).
#[test]
fn dec246_vehicle_action_refusal_lines_carry_their_codes() {
    use tesatlas_hub::collector::ResidentVehicleActionError as Refusal;
    let window = format!("collection_window_closed: {}", tesatlas_hub::collector::VEHICLE_ACTION_WINDOW_CLOSED);
    assert_eq!(crate::vehicle_action_refusal_line(&Refusal::WindowClosed), window);
    assert_eq!(crate::vehicle_action_refusal_line(&Refusal::Unavailable), window);
    assert_eq!(
        crate::vehicle_action_refusal_line(&Refusal::CommandsNotOffered),
        format!("vehicle_commands_not_offered: {}", tesatlas_hub::collector::VEHICLE_COMMANDS_NOT_OFFERED)
    );
    assert_eq!(
        crate::vehicle_action_refusal_line(&Refusal::UnknownAction),
        format!("vehicle_action_unknown: {}", tesatlas_hub::collector::VEHICLE_ACTION_UNKNOWN)
    );
    assert_eq!(
        crate::vehicle_action_refusal_line(&Refusal::NotPermitted),
        format!("vehicle_action_not_permitted: {}", tesatlas_hub::collector::VEHICLE_ACTION_NOT_PERMITTED)
    );
    // A send whose answer was lost is reported as possibly sent, with its
    // receipt, and never as a rejection.
    assert_eq!(
        crate::vehicle_action_refusal_line(&Refusal::OutcomeAmbiguous { audit_receipt_id: Some(42) }),
        "vehicle command outcome is unknown: it may have been sent (audit receipt 42); do not repeat it"
    );
    assert_eq!(
        crate::vehicle_action_refusal_line(&Refusal::OutcomeAmbiguous { audit_receipt_id: None }),
        "vehicle command outcome is unknown: it may have been sent; do not repeat it"
    );
}

/// DEC-246: every vehicle action parses to its Legacy action and carries
/// whether `--confirm` was given.
#[test]
fn dec246_every_vehicle_action_parses_with_its_confirm() {
    use tesatlas_hub::owner_api::LegacyVehicleAction as Action;
    for (arguments, expected) in [
        (&["wake"][..], Action::Wake),
        (&["climate-start"][..], Action::ClimateStart),
        (&["climate-stop"][..], Action::ClimateStop),
        (&["charge-start"][..], Action::ChargeStart),
        (&["charge-stop"][..], Action::ChargeStop),
        (&["set-charge-limit", "--percent", "80"][..], Action::SetChargeLimit(80)),
        (&["lock"][..], Action::Lock),
        (&["unlock"][..], Action::Unlock),
        (&["flash-lights"][..], Action::FlashLights),
        (&["honk-horn"][..], Action::HonkHorn),
    ] {
        for confirm in [false, true] {
            let mut all = vec!["tesatlas-hub", "control"];
            all.extend_from_slice(arguments);
            if confirm {
                all.push("--confirm");
            }
            let Command::Control { command, .. } = Cli::try_parse_from(&all).expect("parses").command else {
                panic!("{all:?}: not a control command");
            };
            assert_eq!(crate::vehicle_action(&command), Some((expected, confirm)), "{all:?}");
        }
    }
    let Command::Control { command, .. } = Cli::try_parse_from(["tesatlas-hub", "control", "geofences"]).expect("parses").command else {
        panic!("not a control command");
    };
    assert_eq!(crate::vehicle_action(&command), None);
}
