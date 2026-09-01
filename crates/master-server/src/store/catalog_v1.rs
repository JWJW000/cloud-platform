//! 图书馆总库 V1 PostgreSQL 持久化访问层。
//!
//! 包含总库、版本、来源记录、标识符、馆藏文件、获取目标与导入流水线的全量持久化操作。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgExecutor, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};

// ============================================================ 结构定义

/// 数据源表行记录。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CatalogSourceRow {
    /// 来源编号。
    pub id: Uuid,
    /// 来源名称。
    pub name: String,
    /// 来源类型（如 excel / csv / api）。
    pub source_type: String,
    /// 来源说明。
    pub description: Option<String>,
    /// 优先级。
    pub priority: i32,
    /// 是否启用。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 导入文件表行记录。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ImportFileRow {
    /// 导入文件编号。
    pub id: Uuid,
    /// 数据源编号。
    pub source_id: Uuid,
    /// 文件路径或名称。
    pub file_path: String,
    /// 文件 SHA-256 哈希。
    pub file_sha256: String,
    /// 文件大小（字节）。
    pub file_size_bytes: i64,
    /// 工作表名称。
    pub sheet_name: String,
    /// 结构识别版本。
    pub structure_version: String,
    /// 估计总行数。
    pub total_rows: i64,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 导入运行批次记录。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ImportRunRow {
    /// 运行编号。
    pub id: Uuid,
    /// 关联的导入文件编号。
    pub import_file_id: Uuid,
    /// 运行状态。
    pub status: String,
    /// 检查点行号。
    pub checkpoint_row: i64,
    /// 总行数。
    pub total_rows: i64,
    /// 已成功导入行数。
    pub imported_count: i64,
    /// 隔离行数。
    pub quarantined_count: i64,
    /// 重复行数。
    pub duplicate_count: i64,
    /// 错误摘要。
    pub error_summary: Option<String>,
    /// 开始时间。
    pub started_at: Option<DateTime<Utc>>,
    /// 完成时间。
    pub completed_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 来源原始记录。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct SourceRecordRow {
    /// 来源记录编号。
    pub id: Uuid,
    /// 数据源编号。
    pub source_id: Uuid,
    /// 导入文件编号。
    pub import_file_id: Uuid,
    /// 外部来源业务 ID。
    pub external_id: Option<String>,
    /// 工作表名称。
    pub sheet_name: String,
    /// 行号。
    pub row_number: i64,
    /// 原始行 JSON 负载。
    pub raw_payload: serde_json::Value,
    /// 规范化书名。
    pub normalized_title: String,
    /// 规范化作者。
    pub normalized_author: Option<String>,
    /// 规范化出版社。
    pub normalized_publisher: Option<String>,
    /// 原始 ISBN 字符串。
    pub raw_isbn: Option<String>,
    /// 原始 DOI 字符串。
    pub raw_doi: Option<String>,
    /// 原始出版年份。
    pub raw_year: Option<String>,
    /// 原始语种。
    pub raw_language: Option<String>,
    /// 原始分类。
    pub raw_category: Option<String>,
    /// 导入版本。
    pub import_version: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 隔离记录（无法解析的原始行）。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct QuarantinedRecordRow {
    /// 隔离记录编号。
    pub id: Uuid,
    /// 导入运行编号。
    pub import_run_id: Option<Uuid>,
    /// 导入文件编号。
    pub import_file_id: Uuid,
    /// 工作表名称。
    pub sheet_name: String,
    /// 行号。
    pub row_number: i64,
    /// 原始行内容。
    pub raw_content: serde_json::Value,
    /// 隔离原因。
    pub error_reason: String,
    /// 是否已解决。
    pub resolved: bool,
    /// 解决时间。
    pub resolved_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 抽象作品表行记录。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WorkRow {
    /// 作品编号。
    pub id: Uuid,
    /// 作品类型（整书/章节/论文/合集/其他）。
    pub work_type: String,
    /// 首选标题。
    pub preferred_title: String,
    /// 规范化标题。
    pub normalized_title: String,
    /// 主要语言。
    pub primary_language: String,
    /// 父作品编号（章节关联专著）。
    pub parent_work_id: Option<Uuid>,
    /// 消歧状态。
    pub resolution_status: String,
    /// 合并指向的目标作品编号。
    pub merged_into_work_id: Option<Uuid>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 规范版本表行记录。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct EditionRow {
    /// 版本编号。
    pub id: Uuid,
    /// 所属作品编号。
    pub work_id: Uuid,
    /// 版本标题。
    pub edition_title: String,
    /// 语言。
    pub language: String,
    /// 出版社。
    pub publisher: Option<String>,
    /// 关联的规范出版社编号。
    pub publisher_id: Option<Uuid>,
    /// 出版年份。
    pub publish_year: Option<i32>,
    /// 出版日期原始文本。
    pub publish_date_text: Option<String>,
    /// 版次。
    pub edition_number: Option<String>,
    /// 简介。
    pub intro: Option<String>,
    /// 格式摘要。
    pub format_summary: Option<String>,
    /// 状态。
    pub status: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 标识符记录。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct IdentifierRow {
    /// 标识符编号。
    pub id: Uuid,
    /// 目标类型（work / edition / source_record）。
    pub object_type: String,
    /// 目标编号。
    pub object_id: Uuid,
    /// 标识符类型（isbn13 / isbn10 / doi / external_id / dams_code / custom）。
    pub identifier_type: String,
    /// 原始值。
    pub raw_value: String,
    /// 规范化值。
    pub normalized_value: String,
    /// 是否有效。
    pub is_valid: bool,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 出版社主表行记录。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PublisherRow {
    /// 出版社编号。
    pub id: Uuid,
    /// 规范名称。
    pub name: String,
    /// 规范化小写去标点名称。
    pub normalized_name: String,
    /// 国家/地区。
    pub country: Option<String>,
    /// 官网。
    pub website: Option<String>,
    /// 描述简介。
    pub description: Option<String>,
    /// 作品数。
    pub works_count: i64,
    /// 版本数。
    pub editions_count: i64,
    /// 馆藏数。
    pub holdings_count: i64,
    /// 已下载获取数。
    pub acquired_count: i64,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 出版社别名表行记录。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PublisherAliasRow {
    /// 别名编号。
    pub id: Uuid,
    /// 关联的出版社编号。
    pub publisher_id: Uuid,
    /// 别名文本。
    pub alias_name: String,
    /// 规范化别名。
    pub normalized_alias: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 贡献者实体。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ContributorRow {
    /// 贡献者编号。
    pub id: Uuid,
    /// 原始名称。
    pub name: String,
    /// 规范化名称。
    pub normalized_name: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 主题分类实体。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct SubjectRow {
    /// 主题编号。
    pub id: Uuid,
    /// 主题类型。
    pub subject_type: String,
    /// 分类代码。
    pub code: Option<String>,
    /// 主题名称。
    pub name: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 来源记录消歧映射。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RecordResolutionRow {
    /// 映射编号。
    pub id: Uuid,
    /// 来源记录编号。
    pub source_record_id: Uuid,
    /// 映射的作品编号。
    pub work_id: Option<Uuid>,
    /// 映射的版本编号。
    pub edition_id: Option<Uuid>,
    /// 匹配方法。
    pub match_method: String,
    /// 置信度。
    pub confidence: f64,
    /// 规则版本。
    pub rule_version: String,
    /// 是否人工裁决。
    pub is_manual: bool,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 来源文件候选资产。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct SourceAssetRow {
    /// 来源文件编号。
    pub id: Uuid,
    /// 来源记录编号。
    pub source_record_id: Uuid,
    /// 格式。
    pub format: String,
    /// 声明大小（字节）。
    pub declared_size_bytes: Option<i64>,
    /// MD5 哈希。
    pub md5: Option<String>,
    /// 下载链接/定位。
    pub download_url: Option<String>,
    /// 状态（可用/不可用/已损坏/未知）。
    pub status: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 馆藏文件实体。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct LibraryFileRow {
    /// 馆藏文件编号。
    pub id: Uuid,
    /// 存储后端。
    pub storage_backend: String,
    /// 存储对象键或相对路径。
    pub object_key: String,
    /// 实际格式。
    pub format: String,
    /// 实际大小（字节）。
    pub actual_size_bytes: i64,
    /// 校验后的 SHA-256。
    pub sha256: String,
    /// 可选 MD5。
    pub md5: Option<String>,
    /// 校验状态。
    pub verify_status: String,
    /// 校验时间。
    pub verified_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 馆藏关联（版本与馆藏文件多对多映射）。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct HoldingRow {
    /// 关联编号。
    pub id: Uuid,
    /// 版本编号。
    pub edition_id: Uuid,
    /// 馆藏文件编号。
    pub library_file_id: Uuid,
    /// 满足的来源资产编号。
    pub source_asset_id: Option<Uuid>,
    /// 匹配方式。
    pub match_type: String,
    /// 是否满足获取策略。
    pub meets_strategy: bool,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 全局获取目标。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AcquisitionTargetRow {
    /// 目标编号。
    pub id: Uuid,
    /// 版本编号。
    pub edition_id: Uuid,
    /// 偏好格式列表。
    pub preferred_formats: serde_json::Value,
    /// 获取状态。
    pub status: String,
    /// 优先级。
    pub priority: i32,
    /// 已尝试次数。
    pub attempts: i32,
    /// 最大尝试次数。
    pub max_attempts: i32,
    /// 下次重试时间。
    pub next_attempt_at: DateTime<Utc>,
    /// 租约节点编号。
    pub lease_node_id: Option<Uuid>,
    /// 租约会话编号。
    pub lease_session_id: Option<Uuid>,
    /// 租约执行记录编号。
    pub lease_execution_id: Option<Uuid>,
    /// 租约过期时间。
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// 当前尝试的来源资产编号。
    pub active_source_asset_id: Option<Uuid>,
    /// 满足条件的馆藏关联编号。
    pub satisfied_holding_id: Option<Uuid>,
    /// 最近错误信息。
    pub last_error: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 获取执行记录。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AcquisitionExecutionRow {
    /// 执行编号。
    pub id: Uuid,
    /// 目标编号。
    pub target_id: Uuid,
    /// 尝试的来源资产编号。
    pub source_asset_id: Option<Uuid>,
    /// Worker 节点编号。
    pub node_id: Option<Uuid>,
    /// Worker 会话编号。
    pub session_id: Option<Uuid>,
    /// 槽位索引。
    pub slot_index: Option<i32>,
    /// 执行阶段。
    pub stage: String,
    /// 执行结果。
    pub result: Option<String>,
    /// 错误代码。
    pub error_code: Option<String>,
    /// 错误详情。
    pub error_message: Option<String>,
    /// 开始时间。
    pub started_at: DateTime<Utc>,
    /// 结束时间。
    pub finished_at: Option<DateTime<Utc>>,
}

/// 搜索 Outbox 消息记录。
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CatalogOutboxRow {
    /// 消息序号。
    pub id: i64,
    /// 事件类型。
    pub event_type: String,
    /// 聚合根类型。
    pub aggregate_type: String,
    /// 聚合根编号。
    pub aggregate_id: Uuid,
    /// 载荷。
    pub payload: serde_json::Value,
    /// 状态。
    pub status: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 同步时间。
    pub synced_at: Option<DateTime<Utc>>,
}

/// 总库核心统计数据。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CatalogStats {
    /// 数据源总数。
    pub total_sources: i64,
    /// 来源记录总数。
    pub total_source_records: i64,
    /// 规范作品总数（整书）。
    pub total_works: i64,
    /// 规范版本总数。
    pub total_editions: i64,
    /// 章节总数。
    pub total_chapters: i64,
    /// 馆藏关联总数。
    pub total_holdings: i64,
    /// 有效馆藏文件总数。
    pub total_library_files: i64,
    /// 当前至少关联一份有效文件的版本数。
    pub editions_with_files: i64,
    /// 已拥有但当前没有有效文件的版本数。
    pub editions_without_files: i64,
    /// 馆藏总字节数。
    pub total_library_bytes: i64,
    /// 已下载获取目标数。
    pub acquired_targets: i64,
    /// 待下载/排队中目标数。
    pub pending_targets: i64,
    /// 正在下载/校验目标数。
    pub downloading_targets: i64,
    /// 暂时失败/来源无效数。
    pub failed_targets: i64,
    /// 待人工确认目标数。
    pub needs_confirm_targets: i64,
    /// 未解决隔离记录数。
    pub total_quarantined: i64,
    /// 缺失 ISBN 版本数。
    pub missing_isbn_count: i64,
    /// 缺失作者版本数。
    pub missing_author_count: i64,
    /// 待消歧作品数。
    pub ambiguous_works_count: i64,
    /// 今日新增下载数。
    pub today_downloaded_count: i64,
    /// 今日新增总库作品数。
    pub today_added_works_count: i64,
}

/// 版本卡片搜索摘要项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditionSearchItem {
    /// 版本编号。
    pub id: Uuid,
    /// 作品编号。
    pub work_id: Uuid,
    /// 作品类型。
    pub work_type: String,
    /// 标题。
    pub title: String,
    /// 作者列表。
    pub authors: Vec<String>,
    /// 出版社。
    pub publisher: Option<String>,
    /// 关联的规范出版社编号。
    pub publisher_id: Option<Uuid>,
    /// 出版年份。
    pub publish_year: Option<i32>,
    /// 语言。
    pub language: String,
    /// 标识符（ISBN 等）。
    pub identifiers: Vec<String>,
    /// 来源格式列表。
    pub source_formats: Vec<String>,
    /// 已有馆藏格式列表。
    pub holding_formats: Vec<String>,
    /// 获取状态。
    pub acquisition_status: String,
    /// 当前 Worker 与执行阶段（仅获取任务页使用）。
    pub worker_name: Option<String>,
    /// 当前获取任务的技术阶段。
    pub acquisition_stage: String,
    /// 尝试/重试现场。
    pub attempts: i32,
    /// 最大尝试次数。
    pub max_attempts: i32,
    /// 下一次允许重试的时间。
    pub next_attempt_at: Option<DateTime<Utc>>,
    /// 最近一次失败的脱敏摘要。
    pub last_error: Option<String>,
    /// 消歧状态。
    pub resolution_status: String,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 完整的版本详情聚合。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditionDetail {
    /// 版本信息。
    pub edition: EditionRow,
    /// 所属作品信息。
    pub work: WorkRow,
    /// 同作品下的其他版本。
    pub sibling_editions: Vec<EditionRow>,
    /// 标识符列表。
    pub identifiers: Vec<IdentifierRow>,
    /// 贡献者列表。
    pub contributors: Vec<ContributorRow>,
    /// 主题分类列表。
    pub subjects: Vec<SubjectRow>,
    /// 关联来源记录。
    pub source_records: Vec<SourceRecordRow>,
    /// 来源候选资产。
    pub source_assets: Vec<SourceAssetRow>,
    /// 馆藏文件与关联。
    pub holdings: Vec<(HoldingRow, LibraryFileRow)>,
    /// 获取目标。
    pub acquisition_target: Option<AcquisitionTargetRow>,
    /// 最近执行记录。
    pub executions: Vec<AcquisitionExecutionRow>,
}

// ============================================================ 数据源与导入操作

/// 获取或创建数据源。
pub async fn get_or_create_source(
    executor: impl PgExecutor<'_>,
    name: &str,
    source_type: &str,
    description: Option<&str>,
    priority: i32,
) -> AppResult<CatalogSourceRow> {
    let source = sqlx::query_as::<_, CatalogSourceRow>(
        "INSERT INTO catalog_sources (id, name, source_type, description, priority, enabled) \
         VALUES ($1, $2, $3, $4, $5, TRUE) \
         ON CONFLICT (name) DO UPDATE SET \
             source_type = EXCLUDED.source_type, \
             description = COALESCE(EXCLUDED.description, catalog_sources.description), \
             priority = EXCLUDED.priority, \
             updated_at = now() \
         RETURNING id, name, source_type, description, priority, enabled, created_at, updated_at",
    )
    .bind(Uuid::new_v4())
    .bind(name)
    .bind(source_type)
    .bind(description)
    .bind(priority)
    .fetch_one(executor)
    .await?;

    Ok(source)
}

/// 列出所有数据源。
pub async fn list_sources(executor: impl PgExecutor<'_>) -> AppResult<Vec<CatalogSourceRow>> {
    let sources = sqlx::query_as::<_, CatalogSourceRow>(
        "SELECT id, name, source_type, description, priority, enabled, created_at, updated_at \
         FROM catalog_sources ORDER BY priority DESC, created_at ASC",
    )
    .fetch_all(executor)
    .await?;
    Ok(sources)
}

/// 登记导入文件元数据。
#[allow(clippy::too_many_arguments)]
pub async fn register_import_file(
    executor: impl PgExecutor<'_>,
    source_id: Uuid,
    file_path: &str,
    file_sha256: &str,
    file_size_bytes: i64,
    sheet_name: &str,
    structure_version: &str,
    total_rows: i64,
) -> AppResult<ImportFileRow> {
    let row = sqlx::query_as::<_, ImportFileRow>(
        "INSERT INTO import_files \
             (id, source_id, file_path, file_sha256, file_size_bytes, sheet_name, structure_version, total_rows) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (source_id, file_path, sheet_name) DO UPDATE SET \
             file_sha256 = EXCLUDED.file_sha256, \
             file_size_bytes = EXCLUDED.file_size_bytes, \
             structure_version = EXCLUDED.structure_version, \
             total_rows = EXCLUDED.total_rows \
         RETURNING id, source_id, file_path, file_sha256, file_size_bytes, sheet_name, structure_version, total_rows, created_at"
    )
    .bind(Uuid::new_v4())
    .bind(source_id)
    .bind(file_path)
    .bind(file_sha256)
    .bind(file_size_bytes)
    .bind(sheet_name)
    .bind(structure_version)
    .bind(total_rows)
    .fetch_one(executor)
    .await?;

    Ok(row)
}

/// 创建导入运行记录。
pub async fn create_import_run(
    executor: impl PgExecutor<'_>,
    import_file_id: Uuid,
    total_rows: i64,
) -> AppResult<ImportRunRow> {
    let run = sqlx::query_as::<_, ImportRunRow>(
        "INSERT INTO import_runs \
             (id, import_file_id, status, checkpoint_row, total_rows, imported_count, quarantined_count, duplicate_count, started_at) \
         VALUES ($1, $2, '运行中', 0, $3, 0, 0, 0, now()) \
         RETURNING id, import_file_id, status, checkpoint_row, total_rows, imported_count, \
                   quarantined_count, duplicate_count, error_summary, started_at, completed_at, created_at, updated_at"
    )
    .bind(Uuid::new_v4())
    .bind(import_file_id)
    .bind(total_rows)
    .fetch_one(executor)
    .await?;

    Ok(run)
}

/// 更新导入运行进度与检查点。
#[allow(clippy::too_many_arguments)]
pub async fn update_import_run_progress(
    executor: impl PgExecutor<'_>,
    run_id: Uuid,
    checkpoint_row: i64,
    imported_count: i64,
    quarantined_count: i64,
    duplicate_count: i64,
    status: &str,
    error_summary: Option<&str>,
) -> AppResult<()> {
    let is_done = status == "已完成" || status == "部分失败" || status == "失败";
    sqlx::query(
        "UPDATE import_runs SET \
             checkpoint_row = $2, \
             imported_count = $3, \
             quarantined_count = $4, \
             duplicate_count = $5, \
             status = $6, \
             error_summary = $7, \
             completed_at = CASE WHEN $8 THEN now() ELSE completed_at END, \
             updated_at = now() \
         WHERE id = $1",
    )
    .bind(run_id)
    .bind(checkpoint_row)
    .bind(imported_count)
    .bind(quarantined_count)
    .bind(duplicate_count)
    .bind(status)
    .bind(error_summary)
    .bind(is_done)
    .execute(executor)
    .await?;

    Ok(())
}

/// 列出导入运行记录。
pub async fn list_import_runs(
    executor: impl PgExecutor<'_>,
    limit: i64,
) -> AppResult<Vec<ImportRunRow>> {
    let runs = sqlx::query_as::<_, ImportRunRow>(
        "SELECT id, import_file_id, status, checkpoint_row, total_rows, imported_count, \
                quarantined_count, duplicate_count, error_summary, started_at, completed_at, created_at, updated_at \
         FROM import_runs ORDER BY created_at DESC LIMIT $1"
    )
    .bind(limit.clamp(1, 200))
    .fetch_all(executor)
    .await?;

    Ok(runs)
}

/// 获取单个导入运行记录。
pub async fn get_import_run(executor: impl PgExecutor<'_>, id: Uuid) -> AppResult<ImportRunRow> {
    sqlx::query_as::<_, ImportRunRow>(
        "SELECT id, import_file_id, status, checkpoint_row, total_rows, imported_count, \
                quarantined_count, duplicate_count, error_summary, started_at, completed_at, created_at, updated_at \
         FROM import_runs WHERE id = $1"
    )
    .bind(id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| AppError::missing("导入运行记录不存在"))
}

/// 隔离一条解析异常记录。
pub async fn quarantine_record(
    executor: impl PgExecutor<'_>,
    run_id: Option<Uuid>,
    file_id: Uuid,
    sheet_name: &str,
    row_number: i64,
    raw_content: serde_json::Value,
    error_reason: &str,
) -> AppResult<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO quarantined_records \
             (id, import_run_id, import_file_id, sheet_name, row_number, raw_content, error_reason, resolved) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, FALSE)"
    )
    .bind(id)
    .bind(run_id)
    .bind(file_id)
    .bind(sheet_name)
    .bind(row_number)
    .bind(raw_content)
    .bind(error_reason)
    .execute(executor)
    .await?;

    Ok(id)
}

/// 列出隔离记录。
pub async fn list_quarantined_records(
    executor: impl PgExecutor<'_>,
    resolved: Option<bool>,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<QuarantinedRecordRow>> {
    let records = sqlx::query_as::<_, QuarantinedRecordRow>(
        "SELECT id, import_run_id, import_file_id, sheet_name, row_number, raw_content, error_reason, resolved, resolved_at, created_at \
         FROM quarantined_records \
         WHERE ($1::boolean IS NULL OR resolved = $1) \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3"
    )
    .bind(resolved)
    .bind(limit.clamp(1, 200))
    .bind(offset.max(0))
    .fetch_all(executor)
    .await?;

    Ok(records)
}

/// 统计总库核心指标。
pub async fn get_catalog_stats(pool: &PgPool) -> AppResult<CatalogStats> {
    use sqlx::Row;
    // 采用单一联合聚合查询，同时命中部分索引，将 9~14 次扫描合并为单次查询
    let row = sqlx::query(
        r#"
        SELECT
          (SELECT count(*) FROM catalog_sources)::bigint as total_sources,
          (SELECT count(*) FROM source_records)::bigint as total_source_records,
          (SELECT count(*) FROM works WHERE work_type != '章节')::bigint as total_works,
          (SELECT count(*) FROM works WHERE work_type = '章节')::bigint as total_chapters,
          (SELECT count(*) FROM editions)::bigint as total_editions,
          (SELECT count(*) FROM holdings)::bigint as total_holdings,
          (SELECT count(*) FROM library_files WHERE verify_status = '有效')::bigint as total_library_files,
          (SELECT count(DISTINCT h.edition_id) FROM holdings h
             JOIN library_files lf ON lf.id = h.library_file_id
             WHERE h.meets_strategy AND lf.verify_status = '有效')::bigint as editions_with_files,
          (SELECT coalesce(sum(actual_size_bytes), 0) FROM library_files WHERE verify_status = '有效')::bigint as total_library_bytes,
          (SELECT count(*) FROM acquisition_targets WHERE status = '已下载')::bigint as acquired_targets,
          (SELECT count(*) FROM acquisition_targets WHERE status IN ('待下载', '排队中'))::bigint as pending_targets,
          (SELECT count(*) FROM acquisition_targets WHERE status IN ('已领取', '下载中', '校验中'))::bigint as downloading_targets,
          (SELECT count(*) FROM acquisition_targets WHERE status IN ('暂时失败', '来源无效'))::bigint as failed_targets,
          (SELECT count(*) FROM acquisition_targets WHERE status = '人工确认')::bigint as needs_confirm_targets,
          (SELECT count(*) FROM quarantined_records WHERE NOT resolved)::bigint as total_quarantined,
          (SELECT count(*) FROM works WHERE resolution_status = '待消歧')::bigint as ambiguous_works_count,
          (SELECT count(*) FROM holdings WHERE created_at >= CURRENT_DATE)::bigint as today_downloaded_count,
          (SELECT count(*) FROM works WHERE created_at >= CURRENT_DATE AND work_type != '章节')::bigint as today_added_works_count
        "#,
    )
    .fetch_one(pool)
    .await;

    let mut stats = match row {
        Ok(r) => CatalogStats {
            total_sources: r.try_get("total_sources").unwrap_or(0),
            total_source_records: r.try_get("total_source_records").unwrap_or(0),
            total_works: r.try_get("total_works").unwrap_or(0),
            total_chapters: r.try_get("total_chapters").unwrap_or(0),
            total_editions: r.try_get("total_editions").unwrap_or(0),
            total_holdings: r.try_get("total_holdings").unwrap_or(0),
            total_library_files: r.try_get("total_library_files").unwrap_or(0),
            editions_with_files: r.try_get("editions_with_files").unwrap_or(0),
            editions_without_files: (r.try_get::<i64, _>("total_editions").unwrap_or(0)
                - r.try_get::<i64, _>("editions_with_files").unwrap_or(0))
            .max(0),
            total_library_bytes: r.try_get("total_library_bytes").unwrap_or(0),
            acquired_targets: r.try_get("acquired_targets").unwrap_or(0),
            pending_targets: r.try_get("pending_targets").unwrap_or(0),
            downloading_targets: r.try_get("downloading_targets").unwrap_or(0),
            failed_targets: r.try_get("failed_targets").unwrap_or(0),
            needs_confirm_targets: r.try_get("needs_confirm_targets").unwrap_or(0),
            total_quarantined: r.try_get("total_quarantined").unwrap_or(0),
            missing_isbn_count: 0,
            missing_author_count: 0,
            ambiguous_works_count: r.try_get("ambiguous_works_count").unwrap_or(0),
            today_downloaded_count: r.try_get("today_downloaded_count").unwrap_or(0),
            today_added_works_count: r.try_get("today_added_works_count").unwrap_or(0),
        },
        Err(e) => {
            tracing::warn!(error = %e, "获取图书总库统计指标失败，使用默认值");
            CatalogStats::default()
        }
    };

    // 缺失 ISBN 与作者的计数采用轻量独立计算
    let missing_isbn: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM editions e WHERE NOT EXISTS (SELECT 1 FROM identifiers i WHERE i.object_id = e.id AND i.identifier_type IN ('isbn13', 'isbn10') AND i.is_valid)"#
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    stats.missing_isbn_count = missing_isbn;

    let missing_author: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM editions e WHERE NOT EXISTS (SELECT 1 FROM edition_contributors ec WHERE ec.edition_id = e.id)"#
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    stats.missing_author_count = missing_author;

    Ok(stats)
}

// ============================================================ 检索与详情查询

#[derive(sqlx::FromRow)]
struct EditionSearchDbRow {
    id: Uuid,
    work_id: Uuid,
    work_type: String,
    edition_title: String,
    publisher: Option<String>,
    publisher_id: Option<Uuid>,
    publish_year: Option<i32>,
    language: String,
    acq_status: String,
    resolution_status: String,
    updated_at: DateTime<Utc>,
    name: Option<String>,
    stage: Option<String>,
    attempts: Option<i32>,
    max_attempts: Option<i32>,
    next_attempt_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

/// 检索版本列表（支持多维筛选与分页）。
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub async fn search_editions(
    pool: &PgPool,
    keyword: Option<&str>,
    acquisition_status: Option<&str>,
    work_type: Option<&str>,
    language: Option<&str>,
    format: Option<&str>,
    resolution_status: Option<&str>,
    limit: i64,
    cursor_updated_at: Option<DateTime<Utc>>,
    cursor_id: Option<Uuid>,
    forward: bool,
) -> AppResult<(Vec<EditionSearchItem>, bool)> {
    let kw_like = keyword.map(|k| format!("%{}%", k.trim().to_lowercase()));
    let fmt_like = format.map(|f| format!("%{}%", f.trim().to_lowercase()));

    let fetch_limit = limit.clamp(1, 100) + 1;

    let sql = if kw_like.is_some() {
        // 存在关键词搜索时：使用 WITH matched_ids (利用各字段独立的 GIN/B-Tree 索引 UNION 极速求并)，百倍提速
        "WITH matched_ids AS ( \
             SELECT id FROM editions WHERE edition_title ILIKE $1 OR publisher ILIKE $1 \
             UNION \
             SELECT e.id FROM works w JOIN editions e ON e.work_id = w.id WHERE w.normalized_title ILIKE $1 OR w.preferred_title ILIKE $1 \
             UNION \
             SELECT object_id AS id FROM identifiers WHERE normalized_value = $11 OR raw_value ILIKE $1 \
             UNION \
             SELECT ec.edition_id AS id FROM contributors c JOIN edition_contributors ec ON ec.contributor_id = c.id WHERE c.name ILIKE $1 \
         ) \
         SELECT e.id, e.work_id, w.work_type, e.edition_title, e.publisher, e.publisher_id, e.publish_year, e.language, \
                CASE WHEN at.status IS NULL OR at.status = '暂不获取' \
                     THEN '总库已拥有' ELSE at.status END as acq_status, \
                w.resolution_status, e.updated_at, \
                wn.name, ae.stage, at.attempts, at.max_attempts, at.next_attempt_at, at.last_error \
         FROM matched_ids m \
         JOIN editions e ON e.id = m.id \
         JOIN works w ON w.id = e.work_id \
         LEFT JOIN acquisition_targets at ON at.edition_id = e.id \
         LEFT JOIN worker_nodes wn ON wn.id = at.lease_node_id \
         LEFT JOIN LATERAL (SELECT stage FROM acquisition_executions x WHERE x.target_id = at.id ORDER BY x.started_at DESC LIMIT 1) ae ON TRUE \
         WHERE ($2::text IS NULL \
                OR ($2 = '__actionable__' AND at.status IN \
                    ('待下载', '排队中', '已领取', '下载中', '校验中', '暂时失败', '来源无效', '人工确认')) \
                OR CASE WHEN at.status IS NULL OR at.status = '暂不获取' \
                        THEN '总库已拥有' ELSE at.status END = $2) \
           AND ($3::text IS NULL OR w.work_type = $3) \
           AND ($4::text IS NULL OR e.language = $4) \
           AND ($5::text IS NULL OR e.format_summary ILIKE $5) \
           AND ($6::text IS NULL OR w.resolution_status = $6) \
           AND ($7::timestamptz IS NULL OR \
                ($9::bool AND (e.updated_at, e.id) < ($7, $8)) OR \
                (NOT $9::bool AND (e.updated_at, e.id) > ($7, $8))) \
         ORDER BY \
           CASE WHEN $9::bool THEN e.updated_at END DESC, \
           CASE WHEN $9::bool THEN e.id END DESC, \
           CASE WHEN NOT $9::bool THEN e.updated_at END ASC, \
           CASE WHEN NOT $9::bool THEN e.id END ASC \
         LIMIT $10"
    } else {
        // 无关键词搜索时：直接走 (updated_at, id) 覆盖索引与外键关联，1~2ms 响应
        "SELECT e.id, e.work_id, w.work_type, e.edition_title, e.publisher, e.publisher_id, e.publish_year, e.language, \
                CASE WHEN at.status IS NULL OR at.status = '暂不获取' \
                     THEN '总库已拥有' ELSE at.status END as acq_status, \
                w.resolution_status, e.updated_at, \
                wn.name, ae.stage, at.attempts, at.max_attempts, at.next_attempt_at, at.last_error \
         FROM editions e \
         JOIN works w ON w.id = e.work_id \
         LEFT JOIN acquisition_targets at ON at.edition_id = e.id \
         LEFT JOIN worker_nodes wn ON wn.id = at.lease_node_id \
         LEFT JOIN LATERAL (SELECT stage FROM acquisition_executions x WHERE x.target_id = at.id ORDER BY x.started_at DESC LIMIT 1) ae ON TRUE \
         WHERE ($2::text IS NULL \
                OR ($2 = '__actionable__' AND at.status IN \
                    ('待下载', '排队中', '已领取', '下载中', '校验中', '暂时失败', '来源无效', '人工确认')) \
                OR CASE WHEN at.status IS NULL OR at.status = '暂不获取' \
                        THEN '总库已拥有' ELSE at.status END = $2) \
           AND ($3::text IS NULL OR w.work_type = $3) \
           AND ($4::text IS NULL OR e.language = $4) \
           AND ($5::text IS NULL OR e.format_summary ILIKE $5) \
           AND ($6::text IS NULL OR w.resolution_status = $6) \
           AND ($7::timestamptz IS NULL OR \
                ($9::bool AND (e.updated_at, e.id) < ($7, $8)) OR \
                (NOT $9::bool AND (e.updated_at, e.id) > ($7, $8))) \
         ORDER BY \
           CASE WHEN $9::bool THEN e.updated_at END DESC, \
           CASE WHEN $9::bool THEN e.id END DESC, \
           CASE WHEN NOT $9::bool THEN e.updated_at END ASC, \
           CASE WHEN NOT $9::bool THEN e.id END ASC \
         LIMIT $10"
    };

    let kw_exact = keyword.map(|k| k.trim().to_string());

    let mut rows: Vec<EditionSearchDbRow> = sqlx::query_as(sql)
        .bind(kw_like)
        .bind(acquisition_status)
        .bind(work_type)
        .bind(language)
        .bind(fmt_like)
        .bind(resolution_status)
        .bind(cursor_updated_at)
        .bind(cursor_id)
        .bind(forward)
        .bind(fetch_limit)
        .bind(kw_exact)
        .fetch_all(pool)
        .await?;

    let has_more = rows.len() as i64 > limit.clamp(1, 100);
    rows.truncate(limit.clamp(1, 100) as usize);
    if !forward {
        rows.reverse();
    }

    if rows.is_empty() {
        return Ok((Vec::new(), false));
    }

    let edition_ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();

    // 批量一次性查询所有作者 (避免 N*4 次循环子查询)
    let all_authors: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT ec.edition_id, c.name \
         FROM edition_contributors ec \
         JOIN contributors c ON c.id = ec.contributor_id \
         WHERE ec.edition_id = ANY($1) \
         ORDER BY ec.edition_id, ec.sort_order",
    )
    .bind(&edition_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    // 批量一次性查询所有标识符
    let all_identifiers: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT object_id, normalized_value \
         FROM identifiers \
         WHERE object_id = ANY($1) AND is_valid",
    )
    .bind(&edition_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    // 批量一次性查询来源格式
    let all_source_formats: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT DISTINCT rr.edition_id, sa.format \
         FROM record_resolutions rr \
         JOIN source_assets sa ON sa.source_record_id = rr.source_record_id \
         WHERE rr.edition_id = ANY($1)",
    )
    .bind(&edition_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    // 批量一次性查询馆藏格式
    let all_holding_formats: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT DISTINCT h.edition_id, lf.format \
         FROM holdings h \
         JOIN library_files lf ON lf.id = h.library_file_id \
         WHERE h.edition_id = ANY($1)",
    )
    .bind(&edition_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    use std::collections::HashMap;
    let mut authors_map: HashMap<Uuid, Vec<String>> = HashMap::new();
    for (eid, name) in all_authors {
        authors_map.entry(eid).or_default().push(name);
    }

    let mut idents_map: HashMap<Uuid, Vec<String>> = HashMap::new();
    for (eid, val) in all_identifiers {
        let list = idents_map.entry(eid).or_default();
        if list.len() < 5 {
            list.push(val);
        }
    }

    let mut src_fmt_map: HashMap<Uuid, Vec<String>> = HashMap::new();
    for (eid, fmt) in all_source_formats {
        src_fmt_map.entry(eid).or_default().push(fmt);
    }

    let mut hld_fmt_map: HashMap<Uuid, Vec<String>> = HashMap::new();
    for (eid, fmt) in all_holding_formats {
        hld_fmt_map.entry(eid).or_default().push(fmt);
    }

    let mut items = Vec::with_capacity(rows.len());
    for r in rows {
        let authors = authors_map.remove(&r.id).unwrap_or_default();
        let identifiers = idents_map.remove(&r.id).unwrap_or_default();
        let source_formats = src_fmt_map.remove(&r.id).unwrap_or_default();
        let holding_formats = hld_fmt_map.remove(&r.id).unwrap_or_default();

        items.push(EditionSearchItem {
            id: r.id,
            work_id: r.work_id,
            work_type: r.work_type,
            title: r.edition_title,
            authors,
            publisher: r.publisher,
            publisher_id: r.publisher_id,
            publish_year: r.publish_year,
            language: r.language,
            identifiers,
            source_formats,
            holding_formats,
            acquisition_status: r.acq_status,
            worker_name: r.name,
            acquisition_stage: r.stage.unwrap_or_default(),
            attempts: r.attempts.unwrap_or(0),
            max_attempts: r.max_attempts.unwrap_or(5),
            next_attempt_at: r.next_attempt_at,
            last_error: r.last_error,
            resolution_status: r.resolution_status,
            updated_at: r.updated_at,
        });
    }

    Ok((items, has_more))
}

/// 查询版本完整详情聚合。
pub async fn get_edition_detail(pool: &PgPool, id: Uuid) -> AppResult<EditionDetail> {
    let edition: EditionRow = sqlx::query_as(
        "SELECT id, work_id, edition_title, language, publisher, publisher_id, publish_year, publish_date_text, \
                edition_number, intro, format_summary, status, created_at, updated_at \
         FROM editions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::missing("图书版本不存在"))?;

    let work: WorkRow = sqlx::query_as(
        "SELECT id, work_type, preferred_title, normalized_title, primary_language, parent_work_id, \
                resolution_status, merged_into_work_id, created_at, updated_at \
         FROM works WHERE id = $1"
    )
    .bind(edition.work_id)
    .fetch_one(pool)
    .await?;

    let sibling_editions: Vec<EditionRow> = sqlx::query_as(
        "SELECT id, work_id, edition_title, language, publisher, publisher_id, publish_year, publish_date_text, \
                edition_number, intro, format_summary, status, created_at, updated_at \
         FROM editions WHERE work_id = $1 AND id != $2 ORDER BY publish_year DESC NULLS LAST",
    )
    .bind(edition.work_id)
    .bind(id)
    .fetch_all(pool)
    .await?;

    let identifiers: Vec<IdentifierRow> = sqlx::query_as(
        "SELECT id, object_type, object_id, identifier_type, raw_value, normalized_value, is_valid, created_at \
         FROM identifiers WHERE (object_type = 'edition' AND object_id = $1) OR (object_type = 'work' AND object_id = $2)"
    )
    .bind(id)
    .bind(work.id)
    .fetch_all(pool)
    .await?;

    let contributors: Vec<ContributorRow> = sqlx::query_as(
        "SELECT c.id, c.name, c.normalized_name, c.created_at \
         FROM edition_contributors ec \
         JOIN contributors c ON c.id = ec.contributor_id \
         WHERE ec.edition_id = $1 \
         ORDER BY ec.sort_order",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    let subjects: Vec<SubjectRow> = sqlx::query_as(
        "SELECT s.id, s.subject_type, s.code, s.name, s.created_at \
         FROM edition_subjects es \
         JOIN subjects s ON s.id = es.subject_id \
         WHERE es.edition_id = $1",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    let source_records: Vec<SourceRecordRow> = sqlx::query_as(
        "SELECT sr.id, sr.source_id, sr.import_file_id, sr.external_id, sr.sheet_name, sr.row_number, \
                sr.raw_payload, sr.normalized_title, sr.normalized_author, sr.normalized_publisher, \
                sr.raw_isbn, sr.raw_doi, sr.raw_year, sr.raw_language, sr.raw_category, sr.import_version, sr.created_at \
         FROM record_resolutions rr \
         JOIN source_records sr ON sr.id = rr.source_record_id \
         WHERE rr.edition_id = $1 \
         ORDER BY sr.created_at ASC"
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    let source_assets: Vec<SourceAssetRow> = sqlx::query_as(
        "SELECT sa.id, sa.source_record_id, sa.format, sa.declared_size_bytes, sa.md5, sa.download_url, sa.status, sa.created_at \
         FROM record_resolutions rr \
         JOIN source_assets sa ON sa.source_record_id = rr.source_record_id \
         WHERE rr.edition_id = $1 \
         ORDER BY sa.created_at ASC"
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    let holdings_raw: Vec<(HoldingRow, LibraryFileRow)> = {
        let holdings = sqlx::query_as::<_, HoldingRow>(
            "SELECT id, edition_id, library_file_id, source_asset_id, match_type, meets_strategy, created_at \
             FROM holdings WHERE edition_id = $1"
        )
        .bind(id)
        .fetch_all(pool)
        .await?;

        let mut res = Vec::new();
        for h in holdings {
            if let Some(lf) = sqlx::query_as::<_, LibraryFileRow>(
                "SELECT id, storage_backend, object_key, format, actual_size_bytes, sha256, md5, verify_status, verified_at, created_at, updated_at \
                 FROM library_files WHERE id = $1"
            )
            .bind(h.library_file_id)
            .fetch_optional(pool)
            .await? {
                res.push((h, lf));
            }
        }
        res
    };

    let acquisition_target: Option<AcquisitionTargetRow> = sqlx::query_as(
        "SELECT id, edition_id, preferred_formats, status, priority, attempts, max_attempts, \
                next_attempt_at, lease_node_id, lease_session_id, lease_execution_id, lease_expires_at, \
                active_source_asset_id, satisfied_holding_id, last_error, created_at, updated_at \
         FROM acquisition_targets WHERE edition_id = $1"
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    let executions: Vec<AcquisitionExecutionRow> = if let Some(ref target) = acquisition_target {
        sqlx::query_as(
            "SELECT id, target_id, source_asset_id, node_id, session_id, slot_index, \
                    stage, result, error_code, error_message, started_at, finished_at \
             FROM acquisition_executions WHERE target_id = $1 ORDER BY started_at DESC LIMIT 50",
        )
        .bind(target.id)
        .fetch_all(pool)
        .await?
    } else {
        Vec::new()
    };

    Ok(EditionDetail {
        edition,
        work,
        sibling_editions,
        identifiers,
        contributors,
        subjects,
        source_records,
        source_assets,
        holdings: holdings_raw,
        acquisition_target,
        executions,
    })
}
