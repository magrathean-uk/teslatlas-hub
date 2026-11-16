// SPDX-License-Identifier: AGPL-3.0-only

//! Wire tokens in both spellings for one release (DEC-160).
//!
//! The Hub keeps sending the tokens a paired App knows (the App switches in
//! its own lockstep batch), and accepts the other spelling of each token a
//! client sends: the discovery path, the `x-` request headers (also in a
//! browser's CORS preflight) and the accepted change-set format. Nothing here
//! changes what the Hub emits, and before the swap ([`spelling::renamed`] is
//! false) nothing here changes anything: each hook behaves exactly as the
//! code it replaced.

use axum::{
    Router,
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
    routing::MethodRouter,
};

use super::spelling;

/// The discovery path in the current spelling.
pub const DISCOVERY_PATH_CURRENT: &str = "/.well-known/tesatlas-hub";
/// The discovery path in the earlier spelling.
pub const DISCOVERY_PATH_EARLIER: &str = "/.well-known/teslatlas-hub"; // rename:keep

/// Whether `path` is the discovery path in either spelling (after the swap;
/// before it the served path's own match arm applies, so this is false).
pub fn is_discovery_path(path: &str) -> bool {
    spelling::renamed() && (path == DISCOVERY_PATH_CURRENT || path == DISCOVERY_PATH_EARLIER)
}

/// The other spelling of the discovery route the server already serves, so
/// both are routed; `None` before the swap or if `served` is neither.
pub fn other_discovery_path(served: &str) -> Option<&'static str> {
    if !spelling::renamed() {
        return None;
    }
    match served {
        DISCOVERY_PATH_EARLIER => Some(DISCOVERY_PATH_CURRENT),
        DISCOVERY_PATH_CURRENT => Some(DISCOVERY_PATH_EARLIER),
        _ => None,
    }
}

/// The discovery route's other spelling, to merge beside the served one; an
/// empty router before the swap.
pub fn discovery_alias<S: Clone + Send + Sync + 'static>(served: &str, handler: MethodRouter<S>) -> Router<S> {
    match other_discovery_path(served) {
        Some(other) => Router::new().route(other, handler),
        None => Router::new(),
    }
}

/// Whether `formats` is exactly `[token]`, after the swap with the token in
/// either spelling.
pub fn is_only_token(formats: &[String], token: &str) -> bool {
    match formats {
        [only] => {
            only == token
                || (spelling::renamed() && spelling::other(token).is_some_and(|other| *only == other))
        }
        _ => false,
    }
}

/// Request middleware: a product `x-` header sent in one spelling is also
/// visible under the other, unless the request already has that one. A header
/// sent once stays a single value under each name. A pass-through before the swap.
pub async fn accept_both_header_spellings(mut request: Request, next: Next) -> Response {
    if spelling::renamed() {
        add_other_header_spellings(request.headers_mut());
    }
    next.run(request).await
}

/// The header step of [`accept_both_header_spellings`], for tests.
pub fn add_other_header_spellings(headers: &mut axum::http::HeaderMap) {
    let additions: Vec<(HeaderName, Vec<HeaderValue>)> = headers
        .keys()
        .filter(|name| is_product_header(name.as_str()))
        .filter_map(|name| {
            let other = HeaderName::from_bytes(spelling::other(name.as_str())?.as_bytes()).ok()?;
            (!headers.contains_key(&other)).then(|| (other, headers.get_all(name).iter().cloned().collect()))
        })
        .collect();
    for (name, values) in additions {
        for value in values {
            headers.append(name.clone(), value);
        }
    }
}

/// CORS preflight: whether the requested header `name` is in the route's
/// `allowed` list (`", "`-separated); after the swap a product header in its
/// other spelling is allowed too.
pub fn header_allowed(name: &str, allowed: &'static str) -> bool {
    let listed = |name: &str| allowed.split(", ").any(|entry| name.eq_ignore_ascii_case(entry));
    listed(name)
        || (spelling::renamed()
            && is_product_header(&name.to_ascii_lowercase())
            && spelling::other(&name.to_ascii_lowercase()).is_some_and(|other| listed(&other)))
}

/// CORS preflight: the `Access-Control-Allow-Headers` value for a route's
/// `allowed` list; after the swap each product header is listed in both
/// spellings, so a browser may send either.
pub fn allow_headers(allowed: &'static str) -> HeaderValue {
    if !spelling::renamed() {
        return HeaderValue::from_static(allowed);
    }
    let mut names: Vec<String> = Vec::new();
    for entry in allowed.split(", ") {
        names.push(entry.to_owned());
        if is_product_header(entry)
            && let Some(other) = spelling::other(entry)
            && !allowed.split(", ").any(|listed| listed == other)
        {
            names.push(other);
        }
    }
    HeaderValue::from_str(&names.join(", ")).unwrap_or_else(|_| HeaderValue::from_static(allowed))
}

