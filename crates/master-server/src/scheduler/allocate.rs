//! 会话分配：原子地凑出「槽位 + 账号 + 代理」（第 6.4、7.1、10.1 节）。
//!
//! 一个执行会话是「一个浏览器实例 + 一个站点账号 + 一条固定代理」的绑定。
//! 三者必须在同一个事务里一起拿到：只拿到账号却没有代理时如果先提交，
//! 这个账号就会被一个永远不会出现的会话挂住，直到租约回收才被放出来。
//! 因此这里的写法是「全部拿齐才提交，任何一步空手就整体回滚」。
//!
//! 代理与账号的绑定在会话存续期间**不换**（第 10.1 节）：同一账号频繁换出口 IP
//! 会被站点判定为异常登录，这比偶尔浪费一条代理的代价大得多。

use chrono::{DateTime, Utc};
use platform_domain::{
    classify_failure, AccountStatus, ProxyStatus, SessionStatus, SlotStatus, TaskType, WorkerStatus,
};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::store;

/// 会话内下发给 Worker 的账号凭据（明文）。
///
/// **不实现 `Serialize`**：明文密码只应出现在「下发给某个已认证节点」的 gRPC 负载里，
/// 类型上禁止序列化可以保证它永远进不了管理后台的 JSON 响应或日志。
#[derive(Debug, Clone)]
pub struct AccountCredential {
    /// 账号编号。
    pub account_id: Uuid,
    /// 登录邮箱。
    pub email: String,
    /// 明文密码。
    pub password: String,
    /// 昵称（注册时生成的展示名）。
    pub nickname: String,
    /// 今日已用额度。
    pub daily_used: i32,
    /// 今日额度上限。
    pub daily_limit: i32,
}

/// 会话内下发给 Worker 的代理凭据（明文）。同样刻意不实现 `Serialize`。
#[derive(Debug, Clone)]
pub struct ProxyCredential {
    /// 代理编号。
    pub proxy_id: Uuid,
    /// 展示名。
    pub label: String,
    /// 协议（技术标识 `http`/`https`/`socks5`）。
    pub scheme: String,
    /// 主机。
    pub host: String,
    /// 端口。
    pub port: i32,
    /// 用户名。
    pub username: Option<String>,
    /// 明文密码。
    pub password: Option<String>,
}

/// 登录恢复所需的任务级邮件 Provider 凭据。
///
/// 不实现 `Serialize`，Debug 也必须脱敏：API Key 只能进入已认证 Worker 的
/// mTLS 下行消息，不得进入日志或管理端 JSON。
#[derive(Clone)]
pub struct MailProviderCredential {
    /// 配置版本。
    pub version: i64,
    /// Provider 类型，下载恢复目前只下发 `outlook_http`。
    pub provider_type: String,
    /// OutlookMail 服务端点。
    pub endpoint: String,
    /// 仅本次会话可见的 API Key 明文。
    pub api_key: String,
    /// 轮询间隔秒数。
    pub poll_interval_secs: i32,
    /// 最长取码时间秒数。
    pub timeout_secs: i32,
    /// SSRF 防护允许的主机。
    pub allowed_hosts: Vec<String>,
    /// 允许的验证码邮件发件人。
    pub allowed_senders: Vec<String>,
}

impl std::fmt::Debug for MailProviderCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailProviderCredential")
            .field("version", &self.version)
            .field("provider_type", &self.provider_type)
            .field("endpoint", &self.endpoint)
            .field("api_key", &"[REDACTED]")
            .field("poll_interval_secs", &self.poll_interval_secs)
            .field("timeout_secs", &self.timeout_secs)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("allowed_senders", &self.allowed_senders)
            .finish()
    }
}

/// 分配成功后交给 gRPC 层组装 `CreateSession` 的全部要素。
#[derive(Debug, Clone)]
pub struct SessionGrant {
    /// 会话编号。
    pub session_id: Uuid,
    /// 节点编号。
    pub node_id: Uuid,
    /// 槽位序号。
    pub slot_index: i32,
    /// 任务类型。
    pub task_type: TaskType,
    /// 账号凭据（NAS 核验、代理检测没有账号）。
    pub account: Option<AccountCredential>,
    /// 代理凭据。
    pub proxy: Option<ProxyCredential>,
    /// 本机固定转发端口。
    pub local_forward_port: Option<i32>,
    /// 本会话最多下载多少本。
    pub max_downloads: i32,
    /// 会话硬性时长上限（秒）。
    pub max_duration_secs: i64,
    /// 会话租约到期时间，Worker 必须在此之前续租。
    pub lease_expires_at: DateTime<Utc>,
    /// 下载登录失败时的 OutlookMail 恢复凭据。
    pub mail_provider: Option<MailProviderCredential>,
}

