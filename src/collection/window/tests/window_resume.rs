// SPDX-License-Identifier: AGPL-3.0-only

use std::{fs, sync::Arc};

use super::support::*;
use crate::collection::window::{
    AnchorStore, CloseReason, HandbackSource, HighWater, HostClaim, PermitKind, Phase, Provider,
    Refusal, ResumeOutcome, SignedGrant, Supervisor, WindowController,
    grant::GrantBody,
    state::{HubEntry, WindowStore},
};

fn closed_entries(rig: &Rig) -> usize {
    WindowStore::new(&rig.window_dir())
        .load()
        .unwrap()
        .ledger
        .iter()
        .filter(|line| matches!(line.entry, HubEntry::Closed { .. }))
        .count()
}

fn open_hub(rig: &Rig, start_in: i64, length: i64) -> WindowController {
    let hub = rig.trusted_hub();
    hub.open(&rig.grant(start_in, length)).unwrap();
    hub
}

#[test]
fn window_resume_carries_on_only_to_the_original_deadline() {
    let rig = Rig::new();
    let hub = open_hub(&rig, 0, 3600);
    rig.clock.advance(1000);
    drop(hub);
    // A restart is a new process on the same boot-scoped monotonic clock.
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Resumed(Phase::Open));
    assert_eq!(
        hub.status().deadline,
        Some(T0 + 3600),
        "restart never moves the deadline"
    );
    let permit = hub
        .gate()
        .permit(Provider::Legacy, PermitKind::Request)
        .unwrap();
    assert_eq!(permit.time_left().as_secs(), 2600);
    assert!(
        HostClaim::acquire(&rig.claim_path()).is_err(),
        "the claim is retaken"
    );
    rig.clock.advance(2599);
    assert_eq!(hub.tick().unwrap(), Phase::Open);
    rig.clock.advance(1);
    assert_eq!(hub.tick().unwrap(), Phase::Closing);
    assert_eq!(hub.finish_close().unwrap().unwrap().closed_at, T0 + 3600);
}

#[test]
fn window_resume_after_expiry_stays_stopped_with_one_close_at_the_deadline() {
    let rig = Rig::new();
    let hub = open_hub(&rig, 0, 3600);
    drop(hub);
    rig.clock.advance(5000);
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::ClosedAtStart(CloseReason::Expired));
    assert_eq!(hub.status().phase, Phase::Closed);
    assert_eq!(hub.status().last_handback.unwrap().closed_at, T0 + 3600);
    assert!(
        hub.gate()
            .permit(Provider::Legacy, PermitKind::Request)
            .is_err()
    );
    assert_eq!(closed_entries(&rig), 1);
    drop(hub);
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Closed);
    assert_eq!(closed_entries(&rig), 1, "no second close");
    assert!(
        hub.gate()
            .permit(Provider::Legacy, PermitKind::Request)
            .is_err()
    );
    assert!(hub.import_guard().is_ok(), "read-only import resumes");
}

#[test]
fn window_resume_reboot_closes() {
    let rig = Rig::new();
    drop(open_hub(&rig, 0, 3600));
    rig.clock.advance(100);
    rig.clock.set_boot(T0 + 50);
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::ClosedAtStart(CloseReason::Reboot));
    assert_eq!(
        hub.status().last_handback.unwrap().reason,
        CloseReason::Reboot
    );
}

#[test]
fn window_resume_watermark_rollback_closes_running_and_at_start() {
    let rig = Rig::new();
    let hub = open_hub(&rig, 0, 3600);
    rig.clock.advance(600);
    assert_eq!(hub.tick().unwrap(), Phase::Open);
    rig.clock.shift_wall(-3000);
    assert_eq!(hub.tick().unwrap(), Phase::Closing);
    assert_eq!(hub.status().close_reason, Some(CloseReason::ClockInvalid));
    assert!(
        hub.gate()
            .permit(Provider::Legacy, PermitKind::Request)
            .is_err()
    );
    drop(hub);

    let rig = Rig::new();
    let hub = open_hub(&rig, 0, 3600);
    rig.clock.advance(600);
    hub.tick().unwrap();
    drop(hub);
    rig.clock.shift_wall(-3000);
    let (_, outcome) = rig.start_hub();
    assert_eq!(
        outcome,
        ResumeOutcome::ClosedAtStart(CloseReason::ClockInvalid)
    );
}

