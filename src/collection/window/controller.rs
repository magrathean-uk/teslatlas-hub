// SPDX-License-Identifier: AGPL-3.0-only

//! The Hub-side window controller: trust, open, tick, close, resume, handback
//! and import admission (design 2.3 to 2.10, rulings R1 to R4).
//!
//! The controller is synchronous and clock-injected. A supervisor (see
//! `supervisor.rs`) calls `tick` and `finish_close`; the egress gate is opened
//! and closed only from here. There is no extend operation: a window ends at
//! its original hard expiry, restart never moves it, and a longer window needs
//! a new grant with a new sequence number.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

use super::{
    BOOT_TIME_TOLERANCE_SECONDS, CLOCK_FLOOR_EPOCH_SECONDS, DRIFT_TOLERANCE_SECONDS,
    NTP_MAX_OFFSET_SECONDS, NTP_RECHECK_SECONDS, SUPERVISOR_POLL_SECONDS,
    anchor::AnchorStore,
    claim::{FileLock, HostClaim, LockMode},
    clock::{ClockGuard, ClockVerdict, OffsetProbe, WindowClock, deadline_mono_for},
    gate::EgressGate,
    grant::{SignedGrant, TrustDocument},
    ledger::Line,
    refusal::Refusal,
    state::{
        CloseReason, Gap, Handback, HighWater, HubEntry, Loaded, Phase, TrustRecord, WindowState,
        WindowStore,
    },
};

/// Per-table Hub high-water marks for the handback record. The store-backed
/// implementation belongs to the service wiring (T10); a source that cannot
/// say returns `HighWater::default()`.
pub trait HandbackSource: Send + Sync {
    fn high_water(&self) -> HighWater;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoHighWater;

impl HandbackSource for NoHighWater {
    fn high_water(&self) -> HighWater {
        HighWater::default()
    }
}

#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// `<data dir>/window`.
    pub dir: PathBuf,
    /// The fixed per-user claim path outside the data directory.
    pub claim_path: PathBuf,
    /// This host's id (T02's host id function; tests inject it).
    pub host_id: String,
}

