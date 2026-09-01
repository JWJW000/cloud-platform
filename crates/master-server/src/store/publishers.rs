//! 出版社管理持久化访问层。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::store::catalog_v1::EditionSearchItem;

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

/// 出版社详情聚合。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublisherDetail {
    /// 出版社基本信息。
    pub publisher: PublisherRow,
    /// 别名列表。
    pub aliases: Vec<PublisherAliasRow>,
}

/// 出版社列表查询参数。
#[derive(Debug, Clone, Deserialize)]
pub struct PublisherListParams {
    /// 关键词搜索。
    pub query: Option<String>,
    /// 排序字段（editions / holdings / acquired / name）。
    pub sort_by: Option<String>,
    /// 分页大小。
    pub limit: Option<i64>,
    /// 分页偏移。
    pub offset: Option<i64>,
}

/// 出版社分页响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublisherListResponse {
    /// 出版社列表。
    pub items: Vec<PublisherRow>,
    /// 总数。
    pub total: i64,
    /// 分页大小。
    pub limit: i64,
    /// 分页偏移。
    pub offset: i64,
}

/// 规范化出版社名称（小写、去空格、去符号）。
pub fn normalize_publisher_name(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_ascii_punctuation() && !c.is_whitespace())
        .collect()
}

/// 获取或创建出版社主档。
pub async fn get_or_create_publisher(
    pool: &PgPool,
    name: &str,
    country: Option<&str>,
) -> AppResult<PublisherRow> {
    let raw_name = name.trim();
    if raw_name.is_empty() {
        return Err(AppError::bad("出版社名称不能为空"));
    }
    let norm = normalize_publisher_name(raw_name);

    // 1. 先查别名表
    let alias_match: Option<Uuid> = sqlx::query_scalar(
        "SELECT publisher_id FROM publisher_aliases WHERE normalized_alias = $1",
    )
    .bind(&norm)
    .fetch_optional(pool)
    .await?;

    if let Some(pid) = alias_match {
        let pub_row = sqlx::query_as::<_, PublisherRow>(
            "SELECT id, name, normalized_name, country, website, description, \
                    works_count, editions_count, holdings_count, acquired_count, created_at, updated_at \
             FROM publishers WHERE id = $1",
        )
        .bind(pid)
        .fetch_one(pool)
        .await?;
        return Ok(pub_row);
    }

    // 2. 查主表或插入
    let pub_row = sqlx::query_as::<_, PublisherRow>(
        "INSERT INTO publishers (name, normalized_name, country) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (normalized_name) DO UPDATE SET \
             country = COALESCE(EXCLUDED.country, publishers.country), \
             updated_at = now() \
         RETURNING id, name, normalized_name, country, website, description, \
                   works_count, editions_count, holdings_count, acquired_count, created_at, updated_at",
    )
    .bind(raw_name)
    .bind(&norm)
    .bind(country)
    .fetch_one(pool)
    .await?;

    Ok(pub_row)
}

/// 分页查询出版社列表。
pub async fn list_publishers(
    pool: &PgPool,
    params: &PublisherListParams,
) -> AppResult<PublisherListResponse> {
    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let offset = params.offset.unwrap_or(0).max(0);
    let kw = params
        .query
        .as_deref()
        .map(|k| k.trim())
        .filter(|k| !k.is_empty());
    let kw_like = kw.map(|k| format!("%{k}%"));

    let total: i64 = if let Some(ref q) = kw_like {
        sqlx::query_scalar(
            "SELECT count(*) FROM publishers WHERE name ILIKE $1 OR description ILIKE $1",
        )
        .bind(q)
        .fetch_one(pool)
        .await?
    } else {
        sqlx::query_scalar("SELECT count(*) FROM publishers")
            .fetch_one(pool)
            .await?
    };

    let order_clause = match params.sort_by.as_deref() {
        Some("editions") => "ORDER BY editions_count DESC, acquired_count DESC",
        Some("holdings") => "ORDER BY holdings_count DESC, editions_count DESC",
        Some("acquired") => "ORDER BY acquired_count DESC, editions_count DESC",
        Some("name") => "ORDER BY name ASC",
        _ => "ORDER BY editions_count DESC, updated_at DESC",
    };

    let sql = format!(
        "SELECT id, name, normalized_name, country, website, description, \
                works_count, editions_count, holdings_count, acquired_count, created_at, updated_at \
         FROM publishers \
         WHERE ($1::text IS NULL OR name ILIKE $1 OR description ILIKE $1) \
         {order_clause} \
         LIMIT $2 OFFSET $3"
    );

    let items = sqlx::query_as::<_, PublisherRow>(&sql)
        .bind(kw_like)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;

    Ok(PublisherListResponse {
        items,
        total,
        limit,
        offset,
    })
}

