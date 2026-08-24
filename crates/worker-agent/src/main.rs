//! Worker Agent 主执行入口。
//!
//! 支持子命令：
//! - `run`: 启动 Worker 客户端守护进程。V5 起无身份时自动注册
//!   （RegisterNode → 等待审核 → 批准后领取证书/令牌 → OpenLink）；
//! - `enroll`: （弃用）旧注册码注册，打印弃用提示后仍可兼容使用；
//! - `reset-identity`: 危险操作，删除本机身份/证书/私钥（需确认；不会让服务器忘记旧身份）。

use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use platform_proto::v1::worker_link_client::WorkerLinkClient;
use platform_proto::v1::EnrollRequest;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use uuid::Uuid;
use worker_agent::config::{SavedIdentity, WorkerConfig};
use worker_agent::tls;

#[derive(Parser, Debug)]
#[command(name = "worker-agent", about = "局域网 Worker Agent", version)]
struct Cli {
    /// 配置文件路径
    #[arg(short, long, default_value = "config/worker.toml", global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// 启动 Worker Agent（V5：无身份时自动注册并等待审核）
    Run,
    /// [弃用] 使用注册码进行初次注册（新部署请直接 `run`，自动注册无需注册码）
    Enroll {
        /// 管理员签发的一次性注册码
        #[arg(short = 'e', long)]
        code: String,
        /// 自定义主机名（默认获取系统主机名）
        #[arg(long)]
        hostname: Option<String>,
    },
    /// 危险操作：删除本机身份/证书/私钥（不会让服务器忘记旧身份，需先由管理员处理云端记录）
    ResetIdentity {
        /// 跳过交互确认（自动化场景使用）
        #[arg(long)]
        yes: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,worker_agent=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    let config = WorkerConfig::load(&cli.config)?;

    match cli.command {
        Commands::Enroll { code, hostname } => {
            tracing::warn!(
                "`enroll` 已弃用：V5 起 Worker 只需配置 endpoint 直接 `run`，会自动注册并等待审核，无需注册码。\
                 本命令保留一个兼容周期，旧注册码仍可完成注册。"
            );
            enroll_with_code(&config, &code, hostname.as_deref()).await?;
        }
        Commands::ResetIdentity { yes } => {
            reset_identity(&config, yes)?;
        }
        Commands::Run => {
            // V5：无身份时自动注册（第 6.10 节流程 1-4）
            let identity = config.load_identity()?;
            let identity = worker_agent::registration::ensure_registered(&config, identity).await?;

            tracing::info!(node_id = %identity.node_id, "启动 Worker Agent 节点...");
            worker_agent::client::run_agent_loop(config, identity).await?;
        }
    }

    Ok(())
}

/// 旧注册码注册（弃用兼容路径；V5 起新部署请直接 `run` 自动注册）。
#[allow(deprecated)]
async fn enroll_with_code(config: &WorkerConfig, code: &str, hostname: Option<&str>) -> Result<()> {
    let hostname = hostname.map(str::to_string).unwrap_or_else(|| {
        sysinfo::System::host_name().unwrap_or_else(|| "worker-node".to_string())
    });

    tracing::info!(
        endpoint = %config.master.endpoint,
        hostname = %hostname,
        "正在生成本地私钥与 CSR 并向 Master 申请节点注册..."
    );

    // 1. 本机生成私钥与 CSR（私钥绝对不离开本机）
    let key = KeyPair::generate().context("生成客户端私钥失败")?;
    let key_pem = key.serialize_pem();

    let mut params = CertificateParams::new(Vec::<String>::new()).context("构造 CSR 参数失败")?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, format!("worker-{hostname}"));
    params.distinguished_name = dn;
    let csr = params.serialize_request(&key).context("生成 CSR 失败")?;
    let csr_pem = csr.pem().context("序列化 CSR PEM 失败")?;

