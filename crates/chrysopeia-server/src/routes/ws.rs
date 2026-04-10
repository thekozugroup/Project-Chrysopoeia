//! WebSocket handler for real-time progress updates.

use axum::{
    Router,
    extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}},
    response::IntoResponse,
    routing::get,
};
use serde::Serialize;

use crate::state::AppState;

/// Build WebSocket routes.
pub fn ws_routes() -> Router<AppState> {
    Router::new().route("/events", get(ws_handler))
}

/// Events sent over the WebSocket connection.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsEvent {
    /// A transcode job's progress has updated.
    JobProgress {
        job_id: String,
        percent: u8,
        fps: Option<f64>,
        eta_secs: Option<f64>,
    },
    /// A transcode job's status has changed.
    JobStatusChanged {
        job_id: String,
        status: String,
    },
    /// A library scan event (new file found, scan complete, etc.).
    ScanEvent {
        event_type: String,
        path: Option<String>,
        total_found: Option<u64>,
    },
}

/// Handle WebSocket upgrade requests.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Handle an individual WebSocket connection.
async fn handle_socket(mut socket: WebSocket, state: AppState) {
    tracing::info!("WebSocket client connected");
    let _ = state;

    // TODO: Subscribe to progress updates from the worker engine
    // and scan events from the scanner, forwarding them as JSON
    // messages over the WebSocket.
    //
    // loop {
    //     tokio::select! {
    //         Some(progress) = progress_rx.recv() => {
    //             let event = WsEvent::JobProgress { ... };
    //             let msg = serde_json::to_string(&event).unwrap();
    //             socket.send(Message::Text(msg)).await.ok();
    //         }
    //         Some(msg) = socket.recv() => {
    //             // Handle client messages (ping/pong, subscription filters)
    //         }
    //     }
    // }

    // Placeholder: keep connection alive
    while let Some(Ok(_msg)) = socket.recv().await {
        // Echo or handle client messages
    }

    tracing::info!("WebSocket client disconnected");
}
