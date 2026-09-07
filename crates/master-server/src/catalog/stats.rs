//! 统计快照：请求不扫描业务表；失败不覆盖旧结果；跨实例只允许一个刷新者。
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{types::Json, PgExecutor};
use std::time::Duration;

use crate::{
    error::AppResult,
    state::AppState,
    store::catalog_v1::{get_catalog_stats, CatalogStats},
};

const SNAPSHOT_KEY: &str = "catalog_stats_v1";
const TTL_SECONDS: i64 = 300;

/// 最近一次完整计算并成功提交的统计。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogStatsSnapshot {
    /// 精确统计指标。
    pub stats: CatalogStats,
    /// 成功计算时间。
    pub computed_at: DateTime<Utc>,
}

impl CatalogStatsSnapshot {
    fn is_stale(&self) -> bool {
        let age = Utc::now()
            .signed_duration_since(self.computed_at)
            .num_seconds();
        !(0..TTL_SECONDS).contains(&age)
    }
}

/// 保持指标平铺兼容，并附带快照状态。
#[derive(Debug, Serialize)]
pub struct CatalogStatsResponse {
    #[serde(flatten)]
    /// 精确统计指标。
    pub stats: CatalogStats,
    /// false 时统计尚未生成，客户端应展示等待状态，不能显示占位的零值。
    pub ready: bool,
    /// 首份快照尚未完成时为空。
    pub computed_at: Option<DateTime<Utc>>,
    /// 超过刷新周期或时钟回拨时为 true。
    pub stale: bool,
}

async fn load_snapshot(executor: impl PgExecutor<'_>) -> AppResult<Option<CatalogStatsSnapshot>> {
    let row: Option<(Json<CatalogStats>, DateTime<Utc>)> =
        sqlx::query_as("SELECT payload, computed_at FROM query_snapshots WHERE snapshot_key = $1")
            .bind(SNAPSHOT_KEY)
            .fetch_optional(executor)
            .await?;
    Ok(row.map(|(Json(stats), computed_at)| CatalogStatsSnapshot { stats, computed_at }))
}

fn remember(state: &AppState, snapshot: CatalogStatsSnapshot) {
    if let Ok(mut cache) = state.catalog_stats_cache.lock() {
        // 并发读取旧快照完成时，不得覆盖刷新线程已经发布的新结果。
        if cache
            .as_ref()
            .is_none_or(|old| snapshot.computed_at >= old.computed_at)
        {
            *cache = Some(snapshot);
        }
    }
}

/// 仅读取内存/持久化快照，绝不等待重计算。
pub async fn read_stats(state: &AppState) -> AppResult<CatalogStatsResponse> {
    let mut snapshot = state
        .catalog_stats_cache
        .lock()
        .ok()
        .and_then(|cache| cache.clone());
    if snapshot.as_ref().is_none_or(CatalogStatsSnapshot::is_stale) {
        match load_snapshot(&state.pool).await {
            Ok(Some(stored)) => {
                remember(state, stored.clone());
                if snapshot
                    .as_ref()
                    .is_none_or(|old| stored.computed_at >= old.computed_at)
                {
                    snapshot = Some(stored);
                }
            }
            Ok(None) => {}
            Err(error) if snapshot.is_some() => {
                tracing::warn!(%error, "读取统计快照失败，保留内存中的上次成功结果");
            }
            Err(error) => return Err(error),
        }
    }
    Ok(match snapshot {
        Some(snapshot) => CatalogStatsResponse {
            stale: snapshot.is_stale(),
            stats: snapshot.stats,
            ready: true,
            computed_at: Some(snapshot.computed_at),
        },
        None => CatalogStatsResponse {
            stats: CatalogStats::default(),
            ready: false,
            computed_at: None,
            stale: true,
        },
    })
}

/// 启动和周期刷新共用入口；返回 true 表示本次确实计算并发布了新快照。
pub async fn refresh_stats(state: &AppState) -> AppResult<bool> {
    let Ok(_local_guard) = state.catalog_stats_refresh_lock.try_lock() else {
        return Ok(false);
    };
    let mut tx = state.pool.begin().await?;
    // 仅限制此后台事务，不影响下载调度、导入和其他连接。
    sqlx::query("SET LOCAL statement_timeout = '120s'")
        .execute(&mut *tx)
        .await?;
    let acquired: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtextextended(current_schema() || ':' || $1, 0))",
    )
    .bind(SNAPSHOT_KEY)
    .fetch_one(&mut *tx)
    .await?;
    if !acquired {
        tx.rollback().await?;
        return Ok(false);
    }
    if let Some(snapshot) = load_snapshot(&mut *tx).await? {
        let fresh = !snapshot.is_stale();
        remember(state, snapshot);
        if fresh {
            tx.commit().await?;
            return Ok(false);
        }
    }
    let stats = get_catalog_stats(&mut *tx).await?;
    let computed_at = Utc::now();
    let computed_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO query_snapshots (snapshot_key, payload, computed_at) VALUES ($1, $2, $3) \
         ON CONFLICT (snapshot_key) DO UPDATE SET payload = EXCLUDED.payload, computed_at = EXCLUDED.computed_at RETURNING computed_at",
    ).bind(SNAPSHOT_KEY).bind(Json(&stats)).bind(computed_at).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    remember(state, CatalogStatsSnapshot { stats, computed_at });
    Ok(true)
}

/// 启动后台刷新；失败间隔重试，保留上一份快照。
pub fn spawn_refresh_worker(state: AppState) {
    tokio::spawn(async move {
        // 完成后再等待，长查询/失败不会触发补偿式连续重算。
        loop {
            if let Err(error) = refresh_stats(&state).await {
                tracing::warn!(%error, "统计快照刷新失败，30 秒后重试并保留上次成功结果");
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}