pub struct WindowDeps {
    pub clock: Arc<dyn WindowClock>,
    pub anchor: Arc<dyn AnchorStore>,
    pub probe: Arc<dyn OffsetProbe>,
    pub gate: EgressGate,
    pub handback: Arc<dyn HandbackSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeOutcome {
    /// No issuer is trusted yet.
    Fresh,
    Closed,
    /// The window carried on to its original deadline.
    Resumed(Phase),
    /// The window was closed at start; the handback is written.
    ClosedAtStart(CloseReason),
    /// Trust exists but state or ledger cannot be verified; opens are refused
    /// until `window trust` is rerun with a newer epoch.
    Faulted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowStatus {
    pub trusted: bool,
    pub faulted: bool,
    pub phase: Phase,
    pub close_reason: Option<CloseReason>,
    pub seq: Option<u64>,
    pub grant_digest: Option<String>,
    pub starts_at: Option<i64>,
    pub deadline: Option<i64>,
    pub opened_at: Option<i64>,
    pub closing_at: Option<i64>,
    pub last_handback: Option<Handback>,
}

/// Held for the whole of an import; it blocks `window open` (`import_running`).
#[derive(Debug)]
pub struct ImportGuard {
    _lock: FileLock,
}

struct Inner {
    state: WindowState,
    trust: Option<TrustRecord>,
    guard: Option<ClockGuard>,
    claim: Option<HostClaim>,
    last_ntp_mono: Option<u64>,
    fault: bool,
    last_handback: Option<Handback>,
}

pub struct WindowController {
    host_id: String,
    claim_path: PathBuf,
    store: WindowStore,
    clock: Arc<dyn WindowClock>,
    anchor: Arc<dyn AnchorStore>,
    probe: Arc<dyn OffsetProbe>,
    gate: EgressGate,
    handback: Arc<dyn HandbackSource>,
    inner: Mutex<Inner>,
}

fn parse_anchor(value: &str) -> Option<(u64, u64)> {
    let (epoch, seq) = value.split_once(':')?;
    Some((epoch.parse().ok()?, seq.parse().ok()?))
}

fn ledger_synced(ledger: &[Line<HubEntry>]) -> bool {
    matches!(
        ledger.last().map(|line| &line.entry),
        None | Some(HubEntry::Trusted { .. } | HubEntry::Closed { .. })
    )
}

fn last_handback(ledger: &[Line<HubEntry>]) -> Option<Handback> {
    ledger.iter().rev().find_map(|line| match &line.entry {
        HubEntry::Closed { handback } => Some(handback.clone()),
        _ => None,
    })
}

impl WindowController {
    /// Builds the controller and resumes whatever the data directory holds
    /// (design 2.9). It never fails: an unverifiable store is reported as
    /// `Faulted` and refuses every open.
    pub fn start(config: WindowConfig, deps: WindowDeps) -> (Self, ResumeOutcome) {
        let controller = Self {
            host_id: config.host_id,
            claim_path: config.claim_path,
            store: WindowStore::new(&config.dir),
            clock: deps.clock,
            anchor: deps.anchor,
            probe: deps.probe,
            gate: deps.gate,
            handback: deps.handback,
            inner: Mutex::new(Inner {
                state: WindowState::closed(),
                trust: None,
                guard: None,
                claim: None,
                last_ntp_mono: None,
                fault: false,
                last_handback: None,
            }),
        };
        let outcome = {
            let mut inner = controller.lock();
            controller.resume_locked(&mut inner)
        };
        (controller, outcome)
    }

    pub fn gate(&self) -> &EgressGate {
        &self.gate
    }

    pub fn clock(&self) -> &Arc<dyn WindowClock> {
        &self.clock
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("window controller lock")
    }

    fn anchor_name(&self) -> String {
        format!("window-consumption-{}", self.host_id)
    }

    // ---- status ----------------------------------------------------------

    pub fn status(&self) -> WindowStatus {
        let inner = self.lock();
        let state = &inner.state;
        WindowStatus {
            trusted: inner.trust.is_some(),
            faulted: inner.fault,
            phase: state.phase,
            close_reason: state.close_reason,
            seq: state.grant.as_ref().map(|grant| grant.body.seq),
            grant_digest: state.grant_digest.clone(),
            starts_at: state.grant.as_ref().map(|grant| grant.body.starts_at),
            deadline: state.grant.as_ref().map(|grant| grant.body.expires_at),
            opened_at: state.opened_at,
            closing_at: state.closing_at,
            last_handback: inner.last_handback.clone(),
        }
    }

    /// Seconds the supervisor may sleep before it must look again.
    pub fn next_wakeup_seconds(&self) -> i64 {
        let status = self.status();
        let now = self.clock.wall();
        let mut wake = SUPERVISOR_POLL_SECONDS;
        for target in [
            status.deadline,
            status.starts_at.filter(|_| status.phase == Phase::Pending),
        ]
        .into_iter()
        .flatten()
        {
            wake = wake.min((target - now).max(1));
        }
        wake
    }

    // ---- trust -----------------------------------------------------------

    /// Stores the issuer's public key and trust epoch and starts a fresh Hub
    /// ledger. The epoch must be newer than the one recorded outside the data
    /// directory, so rerunning it kills every earlier grant and no restore
    /// can revive one. Refused unless Closed and no import is running.
    pub fn trust(&self, document: &TrustDocument) -> Result<(), Refusal> {
        document.verifying_key()?;
        let mut inner = self.lock();
        if !inner.fault && inner.state.phase != Phase::Closed {
            return Err(Refusal::WindowActive);
        }
        self.store.ensure_dir()?;
        let _lock = FileLock::acquire(&self.store.lock_path(), LockMode::Exclusive, false)?
            .ok_or(Refusal::ImportRunning)?;
        let name = self.anchor_name();
        let previous = match self.anchor.read(&name)? {
            Some(value) => Some(parse_anchor(&value).ok_or(Refusal::AnchorMismatch)?),
            None => None,
        };
        if previous.is_some_and(|(epoch, _)| document.trust_epoch <= epoch) {
            return Err(Refusal::TrustEpochNotNewer);
        }
        let record = TrustRecord {
            document: document.clone(),
            trusted_at: self.clock.wall(),
        };
        self.store.reset(&record)?;
        let seq = previous.map_or(0, |(_, seq)| seq);
        self.anchor
            .write(&name, &format!("{}:{seq}", document.trust_epoch))?;
        *inner = Inner {
            state: WindowState::closed(),
            trust: Some(record),
            guard: None,
            claim: None,
            last_ntp_mono: None,
            fault: false,
            last_handback: None,
        };
        self.gate.close();
        Ok(())
    }

    // ---- open ------------------------------------------------------------

    fn check_offset(&self) -> Result<(), Refusal> {
        match self.probe.offset_seconds() {
            Ok(offset) if offset.is_finite() && offset.abs() <= NTP_MAX_OFFSET_SECONDS => Ok(()),
            _ => Err(Refusal::ClockOffset),
        }
    }

    /// Opens a window from a signed grant file's bytes. Every refusal is a
    /// stable code with no state change and no network access beyond the
    /// time probe.
    pub fn open(&self, grant_bytes: &[u8]) -> Result<WindowStatus, Refusal> {
        let mut inner = self.lock();
        if inner.fault {
            return Err(Refusal::StateUnverifiable);
        }
        let trust = inner.trust.clone().ok_or(Refusal::TrustMissing)?;
        let grant = SignedGrant::parse(grant_bytes)?;
        let body = &grant.body;
        if body.issuer_key_id != trust.document.issuer_key_id {
            return Err(Refusal::KeyIdMismatch);
        }
        grant.verify(&trust.document.verifying_key()?)?;
        body.check_shape()?;
        if body.host_id != self.host_id {
            return Err(Refusal::HostMismatch);
        }
        let wall = self.clock.wall();
        let mono = self.clock.mono();
        if wall < CLOCK_FLOOR_EPOCH_SECONDS {
            return Err(Refusal::ClockInvalid);
        }
        if body.expires_at <= wall {
            return Err(Refusal::GrantExpired);
        }
        if body.trust_epoch != trust.document.trust_epoch {
            return Err(Refusal::TrustEpochMismatch);
        }
        let anchor_value = self
            .anchor
            .read(&self.anchor_name())?
            .ok_or(Refusal::AnchorMissing)?;
        let (anchor_epoch, anchor_seq) =
            parse_anchor(&anchor_value).ok_or(Refusal::AnchorMismatch)?;
        if anchor_epoch != trust.document.trust_epoch {
            return Err(Refusal::AnchorMismatch);
        }
        if body.seq <= anchor_seq {
            return Err(Refusal::GrantConsumed);
        }
        let loaded: Loaded = self.store.load()?;
        let ledger_max = loaded
            .ledger
            .iter()
            .filter_map(|line| match &line.entry {
                HubEntry::Consumed { seq, .. } => Some(*seq),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        if body.seq <= ledger_max {
            return Err(Refusal::GrantConsumed);
        }
        if inner.state.phase != Phase::Closed {
            return Err(Refusal::WindowActive);
        }
        if !ledger_synced(&loaded.ledger) {
            return Err(Refusal::HandbackPending);
        }
        let _import_lock = FileLock::acquire(&self.store.lock_path(), LockMode::Exclusive, false)?
            .ok_or(Refusal::ImportRunning)?;
        let claim = HostClaim::acquire(&self.claim_path)?;
        let boot_time = self.clock.boot_time()?;
        self.check_offset()?;

        // Commit. The consumed line is synced before any state change, and the
        // anchor outside the data directory follows it at once.
        let digest = body.digest()?;
        self.store.append(HubEntry::Consumed {
            seq: body.seq,
            grant_digest: digest.clone(),
            host_id: body.host_id.clone(),
            starts_at: body.starts_at,
            expires_at: body.expires_at,
            at: wall,
        })?;
        if let Err(refusal) = self.anchor.write(
            &self.anchor_name(),
            &format!("{}:{}", anchor_epoch, body.seq),
        ) {
            self.seal_orphan(body.seq, &digest, None, wall);
            return Err(refusal);
        }
        let guard = ClockGuard::anchored(wall, mono, body.expires_at, wall);
        inner.state = WindowState {
            phase: Phase::Pending,
            grant: Some(grant.clone()),
            grant_digest: Some(digest.clone()),
            boot_time,
            anchor_wall: wall,
            anchor_mono: mono,
            deadline_mono: guard.deadline_mono,
            watermark: wall,
            ..WindowState::closed()
        };
        inner.guard = Some(guard);
        inner.claim = Some(claim);
        inner.last_ntp_mono = Some(mono);
        if let Err(refusal) = self.store.write_state(&inner.state) {
            self.abandon(&mut inner, body.seq, &digest, wall);
            return Err(refusal);
        }
        if wall >= body.starts_at
            && let Err(refusal) = self.enter_open(&mut inner, wall, mono)
        {
            self.abandon(&mut inner, body.seq, &digest, wall);
            return Err(refusal);
        }
        drop(inner);
        Ok(self.status())
    }

    /// Pending to Open: `opened` is appended and synced before state moves.
    fn enter_open(&self, inner: &mut Inner, wall: i64, mono: u64) -> Result<(), Refusal> {
        let grant = inner
            .state
            .grant
            .as_ref()
            .map(|grant| grant.body.clone())
            .ok_or(Refusal::StateUnverifiable)?;
        self.store.append(HubEntry::Opened {
            seq: grant.seq,
            at: wall,
            mono,
        })?;
        inner.state.phase = Phase::Open;
        inner.state.opened_at = Some(wall);
        inner.state.watermark = inner.state.watermark.max(wall);
        let guard = ClockGuard::anchored(wall, mono, grant.expires_at, inner.state.watermark);
        inner.state.anchor_wall = wall;
        inner.state.anchor_mono = mono;
        inner.state.deadline_mono = guard.deadline_mono;
        inner.guard = Some(guard);
        self.store.write_state(&inner.state)?;
        self.gate.open(grant.expires_at, guard.deadline_mono);
        Ok(())
    }

    /// A failure after the grant was consumed: it stays dead, the window never
    /// ran, and a handback keeps the ledger consistent.
    fn abandon(&self, inner: &mut Inner, seq: u64, digest: &str, wall: i64) {
        self.gate.close();
        inner.claim = None;
        inner.guard = None;
        inner.state = WindowState::closed();
        self.seal_orphan(seq, digest, None, wall);
        let _ = self.store.write_state(&inner.state);
    }

    fn seal_orphan(&self, seq: u64, digest: &str, opened_at: Option<i64>, wall: i64) {
        let handback = Handback {
            seq,
            grant_digest: digest.to_owned(),
            opened_at,
            closed_at: wall,
            reason: CloseReason::StateUnverifiable,
            coverage_seconds: 0,
            gaps: Vec::new(),
            high_water: self.handback.high_water(),
        };
        let _ = self.store.append(HubEntry::Closed { handback });
    }

    // ---- tick ------------------------------------------------------------

    /// Rechecks clock, time offset and deadline; moves Pending to Open at its
    /// start and Pending or Open to Closing at expiry or a bad clock. The
    /// gate checks the deadline itself, so a missed tick cannot extend a window.
    pub fn tick(&self) -> Result<Phase, Refusal> {
        let mut inner = self.lock();
        if inner.fault {
            return Err(Refusal::StateUnverifiable);
        }
        let phase = inner.state.phase;
        self.gate.sweep();
        if !matches!(phase, Phase::Pending | Phase::Open) {
            return Ok(phase);
        }
        let wall = self.clock.wall();
        let mono = self.clock.mono();
        // Expiry the gate has seen is final, whatever the clock reads now.
        if phase == Phase::Open
            && let Some(observed) = self.gate.expiry_observed()
        {
            self.begin_close(&mut inner, CloseReason::Expired, observed);
            return Ok(inner.state.phase);
        }
        let Some(mut guard) = inner.guard else {
            self.begin_close(&mut inner, CloseReason::StateUnverifiable, wall);
            return Ok(inner.state.phase);
        };
        match guard.check(wall, mono) {
            ClockVerdict::Invalid => {
                self.begin_close(&mut inner, CloseReason::ClockInvalid, wall);
                return Ok(inner.state.phase);
            }
            ClockVerdict::Expired => {
                self.begin_close(&mut inner, CloseReason::Expired, wall);
                return Ok(inner.state.phase);
            }
            ClockVerdict::Ok => {}
        }
        inner.guard = Some(guard);
        let due = inner
            .last_ntp_mono
            .is_none_or(|last| mono.saturating_sub(last) >= NTP_RECHECK_SECONDS);
        if due {
            inner.last_ntp_mono = Some(mono);
            if self.check_offset().is_err() {
                self.begin_close(&mut inner, CloseReason::ClockInvalid, wall);
                return Ok(inner.state.phase);
            }
        }
        inner.state.watermark = guard.watermark;
        let starts_at = inner
            .state
            .grant
            .as_ref()
            .map_or(i64::MAX, |grant| grant.body.starts_at);
        if phase == Phase::Pending && wall >= starts_at {
            if self.enter_open(&mut inner, wall, mono).is_err() {
                self.begin_close(&mut inner, CloseReason::StateUnverifiable, wall);
            }
        } else if self.store.write_state(&inner.state).is_err() {
            self.begin_close(&mut inner, CloseReason::StateUnverifiable, wall);
        }
        Ok(inner.state.phase)
    }

    // ---- close -----------------------------------------------------------

    /// Pending or Open to Closing. The gate stops issuing permits and cancels
    /// requests and streams at once; a token exchange permitted earlier may
    /// finish within its grace.
    fn begin_close(&self, inner: &mut Inner, reason: CloseReason, wall: i64) {
        if !matches!(inner.state.phase, Phase::Pending | Phase::Open) {
            return;
        }
        inner.state.phase = Phase::Closing;
        inner.state.close_reason = Some(reason);
        inner.state.closing_at = Some(wall);
        self.gate.close();
        // Fail closed: the gate is shut even if the write fails; the restart
        // path completes the close.
        let _ = self.store.write_state(&inner.state);
    }

    /// Operator or service close (`operator_close`, `relogin_needed`,
    /// `owner_api_refused`). A no-op unless Pending or Open.
    pub fn close(&self, reason: CloseReason) {
        let mut inner = self.lock();
        let wall = self.clock.wall();
        self.begin_close(&mut inner, reason, wall);
    }

    /// Records an interval in which the collector was not running.
    pub fn record_gap(&self, start: i64, end: i64) -> Result<(), Refusal> {
        let mut inner = self.lock();
        if end > start && matches!(inner.state.phase, Phase::Open | Phase::Closing) {
            inner.state.gaps.push(Gap { start, end });
            self.store.write_state(&inner.state)?;
        }
        Ok(())
    }

    /// Completes a close: writes and syncs the handback, releases the host
    /// claim and returns to Closed. Idempotent. The caller (the supervisor)
    /// first waits for an in-flight exchange and releases the collector lease.
    pub fn finish_close(&self) -> Result<Option<Handback>, Refusal> {
        let mut inner = self.lock();
        if inner.fault {
            return Err(Refusal::StateUnverifiable);
        }
        self.finish_close_locked(&mut inner)
    }

    fn finish_close_locked(&self, inner: &mut Inner) -> Result<Option<Handback>, Refusal> {
        match inner.state.phase {
            Phase::Closed => return Ok(None),
            Phase::Pending | Phase::Open => return Err(Refusal::WindowActive),
            Phase::Closing => {}
        }
        let grant = inner
            .state
            .grant
            .as_ref()
            .map(|grant| grant.body.clone())
            .ok_or(Refusal::StateUnverifiable)?;
        let reason = inner
            .state
            .close_reason
            .unwrap_or(CloseReason::StateUnverifiable);
        let closing_at = inner.state.closing_at.unwrap_or_else(|| self.clock.wall());
        let closed_at = if reason == CloseReason::Expired {
            grant.expires_at.min(closing_at)
        } else {
            closing_at
        };
        let (opened_at, gaps, coverage) = match inner.state.opened_at {
            Some(opened) => {
                let gaps: Vec<Gap> = inner
                    .state
                    .gaps
                    .iter()
                    .filter_map(|gap| {
                        let (start, end) = (gap.start.max(opened), gap.end.min(closed_at));
                        (end > start).then_some(Gap { start, end })
                    })
                    .collect();
                let lost: i64 = gaps.iter().map(|gap| gap.end - gap.start).sum();
                (Some(opened), gaps, ((closed_at - opened) - lost).max(0))
            }
            None => (None, Vec::new(), 0),
        };
        let handback = Handback {
            seq: grant.seq,
            grant_digest: inner.state.grant_digest.clone().unwrap_or_default(),
            opened_at,
            closed_at,
            reason,
            coverage_seconds: coverage,
            gaps,
            high_water: self.handback.high_water(),
        };
        let loaded = self.store.load()?;
        let already = matches!(
            loaded.ledger.last().map(|line| &line.entry),
            Some(HubEntry::Closed { handback: last }) if last.seq == grant.seq
        );
        let handback = if already {
            last_handback(&loaded.ledger).unwrap_or(handback)
        } else {
            self.store.append(HubEntry::Closed {
                handback: handback.clone(),
            })?;
            handback
        };
        let mut closed = WindowState::closed();
        closed.close_reason = Some(reason);
        inner.state = closed;
        inner.guard = None;
        inner.claim = None;
        inner.last_handback = Some(handback.clone());
        self.gate.close();
        self.store.write_state(&inner.state)?;
        Ok(Some(handback))
    }

    // ---- resume ----------------------------------------------------------

    /// Marks the store unverifiable: the gate stays shut, nothing is written,
    /// and every open is refused until `window trust` is rerun with a newer
    /// epoch. The in-memory phase reads Closed so no collector starts.
    fn fault_locked(&self, inner: &mut Inner) -> ResumeOutcome {
        self.gate.close();
        inner.fault = true;
        inner.guard = None;
        inner.claim = None;
        let mut state = WindowState::closed();
        state.close_reason = Some(CloseReason::StateUnverifiable);
        inner.state = state;
        ResumeOutcome::Faulted
    }

    fn resume_locked(&self, inner: &mut Inner) -> ResumeOutcome {
        let loaded = match self.store.load() {
            Ok(loaded) => loaded,
            Err(_) => return self.fault_locked(inner),
        };
        if loaded.trust.is_none() {
            return ResumeOutcome::Fresh;
        }
        inner.trust = loaded.trust.clone();
        inner.state = loaded.state.clone();
        inner.last_handback = last_handback(&loaded.ledger);
        let wall = self.clock.wall();
        let mono = self.clock.mono();
        self.gate.close();

        if inner.state.phase == Phase::Closed && !ledger_synced(&loaded.ledger) {
            // A crash between the ledger line and the state file: the grant is
            // consumed and no window ran (or the state write was lost). Seal it.
            self.seal_from_ledger(&loaded.ledger, wall);
            if let Ok(reloaded) = self.store.load() {
                inner.last_handback = last_handback(&reloaded.ledger);
            }
            return ResumeOutcome::Closed;
        }
        match inner.state.phase {
            Phase::Closed => ResumeOutcome::Closed,
            Phase::Closing => {
                if self.verify_persisted(inner, &loaded).is_err() {
                    return self.fault_locked(inner);
                }
                let reason = inner
                    .state
                    .close_reason
                    .unwrap_or(CloseReason::StateUnverifiable);
                match self.finish_close_locked(inner) {
                    Ok(_) => ResumeOutcome::ClosedAtStart(reason),
                    Err(_) => self.fault_locked(inner),
                }
            }
            Phase::Pending | Phase::Open => {
                if self.verify_persisted(inner, &loaded).is_err() {
                    return self.fault_locked(inner);
                }
                match self.verify_anchor(inner) {
                    Ok(()) => self.resume_active(inner, wall, mono),
                    // The authority cannot be checked now: close, keep the
                    // store, and let a later open check the anchor again.
                    Err(Refusal::AnchorUnavailable) => {
                        self.close_at_start(inner, CloseReason::StateUnverifiable, wall)
                    }
                    // A restored or otherwise superseded window.
                    Err(_) => self.fault_locked(inner),
                }
            }
        }
    }

    /// Checks a persisted Pending, Open or Closing state against the trust
    /// record and the Hub ledger. The signed grant must verify for this host
    /// and trust epoch and match the recorded digest; the ledger's last
    /// consumption must be this grant; Pending and Open must have no close
    /// after it, a monotonic deadline that follows from the anchor and the
    /// signed expiry, and, when Open, an `opened` line repeating the anchor.
    /// A parseable but inconsistent state is refused.
    fn verify_persisted(&self, inner: &Inner, loaded: &Loaded) -> Result<(), Refusal> {
        let bad = Refusal::StateUnverifiable;
        let trust = inner.trust.as_ref().ok_or(bad)?;
        let state = &inner.state;
        let grant = state.grant.as_ref().ok_or(bad)?;
        let body = &grant.body;
        if body.issuer_key_id != trust.document.issuer_key_id
            || body.host_id != self.host_id
            || body.trust_epoch != trust.document.trust_epoch
        {
            return Err(bad);
        }
        grant
            .verify(&trust.document.verifying_key().map_err(|_| bad)?)
            .map_err(|_| bad)?;
        body.check_shape().map_err(|_| bad)?;
        let digest = body.digest().map_err(|_| bad)?;
        if state.grant_digest.as_deref() != Some(digest.as_str()) {
            return Err(bad);
        }
        let index = loaded
            .ledger
            .iter()
            .rposition(|line| matches!(line.entry, HubEntry::Consumed { .. }))
            .ok_or(bad)?;
        let HubEntry::Consumed {
            seq,
            grant_digest,
            host_id,
            starts_at,
            expires_at,
            ..
        } = &loaded.ledger[index].entry
        else {
            return Err(bad);
        };
        if *seq != body.seq
            || *grant_digest != digest
            || *host_id != body.host_id
            || *starts_at != body.starts_at
            || *expires_at != body.expires_at
        {
            return Err(bad);
        }
        let after = &loaded.ledger[index + 1..];
        let closed = after.iter().any(
            |line| matches!(&line.entry, HubEntry::Closed { handback } if handback.seq == body.seq),
        );
        let opened = after.iter().rev().find_map(|line| match line.entry {
            HubEntry::Opened { seq, at, mono } if seq == body.seq => Some((at, mono)),
            _ => None,
        });
        let anchored = state.deadline_mono
            == deadline_mono_for(state.anchor_wall, state.anchor_mono, body.expires_at);
        let consistent = match state.phase {
            Phase::Closed | Phase::Closing => true,
            Phase::Pending => {
                !closed && anchored && state.opened_at.is_none() && state.close_reason.is_none()
            }
            Phase::Open => {
                !closed
                    && anchored
                    && state.close_reason.is_none()
                    && state.opened_at == Some(state.anchor_wall)
                    && opened == Some((state.anchor_wall, state.anchor_mono))
            }
        };
        if consistent { Ok(()) } else { Err(bad) }
    }

    /// The consumption anchor outside the data directory must name this
    /// trust epoch and this grant as the last one consumed. A newer epoch or
    /// a later grant means the state was restored from before them; a
    /// missing or unparseable anchor cannot vouch for it.
    fn verify_anchor(&self, inner: &Inner) -> Result<(), Refusal> {
        let trust = inner.trust.as_ref().ok_or(Refusal::StateUnverifiable)?;
        let seq = inner
            .state
            .grant
            .as_ref()
            .map(|grant| grant.body.seq)
            .ok_or(Refusal::StateUnverifiable)?;
        let value = self
            .anchor
            .read(&self.anchor_name())?
            .ok_or(Refusal::AnchorMissing)?;
        let (epoch, anchored_seq) = parse_anchor(&value).ok_or(Refusal::AnchorMismatch)?;
        if epoch != trust.document.trust_epoch {
            return Err(Refusal::AnchorMismatch);
        }
        match anchored_seq.cmp(&seq) {
            std::cmp::Ordering::Equal => Ok(()),
            std::cmp::Ordering::Greater => Err(Refusal::GrantConsumed),
            std::cmp::Ordering::Less => Err(Refusal::AnchorMismatch),
        }
    }

    fn seal_from_ledger(&self, ledger: &[Line<HubEntry>], wall: i64) {
        let mut consumed = None;
        let mut opened_at = None;
        for line in ledger {
            match &line.entry {
                HubEntry::Consumed {
                    seq, grant_digest, ..
                } => {
                    consumed = Some((*seq, grant_digest.clone()));
                    opened_at = None;
                }
                HubEntry::Opened { at, .. } => opened_at = Some(*at),
                _ => {}
            }
        }
        if let Some((seq, digest)) = consumed {
            self.seal_orphan(seq, &digest, opened_at, wall);
        }
    }

    /// Resumes a verified Pending or Open window to its original deadlines.
    /// Clock, boot, expiry, monotonic deadline, offset and claim are all
    /// checked before the gate reopens; any failure closes the window.
    fn resume_active(&self, inner: &mut Inner, wall: i64, mono: u64) -> ResumeOutcome {
        let Some(grant) = inner.state.grant.as_ref().map(|grant| grant.body.clone()) else {
            return self.fault_locked(inner);
        };
        let phase = inner.state.phase;
        if wall < CLOCK_FLOOR_EPOCH_SECONDS
            || wall + DRIFT_TOLERANCE_SECONDS < inner.state.watermark
        {
            return self.close_at_start(inner, CloseReason::ClockInvalid, wall);
        }
        match self.clock.boot_time() {
            Ok(boot) if (boot - inner.state.boot_time).abs() <= BOOT_TIME_TOLERANCE_SECONDS => {}
            Ok(_) => return self.close_at_start(inner, CloseReason::Reboot, wall),
            Err(_) => return self.close_at_start(inner, CloseReason::ClockInvalid, wall),
        }
        if wall >= grant.expires_at {
            return self.close_at_start(inner, CloseReason::Expired, wall);
        }
        let watermark = inner.state.watermark.max(wall);
        let guard = if phase == Phase::Open {
            // The original monotonic deadline holds only on a boot-scoped,
            // sleep-counting clock that has not run back; otherwise the
            // authorised remainder cannot be established and the window
            // fails closed.
            if !self.clock.mono_boot_scoped() || mono < inner.state.anchor_mono {
                return self.close_at_start(inner, CloseReason::ClockInvalid, wall);
            }
            let mut guard = ClockGuard {
                anchor_wall: inner.state.anchor_wall,
                anchor_mono: inner.state.anchor_mono,
                deadline_wall: grant.expires_at,
                deadline_mono: inner.state.deadline_mono,
                watermark,
            };
            match guard.check(wall, mono) {
                ClockVerdict::Ok => guard,
                ClockVerdict::Expired => {
                    return self.close_at_start(inner, CloseReason::Expired, wall);
                }
                ClockVerdict::Invalid => {
                    return self.close_at_start(inner, CloseReason::ClockInvalid, wall);
                }
            }
        } else {
            // Pending: nothing was authorised yet; the guard is anchored now
            // and again when the window opens.
            let guard = ClockGuard::anchored(wall, mono, grant.expires_at, watermark);
            inner.state.anchor_wall = wall;
            inner.state.anchor_mono = mono;
            inner.state.deadline_mono = guard.deadline_mono;
            guard
        };
        // The offset check completes before anything can reopen (R4).
        if self.check_offset().is_err() {
            return self.close_at_start(inner, CloseReason::ClockInvalid, wall);
        }
        match HostClaim::acquire(&self.claim_path) {
            Ok(claim) => inner.claim = Some(claim),
            Err(_) => return self.close_at_start(inner, CloseReason::ClaimLost, wall),
        }
        inner.guard = Some(guard);
        inner.last_ntp_mono = Some(mono);
        inner.state.watermark = guard.watermark;
        if phase == Phase::Open {
            let resumed = self.store.append(HubEntry::Resumed {
                seq: grant.seq,
                at: wall,
            });
            if resumed.is_err() || self.store.write_state(&inner.state).is_err() {
                return self.close_at_start(inner, CloseReason::StateUnverifiable, wall);
            }
            self.gate.open(grant.expires_at, guard.deadline_mono);
        } else if self.store.write_state(&inner.state).is_err() {
            return self.close_at_start(inner, CloseReason::StateUnverifiable, wall);
        }
        ResumeOutcome::Resumed(phase)
    }

    fn close_at_start(&self, inner: &mut Inner, reason: CloseReason, wall: i64) -> ResumeOutcome {
        self.begin_close(inner, reason, wall);
        match self.finish_close_locked(inner) {
            Ok(_) => ResumeOutcome::ClosedAtStart(reason),
            Err(_) => self.fault_locked(inner),
        }
    }

    // ---- import admission (design 2.10) -----------------------------------

    /// Admits a read-only import (or a data restore) only while Closed with
    /// the last handback synced. The shared window lock is held for the whole
    /// import, so `window open` is refused `import_running` meanwhile; import
    /// during Pending, Open or Closing is refused `window_active`.
    pub fn import_guard(&self) -> Result<ImportGuard, Refusal> {
        let inner = self.lock();
        if inner.fault {
            return Err(Refusal::StateUnverifiable);
        }
        if inner.state.phase != Phase::Closed {
            return Err(Refusal::WindowActive);
        }
        self.store.ensure_dir()?;
        if inner.trust.is_some() {
            let loaded = self.store.load()?;
            if !ledger_synced(&loaded.ledger) {
                return Err(Refusal::HandbackPending);
            }
        }
        let lock = FileLock::acquire(&self.store.lock_path(), LockMode::Shared, false)?
            .ok_or(Refusal::WindowActive)?;
        Ok(ImportGuard { _lock: lock })
    }
}
