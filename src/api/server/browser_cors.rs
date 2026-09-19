// SPDX-License-Identifier: AGPL-3.0-only

use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::config::{HttpConfig, HubConfig};

const BASIC_REQUEST_HEADERS: &str = "authorization, content-type, if-none-match, accept";
const SYNC_REQUEST_HEADERS: &str = "authorization, content-type, if-none-match, accept, x-teslatlas-supported-schemas, x-teslatlas-sync-capability";
const PACK_REQUEST_HEADERS: &str =
    "authorization, content-type, if-none-match, accept, range, if-range";
const EXPOSED_RESPONSE_HEADERS: &str = "ETag, X-Request-ID, Accept-Ranges, Content-Range, Content-Length, Content-Encoding, X-Teslatlas-Manifest-Signature, X-Teslatlas-Native-Config-Sha256";

#[derive(Clone, Copy)]
struct RoutePolicy {
    method: &'static str,
    request_headers: &'static str,
}

#[derive(Clone, Default)]
pub(super) struct CorsPolicy {
    pub(super) http: HttpConfig,
    pub(super) same_origin: Option<String>,
}

impl CorsPolicy {
    pub(super) fn for_config(config: &HubConfig) -> Self {
        let endpoint = config
            .tls
            .as_ref()
            .map(|tls| tls.public_url.clone())
            .unwrap_or_else(|| format!("http://{}", config.bind));
        Self {
            http: config.http.clone(),
            same_origin: url::Url::parse(&endpoint)
                .ok()
                .map(|url| url.origin().ascii_serialization()),
        }
    }
}

fn vary(response: &mut Response, preflight: bool) {
    response
        .headers_mut()
        .append(header::VARY, HeaderValue::from_static("Origin"));
    if preflight {
        response.headers_mut().append(
            header::VARY,
            HeaderValue::from_static(
                "Access-Control-Request-Method, Access-Control-Request-Headers",
            ),
        );
    }
}

fn deny(preflight: bool) -> Response {
    let mut response = StatusCode::FORBIDDEN.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    vary(&mut response, preflight);
    response
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(first)
}

fn route_policy(path: &str) -> Option<RoutePolicy> {
    let get = |request_headers| RoutePolicy {
        method: "GET",
        request_headers,
    };
    match path {
        "/healthz" | "/readyz" | "/.well-known/teslatlas-hub" | "/v1/vehicles" => {
            return Some(get(BASIC_REQUEST_HEADERS));
        }
        "/v1/device/rotate" => {
            return Some(RoutePolicy {
                method: "POST",
                request_headers: BASIC_REQUEST_HEADERS,
            });
        }
        _ => {}
    }
    let segments: Vec<_> = path.strip_prefix('/')?.split('/').collect();
    match segments.as_slice() {
        ["v1", "pairings", pairing_id, "claim"] if !pairing_id.is_empty() => Some(RoutePolicy {
            method: "POST",
            request_headers: BASIC_REQUEST_HEADERS,
        }),
        ["v1", "vehicles", vehicle_id, "current" | "drives"] if !vehicle_id.is_empty() => {
            Some(get(BASIC_REQUEST_HEADERS))
        }
        ["v1", "vehicles", vehicle_id, "sync", "manifest" | "noop"] if !vehicle_id.is_empty() => {
            Some(get(SYNC_REQUEST_HEADERS))
        }
        ["v1", "packs", "sha256", object_name] if !object_name.is_empty() => {
            Some(get(PACK_REQUEST_HEADERS))
        }
        _ => None,
    }
}

fn allowed_origin(policy: &CorsPolicy, headers: &HeaderMap) -> Option<HeaderValue> {
    let origin = single_header(headers, "origin")?;
    if origin == "null" || policy.http.validate().is_err() {
        return None;
    }
    let same_origin = policy.same_origin.as_deref() == Some(origin);
    if !same_origin
        && !policy
            .http
            .allowed_origins
            .iter()
            .any(|allowed| allowed == origin)
    {
        return None;
    }
    HeaderValue::from_str(origin).ok()
}

fn decorate_allowed_response(response: &mut Response, origin: HeaderValue, preflight: bool) {
    response
        .headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    response.headers_mut().insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(EXPOSED_RESPONSE_HEADERS),
    );
    vary(response, preflight);
}

/// This layer is applied only to matched public routes, before any pairing or
/// bearer handler. Internal Fleet ingress is merged after it and has no CORS.
pub(super) async fn apply(
    State(policy): State<CorsPolicy>,
    request: Request,
    next: Next,
) -> Response {
    let preflight = request.method() == Method::OPTIONS;
    if !request.headers().contains_key(header::ORIGIN) {
        let mut response = next.run(request).await;
        vary(&mut response, preflight);
        return response;
    }
    let Some(route) = route_policy(request.uri().path()) else {
        return deny(preflight);
    };
    let Some(origin) = allowed_origin(&policy, request.headers()) else {
        return deny(preflight);
    };
    let mut response = if preflight {
        if single_header(request.headers(), "access-control-request-method") != Some(route.method) {
            return deny(true);
        }
        if request
            .headers()
            .contains_key("access-control-request-headers")
        {
            let Some(headers) = single_header(request.headers(), "access-control-request-headers")
            else {
                return deny(true);
            };
            if headers.len() > 2048
                || headers.split(',').any(|name| {
                    let name = name.trim();
                    name.is_empty()
                        || !route
                            .request_headers
                            .split(", ")
                            .any(|allowed| name.eq_ignore_ascii_case(allowed))
                })
            {
                return deny(true);
            }
        }
        let mut response = StatusCode::NO_CONTENT.into_response();
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static(route.method),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(route.request_headers),
        );
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    } else {
        next.run(request).await
    };
    decorate_allowed_response(&mut response, origin, preflight);
    response
}

/// Resource limits wrap the route-local CORS layer so their synthetic response
/// needs a narrow outer decoration pass. Only exact public routes and admitted
/// origins qualify; internal Fleet ingress is never exposed.
pub(super) async fn decorate_resource_limit_response(
    State(policy): State<CorsPolicy>,
    request: Request,
    next: Next,
) -> Response {
    let origin = (request.method() != Method::OPTIONS
        && route_policy(request.uri().path()).is_some())
    .then(|| allowed_origin(&policy, request.headers()))
    .flatten();
    let mut response = next.run(request).await;
    if response.status() == StatusCode::SERVICE_UNAVAILABLE
        && response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
        && let Some(origin) = origin
    {
        decorate_allowed_response(&mut response, origin, false);
    }
    response
}
