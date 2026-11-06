use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use utachat_shared::ApiError;

pub struct AppError {
    pub status: StatusCode,
    pub message: String,
}

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "Session expired or invalid")
    }

    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "You don't have permission to do that")
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "Resource not found")
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, Json(ApiError { error: self.message })).into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(%error, "database operation failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Database operation failed")
    }
}

pub type ApiResult<T> = Result<T, AppError>;
