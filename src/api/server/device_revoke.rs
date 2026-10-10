// SPDX-License-Identifier: AGPL-3.0-only

//! `POST /v1/device/revoke`: a paired device revokes itself.

use super::*;

/// Revoke the calling device.
///
/// The bearer may be the current one or the grace one, and a revoked device's
/// bearers are dead everywhere afterwards, so a repeated call is the ordinary
/// 401: a client treats 401 on revoke as already revoked. The request has no
/// body. A non-empty one, of any size, is 400 and revokes nothing; it is never
/// 413. The body is therefore never read through `Bytes` or `Json`, whose
/// `DefaultBodyLimit` rejection is a 413: it comes in as a raw
/// `Request<Body>` and one byte of it is enough to refuse.
///
/// Order: no paired-device authentication (a trusted local router) is 404, as
/// for rotate and with `auth.revoke` not advertised; then the bearer (401);
/// then the body (400); then the revocation (204, `Cache-Control: no-store`).
pub(super) async fn revoke_device(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Response {
    if !state.require_device_auth {
        return StatusCode::NOT_FOUND.into_response();
    }
    let device = match require_authorized_device(&state, request.headers()) {
        Ok(device) => device,
        Err(status) => return device_auth_reject(status),
    };
    // A body over the one-byte limit is an error, not a partial read.
    if !matches!(
        axum::body::to_bytes(request.into_body(), 1).await,
        Ok(body) if body.is_empty()
    ) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(now_ms) = current_epoch_ms() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    revocation_response(state.store.revoke_device_at(device.device_id, now_ms))
}

/// The answer to a revocation attempt: `204` with `Cache-Control: no-store`, the
/// ordinary `401` when the device is gone since it authenticated, or an empty
/// `503` for a store failure.
pub(super) fn revocation_response(result: Result<(), crate::db::StoreError>) -> Response {
    match result {
        Ok(()) => (
            StatusCode::NO_CONTENT,
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        )
            .into_response(),
        // The device is gone since it authenticated: the same answer a
        // revoked bearer gets.
        Err(crate::db::StoreError::PairingRejected) => unauthorized(),
        Err(error) => {
            tracing::error!(%error, "cannot revoke paired device");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}