    // 2. 使用注册通道连接 Master
    let channel = tls::enrollment_channel(config).await?;
    let mut client = WorkerLinkClient::new(channel);

    let req = EnrollRequest {
        enroll_code: code.to_string(),
        hostname: hostname.clone(),
        os: std::env::consts::OS.to_string(),
        os_version: sysinfo::System::os_version().unwrap_or_default(),
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        requested_slots: config.execution.requested_slots,
        csr_pem,
    };

    let resp = client.enroll(req).await?.into_inner();

    if resp.certificate_pem.trim().is_empty() || resp.ca_certificate_pem.trim().is_empty() {
        bail!("Master 返回的证书或 CA 证书为空，注册未能成功签发客户端证书");
    }

    // 计算证书 SHA-256 指纹（对 DER 计算，见 tls.rs）
    let cert_fingerprint = worker_agent::tls::fingerprint_der(&resp.certificate_pem)?;

    // 3. 原子持久化客户端私钥、证书、Node CA 与身份凭据文件
    let paths = config.identity_paths();
    if let Err(e) = save_enrollment_artifacts(
        config,
        &paths,
        &key_pem,
        &resp.certificate_pem,
        &resp.ca_certificate_pem,
        &SavedIdentity {
            node_id: resp.node_id.clone(),
            node_token: resp.node_token,
            node_name: Some(hostname),
            certificate_fingerprint: Some(cert_fingerprint),
            installation_id: None, // 旧注册码路径没有安装标识；直连注册才生成
            registration_session: None,
            registration_challenge: None,
            status: Some("已批准".to_string()),
            client_certificate_pem: None,
            ca_certificate_pem: None,
        },
    ) {
        // 失败路径只清理本次临时文件；原有可用身份由 save 函数负责保留
        return Err(e).context("保存节点证书与身份文件失败，原身份保持不变");
    }

    println!("注册成功！");
    println!("节点 ID: {}", resp.node_id);
    println!("状态: {}", resp.status);
    println!("消息: {}", resp.message);
    println!("客户端证书已保存至 {}", paths.client_cert_file.display());
    println!("私钥已保存至 {}", paths.client_key_file.display());
    println!("Node CA 已保存至 {}", paths.node_ca_file.display());
    println!("节点凭据已持久化至 {}", paths.identity_file.display());
    println!("请在管理平台审核通过后执行 `worker-agent run` 启动！");
    Ok(())
}

/// 删除本机身份/证书/私钥（危险操作，需要确认；不接触服务器）。
///
/// 服务器上的旧节点记录不会因此消失——管理员必须先拒绝/禁用旧节点，
/// 否则同一安装标识重新注册会被服务器按原决定拒绝。
fn reset_identity(config: &WorkerConfig, yes: bool) -> Result<()> {
    let paths = config.identity_paths();
    let targets = [
        &paths.identity_file,
        &paths.client_key_file,
        &paths.client_cert_file,
        &paths.node_ca_file,
    ];
    let missing = targets.iter().filter(|p| !p.exists()).count();
    let present = targets.iter().filter(|p| p.exists()).count();

    if present == 0 {
        println!("本机没有任何身份/证书文件，无需重置。");
        return Ok(());
    }
    println!("将删除以下文件（{} 个存在，{} 个缺失）：", present, missing);
    for p in targets.iter().filter(|p| p.exists()) {
        println!("  - {}", p.display());
    }
    println!(
        "警告：此操作不可恢复；服务器上的旧节点记录不会被删除。\
         \n请先由管理员在云端拒绝/禁用旧节点，再执行本命令。"
    );

    if !yes {
        print!("确认删除请输入节点编号或 YES（大写）：");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let answer = line.trim().to_string();
        let id_ok = config
            .load_identity()?
            .map(|id| answer == id.node_id)
            .unwrap_or(false);
        if answer != "YES" && !id_ok {
            bail!("确认失败，已取消重置（身份未改动）");
        }
    }

    for p in targets.iter().filter(|p| p.exists()) {
        std::fs::remove_file(p).with_context(|| format!("删除文件失败: {}", p.display()))?;
    }
    println!("已删除本机身份与凭据文件。请先确认云端旧节点已拒绝/禁用，再执行 `worker-agent run` 重新注册。");
    Ok(())
}

