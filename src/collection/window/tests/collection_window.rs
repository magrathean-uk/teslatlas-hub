// SPDX-License-Identifier: AGPL-3.0-only

use std::{fs, sync::Arc, thread, time::Duration};

use ed25519_dalek::SigningKey;

use super::support::*;
use crate::collection::window::{
    AnchorStore, CloseReason, EXCHANGE_GRACE_SECONDS, EXCHANGE_WRITE_MARGIN_SECONDS, EgressGate,
    GRANT_GAP_SECONDS, SUPERVISOR_POLL_SECONDS, WindowClock, write_under_authority, GateRefusal, HostClaim, ManualClock,
    ManualProbe, PermitKind, Phase, Provider, Refusal, SignedGrant, Supervisor,
    WINDOW_CEILING_SECONDS,
    clock::{sntp_offset, sntp_request},
    grant::{GRANT_FORMAT, GrantBody, TrustDocument},
};

fn sample_body() -> GrantBody {
    GrantBody {
        format: GRANT_FORMAT.to_owned(),
        issuer_key_id: "ed25519-sha256-aa".to_owned(),
        seq: 4,
        host_id: HOST.to_owned(),
        starts_at: T0,
        expires_at: T0 + 3600,
        approver: "G".to_owned(),
        reason: "why".to_owned(),
        trust_epoch: 1,
    }
}

fn hand_signed(rig: &Rig, edit: impl FnOnce(&mut GrantBody)) -> Vec<u8> {
    hand_signed_by(&rig.signing_key(), rig, edit)
}

fn hand_signed_by(key: &SigningKey, rig: &Rig, edit: impl FnOnce(&mut GrantBody)) -> Vec<u8> {
    let mut body = sample_body();
    body.issuer_key_id = rig.trust.issuer_key_id.clone();
    body.seq = 50;
    body.starts_at = T0;
    body.expires_at = T0 + 3600;
    edit(&mut body);
    SignedGrant::sign(body, key).unwrap().to_bytes().unwrap()
}

#[test]
fn collection_window_grant_round_trip_and_tamper() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let grant = SignedGrant::sign(sample_body(), &key).unwrap();
    let parsed = SignedGrant::parse(&grant.to_bytes().unwrap()).unwrap();
    assert_eq!(parsed, grant);
    parsed.verify(&key.verifying_key()).unwrap();

    // Tampering with each body field breaks the signature.
    let tampers: Vec<(&str, serde_json::Value)> = vec![
        ("format", "window-grant/2".into()),
        ("issuer_key_id", "other".into()),
        ("seq", 5.into()),
        ("host_id", "other-host".into()),
        ("starts_at", (T0 + 1).into()),
        ("expires_at", (T0 + 3601).into()),
        ("approver", "H".into()),
        ("reason", "other".into()),
        ("trust_epoch", 2.into()),
    ];
    for (field, value) in tampers {
        let mut json: serde_json::Value =
            serde_json::from_slice(&grant.to_bytes().unwrap()).unwrap();
        json["body"][field] = value;
        let tampered = SignedGrant::parse(&serde_json::to_vec(&json).unwrap()).unwrap();
        assert_eq!(
            tampered.verify(&key.verifying_key()),
            Err(Refusal::SignatureInvalid),
            "field {field}"
        );
    }
    // A valid body re-signed with a different key does not verify under the trusted key.
    let other = SigningKey::from_bytes(&[9u8; 32]);
    let resigned = SignedGrant::sign(sample_body(), &other).unwrap();
    assert_eq!(
        resigned.verify(&key.verifying_key()),
        Err(Refusal::SignatureInvalid)
    );
}