#[test]
fn window_resume_repeated_small_rollbacks_are_caught_by_drift() {
    let rig = Rig::new();
    let hub = open_hub(&rig, 0, 3600);
    let mut closed_after = None;
    for round in 1..=6 {
        rig.clock.advance(60);
        rig.clock.shift_wall(-4);
        if hub.tick().unwrap() == Phase::Closing {
            closed_after = Some(round);
            break;
        }
    }
    assert!(
        closed_after.is_some_and(|round| round <= 4),
        "closed after {closed_after:?}"
    );
    assert_eq!(hub.status().close_reason, Some(CloseReason::ClockInvalid));
}

#[test]
fn window_resume_corrupt_state_or_ledger_fails_closed_until_trust_is_rerun() {
    for damage in [
        "state",
        "ledger-edit",
        "ledger-delete",
        "trust-ok-state-missing",
    ] {
        let rig = Rig::new();
        drop(open_hub(&rig, 0, 3600));
        let dir = rig.window_dir();
        match damage {
            "state" => fs::write(dir.join("state.json"), b"{ not json").unwrap(),
            "ledger-edit" => {
                let text = fs::read_to_string(dir.join("ledger.jsonl")).unwrap();
                fs::write(
                    dir.join("ledger.jsonl"),
                    text.replacen("\"seq\":1", "\"seq\":7", 1),
                )
                .unwrap();
            }
            "ledger-delete" => fs::remove_file(dir.join("ledger.jsonl")).unwrap(),
            _ => fs::remove_file(dir.join("state.json")).unwrap(),
        }
        let (hub, outcome) = rig.start_hub();
        assert_eq!(outcome, ResumeOutcome::Faulted, "{damage}");
        assert!(hub.status().faulted);
        assert!(
            hub.gate()
                .permit(Provider::Legacy, PermitKind::Request)
                .is_err(),
            "{damage}"
        );
        rig.clock.advance(5000);
        let stale = rig.grant(0, 600);
        assert_eq!(hub.open(&stale).unwrap_err(), Refusal::StateUnverifiable);
        assert_eq!(hub.import_guard().unwrap_err(), Refusal::StateUnverifiable);
        // The same epoch cannot be reused; a newer one recovers.
        assert_eq!(
            hub.trust(&rig.trust).unwrap_err(),
            Refusal::TrustEpochNotNewer
        );
        let renewed = rig.issuer.bump_trust_epoch(&rig.key_path).unwrap();
        hub.trust(&renewed).unwrap();
        rig.clock.advance(7200);
        assert_eq!(
            hub.open(&rig.grant(0, 600)).unwrap().phase,
            Phase::Open,
            "{damage}"
        );
    }
}

#[test]
fn window_resume_crash_in_closing_completes_at_start() {
    let rig = Rig::new();
    let hub = open_hub(&rig, 0, 3600);
    rig.clock.advance(3600);
    assert_eq!(hub.tick().unwrap(), Phase::Closing);
    drop(hub); // crash before the close finished
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::ClosedAtStart(CloseReason::Expired));
    assert_eq!(closed_entries(&rig), 1);
    assert_eq!(hub.status().last_handback.unwrap().closed_at, T0 + 3600);
    // A crash after the handback line but before the state file is idempotent.
    let (hub2, outcome2) = rig.start_hub_after_partial_close();
    assert_eq!(outcome2, ResumeOutcome::ClosedAtStart(CloseReason::Expired));
    assert_eq!(closed_entries(&rig), 1);
    drop((hub, hub2));
}