/// 无法分配的原因，直接对应 `NoTaskAvailable`。
#[derive(Debug, Clone)]
pub struct Unavailable {
    /// 中文原因，可直接展示。
    pub reason: String,
    /// 建议多久后重试（秒）。
    pub retry_after_secs: u32,
}

/// 分配结果。
#[derive(Debug, Clone)]
pub enum AllocationOutcome {
    /// 分配成功。装箱是因为 `SessionGrant` 明显比另一个分支大。
    Granted(Box<SessionGrant>),
    /// 资源不足，附带中文原因。
    Unavailable(Unavailable),
}

/// 各任务类型对账号的要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountNeed {
    /// 需要一个可用的已注册账号（图书下载）。
    Usable,
    /// 需要一个待注册账号（账号注册任务本身）。
    ToRegister,
    /// 不需要账号（NAS 核验、代理检测）。
    None,
}

/// 各任务类型对代理的要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyNeed {
    /// 需要一条健康代理。
    Healthy,
    /// 需要一条待检测代理（可用或异常都可以，正是要重新判定它）。
    ForCheck,
    /// 不需要代理。
    None,
}

fn needs_of(task_type: TaskType) -> (AccountNeed, ProxyNeed) {
    match task_type {
        TaskType::BookDownload => (AccountNeed::Usable, ProxyNeed::Healthy),
        TaskType::AccountRegister => (AccountNeed::ToRegister, ProxyNeed::Healthy),
        TaskType::NasVerify => (AccountNeed::None, ProxyNeed::None),
        TaskType::ProxyCheck => (AccountNeed::None, ProxyNeed::ForCheck),
    }
}

/// `CreateSession.mail_provider` 从 Worker v0.3.24 起才会被消费。
/// Master 可能先于 Worker 滚动升级，因此不能把历史失败账号发给旧 Worker。
fn worker_supports_login_recovery(agent_version: &str) -> bool {
    let mut parts = agent_version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse::<u64>());
    let version = match (parts.next(), parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch))) => (major, minor, patch),
        _ => return false,
    };
    version >= (0, 3, 24)
}

/// 本机转发端口：每个槽位一个固定端口（第 10.1 节）。
///
/// 固定而不是随机：排障时「19003 端口对应 3 号槽位」这个对应关系可以直接靠肉眼判断，
/// 而随机端口每次重启都变，抓包和防火墙规则都没法沉淀。
pub const FORWARD_PORT_BASE: i32 = 19001;

/// 槽位对应的本机转发端口。
pub fn forward_port(slot_index: i32) -> i32 {
    FORWARD_PORT_BASE + slot_index.max(0)
}

/// 为下载会话构造一份只存在于内存的 OutlookMail 快照。
async fn load_login_recovery_provider(
    state: &AppState,
) -> AppResult<Option<MailProviderCredential>> {
    let Some(record) = store::mail_provider::get_active_config(&state.pool).await? else {
        return Ok(None);
    };
    if record.provider_type != "outlook_http" {
        return Ok(None);
    }
    if !state.config.security.require_client_cert {
        tracing::warn!(
            provider_version = record.version,
            "当前未强制 Worker 客户端证书，不下发登录恢复所需 Outlook 密钥"
        );
        return Ok(None);
    }

    let Some(secret_ref) = record.api_key_secret_ref.as_deref() else {
        return Ok(None);
    };
    let Some(cipher_text) =
        store::mail_provider::get_secret_ciphertext(&state.pool, secret_ref).await?
    else {
        return Ok(None);
    };
    let api_key = state
        .cipher
        .decrypt(&cipher_text)
        .map_err(AppError::Internal)?;
    if api_key.trim().is_empty() {
        return Ok(None);
    }

    Ok(Some(MailProviderCredential {
        version: record.version,
        provider_type: record.provider_type,
        endpoint: record.endpoint,
        api_key,
        poll_interval_secs: record.poll_interval_secs,
        timeout_secs: record.timeout_secs,
        allowed_hosts: record.allowed_hosts,
        allowed_senders: record.allowed_senders,
    }))
}

