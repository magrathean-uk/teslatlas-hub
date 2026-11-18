// SPDX-License-Identifier: AGPL-3.0-only

//! The Fleet command-line grammar and settings tests (M09-T19).

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
