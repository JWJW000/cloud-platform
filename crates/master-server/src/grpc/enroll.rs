//! 节点注册（第 15.1 节）。
//!
//! 这是唯一一个**先于 mTLS**的调用：节点此刻还没有客户端证书，正是来换证书的。
//! 因此它的准入完全靠一次性注册码，而注册码的三条性质缺一不可——
//! 只能用一次、有过期时间、由管理员显式创建。三者都在
//! [`store::node::consume_enroll_code`] 里以 `SELECT … FOR UPDATE` 兑现，
//! 并发注册不会让同一个码放进来两台机器。
//!
//! 注册**不等于**批准：新节点落成「待审核」，管理员点过审核才会进入可分配状态。
//! 一台拿到有效注册码的机器仍然领不到任何任务，这条边界是刻意的。

use platform_domain::{LogLevel, NasLayout, OperationSource};
use platform_proto::v1 as pb;

use crate::error::{AppError, AppResult};
use crate::security::{hash_node_token, new_node_token};
use crate::state::AppState;
use crate::store;

/// 节点名的最大长度。超长的主机名会把管理界面的表格挤变形。
const MAX_NODE_NAME: usize = 64;

/// 处理一次注册请求。
pub async fn enroll(state: &AppState, request: pb::EnrollRequest) -> AppResult<pb::EnrollResponse> {
    let code = request.enroll_code.trim();
    if code.is_empty() {
        return Err(AppError::bad("注册码不能为空"));
    }
    let hostname = request.hostname.trim();
    if hostname.is_empty() {
        return Err(AppError::bad("主机名不能为空"));
    }
    let name = node_name(hostname);

    // 明文凭据只在这个响应里出现一次，数据库里只留散列。
    let token = new_node_token();
    let token_hash = hash_node_token(&token);

    let mut tx = state.pool.begin().await?;

    // 先建节点再消码，是因为消码需要节点编号，而 `upsert_node` 自己生成编号。
    // 顺序不影响正确性：注册码无效时下面的 `?` 会带着整个事务一起回滚，
    // 既不会留下节点行，也不会把码标成已用。
    let node = store::node::upsert_node(
        &mut tx,
        &name,
        hostname,
        os_label(&request.os),
        request.os_version.trim(),
        request.agent_version.trim(),
        request.requested_slots.min(64) as i32,
        &token_hash,
    )
    .await?;

    let issued_code = store::node::consume_enroll_code(&mut tx, code, node.id).await?;

    // 槽位数以注册码为准：管理员在创建码时就决定了这台机器能开几个浏览器，
    // 节点自报的 `requested_slots` 只是没有码上限时的回退值。
    let max_slots = if issued_code.max_slots > 0 {
        issued_code.max_slots
    } else {
        node.max_slots
    };
    let node = if max_slots != node.max_slots {
        store::node::set_node_capacity(&mut *tx, node.id, max_slots, node.upload_concurrency)
            .await?
    } else {
        node
    };
    store::node::ensure_slots(&mut tx, node.id, node.max_slots).await?;

    // 校验 CSR：若开启 mTLS，CSR 必须存在且合法
    let certificate_pem = if request.csr_pem.trim().is_empty() {
        if state.config.security.require_client_cert {
            return Err(AppError::bad(
                "服务器要求 mTLS，注册必须提交有效 CSR".to_string(),
            ));
        }
        String::new()
    } else {
        if request.csr_pem.len() > 8192 {
            return Err(AppError::bad("CSR 大小超出合理限制".to_string()));
        }
        let issued = state
            .ca
            .sign_csr(&request.csr_pem, &node.id.to_string())
            .map_err(|error| AppError::bad(format!("CSR 无法签发：{error}")))?;
        store::node::record_certificate(
            &mut *tx,
            node.id,
            &issued.fingerprint,
            &issued.certificate_pem,
            issued.not_after,
        )
        .await?;
        issued.certificate_pem
    };

    // 建立首个配置版本，节点连上来时就有一份可比对的版本号，
    // 否则第一次心跳会因为「版本不一致」而白白多下发一次配置。
    store::node::publish_config(&mut tx, node.id, &config_snapshot(state, &node)).await?;
    tx.commit().await?;

    store::admin::log(
        &state.pool,
        OperationSource::Worker,
        LogLevel::Info,
        &name,
        "节点注册",
        &node.id.to_string(),
        &format!(
            "主机 {hostname}，系统 {}，Agent {}，槽位 {}",
            node.os, node.agent_version, node.max_slots
        ),
    )
    .await?;
    state
        .events
        .publish("节点变更", serde_json::json!({ "节点": node.id }));

    Ok(pb::EnrollResponse {
        node_id: node.id.to_string(),
        node_token: token,
        status: node.status.clone(),
        certificate_pem,
        ca_certificate_pem: state.ca.certificate_pem().to_string(),
        message: "注册成功，等待管理员审核后才会分配任务".to_string(),
    })
}

