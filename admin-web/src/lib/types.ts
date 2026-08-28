// 领域类型定义（与 Master REST API 对齐）。

export type Role = "超级管理员" | "任务管理员" | "只读用户";

export interface User {
  id: string;
  username: string;
  role: Role;
  status: string;
  token_version: number;
  created_at: string;
  last_login_at: string | null;
}

// ---------------------------------------------------------------- 总览

export interface Overview {
  workers: { total: number; online: number; storage_error: number };
  slots: { total: number; idle: number; running: number; error: number };
  today: {
    completed: number;
    failed: number;
    skipped: number;
    bytes_total: number;
    account_used: number;
  };
  accounts: { total: number; available: number; pending_reg: number };
  proxies: {
    total: number;
    available: number;
    occupied: number;
    cooling: number;
    error: number;
  };
  tasks: {
    pending: number;
    running: number;
    completed: number;
    failed: number;
    needs_confirm: number;
    running_batches: number;
  };
  open_alerts: number;
}

// ---------------------------------------------------------------- Worker

export interface WorkerNode {
  id: string;
  name: string;
  hostname: string;
  os: string;
  os_version: string;
  agent_version: string;
  status: string;
  max_slots: number;
  available_slots: number;
  upload_concurrency: number;
  config_version: string;
  applied_config_version: string;
  diagnostics_enabled: boolean;
  nas_healthy: boolean;
  nas_free_gb: number;
  staging_free_gb: number;
  cpu_percent: number;
  memory_used_mb: number;
  memory_total_mb: number;
  connected: boolean;
  last_heartbeat_at: string | null;
  approved_at: string | null;
  approved_by: string | null;
  installation_id: string | null;
  public_key_fingerprint: string | null;
  registration_status: string;
  requested_slots: number | null;
  configured_slots: number | null;
  registration_expires_at: string | null;
  first_seen_ip: string | null;
  last_registration_at: string | null;
  rejected_at: string | null;
  rejected_by: string | null;
  reject_reason: string | null;
  created_at: string;
}

export interface WorkerSlot {
  node_id: string;
  slot_index: number;
  status: string;
  session_id: string | null;
  task_id: string | null;
  detail: string | null;
}

export interface NodeCertificate {
  id: string;
  node_id: string;
  fingerprint: string;
  not_after: string;
  revoked_at: string | null;
  issued_at: string;
}

// ---------------------------------------------------------------- 图书

export interface Book {
  id: string;
  seq: number;
  raw_title: string;
  raw_author: string | null;
  raw_publisher: string | null;
  raw_isbn: string | null;
  normalized_isbn: string | null;
  dedup_key: string | null;
  verify_status: string;
  merged_into: string | null;
  created_at: string;
}

// ---------------------------------------------------------------- 批次

export interface Batch {
  id: string;
  name: string;
  source_file: string | null;
  status: string;
  priority: number;
  download_format: string;
  created_at: string;
  updated_at: string;
}

export interface BatchProgress {
  batch_id: string;
  total: number;
  done: number;
  failed: number;
  running: number;
  percent: number;
}

export interface GlobalDownloadControl {
  paused: boolean;
  updated_at: string;
  running_tasks: number;
}

// ---------------------------------------------------------------- 任务

export interface Task {
  id: string;
  book_id: string;
  title: string;
  book_seq: number;
  format: string;
  status: string;
  attempts: number;
  max_attempts: number;
  next_attempt_at: string | null;
  stage: string;
  stage_version: number;
  downloaded_bytes: number;
  total_bytes: number;
  lease_node_id: string | null;
  lease_session_id: string | null;
  lease_execution_id: string | null;
  lease_expires_at: string | null;
  nas_relative_path: string | null;
  last_error: string | null;
}

// ---------------------------------------------------------------- 账号 / 代理

