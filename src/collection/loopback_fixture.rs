//! Synthetic loopback issuer, Owner API and stream peer for retained journeys.
use std::sync::{Arc,Mutex,atomic::{AtomicBool,AtomicI64,AtomicUsize,Ordering}};
use futures_util::{SinkExt,StreamExt};
use serde_json::json;
pub(crate) const VEHICLE_ID: i64 = 12345;
pub(crate) const STREAM_ID: i64 = 4242;
pub(crate) const VIN: &str = "5YJ3E1EA7KF000001";
pub(crate) const ACCESS_TOKEN: &str = "loopback-access-0002";
pub(crate) const REFRESH_TOKEN: &str = "loopback-refresh-0002";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdvanceMode {
    Manual,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScenarioStep {
    AsleepDiscovery,
    AsleepRestart,
    ChargeSamples,
    DrivePositions,
    OfflineDiscovery,
    OnlineIdle,
    OnlineSoftwareUpdate,
    Parked,
    UnchangedNoOp,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamScript {
    Normal,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamAuditEvent {
    Subscribe,
    Unsubscribe,
}
#[derive(Clone)]
pub(crate) struct RequestObservation {
    pub(crate) path: String,
    pub(crate) rejected: bool,
    pub(crate) scenario_step: ScenarioStep,
}
#[derive(Clone)]
pub(crate) struct StreamObservation {
    pub(crate) event: StreamAuditEvent,
    pub(crate) accepted: bool,
}
pub(crate) struct SessionObservation {
    pub(crate) active_sessions: usize,
    pub(crate) max_concurrent_sessions: usize,
    pub(crate) connection_attempts: usize,
}
pub(crate) struct FakeTeslaSource {
    state: Arc<FixtureState>,
    base: url::Url,
    stream_url: String,
    http: tokio::task::JoinHandle<()>,
    stream: tokio::task::JoinHandle<()>,
}
struct FixtureState {
    step: Mutex<ScenarioStep>, base_ms: AtomicI64, sample: AtomicI64,
    streams: AtomicBool, owner: AtomicBool, refreshes: AtomicUsize,
    requests: Mutex<Vec<RequestObservation>>, events: Mutex<Vec<StreamObservation>>,
    active: AtomicUsize, maximum: AtomicUsize, attempts: AtomicUsize,
}
impl std::fmt::Debug for FakeTeslaSource {
    fn fmt(&self,f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("FakeTeslaSource([loopback])") }
}
impl FakeTeslaSource {
    pub(crate) async fn spawn_canonical(_mode: AdvanceMode) -> Result<Self, std::io::Error> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        let state = Arc::new(FixtureState { step: Mutex::new(ScenarioStep::AsleepDiscovery),base_ms: AtomicI64::new(now-60_000),sample: AtomicI64::new(0),streams: AtomicBool::new(true),owner: AtomicBool::new(true),refreshes: AtomicUsize::new(0),requests: Mutex::new(Vec::new()),events: Mutex::new(Vec::new()),active: AtomicUsize::new(0),maximum: AtomicUsize::new(0),attempts: AtomicUsize::new(0) });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = url::Url::parse(&format!("http://{}/",listener.local_addr()?)).expect("loopback URL");
        let router = axum::Router::new().fallback(fixture_http).with_state(Arc::clone(&state));
        let http = tokio::spawn(async move { let _ = axum::serve(listener,router).await; });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let stream_url = format!("ws://{}/streaming/",listener.local_addr()?);
        let shared = Arc::clone(&state);
        let stream = tokio::spawn(async move {
            let mut sessions = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => { let Ok((socket,_)) = accepted else { break; }; let state = Arc::clone(&shared); sessions.spawn(async move { fixture_stream(socket,state).await; }); }
                    _ = sessions.join_next(), if !sessions.is_empty() => (),
                }
            }
        });
        Ok(Self { state,base,stream_url,http,stream })
    }
    pub(crate) fn oauth_issuer_url(&self) -> url::Url {
        self.base.clone()
    }
    pub(crate) fn http_base_url(&self) -> &url::Url {
        &self.base
    }
    pub(crate) fn stream_endpoint(&self) -> &str {
        &self.stream_url
    }
    pub(crate) fn token_refresh_request_count(&self) -> usize {
        self.state.refreshes.load(Ordering::SeqCst)
    }
    pub(crate) fn audited_requests(&self) -> Vec<RequestObservation> {
        self.state.requests.lock().unwrap().clone()
    }
    pub(crate) fn audited_stream_events(&self) -> Vec<StreamObservation> {
        self.state.events.lock().unwrap().clone()
    }
    pub(crate) fn stream_session_stats(&self) -> SessionObservation {
        SessionObservation { active_sessions: self.state.active.load(Ordering::SeqCst),max_concurrent_sessions: self.state.maximum.load(Ordering::SeqCst),connection_attempts: self.state.attempts.load(Ordering::SeqCst) }
    }
    pub(crate) fn set_step(&self, step: ScenarioStep) {
        *self.state.step.lock().unwrap() = step; self.state.sample.store(0,Ordering::SeqCst);
    }
    pub(crate) fn current_step(&self) -> ScenarioStep {
        *self.state.step.lock().unwrap()
    }
    pub(crate) fn advance(&self) {
        let steps = [ScenarioStep::AsleepDiscovery,ScenarioStep::OfflineDiscovery,ScenarioStep::OnlineIdle,ScenarioStep::DrivePositions,ScenarioStep::Parked,ScenarioStep::ChargeSamples,ScenarioStep::AsleepRestart,ScenarioStep::OnlineSoftwareUpdate,ScenarioStep::UnchangedNoOp];
        let at = steps.iter().position(|step| *step == self.current_step()).expect("scenario step"); self.set_step(steps[(at+1).min(steps.len()-1)]);
    }
    pub(crate) fn set_base_ts_ms(&self, time: i64) {
        self.state.base_ms.store(time,Ordering::SeqCst); self.state.sample.store(0,Ordering::SeqCst);
    }
    pub(crate) fn set_stream_available(&self, available: bool) {
        self.state.streams.store(available,Ordering::SeqCst);
    }
    pub(crate) fn set_owner_vehicle_data_available(&self, available: bool) {
        self.state.owner.store(available,Ordering::SeqCst);
    }
    pub(crate) fn set_stream_script(&self, _script: StreamScript) {
    }
    pub(crate) fn assert_no_wake_or_command(&self) -> Result<(), String> {
        if self.audited_requests().iter().any(|request| request.path.contains("wake_up") || request.path.contains("/command/")) { Err("vehicle action reached fixture".into()) } else { Ok(()) }
    }
    pub(crate) async fn shutdown(self) {
        self.http.abort(); self.stream.abort(); let _ = self.http.await; let _ = self.stream.await;
    }
}

