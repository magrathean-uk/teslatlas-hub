// SPDX-License-Identifier: AGPL-3.0-only

//! Wire tokens in both spellings for one release (DEC-160).
//!
//! The Hub keeps sending the tokens a paired App knows (the App switches in
//! its own lockstep batch), and accepts the other spelling of each token a
//! client sends: the discovery path, the `x-` request headers and the
//! accepted change-set format. Nothing here changes what the Hub emits.

use axum::{extract::Request, http::HeaderName, middleware::Next, response::Response};

/// The discovery path in the current spelling.
pub const DISCOVERY_PATH_CURRENT: &str = "/.well-known/tesatlas-hub";
/// The discovery path in the earlier spelling.
pub const DISCOVERY_PATH_EARLIER: &str = "/.well-known/teslatlas-hub"; // rename:keep

/// Whether `path` is the discovery path in either spelling.
pub fn is_discovery_path(path: &str) -> bool {
    path == DISCOVERY_PATH_CURRENT || path == DISCOVERY_PATH_EARLIER
}

/// The other spelling of the discovery route the server already serves, so
/// both are routed; `None` if `served` is neither.
pub fn other_discovery_path(served: &str) -> Option<&'static str> {
    match served {
        DISCOVERY_PATH_EARLIER => Some(DISCOVERY_PATH_CURRENT),
        DISCOVERY_PATH_CURRENT => Some(DISCOVERY_PATH_EARLIER),
        _ => None,
    }
}

/// Whether `formats` is exactly `[token]`, the token in either spelling.
pub fn is_only_token(formats: &[String], token: &str) -> bool {
    match formats {
        [only] => only == token || super::spelling::other(token).is_some_and(|other| *only == other),
        _ => false,
    }
}

/// Request middleware: a product `x-` header sent in one spelling is also
/// visible under the other, unless the request already has that one. A header
/// sent once stays a single value under each name.
pub async fn accept_both_header_spellings(mut request: Request, next: Next) -> Response {
    add_other_header_spellings(request.headers_mut());
    next.run(request).await
}

/// The header step of [`accept_both_header_spellings`], for tests.
pub fn add_other_header_spellings(headers: &mut axum::http::HeaderMap) {
    let additions: Vec<(HeaderName, Vec<axum::http::HeaderValue>)> = headers
        .keys()
        .filter(|name| is_product_header(name.as_str()))
        .filter_map(|name| {
            let other = HeaderName::from_bytes(super::spelling::other(name.as_str())?.as_bytes()).ok()?;
            (!headers.contains_key(&other)).then(|| (other, headers.get_all(name).iter().cloned().collect()))
        })
        .collect();
    for (name, values) in additions {
        for value in values {
            headers.append(name.clone(), value);
        }
    }
}

fn is_product_header(name: &str) -> bool {
    name.starts_with("x-tesatlas-") || name.starts_with("x-teslatlas-") // rename:keep
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::*;

    const EARLIER_PROFILE: &str = "x-teslatlas-sync-profile"; // rename:keep

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
    fn the_change_set_format_and_discovery_path_are_accepted_in_either_spelling() {
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
}
