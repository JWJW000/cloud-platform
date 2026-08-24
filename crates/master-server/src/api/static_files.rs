//! 生产静态资源托管与 SPA 回退（V4 方案第 7 节）。
//!
//! 修复目标（V4-01）：镜像内构建产物在 `/app/admin-web/dist`，而配置里写的是
//! `web_root = "web-admin"`，导致生产首页与 `/assets/*` 全部 404。
//! （V5 起前端切换为 admin-web，镜像内路径为 `/app/admin-web/dist`。）
//!
//! 本模块同时落实第 7.2 节的其余约束：
//! 1. 配置了 `web_root` 但目录或 `index.html` 不存在时，启动必须失败（fail fast）；
//! 2. 静态资源请求优先读取真实文件；
//! 3. 非 `/api` 且没有对应文件的前端路由回退到 `index.html`；
//! 4. `/api/*` 绝不回退到 HTML，一律返回 JSON 404；
//! 5. 哈希资源（`/assets/*`）设置长期缓存，`index.html` 不缓存。

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{Method, Request, Response, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use percent_encoding::percent_decode_str;
use serde_json::json;
use tokio::io::AsyncReadExt;

/// 启动前校验 `web_root`：目录必须存在且包含 `index.html`。
///
/// 缺失时直接返回错误，让 Master 拒绝启动，而不是静默地只提供 API——
/// 那样生产环境会得到一个「后台接口都在、首页却 404」的假在线。
pub fn validate_web_root(web_root: &Path) -> anyhow::Result<()> {
    if !web_root.is_dir() {
        anyhow::bail!("web_root 目录不存在：{}", web_root.display());
    }
    if !web_root.join("index.html").is_file() {
        anyhow::bail!("web_root 下缺少 index.html：{}", web_root.display());
    }
    Ok(())
}

/// SPA 静态资源回退处理器。
///
/// 规则：
/// - `/api/*`：返回 JSON 404，绝不回退到 HTML；
/// - 其余路径：若存在对应真实文件则直接返回该文件；
/// - 其余路径且无真实文件：对 GET/HEAD 回退到 `index.html`（前端路由刷新场景）；
/// - 非 GET/HEAD 且无真实文件：返回 405。
pub async fn spa_fallback(State(root): State<Arc<PathBuf>>, req: Request<Body>) -> Response<Body> {
    let uri_path = req.uri().path();
    let method = req.method().clone();

    // /api/* 绝不回退到 HTML：前端路由不存在该路径，接口也不存在，返回 JSON 404。
    if uri_path.starts_with("/api/") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "接口不存在" })),
        )
            .into_response();
    }

    let rel = uri_path.trim_start_matches('/');
    match resolve_within_root(&root, rel) {
        Some(file) if file.is_file() => serve_file(file, uri_path).await,
        _ => {
            if method == Method::GET || method == Method::HEAD {
                serve_index(&root).await
            } else {
                StatusCode::METHOD_NOT_ALLOWED.into_response()
            }
        }
    }
}

/// 把相对路径安全地解析到 web_root 内部。
///
/// 拒绝空路径、`.`、`..`、绝对路径与盘符，并做百分号解码，防止
/// `%2e%2e` 之类的编码逃逸；最终路径必须落在 root 之下。
fn resolve_within_root(root: &Path, rel: &str) -> Option<PathBuf> {
    if rel.is_empty() {
        return Some(root.join("index.html"));
    }
    let decoded = percent_decode_str(rel).decode_utf8().ok()?;
    let decoded = decoded.trim_start_matches('/');
    if decoded.is_empty() {
        return Some(root.join("index.html"));
    }
    let candidate = Path::new(decoded);
    if candidate.is_absolute() {
        return None;
    }
    for component in candidate.components() {
        match component {
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => return None,
            Component::CurDir => {}
            Component::Normal(_) => {}
        }
    }
    let joined = root.join(candidate);
    if joined.starts_with(root) {
        Some(joined)
    } else {
        None
    }
}