fn save_enrollment_artifacts(
    config: &WorkerConfig,
    paths: &worker_agent::config::WorkerIdentityPaths,
    key_pem: &str,
    cert_pem: &str,
    ca_pem: &str,
    identity: &SavedIdentity,
) -> Result<()> {
    // 0. 落盘前校验（V4 方案第 8.4 节）：
    //    证书公钥与私钥匹配、证书链由 Node CA 签发、有效期合法。
    worker_agent::tls::validate_cert_key_pair(cert_pem, key_pem, &paths.client_cert_file)?;
    worker_agent::tls::validate_chain_signed_by(cert_pem, ca_pem)?;
    worker_agent::tls::validate_cert_not_expired(cert_pem, &paths.client_cert_file)?;

    // 1. 写唯一临时文件（与目标同目录，保证 rename 原子性），逐个 sync_all。
    let suffix = format!(".tmp-{}", Uuid::new_v4().simple());
    let targets = [
        (paths.client_key_file.clone(), key_pem.as_bytes()),
        (paths.client_cert_file.clone(), cert_pem.as_bytes()),
        (paths.node_ca_file.clone(), ca_pem.as_bytes()),
    ];
    let mut temp_files = Vec::new();
    let write_result = (|| -> Result<()> {
        for (target, content) in &targets {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temp = target.with_file_name(format!(
                "{}{}",
                target
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file"),
                suffix
            ));
            write_file_atomic(temp.clone(), content)?;
            temp_files.push((temp, target.clone()));
        }
        Ok(())
    })();

    if let Err(err) = write_result {
        // 失败只删除本次临时文件，原有身份保持不动
        for (temp, _) in &temp_files {
            let _ = std::fs::remove_file(temp);
        }
        return Err(err).context("写入注册产物临时文件失败，未改动原有身份");
    }

    // 2. 对已有身份创建可恢复备份，再原子切换。
    //    先备份私钥/证书/CA，最后切换身份文件——identity.json 是「提交点」：
    //    它存在且完整，run 才会认为注册完成。
    let had_old_identity = paths.identity_file.exists();
    if had_old_identity {
        let backup_dir = paths
            .identity_file
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join(format!(
                "identity-backup-{}",
                chrono::Utc::now().timestamp()
            ));
        std::fs::create_dir_all(&backup_dir)?;
        for path in [
            &paths.client_key_file,
            &paths.client_cert_file,
            &paths.node_ca_file,
        ] {
            if path.exists() {
                let dest = backup_dir.join(path.file_name().unwrap_or_default());
                std::fs::copy(path, &dest)?;
            }
        }
        if paths.identity_file.exists() {
            let dest = backup_dir.join("identity.json");
            std::fs::copy(&paths.identity_file, &dest)?;
        }
        tracing::info!(
            backup = %backup_dir.display(),
            "已备份旧身份，切换失败时可从备份恢复"
        );
    }

    // 3. 逐个原子切换到目标路径（先私钥/证书/CA，最后身份文件）。
    for (temp, target) in &temp_files {
        std::fs::rename(temp, target)
            .with_context(|| format!("切换文件失败: {}", target.display()))?;
        // 私钥保持 0600
        #[cfg(unix)]
        if target == &paths.client_key_file {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    config.save_identity(identity)?;

    Ok(())
}

/// 写文件并 fsync（0600 权限，Unix）。
fn write_file_atomic(path: std::path::PathBuf, content: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("创建临时文件失败: {}", path.display()))?;
        file.write_all(content)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("创建临时文件失败: {}", path.display()))?;
        file.write_all(content)?;
        file.sync_all()?;
    }
    Ok(())
}
