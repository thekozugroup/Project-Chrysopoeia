//! Route definitions for the Chrysopeia HTTP API.

pub mod api;
pub mod ws;

use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

/// Build the complete Axum router with all routes.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .nest("/api", api::api_routes())
        .route("/ws", axum::routing::get(ws::ws_handler))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
