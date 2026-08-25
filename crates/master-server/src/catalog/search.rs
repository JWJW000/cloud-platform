//! 图书馆总库检索与分面服务（第 7 节、第 10 节）。
//!
//! 提供基于 PostgreSQL 精确查询与 OpenSearch 投影的统一定位接口。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::store::catalog_v1::{
    get_edition_detail, search_editions, EditionDetail, EditionSearchItem,
};

/// 检索查询参数。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CatalogSearchParams {
    /// 关键词（书名/作者/出版社/ISBN/DOI/来源编号）。
    pub query: Option<String>,
    /// 获取状态过滤。
    pub acquisition_status: Option<String>,
    /// 作品类型过滤（整书/章节/论文等）。
    pub work_type: Option<String>,
    /// 语言过滤（zh/en/ot 等）。
    pub language: Option<String>,
    /// 格式过滤（pdf/epub/azw3/mobi 等）。
    pub format: Option<String>,
    /// 作品消歧状态过滤（数据质量页使用）。
    pub resolution_status: Option<String>,
    /// 分页大小（默认 20，上限 100）。
    pub limit: Option<i64>,
    /// 分页偏移。
    #[deprecated(note = "use cursor")]
    pub offset: Option<i64>,
    /// 不透明的键集分页游标。
    pub cursor: Option<String>,
}

/// 分面统计项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FacetCount {
    /// 分面取值。
    pub key: String,
    /// 数量。
    pub count: i64,
}

