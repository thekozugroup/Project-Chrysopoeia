//! Extractors whose rejections use the API error shape.

use axum::Json;
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// JSON body; malformed bodies become `{"error","code"}` 400s.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(v)| Self(v))
            .map_err(ApiError::from)
    }
}

/// Whether a request says its body is JSON (`application/json`, or a
/// `+json` type).
fn is_json(req: &Request) -> bool {
    req.headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .is_some_and(|v| v == "application/json" || v.ends_with("+json"))
}

/// Optional JSON body: an empty body means `T::default()`. A body that is
/// present must be sent as JSON, which (with the origin check) keeps plain
/// HTML forms on other sites from reaching the API.
#[derive(Debug, Clone, Copy, Default)]
pub struct OptionalJson<T>(pub T);

impl<S, T> FromRequest<S> for OptionalJson<T>
where
    T: DeserializeOwned + Default,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let json = is_json(&req);
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|e| {
                if e.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
                    ApiError::new(
                        e.status(),
                        "body_too_large",
                        "The request body is too large (the limit is 1 MB).",
                    )
                } else {
                    ApiError::bad_request("invalid_request", "The request body couldn't be read.")
                }
            })?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self(T::default()));
        }
        if !json {
            return Err(ApiError::new(
                axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                "Send the request body as JSON (Content-Type: application/json).",
            ));
        }
        parse_json_body(&bytes).map(Self)
    }
}

/// Read a JSON body, naming the value at fault (`profile.quality`) when it
/// doesn't fit. Errors are in plain words, the same as for [`ApiJson`].
fn parse_json_body<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    let mut reader = serde_json::Deserializer::from_slice(bytes);
    let value = serde_path_to_error::deserialize::<_, T>(&mut reader)
        .map_err(|e| ApiError::from_json_error(&e.path().to_string(), e.inner()))?;
    // Anything after the value is not JSON either.
    reader
        .end()
        .map_err(|e| ApiError::from_json_error(".", &e))?;
    Ok(value)
}

/// Query string.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(v)| Self(v))
            .map_err(ApiError::from)
    }
}

/// Path parameters.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiPath<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(v)| Self(v))
            .map_err(ApiError::from)
    }
}
