//! 管理后台 REST API 路由装配（第 16 节）。

#![allow(missing_docs)]

pub mod account_registration_batches;
pub mod account_registration_tasks;
pub mod accounts;
pub mod auth;
pub mod batches;
pub mod books;
pub mod catalog_v1;
pub mod enroll_codes;
pub mod events;
pub mod health;
pub mod imports;
pub mod inventory;
pub mod logs;
pub mod mail_provider;
pub mod manual_actions;
pub mod outlook_accounts;
pub mod overview;
pub mod proxies;
pub mod publishers;
pub mod sessions;
pub mod settings;
pub mod static_files;
pub mod tasks;
pub mod webhook;
pub mod workers;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::{
    ACCEPT, AUTHORIZATION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, ORIGIN, REFERRER_POLICY,
    STRICT_TRANSPORT_SECURITY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderName, HeaderValue, Method, Request, Response};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

/// 安全响应头中间件。
async fn security_headers_middleware(req: Request<Body>, next: Next) -> Response<Body> {
    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();

    headers.insert(
        STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self';",
        ),
    );
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("geolocation=(), camera=(), microphone=()"),
    );

    resp
}

/// CSRF 防护（V4 第 13.4 节）：不安全方法（写操作）必须携带匹配的 Origin。
///
/// - 同源（Origin 与 Host 一致）放行；
/// - 与 `site_base` 一致放行（前端与 API 同源部署时的公共源；开发代理改写 Host
///   或浏览器用 localhost 访问时也能通过）；
/// - 显式配置的允许 Origin（`allowed_origins`）放行；
/// - 其他 Origin 一律 403；
/// - 无 Origin 头（同源导航、curl）放行——Cookie 由 SameSite=Lax 约束跨站携带。
async fn csrf_origin_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    let method = req.method().clone();
    if matches!(
        method,
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    ) {
        if let Some(origin) = req.headers().get(ORIGIN).and_then(|v| v.to_str().ok()) {
            let origin = origin.trim_end_matches('/');
            let host = req
                .headers()
                .get(axum::http::header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let same_origin =
                format!("https://{host}") == origin || format!("http://{host}") == origin;
            let site_base = state.config.server.site_base.trim().trim_end_matches('/');
            let matches_site_base = !site_base.is_empty() && site_base == origin;
            let allowed = state
                .config
                .server
                .allowed_origins
                .iter()
                .any(|o| o.trim_end_matches('/') == origin);
            if !same_origin && !matches_site_base && !allowed {
                return (
                    axum::http::StatusCode::FORBIDDEN,
                    axum::Json(serde_json::json!({ "message": "跨站请求被拒绝（Origin 不匹配）" })),
                )
                    .into_response();
            }
        }
    }
    next.run(req).await
}