fn is_product_header(name: &str) -> bool {
    name.starts_with("x-tesatlas-") || name.starts_with("x-teslatlas-") // rename:keep
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::*;

    const EARLIER_PROFILE: &str = "x-teslatlas-sync-profile"; // rename:keep
    const EARLIER_SYNC_HEADERS: &str = "authorization, x-teslatlas-sync-profile"; // rename:keep

    #[test]
    fn a_header_in_the_new_spelling_is_also_seen_under_the_one_the_hub_reads() {
        let mut headers = HeaderMap::new();
        headers.insert("x-tesatlas-sync-profile", HeaderValue::from_static("hub-sync-v1@1.4.0"));
        headers.insert("x-request-id", HeaderValue::from_static("r1"));
        add_other_header_spellings(&mut headers);
        assert_eq!(headers.get_all(EARLIER_PROFILE).iter().count(), 1);
        assert_eq!(headers.get(EARLIER_PROFILE).unwrap(), "hub-sync-v1@1.4.0");
        assert_eq!(headers.len(), 3);
    }

    #[test]
    fn a_header_sent_in_both_spellings_is_never_doubled_or_overwritten() {
        let mut headers = HeaderMap::new();
        headers.insert(EARLIER_PROFILE, HeaderValue::from_static("earlier"));
        headers.insert("x-tesatlas-sync-profile", HeaderValue::from_static("current"));
        add_other_header_spellings(&mut headers);
        assert_eq!(headers.get_all(EARLIER_PROFILE).iter().collect::<Vec<_>>(), ["earlier"]);
        assert_eq!(headers.get_all("x-tesatlas-sync-profile").iter().collect::<Vec<_>>(), ["current"]);
    }

    #[test]
    fn after_the_swap_the_change_set_format_and_discovery_path_are_accepted_in_either_spelling() {
        let _renamed = spelling::assume_renamed();
        let earlier = "teslatlas-physical-v3-delta-v1"; // rename:keep
        let current = "tesatlas-physical-v3-delta-v1";
        assert!(is_only_token(&[earlier.to_owned()], earlier));
        assert!(is_only_token(&[current.to_owned()], earlier));
        assert!(is_only_token(&[earlier.to_owned()], current));
        assert!(!is_only_token(&[earlier.to_owned(), current.to_owned()], earlier));
        assert!(!is_only_token(&["other-format".to_owned()], earlier));
        assert!(is_discovery_path(DISCOVERY_PATH_CURRENT) && is_discovery_path(DISCOVERY_PATH_EARLIER));
        assert_eq!(other_discovery_path(DISCOVERY_PATH_EARLIER), Some(DISCOVERY_PATH_CURRENT));
        assert_eq!(other_discovery_path("/healthz"), None);
    }

    #[test]
    fn after_the_swap_a_preflight_may_name_either_spelling() {
        let _renamed = spelling::assume_renamed();
        assert!(header_allowed("X-Tesatlas-Sync-Profile", EARLIER_SYNC_HEADERS));
        assert!(header_allowed(EARLIER_PROFILE, EARLIER_SYNC_HEADERS));
        assert!(header_allowed("Authorization", EARLIER_SYNC_HEADERS));
        assert!(!header_allowed("x-tesatlas-supported-schemas", EARLIER_SYNC_HEADERS));
        assert!(!header_allowed("range", EARLIER_SYNC_HEADERS));
        assert_eq!(
            allow_headers(EARLIER_SYNC_HEADERS),
            "authorization, x-teslatlas-sync-profile, x-tesatlas-sync-profile" // rename:keep
        );
    }

    /// Finding 8 of REV-CX-M08-T12A: before the swap every wire hook behaves
    /// exactly as the code it replaced.
    #[test]
    fn before_the_swap_no_wire_alias_is_active() {
        if spelling::renamed() {
            return; // after the swap the test above covers the aliases
        }
        let current = "tesatlas-physical-v3-delta-v1";
        let earlier = "teslatlas-physical-v3-delta-v1"; // rename:keep
        assert!(is_only_token(&[earlier.to_owned()], earlier));
        assert!(!is_only_token(&[current.to_owned()], earlier));
        assert!(!is_discovery_path(DISCOVERY_PATH_CURRENT) && !is_discovery_path(DISCOVERY_PATH_EARLIER));
        assert_eq!(other_discovery_path(DISCOVERY_PATH_EARLIER), None);
        assert!(!header_allowed("x-tesatlas-sync-profile", EARLIER_SYNC_HEADERS));
        assert!(header_allowed(EARLIER_PROFILE, EARLIER_SYNC_HEADERS));
        assert_eq!(allow_headers(EARLIER_SYNC_HEADERS), EARLIER_SYNC_HEADERS);
    }

    #[tokio::test]
    async fn the_header_middleware_passes_requests_through_unchanged_before_the_swap() {
        use tower::ServiceExt as _;
        async fn echo(headers: HeaderMap) -> String {
            let mut names: Vec<_> = headers.keys().map(|name| name.as_str().to_owned()).collect();
            names.sort();
            names.join(",")
        }
        let app = Router::new()
            .route("/", axum::routing::get(echo))
            .layer(axum::middleware::from_fn(accept_both_header_spellings));
        let request = || {
            axum::http::Request::builder()
                .uri("/")
                .header("x-tesatlas-sync-profile", "hub-sync-v1@1.4.0")
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let body = |response: axum::response::Response| async move {
            String::from_utf8(axum::body::to_bytes(response.into_body(), 4096).await.unwrap().to_vec()).unwrap()
        };
        let swapped = spelling::renamed();
        let seen = body(app.clone().oneshot(request()).await.unwrap()).await;
        assert_eq!(seen.contains(EARLIER_PROFILE), swapped);
        let _renamed = spelling::assume_renamed();
        let seen = body(app.oneshot(request()).await.unwrap()).await;
        assert_eq!(seen, format!("x-tesatlas-sync-profile,{EARLIER_PROFILE}"));
    }
}
