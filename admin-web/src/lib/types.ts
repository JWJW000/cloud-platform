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
  value: string;
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
