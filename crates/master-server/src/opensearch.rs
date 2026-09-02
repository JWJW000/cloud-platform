//! OpenSearch 书目查询投影。
//!
//! PostgreSQL 始终是事实源；本模块只维护可删除、可全量重建的搜索副本。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::config::OpenSearchConfig;
use crate::store::catalog_v1::{CatalogOutboxRow, EditionSearchItem};

/// OpenSearch 单页查询结果。
pub struct OpenSearchPage {
    /// 当前页数据。
    pub items: Vec<EditionSearchItem>,
    /// 是否还有下一页。
    pub has_more: bool,
    /// OpenSearch 返回的精确命中数。
    pub total: i64,
    /// 获取状态分面。
    pub status_facets: Vec<(String, i64)>,
    /// 语言分面。
    pub language_facets: Vec<(String, i64)>,
    /// 文件格式分面。
    pub format_facets: Vec<(String, i64)>,
    /// 出版社分面。
    pub publisher_facets: Vec<(String, i64)>,
}

/// OpenSearch HTTP 客户端。URL 和索引名只来自服务端配置，不接受请求参数覆盖。
#[derive(Clone)]
pub struct OpenSearchClient {
    config: Arc<OpenSearchConfig>,
    client: reqwest::Client,
    mapping_ready: Arc<AtomicBool>,
}

impl std::fmt::Debug for OpenSearchClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenSearchClient")
            .field("url", &self.config.url)
            .field("index", &self.config.index)
            .finish_non_exhaustive()
    }
}

