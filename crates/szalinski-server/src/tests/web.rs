//! Static UI hosting, headers, and the WebSocket.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

use super::support::*;

fn with_web_dir(index: &'static str) -> TestOptions {
    TestOptions {
        configure: Box::new(move |c, root| {
            let web = root.join("web");
            std::fs::create_dir_all(web.join("_next/static/chunks")).unwrap();
            std::fs::write(web.join("index.html"), index).unwrap();
            std::fs::write(
                web.join("_next/static/chunks/app-123.js"),
                "console.log('hi');".repeat(200),
            )
            .unwrap();
            std::fs::write(web.join("favicon.ico"), [0u8, 1, 2]).unwrap();
            c.web_dir = web;
        }),
        ..TestOptions::default()
    }
}

#[tokio::test]
async fn serves_the_ui_with_spa_fallback_and_cache_headers() {
    let app = TestApp::start(with_web_dir("<html>szalinski ui</html>")).await;
    let r = app.get("/").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text.contains("szalinski ui"));
    assert_eq!(r.headers["cache-control"], "no-cache");
    assert_eq!(r.headers["x-content-type-options"], "nosniff");
    assert_eq!(r.headers["referrer-policy"], "same-origin");
    assert_eq!(r.headers["x-frame-options"], "SAMEORIGIN");

    let r = app.get("/_next/static/chunks/app-123.js").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.headers["cache-control"],
        "public, max-age=31536000, immutable"
    );

    // Client-side routes get index.html; missing assets get a real 404.
    let r = app.get("/library/abc").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text.contains("szalinski ui"));
    let r = app.get("/_next/static/chunks/missing.js").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(!r.text.contains("szalinski ui"));
    // Traversal out of the web dir is refused.
    let r = app.get("/../data/szalinski.db").await;
    assert_ne!(r.status, StatusCode::OK, "{}", r.text);

    // API paths never fall through to the UI.
    for path in ["/api/unknown", "/api/", "/api"] {
        let r = app.get(path).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(r.json["code"], "not_found", "{path}");
    }
    assert_eq!(r.headers["x-content-type-options"], "nosniff");
}

#[tokio::test]
async fn gzip_when_accepted() {
    let app = TestApp::start(with_web_dir("<html>ui</html>")).await;
    let req = Request::builder()
        .uri("/_next/static/chunks/app-123.js")
        .header("accept-encoding", "gzip")
        .body(Body::empty())
        .unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-encoding"], "gzip");
}

#[tokio::test]
async fn cors_only_with_dev_cors() {
    async fn allow_origin(app: &TestApp) -> Option<String> {
        let req = Request::builder()
            .uri("/api/health")
            .header("origin", "http://localhost:3000")
            .body(Body::empty())
            .unwrap();
        let resp = app.router.clone().oneshot(req).await.unwrap();
        resp.headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap().to_string())
    }
    let app = TestApp::new().await;
    assert_eq!(allow_origin(&app).await, None);
    let app = TestApp::start(TestOptions {
        configure: Box::new(|c, _| c.dev_cors = true),
        ..TestOptions::default()
    })
    .await;
    assert_eq!(allow_origin(&app).await.as_deref(), Some("*"));
}

#[tokio::test]
async fn missing_ui_explains_itself() {
    let app = TestApp::new().await;
    let r = app.get("/").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text.contains("/api/health"));
    assert!(r.text.contains("web interface"));
    let r = app.get("/settings").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app.get("/api/health").await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn websocket_sends_initial_state_then_events() {
    let app = TestApp::new().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app.router.clone();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws"))
        .await
        .unwrap();

    async fn next_json(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Value {
        loop {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
                .await
                .expect("no message in time")
                .expect("socket closed")
                .unwrap();
            if let Message::Text(t) = msg {
                return serde_json::from_str(t.as_str()).unwrap();
            }
        }
    }

    let first = next_json(&mut ws).await;
    assert_eq!(first["type"], "queue.state");
    assert!(first["max_jobs"].is_number());
    let second = next_json(&mut ws).await;
    assert_eq!(second["type"], "stats.updated");
    assert_eq!(second["totals"]["file_count"], 0);

    // Events flow: pausing publishes queue.state; settings changes too.
    // Other queue.state events (e.g. from start-up) may come first.
    let r = app.post_empty("/api/queue/pause").await;
    assert_eq!(r.status, StatusCode::OK);
    let mut seen_pause = false;
    for _ in 0..10 {
        let ev = next_json(&mut ws).await;
        if ev["type"] == "queue.state" && ev["paused"] == true {
            seen_pause = true;
            break;
        }
    }
    assert!(seen_pause);
    app.patch("/api/settings", json!({ "onboarded": true }))
        .await;
    let mut seen_settings = false;
    for _ in 0..5 {
        let ev = next_json(&mut ws).await;
        if ev["type"] == "settings.updated" {
            assert_eq!(ev["settings"]["onboarded"], true);
            seen_settings = true;
            break;
        }
    }
    assert!(seen_settings);
    ws.send(Message::Close(None)).await.unwrap();
}
