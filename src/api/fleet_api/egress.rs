// SPDX-License-Identifier: AGPL-3.0-only

//! The send boundary of every Tesla request (M02-T09; window design section 4).
//!
//! Legacy token exchanges, Owner API reads, Fleet requests and the streaming
//! connection all ask here immediately before they send, and here asks the
//! window's egress gate (`collection::window::gate`), which is the only
//! authority. A refusal (`window_closed`, `provider_parked`) means nothing is
//! sent and no connection is opened; the caller never retries it. A permit
//! bounds its send: the send ends when the time the gate left it runs out and,
//! for a request or a stream, when the window closes and cancels the permit.
//!
//! A permit also keeps its absolute deadlines on the gate's own clock (wall
//! and monotonic) and re-checks them before every send it bounds
//! ([`SendPermit::check`]) and, while held, at least every
//! [`AUTHORITY_RECHECK`]. The runtime timer alone does not count a host sleep
//! and does not see a forward wall-clock step, so without that re-check a held
//! permit could outlive the window until a supervisor sweep cancelled it.
//!
//! In production every send asks the process-wide gate, which is closed until
//! the window controller opens it. Unit tests talk to loopback doubles that have
//! no window: a test build asks the gate a test routed to the endpoint's host
//! and port ([`test_support::route`]) and sends without one otherwise.

use std::{
    future::Future,
    sync::{Arc, OnceLock},
    time::Duration,
};

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::collection::window::{EgressGate, Permit, SystemClock, WindowClock};
pub(crate) use crate::collection::window::{GateRefusal, PermitKind, Provider};

/// Why a permitted send was cut short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cut {
    /// The window closed and cancelled the permit.
    Closed,
    /// The time the gate left the permit ran out.
    Deadline,
}

/// How often a held permit re-reads the gate's clock for its absolute
/// deadlines, so that a host sleep or a forward wall-clock step across the
/// deadline ends it within this much running time.
pub(crate) const AUTHORITY_RECHECK: Duration = Duration::from_secs(1);

/// The permit's absolute deadlines, on the clock of the gate that granted it.
#[derive(Debug)]
struct Authority {
    clock: Arc<dyn WindowClock>,
    deadline_wall: i64,
    deadline_mono: u64,
}

impl Authority {
    /// `wall` and `mono` were read before the gate was asked, so the deadlines
    /// fall no later than the gate's own.
    fn new(clock: Arc<dyn WindowClock>, wall: i64, mono: u64, time_left: Duration) -> Self {
        let left = time_left.as_secs();
        Self {
            clock,
            deadline_wall: wall.saturating_add(i64::try_from(left).unwrap_or(i64::MAX)),
            deadline_mono: mono.saturating_add(left),
        }
    }

    fn expired(&self) -> bool {
        self.clock.wall() >= self.deadline_wall || self.clock.mono() >= self.deadline_mono
    }
}

/// Permission for one send, held until the send ends.
#[derive(Debug)]
pub(crate) struct SendPermit {
    cancel: CancellationToken,
    /// When the send must have ended on the runtime timer; `None` only for a
    /// test send to a double that no test routed through a gate.
    deadline: Option<Instant>,
    /// The absolute deadlines; `None` exactly when `deadline` is.
    authority: Option<Authority>,
    /// The gate's permit: it keeps a token exchange counted as in flight.
    _permit: Option<Permit>,
}

impl SendPermit {
    fn granted(permit: Permit, authority: Authority) -> Self {
        Self {
            cancel: permit.cancel_token().clone(),
            deadline: Some(Instant::now() + permit.time_left()),
            authority: Some(authority),
            _permit: Some(permit),
        }
    }

    #[cfg(test)]
    fn unrestricted() -> Self {
        Self {
            cancel: CancellationToken::new(),
            deadline: None,
            authority: None,
            _permit: None,
        }
    }

    /// Whether the permit still allows a send now: the window has not closed,
    /// the runtime timer has not run out and neither absolute deadline has
    /// passed on the gate's clock. Called before every network transition of a
    /// held permit (connect, subscribe, Ping, send).
    pub(crate) fn check(&self) -> Result<(), Cut> {
        if self.cancel.is_cancelled() {
            return Err(Cut::Closed);
        }
        if self.deadline.is_some_and(|deadline| Instant::now() >= deadline)
            || self.authority.as_ref().is_some_and(Authority::expired)
        {
            return Err(Cut::Deadline);
        }
        Ok(())
    }

