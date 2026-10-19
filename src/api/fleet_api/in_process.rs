// SPDX-License-Identifier: AGPL-3.0-only

//! Test-only in-process transport of the Fleet clients (B2 fix round, P3b fix 4,
//! R-P3bF4-1).
//!
//! A Fleet client built with [`FleetApi::for_in_process_test`] or
//! [`FleetAuthApi::for_in_process_test`] does not open a socket and has no
//! transport timeout: every request is handed to the test's handler and awaited in
//! the caller's task, so a collector test that counts the requests which arrived
//! does not depend on loopback scheduling, a five-second deadline or the load of
//! the machine. The handler returns either a response (the test's double) or the
//! transport error the real client would have reported before the request reached
//! any server. The production send path, response limits, status handling and JSON
//! decoding stay the ones the real client uses.

use std::{future::Future, pin::Pin, sync::Arc};

use super::FleetApiError;

/// What the handler of a request answers: a response, or a transport failure that
/// happened before any server saw the request.
pub(crate) type InProcessAnswer = Result<http::Response<Vec<u8>>, FleetApiError>;

type Handler = dyn Fn(http::Request<Vec<u8>>) -> Pin<Box<dyn Future<Output = InProcessAnswer> + Send>>
    + Send
    + Sync;

/// A per-instance, awaited request handler.
#[derive(Clone)]
pub(crate) struct InProcessTransport(Arc<Handler>);

impl InProcessTransport {
    pub(crate) fn new<F, Fut>(handler: F) -> Self
    where
        F: Fn(http::Request<Vec<u8>>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = InProcessAnswer> + Send + 'static,
    {
        Self(Arc::new(move |request| Box::pin(handler(request))))
    }

    pub(crate) async fn call(
        &self,
        request: reqwest::Request,
    ) -> Result<reqwest::Response, FleetApiError> {
        let (parts, body) = http::Request::try_from(request)
            .map_err(|_| FleetApiError::InvalidEndpoint)?
            .into_parts();
        let body = body.as_bytes().map(<[u8]>::to_vec).unwrap_or_default();
        let response = (self.0)(http::Request::from_parts(parts, body)).await?;
        Ok(reqwest::Response::from(response))
    }
}
