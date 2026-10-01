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

/// Say what is wrong with one value in plain words, from serde's message
/// about it (`unknown variant `x`, expected one of `a`, `b``, `invalid type:
/// string "x", expected u32`, …). `field` is the value's path, when known.
pub fn describe_value_error(field: Option<&str>, serde_message: &str) -> String {
    let lead = match field {
        Some(f) => format!("The value for \"{f}\" isn't valid."),
        None => "A value isn't valid.".to_string(),
    };
    let msg = serde_message.trim();
    let quoted = |text: &str| -> Vec<String> {
        text.split('`')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect()
    };
    let hint = if let Some(rest) = msg.strip_prefix("unknown variant ") {
        let choices = rest
            .split_once("expected")
            .map(|(_, list)| quoted(list))
            .unwrap_or_default();
        if choices.is_empty() {
            "Pick one of the listed choices.".to_string()
        } else {
            format!("Choose one of: {}.", choices.join(", "))
        }
    } else if let Some(rest) = msg.strip_prefix("unknown field ") {
        let name = quoted(rest).into_iter().next().unwrap_or_default();
        format!("There's no setting called \"{name}\" here.")
    } else if msg.starts_with("missing field ") {
        let name = quoted(msg).into_iter().next().unwrap_or_default();
        format!("\"{name}\" is missing.")
    } else if msg.starts_with("invalid digit") || msg.starts_with("cannot parse integer") {
        // A number in the address (`?offset=-1`) that isn't a count.
        "It must be a whole number, 0 or more.".to_string()
    } else if msg.starts_with("number too large") {
        "It's too large.".to_string()
    } else if msg.starts_with("invalid float literal") {
        "It must be a number.".to_string()
    } else if msg.starts_with("invalid type") || msg.starts_with("invalid value") {
        let expected = msg.rsplit_once("expected ").map_or("", |(_, e)| e);
        match expected {
            e if e.starts_with('u') || e.starts_with('i') || e.contains("integer") => {
                "It must be a whole number.".to_string()
            }
            e if e.starts_with('f') => "It must be a number.".to_string(),
            "a boolean" => "It must be true or false.".to_string(),
            e if e.contains("string") => "It must be text.".to_string(),
            e if e.contains("sequence") => "It must be a list.".to_string(),
            e if e.contains("map") || e.starts_with("struct") => {
                "It must be an object with its own settings.".to_string()
            }
            _ => String::new(),
        }
    } else {
        String::new()
    };
    if hint.is_empty() {
        lead
    } else {
        format!("{lead} {hint}")
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rej: JsonRejection) -> Self {
        match rej {
            JsonRejection::MissingJsonContentType(_) => Self::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
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
                match field_of(&detail) {
                    Some(field) => {
                        let serde_message =
                            detail.split_once(": ").map_or(detail.as_str(), |(_, m)| m);
                        Self::bad_request(
                            "invalid_request",
                            describe_value_error(Some(&field), serde_message),
                        )
                        .with_field(field)
                    }
                    None => Self::bad_request(
                        "invalid_request",
                        format!("The request has a wrong or missing field ({detail})."),
                    ),
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
        // Plain words, naming the parameter; never the parser's own text.
        let detail = rejection_detail(&rej.body_text());
        match field_of(&detail) {
            Some(field) => {
                let serde_message = detail.split_once(": ").map_or(detail.as_str(), |(_, m)| m);
                let message = describe_value_error(Some(&field), serde_message).replacen(
                    "The value for",
                    "The value of",
                    1,
                );
                Self::bad_request("invalid_query", message).with_field(field)
            }
            None => Self::bad_request(
                "invalid_query",
                "Something after the ? in the address isn't valid. Check the names and values \
                 there.",
            ),
        }
    }
}

impl From<PathRejection> for ApiError {
    fn from(rej: PathRejection) -> Self {
        use axum::extract::path::ErrorKind;
        let message = match &rej {
            PathRejection::FailedToDeserializePathParams(e) => match e.kind() {
                ErrorKind::ParseErrorAtKey {
                    key,
                    value,
                    expected_type,
                } => path_value_problem(value, key == "id" || expected_type.contains("Uuid")),
                ErrorKind::ParseErrorAtIndex {
                    value,
                    expected_type,
                    ..
                }
                | ErrorKind::ParseError {
                    value,
                    expected_type,
                } => path_value_problem(value, expected_type.contains("Uuid")),
                ErrorKind::DeserializeError {
                    key,
                    value,
                    message,
                } => path_value_problem(value, key == "id" || message.contains("UUID")),
                ErrorKind::InvalidUtf8InPathParam { .. } => {
                    "This address has characters in it that aren't valid text.".to_string()
                }
                _ => "This address isn't valid.".to_string(),
            },
            _ => "This address isn't valid.".to_string(),
        };
        Self::bad_request("invalid_path_param", message)
    }
}

/// Why a value in the address isn't valid, in plain words. `id`: the value
/// should have been an id (every id in the API is a UUID).
fn path_value_problem(value: &str, id: bool) -> String {
    const SHOWN: usize = 60;
    let shown: String = if value.chars().count() > SHOWN {
        format!("{}…", value.chars().take(SHOWN).collect::<String>())
    } else {
        value.to_string()
    };
    if id {
        format!(
            "\"{shown}\" isn't a valid id. Ids look like 0b2f6c1e-5d0a-4c4e-9a53-2f1d7c0e8b41; use \
             one from another answer of this API."
        )
    } else {
        format!("\"{shown}\" isn't valid in this address.")
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

    #[tokio::test]
    async fn a_body_that_is_not_json_is_refused_with_one_code() {
        use axum::extract::FromRequest as _;
        let req = axum::http::Request::builder()
            .method("PATCH")
            .header("content-type", "text/plain")
            .body(axum::body::Body::from("{}"))
            .unwrap();
        let rej = Json::<serde_json::Value>::from_request(req, &())
            .await
            .unwrap_err();
        let e = ApiError::from(rej);
        assert_eq!(e.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(e.code, "unsupported_media_type");
    }

    #[test]
    fn value_errors_are_plain() {
        assert_eq!(
            describe_value_error(
                Some("default_profile.quality"),
                "unknown variant `ultra`, expected one of `smallest`, `small`, `balanced`"
            ),
            "The value for \"default_profile.quality\" isn't valid. Choose one of: smallest, \
             small, balanced."
        );
        assert_eq!(
            describe_value_error(
                Some("max_jobs"),
                "invalid type: string \"lots\", expected u32"
            ),
            "The value for \"max_jobs\" isn't valid. It must be a whole number."
        );
        assert_eq!(
            describe_value_error(
                Some("auto_queue"),
                "invalid type: integer `1`, expected a boolean"
            ),
            "The value for \"auto_queue\" isn't valid. It must be true or false."
        );
        assert_eq!(
            describe_value_error(None, "something odd"),
            "A value isn't valid."
        );
    }

    /// Rejections of the address or its query string say what is wrong in
    /// plain words, never with the parser's own text.
    #[tokio::test]
    async fn address_errors_are_plain() {
        use axum::extract::{FromRequestParts as _, Query};

        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code)]
        struct Page {
            offset: Option<u32>,
            limit: Option<u32>,
        }
        let query = |q: &str| {
            let req = axum::http::Request::builder()
                .uri(format!("/x?{q}"))
                .body(())
                .unwrap();
            let (mut parts, ()) = req.into_parts();
            async move {
                ApiError::from(
                    Query::<Page>::from_request_parts(&mut parts, &())
                        .await
                        .unwrap_err(),
                )
            }
        };
        let e = query("offset=-1").await;
        assert_eq!(e.code, "invalid_query");
        assert_eq!(
            e.message,
            "The value of \"offset\" isn't valid. It must be a whole number, 0 or more."
        );
        assert_eq!(e.field.as_deref(), Some("offset"));
        let e = query("limit=abc").await;
        assert_eq!(
            e.message,
            "The value of \"limit\" isn't valid. It must be a whole number, 0 or more."
        );
        let e = query("limit=99999999999").await;
        assert!(e.message.ends_with("It's too large."), "{}", e.message);

        assert_eq!(
            path_value_problem("not-a-uuid", true),
            "\"not-a-uuid\" isn't a valid id. Ids look like \
             0b2f6c1e-5d0a-4c4e-9a53-2f1d7c0e8b41; use one from another answer of this API."
        );
        assert_eq!(
            path_value_problem(&"x".repeat(100), false),
            format!("\"{}…\" isn't valid in this address.", "x".repeat(60))
        );
    }

    #[test]
    fn internal_hides_details() {
        let e = ApiError::internal("disk I/O error at page 42");
        assert_eq!(e.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(e.message, INTERNAL_MESSAGE);
    }
}
