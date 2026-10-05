//! The HTTP server: accepting connections, their limits, and a graceful
//! stop.
//!
//! `axum::serve` sets no limits: a client that opens a connection and never
//! finishes its request headers holds it (and a file descriptor) forever,
//! and enough of them use up the process's descriptors, after which the UI
//! can't be reached and ffmpeg can't be started. So each connection gets:
//!
//! - [`HEADER_READ_TIMEOUT`] to send a request's headers, which also closes
//!   a kept-open connection that sends nothing more for that long;
//! - one of [`MAX_CONNECTIONS`] places (a WebSocket keeps its place for as
//!   long as it is open). Beyond that, new connections wait in the system's
//!   queue until a place frees up.
//!
//! [`raise_open_file_limit`] also raises the process's soft limit on open
//! files to its hard limit at start (Docker's soft limit is often 1024).

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::Router;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio_util::sync::CancellationToken;

/// Longest wait for a request's headers, and for the next request on a
/// connection kept open.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Connections served at once. The UI needs a handful per open tab.
pub const MAX_CONNECTIONS: usize = 256;

/// Soft limit on open files the server asks for at start (when the hard
/// limit allows): plenty for connections, ffmpeg and the database, without
/// the huge values some systems allow, which make starting a program slow.
const OPEN_FILES_WANTED: u64 = 65_536;

/// Minimum time between two log lines about the same accept problem.
const ACCEPT_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Serve `router` on `listener` until `shutdown` is cancelled, then let the
/// requests in progress finish (connections that stay idle are closed).
pub async fn serve(listener: TcpListener, router: Router, shutdown: CancellationToken) {
    serve_with(
        listener,
        router,
        shutdown,
        MAX_CONNECTIONS,
        HEADER_READ_TIMEOUT,
    )
    .await;
}

async fn serve_with(
    listener: TcpListener,
    router: Router,
    shutdown: CancellationToken,
    max_connections: usize,
    header_timeout: Duration,
) {
    let places = Arc::new(Semaphore::new(max_connections));
    // Every connection holds a receiver; `closed` resolves once all are gone.
    let (open_tx, open_rx) = watch::channel(());
    let mut full_logged: Option<Instant> = None;
    let mut error_logged: Option<Instant> = None;
    loop {
        let place = match Arc::clone(&places).try_acquire_owned() {
            Ok(place) => place,
            Err(_) => {
                if full_logged.is_none_or(|at| at.elapsed() >= ACCEPT_LOG_INTERVAL) {
                    full_logged = Some(Instant::now());
                    tracing::warn!(
                        "{max_connections} connections are open, the most served at once; new \
                         ones wait until one closes"
                    );
                }
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => break,
                    place = Arc::clone(&places).acquire_owned() => match place {
                        Ok(place) => place,
                        Err(_) => break,
                    },
                }
            }
        };
        let stream = tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) if is_connection_error(&e) => continue,
                Err(e) => {
                    // Out of file descriptors, most likely: wait a moment
                    // instead of spinning.
                    if error_logged.is_none_or(|at| at.elapsed() >= ACCEPT_LOG_INTERVAL) {
                        error_logged = Some(Instant::now());
                        tracing::warn!("could not accept a connection: {e}");
                    }
                    drop(place);
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_secs(1)) => continue,
                    }
                }
            },
        };
        let _ = stream.set_nodelay(true);
        let io = TokioIo::new(Counted {
            stream,
            _place: place,
        });
        let service = TowerToHyperService::new(router.clone());
        let shutdown = shutdown.clone();
        let open = open_rx.clone();
        tokio::spawn(async move {
            let mut builder = hyper::server::conn::http1::Builder::new();
            builder
                .timer(TokioTimer::new())
                .header_read_timeout(header_timeout);
            let conn = builder.serve_connection(io, service).with_upgrades();
            let mut conn = std::pin::pin!(conn);
            let mut stopping = false;
            loop {
                tokio::select! {
                    result = conn.as_mut() => {
                        if let Err(e) = result {
                            tracing::trace!("connection ended: {e}");
                        }
                        break;
                    }
                    () = shutdown.cancelled(), if !stopping => {
                        stopping = true;
                        conn.as_mut().graceful_shutdown();
                    }
                }
            }
            drop(open);
        });
    }
    drop(open_rx);
    open_tx.closed().await;
}

