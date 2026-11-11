// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ed25519_dalek::SigningKey;
use tempfile::TempDir;

use crate::collection::window::{
    EgressGate, IssueRequest, Issuer, ManualClock, ManualProbe, MemoryAnchor, NoHighWater,
    ResumeOutcome, WindowConfig, WindowController, WindowDeps, grant::TrustDocument,
    refusal::Refusal, supervisor::CollectorSlot,
};

pub const T0: i64 = 1_800_000_000;
pub const HOST: &str = "host-mac13";
pub const ISSUER_HOST: &str = "host-issuer";

pub struct Rig {
    pub tmp: TempDir,
    pub clock: Arc<ManualClock>,
    pub anchor: Arc<MemoryAnchor>,
    pub probe: Arc<ManualProbe>,
    pub issuer: Issuer,
    pub key_path: PathBuf,
    pub trust: TrustDocument,
    counter: Mutex<u32>,
}

impl Rig {
    pub fn new() -> Self {
        let tmp = TempDir::new().expect("temp dir");
        let clock = Arc::new(ManualClock::new(T0));
        let anchor = Arc::new(MemoryAnchor::new());
        let probe = Arc::new(ManualProbe::new(0.2));
        let issuer = Issuer::new(
            &tmp.path().join("issuer"),
            ISSUER_HOST,
            anchor.clone(),
            clock.clone(),
        );
        let key_path = tmp.path().join("issuer.key");
        let trust = issuer.init(&key_path).expect("issuer init");
        Self {
            tmp,
            clock,
            anchor,
            probe,
            issuer,
            key_path,
            trust,
            counter: Mutex::new(0),
        }
    }

    pub fn window_dir(&self) -> PathBuf {
        self.tmp.path().join("data/window")
    }

    pub fn claim_path(&self) -> PathBuf {
        self.tmp.path().join("claim/host-claim.lock")
    }

    pub fn config(&self) -> WindowConfig {
        WindowConfig {
            dir: self.window_dir(),
            claim_path: self.claim_path(),
            host_id: HOST.to_owned(),
        }
    }

    pub fn start_hub(&self) -> (WindowController, ResumeOutcome) {
        self.start_hub_with(Arc::new(NoHighWater))
    }

    pub fn start_hub_with(
        &self,
        handback: Arc<dyn crate::collection::window::HandbackSource>,
    ) -> (WindowController, ResumeOutcome) {
        WindowController::start(
            self.config(),
            WindowDeps {
                clock: self.clock.clone(),
                anchor: self.anchor.clone(),
                probe: self.probe.clone(),
                gate: EgressGate::new(self.clock.clone()),
                handback,
            },
        )
    }

    /// A Hub that trusts the issuer.
    pub fn trusted_hub(&self) -> WindowController {
        let (hub, outcome) = self.start_hub();
        assert_eq!(outcome, ResumeOutcome::Fresh);
        hub.trust(&self.trust).expect("trust");
        hub
    }

    pub fn request(&self, host: &str, start_in: i64, length: i64) -> IssueRequest {
        let now = self.clock.wall_now();
        IssueRequest {
            host_id: host.to_owned(),
            starts_at: now + start_in,
            expires_at: now + start_in + length,
            approver: "G".to_owned(),
            reason: "test window".to_owned(),
            approval_ref: "approval-test".to_owned(),
        }
    }

    pub fn try_grant(&self, request: &IssueRequest) -> Result<Vec<u8>, Refusal> {
        let number = {
            let mut counter = self.counter.lock().expect("counter");
            *counter += 1;
            *counter
        };
        let out = self.tmp.path().join(format!("grant-{number}.json"));
        self.issuer.issue(&self.key_path, request, &out)?;
        Ok(fs::read(out).expect("grant file"))
    }

    pub fn grant(&self, start_in: i64, length: i64) -> Vec<u8> {
        self.try_grant(&self.request(HOST, start_in, length))
            .expect("grant issued")
    }

    pub fn signing_key(&self) -> SigningKey {
        let text = fs::read_to_string(&self.key_path).expect("key file");
        let seed: [u8; 32] = hex::decode(text.trim())
            .expect("hex")
            .try_into()
            .expect("32 bytes");
        SigningKey::from_bytes(&seed)
    }

    pub fn read(&self, name: &str) -> Vec<u8> {
        fs::read(self.window_dir().join(name)).unwrap_or_default()
    }
}

pub trait WallNow {
    fn wall_now(&self) -> i64;
}

impl WallNow for ManualClock {
    fn wall_now(&self) -> i64 {
        crate::collection::window::WindowClock::wall(self)
    }
}

pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create");
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy");
        }
    }
}

#[derive(Debug, Default)]
pub struct SlotLog {
    pub running: bool,
    pub starts: u32,
    pub stops: u32,
    pub released: u32,
    pub fail_starts: u32,
    pub deadline: Option<i64>,
}

#[derive(Clone, Default)]
pub struct FakeSlot {
    pub log: Arc<Mutex<SlotLog>>,
}

impl CollectorSlot for FakeSlot {
    fn start(&mut self, deadline_wall: i64) -> Result<(), String> {
        let mut log = self.log.lock().expect("slot log");
        if log.fail_starts > 0 {
            log.fail_starts -= 1;
            return Err("collector failed".to_owned());
        }
        log.running = true;
        log.starts += 1;
        log.deadline = Some(deadline_wall);
        Ok(())
    }

    fn is_running(&mut self) -> bool {
        self.log.lock().expect("slot log").running
    }

    fn stop(&mut self) {
        let mut log = self.log.lock().expect("slot log");
        if log.running {
            log.stops += 1;
        }
        log.running = false;
    }

    fn release_lease(&mut self) -> Result<(), String> {
        self.log.lock().expect("slot log").released += 1;
        Ok(())
    }
}
