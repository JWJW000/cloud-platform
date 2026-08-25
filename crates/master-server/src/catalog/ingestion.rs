//! 图书馆总库流式导入与预检流水线（第 8 节）。
//!
//! 实现多格式数据结构自动识别、幂等落库、流式分块、检查点、隔离区与 Outbox 事件投递。

use platform_domain::{clean_text, ImportRunStatus, WorkType};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use crate::catalog::resolution::{resolve_item, ParsedCatalogItem};
use crate::error::{AppError, AppResult};
use crate::store::catalog_v1::{
    create_import_run, get_or_create_source, register_import_file, update_import_run_progress,
};

/// 导入清单预检请求。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ImportManifestRequest {
    /// 来源名称（如 cn, en, 图书书目1, 补充书单等）。
    pub source_name: String,
    /// 来源类型（excel, csv）。
    pub source_type: Option<String>,
    /// 文件名或路径。
    pub file_name: String,
    /// 工作表名称（针对 Excel）。
    pub sheet_name: Option<String>,
    /// 原始文件内容（如果直接上传）。
    pub content: Option<Vec<u8>>,
    /// 文本内容（针对 CSV/TSV 文本输入）。
    pub text_content: Option<String>,
}

/// 预检报告。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportPreviewResult {
    /// 数据源编号。
    pub source_id: Uuid,
    /// 数据源名称。
    pub source_name: String,
    /// 识别出的结构类型。
    pub detected_structure: String,
    /// 字段映射。
    pub column_mapping: HashMap<String, String>,
    /// 估计总行数。
    pub total_rows: usize,
    /// 样本预览（最多前 10 行）。
    pub sample_rows: Vec<ParsedCatalogItemSummary>,
    /// 文件 SHA-256。
    pub file_sha256: String,
    /// 是否已存在完全相同文件。
    pub is_duplicate_file: bool,
}

/// 样本项摘要。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParsedCatalogItemSummary {
    /// 行号。
    pub line: usize,
    /// 标题。
    pub title: String,
    /// 作者。
    pub author: Option<String>,
    /// 出版社。
    pub publisher: Option<String>,
    /// ISBN。
    pub isbn: Option<String>,
    /// DOI。
    pub doi: Option<String>,
    /// 年份。
    pub year: Option<String>,
    /// 格式。
    pub format: Option<String>,
    /// MD5。
    pub md5: Option<String>,
}

/// 执行导入请求。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct StartImportRequest {
    /// 数据源名称。
    pub source_name: String,
    /// 数据源类型。
    pub source_type: Option<String>,
    /// 文件名。
    pub file_name: String,
    /// 工作表名称。
    pub sheet_name: Option<String>,
    /// 文本内容或 CSV 内容。
    pub text_content: Option<String>,
}

/// 数据导入执行结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportExecutionResult {
    /// 导入运行编号。
    pub run_id: Uuid,
    /// 导入文件编号。
    pub import_file_id: Uuid,
    /// 总行数。
    pub total_rows: i64,
    /// 成功导入数。
    pub imported_count: i64,
    /// 重复跳过数。
    pub duplicate_count: i64,
    /// 隔离数。
    pub quarantined_count: i64,
    /// 状态。
    pub status: String,
}

