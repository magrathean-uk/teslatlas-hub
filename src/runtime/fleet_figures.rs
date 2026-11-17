// SPDX-License-Identifier: AGPL-3.0-only

//! `setup-fleet figures` (M09-T21, DEC-225): the Fleet figures in effect, as the
//! service states them, for the Mac app's consent page.
//!
//! The configuration is read through the service's own loader, so every valid
//! spelling of a setting gives the figure the running service uses, and the
//! figures come from the policy the collector's scheduler and the monthly cap
//! are built from (`CollectorConfig::cadence` then `fleet_policy`). Nothing is
//! opened, sent or written; the data folder need not exist.

use std::path::Path;

use serde::Serialize;

use crate::runtime::config::{
    CadenceViolation, CollectorMode, ConfigError, FleetPolicy, HubConfig, collector_mode,
};
use crate::runtime::operator::admission::OperatorRefusal;

/// The answer of `setup-fleet figures`; the field order is the contract's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetFigures {
    pub provider: &'static str,
    pub collection_interval_seconds: u64,
    pub polling_seconds: FleetPollingSeconds,
    pub requests_per_dollar: FleetPollingPrices,
    pub monthly_cap_cents: u32,
}

/// The read interval of every Fleet polling class, in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetPollingSeconds {
    pub driving: u64,
    pub charging: u64,
    pub updating: u64,
    pub online: u64,
    pub state_check: u64,
    pub inventory: u64,
}

/// The prices of the two request kinds polling sends, in requests per US
/// dollar; `state: None` means Tesla does not bill a vehicle list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FleetPollingPrices {
    pub data: u32,
    pub state: Option<u32>,
}

impl FleetFigures {
    fn from_policy(collection_interval_seconds: u64, policy: &FleetPolicy) -> Self {
        let intervals = &policy.intervals;
        Self {
            provider: "fleet",
            collection_interval_seconds,
            polling_seconds: FleetPollingSeconds {
                driving: intervals.driving.as_secs(),
                charging: intervals.charging.as_secs(),
                updating: intervals.updating.as_secs(),
                online: intervals.online.as_secs(),
                state_check: intervals.state_check.as_secs(),
                inventory: intervals.inventory.as_secs(),
            },
            requests_per_dollar: FleetPollingPrices {
                data: policy.pricing.data,
                state: policy.pricing.state,
            },
            monthly_cap_cents: policy.monthly_cap_cents,
        }
    }
}

pub const CONFIG_UNREADABLE: &str = "config_unreadable";
pub const CONFIG_INVALID: &str = "config_invalid";
pub const FLEET_PROVIDER_NOT_SELECTED: &str = "fleet_provider_not_selected";

/// Reads the configuration at `config_path` with the service's loader and
/// states its Fleet figures.
pub fn load_fleet_figures(config_path: &Path) -> Result<FleetFigures, OperatorRefusal> {
    let config = HubConfig::load(config_path).map_err(|error| config_refusal(&error))?;
    fleet_figures(&config)
}

/// The Fleet figures of a loaded configuration.
pub fn fleet_figures(config: &HubConfig) -> Result<FleetFigures, OperatorRefusal> {
    if collector_mode(&config.collector) != CollectorMode::FleetPolling {
        return Err(OperatorRefusal::new(
            FLEET_PROVIDER_NOT_SELECTED,
            "the configuration does not select the Fleet provider",
        ));
    }
    let cadence = config.collector.cadence().map_err(|_| {
        OperatorRefusal::new(
            CadenceViolation::CODE,
            "a polling setting of the configuration is invalid; the service does not collect with it",
        )
    })?;
    Ok(FleetFigures::from_policy(
        config.collector.interval_seconds,
        &cadence.fleet_policy(),
    ))
}

