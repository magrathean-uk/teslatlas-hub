// SPDX-License-Identifier: AGPL-3.0-only

// M02-T10 round 2: the service's operator paths in the binary, which links the
// library without `cfg(test)`, so every credential flag here is the production
// one. Names carry the `collection_window` prefix.

mod operator_window {
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    use tesatlas_hub::{
        CredentialVaultEntry,
        collection::window::{
            CloseReason, CollectorSlot, EgressGate, IssueRequest, Issuer, ManualClock,
            ManualProbe, MemoryAnchor, NoHighWater, Phase, Supervisor, WindowClock, WindowConfig,
            WindowController, WindowDeps,
        },
    };

    use super::*;

    pub(super) const T0: i64 = 1_800_000_000;
    pub(super) const HOST: &str = "host-operator-binary";

    /// A trusted controller on a manual clock with its own gate, and the
    /// issuer that signs its grants.
    pub(super) struct Window {
        pub(super) clock: Arc<ManualClock>,
        pub(super) issuer: Issuer,
        pub(super) key: PathBuf,
        pub(super) controller: Arc<WindowController>,
        pub(super) dir: PathBuf,
    }

    impl Window {
        pub(super) fn new(scratch: &Path, data: &Path) -> Self {
            let clock = Arc::new(ManualClock::new(T0));
            let anchor = Arc::new(MemoryAnchor::new());
            let issuer = Issuer::new(
                &scratch.join("issuer"),
                "host-operator-issuer",
                anchor.clone(),
                clock.clone(),
            );
            let key = scratch.join("issuer.key");
            let trust = issuer.init(&key).expect("issuer init");
            let (controller, _) = WindowController::start(
                WindowConfig {
                    dir: data.join("window"),
                    claim_path: scratch.join("claim/host-claim.lock"),
                    host_id: HOST.to_owned(),
                },
                WindowDeps {
                    clock: clock.clone(),
                    anchor,
                    probe: Arc::new(ManualProbe::new(0.1)),
                    gate: EgressGate::new(clock.clone()),
                    handback: Arc::new(NoHighWater),
                },
            );
            controller.trust(&trust).expect("trusted");
            Self {
                clock,
                issuer,
                key,
                controller: Arc::new(controller),
                dir: scratch.to_owned(),
            }
        }

        pub(super) fn grant(&self, length: i64) -> Vec<u8> {
            let now = self.clock.wall();
            let out = self.dir.join(format!("grant-{now}.json"));
            self.issuer
                .issue(
                    &self.key,
                    &IssueRequest {
                        host_id: HOST.to_owned(),
                        starts_at: now,
                        expires_at: now + length,
                        approver: "G".to_owned(),
                        reason: "binary operator test".to_owned(),
                        approval_ref: "approval-binary-operator".to_owned(),
                    },
                    &out,
                )
                .expect("grant issued");
            fs::read(out).expect("grant bytes")
        }
    }

    /// What a collector run does with the credential vault: it opens the
    /// vault under the service's one admission when it starts and holds it
    /// until it stops.
    #[derive(Clone)]
    struct VaultSlot {
        store: HubStore,
        admission: Arc<AdmittedUserHub>,
        root: PathBuf,
        held: Arc<Mutex<Option<CredentialVaultEntry>>>,
        starts: Arc<Mutex<Vec<Result<(), String>>>>,
    }

    impl CollectorSlot for VaultSlot {
        fn start(&mut self, _deadline_wall: i64) -> Result<(), String> {
            let opened = CredentialVaultEntry::open_for_test(
                self.store.clone(),
                Arc::clone(&self.admission),
                &self.root,
                true,
            );
            let result = opened
                .map(|entry| {
                    *self.held.lock().expect("held") = Some(entry);
                })
                .map_err(|error| error.code().to_owned());
            self.starts.lock().expect("starts").push(result.clone());
            result
        }

        fn is_running(&mut self) -> bool {
            self.held.lock().expect("held").is_some()
        }

        fn stop(&mut self) {
            self.held.lock().expect("held").take();
        }

        fn release_lease(&mut self) -> Result<(), String> {
            if self.is_running() {
                Err("collector_stopping".to_owned())
            } else {
                Ok(())
            }
        }
    }

