//! The HTTP application: `/api` plus the static web UI, with compression,
//! security headers, cache headers, panic protection, and the checks that
//! keep other websites from using the API (see [`crate::guard`]).
//!
//! The UI is the statically exported Next.js app in `--web-dir`. Any GET that
//! is not under `/api` and has no matching file gets `index.html` (the UI
//! routes with the URL hash), except paths that look like assets, which get a
//! plain 404 so a missing script is never answered with HTML.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_TYPE, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{MethodRouter, any};
use tower::ServiceExt;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::error::INTERNAL_MESSAGE;
use crate::guard::RequestGuard;
use crate::state::AppState;

/// Shown when `--web-dir` has no `index.html`.
const UI_MISSING_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Szalinski</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 16px/1.5 system-ui, sans-serif; max-width: 36rem; margin: 4rem auto; padding: 0 1rem; }
  code { font-family: ui-monospace, monospace; }
</style>
</head>
<body>
<h1>Szalinski is running</h1>
<p>The web interface isn't built into this installation, so there is nothing to show here yet.
The server and its API are working: see <a href="/api/health"><code>/api/health</code></a>.</p>
<p>To add the interface, build it with <code>pnpm build</code> in the <code>web</code> folder and
point <code>WEB_DIR</code> (or <code>--web-dir</code>) at the exported <code>out</code> folder.</p>
</body>
</html>
"#;

/// Whether the web UI is present.
pub async fn ui_available(web_dir: &Path) -> bool {
    tokio::fs::metadata(web_dir.join("index.html"))
        .await
        .is_ok_and(|m| m.is_file())
}

async fn ui_missing(req: Request) -> Response {
    let status = if req.uri().path() == "/" {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };
    let mut resp = (status, Html(UI_MISSING_PAGE)).into_response();
    resp.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp
}

/// Whether the last path segment looks like a file with an extension.
fn looks_like_asset(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or("");
    last.contains('.') && !last.ends_with(".html")
}

/// Answer a GET without a matching file: `index.html`, or 404 for assets.
async fn spa_fallback(State(index): State<Arc<PathBuf>>, req: Request) -> Response {
    if looks_like_asset(req.uri().path()) {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    match ServeFile::new(index.as_path()).oneshot(req).await {
        Ok(r) => r.map(Body::new),
        Err(never) => match never {},
    }
}

/// Static files with the SPA fallback.
fn static_ui(web_dir: &Path) -> ServeDir<MethodRouter> {
    let index = Arc::new(web_dir.join("index.html"));
    ServeDir::new(web_dir)
        .append_index_html_on_directories(true)
        .fallback(any(spa_fallback).with_state(index))
}

/// Cache headers for the UI: hashed Next.js assets are immutable, everything
/// else revalidates so upgrades show up immediately.
async fn cache_headers(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let mut resp = next.run(req).await;
    if path.starts_with("/api") {
        return resp;
    }
    let ok = resp.status().is_success() || resp.status() == StatusCode::NOT_MODIFIED;
    if ok && !resp.headers().contains_key(CACHE_CONTROL) {
        let value = if path.starts_with("/_next/static/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        resp.headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static(value));
    }
    resp
}

fn panic_response(_: Box<dyn std::any::Any + Send + 'static>) -> Response {
    // The panic hook has already logged the details.
    let body = serde_json::json!({ "error": INTERNAL_MESSAGE, "code": "internal" }).to_string();
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

/// The complete HTTP application, checking whether the UI is present.
pub async fn app(state: AppState) -> Router {
    let ui = ui_available(&state.config.web_dir).await;
    router(state, ui)
}

/// The complete HTTP application. Without the UI, non-API paths get a page
/// explaining that it isn't built.
pub fn router(state: AppState, ui: bool) -> Router {
    let web_dir = state.config.web_dir.clone();
    let dev_cors = state.config.dev_cors;
    let guard = Arc::new(RequestGuard::new(&state.config));
    let api = crate::api::router()
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            guard,
            crate::guard::middleware,
        ));
    // `/api/` itself is not covered by the nested router; it must not fall
    // through to the UI either.
    let mut app = Router::new()
        .nest("/api", api)
        .route("/api/", any(crate::api::not_found));
    app = if ui {
        app.fallback_service(static_ui(&web_dir))
    } else {
        tracing::warn!(
            web_dir = %web_dir.display(),
            "the web UI was not found; only the API is served"
        );
        app.fallback(ui_missing)
    };
    let mut app = app
        .layer(middleware::from_fn(cache_headers))
        .layer(CatchPanicLayer::custom(panic_response))
        .layer(CompressionLayer::new())
        .layer(SetResponseHeaderLayer::if_not_present(
            X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            X_FRAME_OPTIONS,
            HeaderValue::from_static("SAMEORIGIN"),
        ))
        .layer(TraceLayer::new_for_http());
    if dev_cors {
        app = app.layer(CorsLayer::permissive());
    }
    app
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_detection() {
        assert!(looks_like_asset("/_next/static/chunks/app.js"));
        assert!(looks_like_asset("/favicon.ico"));
        assert!(!looks_like_asset("/settings"));
        assert!(!looks_like_asset("/queue.html"));
        assert!(!looks_like_asset("/"));
    }
}
