//! API errors: `{"error": "<plain sentence>", "code": "<snake_case>"}`.
//!
//! Client errors carry a sentence the UI can show as is. Server errors (500)
//! show a generic sentence; the details go to the log. Validation errors
//! caused by one setting or request field also name it:
//! `{"error", "code", "field": "temp_dir"}`.

use std::borrow::Cow;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Sentence shown for unexpected server-side failures.
pub const INTERNAL_MESSAGE: &str =
    "Something went wrong on the server. The details are in the server log.";

/// An error returned by an API handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// HTTP status.
    pub status: StatusCode,
    /// Stable snake_case identifier for the UI.
    pub code: Cow<'static, str>,
    /// Plain-language sentence.
    pub message: String,
    /// The settings key or request field at fault, when one is.
    pub field: Option<Cow<'static, str>>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    code: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<&'a str>,
}

impl ApiError {
    /// Build an error with any status.
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code: Cow::Borrowed(code),
            message: message.into(),
            field: None,
        }
    }

    /// Name the field at fault: a settings key such as `temp_dir`, or a
    /// request field such as `profile` or `name`.
    #[must_use]
    pub fn with_field(mut self, field: impl Into<Cow<'static, str>>) -> Self {
        self.field = Some(field.into());
        self
    }

    /// Name `field` as the one at fault, unless the error already names one
    /// or isn't about the request (server errors).
    #[must_use]
    pub fn about(self, field: &'static str) -> Self {
        if self.field.is_none() && self.status.is_client_error() {
            self.with_field(field)
        } else {
            self
        }
    }

    /// 400 Bad Request.
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    /// 404 Not Found.
    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    /// 409 Conflict.
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    /// 403 Forbidden.
    pub fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    /// 500 with the generic sentence. The cause is logged here.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        tracing::error!("request failed: {cause:#}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            INTERNAL_MESSAGE,
        )
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: &self.message,
            code: &self.code,
            field: self.field.as_deref(),
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self::internal(format!("database: {e}"))
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::internal(format!("{e:#}"))
    }
}

/// The useful part of an axum rejection text, without its generic prefix
/// or the JSON line/column position.
fn rejection_detail(text: &str) -> String {
    let mut detail = text;
    for prefix in [
        "Failed to deserialize the JSON body into the target type: ",
        "Failed to parse the request body as JSON: ",
        "Failed to deserialize query string: ",
        "Failed to deserialize query string. Error: ",
        "Invalid URL: ",
    ] {
        if let Some(rest) = detail.strip_prefix(prefix) {
            detail = rest;
        }
    }
    let detail = match detail.rfind(" at line ") {
        Some(i) if detail[i..].contains(" column ") => &detail[..i],
        _ => detail,
    };
    detail.trim().trim_end_matches('.').to_string()
}

/// The field a JSON data error is about: the path serde reports before the
/// first `": "` (e.g. `profile.quality` in "profile.quality: unknown
/// variant"), when there is one.
fn field_of(detail: &str) -> Option<String> {
    let (path, _) = detail.split_once(": ")?;
    let valid = !path.is_empty()
        && path != "."
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '[' | ']'));
    valid.then(|| path.to_string())
}

impl From<JsonRejection> for ApiError {
    fn from(rej: JsonRejection) -> Self {
        match rej {
            JsonRejection::MissingJsonContentType(_) => Self::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "invalid_json",
                "Send the request body as JSON (Content-Type: application/json).",
            ),
            JsonRejection::JsonSyntaxError(e) => Self::bad_request(
                "invalid_json",
                format!(
                    "The request body isn't valid JSON ({}).",
                    rejection_detail(&e.body_text())
                ),
            ),
            JsonRejection::JsonDataError(e) => {
                let detail = rejection_detail(&e.body_text());
                let error = Self::bad_request(
                    "invalid_request",
                    format!("The request has a wrong or missing field ({detail})."),
                );
                match field_of(&detail) {
                    Some(field) => error.with_field(field),
                    None => error,
                }
            }
            JsonRejection::BytesRejection(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                Self::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "body_too_large",
                    "The request body is too large (the limit is 1 MB).",
                )
            }
            other => Self::bad_request(
                "invalid_request",
                format!(
                    "The request couldn't be read ({}).",
                    rejection_detail(&other.body_text())
                ),
            ),
        }
    }
}

impl From<QueryRejection> for ApiError {
    fn from(rej: QueryRejection) -> Self {
        Self::bad_request(
            "invalid_query",
            format!(
                "The query string isn't valid ({}).",
                rejection_detail(&rej.body_text())
            ),
        )
    }
}

impl From<PathRejection> for ApiError {
    fn from(rej: PathRejection) -> Self {
        Self::bad_request(
            "invalid_path_param",
            format!(
                "The address isn't valid ({}).",
                rejection_detail(&rej.body_text())
            ),
        )
    }
}

/// Result type for handlers.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn error_body_shape() {
        let resp = ApiError::conflict("library_exists", "That folder is already a library.")
            .into_response();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["code"], "library_exists");
        assert_eq!(v["error"], "That folder is already a library.");
        assert!(v.get("field").is_none());

        let resp = ApiError::bad_request("invalid_settings", "Pick a folder.")
            .with_field("temp_dir")
            .into_response();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["field"], "temp_dir");
        assert_eq!(v["code"], "invalid_settings");
    }

    #[test]
    fn rejection_details_are_trimmed() {
        assert_eq!(
            rejection_detail(
                "Failed to deserialize the JSON body into the target type: bogus: unknown field `bogus` at line 1 column 8"
            ),
            "bogus: unknown field `bogus`"
        );
    }

    #[test]
    fn data_errors_name_their_field() {
        assert_eq!(
            field_of("profile.quality: unknown variant `x`").as_deref(),
            Some("profile.quality")
        );
        assert_eq!(field_of("missing field `path`"), None);
        assert_eq!(field_of(".: invalid type: string"), None);
        assert_eq!(field_of("expected value: at position 3"), None);
    }

    #[test]
    fn internal_hides_details() {
        let e = ApiError::internal("disk I/O error at page 42");
        assert_eq!(e.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(e.message, INTERNAL_MESSAGE);
    }
}
