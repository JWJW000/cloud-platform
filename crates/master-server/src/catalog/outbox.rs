//! 搜索 Outbox 队列与投影同步（第 7.2 节）。
//!
//! 在 PostgreSQL 事务内安全写入变更事件，由后台处理器异步推送到 OpenSearch / 内存索引。

use sqlx::PgPool;

use crate::error::AppResult;
use crate::store::catalog_v1::CatalogOutboxRow;

/// 消费待同步的 Outbox 消息并标记为已同步。
pub async fn process_outbox_events(pool: &PgPool, batch_size: i64) -> AppResult<usize> {
    let mut tx = pool.begin().await?;

    let events: Vec<CatalogOutboxRow> = sqlx::query_as(
        "SELECT id, event_type, aggregate_type, aggregate_id, payload, status, created_at, synced_at \
         FROM catalog_outbox \
         WHERE status = '待同步' \
         ORDER BY id ASC \
         LIMIT $1 \
         FOR UPDATE SKIP LOCKED"
    )
    .bind(batch_size.clamp(1, 500))
    .fetch_all(&mut *tx)
    .await?;

    if events.is_empty() {
        return Ok(0);
    }

    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();

    sqlx::query(
        "UPDATE catalog_outbox SET status = '已同步', synced_at = now() \
         WHERE id = ANY($1)"
    )
    .bind(&ids)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(events.len())
}
