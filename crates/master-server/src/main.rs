//! Master 服务端主入口程序（第 16 节、第 18 节）。
//!
//! 提供四个主要 CLI 命令：
//! - `serve`: 启动 Master 主服务（HTTP REST + gRPC WorkerLink + 调度自愈 Reaper + Webshare 同步）；
//! - `create-admin`: 初始化创建超级管理员账户；
//! - `keygen`: 生成字段加密密钥（AES-256-GCM 32字节 base64）；
//! - `issue-enroll-code`: 快速签发一次性 Worker 注册码。

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use master_server::config::MasterConfig;
use master_server::grpc::WorkerLinkService;
use master_server::security::{hash_password, new_enroll_code, FieldCipher};
use master_server::state::AppState;
use master_server::store;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser, Debug)]
#[command(name = "master-server", about = "云端 Master 服务中心", version)]
struct Cli {
    /// 配置文件路径
    #[arg(short, long, default_value = "config/master.toml", global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// 启动服务
    Serve,
    /// 创建管理员用户
    CreateAdmin {
        /// 用户名
        #[arg(short, long)]
        username: String,
        /// 密码
        #[arg(short, long)]
        password: String,
        /// 角色（默认：超级管理员）
        #[arg(short, long, default_value = "超级管理员")]
        role: String,
    },
    /// 生成字段加密密钥
    Keygen,
    /// 签发一次性 Worker 注册码
    IssueEnrollCode {
        /// 备注说明
        #[arg(short, long)]
        note: Option<String>,
        /// 最大槽位数
        #[arg(short, long, default_value_t = 5)]
        max_slots: i32,
        /// 有效小时数
        #[arg(short, long, default_value_t = 24)]
        valid_hours: i64,
    },
    /// 从 PostgreSQL 全量重建 OpenSearch 书目索引
    ReindexCatalog {
        /// 每次批量写入的文档数
        #[arg(long, default_value_t = 500)]
        batch_size: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志格式
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,master_server=debug,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Keygen => {
            let key = FieldCipher::generate_key_base64();
            println!("已生成 AES-256-GCM 字段加密密钥（32 字节 Base64 编码）：");
            println!("{key}");
            println!("\n可将此密钥配置到 master.toml 的 security.field_key_base64 或环境变量 MASTER_FIELD_KEY 中。");
            return Ok(());
        }
        Commands::CreateAdmin {
            username,
            password,
            role,
        } => {
            let config = MasterConfig::load(&cli.config)?;
            let pool = store::connect(&config.database).await?;
            if config.database.auto_migrate {
                store::run_migrations(&pool).await?;
            }

            let password_hash = hash_password(&password)?;
            let user = store::admin::create_user(&pool, &username, &password_hash, &role).await?;
            println!("管理员用户创建成功：");
            println!("ID: {}", user.id);
            println!("用户名: {}", user.username);
            println!("角色: {}", user.role);
            return Ok(());
        }
        Commands::IssueEnrollCode {
            note,
            max_slots,
            valid_hours,
        } => {
            let config = MasterConfig::load(&cli.config)?;
            let pool = store::connect(&config.database).await?;
            if config.database.auto_migrate {
                store::run_migrations(&pool).await?;
            }

            let code_str = new_enroll_code();
            let code = store::node::issue_enroll_code(
                &pool,
                &code_str,
                note.as_deref(),
                max_slots,
                valid_hours,
                None,
            )
            .await?;

            println!("一次性 Worker 注册码生成成功：");
            println!("注册码: {}", code.code);
            println!("槽位上限: {}", code.max_slots);
            println!("过期时间: {}", code.expires_at.to_rfc3339());
            return Ok(());
        }
        Commands::ReindexCatalog { batch_size } => {
            let config = MasterConfig::load(&cli.config)?;
            if !config.opensearch.enabled {
                anyhow::bail!("OpenSearch 未启用，请先设置 OPENSEARCH_ENABLED=1");
            }
            let pool = store::connect(&config.database).await?;
            if config.database.auto_migrate {
                store::run_migrations(&pool).await?;
            }
            let client =
                master_server::opensearch::OpenSearchClient::new(config.opensearch.clone())?;
            let total =
                master_server::opensearch::reindex_catalog(&pool, &client, batch_size).await?;
            println!("OpenSearch 书目索引重建完成：{total} 条");
            return Ok(());
        }
        Commands::Serve => {
            let config = MasterConfig::load(&cli.config)?;
            tracing::info!("正在初始化 Master 运行时状态...");

            let state = AppState::bootstrap(config).await?;

            // 启动定时回收器（Reaper）
            master_server::scheduler::spawn_reaper(state.clone());

            // 启动 Webshare 代理同步任务
            master_server::webshare::spawn_webshare_sync(state.clone());

            // V5：定期清理过期直连注册会话与超期申请（第 6.4 节）
            master_server::store::registration::spawn_registration_cleanup(state.pool.clone());

            // OpenSearch 是可重建查询投影：故障时保留 Outbox 并由检索接口回退 PostgreSQL。
            if let Some(client) = state.search.clone() {
                master_server::opensearch::spawn_outbox_sync(
                    state.pool.clone(),
                    client,
                    state.config.opensearch.clone(),
                );
            }

            // 启动书目统计后台定时预热与刷新协程（每 60 秒刷新一次，保障前端 100% 毫秒级响应）
            {
                let state_clone = state.clone();
                tokio::spawn(async move {
                    // 启动即刻预热一次
                    if let Ok(stats) =
                        master_server::store::catalog_v1::get_catalog_stats(&state_clone.pool).await
                    {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        if let Ok(mut guard) = state_clone.catalog_stats_cache.lock() {
                            *guard = Some((now, stats));
                        }
                    }
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                    interval.tick().await; // skip initial immediate tick
                    loop {
                        interval.tick().await;
                        if let Ok(stats) =
                            master_server::store::catalog_v1::get_catalog_stats(&state_clone.pool)
                                .await
                        {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            if let Ok(mut guard) = state_clone.catalog_stats_cache.lock() {
                                *guard = Some((now, stats));
                            }
                        }
                    }
                });
            }

            // 启动 gRPC 服务
            let grpc_addr: SocketAddr =
                state.config.server.grpc_listen.parse().with_context(|| {
                    format!("gRPC 监听地址格式无效: {}", state.config.server.grpc_listen)
                })?;

            let grpc_service = WorkerLinkService::new(state.clone()).into_server();

            tokio::spawn(async move {
                tracing::info!(listen = %grpc_addr, "gRPC WorkerLink 服务正在启动...");
                if let Err(err) = tonic::transport::Server::builder()
                    .add_service(grpc_service)
                    .serve(grpc_addr)
                    .await
                {
                    tracing::error!(error = %err, "gRPC 服务异常退出");
                }
            });

            // 构造 HTTP 路由
            let mut app = master_server::api::router(state.clone());

            // 若配置了静态资源目录，则挂载 SPA 静态托管。
            // V4-01：配置了 web_root 但目录或 index.html 缺失时**启动失败**，
            // 不再静默跳过——否则生产环境会出现「接口都在、首页 404」的假在线。
            if let Some(web_root) = &state.config.server.web_root {
                master_server::api::static_files::validate_web_root(web_root)
                    .with_context(|| format!("web_root 配置无效: {}", web_root.display()))?;
                tracing::info!(path = %web_root.display(), "挂载前端静态资源");
                app = app.fallback_service(
                    axum::Router::new()
                        .fallback(master_server::api::static_files::spa_fallback)
                        .with_state(std::sync::Arc::new(web_root.clone())),
                );
            }

            let http_addr: SocketAddr =
                state.config.server.http_listen.parse().with_context(|| {
                    format!("HTTP 监听地址格式无效: {}", state.config.server.http_listen)
                })?;

            tracing::info!(listen = %http_addr, "HTTP 管理后台 API 服务正在启动...");
            let listener = tokio::net::TcpListener::bind(http_addr).await?;
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await?;
        }
    }

    Ok(())
}