impl Rig {
    /// Rebuilds the Closing state file beside an already written handback.
    fn start_hub_after_partial_close(&self) -> (WindowController, ResumeOutcome) {
        let store = WindowStore::new(&self.window_dir());
        let loaded = store.load().unwrap();
        let handback = match &loaded.ledger.last().unwrap().entry {
            HubEntry::Closed { handback } => handback.clone(),
            other => panic!("unexpected tail {other:?}"),
        };
        let mut state = loaded.state.clone();
        state.phase = Phase::Closing;
        state.close_reason = Some(CloseReason::Expired);
        state.closing_at = Some(handback.closed_at);
        let body = crate::collection::window::GrantBody {
            format: crate::collection::window::grant::GRANT_FORMAT.to_owned(),
            issuer_key_id: self.trust.issuer_key_id.clone(),
            seq: handback.seq,
            host_id: HOST.to_owned(),
            starts_at: T0,
            expires_at: T0 + 3600,
            approver: "G".to_owned(),
            reason: "test window".to_owned(),
            trust_epoch: 1,
        };
        state.grant = Some(SignedGrant::sign(body, &self.signing_key()).unwrap());
        state.grant_digest = Some(handback.grant_digest.clone());
        state.opened_at = handback.opened_at;
        store.write_state(&state).unwrap();
        self.start_hub()
    }
}

#[test]
fn window_resume_consumed_grant_is_refused_after_restart_and_restore() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let snapshot = rig.tmp.path().join("snapshot");
    copy_dir(&rig.window_dir(), &snapshot);
    let grant = rig.grant(0, 3600);
    hub.open(&grant).unwrap();
    hub.close(CloseReason::OperatorClose);
    hub.finish_close().unwrap();
    drop(hub);

    // After a plain restart.
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Closed);
    assert_eq!(hub.open(&grant).unwrap_err(), Refusal::GrantConsumed);
    drop(hub);

    // After a restore of an older, valid state-and-ledger pair.
    fs::remove_dir_all(rig.window_dir()).unwrap();
    copy_dir(&snapshot, &rig.window_dir());
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Closed);
    assert_eq!(hub.open(&grant).unwrap_err(), Refusal::GrantConsumed);
    // A later grant still works.
    rig.clock.advance(5000);
    assert_eq!(hub.open(&rig.grant(0, 600)).unwrap().phase, Phase::Open);
}

struct FixedHighWater;

impl HandbackSource for FixedHighWater {
    fn high_water(&self) -> HighWater {
        HighWater {
            drives: Some(12),
            charges: Some(7),
            positions: Some(90_001),
        }
    }
}

#[test]
fn window_resume_handback_record_is_durable() {
    let rig = Rig::new();
    let (hub, _) = rig.start_hub_with(Arc::new(FixedHighWater));
    hub.trust(&rig.trust).unwrap();
    let hub = Arc::new(hub);
    let grant = rig.grant(0, 3600);
    hub.open(&grant).unwrap();
    let slot = FakeSlot::default();
    let mut supervisor = Supervisor::new(hub.clone(), slot.clone());
    supervisor.step();
    rig.clock.advance(1000);
    slot.log.lock().unwrap().running = false;
    slot.log.lock().unwrap().fail_starts = 1;
    supervisor.step(); // failure noted, gap opens at T0+1000
    rig.clock.advance(100);
    hub.close(CloseReason::ReloginNeeded);
    let report = supervisor.step();
    assert_eq!(report.closed, Some(CloseReason::ReloginNeeded));
    let handback = hub.status().last_handback.unwrap();
    assert_eq!(handback.seq, 1);
    assert_eq!(handback.opened_at, Some(T0));
    assert_eq!(handback.closed_at, T0 + 1100);
    assert_eq!(handback.gaps.len(), 1);
    assert_eq!(handback.coverage_seconds, 1100 - 100);
    assert_eq!(handback.high_water.positions, Some(90_001));
    let digest = grant_digest(&grant);
    assert_eq!(handback.grant_digest, digest);
    drop((supervisor, hub));
    // The record survives a restart and carries no credential or location text.
    let (hub, _) = rig.start_hub();
    assert_eq!(hub.status().last_handback, Some(handback));
    let ledger = String::from_utf8(rig.read("ledger.jsonl")).unwrap();
    let state = String::from_utf8(rig.read("state.json")).unwrap();
    for text in [ledger, state] {
        for forbidden in ["token", "vin", "latitude", "longitude", "password"] {
            assert!(!text.to_lowercase().contains(forbidden), "{forbidden}");
        }
    }
}