impl OpenSearchClient {
    /// 按已验证配置创建客户端。禁用重定向，避免服务端配置错误被利用为跨主机凭据转发。
    pub fn new(config: OpenSearchConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(config.timeout_secs))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("创建 OpenSearch HTTP 客户端失败")?;
        Ok(Self {
            config: Arc::new(config),
            client,
            mapping_ready: Arc::new(AtomicBool::new(false)),
        })
    }

    fn endpoint(&self, suffix: &str) -> String {
        format!(
            "{}/{}/{}",
            self.config.url.trim_end_matches('/'),
            self.config.index,
            suffix.trim_start_matches('/')
        )
    }

    fn request(&self, method: Method, url: String) -> RequestBuilder {
        let request = self.client.request(method, url);
        if self.config.username.is_empty() {
            request
        } else {
            request.basic_auth(&self.config.username, Some(&self.config.password))
        }
    }

    /// 创建索引及字段映射；索引已存在时幂等补充当前版本新增字段。
    pub async fn ensure_index(&self) -> Result<()> {
        if self.mapping_ready.load(Ordering::Acquire) {
            return Ok(());
        }
        let index_url = format!(
            "{}/{}",
            self.config.url.trim_end_matches('/'),
            self.config.index
        );
        let status = self
            .request(Method::HEAD, index_url.clone())
            .send()
            .await
            .context("检查 OpenSearch 索引失败")?
            .status();
        if status.is_success() {
            // 索引长期存在时，应用升级新增的字段不会被 ensure_index 自动带入。
            // 本索引使用 dynamic=strict，缺一个字段就会让整批 Bulk 永久失败。
            // OpenSearch 的 PUT _mapping 对已有同类型字段幂等，并可安全追加新字段。
            self.ensure_mapping().await?;
            self.mapping_ready.store(true, Ordering::Release);
            return Ok(());
        }
        if status != StatusCode::NOT_FOUND {
            return Err(anyhow!("检查 OpenSearch 索引返回 HTTP {status}"));
        }

        let response = self
            .request(Method::PUT, index_url)
            .json(&index_mapping())
            .send()
            .await
            .context("创建 OpenSearch 索引失败")?;
        if response.status().is_success() {
            self.mapping_ready.store(true, Ordering::Release);
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if status == StatusCode::BAD_REQUEST
                && body.contains("resource_already_exists_exception")
            {
                self.ensure_mapping().await?;
                self.mapping_ready.store(true, Ordering::Release);
                Ok(())
            } else {
                Err(anyhow!("创建 OpenSearch 索引失败：HTTP {status}"))
            }
        }
    }

    async fn ensure_mapping(&self) -> Result<()> {
        let response = self
            .request(Method::PUT, self.endpoint("_mapping"))
            .json(&json!({"properties": index_properties()}))
            .send()
            .await
            .context("更新 OpenSearch 索引映射失败")?;
        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let summary: String = body.chars().take(500).collect();
            Err(anyhow!(
                "更新 OpenSearch 索引映射失败：HTTP {status}，{summary}"
            ))
        }
    }

    /// 删除并重新创建搜索投影。只能由显式的 `reindex-catalog` 命令调用。
    pub async fn recreate_index(&self) -> Result<()> {
        let index_url = format!(
            "{}/{}",
            self.config.url.trim_end_matches('/'),
            self.config.index
        );
        let response = self.request(Method::DELETE, index_url).send().await?;
        if !response.status().is_success() && response.status() != StatusCode::NOT_FOUND {
            return Err(anyhow!(
                "删除旧 OpenSearch 索引失败：HTTP {}",
                response.status()
            ));
        }
        self.mapping_ready.store(false, Ordering::Release);
        self.ensure_index().await
    }

    /// 执行目录搜索。关键词会被转义为普通文本，不能注入 OpenSearch Query DSL。
    #[allow(clippy::too_many_arguments)]
    pub async fn search(
        &self,
        keyword: &str,
        acquisition_status: Option<&str>,
        work_type: Option<&str>,
        language: Option<&str>,
        format: Option<&str>,
        resolution_status: Option<&str>,
        limit: i64,
        cursor_updated_at: Option<DateTime<Utc>>,
        cursor_id: Option<Uuid>,
        forward: bool,
    ) -> Result<OpenSearchPage> {
        let keyword = keyword.trim();
        if keyword.is_empty() || keyword.chars().count() > 200 {
            return Err(anyhow!("检索关键词长度必须在 1..=200 个字符之间"));
        }
        let escaped = escape_wildcard(keyword);
        let pattern = format!("*{escaped}*");
        let identifier_exact: String = keyword
            .chars()
            .filter(|ch| !ch.is_whitespace() && *ch != '-')
            .flat_map(char::to_lowercase)
            .collect();

        let mut filters = Vec::new();
        if let Some(value) = acquisition_status {
            if value == "__actionable__" {
                filters.push(json!({"terms": {"acquisition_status": [
                    "已领取", "下载中", "校验中", "暂时失败", "来源无效", "人工确认"
                ]}}));
            } else if value == "总库已拥有" {
                // 兼容切换语义前已经建立的 OpenSearch 文档；旧的“待下载/排队中”
                // 来自总库自动目标，并不代表用户导入的下载任务。
                filters.push(json!({"terms": {"acquisition_status": [
                    "总库已拥有", "待下载", "排队中", "暂不获取"
                ]}}));
            } else {
                filters.push(json!({"term": {"acquisition_status": value}}));
            }
        }
        for (field, value) in [
            ("work_type", work_type),
            ("language", language),
            ("resolution_status", resolution_status),
        ] {
            if let Some(value) = value {
                filters.push(json!({"term": {field: value}}));
            }
        }
        if let Some(value) = format {
            filters.push(json!({"bool": {"should": [
                {"term": {"source_formats": value}},
                {"term": {"holding_formats": value}}
            ], "minimum_should_match": 1}}));
        }

        let wildcard = |field: &str, boost: f64| {
            json!({"wildcard": {field: {
                "value": pattern,
                "case_insensitive": true,
                "boost": boost,
                "rewrite": "constant_score"
            }}})
        };
        let query = json!({
            "bool": {
                "filter": filters,
                "should": [
                    {"term": {"identifiers_exact": {"value": identifier_exact, "boost": 20.0}}},
                    wildcard("title", 5.0),
                    wildcard("authors", 3.0),
                    wildcard("publisher", 2.0),
                    wildcard("identifiers", 10.0)
                ],
                "minimum_should_match": 1
            }
        });

        let direction = if forward { "desc" } else { "asc" };
        let mut body = json!({
            "size": limit.clamp(1, 100) + 1,
            "track_total_hits": true,
            "timeout": format!("{}s", self.config.timeout_secs.min(30)),
            "query": query,
            "sort": [
                {"updated_at": {"order": direction}},
                {"id": {"order": direction}}
            ],
            "aggs": {
                "statuses": {"terms": {"field": "acquisition_status", "size": 20}},
                "languages": {"terms": {"field": "language", "size": 30}},
                "formats": {"terms": {"field": "holding_formats", "size": 30}},
                "publishers": {"terms": {"field": "publisher_exact", "size": 20}}
            }
        });
        if let (Some(updated_at), Some(id)) = (cursor_updated_at, cursor_id) {
            body["search_after"] = json!([updated_at.to_rfc3339(), id.to_string()]);
        }

        let response = self
            .request(Method::POST, self.endpoint("_search"))
            .json(&body)
            .send()
            .await
            .context("请求 OpenSearch 检索失败")?;
        if !response.status().is_success() {
            return Err(anyhow!("OpenSearch 检索返回 HTTP {}", response.status()));
        }
        let result: SearchResponse = response
            .json()
            .await
            .context("解析 OpenSearch 检索响应失败")?;
        if result.timed_out {
            return Err(anyhow!("OpenSearch 检索超时"));
        }

        let page_limit = limit.clamp(1, 100) as usize;
        let has_more = result.hits.hits.len() > page_limit;
        let mut items: Vec<EditionSearchItem> = result
            .hits
            .hits
            .into_iter()
            .take(page_limit)
            .map(|hit| hit.source.into())
            .collect();
        if !forward {
            items.reverse();
        }
        let mut status_facets = std::collections::HashMap::<String, i64>::new();
        for (status, count) in buckets(result.aggregations.as_ref(), "statuses") {
            let normalized = match status.as_str() {
                "待下载" | "排队中" | "暂不获取" => "总库已拥有",
                _ => status.as_str(),
            };
            *status_facets.entry(normalized.to_string()).or_default() += count;
        }
        Ok(OpenSearchPage {
            items,
            has_more,
            total: result.hits.total.value,
            status_facets: status_facets.into_iter().collect(),
            language_facets: buckets(result.aggregations.as_ref(), "languages"),
            format_facets: buckets(result.aggregations.as_ref(), "formats"),
            publisher_facets: buckets(result.aggregations.as_ref(), "publishers"),
        })
    }

    async fn bulk_documents(&self, documents: &[SearchDocument]) -> Result<BulkWriteResult> {
        if documents.is_empty() {
            return Ok(BulkWriteResult::default());
        }
        let mut body = String::with_capacity(documents.len() * 1_024);
        for document in documents {
            body.push_str(&serde_json::to_string(
                &json!({"index": {"_id": document.id}}),
            )?);
            body.push('\n');
            let mut doc_val = serde_json::to_value(document)?;
            if let Some(ref pub_str) = document.publisher {
                doc_val["publisher_exact"] = serde_json::Value::String(pub_str.clone());
            }
            body.push_str(&serde_json::to_string(&doc_val)?);
            body.push('\n');
        }
        let response = self
            .request(Method::POST, self.endpoint("_bulk"))
            .header("content-type", "application/x-ndjson")
            .body(body)
            .send()
            .await
            .context("批量写入 OpenSearch 失败")?;
        if !response.status().is_success() {
            return Err(anyhow!("OpenSearch Bulk 返回 HTTP {}", response.status()));
        }
        let result: BulkResponse = response.json().await?;
        if !result.errors {
            return Ok(BulkWriteResult {
                succeeded: documents.iter().map(|document| document.id).collect(),
                failed: HashSet::new(),
            });
        }

        let expected: HashSet<Uuid> = documents.iter().map(|document| document.id).collect();
        let mut succeeded = HashSet::new();
        let mut failure_summaries = Vec::new();
        for item in result.items {
            let Some(operation) = item.into_values().next() else {
                continue;
            };
            let Ok(id) = Uuid::parse_str(&operation.id) else {
                continue;
            };
            if operation.status < 300 && operation.error.is_none() {
                succeeded.insert(id);
            } else if failure_summaries.len() < 3 {
                let reason = operation
                    .error
                    .as_ref()
                    .and_then(|error| error.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("OpenSearch 未返回失败原因");
                failure_summaries.push(format!("{id}: HTTP {} {reason}", operation.status));
            }
        }
        let failed: HashSet<Uuid> = expected.difference(&succeeded).copied().collect();
        if failed.is_empty() {
            return Err(anyhow!("OpenSearch Bulk 报告失败但未返回可识别的失败项目"));
        }
        tracing::warn!(
            failed = failed.len(),
            samples = %failure_summaries.join("；"),
            "OpenSearch Bulk 部分项目失败，成功项目将继续确认"
        );
        Ok(BulkWriteResult { succeeded, failed })
    }

    async fn delete_documents(&self, ids: &[Uuid]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut body = String::new();
        for id in ids {
            body.push_str(&serde_json::to_string(&json!({"delete": {"_id": id}}))?);
            body.push('\n');
        }
        let response = self
            .request(Method::POST, self.endpoint("_bulk"))
            .header("content-type", "application/x-ndjson")
            .body(body)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "OpenSearch 删除文档返回 HTTP {}",
                response.status()
            ));
        }
        Ok(())
    }
}