/// 为节点分配一个执行会话。
///
/// `preferred_slot` 为 `Some` 时只考虑该槽位（Worker 明确说「我 2 号槽空了」），
/// 为 `None` 时取序号最小的空闲槽位。
pub async fn allocate_session(
    state: &AppState,
    node_id: Uuid,
    task_type: TaskType,
    preferred_slot: Option<i32>,
) -> AppResult<AllocationOutcome> {
    let scheduler = state.scheduler();
    let lease_secs = scheduler.task_lease_secs as i64;

    // 跨日额度重置在这里顺手做一次：把「今天还能不能用」这个判断和它依赖的重置
    // 放在同一条调用路径上，就不会出现「定时任务漏跑导致整天分配不出账号」。
    store::resource::reset_expired_quota(&state.pool).await?;

    let node = store::node::get_node(&state.pool, node_id).await?;
    if !node.status.parse::<WorkerStatus>()?.can_accept_work() {
        return Ok(unavailable(
            format!("节点当前状态为{}，不分配会话", node.status),
            30,
        ));
    }
    if !node.nas_healthy && task_type == TaskType::BookDownload {
        return Ok(unavailable(
            "节点 NAS 不可写，暂不分配下载会话".to_string(),
            60,
        ));
    }

    // 在领取账号前就固定本次 Provider 快照：只有新 Worker 且 Outlook 配置可用时，
    // 才允许领取历史“登录失败”账号，避免滚动升级期间被旧 Worker 消耗恢复机会。
    let login_recovery_provider = if task_type == TaskType::BookDownload
        && worker_supports_login_recovery(&node.agent_version)
    {
        load_login_recovery_provider(state).await?
    } else {
        None
    };
    let can_recover_failed_account = login_recovery_provider.is_some();

    let (account_need, proxy_need) = needs_of(task_type);
    let mut tx = state.pool.begin().await?;

    // 选择队列与真正占用槽位之间可能恰好发生全局暂停；在资源领取事务内再次
    // 检查并持有共享锁，保证暂停成功返回后不会新建下载会话或占住账号/代理。
    if task_type == TaskType::BookDownload
        && super::control::global_download_is_paused(&mut tx).await?
    {
        tx.rollback().await?;
        return Ok(unavailable("全局图书下载已暂停".to_string(), 20));
    }

    let Some(slot_index) = claim_slot(&mut tx, node_id, preferred_slot).await? else {
        tx.rollback().await?;
        return Ok(unavailable("没有空闲槽位".to_string(), 15));
    };

    let account = match account_need {
        AccountNeed::None => None,
        need => match claim_account(&mut tx, need, can_recover_failed_account).await? {
            Some(row) => Some(row),
            None => {
                tx.rollback().await?;
                let reason = match need {
                    AccountNeed::ToRegister => "没有待注册账号",
                    _ => "没有可用账号（可能全部占用或今日额度已用尽）",
                };
                return Ok(unavailable(reason.to_string(), 60));
            }
        },
    };

    let proxy = match proxy_need {
        ProxyNeed::None => None,
        need => match claim_proxy(&mut tx, need).await? {
            Some(row) => Some(row),
            None => {
                // 账号已经在本事务里被占上了，这里回滚才不会留下「被占住却无会话」的账号
                tx.rollback().await?;
                return Ok(unavailable("没有可用代理".to_string(), 60));
            }
        },
    };

    let local_forward_port = proxy.as_ref().map(|_| forward_port(slot_index));
    let session = store::session::create_session(
        &mut tx,
        &store::session::NewSession {
            node_id,
            slot_index,
            account_id: account.as_ref().map(|row| row.id),
            proxy_id: proxy.as_ref().map(|row| row.id),
            task_type,
            local_forward_port,
            lease_secs,
        },
    )
    .await?;

    if let Some(row) = &account {
        // 历史“登录失败”账号允许进入一次新的邮箱恢复流程。
        // 这里只记录恢复尝试，不能提前改回“已注册”：Worker 完成登录并上报
        // SessionReady 之前，账号都必须保持隔离，避免暂时性页面错误释放租约后
        // 又被下一槽位立即领取。
        if row.status == AccountStatus::LoginFailed.as_str() {
            sqlx::query(
                "UPDATE accounts SET login_recovery_attempted_at = now(), updated_at = now() \
                     WHERE id = $1",
            )
            .bind(row.id)
            .execute(&mut *tx)
            .await?;
        }
        lease_account(&mut tx, row.id, session.id, session.lease_expires_at).await?;
    }
    if let Some(row) = &proxy {
        lease_proxy(&mut tx, row.id, session.id, session.lease_expires_at).await?;
    }

    store::node::set_slot(
        &mut *tx,
        node_id,
        slot_index,
        SlotStatus::Reserved,
        Some(session.id),
        "已分配账号与代理",
    )
    .await?;
    store::node::refresh_available_slots(&mut *tx, node_id).await?;
    tx.commit().await?;

    // 解密放在事务之外：密钥不在数据库里，解密失败属于配置问题而不是数据问题，
    // 让它不再牵连事务边界，回收逻辑会在租约到期后自动把资源放回去。
    let max_downloads = account
        .as_ref()
        .map(|row| {
            (row.daily_limit - row.daily_used)
                .max(0)
                .min(scheduler.session_max_downloads.max(1))
        })
        .unwrap_or(1);
    let mail_provider = account.as_ref().and(login_recovery_provider);

    let grant = SessionGrant {
        session_id: session.id,
        node_id,
        slot_index,
        task_type,
        account: match account {
            Some(row) => Some(AccountCredential {
                account_id: row.id,
                email: row.email,
                password: state.cipher.decrypt(&row.password_cipher)?,
                nickname: row.nickname,
                daily_used: row.daily_used,
                daily_limit: row.daily_limit,
            }),
            None => None,
        },
        proxy: match proxy {
            Some(row) => Some(ProxyCredential {
                proxy_id: row.id,
                label: row.label,
                scheme: row.scheme,
                host: row.host,
                port: row.port,
                username: row.username,
                password: match row.password_cipher {
                    Some(cipher) => Some(state.cipher.decrypt(&cipher)?),
                    None => None,
                },
            }),
            None => None,
        },
        local_forward_port,
        max_downloads,
        // 硬性时长上限：最坏情况下把额定本数按满租约跑完还有余量，
        // 超过它说明 Worker 已经不正常，宁可结束会话换一个干净的浏览器。
        max_duration_secs: lease_secs * (max_downloads as i64 + 1),
        lease_expires_at: session.lease_expires_at,
        mail_provider,
    };

    state.events.publish(
        "会话变更",
        serde_json::json!({ "会话": session.id, "节点": node_id, "槽位": slot_index }),
    );
    Ok(AllocationOutcome::Granted(Box::new(grant)))
}