fn grant_digest(bytes: &[u8]) -> String {
    crate::collection::window::SignedGrant::parse(bytes)
        .unwrap()
        .body
        .digest()
        .unwrap()
}

#[test]
fn window_resume_import_admission_follows_the_window_and_its_handback() {
    let rig = Rig::new();
    let hub = Arc::new(rig.trusted_hub());
    assert!(
        hub.import_guard().is_ok(),
        "a fresh trusted Hub is read-only import"
    );
    hub.open(&rig.grant(600, 3600)).unwrap();
    assert_eq!(
        hub.import_guard().unwrap_err(),
        Refusal::WindowActive,
        "pending"
    );
    rig.clock.advance(600);
    hub.tick().unwrap();
    assert_eq!(
        hub.import_guard().unwrap_err(),
        Refusal::WindowActive,
        "open"
    );
    hub.close(CloseReason::OperatorClose);
    assert_eq!(
        hub.import_guard().unwrap_err(),
        Refusal::WindowActive,
        "closing"
    );
    let mut supervisor = Supervisor::new(hub.clone(), FakeSlot::default());
    supervisor.step();
    let guard = hub.import_guard().expect("admitted after the handback");
    // An open during the import is refused, and goes through afterwards.
    rig.clock.advance(5000);
    let next = rig.grant(0, 600);
    assert_eq!(hub.open(&next).unwrap_err(), Refusal::ImportRunning);
    drop(guard);
    assert_eq!(hub.open(&next).unwrap().phase, Phase::Open);
}

#[test]
fn window_resume_crash_between_ledger_and_state_is_sealed_at_start() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    drop(hub);
    // A consumed line with no state change behind it.
    let store = WindowStore::new(&rig.window_dir());
    store
        .append(HubEntry::Consumed {
            seq: 1,
            grant_digest: "00".repeat(32),
            host_id: HOST.to_owned(),
            starts_at: T0,
            expires_at: T0 + 600,
            at: T0,
        })
        .unwrap();
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Closed);
    assert_eq!(
        hub.status().last_handback.unwrap().reason,
        CloseReason::StateUnverifiable
    );
    assert!(hub.import_guard().is_ok());
}

#[test]
fn window_resume_pending_window_stays_pending_and_opens_at_its_start() {
    let rig = Rig::new();
    drop(open_hub(&rig, 1200, 3600));
    rig.clock.advance(300);
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Resumed(Phase::Pending));
    assert!(
        hub.gate()
            .permit(Provider::Legacy, PermitKind::Request)
            .is_err()
    );
    rig.clock.advance(900);
    assert_eq!(hub.tick().unwrap(), Phase::Open);
    assert_eq!(hub.status().deadline, Some(T0 + 1200 + 3600));
}

fn permit_refused(hub: &WindowController) -> bool {
    hub.gate()
        .permit(Provider::Legacy, PermitKind::Request)
        .is_err()
}

fn anchor_name() -> String {
    format!("window-consumption-{HOST}")
}

