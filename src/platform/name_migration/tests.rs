// SPDX-License-Identifier: AGPL-3.0-only

//! The Mac hand-over with an injected launchd and a temporary home folder:
//! old state present, a second launch, an interruption at each step, and the
//! current and earlier installations both present. No real launchctl,
//! Keychain or home folder is touched.

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

use super::folders::FolderOutcome;
use super::hand_over::{self, Launchd, Layout, NOTE, Note, Status};

const CURRENT_LABEL: &str = "com.tesatlas.hub";
const EARLIER_LABEL: &str = "com.teslatlas.hub"; // rename:keep
const CURRENT_FOLDER: &str = "Tesatlas Hub";
const EARLIER_FOLDER: &str = "Teslatlas Hub"; // rename:keep
const DOMAIN: &str = "gui/501";
const NO_LOCKS: &[&str] = &[];

#[derive(Default)]
struct FakeLaunchd {
    loaded: HashSet<String>,
    disabled: HashSet<String>,
    /// A bootout that never finishes.
    stuck: bool,
    calls: Vec<String>,
}

impl Launchd for FakeLaunchd {
    fn loaded(&mut self, service: &str) -> io::Result<bool> {
        Ok(self.loaded.contains(service))
    }
    fn disabled(&mut self, _domain: &str, label: &str) -> io::Result<bool> {
        Ok(self.disabled.contains(&format!("{DOMAIN}/{label}")))
    }
    fn bootout(&mut self, service: &str) -> io::Result<()> {
        self.calls.push(format!("bootout {service}"));
        if !self.stuck {
            self.loaded.remove(service);
        }
        Ok(())
    }
    fn bootstrap(&mut self, _domain: &str, plist: &Path) -> io::Result<()> {
        let label = plist.file_stem().unwrap().to_str().unwrap();
        self.calls.push(format!("bootstrap {label}"));
        self.loaded.insert(format!("{DOMAIN}/{label}"));
        Ok(())
    }
    fn disable(&mut self, service: &str) -> io::Result<()> {
        self.calls.push(format!("disable {service}"));
        self.disabled.insert(service.to_owned());
        Ok(())
    }
}

struct Home {
    _dir: tempfile::TempDir,
    layout: Layout,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout {
            home: dir.path().to_path_buf(),
            domain: DOMAIN.to_owned(),
            label: CURRENT_LABEL.to_owned(),
            data_folder: CURRENT_FOLDER.to_owned(),
            other_folders: vec![("logs", PathBuf::from("Library/Logs").join(CURRENT_FOLDER), NO_LOCKS)],
        };
        fs::create_dir_all(dir.path().join("Library/LaunchAgents")).unwrap();
        fs::create_dir_all(dir.path().join("Library/Application Support")).unwrap();
        Self { _dir: dir, layout }
    }
    fn support(&self, folder: &str) -> PathBuf {
        self.layout.home.join("Library/Application Support").join(folder)
    }
    fn earlier(&self) -> PathBuf {
        self.support(EARLIER_FOLDER)
    }
    fn current(&self) -> PathBuf {
        self.support(CURRENT_FOLDER)
    }
    fn plist(&self, label: &str) -> PathBuf {
        self.layout.home.join("Library/LaunchAgents").join(format!("{label}.plist"))
    }
    /// An installed earlier Hub: data, a configuration naming it, logs and its agent.
    fn seed_earlier(&self) {
        let earlier = self.earlier();
        fs::create_dir_all(earlier.join("data")).unwrap();
        fs::write(earlier.join("data/hub.sqlite"), "catalogue").unwrap();
        fs::write(earlier.join("data/.hub-instance.lock"), "").unwrap();
        fs::write(earlier.join("config.toml"), format!("data_dir = \"{}/data\"\n", earlier.display())).unwrap();
        fs::create_dir_all(self.layout.home.join("Library/Logs").join(EARLIER_FOLDER)).unwrap();
        fs::write(self.layout.home.join("Library/Logs").join(EARLIER_FOLDER).join("hub.err.log"), "log").unwrap();
        fs::write(self.plist(EARLIER_LABEL), "earlier agent").unwrap();
    }
    /// What the app's own writer does between the two steps.
    fn app_installs_current_agent(&self) {
        fs::write(self.plist(CURRENT_LABEL), "current agent").unwrap();
    }
    fn running_earlier() -> FakeLaunchd {
        let mut launchd = FakeLaunchd::default();
        launchd.loaded.insert(format!("{DOMAIN}/{EARLIER_LABEL}"));
        launchd
    }
}

fn service(label: &str) -> String {
    format!("{DOMAIN}/{label}")
}

fn note_in(folder: &Path) -> Option<Note> {
    serde_json::from_slice(&fs::read(folder.join(NOTE)).ok()?).ok()
}

