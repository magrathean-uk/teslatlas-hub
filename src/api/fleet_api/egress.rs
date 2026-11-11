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
//! In production every send asks the process-wide gate, which is closed until
//! the window controller opens it. Unit tests talk to loopback doubles that have
//! no window: a test build asks the gate a test routed to the endpoint's host
//! and port ([`test_support::route`]) and sends without one otherwise.

use std::future::Future;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::collection::window::{EgressGate, Permit};
pub(crate) use crate::collection::window::{GateRefusal, PermitKind, Provider};

/// Why a permitted send was cut short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cut {
    /// The window closed and cancelled the permit.
    Closed,
    /// The time the gate left the permit ran out.
    Deadline,
}

/// Permission for one send, held until the send ends.
#[derive(Debug)]
pub(crate) struct SendPermit {
    cancel: CancellationToken,
    /// When the send must have ended; `None` only for a test send to a double
    /// that no test routed through a gate.
    deadline: Option<Instant>,
    /// The gate's permit: it keeps a token exchange counted as in flight.
    _permit: Option<Permit>,
}

impl SendPermit {
    fn granted(permit: Permit) -> Self {
        Self {
            cancel: permit.cancel_token().clone(),
            deadline: Some(Instant::now() + permit.time_left()),
            _permit: Some(permit),
        }
    }

    #[cfg(test)]
    fn unrestricted() -> Self {
        Self {
            cancel: CancellationToken::new(),
            deadline: None,
            _permit: None,
        }
    }

    /// Resolves when the window closes or the time left runs out; never for an
    /// unrestricted test send.
    pub(crate) async fn ended(&self) -> Cut {
        let deadline = self.deadline;
        let expired = async move {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Cut::Closed,
            () = expired => Cut::Deadline,
        }
    }

    /// Runs `send` until it ends or the permit does (see [`Self::ended`]).
    /// Dropping the send closes its connection.
    pub(crate) async fn bound<F: Future>(&self, send: F) -> Result<F::Output, Cut> {
        tokio::select! {
            biased;
            cut = self.ended() => Err(cut),
            output = send => Ok(output),
        }
    }

    /// The time left before the send must have ended.
    #[cfg(test)]
    pub(crate) fn time_left(&self) -> Option<std::time::Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
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
        Some(gate) => gate.permit(provider, kind).map(SendPermit::granted),
        #[cfg(test)]
        None => Ok(SendPermit::unrestricted()),
        #[cfg(not(test))]
        None => Err(GateRefusal::WindowClosed),
    }
}

#[cfg(not(test))]
fn gate_for(_endpoint: &str) -> Option<EgressGate> {
    Some(EgressGate::process_wide().clone())
}

#[cfg(test)]
fn gate_for(endpoint: &str) -> Option<EgressGate> {
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

    const T0: i64 = 1_800_000_000;
    const HOST: &str = "host-egress-test";

    static ROUTES: Mutex<BTreeMap<String, EgressGate>> = Mutex::new(BTreeMap::new());

    fn key(endpoint: &str) -> Option<String> {
        let url = url::Url::parse(endpoint).ok()?;
        Some(format!(
            "{}:{}",
            url.host_str()?.to_ascii_lowercase(),
            url.port_or_known_default()?
        ))
    }

    pub(super) fn routed(endpoint: &str) -> Option<EgressGate> {
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

    pub(crate) fn route(endpoint: &str, gate: EgressGate) -> Route {
        let key = key(endpoint).expect("a routable endpoint");
        let previous = ROUTES.lock().expect("routes").insert(key.clone(), gate);
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
            route(endpoint, self.gate())
        }

        /// Moves the window's clock on; the gate sees the deadline at the next ask.
        pub(crate) fn advance(&self, seconds: u64) {
            self.clock.advance(seconds);
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