/// Finding 1: an Open snapshot restored after the window was closed and a
/// newer trust epoch recorded (or after a later grant was consumed) must not
/// resume; nor may a window whose anchor is missing or unreadable.
#[test]
fn window_resume_restored_open_snapshot_after_trust_reset_is_refused() {
    // Restored after a trust reset.
    let rig = Rig::new();
    let hub = open_hub(&rig, 0, 3600);
    let snapshot = rig.tmp.path().join("snapshot");
    copy_dir(&rig.window_dir(), &snapshot);
    hub.close(CloseReason::OperatorClose);
    hub.finish_close().unwrap();
    let renewed = rig.issuer.bump_trust_epoch(&rig.key_path).unwrap();
    hub.trust(&renewed).unwrap();
    drop(hub);
    fs::remove_dir_all(rig.window_dir()).unwrap();
    copy_dir(&snapshot, &rig.window_dir());
    rig.clock.advance(10);
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Faulted);
    assert!(hub.status().faulted);
    assert_eq!(hub.status().phase, Phase::Closed);
    assert!(permit_refused(&hub));
    assert!(!hub.gate().is_open());
    // Recovery is a trust with a newer epoch still.
    let newer = rig.issuer.bump_trust_epoch(&rig.key_path).unwrap();
    hub.trust(&newer).unwrap();
    drop(hub);

    // Restored after a later grant was consumed in the same epoch.
    let rig = Rig::new();
    let hub = open_hub(&rig, 0, 600);
    let snapshot = rig.tmp.path().join("snapshot");
    copy_dir(&rig.window_dir(), &snapshot);
    hub.close(CloseReason::OperatorClose);
    hub.finish_close().unwrap();
    rig.clock.advance(600 + GRANT_GAP as u64);
    hub.open(&rig.grant(0, 600)).unwrap();
    hub.close(CloseReason::OperatorClose);
    hub.finish_close().unwrap();
    drop(hub);
    fs::remove_dir_all(rig.window_dir()).unwrap();
    copy_dir(&snapshot, &rig.window_dir());
    rig.clock.set_mono(1_000);
    rig.clock.shift_wall(-(600 + GRANT_GAP) + 10);
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Faulted);
    assert!(permit_refused(&hub));
    drop(hub);

    // A missing anchor cannot vouch for the window.
    let rig = Rig::new();
    drop(open_hub(&rig, 0, 3600));
    rig.anchor.remove(&anchor_name());
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Faulted);
    assert!(permit_refused(&hub));
    drop(hub);

    // An unreadable anchor closes the window instead of resuming it.
    let rig = Rig::new();
    drop(open_hub(&rig, 0, 3600));
    rig.anchor.set_fail_reads(true);
    let (hub, outcome) = rig.start_hub();
    assert_eq!(
        outcome,
        ResumeOutcome::ClosedAtStart(CloseReason::StateUnverifiable)
    );
    assert!(permit_refused(&hub));
    rig.anchor.set_fail_reads(false);
    assert!(rig.anchor.read(&anchor_name()).unwrap().is_some());
}

const GRANT_GAP: i64 = crate::collection::window::GRANT_GAP_SECONDS;

fn edit_state(rig: &Rig, edit: impl FnOnce(&mut serde_json::Value)) {
    let path = rig.window_dir().join("state.json");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edit(&mut state);
    fs::write(path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
}

/// Finding 2: the persisted grant is verified on load and reconciled with the
/// ledger; changing the persisted expiry, or swapping in another validly
/// signed grant the ledger never consumed, is refused.
#[test]
fn window_resume_tampered_persisted_grant_is_refused() {
    // The expiry is moved out without a new signature.
    let rig = Rig::new();
    drop(open_hub(&rig, 0, 3600));
    edit_state(&rig, |state| {
        state["grant"]["body"]["expires_at"] = (T0 + 7200).into();
    });
    rig.clock.advance(3700);
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Faulted);
    assert!(permit_refused(&hub));
    drop(hub);

    // A validly signed grant, but not the one the ledger consumed.
    let rig = Rig::new();
    drop(open_hub(&rig, 0, 3600));
    let body = GrantBody {
        format: crate::collection::window::grant::GRANT_FORMAT.to_owned(),
        issuer_key_id: rig.trust.issuer_key_id.clone(),
        seq: 1,
        host_id: HOST.to_owned(),
        starts_at: T0,
        expires_at: T0 + 4 * 3600,
        approver: "G".to_owned(),
        reason: "test window".to_owned(),
        trust_epoch: 1,
    };
    let digest = body.digest().unwrap();
    let other = SignedGrant::sign(body, &rig.signing_key()).unwrap();
    edit_state(&rig, |state| {
        state["grant"] = serde_json::to_value(&other).unwrap();
        state["grant_digest"] = digest.into();
    });
    rig.clock.advance(3700);
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Faulted);
    assert!(permit_refused(&hub));
    drop(hub);

    // The monotonic deadline is moved out while the anchor is kept.
    let rig = Rig::new();
    drop(open_hub(&rig, 0, 3600));
    edit_state(&rig, |state| {
        let deadline = state["deadline_mono"].as_u64().unwrap();
        state["deadline_mono"] = (deadline + 3600).into();
    });
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Faulted);
    assert!(permit_refused(&hub));
}

