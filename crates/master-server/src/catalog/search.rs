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
use crate::opensearch::OpenSearchClient;
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
    /// 出版社过滤。
    pub publisher: Option<String>,
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
    /// 精确总数或已知下界；由 total_is_exact 区分。
    pub total: i64,
    /// PostgreSQL 分页回退不执行全表计数。
    pub total_is_exact: bool,
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
    /// 出版社分面。
    pub publisher_facets: Vec<FacetCount>,
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
    search_catalog_with_opensearch(pool, None, params).await
}

/// 执行总库检索；存在非空关键词且 OpenSearch 可用时优先查询搜索投影。
/// OpenSearch 超时或故障时自动回退 PostgreSQL，确保搜索集群不成为业务单点。
pub async fn search_catalog_with_opensearch(
    pool: &PgPool,
    opensearch: Option<&OpenSearchClient>,
    params: &CatalogSearchParams,
) -> AppResult<CatalogSearchResponse> {
    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let cursor = decode_cursor(params.cursor.as_deref())?;
    let forward = cursor
        .as_ref()
        .is_none_or(|cursor| cursor.direction == CursorDirection::Next);

    let keyword = params
        .query
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if keyword.is_some_and(|value| value.chars().count() > 200) {
        return Err(AppError::bad("检索关键词不能超过 200 个字符"));
    }

    if let (Some(client), Some(keyword)) = (opensearch, keyword) {
        match client
            .search(
                keyword,
                params.acquisition_status.as_deref(),
                params.work_type.as_deref(),
                params.language.as_deref(),
                params.format.as_deref(),
                params.resolution_status.as_deref(),
                params.publisher.as_deref(),
                limit,
                cursor.as_ref().map(|cursor| cursor.updated_at),
                cursor.as_ref().map(|cursor| cursor.id),
                forward,
            )
            .await
        {
            Ok(page) => {
                let next_cursor = page.items.last().and_then(|item| {
                    if !forward || page.has_more {
                        encode_cursor(item, CursorDirection::Next)
                    } else {
                        None
                    }
                });
                let previous_cursor = page.items.first().and_then(|item| {
                    let has_previous = if forward {
                        cursor.is_some()
                    } else {
                        page.has_more
                    };
                    has_previous
                        .then(|| encode_cursor(item, CursorDirection::Previous))
                        .flatten()
                });
                return Ok(CatalogSearchResponse {
                    items: page.items,
                    total: page.total,
                    total_is_exact: true,
                    limit,
                    offset: 0,
                    next_cursor,
                    previous_cursor,
                    status_facets: page
                        .status_facets
                        .into_iter()
                        .map(|(key, count)| FacetCount { key, count })
                        .collect(),
                    language_facets: page
                        .language_facets
                        .into_iter()
                        .map(|(key, count)| FacetCount { key, count })
                        .collect(),
                    format_facets: page
                        .format_facets
                        .into_iter()
                        .map(|(key, count)| FacetCount { key, count })
                        .collect(),
                    publisher_facets: page
                        .publisher_facets
                        .into_iter()
                        .map(|(key, count)| FacetCount { key, count })
                        .collect(),
                });
            }
            Err(error) => {
                tracing::warn!(error = %error, "OpenSearch 检索失败，回退 PostgreSQL");
            }
        }
    }

    let (items, has_more) = search_editions(
        pool,
        keyword,
        params.acquisition_status.as_deref(),
        params.work_type.as_deref(),
        params.language.as_deref(),
        params.format.as_deref(),
        params.resolution_status.as_deref(),
        params.publisher.as_deref(),
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

    // 回退路径只报告能证明的结果数量，不为展示总数扫描全库或虚构分面。
    let total = items.len() as i64 + i64::from(has_more);
    let total_is_exact = cursor.is_none() && !has_more;

    Ok(CatalogSearchResponse {
        items,
        total,
        total_is_exact,
        limit,
        offset: 0,
        next_cursor,
        previous_cursor,
        status_facets: Vec::new(),
        language_facets: Vec::new(),
        format_facets: Vec::new(),
        publisher_facets: Vec::new(),
    })
}

/// 获取单本书目版本的完整详情视图。
pub async fn get_catalog_edition_detail(pool: &PgPool, id: Uuid) -> AppResult<EditionDetail> {
    get_edition_detail(pool, id).await
}