/// 返回一个静态文件，按扩展名给 MIME，按路径给缓存策略。
async fn serve_file(path: PathBuf, uri_path: &str) -> Response<Body> {
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "读取静态文件失败");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let mut bytes = Vec::with_capacity(64 * 1024);
    if let Err(err) = file.read_to_end(&mut bytes).await {
        tracing::warn!(path = %path.display(), error = %err, "读取静态文件失败");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let mime = mime_for(&path);
    // 哈希资源长期缓存；其余（主要是 index.html）不缓存，保证发版后立即生效。
    let cache = if uri_path.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    let mut resp = Response::new(Body::from(bytes));
    resp.headers_mut().insert(
        CONTENT_TYPE,
        mime.parse()
            .unwrap_or_else(|_| "application/octet-stream".parse().unwrap()),
    );
    if let Ok(val) = axum::http::HeaderValue::from_str(cache) {
        resp.headers_mut().insert(CACHE_CONTROL, val);
    }
    resp
}

/// 返回 index.html（SPA 入口）。
async fn serve_index(root: &Path) -> Response<Body> {
    let index_path = root.join("index.html");
    let mut file = match tokio::fs::File::open(&index_path).await {
        Ok(f) => f,
        Err(err) => {
            tracing::error!(path = %index_path.display(), error = %err, "读取 index.html 失败");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let mut bytes = Vec::with_capacity(16 * 1024);
    let _ = file.read_to_end(&mut bytes).await;

    let mut resp = Response::new(Body::from(bytes));
    resp.headers_mut()
        .insert(CONTENT_TYPE, "text/html; charset=utf-8".parse().unwrap());
    resp.headers_mut()
        .insert(CACHE_CONTROL, "no-cache".parse().unwrap());
    resp
}

/// 扩展名到 MIME 的映射（覆盖前端构建产物的常见类型）。
fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("map") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request as HttpRequest;
    use axum::Router;
    use tower::ServiceExt;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("index.html"), "<html>首页</html>").unwrap();
        std::fs::write(dir.path().join("assets/app-hash123.js"), "console.log(1)").unwrap();
        dir
    }

    fn app(dir: &tempfile::TempDir) -> Router {
        Router::new()
            .fallback(spa_fallback)
            .with_state(Arc::new(dir.path().to_path_buf()))
    }

    #[tokio::test]
    async fn root_serves_index_html() {
        let dir = fixture();
        let resp = app(&dir)
            .oneshot(HttpRequest::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("text/html"));
        let body = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("首页"));
    }

    #[tokio::test]
    async fn hashed_asset_is_served_with_long_cache() {
        let dir = fixture();
        let resp = app(&dir)
            .oneshot(
                HttpRequest::builder()
                    .uri("/assets/app-hash123.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("javascript"));
        assert_eq!(
            resp.headers().get(CACHE_CONTROL).unwrap(),
            "public, max-age=31536000, immutable"
        );
    }

    #[tokio::test]
    async fn spa_route_falls_back_to_index_html() {
        // 前端路由 /workers 刷新时没有真实文件，必须回退到 index.html
        let dir = fixture();
        let resp = app(&dir)
            .oneshot(
                HttpRequest::builder()
                    .uri("/workers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("首页"));
    }

    #[tokio::test]
    async fn api_missing_returns_json_404_not_html() {
        let dir = fixture();
        let resp = app(&dir)
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/not-found")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("接口不存在"));
        assert!(!text.contains("<html"));
    }

    #[tokio::test]
    async fn encoded_traversal_is_rejected() {
        let dir = fixture();
        // %2e%2e = ".."，必须被拒绝而不是解析到目录之外
        let resp = app(&dir)
            .oneshot(
                HttpRequest::builder()
                    .uri("/%2e%2e/%2e%2e/etc/passwd")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK); // 回退到 index.html
        let body = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("首页"));
    }

    #[test]
    fn validate_web_root_requires_index_html() {
        let dir = fixture();
        assert!(validate_web_root(dir.path()).is_ok());
        let empty = tempfile::tempdir().unwrap();
        assert!(validate_web_root(empty.path()).is_err());
        assert!(validate_web_root(Path::new("/nonexistent-dir-xyz")).is_err());
    }
}
