use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Serialize;

use crate::admission::Clock;
use crate::credentials::CredentialStore;
use crate::protocol::{HubAckV1, MAX_HUB_ACK_BODY_BYTES};
use crate::spool::Spool;

#[derive(Clone)]
pub struct DeliveryService {
    state: Arc<DeliveryState>,
}

struct DeliveryState {
    spool: Spool,
    credentials: CredentialStore,
    clock: Arc<dyn Clock>,
}

impl DeliveryService {
    pub fn new(spool: Spool, credentials: CredentialStore, clock: Arc<dyn Clock>) -> Self {
        Self {
            state: Arc::new(DeliveryState {
                spool,
                credentials,
                clock,
            }),
        }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/v1/hub/batches/next", get(next_batch))
            .route("/v1/hub/acks", post(acknowledge))
            .layer(DefaultBodyLimit::max(MAX_HUB_ACK_BODY_BYTES))
            .with_state(self.state.clone())
    }

    pub fn spool(&self) -> &Spool {
        &self.state.spool
    }

    pub fn credentials(&self) -> &CredentialStore {
        &self.state.credentials
    }
}

async fn next_batch(State(state): State<Arc<DeliveryState>>, headers: HeaderMap) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return authorization_error(failure);
    }
    match state.spool.next_batch(state.clock.now_ms()) {
        Ok(batch) => axum::Json(batch).into_response(),
        Err(_) => error_response(StatusCode::SERVICE_UNAVAILABLE, "spool_unavailable"),
    }
}

async fn acknowledge(
    State(state): State<Arc<DeliveryState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(failure) = authorize(&state, &headers) {
        return authorization_error(failure);
    }
    let acknowledgement = match HubAckV1::parse(&body) {
        Ok(acknowledgement) => acknowledgement,
        _ => return error_response(StatusCode::BAD_REQUEST, "invalid_acknowledgement"),
    };
    match state.spool.acknowledge(&acknowledgement) {
        Ok(result) => axum::Json(result).into_response(),
        Err(crate::spool::SpoolError::InvalidAcknowledgement) => {
            error_response(StatusCode::BAD_REQUEST, "invalid_acknowledgement")
        }
        Err(_) => error_response(StatusCode::SERVICE_UNAVAILABLE, "spool_unavailable"),
    }
}

fn authorize(state: &DeliveryState, headers: &HeaderMap) -> Result<(), AuthorizationFailure> {
    let Some(token) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
    else {
        return Err(AuthorizationFailure::Rejected);
    };
    match state.credentials.verify(token, state.clock.now_ms()) {
        Ok(true) => Ok(()),
        Ok(false) => Err(AuthorizationFailure::Rejected),
        Err(_) => Err(AuthorizationFailure::Unavailable),
    }
}

enum AuthorizationFailure {
    Rejected,
    Unavailable,
}

fn authorization_error(failure: AuthorizationFailure) -> Response {
    match failure {
        AuthorizationFailure::Rejected => {
            error_response(StatusCode::UNAUTHORIZED, "hub_authentication_failed")
        }
        AuthorizationFailure::Unavailable => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "credential_store_unavailable",
        ),
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

fn error_response(status: StatusCode, error: &'static str) -> Response {
    (status, axum::Json(ErrorResponse { error })).into_response()
}