#[test]
fn a_running_earlier_hub_is_handed_over_whole_and_a_second_launch_does_nothing() {
    let home = Home::new();
    home.seed_earlier();
    let mut launchd = Home::running_earlier();

    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Prepared, "{report:?}");
    assert!(report.install && report.start);
    assert_eq!(report.data, Some(FolderOutcome::Moved));
    assert!(!home.earlier().exists());
    assert_eq!(fs::read_to_string(home.current().join("data/hub.sqlite")).unwrap(), "catalogue");
    assert_eq!(
        fs::read_to_string(home.current().join("config.toml")).unwrap(),
        format!("data_dir = \"{}/data\"\n", home.current().display())
    );
    assert!(home.layout.home.join("Library/Logs").join(CURRENT_FOLDER).join("hub.err.log").exists());
    assert!(!launchd.loaded.contains(&service(EARLIER_LABEL)));
    assert!(launchd.disabled.contains(&service(EARLIER_LABEL)), "a login cannot load it again");
    assert!(!launchd.disabled.contains(&service(CURRENT_LABEL)));
    assert!(home.plist(EARLIER_LABEL).exists(), "kept until the current one is confirmed");

    home.app_installs_current_agent();
    let mut started = false;
    let done = hand_over::complete(&home.layout, report.start, || {
        started = true;
        Ok(())
    });
    assert_eq!(done.status, Status::Done);
    assert!(started);
    assert!(!home.plist(EARLIER_LABEL).exists());
    assert!(note_in(&home.current()).is_none());

    let calls = launchd.calls.len();
    let again = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(again.status, Status::Nothing);
    assert!(!again.install);
    assert_eq!(launchd.calls.len(), calls, "a second launch changes nothing");
    assert_eq!(fs::read_to_string(home.current().join("data/hub.sqlite")).unwrap(), "catalogue");
}

#[test]
fn a_crash_after_the_earlier_service_stopped_still_starts_the_current_one() {
    let home = Home::new();
    home.seed_earlier();
    // The earlier run wrote its note and stopped the service, then died.
    fs::write(home.earlier().join(NOTE), r#"{"serviceWasLoaded":true,"serviceWasDisabled":false}"#).unwrap();
    let mut launchd = FakeLaunchd::default();

    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Prepared);
    assert!(report.install && report.start, "the note remembers the service was running");
    assert_eq!(note_in(&home.current()).map(|note| note.service_was_loaded), Some(true));
}

#[test]
fn a_crash_after_the_move_resumes_from_the_note_in_the_current_folder() {
    let home = Home::new();
    home.seed_earlier();
    let mut launchd = Home::running_earlier();
    assert_eq!(hand_over::prepare(&home.layout, &mut launchd).status, Status::Prepared);
    // The app died before installing the current agent; at the next login
    // launchd loaded nothing (the earlier label is disabled).
    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Prepared);
    assert!(report.install && report.start);
    // The configuration rewrite was also interrupted: it is finished now.
    let config = home.current().join("config.toml");
    fs::write(&config, format!("data_dir = \"{}/data\"\n", home.earlier().display())).unwrap();
    assert_eq!(hand_over::prepare(&home.layout, &mut launchd).status, Status::Prepared);
    assert_eq!(fs::read_to_string(&config).unwrap(), format!("data_dir = \"{}/data\"\n", home.current().display()));
    home.app_installs_current_agent();
    assert_eq!(hand_over::complete(&home.layout, true, || Ok(())).status, Status::Done);
}

#[test]
fn a_service_that_will_not_start_keeps_the_earlier_agent_and_can_be_retried() {
    let home = Home::new();
    home.seed_earlier();
    let mut launchd = Home::running_earlier();
    let report = hand_over::prepare(&home.layout, &mut launchd);
    home.app_installs_current_agent();
    let failed = hand_over::complete(&home.layout, report.start, || Err(io::Error::other("not ready")));
    assert_eq!(failed.status, Status::Failed);
    assert!(home.plist(EARLIER_LABEL).exists());
    assert!(note_in(&home.current()).is_some());
    let retry = hand_over::prepare(&home.layout, &mut launchd);
    assert!(retry.start);
    assert_eq!(hand_over::complete(&home.layout, retry.start, || Ok(())).status, Status::Done);
    assert!(!home.plist(EARLIER_LABEL).exists());
}

#[test]
fn complete_refuses_before_the_current_agent_is_installed() {
    let home = Home::new();
    home.seed_earlier();
    let mut launchd = Home::running_earlier();
    hand_over::prepare(&home.layout, &mut launchd);
    let report = hand_over::complete(&home.layout, true, || panic!("must not start"));
    assert_eq!(report.status, Status::Failed);
    assert!(home.plist(EARLIER_LABEL).exists());
}

#[test]
fn a_busy_earlier_folder_is_deferred_and_the_earlier_service_runs_again() {
    let home = Home::new();
    home.seed_earlier();
    let holder = fs::File::open(home.earlier().join("data/.hub-instance.lock")).unwrap();
    rustix::fs::flock(&holder, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    let mut launchd = Home::running_earlier();

    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Deferred);
    assert!(!report.install);
    assert!(launchd.loaded.contains(&service(EARLIER_LABEL)), "never left stopped");
    assert!(home.earlier().join("data/hub.sqlite").exists());
    assert!(!home.current().exists());
    assert!(note_in(&home.earlier()).is_none());
    assert!(!launchd.disabled.contains(&service(EARLIER_LABEL)));

    drop(holder);
    assert_eq!(hand_over::prepare(&home.layout, &mut launchd).status, Status::Prepared);
}

