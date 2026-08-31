use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Serialize;
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::sync::Semaphore;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::health::HealthResponse;
use crate::metrics::Metrics;
use crate::protocol::{MAX_RECEIVER_BODY_BYTES, ProtocolError, ReceiverEnvelope};
use crate::spool::{EnqueueOutcome, Spool, SpoolError};

const MAX_CONCURRENT_ADMISSIONS: usize = 32;

pub trait Clock: Send + Sync + 'static {
    fn now_ms(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    }
}

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct ReceiverBearer(String);

impl ReceiverBearer {
    pub fn new(value: impl Into<String>) -> Result<Self, ReceiverBearerError> {
        let value = value.into();
        if !(16..=4_096).contains(&value.len())
            || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(ReceiverBearerError::Invalid);
        }
        Ok(Self(value))
    }

    fn matches(&self, candidate: &str) -> bool {
        candidate.len() == self.0.len() && bool::from(candidate.as_bytes().ct_eq(self.0.as_bytes()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ReceiverBearerError {
    #[error("receiver bearer must contain 16 to 4096 visible ASCII bytes")]
    Invalid,
}

#[derive(Clone)]
pub struct AdmissionService {
    state: Arc<AdmissionState>,
}

struct AdmissionState {
    spool: Spool,
    bearer: ReceiverBearer,
    clock: Arc<dyn Clock>,
    metrics: Metrics,
    permits: Semaphore,
}

impl AdmissionService {
    pub fn new(spool: Spool, bearer: ReceiverBearer, clock: Arc<dyn Clock>) -> Self {
        Self {
            state: Arc::new(AdmissionState {
                spool,
                bearer,
                clock,
                metrics: Metrics::default(),
                permits: Semaphore::new(MAX_CONCURRENT_ADMISSIONS),
            }),
        }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route(
                "/v1/internal/fleet-telemetry",
                post(admit_receiver_envelope),
            )
            .route("/healthz", get(liveness))
            .route("/readyz", get(readiness))
            .route("/metrics", get(metrics))
            .layer(DefaultBodyLimit::max(MAX_RECEIVER_BODY_BYTES))
            .with_state(self.state.clone())
    }

    pub fn spool(&self) -> &Spool {
        &self.state.spool
    }
}

async fn admit_receiver_envelope(
    State(state): State<Arc<AdmissionState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(candidate) = bearer_from_headers(&headers) else {
        state.metrics.rejected_auth();
        return error_response(StatusCode::UNAUTHORIZED, "receiver_authentication_failed");
    };
    if !state.bearer.matches(candidate) {
        state.metrics.rejected_auth();
        return error_response(StatusCode::UNAUTHORIZED, "receiver_authentication_failed");
    }
    let Ok(_permit) = state.permits.try_acquire() else {
        state.metrics.rejected_internal();
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "receiver_busy");
    };
    let now_ms = state.clock.now_ms();
    let envelope = match ReceiverEnvelope::parse_at(&body, now_ms) {
        Ok(envelope) => envelope,
        Err(ProtocolError::InputTooLarge) => {
            state.metrics.rejected_invalid();
            return error_response(StatusCode::PAYLOAD_TOO_LARGE, "receiver_body_too_large");
        }
        Err(_) => {
            state.metrics.rejected_invalid();
            return error_response(StatusCode::BAD_REQUEST, "invalid_receiver_envelope");
        }
    };
    match state.spool.enqueue(envelope, now_ms) {
        Ok(EnqueueOutcome::Stored(_)) => {
            state.metrics.admitted();
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(EnqueueOutcome::AlreadyPresent(_)) => {
            state.metrics.duplicate();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(SpoolError::CapacityExceeded) => {
            state.metrics.rejected_capacity();
            error_response(StatusCode::INSUFFICIENT_STORAGE, "spool_capacity_exceeded")
        }
        Err(SpoolError::StorageFull) => {
            state.metrics.rejected_storage();
            error_response(StatusCode::INSUFFICIENT_STORAGE, "storage_full")
        }
        Err(_) => {
            state.metrics.rejected_internal();
            error_response(StatusCode::SERVICE_UNAVAILABLE, "spool_unavailable")
        }
    }
}

async fn liveness(State(state): State<Arc<AdmissionState>>) -> impl IntoResponse {
    let snapshot = state.spool.snapshot(state.clock.now_ms());
    (
        StatusCode::OK,
        axum::Json(HealthResponse::from_snapshot(&snapshot)),
    )
}

async fn readiness(State(state): State<Arc<AdmissionState>>) -> impl IntoResponse {
    let snapshot = state.spool.snapshot(state.clock.now_ms());
    let status = if snapshot.degraded {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (status, axum::Json(HealthResponse::from_snapshot(&snapshot)))
}

async fn metrics(State(state): State<Arc<AdmissionState>>) -> Response {
    let snapshot = state.spool.snapshot(state.clock.now_ms());
    let mut response = state.metrics.render(&snapshot).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response
}

fn bearer_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|value| !value.is_empty())
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

fn error_response(status: StatusCode, error: &'static str) -> Response {
    (status, axum::Json(ErrorResponse { error })).into_response()
}