/// 获取单个出版社详情与别名。
pub async fn get_publisher_detail(pool: &PgPool, id: Uuid) -> AppResult<PublisherDetail> {
    let publisher = sqlx::query_as::<_, PublisherRow>(
        "SELECT id, name, normalized_name, country, website, description, \
                works_count, editions_count, holdings_count, acquired_count, created_at, updated_at \
         FROM publishers WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::missing("出版社不存在"))?;

    let aliases = sqlx::query_as::<_, PublisherAliasRow>(
        "SELECT id, publisher_id, alias_name, normalized_alias, created_at \
         FROM publisher_aliases WHERE publisher_id = $1 ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    Ok(PublisherDetail { publisher, aliases })
}

/// 更新出版社基本信息。
pub async fn update_publisher(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    country: Option<&str>,
    website: Option<&str>,
    description: Option<&str>,
) -> AppResult<PublisherRow> {
    let raw_name = name.trim();
    if raw_name.is_empty() {
        return Err(AppError::bad("出版社名称不能为空"));
    }
    let norm = normalize_publisher_name(raw_name);

    let row = sqlx::query_as::<_, PublisherRow>(
        "UPDATE publishers SET name = $2, normalized_name = $3, country = $4, \
                website = $5, description = $6, updated_at = now() \
         WHERE id = $1 \
         RETURNING id, name, normalized_name, country, website, description, \
                   works_count, editions_count, holdings_count, acquired_count, created_at, updated_at",
    )
    .bind(id)
    .bind(raw_name)
    .bind(&norm)
    .bind(country)
    .bind(website)
    .bind(description)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::missing("出版社不存在"))?;

    Ok(row)
}

/// 为出版社添加别名。
pub async fn add_publisher_alias(
    pool: &PgPool,
    publisher_id: Uuid,
    alias_name: &str,
) -> AppResult<PublisherAliasRow> {
    let raw_alias = alias_name.trim();
    if raw_alias.is_empty() {
        return Err(AppError::bad("别名不能为空"));
    }
    let norm = normalize_publisher_name(raw_alias);

    // 检查出版社是否存在
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM publishers WHERE id = $1)")
        .bind(publisher_id)
        .fetch_one(pool)
        .await?;
    if !exists {
        return Err(AppError::missing("出版社不存在"));
    }

    let alias = sqlx::query_as::<_, PublisherAliasRow>(
        "INSERT INTO publisher_aliases (id, publisher_id, alias_name, normalized_alias) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (normalized_alias) DO UPDATE SET \
             alias_name = EXCLUDED.alias_name \
         RETURNING id, publisher_id, alias_name, normalized_alias, created_at",
    )
    .bind(Uuid::new_v4())
    .bind(publisher_id)
    .bind(raw_alias)
    .bind(&norm)
    .fetch_one(pool)
    .await?;

    // 自动将以此别名录入的 editions 绑定至该出版社
    sqlx::query(
        "UPDATE editions SET publisher_id = $1 \
         WHERE publisher_id IS NULL AND (publisher = $2 OR publisher ILIKE $2)",
    )
    .bind(publisher_id)
    .bind(raw_alias)
    .execute(pool)
    .await?;

    // 重新计算出版社计数
    recalculate_publisher_stats(pool, publisher_id).await?;

    Ok(alias)
}

/// 合并两家出版社（将 source_id 及其别名/图书全部合并到 target_id 并删除 source_id）。
pub async fn merge_publishers(pool: &PgPool, source_id: Uuid, target_id: Uuid) -> AppResult<()> {
    if source_id == target_id {
        return Err(AppError::bad("不能合并同一个出版社"));
    }

    let mut tx = pool.begin().await?;

    let source = sqlx::query_as::<_, PublisherRow>(
        "SELECT id, name, normalized_name, country, website, description, \
                works_count, editions_count, holdings_count, acquired_count, created_at, updated_at \
         FROM publishers WHERE id = $1",
    )
    .bind(source_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::missing("源出版社不存在"))?;

    let target_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM publishers WHERE id = $1)")
            .bind(target_id)
            .fetch_one(&mut *tx)
            .await?;
    if !target_exists {
        return Err(AppError::missing("目标出版社不存在"));
    }

    // 1. 将源出版社的名字作为别名沉淀给目标出版社
    let norm = normalize_publisher_name(&source.name);
    sqlx::query(
        "INSERT INTO publisher_aliases (id, publisher_id, alias_name, normalized_alias) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (normalized_alias) DO UPDATE SET publisher_id = $2",
    )
    .bind(Uuid::new_v4())
    .bind(target_id)
    .bind(&source.name)
    .bind(&norm)
    .execute(&mut *tx)
    .await?;

    // 2. 将源出版社的所有已有别名挂到目标出版社
    sqlx::query("UPDATE publisher_aliases SET publisher_id = $1 WHERE publisher_id = $2")
        .bind(target_id)
        .bind(source_id)
        .execute(&mut *tx)
        .await?;

    // 3. 将所有关联图书版本的 publisher_id 迁移到目标出版社
    sqlx::query("UPDATE editions SET publisher_id = $1 WHERE publisher_id = $2")
        .bind(target_id)
        .bind(source_id)
        .execute(&mut *tx)
        .await?;

    // 4. 删除源出版社
    sqlx::query("DELETE FROM publishers WHERE id = $1")
        .bind(source_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    // 5. 重新计算目标出版社统计
    recalculate_publisher_stats(pool, target_id).await?;

    Ok(())
}