/// 把主机名收成一个可读且可作唯一键的节点名。
///
/// 用主机名而不是随机名做唯一键，是为了让「同一台机器重装后重新注册」
/// 复用原来那一行，而不是在管理界面里留下一串幽灵节点。
fn node_name(hostname: &str) -> String {
    let cleaned: String = hostname
        .chars()
        .filter(|c| !c.is_control() && *c != '\'' && *c != '"')
        .collect();
    let cleaned = cleaned.trim();
    let name: String = cleaned.chars().take(MAX_NODE_NAME).collect();
    if name.is_empty() {
        "未命名节点".to_string()
    } else {
        name
    }
}

/// 归一化操作系统标识。
///
/// 这是少数**不中文化**的字段（第 2 节命名约定）：`Windows`/`macOS`/`Linux`
/// 要和 Agent 的构建目标、脚本里的判断保持一致。节点自报的写法五花八门
/// （`windows`、`darwin`、`WIN32`），在入口处收敛一次，
/// 管理界面的筛选就不必再对付大小写。
pub fn os_label(raw: &str) -> &'static str {
    let lowered = raw.trim().to_ascii_lowercase();
    if lowered.starts_with("win") {
        "Windows"
    } else if lowered.starts_with("mac")
        || lowered.starts_with("darwin")
        || lowered.starts_with("os x")
    {
        "macOS"
    } else if lowered.starts_with("linux")
        || lowered.starts_with("ubuntu")
        || lowered.starts_with("debian")
    {
        "Linux"
    } else {
        "未知"
    }
}

/// 存档一份「下发给该节点的配置长什么样」。
///
/// 只用于版本记账与排障对照，真正下发的是 [`crate::grpc::convert::node_config_message`]；
/// 两者都从同一份 [`crate::config::MasterConfig`] 取值，因此不会各说一套。
fn config_snapshot(state: &AppState, node: &crate::models::WorkerNode) -> serde_json::Value {
    let scheduler = state.scheduler();
    serde_json::json!({
        "槽位上限": node.max_slots,
        "上传并发": node.upload_concurrency,
        "心跳间隔秒": scheduler.heartbeat_interval_secs,
        "会话续租秒": scheduler.session_renew_secs,
        "进度最小间隔秒": scheduler.progress_min_interval_secs,
        "进度最小字节": scheduler.progress_min_bytes,
        "会话时长上限秒": scheduler.session_max_duration_secs,
        "停滞判定秒": scheduler.stall_timeout_secs,
        "NAS目录": NasLayout::default().files_dir,
        "最低剩余GB": state.config.nas.free_space_alert_gb,
        "站点地址": state.config.server.site_base,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_names_are_reused_as_node_names() {
        assert_eq!(node_name("  书房台式机 "), "书房台式机");
    }

    #[test]
    fn quotes_and_control_characters_are_stripped() {
        assert_eq!(node_name("节点\u{0}\"A\'"), "节点A");
    }

    #[test]
    fn empty_host_name_still_yields_a_usable_name() {
        assert_eq!(node_name("   "), "未命名节点");
    }

    #[test]
    fn long_host_names_are_truncated_by_characters_not_bytes() {
        let name = node_name(&"节".repeat(200));
        assert_eq!(name.chars().count(), MAX_NODE_NAME);
    }

    #[test]
    fn operating_systems_keep_their_technical_spelling() {
        // 这几个值要和 Agent 构建目标一致，因此不中文化
        assert_eq!(os_label("windows"), "Windows");
        assert_eq!(os_label("Windows 11"), "Windows");
        assert_eq!(os_label("darwin"), "macOS");
        assert_eq!(os_label("Mac OS 15"), "macOS");
        assert_eq!(os_label("Ubuntu 24.04"), "Linux");
        assert_eq!(os_label("Plan 9"), "未知");
    }
}
