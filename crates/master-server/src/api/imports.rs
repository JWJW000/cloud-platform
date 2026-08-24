//! 文件两阶段导入 API（V6 方案第 4 节）。
//!
//! 包含图书 CSV 导入与账号文件导入两套完整的预检（preview）与提交（commit）流程。
//! 遵循原则：
//! 1. 两阶段提交，防误建批次；
//! 2. 幂等提交（相同 token 重复提交返回同一批次）；
//! 3. 密码在内存中加密，绝不进入日志、错误信息与预览响应；
//! 4. CSV 导出防止公式注入；
//! 5. 流式读取并实施严格大小与行数限制。

use std::collections::HashSet;

use axum::extract::{Multipart, Path, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use platform_domain::{
    normalize_isbn, AccountImportMode, AccountStatus, BatchStatus, BookIdentity, ImportStatus,
    ImportType,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::api::auth::AuthenticatedUser;
use crate::error::{AppError, AppResult};
use crate::models::{
    AccountImportPreview, AccountPreviewRow, BookImportPreview, BookPreviewRow, DownloadBatch,
    ImportRow, InvalidRow,
};
use crate::scheduler;
use crate::security;
use crate::state::AppState;
use crate::store;

/// 最大图书 CSV 大小：10 MiB。
const MAX_BOOK_CSV_BYTES: usize = 10 * 1024 * 1024;
/// 最大账号文件大小：5 MiB。
const MAX_ACCOUNT_FILE_BYTES: usize = 5 * 1024 * 1024;
/// 最大图书行数：50,000。
const MAX_BOOK_ROWS: usize = 50_000;
/// 最大账号行数：10,000。
const MAX_ACCOUNT_ROWS: usize = 10_000;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/books/preview", post(preview_books))
        .route("/books/commit", post(commit_books))
        .route("/accounts/preview", post(preview_accounts))
        .route("/accounts/commit", post(commit_accounts))
        .route("/:id", delete(delete_import))
        .route("/:id/errors.csv", get(download_errors_csv))
}

// ---------------------------------------------------------------- 图书 CSV 导入

#[derive(Debug, Clone, Deserialize)]
pub struct CommitBooksRequest {
    pub import_token: String,
    #[serde(default)]
    pub start_immediately: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitBooksResponse {
    pub batch: DownloadBatch,
    pub deduplicated: usize,
    pub already_ingested: usize,
}

/// 图书预检接口（multipart/form-data）。
async fn preview_books(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    mut multipart: Multipart,
) -> AppResult<Json<BookImportPreview>> {
    auth.require_write()?;

    let mut file_bytes: Option<Vec<u8>> = None;
    let mut file_name: String = "books.csv".to_string();
    let mut batch_name: String = String::new();
    let mut download_format: String = "pdf".to_string();
    let mut priority: i32 = 0;
    let mut max_attempts: i32 = 3;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad(format!("读取表单字段失败：{e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            if let Some(f_name) = field.file_name() {
                file_name = f_name.to_string();
            }
            let data = field
                .bytes()
                .await
                .map_err(|e| AppError::bad(format!("读取文件内容失败：{e}")))?;
            if data.len() > MAX_BOOK_CSV_BYTES {
                return Err(AppError::bad(format!(
                    "文件大小超过 10 MiB 限制（实际 {} 字节）",
                    data.len()
                )));
            }
            file_bytes = Some(data.to_vec());
        } else if name == "batch_name" {
            batch_name = field.text().await.unwrap_or_default().trim().to_string();
        } else if name == "download_format" {
            let fmt = field.text().await.unwrap_or_default().trim().to_lowercase();
            if fmt == "epub" || fmt == "pdf" {
                download_format = fmt;
            }
        } else if name == "priority" {
            if let Ok(p) = field.text().await.unwrap_or_default().trim().parse::<i32>() {
                priority = p;
            }
        } else if name == "max_attempts" {
            if let Ok(m) = field.text().await.unwrap_or_default().trim().parse::<i32>() {
                max_attempts = m.clamp(1, 10);
            }
        }
    }

    let Some(data) = file_bytes else {
        return Err(AppError::bad("未提供上传文件"));
    };

    if data.is_empty() {
        return Err(AppError::bad("上传文件内容为空"));
    }

    if batch_name.is_empty() {
        batch_name = file_name.trim_end_matches(".csv").to_string();
        if batch_name.is_empty() {
            batch_name = format!("批次-{}", Utc::now().format("%Y%m%d%H%M%S"));
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(&data);
    let file_sha256 = hex::encode(hasher.finalize());

    let (parsed_rows, invalid_rows, warnings) = parse_book_csv_bytes(&data)?;

    if parsed_rows.len() > MAX_BOOK_ROWS {
        return Err(AppError::bad(format!(
            "文件行数超过 {} 上限（实际 {} 行）",
            MAX_BOOK_ROWS,
            parsed_rows.len()
        )));
    }

    let total_rows = parsed_rows.len() + invalid_rows.len();
    let mut seen_in_file: HashSet<String> = HashSet::new();
    let mut duplicate_in_file_count = 0usize;
    let mut duplicate_in_library_count = 0usize;
    let mut already_ingested_count = 0usize;

    let mut preview_rows: Vec<BookPreviewRow> = Vec::new();
    let mut valid_parsed: Vec<ParsedBookRow> = Vec::new();

    for row in parsed_rows {
        let dedup_str = BookIdentity::from_raw(
            &row.title,
            row.author.as_deref(),
            row.publisher.as_deref(),
            row.isbn.as_deref(),
        )
        .map(|i| i.storage_key())
        .unwrap_or_else(|| row.title.clone());

        let is_file_dup = !seen_in_file.insert(dedup_str.clone());
        if is_file_dup {
            duplicate_in_file_count += 1;
            if preview_rows.len() < 100 {
                preview_rows.push(BookPreviewRow {
                    line: row.line,
                    title: row.title.clone(),
                    author: row.author.clone(),
                    publisher: row.publisher.clone(),
                    isbn: row.isbn.clone(),
                    status: "文件内重复".to_string(),
                    reason: Some("文件中存在相同图书".to_string()),
                });
            }
            continue;
        }

        let existing_book: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM books WHERE dedup_key = $1")
                .bind(&dedup_str)
                .fetch_optional(&state.pool)
                .await?;
        let is_lib_dup = existing_book.is_some();
        let mut ingested = false;

        if let Some((book_id,)) = existing_book {
            duplicate_in_library_count += 1;
            let files = store::catalog::list_book_files(&state.pool, book_id).await?;
            ingested = files
                .iter()
                .any(|f| f.format == download_format && f.status == "有效");
            if ingested {
                already_ingested_count += 1;
            }
        }

        let status_str = if ingested {
            "已入库"
        } else if is_lib_dup {
            "库内已有"
        } else {
            "有效待下"
        };

        if preview_rows.len() < 100 {
            preview_rows.push(BookPreviewRow {
                line: row.line,
                title: row.title.clone(),
                author: row.author.clone(),
                publisher: row.publisher.clone(),
                isbn: row.isbn.clone(),
                status: status_str.to_string(),
                reason: None,
            });
        }

        valid_parsed.push(row);
    }

    for inv in &invalid_rows {
        if preview_rows.len() < 100 {
            preview_rows.push(BookPreviewRow {
                line: inv.line,
                title: inv.raw.clone(),
                author: None,
                publisher: None,
                isbn: None,
                status: "错误".to_string(),
                reason: Some(inv.reason.clone()),
            });
        }
    }

    let mut token_bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut token_bytes);
    let import_token = hex::encode(token_bytes);
    let token_hash = security::hash_token(&import_token);

    let payload = serde_json::to_string(&BookImportPayload {
        batch_name: batch_name.clone(),
        download_format: download_format.clone(),
        priority,
        max_attempts,
        rows: valid_parsed.clone(),
        invalid_rows: invalid_rows.clone(),
    })
    .map_err(|e| AppError::internal(e.to_string()))?;

    let payload_encrypted = state.cipher.encrypt(&payload)?;

    let summary = serde_json::json!({
        "batch_name": batch_name,
        "download_format": download_format,
        "priority": priority,
        "max_attempts": max_attempts,
        "total_rows": total_rows,
        "valid_rows": valid_parsed.len(),
        "duplicate_in_file": duplicate_in_file_count,
        "duplicate_in_library": duplicate_in_library_count,
        "already_ingested": already_ingested_count,
        "error_rows": invalid_rows.len(),
        "invalid_rows": invalid_rows,
    });

    let new_job = store::import_job::NewImportJob {
        import_type: ImportType::Books,
        original_file_name: file_name.clone(),
        file_sha256: file_sha256.clone(),
        temp_path: None,
        token_hash,
        created_by: Some(auth.id),
        expires_at: Utc::now() + Duration::minutes(30),
        summary,
        payload_encrypted: Some(payload_encrypted),
    };

    store::import_job::create_import_job(&state.pool, &new_job).await?;

    Ok(Json(BookImportPreview {
        import_token,
        file_name,
        file_sha256,
        total_rows,
        valid_rows: valid_parsed.len(),
        duplicate_in_file: duplicate_in_file_count,
        duplicate_in_library: duplicate_in_library_count,
        already_ingested: already_ingested_count,
        error_rows: invalid_rows.len(),
        warnings,
        preview: preview_rows,
    }))
}

/// 提交图书导入并创建批次与任务（幂等）。
async fn commit_books(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<CommitBooksRequest>,
) -> AppResult<Json<CommitBooksResponse>> {
    auth.require_write()?;

    let token_hash = security::hash_token(req.import_token.trim());
    let job = store::import_job::get_valid_job_by_token_hash(&state.pool, &token_hash).await?;

    if job.status == ImportStatus::Committed.as_str() {
        if let Some(batch_id) = job.committed_resource_id {
            let batch = store::catalog::get_batch(&state.pool, batch_id).await?;
            return Ok(Json(CommitBooksResponse {
                batch,
                deduplicated: job
                    .summary
                    .get("duplicate_in_library")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize,
                already_ingested: job
                    .summary
                    .get("already_ingested")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize,
            }));
        }
    }

    let Some(encrypted_payload) = &job.payload_encrypted else {
        return Err(AppError::bad("导入任务载荷已丢失或已提交"));
    };

    let decrypted = state.cipher.decrypt(encrypted_payload)?;
    let parsed: BookImportPayload = serde_json::from_str(&decrypted)
        .map_err(|e| AppError::internal(format!("解析导入载荷失败：{e}")))?;

    let import_rows: Vec<ImportRow> = parsed
        .rows
        .iter()
        .map(|r| ImportRow {
            title: r.title.clone(),
            author: r.author.clone(),
            publisher: r.publisher.clone(),
            isbn: r.isbn.clone(),
        })
        .collect();

    let import_summary = store::catalog::import_books(
        &state.pool,
        &store::catalog::ImportRequest {
            batch_name: &parsed.batch_name,
            source_file: Some(&job.original_file_name),
            priority: parsed.priority,
            format: &parsed.download_format,
            max_attempts: parsed.max_attempts,
            created_by: Some(auth.id),
        },
        &import_rows,
    )
    .await?;

    let batch_id = import_summary
        .batch_id
        .ok_or_else(|| AppError::internal("创建批次失败"))?;

    if req.start_immediately {
        store::catalog::set_batch_status(&state.pool, batch_id, BatchStatus::Running).await?;
        let _ = scheduler::trigger_scheduler_sweep(&state).await;
    }

    store::import_job::mark_import_job_committed(&state.pool, job.id, batch_id).await?;

    state.events.publish(
        "批次变更",
        serde_json::json!({
            "批次": batch_id,
            "名称": parsed.batch_name,
            "状态": if req.start_immediately { "执行中" } else { "待开始" },
        }),
    );

    let batch = store::catalog::get_batch(&state.pool, batch_id).await?;
    Ok(Json(CommitBooksResponse {
        batch,
        deduplicated: import_summary.deduplicated,
        already_ingested: import_summary.already_ingested,
    }))
}

// ---------------------------------------------------------------- 账号导入

#[derive(Debug, Clone, Deserialize)]
pub struct CommitAccountsRequest {
    pub import_token: String,
    pub mode: AccountImportMode,
    #[serde(default)]
    pub create_registration_batch: bool,
    #[serde(default)]
    pub batch_name: Option<String>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub start_immediately: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitAccountsResponse {
    pub imported_accounts: usize,
    pub registration_batch: Option<crate::models::AccountRegistrationBatch>,
}

/// 账号文件预检接口。
async fn preview_accounts(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    mut multipart: Multipart,
) -> AppResult<Json<AccountImportPreview>> {
    auth.require_super_admin()?;

    let mut file_bytes: Option<Vec<u8>> = None;
    let mut file_name: String = "accounts.txt".to_string();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad(format!("读取表单字段失败：{e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            if let Some(f_name) = field.file_name() {
                file_name = f_name.to_string();
            }
            let data = field
                .bytes()
                .await
                .map_err(|e| AppError::bad(format!("读取文件内容失败：{e}")))?;
            if data.len() > MAX_ACCOUNT_FILE_BYTES {
                return Err(AppError::bad(format!(
                    "文件大小超过 5 MiB 限制（实际 {} 字节）",
                    data.len()
                )));
            }
            file_bytes = Some(data.to_vec());
        }
    }

    let Some(data) = file_bytes else {
        return Err(AppError::bad("未提供上传文件"));
    };

    if data.is_empty() {
        return Err(AppError::bad("上传文件内容为空"));
    }

    let mut hasher = Sha256::new();
    hasher.update(&data);
    let file_sha256 = hex::encode(hasher.finalize());

    let (parsed_accounts, invalid_rows, warnings) = parse_account_file_bytes(&data)?;

    if parsed_accounts.len() > MAX_ACCOUNT_ROWS {
        return Err(AppError::bad(format!(
            "账号行数超过 {} 上限（实际 {} 行）",
            MAX_ACCOUNT_ROWS,
            parsed_accounts.len()
        )));
    }

    let total_rows = parsed_accounts.len() + invalid_rows.len();
    let mut seen_in_file: HashSet<String> = HashSet::new();
    let mut duplicate_in_file_count = 0usize;
    let mut duplicate_in_library_count = 0usize;

    let mut preview_rows: Vec<AccountPreviewRow> = Vec::new();
    let mut valid_accounts: Vec<ParsedAccountRow> = Vec::new();

    for acc in parsed_accounts {
        let email_lower = acc.email.to_lowercase();
        if !seen_in_file.insert(email_lower.clone()) {
            duplicate_in_file_count += 1;
            if preview_rows.len() < 100 {
                preview_rows.push(AccountPreviewRow {
                    line: acc.line,
                    email_masked: mask_email(&acc.email),
                    nickname: acc.nickname.clone(),
                    password_provided: !acc.password.is_empty(),
                    status: "文件内重复".to_string(),
                    reason: Some("文件中存在相同邮箱".to_string()),
                });
            }
            continue;
        }

        let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM accounts WHERE email = $1")
            .bind(&acc.email)
            .fetch_optional(&state.pool)
            .await?;

        if exists.is_some() {
            duplicate_in_library_count += 1;
            if preview_rows.len() < 100 {
                preview_rows.push(AccountPreviewRow {
                    line: acc.line,
                    email_masked: mask_email(&acc.email),
                    nickname: acc.nickname.clone(),
                    password_provided: !acc.password.is_empty(),
                    status: "库内已有".to_string(),
                    reason: Some("数据库已存在该邮箱账号".to_string()),
                });
            }
            continue;
        }

        if preview_rows.len() < 100 {
            preview_rows.push(AccountPreviewRow {
                line: acc.line,
                email_masked: mask_email(&acc.email),
                nickname: acc.nickname.clone(),
                password_provided: !acc.password.is_empty(),
                status: "有效待导入".to_string(),
                reason: None,
            });
        }

        valid_accounts.push(acc);
    }

    for inv in &invalid_rows {
        if preview_rows.len() < 100 {
            preview_rows.push(AccountPreviewRow {
                line: inv.line,
                email_masked: mask_email(&inv.raw),
                nickname: String::new(),
                password_provided: false,
                status: "错误".to_string(),
                reason: Some(inv.reason.clone()),
            });
        }
    }

    let mut token_bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut token_bytes);
    let import_token = hex::encode(token_bytes);
    let token_hash = security::hash_token(&import_token);

    let payload =
        serde_json::to_string(&valid_accounts).map_err(|e| AppError::internal(e.to_string()))?;
    let payload_encrypted = state.cipher.encrypt(&payload)?;

    let summary = serde_json::json!({
        "total_rows": total_rows,
        "valid_rows": valid_accounts.len(),
        "duplicate_in_file": duplicate_in_file_count,
        "duplicate_in_library": duplicate_in_library_count,
        "error_rows": invalid_rows.len(),
        "invalid_rows": invalid_rows,
    });

    let new_job = store::import_job::NewImportJob {
        import_type: ImportType::Accounts,
        original_file_name: file_name.clone(),
        file_sha256: file_sha256.clone(),
        temp_path: None,
        token_hash,
        created_by: Some(auth.id),
        expires_at: Utc::now() + Duration::minutes(30),
        summary,
        payload_encrypted: Some(payload_encrypted),
    };

    store::import_job::create_import_job(&state.pool, &new_job).await?;

    Ok(Json(AccountImportPreview {
        import_token,
        file_name,
        file_sha256,
        total_rows,
        valid_rows: valid_accounts.len(),
        duplicate_in_file: duplicate_in_file_count,
        duplicate_in_library: duplicate_in_library_count,
        error_rows: invalid_rows.len(),
        warnings,
        preview: preview_rows,
    }))
}

/// 提交账号导入并可选创建账号注册批次（幂等）。
async fn commit_accounts(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Json(req): Json<CommitAccountsRequest>,
) -> AppResult<Json<CommitAccountsResponse>> {
    auth.require_super_admin()?;

    let token_hash = security::hash_token(req.import_token.trim());
    let job = store::import_job::get_valid_job_by_token_hash(&state.pool, &token_hash).await?;

    let Some(encrypted_payload) = &job.payload_encrypted else {
        return Err(AppError::bad("导入任务载荷已丢失或已提交"));
    };

    let decrypted = state.cipher.decrypt(encrypted_payload)?;
    let parsed: Vec<ParsedAccountRow> = serde_json::from_str(&decrypted)
        .map_err(|e| AppError::internal(format!("解析账号载荷失败：{e}")))?;

    let mut tx = state.pool.begin().await?;

    let status = match req.mode {
        AccountImportMode::PendingRegistration => AccountStatus::PendingRegistration,
        AccountImportMode::Registered => AccountStatus::Registered,
    };

    let mut account_ids: Vec<Uuid> = Vec::new();

    for row in &parsed {
        let password_cipher = state.cipher.encrypt(&row.password)?;
        let account = store::resource::create_account(
            &mut *tx,
            &row.email,
            &password_cipher,
            &row.nickname,
            10,
            status,
        )
        .await?;
        account_ids.push(account.id);
    }

    let mut created_batch = None;

    if req.create_registration_batch
        && req.mode == AccountImportMode::PendingRegistration
        && !account_ids.is_empty()
    {
        let batch_name = req
            .batch_name
            .unwrap_or_else(|| format!("注册批次-{}", Utc::now().format("%Y%m%d%H%M%S")));

        let batch = store::account_registration::create_batch(
            &mut *tx,
            &store::account_registration::NewAccountRegistrationBatch {
                name: batch_name,
                source_file: Some(job.original_file_name.clone()),
                priority: req.priority,
                created_by: Some(auth.id),
            },
        )
        .await?;

        for acc_id in &account_ids {
            store::account_registration::create_task(&mut *tx, batch.id, *acc_id, req.priority)
                .await?;
        }

        if req.start_immediately {
            store::account_registration::update_batch_status(
                &mut *tx,
                batch.id,
                BatchStatus::NotStarted,
                BatchStatus::Running,
            )
            .await?;
        }

        store::import_job::mark_import_job_committed(&mut *tx, job.id, batch.id).await?;
        created_batch = Some(batch);
    } else {
        store::import_job::mark_import_job_committed(&mut *tx, job.id, job.id).await?;
    }

    tx.commit().await?;

    if req.start_immediately && created_batch.is_some() {
        let _ = scheduler::trigger_scheduler_sweep(&state).await;
    }

    state.events.publish(
        "账号变更",
        serde_json::json!({
            "数量": account_ids.len(),
            "模式": req.mode.as_str(),
        }),
    );

    Ok(Json(CommitAccountsResponse {
        imported_accounts: account_ids.len(),
        registration_batch: created_batch,
    }))
}

/// 删除导入任务。
async fn delete_import(
    State(state): State<AppState>,
    auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    auth.require_write()?;
    store::import_job::delete_import_job(&state.pool, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 下载错误行 CSV（防公式注入）。
async fn download_errors_csv(
    State(state): State<AppState>,
    _auth: AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let job = store::import_job::get_import_job(&state.pool, id).await?;
    let invalid_rows: Vec<InvalidRow> = job
        .summary
        .get("invalid_rows")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let mut wtr = csv::WriterBuilder::new().from_writer(vec![]);
    wtr.write_record(&["行号", "原始内容", "错误原因"])
        .map_err(|e| AppError::internal(e.to_string()))?;

    for inv in invalid_rows {
        let safe_raw = escape_csv_formula(&inv.raw);
        let safe_reason = escape_csv_formula(&inv.reason);
        wtr.write_record(&[inv.line.to_string(), safe_raw, safe_reason])
            .map_err(|e| AppError::internal(e.to_string()))?;
    }

    let csv_data = wtr
        .into_inner()
        .map_err(|e| AppError::internal(e.to_string()))?;

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, "text/csv; charset=utf-8".parse().unwrap());
    headers.insert(
        CONTENT_DISPOSITION,
        format!("attachment; filename=\"import_errors_{}.csv\"", job.id)
            .parse()
            .unwrap(),
    );

    Ok((headers, csv_data))
}

// ---------------------------------------------------------------- 工具函数

/// CSV 公式注入转义：以 `= + - @ \t \r` 开头的字段前面加 `'`。
pub fn escape_csv_formula(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with('=')
        || trimmed.starts_with('+')
        || trimmed.starts_with('-')
        || trimmed.starts_with('@')
        || trimmed.starts_with('\t')
        || trimmed.starts_with('\r')
    {
        format!("'{text}")
    } else {
        text.to_string()
    }
}

/// 邮箱脱敏：a***b@example.com
pub fn mask_email(email: &str) -> String {
    let parts: Vec<&str> = email.split('@').collect();
    if parts.len() != 2 {
        return email.to_string();
    }
    let name = parts[0];
    let domain = parts[1];
    if name.len() <= 2 {
        format!("{}***@{}", name, domain)
    } else {
        let first = &name[..1];
        let last = &name[name.len() - 1..];
        format!("{}***{}@{}", first, last, domain)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedBookRow {
    pub line: usize,
    pub title: String,
    pub author: Option<String>,
    pub publisher: Option<String>,
    pub isbn: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BookImportPayload {
    pub batch_name: String,
    pub download_format: String,
    pub priority: i32,
    pub max_attempts: i32,
    pub rows: Vec<ParsedBookRow>,
    pub invalid_rows: Vec<InvalidRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedAccountRow {
    pub line: usize,
    pub email: String,
    pub password: String,
    pub nickname: String,
}

/// 解析图书 CSV 字节数据（标准 RFC 4180 CSV，支持 BOM、引号内逗号、单列或四列）。
pub fn parse_book_csv_bytes(
    data: &[u8],
) -> AppResult<(Vec<ParsedBookRow>, Vec<InvalidRow>, Vec<String>)> {
    let mut warnings = Vec::new();
    let content = strip_bom(data);
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(content.as_bytes());

    let mut parsed_rows = Vec::new();
    let mut invalid_rows = Vec::new();
    let mut is_first_row = true;

    for (idx, result) in rdr.records().enumerate() {
        let line_no = idx + 1;
        let record = match result {
            Ok(r) => r,
            Err(e) => {
                invalid_rows.push(InvalidRow {
                    line: line_no,
                    raw: String::new(),
                    reason: format!("CSV 格式解析错误：{e}"),
                });
                continue;
            }
        };

        if record.is_empty() {
            continue;
        }

        let first_col = record.get(0).unwrap_or("").trim();
        if first_col.is_empty() {
            let all_empty =
                (0..record.len()).all(|i| record.get(i).unwrap_or("").trim().is_empty());
            if all_empty {
                continue;
            }
            invalid_rows.push(InvalidRow {
                line: line_no,
                raw: record.iter().collect::<Vec<_>>().join(","),
                reason: "第一列书名不能为空".to_string(),
            });
            continue;
        }

        // 检查首行是否为表头
        if is_first_row {
            is_first_row = false;
            let col0 = first_col.to_lowercase();
            if col0 == "书名" || col0 == "title" || col0 == "book" || col0 == "name" {
                continue;
            }
        }

        let title = first_col.to_string();
        let author = record
            .get(1)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let publisher = record
            .get(2)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let raw_isbn = record
            .get(3)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let mut valid_isbn = None;
        if let Some(isbn_str) = raw_isbn {
            if let Some(norm) = normalize_isbn(&isbn_str) {
                valid_isbn = Some(norm.to_string());
            } else {
                warnings.push(format!(
                    "第 {} 行 ISBN「{}」不合法，已保留原始书名继续处理",
                    line_no, isbn_str
                ));
            }
        }

        parsed_rows.push(ParsedBookRow {
            line: line_no,
            title,
            author,
            publisher,
            isbn: valid_isbn,
        });
    }

    Ok((parsed_rows, invalid_rows, warnings))
}

/// 解析账号文本/CSV。
pub fn parse_account_file_bytes(
    data: &[u8],
) -> AppResult<(Vec<ParsedAccountRow>, Vec<InvalidRow>, Vec<String>)> {
    let warnings = Vec::new();
    let content = strip_bom(data);
    let mut parsed_rows = Vec::new();
    let mut invalid_rows = Vec::new();

    let lines: Vec<&str> = content.lines().collect();
    let mut is_first_row = true;

    for (idx, raw_line) in lines.iter().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        // 支持格式 1: email----password
        if line.contains("----") {
            let parts: Vec<&str> = line.split("----").collect();
            if parts.len() >= 2 {
                let email = parts[0].trim().to_string();
                let password = parts[1].trim().to_string();
                if !validate_email(&email) {
                    invalid_rows.push(InvalidRow {
                        line: line_no,
                        raw: line.to_string(),
                        reason: "邮箱格式不正确".to_string(),
                    });
                    continue;
                }
                if password.is_empty() {
                    invalid_rows.push(InvalidRow {
                        line: line_no,
                        raw: line.to_string(),
                        reason: "密码不能为空".to_string(),
                    });
                    continue;
                }
                let nickname = email.split('@').next().unwrap_or("").to_string();
                parsed_rows.push(ParsedAccountRow {
                    line: line_no,
                    email,
                    password,
                    nickname,
                });
                continue;
            }
        }

        // 支持格式 2: CSV 邮箱,密码,昵称
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(line.as_bytes());

        if let Some(Ok(rec)) = rdr.records().next() {
            if rec.len() >= 2 {
                let col0 = rec.get(0).unwrap_or("").trim();
                let col1 = rec.get(1).unwrap_or("").trim();
                let col2 = rec.get(2).map(|s| s.trim()).unwrap_or("");

                if is_first_row && (col0 == "邮箱" || col0.eq_ignore_ascii_case("email")) {
                    is_first_row = false;
                    continue;
                }
                is_first_row = false;

                if !validate_email(col0) {
                    invalid_rows.push(InvalidRow {
                        line: line_no,
                        raw: line.to_string(),
                        reason: "邮箱格式不正确".to_string(),
                    });
                    continue;
                }
                if col1.is_empty() {
                    invalid_rows.push(InvalidRow {
                        line: line_no,
                        raw: line.to_string(),
                        reason: "密码不能为空".to_string(),
                    });
                    continue;
                }

                let nickname = if col2.is_empty() {
                    col0.split('@').next().unwrap_or("").to_string()
                } else {
                    col2.to_string()
                };

                parsed_rows.push(ParsedAccountRow {
                    line: line_no,
                    email: col0.to_string(),
                    password: col1.to_string(),
                    nickname,
                });
                continue;
            }
        }

        invalid_rows.push(InvalidRow {
            line: line_no,
            raw: line.to_string(),
            reason: "行格式不支持（支持 邮箱----密码 或 邮箱,密码,昵称）".to_string(),
        });
    }

    Ok((parsed_rows, invalid_rows, warnings))
}

fn strip_bom(data: &[u8]) -> String {
    let mut data = data;
    if data.starts_with(b"\xef\xbb\xbf") {
        data = &data[3..];
    }
    String::from_utf8_lossy(data).to_string()
}

fn validate_email(email: &str) -> bool {
    let email = email.trim();
    if email.len() < 3 || email.len() > 254 {
        return false;
    }
    let parts: Vec<&str> = email.split('@').collect();
    if parts.len() != 2 {
        return false;
    }
    let user = parts[0];
    let domain = parts[1];
    !user.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_column_book_csv() {
        let data = "第一本书\n第二本书\n第三本书".as_bytes();
        let (rows, invalid, warnings) = parse_book_csv_bytes(data).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(invalid.len(), 0);
        assert_eq!(warnings.len(), 0);
        assert_eq!(rows[0].title, "第一本书");
        assert_eq!(rows[1].title, "第二本书");
        assert_eq!(rows[2].title, "第三本书");
    }

    #[test]
    fn four_column_book_csv_with_quotes_and_commas() {
        let data = "书名,作者,出版社,ISBN\n\"天上有个,大薯片\",夏忠波著,电子工业出版社有限公司,978-7-121-11062-7\n".as_bytes();
        let (rows, invalid, _warnings) = parse_book_csv_bytes(data).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(invalid.len(), 0);
        assert_eq!(rows[0].title, "天上有个,大薯片");
        assert_eq!(rows[0].author.as_deref(), Some("夏忠波著"));
        assert_eq!(rows[0].publisher.as_deref(), Some("电子工业出版社有限公司"));
        assert_eq!(rows[0].isbn.as_deref(), Some("9787121110627"));
    }

    #[test]
    fn utf8_bom_support() {
        let mut data = vec![0xef, 0xbb, 0xbf];
        data.extend_from_slice("带BOM的书名,作者\n".as_bytes());
        let (rows, invalid, _) = parse_book_csv_bytes(&data).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(invalid.len(), 0);
        assert_eq!(rows[0].title, "带BOM的书名");
        assert_eq!(rows[0].author.as_deref(), Some("作者"));
    }

    #[test]
    fn headerless_and_with_headers() {
        let data1 = "书名,作者\n书A,作者A\n".as_bytes();
        let (rows1, _, _) = parse_book_csv_bytes(data1).unwrap();
        assert_eq!(rows1.len(), 1);
        assert_eq!(rows1[0].title, "书A");

        let data2 = "书A,作者A\n".as_bytes();
        let (rows2, _, _) = parse_book_csv_bytes(data2).unwrap();
        assert_eq!(rows2.len(), 1);
        assert_eq!(rows2[0].title, "书A");
    }

    #[test]
    fn invalid_isbn_warning_not_failure() {
        let data = "书名,作者,出版社,ISBN\n好书,好作者,好社,123456\n".as_bytes();
        let (rows, invalid, warnings) = parse_book_csv_bytes(data).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(invalid.len(), 0);
        assert_eq!(rows[0].isbn, None);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("ISBN「123456」不合法"));
    }

    #[test]
    fn formula_injection_escaping() {
        assert_eq!(escape_csv_formula("=SUM(A1:A10)"), "'=SUM(A1:A10)");
        assert_eq!(escape_csv_formula("+12345"), "'+12345");
        assert_eq!(escape_csv_formula("-cmd"), "'-cmd");
        assert_eq!(escape_csv_formula("@exec"), "'@exec");
        assert_eq!(escape_csv_formula("正常文本"), "正常文本");
    }

    #[test]
    fn account_separator_format() {
        let data = "user1@example.com----pass123\nuser2@example.com----pass456\n".as_bytes();
        let (rows, invalid, _) = parse_account_file_bytes(data).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(invalid.len(), 0);
        assert_eq!(rows[0].email, "user1@example.com");
        assert_eq!(rows[0].password, "pass123");
        assert_eq!(rows[1].email, "user2@example.com");
        assert_eq!(rows[1].password, "pass456");
    }

    #[test]
    fn account_csv_format() {
        let data = "邮箱,密码,昵称\nadmin@test.org,StrongPass,管理员\n".as_bytes();
        let (rows, invalid, _) = parse_account_file_bytes(data).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(invalid.len(), 0);
        assert_eq!(rows[0].email, "admin@test.org");
        assert_eq!(rows[0].password, "StrongPass");
        assert_eq!(rows[0].nickname, "管理员");
    }

    #[test]
    fn email_masking() {
        assert_eq!(mask_email("user@example.com"), "u***r@example.com");
        assert_eq!(mask_email("ab@domain.com"), "ab***@domain.com");
        assert_eq!(mask_email("invalid"), "invalid");
    }
}