/// 预检解析。
pub async fn preview_import(
    pool: &PgPool,
    req: &ImportManifestRequest,
) -> AppResult<ImportPreviewResult> {
    let source = get_or_create_source(
        pool,
        &req.source_name,
        req.source_type.as_deref().unwrap_or("csv"),
        None,
        0,
    )
    .await?;

    let (content_bytes, text_str) = match (&req.content, &req.text_content) {
        (Some(bytes), _) => (bytes.clone(), String::from_utf8_lossy(bytes).to_string()),
        (None, Some(text)) => (text.as_bytes().to_vec(), text.clone()),
        (None, None) => return Err(AppError::bad("必须提供文件内容或文本数据")),
    };

    let mut hasher = Sha256::new();
    hasher.update(&content_bytes);
    let file_sha256 = hex::encode(hasher.finalize());

    let is_dup: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM import_files WHERE file_sha256 = $1)")
            .bind(&file_sha256)
            .fetch_one(pool)
            .await?;

    let parsed_items = parse_csv_stream(&text_str)?;
    let total_rows = parsed_items.len();

    let sample_rows: Vec<ParsedCatalogItemSummary> = parsed_items
        .iter()
        .take(10)
        .enumerate()
        .map(|(idx, item)| ParsedCatalogItemSummary {
            line: idx + 1,
            title: item.raw_title.clone(),
            author: item.raw_author.clone(),
            publisher: item.raw_publisher.clone(),
            isbn: item.raw_isbn.clone(),
            doi: item.raw_doi.clone(),
            year: item.raw_year.clone(),
            format: item.format.clone(),
            md5: item.md5.clone(),
        })
        .collect();

    let mut mapping = HashMap::new();
    mapping.insert("title".to_string(), "规范书名".to_string());
    mapping.insert("author".to_string(), "责任者".to_string());
    mapping.insert("publisher".to_string(), "出版者".to_string());
    mapping.insert("isbn".to_string(), "国际标准书号".to_string());

    Ok(ImportPreviewResult {
        source_id: source.id,
        source_name: source.name,
        detected_structure: "流式结构识别 V1".to_string(),
        column_mapping: mapping,
        total_rows,
        sample_rows,
        file_sha256,
        is_duplicate_file: is_dup,
    })
}

/// 执行流式导入任务。
pub async fn execute_import(
    pool: &PgPool,
    req: &StartImportRequest,
) -> AppResult<ImportExecutionResult> {
    let source = get_or_create_source(
        pool,
        &req.source_name,
        req.source_type.as_deref().unwrap_or("csv"),
        None,
        0,
    )
    .await?;

    let text_str = req.text_content.clone().unwrap_or_default();
    if text_str.trim().is_empty() {
        return Err(AppError::bad("导入内容为空"));
    }

    let mut hasher = Sha256::new();
    hasher.update(text_str.as_bytes());
    let file_sha256 = hex::encode(hasher.finalize());

    let parsed_items = parse_csv_stream(&text_str)?;
    let total_rows = parsed_items.len() as i64;
    let sheet = req.sheet_name.as_deref().unwrap_or("");

    let import_file = register_import_file(
        pool,
        source.id,
        &req.file_name,
        &file_sha256,
        text_str.len() as i64,
        sheet,
        "v1",
        total_rows,
    )
    .await?;

    let run = create_import_run(pool, import_file.id, total_rows).await?;

    let mut imported_count = 0i64;
    let mut duplicate_count = 0i64;
    let mut quarantined_count = 0i64;

    // 采用分块事务处理（每批 200 行）
    const CHUNK_SIZE: usize = 200;
    for (chunk_idx, chunk) in parsed_items.chunks(CHUNK_SIZE).enumerate() {
        let mut tx = pool.begin().await?;

        for (item_idx, item) in chunk.iter().enumerate() {
            let row_number = (chunk_idx * CHUNK_SIZE + item_idx + 1) as i64;

            // 书名为空直接进入隔离区
            let clean_title = clean_text(&item.raw_title);
            if clean_title.is_empty() {
                quarantined_count += 1;
                sqlx::query(
                    "INSERT INTO quarantined_records (id, import_run_id, import_file_id, sheet_name, row_number, raw_content, error_reason, resolved) \
                     VALUES ($1, $2, $3, $4, $5, $6, '书名为空或无效', FALSE)"
                )
                .bind(Uuid::new_v4())
                .bind(run.id)
                .bind(import_file.id)
                .bind(sheet)
                .bind(row_number)
                .bind(&item.raw_payload)
                .execute(&mut *tx)
                .await?;
                continue;
            }

            // 写入 source_records，利用 (import_file_id, sheet_name, row_number) 唯一索引保证幂等
            let source_record_id = Uuid::new_v4();
            let inserted: Option<Uuid> = sqlx::query_scalar(
                "INSERT INTO source_records \
                     (id, source_id, import_file_id, external_id, sheet_name, row_number, raw_payload, \
                      normalized_title, normalized_author, normalized_publisher, raw_isbn, raw_doi, raw_year, raw_language, raw_category) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) \
                 ON CONFLICT (import_file_id, sheet_name, row_number) DO NOTHING \
                 RETURNING id"
            )
            .bind(source_record_id)
            .bind(source.id)
            .bind(import_file.id)
            .bind(&item.external_id)
            .bind(sheet)
            .bind(row_number)
            .bind(&item.raw_payload)
            .bind(&clean_title)
            .bind(&item.raw_author)
            .bind(&item.raw_publisher)
            .bind(&item.raw_isbn)
            .bind(&item.raw_doi)
            .bind(&item.raw_year)
            .bind(&item.raw_language)
            .bind(&item.raw_category)
            .fetch_optional(&mut *tx)
            .await?;

            let Some(actual_record_id) = inserted else {
                duplicate_count += 1;
                continue;
            };

            // 进行规范化与消歧处理
            let resolution_res = resolve_item(&mut tx, source.id, actual_record_id, item).await?;

            // 写入 Outbox 事件
            let outbox_payload = serde_json::json!({
                "work_id": resolution_res.work_id,
                "edition_id": resolution_res.edition_id,
                "source_record_id": actual_record_id,
                "match_method": resolution_res.match_method,
            });

            sqlx::query(
                "INSERT INTO catalog_outbox (event_type, aggregate_type, aggregate_id, payload, status) \
                 VALUES ('catalog.edition_indexed', 'edition', $1, $2, '待同步')"
            )
            .bind(resolution_res.edition_id)
            .bind(outbox_payload)
            .execute(&mut *tx)
            .await?;

            imported_count += 1;
        }

        let checkpoint = (chunk_idx + 1) * CHUNK_SIZE;
        update_import_run_progress(
            &mut *tx,
            run.id,
            checkpoint as i64,
            imported_count,
            quarantined_count,
            duplicate_count,
            ImportRunStatus::Running.as_str(),
            None,
        )
        .await?;

        tx.commit().await?;
    }

    // 完成状态更新
    let final_status = if quarantined_count > 0 && imported_count == 0 {
        ImportRunStatus::Failed
    } else if quarantined_count > 0 {
        ImportRunStatus::PartiallyFailed
    } else {
        ImportRunStatus::Completed
    };

    update_import_run_progress(
        pool,
        run.id,
        total_rows,
        imported_count,
        quarantined_count,
        duplicate_count,
        final_status.as_str(),
        None,
    )
    .await?;

    Ok(ImportExecutionResult {
        run_id: run.id,
        import_file_id: import_file.id,
        total_rows,
        imported_count,
        duplicate_count,
        quarantined_count,
        status: final_status.as_str().to_string(),
    })
}

