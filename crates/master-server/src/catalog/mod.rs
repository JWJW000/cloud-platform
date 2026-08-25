//! 图书馆总库与索引核心领域系统（设计方案 V1）。
//!
//! 包含：
//! - `ingestion`：流式解析、结构识别、幂等检查点与隔离区；
//! - `resolution`：三层去重、规范化实体消歧与章节关联；
//! - `search`：多维分面过滤、游标分页与书目详情聚合；
//! - `acquisition`：唯一持续全局获取池、租约与重试退避；
//! - `storage`：馆藏文件证据校验与状态收敛；
//! - `outbox`：搜索投影事务外发。

pub mod acquisition;
pub mod ingestion;
pub mod outbox;
pub mod resolution;
pub mod search;
pub mod storage;

pub use acquisition::{
    claim_acquisition_task, report_acquisition_task, retry_acquisition_target,
    set_acquisition_priority, AcquisitionAssignment, AcquisitionReportRequest, WorkerClaimRequest,
};
pub use ingestion::{
    execute_import, parse_csv_stream, preview_import, ImportExecutionResult, ImportManifestRequest,
    ImportPreviewResult, ParsedCatalogItemSummary, StartImportRequest,
};
pub use outbox::process_outbox_events;
pub use resolution::{resolve_item, ParsedCatalogItem, ResolutionResult};
pub use search::{
    get_catalog_edition_detail, search_catalog, CatalogSearchParams, CatalogSearchResponse,
    FacetCount,
};
pub use storage::{commit_library_file, CommitLibraryFileRequest, CommitLibraryFileResult};