#[test]
fn collection_window_refusals_change_nothing() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let good = rig.grant(0, 3600);
    let before = (rig.read("state.json"), rig.read("ledger.jsonl"));

    let mut unknown: serde_json::Value = serde_json::from_slice(&good).unwrap();
    unknown["body"]["extra"] = 1.into();
    let other_key = SigningKey::from_bytes(&[3u8; 32]);
    let cases: Vec<(&str, Vec<u8>, Refusal)> = vec![
        ("oversize", vec![b' '; 5000], Refusal::GrantTooLarge),
        ("garbage", b"not json".to_vec(), Refusal::GrantMalformed),
        (
            "unknown field",
            serde_json::to_vec(&unknown).unwrap(),
            Refusal::GrantMalformed,
        ),
        (
            "other host",
            hand_signed(&rig, |b| b.host_id = "elsewhere".into()),
            Refusal::HostMismatch,
        ),
        (
            "approver",
            hand_signed(&rig, |b| b.approver = "H".into()),
            Refusal::ApproverNotG,
        ),
        (
            "format",
            hand_signed(&rig, |b| b.format = "window-grant/9".into()),
            Refusal::FormatUnknown,
        ),
        (
            "ceiling",
            hand_signed(&rig, |b| {
                b.expires_at = b.starts_at + WINDOW_CEILING_SECONDS + 1
            }),
            Refusal::LengthExceedsCeiling,
        ),
        (
            "expiry before start",
            hand_signed(&rig, |b| b.expires_at = b.starts_at),
            Refusal::ExpiryInvalid,
        ),
        (
            "reason",
            hand_signed(&rig, |b| b.reason = "x".repeat(201)),
            Refusal::ReasonTooLong,
        ),
        (
            "expired",
            hand_signed(&rig, |b| {
                b.starts_at = T0 - 7200;
                b.expires_at = T0 - 3600;
            }),
            Refusal::GrantExpired,
        ),
        (
            "epoch",
            hand_signed(&rig, |b| b.trust_epoch = 2),
            Refusal::TrustEpochMismatch,
        ),
        (
            "key id",
            hand_signed(&rig, |b| b.issuer_key_id = "ed25519-sha256-00".into()),
            Refusal::KeyIdMismatch,
        ),
        (
            "wrong signer",
            hand_signed_by(&other_key, &rig, |_| {}),
            Refusal::SignatureInvalid,
        ),
    ];
    for (name, bytes, expected) in cases {
        assert_eq!(hub.open(&bytes).unwrap_err(), expected, "{name}");
        assert_eq!(hub.status().phase, Phase::Closed, "{name}");
        assert!(!hub.gate().is_open(), "{name}");
    }

    // Clock offset: too large, and unmeasurable.
    rig.probe.set(Ok(31.0));
    assert_eq!(hub.open(&good).unwrap_err(), Refusal::ClockOffset);
    rig.probe.set(Err(Refusal::ClockOffset));
    assert_eq!(hub.open(&good).unwrap_err(), Refusal::ClockOffset);
    rig.probe.set(Ok(-30.0));
    // (the exact limit is accepted; reset below before the real open)
    rig.probe.set(Ok(0.1));

    // Import running, and another collector on the host.
    let import = hub.import_guard().unwrap();
    assert_eq!(hub.open(&good).unwrap_err(), Refusal::ImportRunning);
    drop(import);
    let claim = HostClaim::acquire(&rig.claim_path()).unwrap();
    assert_eq!(hub.open(&good).unwrap_err(), Refusal::CollectorBusy);
    drop(claim);

    assert_eq!((rig.read("state.json"), rig.read("ledger.jsonl")), before);
    // A Hub with no trust refuses with its own code.
    let bare = Rig::new();
    let (fresh, _) = bare.start_hub();
    assert_eq!(fresh.open(&good).unwrap_err(), Refusal::TrustMissing);
    // And after all of it the good grant still opens.
    assert_eq!(hub.open(&good).unwrap().phase, Phase::Open);
}

#[test]
fn collection_window_exact_ceiling_is_accepted() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let grant = rig.grant(0, WINDOW_CEILING_SECONDS);
    assert_eq!(hub.open(&grant).unwrap().phase, Phase::Open);
    let too_long = rig.request(
        HOST,
        4 * 3600 + GRANT_GAP_SECONDS + 60,
        WINDOW_CEILING_SECONDS + 1,
    );
    assert_eq!(
        rig.try_grant(&too_long).unwrap_err(),
        Refusal::LengthExceedsCeiling
    );
}

#[test]
fn collection_window_valid_grant_opens_and_gates() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    assert_eq!(
        hub.gate()
            .permit(Provider::Legacy, PermitKind::Request)
            .unwrap_err(),
        GateRefusal::WindowClosed
    );
    let status = hub.open(&rig.grant(0, 3600)).unwrap();
    assert_eq!(status.phase, Phase::Open);
    assert_eq!(status.deadline, Some(T0 + 3600));
    assert_eq!(
        rig.anchor
            .read(&format!("window-consumption-{HOST}"))
            .unwrap()
            .unwrap(),
        "1:1"
    );
    let permit = hub
        .gate()
        .permit(Provider::Legacy, PermitKind::Request)
        .unwrap();
    assert_eq!(permit.time_left(), Duration::from_secs(3600));
    assert_eq!(
        hub.gate()
            .permit(Provider::Fleet, PermitKind::Request)
            .unwrap_err(),
        GateRefusal::ProviderParked
    );
}