fn unavailable(reason: String, retry_after_secs: u32) -> AllocationOutcome {
    AllocationOutcome::Unavailable(Unavailable {
        reason,
        retry_after_secs,
    })
}

/// 锁一个空闲槽位。
///
/// `FOR UPDATE SKIP LOCKED`：同一节点的多个槽位可能同时来申请会话，
/// 跳过被别人锁住的行比排队等锁更合适——申请者拿不到就退回「没有空闲槽位」，
/// 而不是把 gRPC 流卡在数据库锁上。
async fn claim_slot(
    tx: &mut sqlx::PgConnection,
    node_id: Uuid,
    preferred_slot: Option<i32>,
) -> AppResult<Option<i32>> {
    // 只有当前槽位为空闲、且没有未结束的执行会话时才允许分配新会话，防止重复分配冲掉还在启动的浏览器
    let slot: Option<i32> = sqlx::query_scalar(
        "SELECT ws.slot_index FROM worker_slots ws \
         WHERE ws.node_id = $1 AND ws.status = $2 AND ws.session_id IS NULL \
           AND ($3::int IS NULL OR ws.slot_index = $3) \
           AND NOT EXISTS ( \
               SELECT 1 FROM execution_sessions es \
               WHERE es.node_id = ws.node_id AND es.slot_index = ws.slot_index \
                 AND es.ended_at IS NULL AND es.status IN ('创建中', '运行中') \
           ) \
         ORDER BY ws.slot_index \
         FOR UPDATE SKIP LOCKED LIMIT 1",
    )
    .bind(node_id)
    .bind(SlotStatus::Idle.as_str())
    .bind(preferred_slot)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(slot)
}