/// 重新计算出版社的物化统计字段。
pub async fn recalculate_publisher_stats(pool: &PgPool, publisher_id: Uuid) -> AppResult<()> {
    sqlx::query(
        "WITH stats AS ( \
             SELECT \
                 count(DISTINCT e.work_id) as works_c, \
                 count(e.id) as editions_c, \
                 count(DISTINCT CASE WHEN lf.verify_status = '有效' THEN h.id END) as holdings_c, \
                 count(DISTINCT CASE WHEN lf.verify_status = '有效' AND h.meets_strategy THEN e.id END) as acquired_c \
             FROM editions e \
             LEFT JOIN holdings h ON h.edition_id = e.id \
             LEFT JOIN library_files lf ON lf.id = h.library_file_id \
             WHERE e.publisher_id = $1 AND e.owned_at IS NOT NULL \
         ) \
         UPDATE publishers SET \
             works_count = stats.works_c, \
             editions_count = stats.editions_c, \
             holdings_count = stats.holdings_c, \
             acquired_count = stats.acquired_c, \
             updated_at = now() \
         FROM stats WHERE publishers.id = $1",
    )
    .bind(publisher_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// 查询某个出版社旗下的所有图书版本（分页）。
pub async fn list_publisher_editions(
    pool: &PgPool,
    publisher_id: Uuid,
    status: Option<&str>,
    limit: i64,
    offset: i64,
) -> AppResult<(Vec<EditionSearchItem>, i64)> {
    let limit = limit.clamp(1, 100);
    let offset = offset.max(0);

    // 无状态筛选是详情页首屏的主要路径，直接复用 publishers 上维护的物化统计，
    // 避免每次翻页都扫描该出版社的全部 editions。带筛选时仍返回精确计数。
    let total: i64 = if status.is_none() {
        sqlx::query_scalar("SELECT editions_count FROM publishers WHERE id = $1")
            .bind(publisher_id)
            .fetch_optional(pool)
            .await?
            .unwrap_or(0)
    } else {
        sqlx::query_scalar(
            "SELECT count(*) FROM editions e \
             LEFT JOIN acquisition_targets at ON at.edition_id = e.id \
             WHERE e.publisher_id = $1 AND e.owned_at IS NOT NULL \
               AND CASE WHEN at.status IS NULL OR at.status = '暂不获取' \
                        THEN '总库已拥有' ELSE at.status END = $2",
        )
        .bind(publisher_id)
        .bind(status)
        .fetch_one(pool)
        .await?
    };

    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        Uuid,
        Uuid,
        String,
        String,
        Option<String>,
        Option<i32>,
        String,
        String,
        String,
        DateTime<Utc>,
        Option<String>,
        Option<String>,
        Option<i32>,
        Option<i32>,
        Option<DateTime<Utc>>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT e.id, e.work_id, w.work_type, e.edition_title, e.publisher, e.publish_year, e.language, \
                CASE WHEN at.status IS NULL OR at.status = '暂不获取' \
                     THEN '总库已拥有' ELSE at.status END as acq_status, w.resolution_status, e.updated_at, \
                wn.name, ae.stage, at.attempts, at.max_attempts, at.next_attempt_at, at.last_error \
         FROM editions e \
         JOIN works w ON w.id = e.work_id \
         LEFT JOIN acquisition_targets at ON at.edition_id = e.id \
         LEFT JOIN worker_nodes wn ON wn.id = at.lease_node_id \
         LEFT JOIN LATERAL (SELECT stage FROM acquisition_executions x WHERE x.target_id = at.id ORDER BY x.started_at DESC LIMIT 1) ae ON TRUE \
         WHERE e.publisher_id = $1 AND e.owned_at IS NOT NULL \
           AND ($2::text IS NULL OR CASE WHEN at.status IS NULL OR at.status = '暂不获取' \
                                         THEN '总库已拥有' ELSE at.status END = $2) \
         ORDER BY e.publish_year DESC NULLS LAST, e.updated_at DESC, e.id DESC \
         LIMIT $3 OFFSET $4",
    )
    .bind(publisher_id)
    .bind(status)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return Ok((Vec::new(), total));
    }

    let edition_ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();

    // 作者与格式
    let all_authors: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT ec.edition_id, c.name FROM edition_contributors ec \
         JOIN contributors c ON c.id = ec.contributor_id \
         WHERE ec.edition_id = ANY($1) ORDER BY ec.edition_id, ec.sort_order",
    )
    .bind(&edition_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let all_identifiers: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT object_id, normalized_value FROM identifiers WHERE object_id = ANY($1) AND is_valid",
    )
    .bind(&edition_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let all_source_formats: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT DISTINCT rr.edition_id, sa.format FROM record_resolutions rr \
         JOIN source_assets sa ON sa.source_record_id = rr.source_record_id WHERE rr.edition_id = ANY($1)",
    )
    .bind(&edition_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let all_holding_formats: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT DISTINCT h.edition_id, lf.format FROM holdings h \
         JOIN library_files lf ON lf.id = h.library_file_id WHERE h.edition_id = ANY($1)",
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

    let items = rows
        .into_iter()
        .map(|r| EditionSearchItem {
            id: r.0,
            work_id: r.1,
            work_type: r.2,
            title: r.3,
            authors: authors_map.remove(&r.0).unwrap_or_default(),
            publisher: r.4,
            publisher_id: Some(publisher_id),
            publish_year: r.5,
            language: r.6,
            identifiers: idents_map.remove(&r.0).unwrap_or_default(),
            source_formats: src_fmt_map.remove(&r.0).unwrap_or_default(),
            holding_formats: hld_fmt_map.remove(&r.0).unwrap_or_default(),
            acquisition_status: r.7,
            worker_name: r.10,
            acquisition_stage: r.11.unwrap_or_default(),
            attempts: r.12.unwrap_or(0),
            max_attempts: r.13.unwrap_or(5),
            next_attempt_at: r.14,
            last_error: r.15,
            resolution_status: r.8,
            updated_at: r.9,
        })
        .collect();

    Ok((items, total))
}