/// 检索响应包。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogSearchResponse {
    /// 匹配项列表。
    pub items: Vec<EditionSearchItem>,
    /// 总匹配数估计。
    pub total: i64,
    /// 分页大小。
    pub limit: i64,
    /// 当前偏移。
    pub offset: i64,
    /// 下一页游标；为空表示已到末尾。
    pub next_cursor: Option<String>,
    /// 上一页游标；为空表示当前为第一页。
    pub previous_cursor: Option<String>,
    /// 状态分面。
    pub status_facets: Vec<FacetCount>,
    /// 语言分面。
    pub language_facets: Vec<FacetCount>,
    /// 格式分面。
    pub format_facets: Vec<FacetCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SearchCursor {
    updated_at: DateTime<Utc>,
    id: Uuid,
    direction: CursorDirection,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CursorDirection {
    Next,
    Previous,
}

fn decode_cursor(value: Option<&str>) -> AppResult<Option<SearchCursor>> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > 512 {
        return Err(AppError::bad("分页游标无效"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AppError::bad("分页游标无效"))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| AppError::bad("分页游标无效"))
}

fn encode_cursor(item: &EditionSearchItem, direction: CursorDirection) -> Option<String> {
    serde_json::to_vec(&SearchCursor {
        updated_at: item.updated_at,
        id: item.id,
        direction,
    })
    .ok()
    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
}

/// 执行总库检索并计算分面。
pub async fn search_catalog(
    pool: &PgPool,
    params: &CatalogSearchParams,
) -> AppResult<CatalogSearchResponse> {
    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let cursor = decode_cursor(params.cursor.as_deref())?;
    let forward = cursor
        .as_ref()
        .is_none_or(|cursor| cursor.direction == CursorDirection::Next);

    let (items, has_more) = search_editions(
        pool,
        params.query.as_deref(),
        params.acquisition_status.as_deref(),
        params.work_type.as_deref(),
        params.language.as_deref(),
        params.format.as_deref(),
        params.resolution_status.as_deref(),
        limit,
        cursor.as_ref().map(|cursor| cursor.updated_at),
        cursor.as_ref().map(|cursor| cursor.id),
        forward,
    )
    .await?;

    let next_cursor = items.last().and_then(|item| {
        if !forward || has_more {
            encode_cursor(item, CursorDirection::Next)
        } else {
            None
        }
    });
    let previous_cursor = items.first().and_then(|item| {
        let has_previous = if forward { cursor.is_some() } else { has_more };
        has_previous
            .then(|| encode_cursor(item, CursorDirection::Previous))
            .flatten()
    });

    // 无筛选时使用 PostgreSQL 统计估值，避免大表每次翻页都执行全表 COUNT；
    // 有筛选时返回与列表条件一致的精确数量。
    let total: i64 = if params.query.is_none()
        && params.acquisition_status.is_none()
        && params.work_type.is_none()
        && params.language.is_none()
        && params.format.is_none()
        && params.resolution_status.is_none()
    {
        sqlx::query_scalar(
            "SELECT greatest(coalesce(reltuples, 0)::bigint, 0) FROM pg_class WHERE oid = 'editions'::regclass",
        )
        .fetch_one(pool)
        .await
        .unwrap_or(0)
    } else {
        let keyword = params
            .query
            .as_deref()
            .map(|value| format!("%{}%", value.trim().to_lowercase()));
        let format = params
            .format
            .as_deref()
            .map(|value| format!("%{}%", value.trim().to_lowercase()));
        sqlx::query_scalar(
            "SELECT count(*)::bigint \
             FROM editions e \
             JOIN works w ON w.id = e.work_id \
             LEFT JOIN acquisition_targets at ON at.edition_id = e.id \
             WHERE ($1::text IS NULL OR e.edition_title ILIKE $1 OR w.preferred_title ILIKE $1 OR e.publisher ILIKE $1 \
                    OR EXISTS (SELECT 1 FROM identifiers i WHERE i.object_id = e.id AND i.raw_value ILIKE $1) \
                    OR EXISTS (SELECT 1 FROM edition_contributors ec JOIN contributors c ON c.id = ec.contributor_id WHERE ec.edition_id = e.id AND c.name ILIKE $1)) \
               AND ($2::text IS NULL \
                    OR ($2 = '__actionable__' AND coalesce(at.status, '待下载') NOT IN ('已下载', '已完成', '已取消')) \
                    OR coalesce(at.status, '待下载') = $2) \
               AND ($3::text IS NULL OR w.work_type = $3) \
               AND ($4::text IS NULL OR e.language = $4) \
               AND ($5::text IS NULL OR e.format_summary ILIKE $5) \
               AND ($6::text IS NULL OR w.resolution_status = $6)",
        )
        .bind(keyword)
        .bind(params.acquisition_status.as_deref())
        .bind(params.work_type.as_deref())
        .bind(params.language.as_deref())
        .bind(format)
        .bind(params.resolution_status.as_deref())
        .fetch_one(pool)
        .await
        .unwrap_or(0)
    };

    let status_facets_raw: Vec<(String, i64)> = sqlx::query_as(
        "SELECT coalesce(at.status, '待下载') as k, count(*)::bigint FROM editions e \
         LEFT JOIN acquisition_targets at ON at.edition_id = e.id \
         GROUP BY coalesce(at.status, '待下载') ORDER BY count(*) DESC LIMIT 10",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let language_facets_raw: Vec<(String, i64)> = sqlx::query_as(
        "SELECT language as k, count(*)::bigint FROM editions GROUP BY language ORDER BY count(*) DESC LIMIT 10"
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let status_facets = status_facets_raw
        .into_iter()
        .map(|(key, count)| FacetCount { key, count })
        .collect();
    let language_facets = language_facets_raw
        .into_iter()
        .map(|(key, count)| FacetCount { key, count })
        .collect();

    Ok(CatalogSearchResponse {
        items,
        total,
        limit,
        offset: 0,
        next_cursor,
        previous_cursor,
        status_facets,
        language_facets,
        format_facets: Vec::new(),
    })
}

/// 获取单本书目版本的完整详情视图。
pub async fn get_catalog_edition_detail(pool: &PgPool, id: Uuid) -> AppResult<EditionDetail> {
    get_edition_detail(pool, id).await
}
