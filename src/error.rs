use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("missing or invalid API key")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("{0}")]
    BadRequest(String),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    /// The detail is for the log. The body says only that the request failed.
    #[error("aws request failed")]
    Aws(String),
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        Self::Aws(format!("{e:#}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = Json(json!({ "error": self.to_string() }));
        match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
                body,
            )
                .into_response(),
            Self::Forbidden => (StatusCode::FORBIDDEN, body).into_response(),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, body).into_response(),
            Self::NotFound => (StatusCode::NOT_FOUND, body).into_response(),
            Self::Conflict(_) => (StatusCode::CONFLICT, body).into_response(),
            Self::Aws(detail) => {
                tracing::error!(%detail, "aws request failed");
                (StatusCode::BAD_GATEWAY, body).into_response()
            }
        }
    }
}