/// 流式 CSV 解析器，自动识别列头或按顺序映射。
pub fn parse_csv_stream(text: &str) -> AppResult<Vec<ParsedCatalogItem>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());

    let mut items = Vec::new();
    let mut headers: Option<Vec<String>> = None;

    for result in rdr.records() {
        let record = result.map_err(|e| AppError::bad(format!("CSV 解析错误: {e}")))?;
        if record.is_empty() {
            continue;
        }

        // 首行如果包含典型列头词，识别为表头
        if headers.is_none() {
            let looks_like_header = record.iter().any(|c| {
                let lower = c.to_lowercase();
                lower.contains("title")
                    || lower.contains("name")
                    || lower.contains("书名")
                    || lower.contains("isbn")
                    || lower.contains("author")
                    || lower.contains("作者")
                    || lower.contains("press")
                    || lower.contains("publisher")
                    || lower.contains("出版社")
            });

            if looks_like_header {
                headers = Some(record.iter().map(|s| s.trim().to_lowercase()).collect());
                continue;
            }
        }

        let mut item = ParsedCatalogItem::default();
        let mut raw_map = serde_json::Map::new();

        if let Some(ref hdrs) = headers {
            for (idx, field) in record.iter().enumerate() {
                let hdr = hdrs.get(idx).map(|s| s.as_str()).unwrap_or("");
                raw_map.insert(
                    hdr.to_string(),
                    serde_json::Value::String(field.to_string()),
                );

                if hdr.contains("title")
                    || hdr.contains("bookname")
                    || hdr.contains("name")
                    || hdr.contains("书名")
                {
                    item.raw_title = field.to_string();
                } else if hdr.contains("author") || hdr.contains("authors") || hdr.contains("作者")
                {
                    item.raw_author = Some(field.to_string());
                } else if hdr.contains("publisher")
                    || hdr.contains("press")
                    || hdr.contains("出版社")
                {
                    item.raw_publisher = Some(field.to_string());
                } else if hdr.contains("isbn") {
                    item.raw_isbn = Some(field.to_string());
                } else if hdr.contains("doi") {
                    item.raw_doi = Some(field.to_string());
                } else if hdr.contains("year")
                    || hdr.contains("pubdate")
                    || hdr.contains("publishdate")
                    || hdr.contains("出版年")
                {
                    item.raw_year = Some(field.to_string());
                } else if hdr.contains("lang") || hdr.contains("language") || hdr.contains("语种")
                {
                    item.raw_language = Some(field.to_string());
                } else if hdr.contains("category")
                    || hdr.contains("scode")
                    || hdr.contains("ztcode")
                    || hdr.contains("分类")
                {
                    item.raw_category = Some(field.to_string());
                } else if hdr.contains("intro") || hdr.contains("简介") {
                    item.intro = Some(field.to_string());
                } else if hdr.contains("extension")
                    || hdr.contains("format")
                    || hdr.contains("格式")
                {
                    item.format = Some(field.to_string());
                } else if hdr.contains("md5") {
                    item.md5 = Some(field.to_string());
                } else if hdr.contains("filesize") || hdr.contains("size") {
                    item.filesize = field.parse::<i64>().ok();
                } else if hdr.contains("id") || hdr.contains("bookid") || hdr.contains("damscode") {
                    item.external_id = Some(field.to_string());
                }
            }

            if hdrs
                .iter()
                .any(|h| h.contains("chapter") || h.contains("章节"))
            {
                item.work_type = WorkType::Chapter;
            }
        } else {
            // 无表头：按默认列序
            // 4列模式: title, author, publisher, isbn
            // 或 11列模式: id, title, author, lang, ext, size, publisher, year, isbns, category, md5
            match record.len() {
                4 => {
                    item.raw_title = record.get(0).unwrap_or("").to_string();
                    item.raw_author = record.get(1).map(|s| s.to_string());
                    item.raw_publisher = record.get(2).map(|s| s.to_string());
                    item.raw_isbn = record.get(3).map(|s| s.to_string());
                }
                11 => {
                    item.external_id = record.get(0).map(|s| s.to_string());
                    item.raw_title = record.get(1).unwrap_or("").to_string();
                    item.raw_author = record.get(2).map(|s| s.to_string());
                    item.raw_language = record.get(3).map(|s| s.to_string());
                    item.format = record.get(4).map(|s| s.to_string());
                    item.filesize = record.get(5).and_then(|s| s.parse::<i64>().ok());
                    item.raw_publisher = record.get(6).map(|s| s.to_string());
                    item.raw_year = record.get(7).map(|s| s.to_string());
                    item.raw_isbn = record.get(8).map(|s| s.to_string());
                    item.raw_category = record.get(9).map(|s| s.to_string());
                    item.md5 = record.get(10).map(|s| s.to_string());
                }
                _ => {
                    item.raw_title = record.get(0).unwrap_or("").to_string();
                    if record.len() > 1 {
                        item.raw_author = record.get(1).map(|s| s.to_string());
                    }
                    if record.len() > 2 {
                        item.raw_publisher = record.get(2).map(|s| s.to_string());
                    }
                    if record.len() > 3 {
                        item.raw_isbn = record.get(3).map(|s| s.to_string());
                    }
                }
            }

            for (idx, field) in record.iter().enumerate() {
                raw_map.insert(
                    format!("col_{idx}"),
                    serde_json::Value::String(field.to_string()),
                );
            }
        }

        item.raw_payload = serde_json::Value::Object(raw_map);
        items.push(item);
    }

    Ok(items)
}