#[test]
fn collection_window_future_grant_pends_then_opens() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let status = hub.open(&rig.grant(600, 3600)).unwrap();
    assert_eq!(status.phase, Phase::Pending);
    assert!(
        hub.gate()
            .permit(Provider::Legacy, PermitKind::Request)
            .is_err()
    );
    rig.clock.advance(300);
    assert_eq!(hub.tick().unwrap(), Phase::Pending);
    rig.clock.advance(301);
    assert_eq!(hub.tick().unwrap(), Phase::Open);
    assert!(
        hub.gate()
            .permit(Provider::Legacy, PermitKind::Request)
            .is_ok()
    );
}

#[test]
fn collection_window_stops_at_expiry_without_operator() {
    let rig = Rig::new();
    let hub = Arc::new(rig.trusted_hub());
    hub.open(&rig.grant(0, 3600)).unwrap();
    let slot = FakeSlot::default();
    let mut supervisor = Supervisor::new(hub.clone(), slot.clone());
    let report = supervisor.step();
    assert!(report.collector_running);
    assert_eq!(slot.log.lock().unwrap().deadline, Some(T0 + 3600));

    // Time passes with nobody there. The gate refuses at the deadline even
    // before any supervisor step runs.
    let stream = hub
        .gate()
        .permit(Provider::Legacy, PermitKind::Stream)
        .unwrap();
    rig.clock.advance(3600);
    assert_eq!(
        hub.gate()
            .permit(Provider::Legacy, PermitKind::Request)
            .unwrap_err(),
        GateRefusal::WindowClosed
    );
    let report = supervisor.step();
    assert_eq!(report.closed, Some(CloseReason::Expired));
    assert_eq!(hub.status().phase, Phase::Closed);
    let log = slot.log.lock().unwrap();
    assert!(!log.running);
    assert_eq!(log.released, 1);
    assert!(stream.is_cancelled());
    let handback = hub.status().last_handback.unwrap();
    assert_eq!(handback.closed_at, T0 + 3600);
    assert_eq!(handback.reason, CloseReason::Expired);
    assert_eq!(handback.coverage_seconds, 3600);
    // Retries end: every attempt needs a new permit, and none is issued.
    for _ in 0..3 {
        assert!(
            hub.gate()
                .permit(Provider::Legacy, PermitKind::TokenExchange)
                .is_err()
        );
    }
}

#[test]
fn collection_window_exchange_grace_holds_closing_then_releases() {
    let rig = Rig::new();
    let hub = Arc::new(rig.trusted_hub());
    hub.open(&rig.grant(0, 3600)).unwrap();
    let slot = FakeSlot::default();
    let mut supervisor = Supervisor::new(hub.clone(), slot.clone());
    supervisor.step();
    rig.clock.advance(3590);
    let exchange = hub
        .gate()
        .permit(Provider::Legacy, PermitKind::TokenExchange)
        .unwrap();
    assert_eq!(exchange.time_left(), Duration::from_secs(10 + 60));
    rig.clock.advance(10);
    let report = supervisor.step();
    assert_eq!(report.phase, Phase::Closing);
    assert_eq!(
        slot.log.lock().unwrap().released,
        0,
        "lease kept while the exchange runs"
    );
    assert!(
        hub.gate()
            .permit(Provider::Legacy, PermitKind::TokenExchange)
            .is_err()
    );
    drop(exchange);
    let report = supervisor.step();
    assert_eq!(report.closed, Some(CloseReason::Expired));
    assert_eq!(slot.log.lock().unwrap().released, 1);

    // An exchange that has not finished when its grace lapses (the grace
    // less the write margin) is cancelled there, and the window is handed
    // back at once although the exchange still holds its permit: no write
    // can begin under a cancelled permit (M02-T13 round 2, design 2.7).
    let rig = Rig::new();
    let hub = Arc::new(rig.trusted_hub());
    hub.open(&rig.grant(0, 3600)).unwrap();
    let slot = FakeSlot::default();
    let mut supervisor = Supervisor::new(hub.clone(), slot.clone());
    supervisor.step();
    rig.clock.advance(3599);
    let held = hub
        .gate()
        .permit(Provider::Legacy, PermitKind::TokenExchange)
        .unwrap();
    rig.clock.advance(1);
    assert_eq!(supervisor.step().phase, Phase::Closing);
    assert!(!held.is_cancelled(), "the exchange keeps its grace");
    rig.clock.advance((EXCHANGE_GRACE_SECONDS - EXCHANGE_WRITE_MARGIN_SECONDS) as u64);
    let report = step_until_closed(&mut supervisor);
    assert_eq!(report.closed, Some(CloseReason::Expired));
    assert!(held.is_cancelled(), "the grace has lapsed");
    assert!(hub.gate().exchange_in_flight(), "handed back with the permit still held");
    drop(held);
}