/// 事务内取到的账号行，带密文。
#[derive(Debug, Clone, sqlx::FromRow)]
struct ClaimedAccount {
    id: Uuid,
    email: String,
    nickname: String,
    password_cipher: String,
    status: String,
    daily_used: i32,
    daily_limit: i32,
}

/// 锁一个账号。
///
/// 排序用 `daily_used ASC`：优先挑额度最富余的账号，会话的 `max_downloads`
/// 就能开到最大，从而减少「跑两本就得换账号重开浏览器」的开销。
async fn claim_account(
    tx: &mut sqlx::PgConnection,
    need: AccountNeed,
    can_recover_failed: bool,
) -> AppResult<Option<ClaimedAccount>> {
    let row = match need {
        AccountNeed::ToRegister => {
            // 注册会话必须从“当前确实存在可领取任务”的账号中选择。只按账号状态挑选
            // 会选中仍处于退避期、已取消或批次未运行的账号，浏览器启动后 Master
            // 找不到对应任务，Worker 只能等待 30 秒后关闭。
            sqlx::query_as::<_, ClaimedAccount>(
                "SELECT a.id, a.email, a.nickname, a.password_cipher, a.status, a.daily_used, a.daily_limit \
                 FROM accounts a \
                 JOIN account_registration_tasks t ON t.account_id = a.id \
                 JOIN account_registration_batches b ON b.id = t.batch_id \
                 WHERE a.status = $1 AND a.lease_session_id IS NULL \
                   AND t.status IN ('待处理', '正在重试') \
                   AND t.next_attempt_at <= now() \
                   AND t.cancel_requested = FALSE AND t.attempts < t.max_attempts \
                   AND b.status = '执行中' \
                 ORDER BY b.priority DESC, t.priority DESC, t.created_at, a.created_at \
                 FOR UPDATE OF a SKIP LOCKED LIMIT 1",
            )
            .bind(AccountStatus::PendingRegistration.as_str())
            .fetch_optional(&mut *tx)
            .await?
        }
        AccountNeed::Usable if can_recover_failed => {
            sqlx::query_as::<_, ClaimedAccount>(
                "SELECT id, email, nickname, password_cipher, status, daily_used, daily_limit FROM accounts \
                 WHERE status IN ($1, $2) AND lease_session_id IS NULL AND daily_used < daily_limit \
                   AND (status = $1 OR login_recovery_attempted_at IS NULL) \
                 ORDER BY CASE WHEN status = $2 THEN 0 ELSE 1 END, daily_used, updated_at, created_at \
                 FOR UPDATE SKIP LOCKED LIMIT 1",
            )
            .bind(AccountStatus::Registered.as_str())
            .bind(AccountStatus::LoginFailed.as_str())
            .fetch_optional(&mut *tx)
            .await?
        }
        AccountNeed::Usable => {
            sqlx::query_as::<_, ClaimedAccount>(
                "SELECT id, email, nickname, password_cipher, status, daily_used, daily_limit FROM accounts \
                 WHERE status = $1 AND lease_session_id IS NULL AND daily_used < daily_limit \
                 ORDER BY daily_used, updated_at, created_at \
                 FOR UPDATE SKIP LOCKED LIMIT 1",
            )
            .bind(AccountStatus::Registered.as_str())
            .fetch_optional(&mut *tx)
            .await?
        }
        AccountNeed::None => None,
    };
    Ok(row)
}

/// 事务内取到的代理行，带密文。
#[derive(Debug, Clone, sqlx::FromRow)]
struct ClaimedProxy {
    id: Uuid,
    label: String,
    scheme: String,
    host: String,
    port: i32,
    username: Option<String>,
    password_cipher: Option<String>,
}