/// 消费一批 PostgreSQL Outbox。只有 OpenSearch 确认整批成功后才标记已同步。
pub async fn process_outbox_events(
    pool: &PgPool,
    client: &OpenSearchClient,
    batch_size: usize,
) -> Result<usize> {
    let events: Vec<CatalogOutboxRow> = sqlx::query_as(
        "SELECT id, event_type, aggregate_type, aggregate_id, payload, status, created_at, synced_at \
         FROM catalog_outbox WHERE status = '待同步' ORDER BY id ASC LIMIT $1",
    )
    .bind(batch_size.clamp(1, 2_000) as i64)
    .fetch_all(pool)
    .await?;
    if events.is_empty() {
        return Ok(0);
    }

    let edition_ids: Vec<Uuid> = events
        .iter()
        .filter(|event| event.aggregate_type == "edition")
        .map(|event| event.aggregate_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let documents = load_documents(pool, &edition_ids).await?;
    let existing: HashSet<Uuid> = documents.iter().map(|doc| doc.id).collect();
    let deleted: Vec<Uuid> = edition_ids
        .iter()
        .copied()
        .filter(|id| !existing.contains(id))
        .collect();

    let write_result = client.bulk_documents(&documents).await?;
    client.delete_documents(&deleted).await?;

    let mut acknowledged_aggregates = write_result.succeeded;
    acknowledged_aggregates.extend(deleted);
    if acknowledged_aggregates.is_empty() {
        return Err(anyhow!("OpenSearch Bulk 中所有项目均失败，事件保留待重试"));
    }

    // 只确认本批次里对应成功文档的事件；失败项目继续留在队列重试，但不再阻塞
    // 同批其他版本。重复 aggregate 在批内自然合并为一次文档写入。
    let ids: Vec<i64> = events
        .iter()
        .filter(|event| acknowledged_aggregates.contains(&event.aggregate_id))
        .map(|event| event.id)
        .collect();
    let acknowledged = sqlx::query(
        "UPDATE catalog_outbox SET status = '已同步', synced_at = now() \
         WHERE status = '待同步' AND id = ANY($1)",
    )
    .bind(&ids)
    .execute(pool)
    .await?
    .rows_affected() as usize;
    Ok(acknowledged)
}

/// 启动常驻 Outbox 同步器。OpenSearch 故障不会拖垮主服务，事件会保留并退避重试。
pub fn spawn_outbox_sync(pool: PgPool, client: OpenSearchClient, config: OpenSearchConfig) {
    tokio::spawn(async move {
        let mut failures = 0_u32;
        loop {
            match client.ensure_index().await {
                Ok(()) => match process_outbox_events(&pool, &client, config.batch_size).await {
                    Ok(processed) => {
                        failures = 0;
                        if processed > 0 {
                            tracing::info!(processed, "OpenSearch 书目索引增量同步完成");
                        }
                    }
                    Err(error) => {
                        failures = failures.saturating_add(1);
                        tracing::warn!(error = %error, failures, "OpenSearch Outbox 同步失败，将保留事件重试");
                    }
                },
                Err(error) => {
                    failures = failures.saturating_add(1);
                    tracing::warn!(error = %error, failures, "OpenSearch 暂不可用");
                }
            }
            let backoff = if failures == 0 {
                config.poll_millis.max(100)
            } else {
                (1_000_u64.saturating_mul(2_u64.pow(failures.min(6)))).min(60_000)
            };
            tokio::time::sleep(Duration::from_millis(backoff)).await;
        }
    });
}

/// 从 PostgreSQL 全量重建 OpenSearch 投影，返回写入文档数。
pub async fn reindex_catalog(
    pool: &PgPool,
    client: &OpenSearchClient,
    batch_size: usize,
) -> Result<u64> {
    client.recreate_index().await?;
    let mut cursor: Option<Uuid> = None;
    let mut total = 0_u64;
    loop {
        let rows = load_documents_page(pool, cursor, batch_size.clamp(10, 2_000)).await?;
        if rows.is_empty() {
            break;
        }
        cursor = rows.last().map(|row| row.id);
        let result = client.bulk_documents(&rows).await?;
        if !result.failed.is_empty() {
            return Err(anyhow!(
                "OpenSearch 全量索引批次有 {} 条写入失败",
                result.failed.len()
            ));
        }
        total += rows.len() as u64;
        tracing::info!(total, "OpenSearch 全量索引构建中");
    }
    Ok(total)
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
struct SearchDocument {
    id: Uuid,
    work_id: Uuid,
    work_type: String,
    #[sqlx(rename = "edition_title")]
    title: String,
    authors: Vec<String>,
    publisher: Option<String>,
    publisher_id: Option<Uuid>,
    publish_year: Option<i32>,
    language: String,
    identifiers: Vec<String>,
    identifiers_exact: Vec<String>,
    source_formats: Vec<String>,
    holding_formats: Vec<String>,
    acquisition_status: String,
    worker_name: Option<String>,
    acquisition_stage: String,
    attempts: i32,
    max_attempts: i32,
    next_attempt_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    resolution_status: String,
    updated_at: DateTime<Utc>,
}

impl From<SearchDocument> for EditionSearchItem {
    fn from(doc: SearchDocument) -> Self {
        let acquisition_status = match doc.acquisition_status.as_str() {
            // 兼容语义切换前的存量索引；这些状态由旧版“总库自动建目标”产生。
            "待下载" | "排队中" | "暂不获取" => "总库已拥有".to_string(),
            _ => doc.acquisition_status,
        };
        Self {
            id: doc.id,
            work_id: doc.work_id,
            work_type: doc.work_type,
            title: doc.title,
            authors: doc.authors,
            publisher: doc.publisher,
            publisher_id: doc.publisher_id,
            publish_year: doc.publish_year,
            language: doc.language,
            identifiers: doc.identifiers,
            source_formats: doc.source_formats,
            holding_formats: doc.holding_formats,
            acquisition_status,
            worker_name: doc.worker_name,
            acquisition_stage: doc.acquisition_stage,
            attempts: doc.attempts,
            max_attempts: doc.max_attempts,
            next_attempt_at: doc.next_attempt_at,
            last_error: doc.last_error,
            resolution_status: doc.resolution_status,
            updated_at: doc.updated_at,
        }
    }
}

const DOCUMENT_SELECT: &str =
    "SELECT e.id, e.work_id, w.work_type, e.edition_title, \
        ARRAY(SELECT c.name FROM edition_contributors ec JOIN contributors c ON c.id = ec.contributor_id \
              WHERE ec.edition_id = e.id ORDER BY ec.sort_order) AS authors, \
        e.publisher, e.publisher_id, e.publish_year, e.language, \
        ARRAY(SELECT i.normalized_value FROM identifiers i \
              WHERE i.object_type = 'edition' AND i.object_id = e.id AND i.is_valid ORDER BY i.created_at LIMIT 20) AS identifiers, \
        ARRAY(SELECT i.normalized_value FROM identifiers i \
              WHERE i.object_type = 'edition' AND i.object_id = e.id AND i.is_valid ORDER BY i.created_at LIMIT 20) AS identifiers_exact, \
        ARRAY(SELECT DISTINCT sa.format FROM record_resolutions rr JOIN source_assets sa ON sa.source_record_id = rr.source_record_id \
              WHERE rr.edition_id = e.id) AS source_formats, \
        ARRAY(SELECT DISTINCT lf.format FROM holdings h JOIN library_files lf ON lf.id = h.library_file_id \
              WHERE h.edition_id = e.id) AS holding_formats, \
        CASE WHEN at.status IS NULL OR at.status = '暂不获取' \
             THEN '总库已拥有' ELSE at.status END AS acquisition_status, wn.name AS worker_name, \
        coalesce((SELECT x.stage FROM acquisition_executions x WHERE x.target_id = at.id ORDER BY x.started_at DESC LIMIT 1), '') AS acquisition_stage, \
        coalesce(at.attempts, 0) AS attempts, coalesce(at.max_attempts, 5) AS max_attempts, \
        at.next_attempt_at, at.last_error, w.resolution_status, e.updated_at \
     FROM editions e JOIN works w ON w.id = e.work_id \
     LEFT JOIN acquisition_targets at ON at.edition_id = e.id \
     LEFT JOIN worker_nodes wn ON wn.id = at.lease_node_id";

async fn load_documents(pool: &PgPool, ids: &[Uuid]) -> Result<Vec<SearchDocument>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!("{DOCUMENT_SELECT} WHERE e.id = ANY($1) AND e.owned_at IS NOT NULL");
    Ok(sqlx::query_as(&sql).bind(ids).fetch_all(pool).await?)
}

async fn load_documents_page(
    pool: &PgPool,
    cursor: Option<Uuid>,
    limit: usize,
) -> Result<Vec<SearchDocument>> {
    let sql = format!(
        "{DOCUMENT_SELECT} WHERE e.owned_at IS NOT NULL AND ($1::uuid IS NULL OR e.id > $1) ORDER BY e.id ASC LIMIT $2"
    );
    Ok(sqlx::query_as(&sql)
        .bind(cursor)
        .bind(limit as i64)
        .fetch_all(pool)
        .await?)
}

fn index_mapping() -> Value {
    json!({
        "settings": {"number_of_shards": 1, "number_of_replicas": 0},
        "mappings": {
            "dynamic": "strict",
            "properties": index_properties()
        }
    })
}

fn index_properties() -> Value {
    json!({
        "id": {"type": "keyword"},
        "work_id": {"type": "keyword"},
        "work_type": {"type": "keyword"},
        "title": {"type": "wildcard"},
        "authors": {"type": "wildcard"},
        "publisher": {"type": "wildcard"},
        "publisher_exact": {"type": "keyword", "ignore_above": 256},
        "publisher_id": {"type": "keyword"},
        "publish_year": {"type": "integer"},
        "language": {"type": "keyword"},
        "identifiers": {"type": "wildcard"},
        "identifiers_exact": {"type": "keyword", "ignore_above": 512},
        "source_formats": {"type": "keyword"},
        "holding_formats": {"type": "keyword"},
        "acquisition_status": {"type": "keyword"},
        "worker_name": {"type": "keyword", "ignore_above": 512},
        "acquisition_stage": {"type": "keyword"},
        "attempts": {"type": "integer"},
        "max_attempts": {"type": "integer"},
        "next_attempt_at": {"type": "date"},
        "last_error": {"type": "keyword", "index": false, "doc_values": false},
        "resolution_status": {"type": "keyword"},
        "updated_at": {"type": "date"}
    })
}

fn escape_wildcard(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '*' | '?' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    timed_out: bool,
    hits: SearchHits,
    aggregations: Option<HashMap<String, Aggregation>>,
}

#[derive(Deserialize)]
struct SearchHits {
    total: SearchTotal,
    hits: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct SearchTotal {
    value: i64,
}

#[derive(Deserialize)]
struct SearchHit {
    #[serde(rename = "_source")]
    source: SearchDocument,
}

#[derive(Deserialize)]
struct Aggregation {
    #[serde(default)]
    buckets: Vec<AggregationBucket>,
}

#[derive(Deserialize)]
struct AggregationBucket {
    key: String,
    doc_count: i64,
}

#[derive(Deserialize)]
struct BulkResponse {
    #[serde(default)]
    errors: bool,
    #[serde(default)]
    items: Vec<HashMap<String, BulkItemResult>>,
}

#[derive(Deserialize)]
struct BulkItemResult {
    #[serde(rename = "_id")]
    id: String,
    #[serde(default)]
    status: u16,
    error: Option<Value>,
}

#[derive(Default)]
struct BulkWriteResult {
    succeeded: HashSet<Uuid>,
    failed: HashSet<Uuid>,
}

fn buckets(aggregations: Option<&HashMap<String, Aggregation>>, name: &str) -> Vec<(String, i64)> {
    aggregations
        .and_then(|aggs| aggs.get(name))
        .map(|agg| {
            agg.buckets
                .iter()
                .map(|bucket| (bucket.key.clone(), bucket.doc_count))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_document(id: Uuid) -> SearchDocument {
        SearchDocument {
            id,
            work_id: Uuid::new_v4(),
            work_type: "图书".to_string(),
            title: "测试图书".to_string(),
            authors: vec!["测试作者".to_string()],
            publisher: Some("测试出版社".to_string()),
            publisher_id: None,
            publish_year: Some(2026),
            language: "zh".to_string(),
            identifiers: Vec::new(),
            identifiers_exact: Vec::new(),
            source_formats: vec!["pdf".to_string()],
            holding_formats: Vec::new(),
            acquisition_status: "待下载".to_string(),
            worker_name: None,
            acquisition_stage: String::new(),
            attempts: 0,
            max_attempts: 5,
            next_attempt_at: None,
            last_error: None,
            resolution_status: "已确认".to_string(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn wildcard_metacharacters_are_escaped() {
        assert_eq!(escape_wildcard("a*b?c\\d"), "a\\*b\\?c\\\\d");
    }

    #[test]
    fn mapping_is_strict_and_has_wildcard_fields() {
        let mapping = index_mapping();
        assert_eq!(mapping["mappings"]["dynamic"], "strict");
        assert_eq!(
            mapping["mappings"]["properties"]["title"]["type"],
            "wildcard"
        );
    }

    #[tokio::test]
    async fn partial_bulk_failure_keeps_only_the_rejected_document_pending() {
        use axum::body::Bytes;
        use axum::routing::post;
        use axum::Router;

        let app = Router::new().route(
            "/catalog-editions-v1/_bulk",
            post(|body: Bytes| async move {
                let ids: Vec<String> = std::str::from_utf8(&body)
                    .unwrap()
                    .lines()
                    .step_by(2)
                    .map(|line| {
                        serde_json::from_str::<Value>(line).unwrap()["index"]["_id"]
                            .as_str()
                            .unwrap()
                            .to_string()
                    })
                    .collect();
                axum::Json(json!({
                    "errors": true,
                    "items": [
                        {"index": {"_id": ids[0], "status": 201}},
                        {"index": {"_id": ids[1], "status": 400, "error": {"reason": "字段不兼容"}}}
                    ]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = OpenSearchClient::new(OpenSearchConfig {
            enabled: true,
            url: format!("http://{address}"),
            ..Default::default()
        })
        .unwrap();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let result = client
            .bulk_documents(&[sample_document(first), sample_document(second)])
            .await
            .unwrap();
        assert_eq!(result.succeeded, HashSet::from([first]));
        assert_eq!(result.failed, HashSet::from([second]));
    }
}