async fn fixture_http(axum::extract::State(state): axum::extract::State<Arc<FixtureState>>, uri: axum::http::Uri) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    use axum::http::StatusCode;
    let path = uri.path(); let step = *state.step.lock().unwrap();
    let asleep = matches!(step,ScenarioStep::AsleepDiscovery | ScenarioStep::AsleepRestart);
    let offline = step == ScenarioStep::OfflineDiscovery;
    let action = path.contains("wake_up") || path.contains("/command/");
    let rejected = action || (path.ends_with("vehicle_data") && (asleep || offline || !state.owner.load(Ordering::SeqCst)));
    state.requests.lock().unwrap().push(RequestObservation {path:path.into(),rejected,scenario_step:step});
    if rejected { return (StatusCode::SERVICE_UNAVAILABLE,axum::Json(json!({"error":"fixture unavailable"}))); }
    if path.ends_with("token") {
        state.refreshes.fetch_add(1,Ordering::SeqCst);
        return (StatusCode::OK,axum::Json(json!({"access_token":ACCESS_TOKEN,"refresh_token":REFRESH_TOKEN,"expires_in":28800,"token_type":"Bearer"})));
    }
    let vehicle_state = if asleep { "asleep" } else if offline { "offline" } else { "online" };
    let mut vehicle = json!({"id":VEHICLE_ID,"id_s":VEHICLE_ID.to_string(),"vehicle_id":STREAM_ID,"vin":VIN,"display_name":"Synthetic vehicle","state":vehicle_state,"in_service":false});
    if path.ends_with("products") || path.ends_with("vehicles") { return (StatusCode::OK,axum::Json(json!({"response":[vehicle],"count":1}))); }
    if path.ends_with("vehicle_data") {
        let sample = state.sample.fetch_add(1,Ordering::SeqCst);
        let timestamp = state.base_ms.load(Ordering::SeqCst) + sample*1000;
        let driving = step == ScenarioStep::DrivePositions;
        let charging = step == ScenarioStep::ChargeSamples;
        vehicle["drive_state"] = json!({"timestamp":timestamp,"gps_as_of":timestamp/1000,"latitude":51.5 + sample as f64*0.0001,"longitude":-0.12,"heading":180,"speed":if driving {30}else{0},"power":if driving {15}else{0},"shift_state":if driving {"D"}else{"P"}});
        vehicle["charge_state"] = json!({"timestamp":timestamp,"battery_level":if charging {40+sample.min(40)}else{80},"usable_battery_level":80,"battery_range":240.0,"est_battery_range":230.0,"ideal_battery_range":250.0,"charging_state":if charging {"Charging"}else{"Disconnected"},"charger_power":if charging {7}else{0},"charge_energy_added":if charging {sample as f64*0.1}else{0.0}});
        vehicle["climate_state"] = json!({"timestamp":timestamp,"inside_temp":20.0,"outside_temp":15.0,"is_climate_on":false});
        vehicle["vehicle_state"] = json!({"timestamp":timestamp,"odometer":12345.0+sample as f64*0.01,"car_version":"2026.1","locked":true,"software_update":{"status":if step == ScenarioStep::OnlineSoftwareUpdate {"installing"}else{""},"version":"2026.2"}});
        vehicle["vehicle_config"] = json!({"car_type":"model3"});
        return (StatusCode::OK,axum::Json(json!({"response":vehicle})));
    }
    (StatusCode::NOT_FOUND,axum::Json(json!({"error":"fixture route"})))
}