fn config_refusal(error: &ConfigError) -> OperatorRefusal {
    match error {
        ConfigError::Read
        | ConfigError::TooLarge
        | ConfigError::UnsafeFile
        | ConfigError::FileIdentityChanged => OperatorRefusal::new(
            CONFIG_UNREADABLE,
            "the configuration file cannot be read safely",
        ),
        _ => OperatorRefusal::new(
            CONFIG_INVALID,
            "the configuration is not a valid Hub configuration",
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::runtime::config::FLEET_PRICING;

    struct Folder {
        dir: tempfile::TempDir,
    }

    impl Folder {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().expect("temporary folder"),
            }
        }

        /// The data folder the configurations name; it is never created.
        fn data(&self) -> PathBuf {
            self.dir.path().join("absent-data")
        }

        /// A configuration file of `collector` under a valid head.
        fn config(&self, name: &str, collector: &str) -> PathBuf {
            let path = self.dir.path().join(name);
            let text = format!(
                "data_dir = {:?}\nbind = '127.0.0.1:8080'\n{collector}",
                self.data().to_str().expect("path")
            );
            fs::write(&path, text).expect("configuration");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
            path
        }

        fn figures(&self, name: &str, collector: &str) -> Result<FleetFigures, OperatorRefusal> {
            load_fleet_figures(&self.config(name, collector))
        }
    }

    const FLEET: &str = "[collector]\nprovider = 'fleet'\ninterval_seconds = 0\n";

    const PLAIN: &str = "[collector]\nprovider = 'fleet'\ninterval_seconds = 300\n\
         [collector.fleet_polling]\nmonthly_cap_cents = 2000\ndriving_poll_seconds = 60\n";

    fn code(result: Result<FleetFigures, OperatorRefusal>) -> String {
        result.expect_err("refused").code
    }

    #[test]
    fn fleet_figures_state_the_defaults_in_the_contract_order() {
        let folder = Folder::new();
        let figures = folder.figures("hub.toml", FLEET).expect("figures");
        assert_eq!(
            serde_json::to_string(&figures).expect("json"),
            r#"{"provider":"fleet","collectionIntervalSeconds":0,"pollingSeconds":{"driving":30,"charging":120,"updating":300,"online":120,"stateCheck":600,"inventory":3600},"requestsPerDollar":{"data":500,"state":500},"monthlyCapCents":1000}"#
        );
    }

    #[test]
    fn fleet_figures_follow_a_plain_cap_and_polling_override() {
        let folder = Folder::new();
        let figures = folder.figures("hub.toml", PLAIN).expect("figures");
        assert_eq!(figures.collection_interval_seconds, 300);
        assert_eq!(figures.monthly_cap_cents, 2000);
        assert_eq!(figures.polling_seconds.driving, 60);
        assert_eq!(figures.polling_seconds.charging, 120);
        let updating = folder
            .figures(
                "updating.toml",
                "[collector]\nprovider = 'fleet'\n[collector.fleet_polling]\n\
                 updating_poll_seconds = 900\nstate_poll_seconds = 1200\n",
            )
            .expect("figures");
        assert_eq!(updating.polling_seconds.updating, 900);
        assert_eq!(updating.polling_seconds.state_check, 1200);
        assert_eq!(updating.polling_seconds.inventory, 3600);
    }

    /// R2 of the other vendor's review: a quoted, escaped table name, a dotted
    /// key and an inline table are the same settings as the plain form.
    #[test]
    fn fleet_figures_read_every_spelling_as_the_plain_form() {
        let folder = Folder::new();
        let plain = folder.figures("plain.toml", PLAIN).expect("plain");
        let spellings = [
            "[collector]\nprovider = \"fleet\"\ninterval_seconds = 300\n\n\
             [collector.\"fleet\\u005fpolling\"]\nmonthly_cap_cents = 2000\ndriving_poll_seconds = 60\n",
            "[collector]\nprovider = 'fleet'\ninterval_seconds = 300\n\
             fleet_polling.monthly_cap_cents = 2000\nfleet_polling.driving_poll_seconds = 60\n",
            "[collector]\nprovider = 'fleet'\ninterval_seconds = 300\n\
             fleet_polling = { monthly_cap_cents = 2_000, driving_poll_seconds = 0x3c }\n",
            "collector.provider = 'fleet'\ncollector.interval_seconds = 300\n\
             collector.'fleet_polling'.monthly_cap_cents = 2000\n\
             \"collector\".fleet_polling.\"driving_poll_seconds\" = 60\n",
        ];
        for (index, spelling) in spellings.iter().enumerate() {
            let figures = folder
                .figures(&format!("spelling-{index}.toml"), spelling)
                .unwrap_or_else(|refusal| panic!("spelling {index}: {refusal}"));
            assert_eq!(figures, plain, "spelling {index}");
        }
        assert_eq!(plain.monthly_cap_cents, 2000);
    }

    #[test]
    fn fleet_figures_refuse_a_configuration_that_is_not_fleet() {
        let folder = Folder::new();
        for (index, collector) in ["", "[collector]\nprovider = 'legacy'\n"].iter().enumerate() {
            assert_eq!(
                code(folder.figures(&format!("legacy-{index}.toml"), collector)),
                FLEET_PROVIDER_NOT_SELECTED
            );
        }
    }

    #[test]
    fn fleet_figures_refuse_an_unreadable_or_invalid_configuration() {
        let folder = Folder::new();
        assert_eq!(
            code(load_fleet_figures(&folder.dir.path().join("missing.toml"))),
            CONFIG_UNREADABLE
        );
        let shared = folder.config("shared.toml", FLEET);
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o666)).expect("mode");
        assert_eq!(code(load_fleet_figures(&shared)), CONFIG_UNREADABLE);
        assert_eq!(code(load_fleet_figures(folder.dir.path())), CONFIG_UNREADABLE);
        assert_eq!(
            code(folder.figures("broken.toml", "[collector\nprovider = 'fleet'\n")),
            CONFIG_INVALID
        );
        assert_eq!(
            code(folder.figures(
                "unknown.toml",
                "[collector]\nprovider = 'fleet'\n[collector.fleet_polling]\nmonthly_cap = 5\n"
            )),
            CONFIG_INVALID
        );
        assert_eq!(
            code(folder.figures(
                "zero.toml",
                "[collector]\nprovider = 'fleet'\n[collector.fleet_polling]\ndriving_poll_seconds = 0\n"
            )),
            CadenceViolation::CODE
        );
    }

    #[test]
    fn fleet_figures_need_no_data_folder() {
        let folder = Folder::new();
        assert!(folder.figures("hub.toml", PLAIN).is_ok());
        assert!(!folder.data().exists(), "nothing was created");
    }

    /// Every figure is the value the collector's scheduler and the monthly cap
    /// are built from: `cadence().fleet_policy()` (scheduler `new_for_mode`,
    /// `RequestLedger::standalone`), whose pricing the request counter prices
    /// data and state requests with, and `interval_seconds`, the supervised
    /// loop's interval.
    #[test]
    fn fleet_figures_equal_the_collector_and_cap_policy() {
        let folder = Folder::new();
        let collectors = [
            FLEET,
            PLAIN,
            "[collector]\nprovider = 'fleet'\ninterval_seconds = 45\n[collector.fleet_polling]\n\
             driving_poll_seconds = 7\ncharging_poll_seconds = 8\nupdating_poll_seconds = 9\n\
             online_poll_seconds = 10\nstate_poll_seconds = 11\ninventory_poll_seconds = 12\n\
             monthly_cap_cents = 0\n",
        ];
        for (index, collector) in collectors.iter().enumerate() {
            let path = folder.config(&format!("policy-{index}.toml"), collector);
            let config = HubConfig::load(&path).expect("configuration");
            let policy = config.collector.cadence().expect("cadence").fleet_policy();
            let figures = load_fleet_figures(&path).expect("figures");
            let seconds = |value: u64| Duration::from_secs(value);
            let polling = figures.polling_seconds;
            assert_eq!(seconds(polling.driving), policy.intervals.driving);
            assert_eq!(seconds(polling.charging), policy.intervals.charging);
            assert_eq!(seconds(polling.updating), policy.intervals.updating);
            assert_eq!(seconds(polling.online), policy.intervals.online);
            assert_eq!(seconds(polling.state_check), policy.intervals.state_check);
            assert_eq!(seconds(polling.inventory), policy.intervals.inventory);
            assert_eq!(figures.monthly_cap_cents, policy.monthly_cap_cents);
            assert_eq!(figures.requests_per_dollar.data, policy.pricing.data);
            assert_eq!(figures.requests_per_dollar.state, policy.pricing.state);
            assert_eq!(policy.pricing, FLEET_PRICING);
            assert_eq!(
                Duration::from_secs(figures.collection_interval_seconds),
                config.collector.supervised_interval().unwrap_or(Duration::ZERO)
            );
        }
    }
}