/// 构造管理后台 API 的 Axum 路由树。
pub fn router(state: AppState) -> Router {
    let allowed_origins = &state.config.server.allowed_origins;
    let cors = if !allowed_origins.is_empty() {
        let mut origin_headers = Vec::new();
        for o in allowed_origins {
            if let Ok(val) = o.parse::<HeaderValue>() {
                origin_headers.push(val);
            }
        }
        CorsLayer::new()
            .allow_origin(origin_headers)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                AUTHORIZATION,
                CONTENT_TYPE,
                ACCEPT,
                HeaderName::from_static("x-requested-with"),
            ])
            .allow_credentials(true)
    } else {
        CorsLayer::new()
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                AUTHORIZATION,
                CONTENT_TYPE,
                ACCEPT,
                HeaderName::from_static("x-requested-with"),
            ])
    };

    Router::new()
        // 认证与个人
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/me", get(auth::me))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/password", put(auth::change_password))
        // 总览与统计
        .route("/api/overview", get(overview::get_overview))
        .route("/api/overview/stats", get(overview::get_stats))
        .route(
            "/api/overview/recent-executions",
            get(overview::get_recent_executions),
        )
        // Worker 节点与槽位
        .route("/api/workers", get(workers::list_workers))
        .route("/api/workers/:id", get(workers::get_worker))
        .route("/api/workers/:id/approve", post(workers::approve_worker))
        .route("/api/workers/:id/reject", post(workers::reject_worker))
        .route("/api/workers/:id/disable", post(workers::disable_worker))
        .route("/api/workers/:id/enable", post(workers::enable_worker))
        .route(
            "/api/workers/:id/status",
            put(workers::update_worker_status),
        )
        .route(
            "/api/workers/:id/capacity",
            put(workers::update_worker_capacity),
        )
        .route(
            "/api/workers/:id/diagnostics",
            put(workers::update_worker_diagnostics),
        )
        .route("/api/workers/:id/pause", post(workers::pause_worker))
        .route("/api/workers/:id/resume", post(workers::resume_worker))
        .route("/api/workers/:id/slots", get(workers::list_worker_slots))
        .route("/api/slots", get(workers::list_all_slots))
        .route(
            "/api/workers/:id/certificates",
            get(workers::list_worker_certificates),
        )
        .route(
            "/api/workers/certificates/:fingerprint/revoke",
            post(workers::revoke_certificate),
        )
        // 注册码
        .route(
            "/api/enroll-codes",
            get(enroll_codes::list_enroll_codes).post(enroll_codes::create_enroll_code),
        )
        .route(
            "/api/enroll-codes/:code",
            delete(enroll_codes::delete_enroll_code),
        )
        // 图书主数据
        .route("/api/books", get(books::list_books))
        .route("/api/books/:id", get(books::get_book))
        .route("/api/books/:id/files", get(books::list_book_files))
        .route("/api/books/:id/confirm", post(books::confirm_book))
        .route("/api/books/:id/merge", post(books::merge_books))
        // 图书馆总库与索引 V1 核心接口
        .route("/api/catalog/stats", get(catalog_v1::get_stats))
        .route(
            "/api/catalog/search",
            get(catalog_v1::search_editions_handler),
        )
        .route(
            "/api/catalog/editions",
            get(catalog_v1::search_editions_handler),
        )
        .route(
            "/api/catalog/editions/:id",
            get(catalog_v1::get_edition_handler),
        )
        .route(
            "/api/catalog/sources",
            get(catalog_v1::list_sources_handler).post(catalog_v1::create_source_handler),
        )
        .route(
            "/api/catalog/imports/preview",
            post(catalog_v1::preview_import_handler).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route(
            "/api/catalog/imports/manifests",
            get(catalog_v1::list_server_manifests_handler),
        )
        .route(
            "/api/catalog/imports/submit",
            post(catalog_v1::submit_import_handler).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route(
            "/api/catalog/imports/submitRequest",
            post(catalog_v1::submit_import_handler).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route(
            "/api/catalog/imports/runs",
            get(catalog_v1::list_import_runs_handler),
        )
        .route(
            "/api/catalog/imports/runs/:id",
            get(catalog_v1::get_import_run_handler),
        )
        .route(
            "/api/catalog/imports/quarantine",
            get(catalog_v1::list_quarantined_records_handler),
        )
        .route(
            "/api/catalog/imports/quarantine/:id/resolve",
            post(catalog_v1::resolve_quarantine_handler),
        )
        .route(
            "/api/catalog/acquisitions",
            get(catalog_v1::list_acquisitions_handler),
        )
        .route(
            "/api/catalog/acquisitions/:id/retry",
            post(catalog_v1::retry_acquisition_handler),
        )
        .route(
            "/api/catalog/acquisitions/:id/priority",
            post(catalog_v1::update_acquisition_priority_handler),
        )
        .route(
            "/api/catalog/acquisitions/claim",
            post(catalog_v1::claim_acquisition_handler),
        )
        .route(
            "/api/catalog/acquisitions/report",
            post(catalog_v1::report_acquisition_handler),
        )
        .route(
            "/api/catalog/storage/commit",
            post(catalog_v1::commit_storage_handler),
        )
        .route(
            "/api/catalog/resolutions/merge",
            post(catalog_v1::merge_works_handler),
        )
        .route(
            "/api/catalog/resolutions/merge-preview",
            get(catalog_v1::merge_preview_handler),
        )
        .route(
            "/api/catalog/outbox/process",
            post(catalog_v1::process_outbox_handler),
        )
        // 馆藏扫描与审核（方案第 10 节）
        .nest("/api/catalog", inventory::inventory_routes())
        // 批次
        .route(
            "/api/download-control",
            get(batches::get_global_download_control).put(batches::update_global_download_control),
        )
        .route("/api/batches", get(batches::list_batches))
        .route("/api/batches/:id", get(batches::get_batch))
        .route(
            "/api/batches/:id/progress",
            get(batches::get_batch_progress),
        )
        .route("/api/batches/import", post(batches::import_batch))
        .route("/api/batches/:id/start", post(batches::start_batch))
        .route("/api/batches/:id/pause", post(batches::pause_batch))
        .route("/api/batches/:id/resume", post(batches::resume_batch))
        .route("/api/batches/:id/cancel", post(batches::cancel_batch))
        .route(
            "/api/batches/:id/priority",
            put(batches::update_batch_priority),
        )
        .route(
            "/api/batches/:id/retry-failed",
            post(batches::retry_failed_tasks),
        )
        .route("/api/batches/:id/export", get(batches::export_batch))
        // 任务
        .route("/api/tasks", get(tasks::list_tasks))
        .route("/api/tasks/:id", get(tasks::get_task))
        .route(
            "/api/tasks/:id/executions",
            get(tasks::list_task_executions),
        )
        .route("/api/tasks/:id/retry", post(tasks::retry_task))
        .route("/api/tasks/:id/cancel", post(tasks::cancel_task))
        .route("/api/tasks/needs-confirm", get(tasks::list_needs_confirm))
        .route("/api/tasks/:id/verify-nas", post(tasks::trigger_nas_verify))
        // 账号
        .route(
            "/api/accounts",
            get(accounts::list_accounts).post(accounts::create_account),
        )
        .route(
            "/api/accounts/reset-quota",
            post(accounts::reset_account_quota),
        )
        .route(
            "/api/accounts/reset-disabled",
            post(accounts::reset_disabled_accounts),
        )
        .route(
            "/api/accounts/outlook/preview",
            post(outlook_accounts::preview_outlook_accounts),
        )
        .route(
            "/api/accounts/outlook/sync",
            post(outlook_accounts::sync_outlook_accounts),
        )
        .route(
            "/api/accounts/:id",
            get(accounts::get_account).delete(accounts::delete_account),
        )
        .route(
            "/api/accounts/:id/status",
            put(accounts::update_account_status),
        )
        .route(
            "/api/accounts/:id/limit",
            put(accounts::update_account_limit),
        )
        .route(
            "/api/accounts/:id/password",
            put(accounts::update_account_password),
        )
        // 代理
        .route(
            "/api/proxies",
            get(proxies::list_proxies).post(proxies::create_proxy),
        )
        .route(
            "/api/proxies/:id",
            get(proxies::get_proxy).delete(proxies::delete_proxy),
        )
        .route("/api/proxies/:id/status", put(proxies::update_proxy_status))
        // 会话
        .route("/api/sessions", get(sessions::list_sessions))
        .route("/api/sessions/:id", get(sessions::get_session))
        .route(
            "/api/sessions/:id/terminate",
            post(sessions::terminate_session),
        )
        // 日志与告警
        .route("/api/logs", get(logs::list_logs))
        .route("/api/alerts", get(logs::list_alerts))
        .route("/api/alerts/:id/resolve", post(logs::resolve_alert))
        // 邮件验证码 Provider 设置
        .route(
            "/api/settings/mail-provider",
            get(mail_provider::get_mail_provider_config)
                .put(mail_provider::update_mail_provider_config),
        )
        .route(
            "/api/settings/mail-provider/test",
            post(mail_provider::test_mail_provider),
        )
        .route(
            "/api/mail-provider/status",
            get(mail_provider::get_mail_provider_status),
        )
        // Webhook 定时推送与测试
        .route(
            "/api/settings/webhook",
            get(webhook::get_webhook_endpoint).put(webhook::update_webhook_endpoint),
        )
        .route(
            "/api/settings/webhook/send",
            post(webhook::manual_send_webhook),
        )
        // 下载站点搜索参数
        .route(
            "/api/settings/download-search",
            get(settings::get_download_search_options)
                .put(settings::update_download_search_options),
        )
        // 出版社管理接口
        .route(
            "/api/publishers",
            get(publishers::list_publishers_handler).post(publishers::create_publisher_handler),
        )
        .route(
            "/api/publishers/merge",
            post(publishers::merge_publishers_handler),
        )
        .route(
            "/api/publishers/sync-from-editions",
            post(publishers::sync_publishers_handler),
        )
        .route(
            "/api/publishers/:id",
            get(publishers::get_publisher_handler).put(publishers::update_publisher_handler),
        )
        .route(
            "/api/publishers/:id/aliases",
            post(publishers::add_alias_handler),
        )
        .route(
            "/api/publishers/:id/editions",
            get(publishers::list_publisher_editions_handler),
        )
        // 系统设置与字典
        .route("/api/settings", get(settings::list_settings))
        .route(
            "/api/settings/:key",
            get(settings::get_setting).put(settings::put_setting),
        )
        .route("/api/dict", get(settings::get_dict))
        // V6 导入、账号注册批次、注册任务与待确认事项
        .nest("/api/imports", imports::routes())
        .nest(
            "/api/account-registration-batches",
            account_registration_batches::routes(),
        )
        .nest(
            "/api/account-registration-tasks",
            account_registration_tasks::routes(),
        )
        .nest("/api/manual-actions", manual_actions::routes())
        // 健康检查（不要求认证，见 health.rs）
        .route("/health/live", get(health::live))
        .route("/health/ready", get(health::ready))
        // 实时事件流
        .route("/api/events", get(events::sse_handler))
        .layer(cors)
        .layer(middleware::from_fn(security_headers_middleware))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            csrf_origin_middleware,
        ))
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
