//! `GET /api/ws`: live events.
//!
//! On connect the server sends `queue.state` and `stats.updated`, then
//! forwards every event from the bus. It pings every 30 s. A client that
//! falls behind just misses events (clients refetch on reconnect).

use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use chrysopoeia_core::Event;
use tokio::sync::broadcast::error::RecvError;

use crate::services::dispatcher;
use crate::state::AppState;

/// Interval between server pings.
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Largest message a client may send. The UI only answers pings and
/// closes; the default (64 MB) would let one client make the server hold a
/// lot of memory.
pub const MAX_CLIENT_MESSAGE: usize = 64 * 1024;

/// Upgrade handler.
pub async fn handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(MAX_CLIENT_MESSAGE)
        .max_frame_size(MAX_CLIENT_MESSAGE)
        .on_upgrade(move |socket| session(state, socket))
}

fn encode(event: &Event) -> Option<Message> {
    match serde_json::to_string(event) {
        Ok(s) => Some(Message::Text(Utf8Bytes::from(s))),
        Err(e) => {
            tracing::error!("could not encode an event: {e}");
            None
        }
    }
}

async fn initial_events(state: &AppState) -> Vec<Event> {
    let mut out = Vec::with_capacity(2);
    match dispatcher::queue_state(state).await {
        Ok(q) => out.push(Event::QueueState(q)),
        Err(e) => tracing::error!("could not compute the queue state: {e}"),
    }
    match crate::db::stats::totals(state.db.pool()).await {
        Ok(totals) => out.push(Event::StatsUpdated { totals }),
        Err(e) => tracing::error!("could not compute stats: {e}"),
    }
    out
}

async fn session(state: AppState, mut socket: WebSocket) {
    // Subscribe before computing the initial state so nothing in between is lost.
    let mut rx = state.events.subscribe();
    for event in initial_events(&state).await {
        if let Some(msg) = encode(&event)
            && socket.send(msg).await.is_err()
        {
            return;
        }
    }
    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    loop {
        tokio::select! {
            () = state.shutdown.cancelled() => {
                let _ = socket.send(Message::Close(None)).await;
                break;
            }
            received = rx.recv() => match received {
                Ok(event) => {
                    if let Some(msg) = encode(&event)
                        && socket.send(msg).await.is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::debug!("a WebSocket client fell behind and missed {n} events");
                }
                Err(RecvError::Closed) => break,
            },
            _ = ping.tick() => {
                if socket.send(Message::Ping(Default::default())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}