export interface Account {
  id: string;
  email: string;
  nickname: string | null;
  status: string;
  daily_used: number;
  daily_limit: number;
  reset_date: string;
  lease_session_id: string | null;
  last_error: string | null;
  registered_at: string | null;
  last_login_at: string | null;
  created_at: string;
}

export interface AccountSummary {
  total: number;
  available: number;
  registered: number;
  pending_registration: number;
  verification_pending: number;
  login_failed: number;
  exhausted_today: number;
  disabled: number;
}

export interface AccountListResponse {
  items: Account[];
  total: number;
  limit: number;
  offset: number;
  summary: AccountSummary;
}

export interface ResetQuotaResponse {
  reset_count: number;
  message: string;
}

export interface Proxy {
  id: string;
  provider: string;
  external_id: string | null;
  label: string | null;
  scheme: string;
  host: string;
  port: number;
  status: string;
  exit_ip: string | null;
  latency_ms: number | null;
  success_count: number;
  failure_count: number;
  throttle_count: number;
  cooldown_until: string | null;
  lease_session_id: string | null;
  last_checked_at: string | null;
}

// ---------------------------------------------------------------- 会话 / 日志 / 告警

export interface Session {
  id: string;
  node_id: string;
  slot_index: number;
  account_id: string | null;
  proxy_id: string | null;
  task_type: string;
  status: string;
  local_forward_port: number;
  completed_count: number;
  lease_expires_at: string;
  protected_until: string | null;
  started_at: string;
}

export interface LogEntry {
  id: string;
  source: string;
  level: string;
  actor: string;
  action: string;
  target: string;
  detail: string;
  created_at: string;
}

export interface Alert {
  id: string;
  level: string;
  category: string;
  title: string;
  detail: string;
  node_id: string | null;
  resolved_at: string | null;
  created_at: string;
}

// ---------------------------------------------------------------- 设置 / 字典

export interface Setting {
  key: string;
  value: unknown;
}

export interface Dict {
  account_status: string[];
  task_status: string[];
  batch_status: string[];
  worker_status: string[];
  worker_registration_status: string[];
  session_status: string[];
  proxy_status: string[];
  log_level: string[];
  alert_level: string[];
  download_formats: string[];
  verify_status: string[];
}

// ---------------------------------------------------------------- V6 导入与业务任务

export interface BookPreviewRow {
  line: number;
  title: string;
  author: string | null;
  publisher: string | null;
  isbn: string | null;
  status: string;
  reason: string | null;
}

export interface BookImportPreview {
  import_token: string;
  file_name: string;
  file_sha256: string;
  total_rows: number;
  valid_rows: number;
  duplicate_in_file: number;
  duplicate_in_library: number;
  already_ingested: number;
  error_rows: number;
  warnings: string[];
  preview: BookPreviewRow[];
}

export interface CommitBooksRequest {
  import_token: string;
  start_immediately?: boolean;
}

export interface CommitBooksResponse {
  batch: Batch;
  deduplicated: number;
  already_ingested: number;
}

export type AccountImportMode = "待注册" | "已注册";

export interface AccountPreviewRow {
  line: number;
  email_masked: string;
  nickname: string;
  password_provided: boolean;
  status: string;
  reason: string | null;
}

// ---------------------------------------------------------------- 馆藏扫描与审核