    /// Resolves when the window closes, the time left runs out or an absolute
    /// deadline is found passed (checked at once and every
    /// [`AUTHORITY_RECHECK`]); never for an unrestricted test send.
    pub(crate) async fn ended(&self) -> Cut {
        let deadline = self.deadline;
        let expired = async move {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        let lapsed = async {
            match &self.authority {
                Some(authority) => {
                    while !authority.expired() {
                        tokio::time::sleep(AUTHORITY_RECHECK).await;
                    }
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Cut::Closed,
            () = expired => Cut::Deadline,
            () = lapsed => Cut::Deadline,
        }
    }

    /// Runs `send` until it ends or the permit does (see [`Self::ended`]). The
    /// permit is checked first, so a permit that has already ended sends
    /// nothing. Dropping the send closes its connection.
    pub(crate) async fn bound<F: Future>(&self, send: F) -> Result<F::Output, Cut> {
        self.check()?;
        tokio::select! {
            biased;
            cut = self.ended() => Err(cut),
            output = send => Ok(output),
        }
    }
}

impl Drop for SendPermit {
    /// The send is over: its token is cancelled so the gate stops tracking it.
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Asks the window for permission to send to `endpoint`. Called immediately
/// before every Tesla send, after every local check, so that a refusal opens
/// no connection.
pub(crate) fn ask(
    endpoint: &str,
    provider: Provider,
    kind: PermitKind,
) -> Result<SendPermit, GateRefusal> {
    match gate_for(endpoint) {
        Some((gate, clock)) => {
            let (wall, mono) = (clock.wall(), clock.mono());
            let permit = gate.permit(provider, kind)?;
            let authority = Authority::new(clock, wall, mono, permit.time_left());
            Ok(SendPermit::granted(permit, authority))
        }
        #[cfg(test)]
        None => Ok(SendPermit::unrestricted()),
        #[cfg(not(test))]
        None => Err(GateRefusal::WindowClosed),
    }
}

/// The system clock, as the process-wide gate reads it (wall time and a
/// monotonic reading). Deadlines are taken relative to this one's own readings.
fn system_clock() -> Arc<dyn WindowClock> {
    static CLOCK: OnceLock<Arc<dyn WindowClock>> = OnceLock::new();
    CLOCK.get_or_init(|| Arc::new(SystemClock::new())).clone()
}

#[cfg(not(test))]
fn gate_for(_endpoint: &str) -> Option<(EgressGate, Arc<dyn WindowClock>)> {
    Some((EgressGate::process_wide().clone(), system_clock()))
}

#[cfg(test)]
fn gate_for(endpoint: &str) -> Option<(EgressGate, Arc<dyn WindowClock>)> {
    test_support::routed(endpoint)
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Real windows for the egress tests: a trusted window controller on a
    //! manual clock, opened with a grant from a real issuer, whose gate is
    //! routed to one test endpoint.

    use std::{
        collections::BTreeMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use tempfile::TempDir;
    use tokio::{io::AsyncReadExt, net::TcpListener, task::JoinHandle};

    use crate::collection::window::{
        CloseReason, EgressGate, IssueRequest, Issuer, ManualClock, ManualProbe, MemoryAnchor,
        NoHighWater, Phase, WindowClock, WindowConfig, WindowController, WindowDeps,
    };

    type Routed = (EgressGate, Arc<dyn WindowClock>);

    const T0: i64 = 1_800_000_000;
    const HOST: &str = "host-egress-test";

    static ROUTES: Mutex<BTreeMap<String, Routed>> = Mutex::new(BTreeMap::new());

    fn key(endpoint: &str) -> Option<String> {
        let url = url::Url::parse(endpoint).ok()?;
        Some(format!(
            "{}:{}",
            url.host_str()?.to_ascii_lowercase(),
            url.port_or_known_default()?
        ))
    }

    pub(super) fn routed(endpoint: &str) -> Option<Routed> {
        let key = key(endpoint)?;
        ROUTES.lock().expect("routes").get(&key).cloned()
    }

    /// Sends to the endpoint's host and port ask `gate` while this lives.
    #[must_use = "the route ends when it is dropped"]
    pub(crate) struct Route(String);

    impl Drop for Route {
        fn drop(&mut self) {
            ROUTES.lock().expect("routes").remove(&self.0);
        }
    }

    /// Sends to the endpoint ask `gate`, which reads the system clock.
    pub(crate) fn route(endpoint: &str, gate: EgressGate) -> Route {
        route_on(endpoint, gate, super::system_clock())
    }

    /// Sends to the endpoint ask `gate`, which reads `clock`.
    pub(crate) fn route_on(endpoint: &str, gate: EgressGate, clock: Arc<dyn WindowClock>) -> Route {
        let key = key(endpoint).expect("a routable endpoint");
        let previous = ROUTES
            .lock()
            .expect("routes")
            .insert(key.clone(), (gate, clock));
        assert!(
            previous.is_none(),
            "{key} is already routed by another test"
        );
        Route(key)
    }

    /// A Hub-side window controller with its own issuer, on a manual clock.
    pub(crate) struct TestWindow {
        pub(crate) clock: Arc<ManualClock>,
        pub(crate) hub: WindowController,
        issuer: Issuer,
        key_path: std::path::PathBuf,
        tmp: TempDir,
    }

    impl TestWindow {
        /// A trusted Hub with no window open: its gate is closed.
        pub(crate) fn closed() -> Self {
            let tmp = TempDir::new().expect("temp dir");
            let clock = Arc::new(ManualClock::new(T0));
            let anchor = Arc::new(MemoryAnchor::new());
            let issuer = Issuer::new(
                &tmp.path().join("issuer"),
                "host-egress-issuer",
                anchor.clone(),
                clock.clone(),
            );
            let key_path = tmp.path().join("issuer.key");
            let trust = issuer.init(&key_path).expect("issuer init");
            let (hub, _) = WindowController::start(
                WindowConfig {
                    dir: tmp.path().join("data/window"),
                    claim_path: tmp.path().join("claim/host-claim.lock"),
                    host_id: HOST.to_owned(),
                },
                WindowDeps {
                    clock: clock.clone(),
                    anchor,
                    probe: Arc::new(ManualProbe::new(0.2)),
                    gate: EgressGate::new(clock.clone()),
                    handback: Arc::new(NoHighWater),
                },
            );
            hub.trust(&trust).expect("trust");
            Self {
                clock,
                hub,
                issuer,
                key_path,
                tmp,
            }
        }

        /// A window open from now for `length` seconds.
        pub(crate) fn open(length: i64) -> Self {
            let window = Self::closed();
            let now = window.clock.wall();
            let request = IssueRequest {
                host_id: HOST.to_owned(),
                starts_at: now,
                expires_at: now + length,
                approver: "G".to_owned(),
                reason: "egress test window".to_owned(),
                approval_ref: "approval-egress-test".to_owned(),
            };
            let out = window.tmp.path().join("grant.json");
            window
                .issuer
                .issue(&window.key_path, &request, &out)
                .expect("grant issued");
            let grant = std::fs::read(out).expect("grant file");
            assert_eq!(window.hub.open(&grant).expect("open").phase, Phase::Open);
            window
        }

        pub(crate) fn gate(&self) -> EgressGate {
            self.hub.gate().clone()
        }

        /// Routes `endpoint` through this window's gate.
        pub(crate) fn route(&self, endpoint: &str) -> Route {
            route_on(endpoint, self.gate(), self.clock.clone())
        }

        /// Moves the window's clock on; the gate sees the deadline at the next ask.
        /// The runtime clock does not move, as across a host sleep, which the
        /// wall and the sleep-counting monotonic clock count and the runtime
        /// timer does not.
        pub(crate) fn advance(&self, seconds: u64) {
            self.clock.advance(seconds);
        }

        /// Steps only the window's wall clock forward (an operator or NTP step).
        pub(crate) fn step_wall(&self, seconds: i64) {
            self.clock.shift_wall(seconds);
        }

        /// The operator closes the window.
        pub(crate) fn close(&self) {
            self.hub.close(CloseReason::OperatorClose);
        }
    }

    /// A loopback TCP server that counts the connections it accepts, reads
    /// what they send and never answers.
    pub(crate) struct SilentServer {
        pub(crate) url: String,
        accepted: Arc<AtomicUsize>,
        received: Arc<AtomicUsize>,
        task: JoinHandle<()>,
    }

    impl SilentServer {
        pub(crate) async fn spawn(scheme: &str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let port = listener.local_addr().expect("address").port();
            let accepted = Arc::new(AtomicUsize::new(0));
            let received = Arc::new(AtomicUsize::new(0));
            let task = {
                let accepted = accepted.clone();
                let received = received.clone();
                tokio::spawn(async move {
                    while let Ok((mut socket, _)) = listener.accept().await {
                        accepted.fetch_add(1, Ordering::SeqCst);
                        let received = received.clone();
                        tokio::spawn(async move {
                            let mut buffer = [0_u8; 4096];
                            while let Ok(read) = socket.read(&mut buffer).await {
                                if read == 0 {
                                    break;
                                }
                                received.fetch_add(read, Ordering::SeqCst);
                            }
                        });
                    }
                })
            };
            Self {
                url: format!("{scheme}://127.0.0.1:{port}"),
                accepted,
                received,
                task,
            }
        }

        pub(crate) fn connections(&self) -> usize {
            self.accepted.load(Ordering::SeqCst)
        }

        pub(crate) fn bytes_received(&self) -> usize {
            self.received.load(Ordering::SeqCst)
        }
    }

    impl Drop for SilentServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// Waits in real time, holding a paused clock back, until `condition`
    /// holds (at most about 10 s).
    pub(crate) async fn eventually(mut condition: impl FnMut() -> bool) {
        for _ in 0..2_000 {
            if condition() {
                return;
            }
            tokio::task::spawn_blocking(|| std::thread::sleep(std::time::Duration::from_millis(5)))
                .await
                .expect("pause");
        }
        panic!("the condition never held");
    }
}