/// Errors about one connection (the client went away while it was being
/// accepted), not about the listener.
fn is_connection_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    )
}

/// A connection that keeps its place among [`MAX_CONNECTIONS`] for as long
/// as it is open, including after a WebSocket upgrade.
struct Counted {
    stream: TcpStream,
    _place: OwnedSemaphorePermit,
}

impl AsyncRead for Counted {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Counted {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

/// Raise the soft limit on open files towards the hard limit (at most
/// 65,536). Docker often starts containers with a soft limit of 1024, which
/// a busy server with several jobs can reach. Returns the soft limit in
/// effect afterwards, when known.
pub fn raise_open_file_limit() -> Option<u64> {
    #[cfg(unix)]
    {
        use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
        let limit = getrlimit(Resource::Nofile);
        // `None` means unlimited.
        let current = limit.current?;
        let wanted = limit
            .maximum
            .map_or(OPEN_FILES_WANTED, |max| max.min(OPEN_FILES_WANTED));
        if current >= wanted {
            return Some(current);
        }
        match setrlimit(
            Resource::Nofile,
            Rlimit {
                current: Some(wanted),
                maximum: limit.maximum,
            },
        ) {
            Ok(()) => {
                tracing::debug!("raised the open file limit from {current} to {wanted}");
                Some(wanted)
            }
            Err(e) => {
                tracing::debug!("could not raise the open file limit ({current}): {e}");
                Some(current)
            }
        }
    }
    #[cfg(not(unix))]
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    async fn server(
        max: usize,
        header_timeout: Duration,
    ) -> (std::net::SocketAddr, CancellationToken) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new().route("/ping", axum::routing::get(|| async { "pong" }));
        let stop = CancellationToken::new();
        tokio::spawn(serve_with(
            listener,
            router,
            stop.clone(),
            max,
            header_timeout,
        ));
        (addr, stop)
    }

    async fn ping(addr: std::net::SocketAddr) -> String {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(b"GET /ping HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    /// A client that never finishes its request is disconnected after the
    /// header timeout, instead of holding the connection forever.
    #[tokio::test]
    async fn unfinished_requests_are_dropped() {
        let (addr, stop) = server(8, Duration::from_millis(300)).await;
        let mut half = TcpStream::connect(addr).await.unwrap();
        half.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), half.read_to_end(&mut buf)).await;
        assert!(read.is_ok(), "the connection stayed open");
        assert!(ping(addr).await.ends_with("pong"));
        stop.cancel();
    }

    /// Connections beyond the limit wait for a place instead of using up
    /// the process's files; they are served once one closes.
    #[tokio::test]
    async fn connections_beyond_the_limit_wait_their_turn() {
        let (addr, stop) = server(2, Duration::from_secs(30)).await;
        let a = TcpStream::connect(addr).await.unwrap();
        let b = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let third = tokio::spawn(ping(addr));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!third.is_finished(), "served past the limit");
        drop(a);
        let answer = tokio::time::timeout(Duration::from_secs(5), third)
            .await
            .expect("not served once a place freed up")
            .unwrap();
        assert!(answer.ends_with("pong"), "{answer}");
        drop(b);
        stop.cancel();
    }

    #[test]
    fn the_open_file_limit_is_raised() {
        #[cfg(unix)]
        {
            let limit = rustix::process::getrlimit(rustix::process::Resource::Nofile);
            let after = raise_open_file_limit();
            if let (Some(before), Some(after)) = (limit.current, after) {
                assert!(after >= before.min(OPEN_FILES_WANTED));
            }
        }
    }
}
