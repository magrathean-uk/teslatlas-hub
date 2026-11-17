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
use super::hand_over::{self, HOST_CLAIM_LOCK, Launchd, Layout, NOTE, Note, Status};

const CURRENT_LABEL: &str = "com.tesatlas.hub";
const EARLIER_LABEL: &str = "com.teslatlas.hub"; // rename:keep
const CURRENT_FOLDER: &str = "Tesatlas Hub";
const EARLIER_FOLDER: &str = "Teslatlas Hub"; // rename:keep
const DOMAIN: &str = "gui/501";
const NO_LOCKS: &[&str] = &[];
const CLAIM_LOCKS: &[&str] = &[HOST_CLAIM_LOCK];

#[derive(Default)]
struct FakeLaunchd {
    loaded: HashSet<String>,
    disabled: HashSet<String>,
    /// A bootout that never finishes.
    stuck: bool,
    /// A bootstrap that fails (a transient launchd error).
    bootstrap_fails: bool,
    calls: Vec<String>,
    /// Every question asked of launchd, changes or not.
    queries: usize,
}

impl Launchd for FakeLaunchd {
    fn loaded(&mut self, service: &str) -> io::Result<bool> {
        self.queries += 1;
        Ok(self.loaded.contains(service))
    }
    fn disabled(&mut self, _domain: &str, label: &str) -> io::Result<bool> {
        self.queries += 1;
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
        if self.bootstrap_fails {
            return Err(io::Error::other("Bootstrap failed: 5: Input/output error"));
        }
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
            other_folders: vec![
                ("window", PathBuf::from("Library/Application Support").join(format!("{CURRENT_FOLDER} Window")), CLAIM_LOCKS),
                ("logs", PathBuf::from("Library/Logs").join(CURRENT_FOLDER), NO_LOCKS),
            ],
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
    fn window(&self, folder: &str) -> PathBuf {
        self.support(&format!("{folder} Window"))
    }
    /// The earlier window claim, held by a process until the file is dropped.
    fn hold_earlier_claim(&self) -> fs::File {
        let claim = self.window(EARLIER_FOLDER).join(HOST_CLAIM_LOCK);
        let holder = fs::File::open(claim).unwrap();
        rustix::fs::flock(&holder, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        holder
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
        fs::create_dir_all(self.window(EARLIER_FOLDER)).unwrap();
        fs::write(self.window(EARLIER_FOLDER).join(HOST_CLAIM_LOCK), "").unwrap();
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
    assert!(home.window(CURRENT_FOLDER).join(HOST_CLAIM_LOCK).exists() && !home.window(EARLIER_FOLDER).exists());
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

/// REV-CX-M08-T12A finding 2: a restart that fails after a deferral keeps the
/// note, reports the failure, and the next attempt (or launch) still starts
/// the Hub once the data moves.
#[test]
fn a_failed_restart_after_a_deferral_keeps_the_intent_to_run_the_service() {
    let home = Home::new();
    home.seed_earlier();
    let holder = fs::File::open(home.earlier().join("data/.hub-instance.lock")).unwrap();
    rustix::fs::flock(&holder, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    let mut launchd = Home::running_earlier();
    launchd.bootstrap_fails = true;

    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Failed, "{report:?}");
    assert!(report.detail.as_deref().unwrap().contains("could not be started again"), "{report:?}");
    assert!(!launchd.loaded.contains(&service(EARLIER_LABEL)));
    assert_eq!(note_in(&home.earlier()).map(|note| note.service_was_loaded), Some(true), "the intent is kept");

    // Try Again while the folder is still busy: the restart is tried again and now works.
    launchd.bootstrap_fails = false;
    assert_eq!(hand_over::prepare(&home.layout, &mut launchd).status, Status::Deferred);
    assert!(launchd.loaded.contains(&service(EARLIER_LABEL)));
    assert!(note_in(&home.earlier()).is_none(), "the earlier state is back, so the note goes");

    // The owner quit after a failed restart instead; the next launch still starts the Hub.
    launchd.bootstrap_fails = true;
    assert_eq!(hand_over::prepare(&home.layout, &mut launchd).status, Status::Failed);
    drop(holder);
    let mut next_launch = FakeLaunchd::default();
    let report = hand_over::prepare(&home.layout, &mut next_launch);
    assert_eq!(report.status, Status::Prepared, "{report:?}");
    assert!(report.install && report.start, "the earlier service was running when the hand-over began");
    home.app_installs_current_agent();
    let mut started = false;
    let done = hand_over::complete(&home.layout, report.start, || {
        started = true;
        Ok(())
    });
    assert_eq!(done.status, Status::Done);
    assert!(started);
}

/// REV-CX-M08-T12A finding 3: a current installation keeps its own service
/// state when an earlier one is found beside it, stopped or running.
#[test]
fn a_current_installation_keeps_its_own_service_state() {
    for (earlier_running, current_running) in [(false, true), (true, false)] {
        let home = Home::new();
        home.seed_earlier();
        fs::create_dir_all(home.current().join("data")).unwrap();
        fs::write(home.current().join("data/hub.sqlite"), "current catalogue").unwrap();
        home.app_installs_current_agent();
        let mut launchd = FakeLaunchd::default();
        if earlier_running {
            launchd.loaded.insert(service(EARLIER_LABEL));
        } else {
            launchd.disabled.insert(service(EARLIER_LABEL));
        }
        if current_running {
            launchd.loaded.insert(service(CURRENT_LABEL));
        } else {
            launchd.disabled.insert(service(CURRENT_LABEL));
        }

        let report = hand_over::prepare(&home.layout, &mut launchd);
        assert_eq!(report.status, Status::Prepared, "{report:?}");
        assert_eq!(report.data, Some(FolderOutcome::BothKept));
        assert!(report.install && !report.start, "nothing is started for the current Hub");
        assert_eq!(note_in(&home.current()).map(|note| note.current_was_installed), Some(true));
        assert!(!launchd.loaded.contains(&service(EARLIER_LABEL)), "two services never run");
        assert!(launchd.disabled.contains(&service(EARLIER_LABEL)));
        assert!(!launchd.calls.contains(&format!("disable {}", service(CURRENT_LABEL))));
        // Interrupted before the app finished: the next run still keeps the current state.
        assert!(!hand_over::prepare(&home.layout, &mut launchd).start);
        assert_eq!(hand_over::complete(&home.layout, false, || panic!("must not start")).status, Status::Done);
        assert_eq!(launchd.loaded.contains(&service(CURRENT_LABEL)), current_running);
        assert_eq!(launchd.disabled.contains(&service(CURRENT_LABEL)), !current_running);
        assert!(!home.plist(EARLIER_LABEL).exists());
        assert_eq!(fs::read_to_string(home.current().join("data/hub.sqlite")).unwrap(), "current catalogue");
    }
}

/// REV-CX-M08-T12A finding 4: a window claim still held under the earlier
/// name defers the whole hand-over before anything moves or starts.
#[test]
fn a_window_claim_held_under_the_earlier_name_defers_everything() {
    let home = Home::new();
    home.seed_earlier();
    let holder = home.hold_earlier_claim();
    let mut launchd = Home::running_earlier();

    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Deferred, "{report:?}");
    assert!(!report.install && !report.start);
    assert!(report.detail.as_deref().unwrap().contains("window claim"), "{report:?}");
    assert!(home.earlier().join("data/hub.sqlite").exists() && !home.current().exists());
    assert!(home.window(EARLIER_FOLDER).join(HOST_CLAIM_LOCK).exists() && !home.window(CURRENT_FOLDER).exists());
    assert!(launchd.loaded.contains(&service(EARLIER_LABEL)), "the earlier service runs again");

    // Both claim folders exist (the current one left by an earlier run): the
    // held earlier claim still defers.
    fs::create_dir_all(home.window(CURRENT_FOLDER)).unwrap();
    fs::write(home.window(CURRENT_FOLDER).join(HOST_CLAIM_LOCK), "").unwrap();
    assert_eq!(hand_over::prepare(&home.layout, &mut launchd).status, Status::Deferred);

    drop(holder);
    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Prepared, "{report:?}");
    assert!(report.folders.contains(&("window".to_owned(), "both_kept".to_owned())), "{report:?}");
    assert!(report.start);
}

/// A claim taken under the earlier name after the data moved (the app died in
/// between): nothing starts until it is free, and the note is kept.
#[test]
fn a_window_claim_taken_after_the_data_moved_holds_the_start_back() {
    let home = Home::new();
    home.seed_earlier();
    let mut launchd = Home::running_earlier();
    assert_eq!(hand_over::prepare(&home.layout, &mut launchd).status, Status::Prepared);
    fs::create_dir_all(home.window(EARLIER_FOLDER)).unwrap();
    fs::write(home.window(EARLIER_FOLDER).join(HOST_CLAIM_LOCK), "").unwrap();
    let holder = home.hold_earlier_claim();
    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Deferred);
    assert!(!report.install && !report.start);
    assert!(note_in(&home.current()).is_some());
    drop(holder);
    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert!(report.install && report.start, "{report:?}");
}

/// REV2-CX-M08-T12A finding 4: a running current Hub holds its claim under
/// both spellings (it recreates the earlier claim folder to do so). An app
/// start must not take that for an earlier Hub and defer; an earlier Hub
/// holding only the earlier claim still defers.
#[test]
fn a_current_hub_holding_both_claims_does_not_defer_the_app_start() {
    let _renamed = super::spelling::assume_renamed();
    let home = Home::new();
    home.seed_earlier();
    let mut launchd = Home::running_earlier();
    assert_eq!(hand_over::prepare(&home.layout, &mut launchd).status, Status::Prepared);
    home.app_installs_current_agent();
    assert_eq!(hand_over::complete(&home.layout, true, || Ok(())).status, Status::Done);
    assert!(!home.window(EARLIER_FOLDER).exists());

    let claim = crate::collection::window::HostClaim::acquire(&home.window(CURRENT_FOLDER).join(HOST_CLAIM_LOCK)).unwrap();
    assert!(home.window(EARLIER_FOLDER).join(HOST_CLAIM_LOCK).exists());
    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Nothing, "{report:?}");
    assert!(report.folders.contains(&("window".to_owned(), "both_kept".to_owned())), "{report:?}");
    drop(claim);

