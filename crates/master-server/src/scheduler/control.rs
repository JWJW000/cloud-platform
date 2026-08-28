//! 分布式图书下载的全局调度闸门。
//!
//! 对外只有读取状态、切换状态和事务内检查三个接口；持久化格式、异常值的
//! fail-closed 策略以及与并发领取的锁协调都隐藏在本模块内。

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::{AppError, AppResult};

const SETTING_KEY: &str = "global_download_paused";

/// 管理后台展示的全局下载状态。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GlobalDownloadControl {
    /// 是否停止派发新的图书下载任务。
    pub paused: bool,
    /// 最近一次切换时间。
    pub updated_at: DateTime<Utc>,
}

/// 读取持久化的全局下载状态。
pub async fn get_global_download_control(pool: &PgPool) -> AppResult<GlobalDownloadControl> {
    read_state(pool)
        .await?
        .ok_or_else(|| AppError::internal("缺少全局下载控制设置，请先执行数据库迁移"))
}

/// 原子切换全局下载状态。
///
/// 先对固定设置行加排他锁；正在领取任务的事务持有共享锁时，本操作会等待其完成。
/// 本操作提交后，后续领取事务只能观察到新状态。
pub async fn set_global_download_paused(
    pool: &PgPool,
    paused: bool,
) -> AppResult<GlobalDownloadControl> {
    let mut tx = pool.begin().await?;
    let exists: Option<String> =
        sqlx::query_scalar("SELECT key FROM settings WHERE key = $1 FOR UPDATE")
            .bind(SETTING_KEY)
            .fetch_optional(&mut *tx)
            .await?;
    if exists.is_none() {
        return Err(AppError::internal(
            "缺少全局下载控制设置，请先执行数据库迁移",
        ));
    }

    let (paused, updated_at): (bool, DateTime<Utc>) = sqlx::query_as(
        "UPDATE settings SET value = to_jsonb($2::boolean), updated_at = now() \
         WHERE key = $1 RETURNING (value = 'true'::jsonb), updated_at",
    )
    .bind(SETTING_KEY)
    .bind(paused)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(GlobalDownloadControl { paused, updated_at })
}

/// 在任务领取事务内检查调度闸门，并持有共享锁直至领取提交或回滚。
///
/// 非布尔设置视为暂停（fail closed），避免人工误改 JSON 后意外恢复大规模派发。
pub async fn global_download_is_paused(tx: &mut Transaction<'_, Postgres>) -> AppResult<bool> {
    let paused: Option<bool> = sqlx::query_scalar(
        "SELECT CASE WHEN jsonb_typeof(value) = 'boolean' \
                     THEN value = 'true'::jsonb ELSE TRUE END \
         FROM settings WHERE key = $1 FOR SHARE",
    )
    .bind(SETTING_KEY)
    .fetch_optional(&mut **tx)
    .await?;
    paused.ok_or_else(|| AppError::internal("缺少全局下载控制设置，请先执行数据库迁移"))
}

async fn read_state(pool: &PgPool) -> AppResult<Option<GlobalDownloadControl>> {
    let row: Option<(bool, DateTime<Utc>)> = sqlx::query_as(
        "SELECT CASE WHEN jsonb_typeof(value) = 'boolean' \
                     THEN value = 'true'::jsonb ELSE TRUE END, updated_at \
         FROM settings WHERE key = $1",
    )
    .bind(SETTING_KEY)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(paused, updated_at)| GlobalDownloadControl { paused, updated_at }))
}