/// Steps until the window is handed back, without moving the clock: a write
/// of another test may hold the process's write fence for a moment.
fn step_until_closed(supervisor: &mut Supervisor<FakeSlot>) -> crate::collection::window::StepReport {
    for _ in 0..500 {
        let report = supervisor.step();
        if report.closed.is_some() {
            return report;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("the window was not handed back");
}

/// M02-T13 round 2 (REV-M02-T13 finding 1): a write under an exchange's
/// permit that passed its check before the lapse is waited for, but only up
/// to the handback deadline, so the window is handed back within the grace
/// however long the write stalls; and once the permit has lapsed no write
/// begins under it. Before this round the close waited for the exchange's
/// permit without bound.
#[test]
fn collection_window_close_hands_back_within_the_grace_while_a_write_stalls() {
    use std::sync::{Barrier, mpsc};
    let rig = Rig::new();
    let hub = Arc::new(rig.trusted_hub());
    hub.open(&rig.grant(0, 3600)).unwrap();
    let slot = FakeSlot::default();
    let mut supervisor = Supervisor::new(hub.clone(), slot.clone());
    supervisor.step();
    let permit = Arc::new(
        hub.gate()
            .permit(Provider::Legacy, PermitKind::TokenExchange)
            .unwrap(),
    );
    hub.close(CloseReason::OperatorClose);
    let closing_at = rig.clock.wall();
    assert_eq!(supervisor.step().phase, Phase::Closing);

    // A write passes its check and stalls inside its mutation.
    let started = Arc::new(Barrier::new(2));
    let (release, stalled) = mpsc::channel::<()>();
    let writer = {
        let (permit, started) = (Arc::clone(&permit), Arc::clone(&started));
        thread::spawn(move || {
            let live = || !permit.is_cancelled();
            write_under_authority(&live, || {
                started.wait();
                stalled.recv().expect("released");
            })
        })
    };
    started.wait();
    rig.clock.advance((EXCHANGE_GRACE_SECONDS - EXCHANGE_WRITE_MARGIN_SECONDS) as u64);
    let report = supervisor.step();
    assert_eq!(report.phase, Phase::Closing, "the running write is waited for");
    assert!(permit.is_cancelled(), "the permit has lapsed");
    let late = || !permit.is_cancelled();
    assert!(
        write_under_authority(&late, || ()).is_none(),
        "no write begins after the lapse"
    );
    // The handback deadline: handed back while the write still stalls.
    rig.clock.advance((EXCHANGE_WRITE_MARGIN_SECONDS - SUPERVISOR_POLL_SECONDS) as u64);
    let report = supervisor.step();
    assert_eq!(report.closed, Some(CloseReason::OperatorClose));
    assert!(rig.clock.wall() < closing_at + EXCHANGE_GRACE_SECONDS, "within the grace");
    assert!(
        write_under_authority(&late, || ()).is_none(),
        "and none after the handback"
    );
    release.send(()).expect("release");
    assert_eq!(writer.join().expect("writer"), Some(()));
}

#[test]
fn collection_window_never_extends_without_a_new_grant() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let first = rig.grant(0, 3600);
    hub.open(&first).unwrap();
    // A second grant (issued after the gap) cannot extend or replace the window.
    let second = rig
        .try_grant(&rig.request(HOST, 3600 + GRANT_GAP_SECONDS, 3600))
        .unwrap();
    assert_eq!(hub.open(&second).unwrap_err(), Refusal::WindowActive);
    assert_eq!(hub.status().deadline, Some(T0 + 3600));
    // The consumed grant never reopens.
    hub.close(CloseReason::OperatorClose);
    hub.finish_close().unwrap();
    assert_eq!(hub.open(&first).unwrap_err(), Refusal::GrantConsumed);
    // Time passes; the second, higher grant opens a new window.
    rig.clock.advance((3600 + GRANT_GAP_SECONDS) as u64);
    assert_eq!(hub.open(&second).unwrap().phase, Phase::Open);
    assert_eq!(hub.status().seq, Some(2));
}

#[test]
fn collection_window_trust_reset_kills_older_grants() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    let old = rig.grant(0, 600);
    let renewed = rig.issuer.bump_trust_epoch(&rig.key_path).unwrap();
    assert_eq!(
        hub.trust(&rig.trust).unwrap_err(),
        Refusal::TrustEpochNotNewer
    );
    hub.trust(&renewed).unwrap();
    assert_eq!(hub.open(&old).unwrap_err(), Refusal::TrustEpochMismatch);
    rig.clock.advance(1600);
    assert_eq!(hub.open(&rig.grant(0, 600)).unwrap().phase, Phase::Open);
    // Trust cannot be reset while a window runs.
    let again = rig.issuer.bump_trust_epoch(&rig.key_path).unwrap();
    assert_eq!(hub.trust(&again).unwrap_err(), Refusal::WindowActive);
}