    /// Finding 1: a second window in the same service lifetime, after a
    /// credential recovery between them, opens the vault again.
    #[test]
    fn collection_window_two_windows_in_one_service_lifetime_both_open_the_vault() {
        let root = root();
        let scratch = tempfile::tempdir().expect("scratch");
        let store = HubStore::initialize(root.path()).expect("store");
        let admission = AdmittedUserHub::admit(root.path()).expect("one service admission");
        let window = Window::new(scratch.path(), root.path());
        let slot = VaultSlot {
            store: store.clone(),
            admission: Arc::clone(&admission),
            root: root.path().to_owned(),
            held: Arc::default(),
            starts: Arc::default(),
        };
        let starts = Arc::clone(&slot.starts);
        let mut supervisor = Supervisor::new(Arc::clone(&window.controller), slot);

        for round in 0..2 {
            window.controller.open(&window.grant(600)).expect("window opens");
            let report = supervisor.step();
            assert!(report.collector_running, "round {round}: {:?}", starts.lock().expect("starts"));
            window.controller.close(CloseReason::OperatorClose);
            assert_eq!(supervisor.step().phase, Phase::Closed, "round {round}");
            // Credential recovery runs between windows on the same admission.
            let recovery = CredentialVaultEntry::open_for_test(
                store.clone(),
                Arc::clone(&admission),
                root.path(),
                true,
            );
            assert!(recovery.is_ok(), "round {round}: recovery opens the vault");
            drop(recovery);
            window.clock.advance(2_000);
        }
        assert_eq!(*starts.lock().expect("starts"), vec![Ok(()), Ok(())]);
        // Two at once are still refused.
        let first = CredentialVaultEntry::open_for_test(store.clone(), Arc::clone(&admission), root.path(), true)
            .unwrap_or_else(|_| panic!("first vault"));
        let second = CredentialVaultEntry::open_for_test(store.clone(), Arc::clone(&admission), root.path(), true);
        assert!(second.is_err(), "a second concurrent vault is refused");
        drop(first);
    }
    /// A slot that refuses to start until the Hub is configured, as an idle
    /// service's does, and counts the refusals.
    #[derive(Clone, Default)]
    struct ConfiguredSlot {
        configured: Arc<std::sync::atomic::AtomicBool>,
        running: Arc<std::sync::atomic::AtomicBool>,
        refusals: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl CollectorSlot for ConfiguredSlot {
        fn start(&mut self, _deadline_wall: i64) -> Result<(), String> {
            use std::sync::atomic::Ordering::SeqCst;
            if !self.configured.load(SeqCst) {
                self.refusals.fetch_add(1, SeqCst);
                return Err("collector_not_ready".to_owned());
            }
            self.running.store(true, SeqCst);
            Ok(())
        }
        fn is_running(&mut self) -> bool {
            self.running.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn stop(&mut self) {
            self.running.store(false, std::sync::atomic::Ordering::SeqCst);
        }
        fn release_lease(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    /// M02-T24 (DEC-141): an idle service in an Open window has had its start
    /// refused, so the supervisor holds a start backoff. The first sign-in's
    /// reload must start the collector at once, not wait out that backoff
    /// (the manual clock never advances, so a held backoff never expires).
    #[test]
    fn collection_window_reload_after_sign_in_starts_the_collector_without_a_restart() {
        use std::sync::atomic::Ordering::SeqCst;
        let root = root();
        let scratch = tempfile::tempdir().expect("scratch");
        let window = Window::new(scratch.path(), root.path());
        window.controller.open(&window.grant(600)).expect("window opens");
        let slot = ConfiguredSlot::default();
        let thread = tesatlas_hub::runtime::operator::authority::SupervisorThread::spawn(
            Arc::clone(&window.controller),
            slot.clone(),
        )
        .expect("supervisor thread");
        let wait_for = |what: &str, done: &dyn Fn() -> bool| {
            let began = std::time::Instant::now();
            while !done() {
                assert!(began.elapsed() < std::time::Duration::from_secs(10), "{what}");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        wait_for("the idle start is refused", &|| slot.refusals.load(SeqCst) >= 1);
        // A kick alone leaves the backoff in place: nothing starts.
        slot.configured.store(true, SeqCst);
        (thread.kicker())();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!slot.running.load(SeqCst), "a plain kick waits out the backoff");
        // The reload clears it.
        (thread.reload_kicker())();
        wait_for("the collector starts after the reload", &|| slot.running.load(SeqCst));
        thread.stop();
    }

    /// M02-T24 review: the reload clears the start backoff but keeps the
    /// pending gap. With the clock advancing, the idle time before the first
    /// sign-in stays recorded as a gap in the handback, so import still
    /// fills it from the source.
    #[test]
    fn collection_window_reload_keeps_the_gap_from_before_the_sign_in() {
        use std::sync::atomic::Ordering::SeqCst;
        let root = root();
        let scratch = tempfile::tempdir().expect("scratch");
        let window = Window::new(scratch.path(), root.path());
        window.controller.open(&window.grant(600)).expect("window opens");
        let slot = ConfiguredSlot::default();
        let mut supervisor = Supervisor::new(Arc::clone(&window.controller), slot.clone());
        assert!(!supervisor.step().collector_running, "the idle start is refused");
        assert_eq!(slot.refusals.load(SeqCst), 1);
        slot.configured.store(true, SeqCst);
        // Inside the backoff (4 s after one refusal) nothing starts.
        window.clock.advance(3);
        assert!(!supervisor.step().collector_running, "the backoff holds");
        window.clock.advance(27);
        supervisor.reset_start_backoff();
        assert!(supervisor.step().collector_running, "the reload starts the collector");
        window.clock.advance(10);
        window.controller.close(CloseReason::OperatorClose);
        assert_eq!(supervisor.step().phase, Phase::Closed);
        let handback = window.controller.status().last_handback.expect("handback");
        assert_eq!(handback.gaps.len(), 1, "{:?}", handback.gaps);
        assert_eq!(handback.gaps[0].start, T0);
        assert_eq!(handback.gaps[0].end, T0 + 30);
        assert_eq!(handback.coverage_seconds, 10);
    }
}
