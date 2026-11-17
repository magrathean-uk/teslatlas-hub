// SPDX-License-Identifier: AGPL-3.0-only

//! Shared listener policy and its in-memory boundary tests.

use std::net::SocketAddr;

pub(super) fn configure_http_server<A>(
    server: axum_server::Server<SocketAddr, A>,
) -> axum_server::Server<SocketAddr, A> {
    server
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum_server::accept::Accept;
    use hyper_util::{rt::TokioIo, service::TowerToHyperService};
    use std::{convert::Infallible, time::Duration};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(start_paused = true)]
    async fn http_connection_policy_is_bounded() {
        for input in [
            b"GET /healthz HTTP/1.1\r\nHost: fixture\r\n".as_slice(),
            b"PRI * ".as_slice(),
            b"".as_slice(),
            b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".as_slice(),
        ] {
            // bind constructs a builder; serve is never called and no socket is opened.
            let mut server =
                configure_http_server(axum_server::bind(SocketAddr::from(([127, 0, 0, 1], 1))));
            let (mut peer, io) = tokio::io::duplex(4096);
            peer.write_all(input).await.expect("fixture input");
            let service = tower::service_fn(|_| async {
                Ok::<_, Infallible>(axum::response::Response::new(Body::empty()))
            });
            let (io, service) = server
                .get_ref()
                .accept(io, service)
                .await
                .expect("admission");
            let connection = server.http_builder().serve_connection_with_upgrades(
                TokioIo::new(io),
                TowerToHyperService::new(service),
            );
            let result = tokio::time::timeout(Duration::from_secs(61), connection).await;
            assert!(
                result.is_ok(),
                "connection outlives the resource-policy deadline"
            );
            assert!(
                result.unwrap().is_err(),
                "an unfinished exchange must not succeed"
            );
            drop(peer);
        }
    }

    #[tokio::test]
    async fn http_connection_policy_releases_capacity() {
        let server =
            configure_http_server(axum_server::bind(SocketAddr::from(([127, 0, 0, 1], 1))));
        let mut held = Vec::new();
        for _ in 0..128 {
            let (peer, io) = tokio::io::duplex(64);
            let (io, ()) = server
                .get_ref()
                .accept(io, ())
                .await
                .expect("available admission");
            held.push((peer, io));
        }
        let (_peer, io) = tokio::io::duplex(64);
        assert!(
            server.get_ref().accept(io, ()).await.is_err(),
            "admission must have a finite bound"
        );
        drop(held.pop());
        let (_peer, io) = tokio::io::duplex(64);
        assert!(
            server.get_ref().accept(io, ()).await.is_ok(),
            "closed connection releases admission"
        );
    }

    #[tokio::test]
    async fn http_connection_policy_preserves_http1() {
        let mut server =
            configure_http_server(axum_server::bind(SocketAddr::from(([127, 0, 0, 1], 1))));
        let (mut peer, io) = tokio::io::duplex(4096);
        let service = tower::service_fn(|_| async {
            Ok::<_, Infallible>(axum::response::Response::new(Body::from("fixture-ok")))
        });
        let (io, service) = server
            .get_ref()
            .accept(io, service)
            .await
            .expect("admission");
        let connection = server
            .http_builder()
            .serve_connection_with_upgrades(TokioIo::new(io), TowerToHyperService::new(service));
        let client = async {
            peer.write_all(b"GET /fixture HTTP/1.1\r\nHost: fixture\r\nConnection: close\r\n\r\n")
                .await
                .expect("request");
            let mut response = String::new();
            peer.read_to_string(&mut response).await.expect("response");
            assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
            assert!(response.ends_with("fixture-ok"));
        };
        let (served, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(connection, client)
        })
        .await
        .expect("complete exchange");
        served.expect("server exchange");
    }
}