/// 锁一条代理。
///
/// 健康代理按 `failure_count, latency_ms` 排序：失败少、延迟低的先用，
/// 让偶发抖动过的代理自然沉到队尾，而不需要额外维护一个评分字段。
async fn claim_proxy(
    tx: &mut sqlx::PgConnection,
    need: ProxyNeed,
) -> AppResult<Option<ClaimedProxy>> {
    let sql = match need {
        ProxyNeed::ForCheck => {
            "SELECT id, label, scheme, host, port, username, password_cipher FROM proxies \
             WHERE status IN ($1, $2) AND lease_session_id IS NULL \
             ORDER BY last_checked_at ASC NULLS FIRST \
             FOR UPDATE SKIP LOCKED LIMIT 1"
        }
        _ => {
            "SELECT id, label, scheme, host, port, username, password_cipher FROM proxies \
             WHERE status = $1 \
               AND last_checked_at >= now() - interval '10 minutes' \
               AND exit_ip IS NOT NULL \
               AND lease_session_id IS NULL \
               AND (cooldown_until IS NULL OR cooldown_until <= now()) \
               AND $2 = $2 \
             ORDER BY failure_count, latency_ms ASC NULLS LAST \
             FOR UPDATE SKIP LOCKED LIMIT 1"
        }
    };
    let row = sqlx::query_as::<_, ClaimedProxy>(sql)
        .bind(ProxyStatus::Available.as_str())
        .bind(ProxyStatus::Error.as_str())
        .fetch_optional(&mut *tx)
        .await?;
    Ok(row)
}

async fn lease_account(
    tx: &mut sqlx::PgConnection,
    account_id: Uuid,
    session_id: Uuid,
    expires_at: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE accounts SET lease_session_id = $2, lease_expires_at = $3, updated_at = now() \
         WHERE id = $1",
    )
    .bind(account_id)
    .bind(session_id)
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn lease_proxy(
    tx: &mut sqlx::PgConnection,
    proxy_id: Uuid,
    session_id: Uuid,
    expires_at: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE proxies SET lease_session_id = $2, lease_expires_at = $3, status = $4, \
             updated_at = now() \
         WHERE id = $1",
    )
    .bind(proxy_id)
    .bind(session_id)
    .bind(expires_at)
    .bind(ProxyStatus::Occupied.as_str())
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// 会话进入运行中（Worker 报告浏览器和代理都就绪）。
pub async fn activate(state: &AppState, session_id: Uuid) -> AppResult<()> {
    let session = store::session::get_session(&state.pool, session_id).await?;
    store::session::activate_session(&state.pool, session_id).await?;
    if let Some(account_id) = session.account_id {
        sqlx::query(
            "UPDATE accounts SET status = $2, last_error = NULL, \
                 login_recovery_attempted_at = NULL, updated_at = now() \
             WHERE id = $1 AND status = $3 AND login_recovery_attempted_at IS NOT NULL",
        )
        .bind(account_id)
        .bind(AccountStatus::Registered.as_str())
        .bind(AccountStatus::LoginFailed.as_str())
        .execute(&state.pool)
        .await?;
    }
    store::node::set_slot(
        &state.pool,
        session.node_id,
        session.slot_index,
        SlotStatus::Running,
        Some(session_id),
        "会话已就绪",
    )
    .await?;
    state
        .events
        .publish("会话变更", serde_json::json!({ "会话": session_id }));
    Ok(())
}

