//! 图书馆总库检索与分面服务（第 7 节、第 10 节）。
//!
//! 提供基于 PostgreSQL 精确查询与 OpenSearch 投影的统一定位接口。

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppResult;
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
    /// 分页大小（默认 20，上限 100）。
    pub limit: Option<i64>,
    /// 分页偏移。
    pub offset: Option<i64>,
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
    /// 状态分面。
    pub status_facets: Vec<FacetCount>,
    /// 语言分面。
    pub language_facets: Vec<FacetCount>,
    /// 格式分面。
    pub format_facets: Vec<FacetCount>,
}

/// 执行总库检索并计算分面。
pub async fn search_catalog(
    pool: &PgPool,
    params: &CatalogSearchParams,
) -> AppResult<CatalogSearchResponse> {
    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let offset = params.offset.unwrap_or(0).max(0);

    let items = search_editions(
        pool,
        params.query.as_deref(),
        params.acquisition_status.as_deref(),
        params.work_type.as_deref(),
        params.language.as_deref(),
        params.format.as_deref(),
        limit,
        offset,
    )
    .await?;

    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM editions")
        .fetch_one(pool)
        .await
        .unwrap_or(0);

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
        offset,
        status_facets,
        language_facets,
        format_facets: Vec::new(),
    })
}

/// 获取单本书目版本的完整详情视图。
pub async fn get_catalog_edition_detail(pool: &PgPool, id: Uuid) -> AppResult<EditionDetail> {
    get_edition_detail(pool, id).await
}
