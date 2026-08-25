//! 平台共享领域模型。
//!
//! 设计约束（见 `docs/cloud-master-worker-architecture.md` 第 3.4 与第 11 节）：
//!
//! - **业务枚举值一律为中文**，例如 `已注册`、`等待入库`。数据库存储值、REST JSON、
//!   gRPC 负载与前端展示都直接使用同一份中文字符串，前端不得自行翻译。
//! - **技术标识保持标准值**：Rust 标识符、数据库字段名、UUID、时间戳、
//!   操作系统名（`Windows`/`macOS`/`Linux`）、文件扩展名（`pdf`/`epub`）与协议名不做中文化。
//!
//! 因此本 crate 中的类型与字段名都是英文，只有 *序列化出去的值* 是中文。

pub mod catalog_norm;
pub mod dedup;
pub mod enums;
pub mod failure;
pub mod isbn;
pub mod legacy;
pub mod naming;
pub mod transitions;

pub use catalog_norm::{
    clean_text, extract_isbns, normalize_doi, normalize_format, normalize_md5, normalize_sha256,
    parse_publish_year,
};
pub use dedup::{normalize_person, normalize_title, BookIdentity, DedupKey};
pub use enums::{
    AccountImportMode, AccountRegistrationTaskStatus, AccountStatus, AcquisitionStatus, AlertLevel,
    BatchStatus, CatalogFileVerifyStatus, ContributorRole, EnumParseError, ExecutionResult,
    IdentifierType, ImportRunStatus, ImportStatus, ImportType, LogLevel, ManualActionStatus,
    ManualActionType, OperationSource, ProxyStatus, ResolutionStatus, SessionStatus, SlotStatus,
    SourceAssetStatus, StorageBackend, SubjectType, TaskStatus, TaskType, VerifyStatus, WorkType,
    WorkerCommandStatus, WorkerStatus,
};
pub use failure::{classify_failure, FailureClass};
pub use isbn::{normalize_isbn, Isbn};
pub use legacy::{migrate_account_status, migrate_task_status, LegacyMigrationError};
pub use naming::{sanitize_filename, NasLayout};
pub use transitions::{adopt_reported_worker_status, StatusAdoption, TransitionError};

/// 平台内部使用的唯一编号类型（技术标识，保持 UUID 标准形态）。
pub type Id = uuid::Uuid;

/// 生成一个新的唯一编号。
pub fn new_id() -> Id {
    uuid::Uuid::new_v4()
}

/// 支持的图书文件格式。扩展名属于技术标识，保持英文小写。
pub const SUPPORTED_FORMATS: [&str; 2] = ["pdf", "epub"];

/// 支持的操作系统标识。属于技术标识，保持原始大小写。
pub const SUPPORTED_OS: [&str; 3] = ["Windows", "macOS", "Linux"];