#[test]
fn collection_window_issuer_refusals() {
    let rig = Rig::new();
    let out = rig.tmp.path().join("out.json");
    let ok = rig.request(HOST, 0, 3600);
    let check = |request: &crate::collection::window::IssueRequest, expected: Refusal| {
        assert_eq!(
            rig.issuer.issue(&rig.key_path, request, &out).unwrap_err(),
            expected
        );
        assert!(!out.exists(), "no grant written on refusal");
    };
    let mut bad = ok.clone();
    bad.approver = "H".into();
    check(&bad, Refusal::ApproverNotG);
    let mut bad = ok.clone();
    bad.approval_ref = "  ".into();
    check(&bad, Refusal::ApprovalRefMissing);
    let mut bad = ok.clone();
    bad.expires_at = bad.starts_at;
    check(&bad, Refusal::ExpiryInvalid);
    let mut bad = ok.clone();
    bad.expires_at = bad.starts_at + WINDOW_CEILING_SECONDS + 1;
    check(&bad, Refusal::LengthExceedsCeiling);
    let mut bad = ok.clone();
    bad.starts_at = T0 - 7200;
    bad.expires_at = T0 - 3600;
    check(&bad, Refusal::GrantExpired);
    let mut bad = ok.clone();
    bad.reason = "x".repeat(201);
    check(&bad, Refusal::ReasonTooLong);

    // Key problems.
    let missing = rig.tmp.path().join("nokey");
    assert_eq!(
        rig.issuer.issue(&missing, &ok, &out).unwrap_err(),
        Refusal::IssuerKeyMissing
    );
    let wide = rig.tmp.path().join("widekey");
    fs::copy(&rig.key_path, &wide).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&wide, fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert_eq!(
        rig.issuer.issue(&wide, &ok, &out).unwrap_err(),
        Refusal::IssuerKeyUnreadable
    );
    let short = rig.tmp.path().join("shortkey");
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&short)
            .unwrap();
        file.write_all(b"abcd").unwrap();
    }
    assert_eq!(
        rig.issuer.issue(&short, &ok, &out).unwrap_err(),
        Refusal::IssuerKeyLength
    );
    let other = rig.tmp.path().join("otherkey");
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&other)
            .unwrap();
        file.write_all(hex::encode([5u8; 32]).as_bytes()).unwrap();
    }
    assert_eq!(
        rig.issuer.issue(&other, &ok, &out).unwrap_err(),
        Refusal::IssuerKeyIdMismatch
    );

    // Another host's issuer; a missing, broken and unpinned ledger.
    let elsewhere = crate::collection::window::Issuer::new(
        &rig.tmp.path().join("issuer"),
        "other-host",
        rig.anchor.clone(),
        rig.clock.clone(),
    );
    assert_eq!(
        elsewhere.issue(&rig.key_path, &ok, &out).unwrap_err(),
        Refusal::IssuerWrongHost
    );
    let nowhere = crate::collection::window::Issuer::new(
        &rig.tmp.path().join("nowhere"),
        ISSUER_HOST,
        rig.anchor.clone(),
        rig.clock.clone(),
    );
    assert_eq!(
        nowhere.issue(&rig.key_path, &ok, &out).unwrap_err(),
        Refusal::IssuerLedgerMissing
    );
    rig.try_grant(&ok).unwrap();
    let ledger = rig.issuer.ledger_path();
    let text = fs::read_to_string(&ledger).unwrap();
    fs::write(&ledger, text.replace("\"seq\":1", "\"seq\":9")).unwrap();
    let later = rig.request(HOST, 6 * 3600, 600);
    assert_eq!(
        rig.issuer.issue(&rig.key_path, &later, &out).unwrap_err(),
        Refusal::IssuerLedgerBroken
    );
    fs::write(&ledger, text).unwrap();
    let fresh_anchor = Arc::new(crate::collection::window::MemoryAnchor::new());
    let unpinned = crate::collection::window::Issuer::new(
        &rig.tmp.path().join("issuer"),
        ISSUER_HOST,
        fresh_anchor,
        rig.clock.clone(),
    );
    assert_eq!(
        unpinned.issue(&rig.key_path, &later, &out).unwrap_err(),
        Refusal::IssuerPinMissing
    );
}