    // Only the earlier claim held (an old binary run by hand): still deferred.
    let holder = home.hold_earlier_claim();
    assert_eq!(hand_over::prepare(&home.layout, &mut launchd).status, Status::Deferred);
    drop(holder);
    assert_eq!(hand_over::prepare(&home.layout, &mut launchd).status, Status::Nothing);
}

/// REV-CX-M08-T12A finding 8: with this build's own names (before the swap)
/// an installed Hub is not touched: no folder, configuration or launchd step.
#[test]
fn the_current_build_takes_no_migration_step() {
    if super::spelling::renamed() {
        return; // after the swap the hand-over tests above apply
    }
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::production(dir.path().to_path_buf(), DOMAIN.to_owned());
    let root = layout.data_root();
    fs::create_dir_all(root.join("data")).unwrap();
    fs::write(root.join("data/hub.sqlite"), "catalogue").unwrap();
    let config = format!("data_dir = \"{}/data\"\n", root.display());
    fs::write(root.join("config.toml"), &config).unwrap();
    let agents = dir.path().join("Library/LaunchAgents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(agents.join(format!("{}.plist", hand_over::LAUNCH_AGENT_LABEL)), "agent").unwrap();
    let mut launchd = FakeLaunchd::default();
    launchd.loaded.insert(format!("{DOMAIN}/{}", hand_over::LAUNCH_AGENT_LABEL));
    let before = listing(dir.path());

    assert_eq!(hand_over::prepare(&layout, &mut launchd).status, Status::Inert);
    assert_eq!(hand_over::complete(&layout, true, || panic!("must not start")).status, Status::Inert);
    assert_eq!(
        super::config_paths::prepare_config_path(&root.join("config.toml")).unwrap(),
        super::config_paths::ConfigOutcome::Current
    );
    assert_eq!((launchd.queries, launchd.calls.len()), (0, 0), "launchd is not even asked");
    assert_eq!(listing(dir.path()), before);
    assert_eq!(fs::read_to_string(root.join("config.toml")).unwrap(), config);
}

fn listing(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            paths.push(path);
        }
    }
    paths.sort();
    paths
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

