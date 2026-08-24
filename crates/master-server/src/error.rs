//! 统一错误类型：一处定义 HTTP 状态码与中文提示，避免每个处理函数各写一套。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

/// Master 内部错误。
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// 请求参数不合法。
    #[error("{0}")]
    BadRequest(String),
    /// 未登录或凭据无效。
    #[error("{0}")]
    Unauthorized(String),
    /// 已登录但权限不足。
    #[error("{0}")]
    Forbidden(String),
    /// 目标对象不存在。
    #[error("{0}")]
    NotFound(String),
    /// 与当前状态冲突（例如非法状态迁移）。
    #[error("{0}")]
    Conflict(String),
    /// 请求过于频繁被限流。
    #[error("{0}")]
    TooManyRequests(String),
    /// 数据库错误。
    #[error("数据库操作失败：{0}")]
    Database(#[from] sqlx::Error),
    /// 其他内部错误。
    #[error("内部错误：{0}")]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    /// 参数错误的便捷构造。
    pub fn bad(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    /// 未找到的便捷构造。
    pub fn missing(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    /// 状态冲突的便捷构造。
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    /// 未认证的便捷构造。
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::Unauthorized(message.into())
    }

    /// 权限不足的便捷构造。
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden(message.into())
    }

    /// 限流的便捷构造。
    pub fn too_many(message: impl Into<String>) -> Self {
        Self::TooManyRequests(message.into())
    }

    /// 内部错误的便捷构造。
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(anyhow::anyhow!(message.into()))
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::Database(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// 错误响应体：前端只需读 `message` 即可直接展示中文提示。
#[derive(Debug, Serialize)]
struct ErrorBody {
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message = self.to_string();
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(错误 = %message, "请求处理失败");
        }
        (status, Json(ErrorBody { message })).into_response()
    }
}

/// 处理函数返回类型别名。
pub type AppResult<T> = Result<T, AppError>;

impl From<platform_domain::TransitionError> for AppError {
    fn from(value: platform_domain::TransitionError) -> Self {
        Self::Conflict(value.to_string())
    }
}

impl From<platform_domain::EnumParseError> for AppError {
    fn from(value: platform_domain::EnumParseError) -> Self {
        Self::BadRequest(value.to_string())
    }
}