#[test]
fn an_earlier_service_that_does_not_stop_is_reported_and_nothing_moves() {
    let home = Home::new();
    home.seed_earlier();
    let mut launchd = Home::running_earlier();
    launchd.stuck = true;
    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Failed);
    assert!(home.earlier().join("data/hub.sqlite").exists());
    assert!(!home.current().exists());
    assert!(launchd.loaded.contains(&service(EARLIER_LABEL)));
}

#[test]
fn when_both_installations_exist_the_current_one_wins_and_the_earlier_is_kept_untouched() {
    let home = Home::new();
    home.seed_earlier();
    fs::create_dir_all(home.current().join("data")).unwrap();
    fs::write(home.current().join("data/hub.sqlite"), "current catalogue").unwrap();
    let mut launchd = Home::running_earlier();

    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Prepared);
    assert_eq!(report.data, Some(FolderOutcome::BothKept));
    assert!(!launchd.loaded.contains(&service(EARLIER_LABEL)), "two services never run");
    assert_eq!(fs::read_to_string(home.earlier().join("data/hub.sqlite")).unwrap(), "catalogue");
    assert_eq!(fs::read_to_string(home.current().join("data/hub.sqlite")).unwrap(), "current catalogue");
    assert!(fs::read_to_string(home.earlier().join("config.toml")).unwrap().contains(EARLIER_FOLDER));
    home.app_installs_current_agent();
    assert_eq!(hand_over::complete(&home.layout, report.start, || Ok(())).status, Status::Done);

    // Later launches leave the kept earlier folder alone and never stop the current service.
    launchd.loaded.insert(service(CURRENT_LABEL));
    let calls = launchd.calls.len();
    let later = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(later.status, Status::Nothing);
    assert_eq!(later.data, Some(FolderOutcome::BothKept));
    assert_eq!(launchd.calls.len(), calls);
    assert!(!launchd.disabled.contains(&service(CURRENT_LABEL)));
}

#[test]
fn a_hub_the_owner_stopped_stays_stopped_under_the_current_label() {
    let home = Home::new();
    home.seed_earlier();
    let mut launchd = FakeLaunchd::default();
    launchd.disabled.insert(service(EARLIER_LABEL));
    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Prepared);
    assert!(report.install && !report.start);
    assert!(launchd.disabled.contains(&service(CURRENT_LABEL)));
    home.app_installs_current_agent();
    assert_eq!(hand_over::complete(&home.layout, report.start, || panic!("must not start")).status, Status::Done);
}

#[test]
fn a_hub_without_a_service_moves_its_data_and_installs_nothing() {
    let home = Home::new();
    home.seed_earlier();
    fs::remove_file(home.plist(EARLIER_LABEL)).unwrap();
    let mut launchd = FakeLaunchd::default();
    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Prepared);
    assert!(!report.install && !report.start);
    assert!(home.current().join("data/hub.sqlite").exists());
    assert!(note_in(&home.current()).is_none());
    assert!(launchd.calls.is_empty());
}

#[test]
fn before_the_rename_nothing_is_touched() {
    let home = Home::new();
    home.seed_earlier();
    let mut layout = home.layout.clone();
    layout.label = EARLIER_LABEL.to_owned();
    layout.data_folder = EARLIER_FOLDER.to_owned();
    let mut launchd = Home::running_earlier();
    assert_eq!(hand_over::prepare(&layout, &mut launchd).status, Status::Inert);
    assert_eq!(hand_over::complete(&layout, true, || panic!("must not start")).status, Status::Inert);
    assert!(launchd.calls.is_empty());
    assert!(home.earlier().join("data/hub.sqlite").exists());
}

#[test]
fn the_hand_over_names_match_the_service_constants() {
    assert_eq!(hand_over::LAUNCH_AGENT_LABEL, crate::macos_launch_agent::LABEL);
    assert_eq!(hand_over::LOG_FOLDER, crate::macos_launch_agent::INHERITED_LOG_DIRECTORY);
    let production = Layout::production(PathBuf::from("/Users/owner"), DOMAIN.to_owned());
    assert_eq!(production.data_root(), PathBuf::from("/Users/owner/Library/Application Support").join(hand_over::DATA_FOLDER));
    assert!(production.other_folders.iter().any(|(name, path, _)| *name == "window"
        && crate::collection::window::HostClaim::default_path(Path::new("/Users/owner")).starts_with(Path::new("/Users/owner").join(path))));
}

#[test]
fn print_disabled_output_is_read_exactly() {
    let output = "disabled services = {\n\t\"com.example.other\" => disabled\n\t\"com.tesatlas.hub\" => enabled\n\t\"com.tesatlas.hub.x\" => disabled\n}\n";
    assert!(!hand_over::print_disabled_lists(output, CURRENT_LABEL));
    assert!(hand_over::print_disabled_lists(output, "com.example.other"));
    assert!(hand_over::print_disabled_lists("\t\"com.tesatlas.hub\" => true\n", CURRENT_LABEL));
}
