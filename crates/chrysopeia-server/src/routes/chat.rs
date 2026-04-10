//! Chat API route handlers.

use axum::{Json, Router, extract::State, routing::{get, post}};
use serde::{Deserialize, Serialize};

use crate::state::AppState;
use chrysopeia_core::models::{ChatMessage, ChatRole};

/// Build chat-specific routes.
pub fn chat_routes() -> Router<AppState> {
    Router::new()
        .route("/", post(send_message))
        .route("/history", get(get_history))
}

/// Request body for sending a chat message.
#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
}

/// Response from the chat endpoint.
#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub user_message: ChatMessage,
    pub assistant_message: ChatMessage,
}

/// POST /api/chat - Send a message and get an interpreted response.
async fn send_message(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Json<ChatResponse> {
    let now = chrono::Utc::now();
    let user_msg = ChatMessage {
        id: uuid::Uuid::new_v4(),
        role: ChatRole::User,
        content: req.message.clone(),
        timestamp: now,
        job_id: None,
    };

    // Interpret the user's message and produce a response
    let response_content = crate::chat::interpret_command(&req.message, &state).await;

    let assistant_msg = ChatMessage {
        id: uuid::Uuid::new_v4(),
        role: ChatRole::Assistant,
        content: response_content,
        timestamp: chrono::Utc::now(),
        job_id: None,
    };

    // TODO: Persist both messages to database

    Json(ChatResponse {
        user_message: user_msg,
        assistant_message: assistant_msg,
    })
}

/// GET /api/chat/history - Get chat message history.
async fn get_history(State(state): State<AppState>) -> Json<Vec<ChatMessage>> {
    let _ = state;
    // TODO: Query database for chat history
    Json(vec![])
}
