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

/// Every refusal code a `setup-fleet` verb can give (M09-T19a section 11,
/// with the configuration and rename codes of M09-T24). No type enumerates
/// them: an `OperatorRefusal` carries its code as text, built where the
/// refusal arises, so the list is the contract's table.
const FLEET_REFUSAL_CODES: &[&str] = &[
    // Every forwarded verb, before the service answers.
    "config_unreadable", "config_invalid", "config_rename_failed", "service_not_running",
    "service_unauthenticated", "control_channel_failed", "control_channel_unsafe",
    "control_channel_busy", "credential_recovery_pending", "input_unreadable",
    // begin and redeem.
    "setup_input_invalid", "source_credential_disabled", "sign_in_settings_invalid", "window_closed",
    "sign_in_busy", "collector_still_running", "sign_in_unavailable", "discovery_selection_invalid",
    "sign_in_binding_invalid", "sign_in_expired", "sign_in_material_invalid", "sign_in_code_reused",
    "sign_in_refused", "sign_in_answer_invalid", "fleet_scope_missing_vehicle_device_data",
    "fleet_scope_missing_vehicle_location", "fleet_scopes_missing", "fleet_token_claims_invalid",
    "sign_in_stored_unconfirmed", "setup_marker_unavailable",
    // The vault's codes.
    "keyring_changed", "keyring_exists", "keyring_invalid", "keyring_missing", "keyring_newer",
    "keyring_unreadable", "keyring_unsafe", "credential_store_unavailable", "durability_unavailable",
    "secrets_backup_exclusion_failed", "binding_mismatch", "corrupt", "credential_lock_poisoned",
    "empty_access_token", "empty_refresh_token", "fresh_sign_in_required", "invalid_origin",
    "migration_pending", "over_cap", "refresh_token_spent", "sensitive_access_unavailable",
    "slot_occupied", "successor_not_canonical", "token_too_large", "unknown_format", "unknown_key",
    "unknown_origin",
    // discover.
    "discovery_pair_unavailable", "discovery_settings_invalid", "discovery_selection_required",
    "discovery_vehicle_not_listed", "discovery_no_vehicles", "discovery_vehicle_conflict",
    "discovery_refused", "discovery_rate_limited", "discovery_unavailable", "discovery_answer_invalid",
    "discovery_failed",
    // activate.
    "fleet_activation_unavailable", "fleet_sign_in_required", "fleet_vehicles_required",
    "collection_interval_unavailable",
    // register and public-key.
    "fleet_partner_settings_invalid", "provider_parked", "fleet_partner_timeout", "fleet_partner_transport",
    "fleet_partner_auth_failed", "fleet_partner_public_key_mismatch", "fleet_partner_over_limit",
    "fleet_partner_response_1xx", "fleet_partner_response_2xx", "fleet_partner_response_3xx",
    "fleet_partner_response_4xx", "fleet_partner_response_5xx", "fleet_partner_registration_failed",
    "fleet_partner_unavailable", "fleet_partner_key_busy", "fleet_partner_key_backup_exclusion",
    "fleet_partner_key_storage", "fleet_partner_key_unsafe", "fleet_partner_key_invalid",
    "fleet_partner_key_generation", "fleet_public_key_path_protected", "fleet_public_key_unwritable",
];

/// M09-T24: the one Fleet report writes every refusal code, also those only
/// Tesla, an Open window or a stored login reach, as the contract's bare
/// `<code>: <text>` line and exit 1; an error without a code keeps the form
/// every other command reports.
#[test]
fn every_fleet_refusal_code_is_reported_as_its_bare_contract_line() {
    use tesatlas_hub::runtime::operator::admission::OperatorRefusal;
    let codes: std::collections::BTreeSet<&str> = FLEET_REFUSAL_CODES.iter().copied().collect();
    assert_eq!(codes.len(), FLEET_REFUSAL_CODES.len(), "each code once");
    for code in FLEET_REFUSAL_CODES {
        let text = format!("the fixed text of {code}; nothing was changed");
        let mut stderr = Vec::new();
        let status = crate::report_fleet_outcome(Err(Box::new(OperatorRefusal::new(code, text.clone()))), &mut stderr);
        assert_eq!(status, 1, "{code}");
        assert_eq!(String::from_utf8(stderr).expect("UTF-8"), format!("{code}: {text}\n"), "{code}");
    }
    let mut stderr = Vec::new();
    assert_eq!(crate::report_fleet_outcome(Ok(()), &mut stderr), 0);
    assert!(stderr.is_empty(), "success writes nothing");
    let mut stderr = Vec::new();
    assert_eq!(crate::report_fleet_outcome(Err(std::io::Error::other("not a refusal").into()), &mut stderr), 1);
    assert_eq!(String::from_utf8(stderr).expect("UTF-8"), "tesatlas-hub: not a refusal\n");
}