// The start confirmation (M08-T16). `serving-check` is the Hub binary's own
// verb (pairing and the history import use it); these stand-ins answer as it
// does for each case, so the hand-over's judgement and fences are exercised
// without launchctl or a running Hub.

/// A stand-in Hub binary whose `serving-check` runs `answer` (a shell fragment
/// that sees `$config` and `$wait`); anything else exits 2.
fn fake_hub(home: &Home, answer: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let folder = home.layout.home.join("bin");
    fs::create_dir_all(&folder).unwrap();
    let program = folder.join("tesatlas-hub");
    fs::write(
        &program,
        format!(
            "#!/bin/sh\n[ \"$#\" = 5 ] && [ \"$1\" = --config ] && [ \"$3\" = serving-check ] && [ \"$4\" = --wait-seconds ] || exit 2\nconfig=\"$2\"; wait=\"$5\"\n[ -f \"$config\" ] || exit 2\n{answer}\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    program
}

const SERVING_BETWEEN_WINDOWS: &str = r#"echo '{"status":"serving","readiness":"between_windows"}'"#;

/// A running earlier Hub moved under the current name, its agent written by the
/// app: what `migrate-names --complete --start` finds.
fn prepared_for_start(home: &Home) -> (FakeLaunchd, PathBuf) {
    home.seed_earlier();
    let mut launchd = Home::running_earlier();
    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert!(report.install && report.start, "{report:?}");
    home.app_installs_current_agent();
    (launchd, home.current().join("config.toml"))
}

#[test]
fn handover_a_hub_with_a_car_between_windows_counts_as_started() {
    let home = Home::new();
    let (_launchd, config) = prepared_for_start(&home);
    // A configured car with no open window: `/readyz` is 503 `collector_absent`,
    // which `serving-check` reports as serving `between_windows`.
    let hub = fake_hub(&home, SERVING_BETWEEN_WINDOWS);
    let done = hand_over::complete(&home.layout, true, || super::cli::confirm_serving(&hub, &config));
    assert_eq!(done.status, Status::Done, "{done:?}");
    assert!(!home.plist(EARLIER_LABEL).exists(), "removed once the start is confirmed");
    assert!(note_in(&home.current()).is_none());
    assert_eq!(fs::read_to_string(home.current().join("data/hub.sqlite")).unwrap(), "catalogue");
}

#[test]
fn handover_inside_an_open_window_waits_out_the_collector_restart() {
    let home = Home::new();
    let (_launchd, config) = prepared_for_start(&home);
    // Inside a window the start's restart leaves the collector without its
    // lease for about 11 s (lab); until it takes the lease again the answer can
    // be a refusal. The check serves only when its wait covers the restart, and
    // the hand-over waits for the check to finish.
    let hub = fake_hub(
        &home,
        r#"sleep 1
if [ "$wait" -ge 11 ]; then echo '{"status":"serving","readiness":"ready"}'
else echo 'tesatlas-hub: the Hub service is running but cannot serve its history (collector_stale)' >&2; exit 1; fi"#,
    );
    let done = hand_over::complete(&home.layout, true, || super::cli::confirm_serving(&hub, &config));
    assert_eq!(done.status, Status::Done, "{done:?}");
    assert!(!home.plist(EARLIER_LABEL).exists());
}

#[test]
fn handover_a_replacement_with_a_real_fault_is_refused_with_its_reason() {
    for reason in [
        "the Hub service is running but cannot serve its history (catalogue_unavailable)",
        "the Hub service is running but cannot serve its history (lifecycle_quarantined)",
        "the Hub service is not serving its configuration on its listener",
    ] {
        let home = Home::new();
        let (_launchd, config) = prepared_for_start(&home);
        let hub = fake_hub(&home, &format!("echo 'tesatlas-hub: {reason}' >&2; exit 1"));
        let failed = hand_over::complete(&home.layout, true, || super::cli::confirm_serving(&hub, &config));
        assert_eq!(failed.status, Status::Failed);
        assert_eq!(failed.detail.as_deref(), Some(format!("the Hub service did not start under its current name: {reason}").as_str()));
        assert!(home.plist(EARLIER_LABEL).exists(), "kept: the start is not confirmed");
        assert!(note_in(&home.current()).is_some(), "the next attempt resumes");
    }
}

#[test]
fn handover_left_failed_by_the_strict_wait_completes_at_the_next_start_without_moving_data_again() {
    let home = Home::new();
    let (mut launchd, config) = prepared_for_start(&home);
    // The 1ac8196a build: moved and running under the current label, but its
    // start check refused the between-windows answer.
    let failed = hand_over::complete(&home.layout, true, || {
        Err(io::Error::new(io::ErrorKind::TimedOut, "replacement Hub service did not become stably ready"))
    });
    assert_eq!(failed.status, Status::Failed);
    assert!(home.plist(EARLIER_LABEL).exists() && note_in(&home.current()).is_some());
    launchd.loaded.insert(service(CURRENT_LABEL));
    let calls = launchd.calls.clone();

    // The fixed build's next start.
    let report = hand_over::prepare(&home.layout, &mut launchd);
    assert_eq!(report.status, Status::Prepared, "{report:?}");
    assert_eq!(report.data, Some(FolderOutcome::AlreadyMoved), "no data moved twice");
    assert!(report.install && report.start);
    let new_calls = &launchd.calls[calls.len()..];
    assert!(
        new_calls.iter().all(|call| call == &format!("disable {}", service(EARLIER_LABEL))),
        "the earlier service is neither stopped nor started again: {new_calls:?}"
    );
    assert!(!home.earlier().exists());
    home.app_installs_current_agent();
    let hub = fake_hub(&home, SERVING_BETWEEN_WINDOWS);
    let done = hand_over::complete(&home.layout, report.start, || super::cli::confirm_serving(&hub, &config));
    assert_eq!(done.status, Status::Done, "{done:?}");
    assert!(!home.plist(EARLIER_LABEL).exists());
    assert!(note_in(&home.current()).is_none());
    assert_eq!(fs::read_to_string(home.current().join("data/hub.sqlite")).unwrap(), "catalogue");
    assert_eq!(hand_over::prepare(&home.layout, &mut launchd).status, Status::Nothing, "and stays done");
}