/// 结束会话并释放它占用的一切。
///
/// 与「回收」的唯一区别是原因不同：这里是正常收尾或管理员要求，
/// 因此仍然要走同一套释放动作——否则两条路径迟早会漂移出不一致的清理逻辑。
pub async fn close_session(
    state: &AppState,
    session_id: Uuid,
    status: SessionStatus,
    reason: &str,
) -> AppResult<()> {
    if !matches!(status, SessionStatus::Ended | SessionStatus::Failed) {
        return Err(AppError::bad("结束会话只能落到「已结束」或「失败」"));
    }
    let session = store::session::get_session(&state.pool, session_id).await?;

    let mut tx = state.pool.begin().await?;
    if status == SessionStatus::Failed {
        let attribution = classify_failure(reason, None).attribution();
        if let (Some(proxy_id), Some(proxy_status)) = (session.proxy_id, attribution.proxy_status) {
            crate::scheduler::submit::apply_proxy_status(&mut tx, proxy_id, proxy_status, state)
                .await?;
        }
        if let (Some(account_id), Some(account_status)) =
            (session.account_id, attribution.account_status)
        {
            store::resource::set_account_status(&mut *tx, account_id, account_status, Some(reason))
                .await?;
            if account_status == AccountStatus::LoginFailed
                && reason.contains("automatic login recovery failed:")
            {
                sqlx::query(
                    "UPDATE accounts SET login_recovery_attempted_at = now(), updated_at = now() \
                     WHERE id = $1",
                )
                .bind(account_id)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    store::session::end_session(&mut *tx, session_id, status, reason).await?;
    store::session::release_session_resources(&mut tx, session_id).await?;
    requeue_leased_tasks(&mut tx, session_id, reason).await?;
    store::node::set_slot(
        &mut *tx,
        session.node_id,
        session.slot_index,
        SlotStatus::Idle,
        None,
        "",
    )
    .await?;
    store::node::refresh_available_slots(&mut *tx, session.node_id).await?;
    tx.commit().await?;

    state.events.publish(
        "会话变更",
        serde_json::json!({ "会话": session_id, "原因": reason }),
    );
    Ok(())
}

/// 把会话里还持着租约、但尚未真正开始下载的任务放回待处理。
///
/// 只回退「已分配」：一旦到了「执行中」或「等待入库」，本机就可能已经有半个文件，
/// 直接放回待处理会让另一个 Worker 重下同一本书，也会让原 Worker 之后的成功上报
/// 被当成过期事件丢弃。那些任务交给第 14.4 节的「待确认 + NAS 核验」处理。
pub(crate) async fn requeue_leased_tasks(
    tx: &mut sqlx::PgConnection,
    session_id: Uuid,
    reason: &str,
) -> AppResult<u64> {
    let claimed = sqlx::query(
        "UPDATE book_tasks SET status = $2, attempts = GREATEST(attempts - 1, 0), \
             stage = '', stage_version = stage_version + 1, \
             lease_node_id = NULL, lease_session_id = NULL, lease_execution_id = NULL, \
             lease_expires_at = NULL, last_error = $4, updated_at = now() \
         WHERE lease_session_id = $1 AND status = $3",
    )
    .bind(session_id)
    .bind(platform_domain::TaskStatus::Pending.as_str())
    .bind(platform_domain::TaskStatus::Claimed.as_str())
    .bind(reason)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let uncertain = sqlx::query(
        "UPDATE book_tasks SET status = $2, stage_version = stage_version + 1, \
             lease_expires_at = NULL, last_error = $5, updated_at = now() \
         WHERE lease_session_id = $1 AND status IN ($3, $4)",
    )
    .bind(session_id)
    .bind(platform_domain::TaskStatus::NeedsConfirm.as_str())
    .bind(platform_domain::TaskStatus::Running.as_str())
    .bind(platform_domain::TaskStatus::AwaitingIngest.as_str())
    .bind(reason)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    Ok(claimed + uncertain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_port_is_stable_per_slot() {
        assert_eq!(forward_port(0), 19001);
        assert_eq!(forward_port(3), 19004);
        // 负数槽位不存在，但也不该算出一个撞上别人的端口
        assert_eq!(forward_port(-1), 19001);
    }

    #[test]
    fn download_needs_both_account_and_proxy() {
        assert_eq!(
            needs_of(TaskType::BookDownload),
            (AccountNeed::Usable, ProxyNeed::Healthy)
        );
        assert_eq!(
            needs_of(TaskType::AccountRegister),
            (AccountNeed::ToRegister, ProxyNeed::Healthy)
        );
    }

    #[test]
    fn nas_verify_needs_neither() {
        assert_eq!(
            needs_of(TaskType::NasVerify),
            (AccountNeed::None, ProxyNeed::None)
        );
    }

    #[test]
    fn proxy_check_reuses_error_proxies() {
        // 检测任务的意义就是重新判定异常代理，因此它不能只挑「可用」的
        assert_eq!(
            needs_of(TaskType::ProxyCheck),
            (AccountNeed::None, ProxyNeed::ForCheck)
        );
    }

    #[test]
    fn login_recovery_is_only_sent_to_compatible_workers() {
        assert!(!worker_supports_login_recovery(""));
        assert!(!worker_supports_login_recovery("0.3.23"));
        assert!(worker_supports_login_recovery("0.3.24"));
        assert!(worker_supports_login_recovery("v0.4.0"));
        assert!(worker_supports_login_recovery("1.0.0"));
    }
}
