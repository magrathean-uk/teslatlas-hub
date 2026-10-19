// SPDX-License-Identifier: AGPL-3.0-only

//! Test-only safety net: a unit test that sends a request to a Tesla production
//! host panics before the request leaves the process. The check sits in the send
//! path of the Owner API, Fleet API, Fleet auth, token-endpoint and stream
//! clients; it exists only in test builds, so production behaviour is unchanged.
//! Tests point their clients at a recording loopback double or an unroutable
//! endpoint instead.

use reqwest::RequestBuilder;

/// The registrable domains of Tesla's production hosts (global, China and the
/// regional Fleet, authentication, Owner API and streaming hosts below them).
const TESLA_PRODUCTION_DOMAINS: [&str; 3] = ["tesla.com", "teslamotors.com", "tesla.cn"];

fn is_tesla_production_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    TESLA_PRODUCTION_DOMAINS.iter().any(|domain| {
        host == *domain
            || host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

/// Panic when `host` is a Tesla production host.
pub(crate) fn refuse_production_host(host: &str) {
    assert!(
        !is_tesla_production_host(host),
        "a unit test tried to reach a Tesla production host ({host}): point the client at a \
         local double or an unroutable endpoint"
    );
}

/// Panic when `request` is addressed to a Tesla production host. A request whose
/// body cannot be cloned is not inspected.
pub(crate) fn refuse_production_request(request: &RequestBuilder) {
    let Some(built) = request.try_clone().and_then(|clone| clone.build().ok()) else {
        return;
    };
    if let Some(host) = built.url().host_str() {
        refuse_production_host(host);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet_api::{FleetAuthApi, FleetClientId, FleetRefreshToken, FleetRegion};
    use std::{panic::catch_unwind, time::Duration};

    #[test]
    fn every_tesla_production_host_is_refused_and_local_hosts_are_not() {
        for host in [
            "auth.tesla.com",
            "fleet-auth.prd.vn.cloud.tesla.com",
            "fleet-api.prd.na.vn.cloud.tesla.com",
            "fleet-api.prd.eu.vn.cloud.tesla.com",
            "owner-api.teslamotors.com",
            "streaming.vn.teslamotors.com",
            "AUTH.TESLA.CN",
            "fleet-api.prd.cn.vn.cloud.tesla.cn.",
        ] {
            assert!(
                catch_unwind(|| refuse_production_host(host)).is_err(),
                "{host} is a production host"
            );
        }
        for host in [
            "127.0.0.1",
            "localhost",
            "provider.example.test",
            "nottesla.com",
            "tesla.com.example.test",
        ] {
            refuse_production_host(host);
        }
    }

    #[tokio::test]
    #[should_panic(expected = "a unit test tried to reach a Tesla production host")]
    async fn a_production_fleet_client_panics_before_it_sends() {
        // The production client, as the entries build it: the first request panics
        // in the send path, before the connection is made.
        let auth = FleetAuthApi::new(
            FleetRegion::EuropeMiddleEastAndAfrica,
            Duration::from_secs(2),
        )
        .expect("production Fleet auth client");
        let _ = auth
            .refresh(
                &FleetClientId::parse("client").expect("client id"),
                &FleetRefreshToken::new("refresh").expect("refresh token"),
            )
            .await;
    }
}