/// 一键从现有图书 editions 中反向抽取并初始化出版社主档与关联。
pub async fn sync_publishers_from_editions(pool: &PgPool) -> AppResult<usize> {
    // 1. 抽取不重复的非空出版社插入 publishers
    let inserted: u64 = sqlx::query(
        "INSERT INTO publishers (name, normalized_name) \
         SELECT DISTINCT trim(publisher) as name, \
                regexp_replace(lower(trim(publisher)), '[^a-z0-9\\u4e00-\\u9fa5]', '', 'g') as norm \
         FROM editions \
         WHERE owned_at IS NOT NULL AND publisher IS NOT NULL AND trim(publisher) != '' \
         ON CONFLICT (normalized_name) DO NOTHING",
    )
    .execute(pool)
    .await?
    .rows_affected();

    // 2. 批量将已有的 editions 关联至 publishers.id
    sqlx::query(
        "UPDATE editions SET publisher_id = p.id \
         FROM publishers p \
         WHERE editions.publisher_id IS NULL \
           AND editions.owned_at IS NOT NULL \
           AND editions.publisher IS NOT NULL \
           AND regexp_replace(lower(trim(editions.publisher)), '[^a-z0-9\\u4e00-\\u9fa5]', '', 'g') = p.normalized_name",
    )
    .execute(pool)
    .await?;

    // 3. 批量更新各出版社的物化统计
    sqlx::query(
        "WITH stats AS ( \
             SELECT \
                 e.publisher_id, \
                 count(DISTINCT e.work_id) as works_c, \
                 count(e.id) as editions_c, \
                 count(DISTINCT CASE WHEN lf.verify_status = '有效' THEN h.id END) as holdings_c, \
                 count(DISTINCT CASE WHEN lf.verify_status = '有效' AND h.meets_strategy THEN e.id END) as acquired_c \
             FROM editions e \
             LEFT JOIN holdings h ON h.edition_id = e.id \
             LEFT JOIN library_files lf ON lf.id = h.library_file_id \
             WHERE e.publisher_id IS NOT NULL AND e.owned_at IS NOT NULL \
             GROUP BY e.publisher_id \
         ) \
         UPDATE publishers SET \
             works_count = stats.works_c, \
             editions_count = stats.editions_c, \
             holdings_count = stats.holdings_c, \
             acquired_count = stats.acquired_c, \
             updated_at = now() \
         FROM stats WHERE publishers.id = stats.publisher_id",
    )
    .execute(pool)
    .await?;

    Ok(inserted as usize)
}