export interface StorageLocation {
  id: string;
  node_id: string | null;
  root_key: string;
  backend: string;
  display_name: string;
  availability: "在线" | "离线" | "未知" | "已停用";
  last_seen_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface InventoryScanJob {
  id: string;
  node_id: string;
  storage_location_id: string;
  status: "待下发" | "扫描中" | "暂停" | "已完成" | "部分失败" | "已取消" | "失败";
  scan_mode: "增量" | "全量复核";
  checkpoint: Record<string, unknown>;
  discovered_count: number;
  hashed_count: number;
  matched_count: number;
  review_count: number;
  unmatched_count: number;
  skipped_count: number;
  error_count: number;
  started_at: string | null;
  finished_at: string | null;
  last_error: string | null;
  created_by: string | null;
  created_at: string;
  updated_at: string;
}

export interface InventoryCandidateDetail {
  candidate_id: string;
  edition_id: string;
  edition_title: string;
  publisher: string | null;
  publish_year: number | null;
  match_score: number;
  matched_fields: string[];
  conflict_fields: string[];
}

export interface InventoryReviewDetail {
  id: string;
  scan_job_id: string;
  object_key: string;
  file_name: string;
  extension: string;
  actual_size_bytes: number;
  sha256: string;
  md5: string | null;
  error_reason: string | null;
  candidates: InventoryCandidateDetail[];
}


export interface AccountImportPreview {
  import_token: string;
  file_name: string;
  file_sha256: string;
  total_rows: number;
  valid_rows: number;
  duplicate_in_file: number;
  duplicate_in_library: number;
  error_rows: number;
  warnings: string[];
  preview: AccountPreviewRow[];
}

export interface CommitAccountsRequest {
  import_token: string;
  mode: AccountImportMode;
  create_registration_batch?: boolean;
  batch_name?: string;
  priority?: number;
  start_immediately?: boolean;
}

export interface CommitAccountsResponse {
  imported_accounts: number;
  registration_batch: AccountRegistrationBatch | null;
}

export interface OutlookPreviewAccount {
  email: string;
  nickname: string;
}

export interface OutlookPreviewResponse {
  fetched: number;
  skipped: number;
  accounts: OutlookPreviewAccount[];
}

export interface OutlookSyncRequest {
  default_password: string;
  emails: string[];
  create_batch?: boolean;
  batch_name?: string;
  priority?: number;
  start_immediately?: boolean;
}

export interface SyncedAccount {
  id: string;
  email: string;
  nickname: string;
}

export interface OutlookSyncResponse {
  inserted: number;
  duplicates: number;
  skipped: number;
  accounts: SyncedAccount[];
  registration_batch: AccountRegistrationBatch | null;
}

export interface AccountRegistrationBatch {
  id: string;
  name: string;
  source_file: string | null;
  status: string;
  priority: number;
  created_at: string;
  updated_at: string;
}

export interface AccountRegistrationBatchProgress {
  batch_id: string;
  total: number;
  completed: number;
  failed: number;
  running: number;
  awaiting_confirm: number;
  pending: number;
}

export interface BatchWithProgress {
  id: string;
  name: string;
  source_file: string | null;
  status: string;
  priority: number;
  created_at: string;
  updated_at: string;
  progress: AccountRegistrationBatchProgress;
}

export interface AccountRegistrationTask {
  id: string;
  batch_id: string;
  account_id: string;
  email: string;
  nickname: string;
  status: string;
  priority: number;
  attempts: number;
  max_attempts: number;
  next_attempt_at: string;
  lease_node_id: string | null;
  lease_session_id: string | null;
  lease_execution_id: string | null;
  lease_expires_at: string | null;
  stage: string;
  stage_version: number;
  last_error: string | null;
  cancel_requested: boolean;
  created_at: string;
  updated_at: string;
}

export interface ManualAction {
  id: string;
  task_type: string;
  registration_task_id: string | null;
  book_task_id: string | null;
  execution_id: string | null;
  node_id: string | null;
  session_id: string | null;
  action_type: string;
  prompt: string;
  status: string;
  artifact_url: string | null;
  expires_at: string;
  resolved_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface TaskExecution {
  id: string;
  task_id: string | null;
  account_registration_task_id: string | null;
  session_id: string | null;
  node_id: string | null;
  slot_index: number | null;
  account_id: string | null;
  proxy_id: string | null;
  task_type: string;
  attempt: number;
  stage_version: number;
  result: string | null;
  error: string | null;
  duration_ms: number | null;
  started_at: string;
  finished_at: string | null;
}

// ---------------------------------------------------------------- 图书馆总库与索引 V1

export interface CatalogStats {
  total_sources: number;
  total_source_records: number;
  total_works: number;
  total_editions: number;
  total_chapters: number;
  total_holdings: number;
  total_library_files: number;
  total_library_bytes: number;
  acquired_targets: number;
  pending_targets: number;
  downloading_targets: number;
  failed_targets: number;
  needs_confirm_targets: number;
  total_quarantined: number;
  missing_isbn_count: number;
  missing_author_count: number;
  ambiguous_works_count: number;
  today_downloaded_count?: number;
  today_added_works_count?: number;
}

export interface EditionSearchItem {
  id: string;
  work_id: string;
  work_type: string;
  title: string;
  authors: string[];
  publisher: string | null;
  publish_year: number | null;
  language: string;
  identifiers: string[];
  source_formats: string[];
  holding_formats: string[];
  acquisition_status: string;
  worker_name?: string | null;
  acquisition_stage?: string;
  attempts?: number;
  max_attempts?: number;
  next_attempt_at?: string | null;
  last_error?: string | null;
  resolution_status: string;
  updated_at: string;
}

export interface FacetCount {
  key: string;
  count: number;
}

export interface CatalogSearchResponse {
  items: EditionSearchItem[];
  total: number;
  limit: number;
  offset: number;
  next_cursor?: string | null;
  previous_cursor?: string | null;
  status_facets: FacetCount[];
  language_facets: FacetCount[];
  format_facets: FacetCount[];
}

export interface MailProviderConfig {
  provider_type: "manual" | "outlook_http" | "mock" | string;
  endpoint: string;
  has_api_key: boolean;
  poll_interval_secs: number;
  timeout_secs: number;
  allowed_hosts: string[];
  allowed_senders: string[];
  version: number;
  is_active: boolean;
  updated_by: string;
  updated_at: string;
}

export interface UpdateMailProviderPayload {
  provider_type: string;
  endpoint: string;
  api_key?: string;
  poll_interval_secs?: number;
  timeout_secs?: number;
  allowed_hosts?: string[];
  allowed_senders?: string[];
}

export interface MailProviderStatus {
  provider_type: "manual" | "outlook_http" | "mock" | string;
  version: number;
  is_active: boolean;
  has_api_key: boolean;
  health: string;
  workers_applied: number;
  workers_online: number;
}

export interface TestMailProviderPayload {
  provider_type: string;
  endpoint: string;
  api_key?: string;
  allowed_hosts?: string[];
}

export interface TestMailProviderResult {
  success: boolean;
  message: string;
  latency_ms?: number | null;
}

export interface WorkRow {
  id: string;
  work_type: string;
  preferred_title: string;
  normalized_title: string;
  primary_language: string;
  parent_work_id: string | null;
  resolution_status: string;
  merged_into_work_id: string | null;
  created_at: string;
  updated_at: string;
}

export interface EditionRow {
  id: string;
  work_id: string;
  edition_title: string;
  language: string;
  publisher: string | null;
  publish_year: number | null;
  publish_date_text: string | null;
  edition_number: string | null;
  intro: string | null;
  format_summary: string | null;
  status: string;
  created_at: string;
  updated_at: string;
}

export interface IdentifierRow {
  id: string;
  object_type: string;
  object_id: string;
  identifier_type: string;
  raw_value: string;
  normalized_value: string;
  is_valid: boolean;
  created_at: string;
}

export interface ContributorRow {
  id: string;
  name: string;
  normalized_name: string;
  created_at: string;
}

export interface SubjectRow {
  id: string;
  subject_type: string;
  code: string | null;
  name: string;
  created_at: string;
}

export interface SourceRecordRow {
  id: string;
  source_id: string;
  import_file_id: string;
  external_id: string | null;
  sheet_name: string;
  row_number: number;
  raw_payload: Record<string, any>;
  normalized_title: string;
  normalized_author: string | null;
  normalized_publisher: string | null;
  raw_isbn: string | null;
  raw_doi: string | null;
  raw_year: string | null;
  raw_language: string | null;
  raw_category: string | null;
  import_version: string;
  created_at: string;
}

export interface SourceAssetRow {
  id: string;
  source_record_id: string;
  format: string;
  declared_size_bytes: number | null;
  md5: string | null;
  download_url: string | null;
  status: string;
  created_at: string;
}

export interface LibraryFileRow {
  id: string;
  storage_backend: string;
  object_key: string;
  format: string;
  actual_size_bytes: number;
  sha256: string;
  md5: string | null;
  verify_status: string;
  verified_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface HoldingRow {
  id: string;
  edition_id: string;
  library_file_id: string;
  source_asset_id: string | null;
  match_type: string;
  meets_strategy: boolean;
  created_at: string;
}

export interface AcquisitionTargetRow {
  id: string;
  edition_id: string;
  preferred_formats: string[];
  status: string;
  priority: number;
  attempts: number;
  max_attempts: number;
  next_attempt_at: string;
  lease_node_id: string | null;
  lease_session_id: string | null;
  lease_execution_id: string | null;
  lease_expires_at: string | null;
  active_source_asset_id: string | null;
  satisfied_holding_id: string | null;
  last_error: string | null;
  created_at: string;
  updated_at: string;
}

export interface AcquisitionExecutionRow {
  id: string;
  target_id: string;
  source_asset_id: string | null;
  node_id: string | null;
  session_id: string | null;
  slot_index: number | null;
  stage: string;
  result: string | null;
  error_code: string | null;
  error_message: string | null;
  started_at: string;
  finished_at: string | null;
}

export interface EditionDetail {
  edition: EditionRow;
  work: WorkRow;
  sibling_editions: EditionRow[];
  identifiers: IdentifierRow[];
  contributors: ContributorRow[];
  subjects: SubjectRow[];
  source_records: SourceRecordRow[];
  source_assets: SourceAssetRow[];
  holdings: [HoldingRow, LibraryFileRow][];
  acquisition_target: AcquisitionTargetRow | null;
  executions: AcquisitionExecutionRow[];
}

export interface CatalogSource {
  id: string;
  name: string;
  source_type: string;
  description: string | null;
  priority: number;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface ImportRun {
  id: string;
  import_file_id: string;
  status: string;
  checkpoint_row: number;
  total_rows: number;
  imported_count: number;
  quarantined_count: number;
  duplicate_count: number;
  error_summary: string | null;
  started_at: string | null;
  completed_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface QuarantinedRecord {
  id: string;
  import_run_id: string | null;
  import_file_id: string;
  sheet_name: string;
  row_number: number;
  raw_content: Record<string, any>;
  error_reason: string;
  resolved: boolean;
  resolved_at: string | null;
  created_at: string;
}

export interface ImportPreviewResult {
  source_id: string;
  source_name: string;
  detected_structure: string;
  column_mapping: Record<string, string>;
  total_rows: number;
  sample_rows: Array<{
    line: number;
    title: string;
    author?: string;
    publisher?: string;
    isbn?: string;
    doi?: string;
    year?: string;
    format?: string;
    md5?: string;
  }>;
  file_sha256: string;
  is_duplicate_file: boolean;
}

export interface ImportExecutionResult {
  run_id: string;
  import_file_id: string;
  total_rows: number;
  imported_count: number;
  duplicate_count: number;
  quarantined_count: number;
  status: string;
}

// ---------------------------------------------------------------- 统一错误

/** API 错误：优先展示后端中文 message，其次稳定的 code。 */
export class ApiError extends Error {
  code: string;
  status: number;
  request_id?: string;
  constructor(message: string, status: number, code = "unknown", requestId?: string) {
    super(message);
    this.name = "ApiError";
    this.code = code;
    this.status = status;
    this.request_id = requestId;
  }
}
