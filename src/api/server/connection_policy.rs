// SPDX-License-Identifier: AGPL-3.0-only

//! Shared listener policy and its in-memory boundary tests.

use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use axum_server::accept::Accept;
use hyper_util::rt::TokioTimer;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Sleep,
};

const MAX_CONNECTIONS: usize = 128;
const READ_DEADLINE: Duration = Duration::from_secs(30);
const HTTP2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

pub(super) fn configure_http_server<A>(
    mut server: axum_server::Server<SocketAddr, A>,
) -> axum_server::Server<SocketAddr, ConnectionAcceptor<A>> {
    server
        .http_builder()
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(READ_DEADLINE);
    server
        .http_builder()
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(READ_DEADLINE)
        .keep_alive_timeout(Duration::from_secs(10));
    server.map(|inner| ConnectionAcceptor {
        inner,
        slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
    })
}

#[derive(Clone)]
pub(super) struct ConnectionAcceptor<A> {
    inner: A,
    slots: Arc<Semaphore>,
}

impl<I, S, A> Accept<I, S> for ConnectionAcceptor<A>
where
    A: Accept<I, S>,
    A::Future: Send + 'static,
    A::Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    A::Service: Send + 'static,
{
    type Stream = ConnectionIo<A::Stream>;
    type Service = A::Service;
    type Future = Pin<Box<dyn Future<Output = io::Result<(Self::Stream, Self::Service)>> + Send>>;

    fn accept(&self, stream: I, service: S) -> Self::Future {
        let Ok(permit) = self.slots.clone().try_acquire_owned() else {
            return Box::pin(async {
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "listener is at capacity",
                ))
            });
        };
        // Admission also covers the inner acceptor's bounded TLS handshake.
        let accepted = self.inner.accept(stream, service);
        Box::pin(async move {
            let (inner, service) = accepted.await?;
            Ok((
                ConnectionIo {
                    inner,
                    _permit: permit,
                    initial_deadline: Box::pin(tokio::time::sleep(READ_DEADLINE)),
                    matched_preface_bytes: 0,
                    detected: false,
                },
                service,
            ))
        })
    }
}

pub(super) struct ConnectionIo<I> {
    inner: I,
    _permit: OwnedSemaphorePermit,
    initial_deadline: Pin<Box<Sleep>>,
    matched_preface_bytes: usize,
    detected: bool,
}

impl<I: AsyncRead + Unpin> AsyncRead for ConnectionIo<I> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        // The HTTP builder's timer starts after protocol detection. Bound the
        // preceding read as well, without consuming or changing any bytes.
        if !this.detected && this.initial_deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "listener read deadline",
            )));
        }
        let before = buffer.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buffer);
        if matches!(result, Poll::Ready(Ok(()))) && !this.detected {
            let bytes = &buffer.filled()[before..];
            let length = bytes
                .len()
                .min(HTTP2_PREFACE.len() - this.matched_preface_bytes);
            let end = this.matched_preface_bytes + length;
            this.detected = bytes[..length] != HTTP2_PREFACE[this.matched_preface_bytes..end]
                || end == HTTP2_PREFACE.len();
            this.matched_preface_bytes = end;
        }
        result
    }
}

impl<I: AsyncWrite + Unpin> AsyncWrite for ConnectionIo<I> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, bytes)
    }
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bytes)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
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
    #[tokio::test]
    async fn http_connection_policy_preserves_http2() {
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
        let client = async move {
            // A SETTINGS frame and a complete GET / on stream 1. The header
            // block uses static HPACK entries plus a synthetic authority.
            let mut request = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec();
            request.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 0, 0]);
            let mut block = vec![0x82, 0x86, 0x84, 0x01, 7];
            block.extend_from_slice(b"fixture");
            request.extend_from_slice(&[0, 0, block.len() as u8, 1, 5, 0, 0, 0, 1]);
            request.extend_from_slice(&block);
            peer.write_all(&request).await.expect("HTTP/2 request");
            let mut body = Vec::new();
            loop {
                let mut header = [0_u8; 9];
                peer.read_exact(&mut header)
                    .await
                    .expect("HTTP/2 frame header");
                let length = (usize::from(header[0]) << 16)
                    | (usize::from(header[1]) << 8)
                    | usize::from(header[2]);
                let stream_id = u32::from_be_bytes(header[5..9].try_into().unwrap()) & 0x7fff_ffff;
                let mut payload = vec![0; length];
                peer.read_exact(&mut payload)
                    .await
                    .expect("HTTP/2 frame payload");
                assert!(
                    header[3] != 3 || stream_id != 1,
                    "ordinary request must not be reset"
                );
                if stream_id == 1 && header[3] == 0 {
                    body.extend_from_slice(&payload);
                }
                if stream_id == 1 && matches!(header[3], 0 | 1) && header[4] & 1 != 0 {
                    break;
                }
            }
            assert_eq!(body, b"fixture-ok");
        };
        let (_served, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(connection, client)
        })
        .await
        .expect("complete HTTP/2 exchange");
    }
}
