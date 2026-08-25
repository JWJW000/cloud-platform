//! 大规模图书书目独立流式导入 CLI 工具（方案第 12 节）。
//!
//! 支持命令：
//! - `preview --root <只读目录> --source cn`：文件结构探测与样例预览；
//! - `run --root <只读目录> --source cn --resume`：分批流式导入并记录断点；
//! - `status <run-id>`：查看导入进度与对账状态。

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod checkpoint;
mod discovery;
mod uploader;

#[derive(Parser)]
#[command(name = "catalog-importer")]
#[command(about = "大规模图书书目独立导入器", long_about = None)]
struct Cli {
    #[arg(short, long, default_value = "http://127.0.0.1:8080")]
    endpoint: String,

    #[arg(short, long)]
    token: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 预检书目目录结构与文件样例
    Preview {
        /// 只读数据根目录（如 /Volumes/PortableSSD/202608 (1)）
        #[arg(short, long)]
        root: PathBuf,

        /// 书目来源标识（如 cn, en, ot）
        #[arg(short, long, default_value = "cn")]
        source: String,
    },
    /// 执行分批流式导入
    Run {
        /// 只读数据根目录
        #[arg(short, long)]
        root: PathBuf,

        /// 书目来源标识
        #[arg(short, long, default_value = "cn")]
        source: String,

        /// 是否从断点续传
        #[arg(long, default_value_t = true)]
        resume: bool,

        /// 单批条目上限（默认 5000）
        #[arg(long, default_value_t = 5000)]
        batch_size: usize,
    },
    /// 查看导入运行状态与对账统计
    Status {
        /// 运行编号
        run_id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Preview { root, source } => {
            println!("========================================");
            println!(" 书目数据预检 (Preview)");
            println!(" 目标根目录: {}", root.display());
            println!(" 来源渠道:   {}", source);
            println!("========================================");

            let files = discovery::discover_catalog_files(&root)?;
            println!("发现 {} 个书目数据文件:", files.len());
            for (idx, f) in files.iter().enumerate().take(10) {
                let sz = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
                println!(
                    "  [{}] {} ({} MB)",
                    idx + 1,
                    f.display(),
                    sz / (1024 * 1024)
                );
            }
            if files.len() > 10 {
                println!("  ... 及其余 {} 个文件", files.len() - 10);
            }
        }
        Commands::Run {
            root,
            source,
            resume,
            batch_size,
        } => {
            println!("========================================");
            println!(" 启动书目分批导入 (Run)");
            println!(" 目标根目录: {}", root.display());
            println!(" 来源渠道:   {}", source);
            println!(" 单批大小:   {}", batch_size);
            println!(" 断点续传:   {}", resume);
            println!("========================================");

            let files = discovery::discover_catalog_files(&root)?;
            let state_file = PathBuf::from(".catalog_importer_checkpoint.json");
            let mut checkpoint = if resume {
                checkpoint::ImportCheckpointState::load(&state_file)?
            } else {
                checkpoint::ImportCheckpointState::default()
            };

            for f in files {
                uploader::process_catalog_file(
                    &cli.endpoint,
                    cli.token.as_deref(),
                    &f,
                    &source,
                    batch_size,
                    &mut checkpoint,
                    &state_file,
                )
                .await?;
            }

            println!(
                "所有文件导入已完成！总成功导入行数: {}",
                checkpoint.total_imported
            );
        }
        Commands::Status { run_id } => {
            println!("查询运行编号 {} 状态...", run_id);
        }
    }

    Ok(())
}