#[test]
fn collection_window_issuer_overlap_and_gap_across_hosts() {
    let rig = Rig::new();
    rig.try_grant(&rig.request("host-a", 0, 3600)).unwrap();
    let after = |extra: i64| rig.request("host-b", 3600 + extra, 600);
    assert_eq!(
        rig.try_grant(&rig.request("host-b", 1800, 600))
            .unwrap_err(),
        Refusal::Overlap
    );
    assert_eq!(
        rig.try_grant(&after(10 * 60)).unwrap_err(),
        Refusal::Overlap
    );
    assert_eq!(
        rig.try_grant(&after(GRANT_GAP_SECONDS - 1)).unwrap_err(),
        Refusal::Overlap
    );
    assert!(rig.try_grant(&after(GRANT_GAP_SECONDS)).is_ok());
}

#[test]
fn collection_window_issuer_concurrent_issuers_never_overlap() {
    let rig = Arc::new(Rig::new());
    let handles: Vec<_> = (0..2)
        .map(|index| {
            let rig = rig.clone();
            thread::spawn(move || {
                let issuer = crate::collection::window::Issuer::new(
                    &rig.tmp.path().join("issuer"),
                    ISSUER_HOST,
                    rig.anchor.clone(),
                    rig.clock.clone(),
                );
                let request = rig.request(&format!("host-{index}"), 0, 3600);
                let out = rig.tmp.path().join(format!("concurrent-{index}.json"));
                issuer.issue(&rig.key_path, &request, &out)
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(results.contains(&Err(Refusal::Overlap)));
}

#[test]
fn collection_window_issuer_copied_older_ledger_is_refused() {
    let rig = Rig::new();
    rig.try_grant(&rig.request("host-a", 0, 600)).unwrap();
    let older = fs::read(rig.issuer.ledger_path()).unwrap();
    rig.try_grant(&rig.request("host-a", 3600, 600)).unwrap();
    fs::write(rig.issuer.ledger_path(), older).unwrap();
    assert_eq!(
        rig.try_grant(&rig.request("host-a", 7200, 600))
            .unwrap_err(),
        Refusal::IssuerLedgerRolledBack
    );
}

#[test]
fn collection_window_issuer_failed_publication_keeps_its_reservation() {
    let rig = Rig::new();
    let request = rig.request(HOST, 0, 3600);
    let nowhere = rig.tmp.path().join("missing-dir/grant.json");
    assert_eq!(
        rig.issuer
            .issue(&rig.key_path, &request, &nowhere)
            .unwrap_err(),
        Refusal::PublicationFailed
    );
    assert_eq!(rig.try_grant(&request).unwrap_err(), Refusal::Overlap);
    rig.issuer
        .void(&rig.key_path, 1, "publication failed")
        .unwrap();
    assert!(rig.try_grant(&request).is_ok());
    // A grant that was published cannot be voided, nor can an unknown one.
    assert_eq!(
        rig.issuer.void(&rig.key_path, 2, "no").unwrap_err(),
        Refusal::VoidTarget
    );
    assert_eq!(
        rig.issuer.void(&rig.key_path, 9, "no").unwrap_err(),
        Refusal::VoidTarget
    );
}

#[test]
fn collection_window_gate_closed_by_default_and_fleet_parked() {
    let clock = Arc::new(ManualClock::new(T0));
    let gate = EgressGate::new(clock);
    for kind in [
        PermitKind::Request,
        PermitKind::Stream,
        PermitKind::TokenExchange,
    ] {
        assert_eq!(
            gate.permit(Provider::Legacy, kind).unwrap_err().code(),
            "window_closed"
        );
        assert_eq!(
            gate.permit(Provider::Fleet, kind).unwrap_err().code(),
            "window_closed"
        );
    }
    assert!(!EgressGate::process_wide().is_open());
    assert!(!gate.exchange_in_flight());
}

#[test]
fn collection_window_supervisor_keeps_api_up_and_restarts_with_backoff() {
    let rig = Rig::new();
    let hub = Arc::new(rig.trusted_hub());
    let slot = FakeSlot::default();
    let mut supervisor = Supervisor::new(hub.clone(), slot.clone());
    // Closed: serving only, no collector.
    let report = supervisor.step();
    assert_eq!(
        (report.phase, report.collector_running),
        (Phase::Closed, false)
    );
    // Pending: still no collector.
    hub.open(&rig.grant(600, 3600)).unwrap();
    let report = supervisor.step();
    assert_eq!(
        (report.phase, report.collector_running),
        (Phase::Pending, false)
    );
    assert_eq!(slot.log.lock().unwrap().starts, 0);
    // Open: the collector starts within one step.
    rig.clock.advance(600);
    let report = supervisor.step();
    assert_eq!(
        (report.phase, report.collector_running),
        (Phase::Open, true)
    );
    // The collector fails: it is restarted after the backoff, and the gap is recorded.
    slot.log.lock().unwrap().running = false;
    slot.log.lock().unwrap().fail_starts = 2;
    rig.clock.advance(1);
    assert!(!supervisor.step().collector_running);
    rig.clock.advance(10);
    assert!(!supervisor.step().collector_running);
    rig.clock.advance(30);
    assert!(supervisor.step().collector_running);
    assert_eq!(
        hub.status().phase,
        Phase::Open,
        "a collector failure never ends the service"
    );
    hub.close(CloseReason::OperatorClose);
    let report = supervisor.step();
    assert_eq!(report.closed, Some(CloseReason::OperatorClose));
    let handback = hub.status().last_handback.unwrap();
    assert!(!handback.gaps.is_empty());
    assert_eq!(
        handback.coverage_seconds,
        (handback.closed_at - handback.opened_at.unwrap())
            - handback.gaps.iter().map(|g| g.end - g.start).sum::<i64>()
    );
}

#[test]
fn collection_window_time_offset_is_measured_at_open_and_every_five_minutes() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    hub.open(&rig.grant(0, 3600)).unwrap();
    assert_eq!(rig.probe.calls(), 1);
    rig.clock.advance(120);
    hub.tick().unwrap();
    assert_eq!(rig.probe.calls(), 1);
    rig.clock.advance(200);
    hub.tick().unwrap();
    assert_eq!(rig.probe.calls(), 2);
    // An offset over the limit closes the window; so does a failed measurement.
    rig.probe.set(Ok(31.0));
    rig.clock.advance(301);
    assert_eq!(hub.tick().unwrap(), Phase::Closing);
    assert_eq!(hub.status().close_reason, Some(CloseReason::ClockInvalid));

    let rig = Rig::new();
    let hub = rig.trusted_hub();
    hub.open(&rig.grant(0, 3600)).unwrap();
    rig.probe.set(Err(Refusal::ClockOffset));
    rig.clock.advance(301);
    assert_eq!(hub.tick().unwrap(), Phase::Closing);
    let _ = ManualProbe::new(0.0);
}

#[test]
fn collection_window_sntp_offset_arithmetic() {
    let sent = 1_800_000_000.0;
    let reply = |skew: f64, mode: u8, origin_ok: bool| {
        let mut packet = [0u8; 48];
        packet[0] = (4 << 3) | mode;
        packet[1] = 2;
        let request = sntp_request(sent);
        if origin_ok {
            packet[24..32].copy_from_slice(&request[40..48]);
        }
        for (range, seconds) in [(32..40, sent + skew + 0.05), (40..48, sent + skew + 0.06)] {
            let ntp = seconds + 2_208_988_800.0;
            let whole = ntp.floor();
            packet[range.start..range.start + 4].copy_from_slice(&(whole as u32).to_be_bytes());
            packet[range.start + 4..range.end]
                .copy_from_slice(&(((ntp - whole) * 4_294_967_296.0) as u32).to_be_bytes());
        }
        packet
    };
    let offset = sntp_offset(sent, &reply(10.0, 4, true), sent + 0.11).unwrap();
    assert!((offset - 10.0).abs() < 0.01, "offset {offset}");
    assert!(sntp_offset(sent, &reply(0.0, 3, true), sent + 0.1).is_err());
    assert!(sntp_offset(sent, &reply(0.0, 4, false), sent + 0.1).is_err());
    assert!(sntp_offset(sent, &[0u8; 10], sent + 0.1).is_err());
}

#[test]
fn collection_window_trust_document_round_trip() {
    let rig = Rig::new();
    let bytes = rig.trust.to_bytes().unwrap();
    assert_eq!(TrustDocument::parse(&bytes).unwrap(), rig.trust);
    let mut bad = rig.trust.clone();
    bad.issuer_key_id = "ed25519-sha256-00".into();
    assert_eq!(
        TrustDocument::parse(&bad.to_bytes().unwrap()).unwrap_err(),
        Refusal::TrustInvalid
    );
}

/// Finding 5: a reservation whose grant may have been published (a crash
/// after the rename and before `issued`, or a failed directory sync after the
/// rename) cannot be voided, so no overlapping grant can follow it.
#[test]
fn collection_window_issuer_possibly_published_reservation_is_not_voidable() {
    use crate::collection::window::issuer::PublishFault;

    // A crash after the rename, before `issued` is appended.
    let rig = Rig::new();
    let request = rig.request(HOST, 0, 3600);
    let out = rig.tmp.path().join("published.json");
    rig.issuer.inject(PublishFault::CrashAfterRename);
    assert_eq!(
        rig.issuer.issue(&rig.key_path, &request, &out).unwrap_err(),
        Refusal::PublicationFailed
    );
    let published = fs::read(&out).expect("the grant is in place");
    assert_eq!(SignedGrant::parse(&published).unwrap().body.seq, 1);
    assert_eq!(
        rig.issuer.void(&rig.key_path, 1, "crash").unwrap_err(),
        Refusal::PossiblyPublished
    );
    assert_eq!(
        rig.try_grant(&rig.request("host-b", 0, 3600)).unwrap_err(),
        Refusal::Overlap
    );
    // It is a valid grant for its host, so the interval had to stay blocked.
    let hub = rig.trusted_hub();
    assert_eq!(hub.open(&published).unwrap().phase, Phase::Open);

    // A failed directory sync after the rename.
    let rig = Rig::new();
    let request = rig.request(HOST, 0, 3600);
    let out = rig.tmp.path().join("published.json");
    rig.issuer.inject(PublishFault::DirSyncFails);
    assert_eq!(
        rig.issuer.issue(&rig.key_path, &request, &out).unwrap_err(),
        Refusal::PublicationFailed
    );
    assert!(out.exists());
    assert!(rig.issuer.void(&rig.key_path, 1, "sync").is_err());
    assert_eq!(
        rig.try_grant(&rig.request("host-b", 0, 3600)).unwrap_err(),
        Refusal::Overlap
    );
}

/// Finding 6: once the gate sees a deadline reached it latches closed and
/// cancels cancellable permits; stepping the clock back before any
/// supervisor sweep cannot reopen it, and the next tick closes `expired`.
#[test]
fn collection_window_observed_expiry_latches_closed_against_rollback() {
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    hub.open(&rig.grant(0, 3600)).unwrap();
    let gate = hub.gate().clone();
    let stream = gate.permit(Provider::Legacy, PermitKind::Stream).unwrap();
    let exchange = gate
        .permit(Provider::Legacy, PermitKind::TokenExchange)
        .unwrap();
    // Wall time crosses the expiry while monotonic time does not.
    rig.clock.shift_wall(3600);
    assert_eq!(
        gate.permit(Provider::Legacy, PermitKind::Request)
            .unwrap_err(),
        GateRefusal::WindowClosed
    );
    assert!(stream.is_cancelled(), "streams are cancelled at once");
    assert!(!exchange.is_cancelled(), "an exchange keeps its grace");
    assert!(!gate.is_open());
    assert_eq!(gate.expiry_observed(), Some(T0 + 3600));
    // The wall clock moves back before any supervisor sweep.
    rig.clock.shift_wall(-3600);
    for kind in [
        PermitKind::Request,
        PermitKind::Stream,
        PermitKind::TokenExchange,
    ] {
        assert_eq!(
            gate.permit(Provider::Legacy, kind).unwrap_err(),
            GateRefusal::WindowClosed,
            "{kind:?}"
        );
    }
    assert_eq!(hub.tick().unwrap(), Phase::Closing);
    assert_eq!(hub.status().close_reason, Some(CloseReason::Expired));
    assert!(gate.permit(Provider::Legacy, PermitKind::Request).is_err());
    drop((stream, exchange));
    let handback = hub.finish_close().unwrap().unwrap();
    assert_eq!(handback.closed_at, T0 + 3600, "closed when expiry was seen");

    // The supervisor's sweep latches the same way.
    let rig = Rig::new();
    let hub = rig.trusted_hub();
    hub.open(&rig.grant(0, 3600)).unwrap();
    let stream = hub
        .gate()
        .permit(Provider::Legacy, PermitKind::Stream)
        .unwrap();
    rig.clock.shift_wall(3600);
    assert_eq!(hub.tick().unwrap(), Phase::Closing);
    assert!(stream.is_cancelled());
    rig.clock.shift_wall(-3600);
    assert!(
        hub.gate()
            .permit(Provider::Legacy, PermitKind::Request)
            .is_err()
    );
}