async fn fixture_stream(socket: tokio::net::TcpStream, state: Arc<FixtureState>) {
    state.attempts.fetch_add(1,Ordering::SeqCst);
    if !state.streams.load(Ordering::SeqCst) { return; }
    let Ok(mut socket) = tokio_tungstenite::accept_async(socket).await else { return; };
    struct Active(Arc<FixtureState>);
    impl Drop for Active { fn drop(&mut self) { self.0.active.fetch_sub(1,Ordering::SeqCst); } }
    let active = state.active.fetch_add(1,Ordering::SeqCst)+1; state.maximum.fetch_max(active,Ordering::SeqCst); let _active = Active(Arc::clone(&state));
    use tokio_tungstenite::tungstenite::Message;
    if socket.send(Message::Text(json!({"msg_type":"control:hello","code":0,"connection_timeout":30000}).to_string().into())).await.is_err() { return; }
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = interval.tick() => { if !state.streams.load(Ordering::SeqCst) { let _ = socket.close(None).await; break; } }
            message = socket.next() => {
                let Some(Ok(message)) = message else { break; };
                let Message::Text(text) = message else { continue; };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else { continue; };
                let kind = value["msg_type"].as_str().unwrap_or_default();
                if kind.starts_with("data:subscribe") {
                    state.events.lock().unwrap().push(StreamObservation {event:StreamAuditEvent::Subscribe,accepted:true});
                    let tag = value["tag"].as_str().unwrap_or("4242");
                    for index in 0..8 {
                        let timestamp = state.base_ms.load(Ordering::SeqCst)+index*1000;
                        let frame = json!({"msg_type":"data:update","tag":tag,"value":format!("{timestamp},25,51.5,-0.12,230.5,180,12345.6,15,241.25,D,80,30")});
                        if socket.send(Message::Text(frame.to_string().into())).await.is_err() { return; }
                    }
                } else if kind == "data:unsubscribe" {
                    state.events.lock().unwrap().push(StreamObservation {event:StreamAuditEvent::Unsubscribe,accepted:true}); let _ = socket.close(None).await; break;
                }
            }
        }
    }
}