/// Finding 3: on restart the time offset is measured before the gate
/// reopens; an unavailable or out-of-bound offset gives no permit at all.
#[test]
fn window_resume_offset_check_completes_before_the_gate_reopens() {
    for result in [Err(Refusal::ClockOffset), Ok(31.0), Ok(f64::NAN)] {
        let rig = Rig::new();
        drop(open_hub(&rig, 0, 3600));
        rig.clock.advance(60);
        rig.probe.set(result);
        let calls = rig.probe.calls();
        let (hub, outcome) = rig.start_hub();
        assert_eq!(rig.probe.calls(), calls + 1, "{result:?}");
        assert_eq!(
            outcome,
            ResumeOutcome::ClosedAtStart(CloseReason::ClockInvalid),
            "{result:?}"
        );
        assert!(permit_refused(&hub), "{result:?}");
        assert!(!hub.gate().is_open(), "{result:?}");
    }
    // A good offset is measured once at restart, and the window resumes.
    let rig = Rig::new();
    drop(open_hub(&rig, 0, 3600));
    let calls = rig.probe.calls();
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Resumed(Phase::Open));
    assert_eq!(rig.probe.calls(), calls + 1);
    assert!(!permit_refused(&hub));
}

/// Finding 4: a tolerated rollback followed by a restart must not buy time;
/// the original monotonic deadline survives the restart, and a clock that
/// cannot carry it fails closed.
#[test]
fn window_resume_rollback_then_restart_keeps_the_original_monotonic_deadline() {
    let rig = Rig::new();
    let hub = open_hub(&rig, 0, 60);
    rig.clock.advance(59);
    rig.clock.shift_wall(-4);
    assert_eq!(
        hub.tick().unwrap(),
        Phase::Open,
        "the rollback is tolerated"
    );
    drop(hub);
    let (hub, outcome) = rig.start_hub();
    assert_eq!(outcome, ResumeOutcome::Resumed(Phase::Open));
    let permit = hub
        .gate()
        .permit(Provider::Legacy, PermitKind::Request)
        .unwrap();
    assert_eq!(permit.time_left().as_secs(), 1, "one second, not five");
    rig.clock.advance(1);
    assert!(permit_refused(&hub));
    assert!(permit.is_cancelled());
    assert_eq!(hub.tick().unwrap(), Phase::Closing);
    drop(hub);

    // A process-scoped monotonic clock cannot carry the deadline: fail closed.
    let rig = Rig::new();
    drop(open_hub(&rig, 0, 3600));
    rig.clock.advance(100);
    rig.clock.set_boot_scoped(false);
    let (hub, outcome) = rig.start_hub();
    assert_eq!(
        outcome,
        ResumeOutcome::ClosedAtStart(CloseReason::ClockInvalid)
    );
    assert!(permit_refused(&hub));
    drop(hub);

    // A monotonic reading behind the persisted anchor: fail closed.
    let rig = Rig::new();
    drop(open_hub(&rig, 0, 3600));
    rig.clock.advance(100);
    rig.clock.set_mono(5);
    let (hub, outcome) = rig.start_hub();
    assert_eq!(
        outcome,
        ResumeOutcome::ClosedAtStart(CloseReason::ClockInvalid)
    );
    assert!(permit_refused(&hub));
}
