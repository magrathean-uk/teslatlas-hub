//! Temporary unaccepted-candidate test input contract, derived only from allowed consumer signatures.
//! No predecessor implementation, executable issuer or credential value is supplied.
//! The context is uninhabited; its factory always fails and every observation is unreachable.
use std::convert::Infallible;
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
pub(crate) struct RequestObservation {
    pub(crate) path: String,
    pub(crate) rejected: bool,
    pub(crate) scenario_step: ScenarioStep,
}
pub(crate) struct StreamObservation {
    pub(crate) event: StreamAuditEvent,
    pub(crate) accepted: bool,
}
pub(crate) struct SessionObservation {
    pub(crate) active_sessions: usize,
    pub(crate) max_concurrent_sessions: usize,
    pub(crate) connection_attempts: usize,
}
#[derive(Debug)]
pub(crate) struct FakeTeslaSource {
    never: Infallible,
}
impl FakeTeslaSource {
    pub(crate) async fn spawn_canonical(_mode: AdvanceMode) -> Result<Self, std::io::Error> {
        panic!("C10_UNAVAILABLE_V1: unaccepted candidate; removed credential dependency")
    }
    pub(crate) fn oauth_issuer_url(&self) -> url::Url {
        match self.never {}
    }
    pub(crate) fn http_base_url(&self) -> &url::Url {
        match self.never {}
    }
    pub(crate) fn stream_endpoint(&self) -> &str {
        match self.never {}
    }
    pub(crate) fn token_refresh_request_count(&self) -> usize {
        match self.never {}
    }
    pub(crate) fn audited_requests(&self) -> Vec<RequestObservation> {
        match self.never {}
    }
    pub(crate) fn audited_stream_events(&self) -> Vec<StreamObservation> {
        match self.never {}
    }
    pub(crate) fn stream_session_stats(&self) -> SessionObservation {
        match self.never {}
    }
    pub(crate) fn set_step(&self, _step: ScenarioStep) {
        match self.never {}
    }
    pub(crate) fn current_step(&self) -> ScenarioStep {
        match self.never {}
    }
    pub(crate) fn advance(&self) {
        match self.never {}
    }
    pub(crate) fn set_base_ts_ms(&self, _time: i64) {
        match self.never {}
    }
    pub(crate) fn set_stream_available(&self, _available: bool) {
        match self.never {}
    }
    pub(crate) fn set_owner_vehicle_data_available(&self, _available: bool) {
        match self.never {}
    }
    pub(crate) fn set_stream_script(&self, _script: StreamScript) {
        match self.never {}
    }
    pub(crate) fn assert_no_wake_or_command(&self) -> Result<(), String> {
        match self.never {}
    }
    pub(crate) async fn shutdown(self) {
        match self.never {}
    }
}
