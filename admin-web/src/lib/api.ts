// 统一 API 客户端（V5 方案第 5.5 节）。
//
// 约定：
// - 会话凭据走 Cookie（HttpOnly），fetch 一律 `credentials: "same-origin"`；
// - 写操作携带 `X-Requested-With: fetch` 头（Master CSRF 中间件要求）；
// - 4xx/5xx 统一抛 [`ApiError`]，message 优先取后端中文 message；
// - 401 触发全局会话失效回调（AuthProvider 注册）；
// - 409 携带资源状态冲突提示，页面据此刷新资源；
// - 422 携带字段错误列表（errors），表单页逐字段展示。

import { ApiError } from "./types";

let onUnauthorized: (() => void) | null = null;

export function setUnauthorizedHandler(handler: (() => void) | null) {
  onUnauthorized = handler;
}

interface ErrorBody {
  code?: string;
  message?: string;
  request_id?: string;
  errors?: Record<string, string>;
}

export interface ApiResult<T> {
  data: T;
}

/** 解析错误响应体（后端统一 {code, message, request_id, errors}）。 */
async function parseErrorBody(resp: Response): Promise<ErrorBody> {
  try {
    const body = (await resp.json()) as ErrorBody;
    return body ?? {};
  } catch {
    return {};
  }
}

/** 组装稳定的 ApiError。 */
async function toApiError(resp: Response): Promise<ApiError> {
  const body = await parseErrorBody(resp);
  const message = body.message?.trim() || defaultMessage(resp.status);
  return new ApiError(message, resp.status, body.code || `http_${resp.status}`, body.request_id);
}

function defaultMessage(status: number): string {
  switch (status) {
    case 400:
      return "请求参数错误";
    case 401:
      return "登录已失效，请重新登录";
    case 403:
      return "权限不足";
    case 404:
      return "资源不存在";
    case 409:
      return "资源状态冲突，请刷新后重试";
    case 422:
      return "提交内容有误，请检查表单";
    case 429:
      return "操作过于频繁，请稍后再试";
    default:
      return `请求失败（HTTP ${status}）`;
  }
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const isFormData = typeof FormData !== "undefined" && init.body instanceof FormData;
  const defaultHeaders: Record<string, string> = {
    "X-Requested-With": "fetch",
  };
  if (init.body && !isFormData) {
    defaultHeaders["Content-Type"] = "application/json";
  }

  const resp = await fetch(path, {
    ...init,
    credentials: "same-origin",
    headers: {
      ...defaultHeaders,
      ...(init.headers ?? {}),
    },
  });

  if (resp.status === 401) {
    // 全局会话失效：通知 AuthProvider 清理登录态
    const err = await toApiError(resp);
    onUnauthorized?.();
    throw err;
  }
  if (!resp.ok) {
    throw await toApiError(resp);
  }
  if (resp.status === 204) {
    return undefined as T;
  }
  return (await resp.json()) as T;
}

export const api = {
  get<T>(path: string, params?: Record<string, string | number | boolean | undefined>) {
    const qs = params
      ? "?" +
        Object.entries(params)
          .filter(([, v]) => v !== undefined && v !== null && v !== "")
          .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`)
          .join("&")
      : "";
    return request<T>(`${path}${qs}`);
  },

  post<T>(path: string, body?: unknown) {
    return request<T>(path, {
      method: "POST",
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  },

  postForm<T>(path: string, formData: FormData) {
    return request<T>(path, {
      method: "POST",
      body: formData,
    });
  },

  put<T>(path: string, body?: unknown) {
    return request<T>(path, {
      method: "PUT",
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  },

  patch<T>(path: string, body?: unknown) {
    return request<T>(path, {
      method: "PATCH",
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  },

  delete<T>(path: string) {
    return request<T>(path, { method: "DELETE" });
  },
};

// ---------------------------------------------------------------- 鉴权

export interface LoginPayload {
  username: string;
  password: string;
}

export interface LoginResponse {
  user: User;
}

import type { User } from "./types";

export async function login(payload: LoginPayload): Promise<User> {
  const resp = await api.post<LoginResponse>("/api/auth/login", payload);
  return resp.user;
}

export async function fetchMe(): Promise<User> {
  return api.get<User>("/api/auth/me");
}

export async function logout(): Promise<void> {
  await api.post<void>("/api/auth/logout");
}

export async function changePassword(payload: {
  old_password: string;
  new_password: string;
}): Promise<void> {
  await api.put<void>("/api/auth/password", payload);
}

// ---------------------------------------------------------------- V6 导入与业务任务 API

import type {
  AccountImportPreview,
  AccountRegistrationBatch,
  AccountRegistrationTask,
  Batch,
  BatchProgress,
  BatchWithProgress,
  BookImportPreview,
  CommitAccountsRequest,
  CommitAccountsResponse,
  CommitBooksRequest,
  CommitBooksResponse,
  ManualAction,
  OutlookPreviewResponse,
  OutlookSyncRequest,
  OutlookSyncResponse,
} from "./types";

export async function previewBooksImport(formData: FormData): Promise<BookImportPreview> {
  return api.postForm<BookImportPreview>("/api/imports/books/preview", formData);
}

export async function commitBooksImport(req: CommitBooksRequest): Promise<CommitBooksResponse> {
  return api.post<CommitBooksResponse>("/api/imports/books/commit", req);
}

export async function previewAccountsImport(formData: FormData): Promise<AccountImportPreview> {
  return api.postForm<AccountImportPreview>("/api/imports/accounts/preview", formData);
}

export async function commitAccountsImport(req: CommitAccountsRequest): Promise<CommitAccountsResponse> {
  return api.post<CommitAccountsResponse>("/api/imports/accounts/commit", req);
}

export async function deleteImportJob(id: string): Promise<void> {
  await api.delete<void>(`/api/imports/${id}`);
}

export async function previewOutlookAccounts(): Promise<OutlookPreviewResponse> {
  return api.post<OutlookPreviewResponse>("/api/accounts/outlook/preview");
}

export async function syncOutlookAccounts(req: OutlookSyncRequest): Promise<OutlookSyncResponse> {
  return api.post<OutlookSyncResponse>("/api/accounts/outlook/sync", req);
}

export async function listAccountRegistrationBatches(params?: { limit?: number }): Promise<BatchWithProgress[]> {
  return api.get<BatchWithProgress[]>("/api/account-registration-batches", params);
}

export async function createAccountRegistrationBatch(data: {
  name: string;
  source_file?: string;
  priority?: number;
  account_ids?: string[];
  include_all_pending?: boolean;
  start_immediately?: boolean;
}): Promise<AccountRegistrationBatch> {
  return api.post<AccountRegistrationBatch>("/api/account-registration-batches", data);
}

export async function getAccountRegistrationBatch(id: string): Promise<BatchWithProgress> {
  return api.get<BatchWithProgress>(`/api/account-registration-batches/${id}`);
}

export async function startAccountRegistrationBatch(id: string): Promise<AccountRegistrationBatch> {
  return api.post<AccountRegistrationBatch>(`/api/account-registration-batches/${id}/start`);
}

export async function pauseAccountRegistrationBatch(id: string): Promise<AccountRegistrationBatch> {
  return api.post<AccountRegistrationBatch>(`/api/account-registration-batches/${id}/pause`);
}

export async function resumeAccountRegistrationBatch(id: string): Promise<AccountRegistrationBatch> {
  return api.post<AccountRegistrationBatch>(`/api/account-registration-batches/${id}/resume`);
}

export async function cancelAccountRegistrationBatch(id: string): Promise<AccountRegistrationBatch> {
  return api.post<AccountRegistrationBatch>(`/api/account-registration-batches/${id}/cancel`);
}

export async function setAccountRegistrationBatchPriority(id: string, priority: number): Promise<AccountRegistrationBatch> {
  return api.patch<AccountRegistrationBatch>(`/api/account-registration-batches/${id}/priority`, { priority });
}

export async function listAccountRegistrationBatchTasks(
  batchId: string,
  params?: { status?: string; limit?: number; offset?: number }
): Promise<AccountRegistrationTask[]> {
  return api.get<AccountRegistrationTask[]>(`/api/account-registration-batches/${batchId}/tasks`, params);
}

export async function listAccountRegistrationTasks(params?: {
  status?: string;
  limit?: number;
  offset?: number;
}): Promise<AccountRegistrationTask[]> {
  return api.get<AccountRegistrationTask[]>("/api/account-registration-tasks", params);
}

export async function retryAccountRegistrationTask(id: string): Promise<AccountRegistrationTask> {
  return api.post<AccountRegistrationTask>(`/api/account-registration-tasks/${id}/retry`);
}

export async function cancelAccountRegistrationTask(id: string): Promise<AccountRegistrationTask> {
  return api.post<AccountRegistrationTask>(`/api/account-registration-tasks/${id}/cancel`);
}

export async function listManualActions(params?: { status?: string; limit?: number }): Promise<ManualAction[]> {
  return api.get<ManualAction[]>("/api/manual-actions", params);
}

export async function getManualAction(id: string): Promise<ManualAction> {
  return api.get<ManualAction>(`/api/manual-actions/${id}`);
}

export async function resolveManualAction(id: string, input_code: string): Promise<ManualAction> {
  return api.post<ManualAction>(`/api/manual-actions/${id}/resolve`, { input_code });
}

export async function cancelManualAction(id: string): Promise<ManualAction> {
  return api.post<ManualAction>(`/api/manual-actions/${id}/cancel`);
}

export async function listBatches(): Promise<Batch[]> {
  return api.get<Batch[]>("/api/batches");
}

export async function getGlobalDownloadControl(): Promise<import("./types").GlobalDownloadControl> {
  return api.get<import("./types").GlobalDownloadControl>("/api/download-control");
}

export async function updateGlobalDownloadControl(
  paused: boolean,
): Promise<import("./types").GlobalDownloadControl> {
  return api.put<import("./types").GlobalDownloadControl>("/api/download-control", { paused });
}

export async function getBatch(id: string): Promise<Batch> {
  return api.get<Batch>(`/api/batches/${id}`);
}

export async function getBatchProgress(id: string): Promise<BatchProgress> {
  return api.get<BatchProgress>(`/api/batches/${id}/progress`);
}

export async function startBatch(id: string): Promise<Batch> {
  return api.post<Batch>(`/api/batches/${id}/start`);
}

export async function pauseBatch(id: string): Promise<Batch> {
  return api.post<Batch>(`/api/batches/${id}/pause`);
}

export async function resumeBatch(id: string): Promise<Batch> {
  return api.post<Batch>(`/api/batches/${id}/resume`);
}

export async function cancelBatch(id: string): Promise<Batch> {
  return api.post<Batch>(`/api/batches/${id}/cancel`);
}

// ---------------------------------------------------------------- 馆藏扫描与审核

export async function listStorageLocations(): Promise<{ success: boolean; locations: import("./types").StorageLocation[] }> {
  return api.get<{ success: boolean; locations: import("./types").StorageLocation[] }>("/api/catalog/storage-locations");
}

export async function listInventoryScans(): Promise<{ success: boolean; jobs: import("./types").InventoryScanJob[] }> {
  return api.get<{ success: boolean; jobs: import("./types").InventoryScanJob[] }>("/api/catalog/inventory/scans");
}

export async function createInventoryScan(data: {
  node_id: string;
  storage_location_id: string;
  scan_mode: string;
}): Promise<{ success: boolean; job: import("./types").InventoryScanJob }> {
  return api.post<{ success: boolean; job: import("./types").InventoryScanJob }>("/api/catalog/inventory/scans", data);
}

export async function cancelInventoryScan(id: string): Promise<{ success: boolean; message: string }> {
  return api.post<{ success: boolean; message: string }>(`/api/catalog/inventory/scans/${id}/cancel`);
}

export async function listInventoryReviews(): Promise<{ success: boolean; reviews: import("./types").InventoryReviewDetail[] }> {
  return api.get<{ success: boolean; reviews: import("./types").InventoryReviewDetail[] }>("/api/catalog/inventory/reviews");
}

export async function confirmInventoryReview(
  id: string,
  edition_id: string
): Promise<{ success: boolean; message: string }> {
  return api.post<{ success: boolean; message: string }>(`/api/catalog/inventory/reviews/${id}/confirm`, {
    edition_id,
  });
}

export async function ignoreInventoryReview(id: string): Promise<{ success: boolean; message: string }> {
  return api.post<{ success: boolean; message: string }>(`/api/catalog/inventory/reviews/${id}/ignore`);
}

export async function recomputeInventoryState(edition_id: string): Promise<{ success: boolean; status: string }> {
  return api.post<{ success: boolean; status: string }>(`/api/catalog/inventory/recompute/${edition_id}`);
}


// ---------------------------------------------------------------- 图书馆总库与索引 V1 接口

export async function getCatalogStats(): Promise<import("./types").CatalogStats> {
  return api.get<import("./types").CatalogStats>("/api/catalog/stats");
}

export async function searchCatalog(params: {
  query?: string;
  acquisition_status?: string;
  work_type?: string;
  language?: string;
  format?: string;
  resolution_status?: string;
  limit?: number;
  offset?: number;
  cursor?: string;
}): Promise<import("./types").CatalogSearchResponse> {
  return api.get<import("./types").CatalogSearchResponse>("/api/catalog/search", params);
}

export async function getCatalogEdition(id: string): Promise<import("./types").EditionDetail> {
  return api.get<import("./types").EditionDetail>(`/api/catalog/editions/${id}`);
}

export async function listCatalogSources(): Promise<import("./types").CatalogSource[]> {
  return api.get<import("./types").CatalogSource[]>("/api/catalog/sources");
}

export async function createCatalogSource(data: {
  name: string;
  source_type?: string;
  description?: string;
  priority?: number;
}): Promise<import("./types").CatalogSource> {
  return api.post<import("./types").CatalogSource>("/api/catalog/sources", data);
}

export async function previewCatalogImport(data: {
  source_name: string;
  source_type?: string;
  file_name: string;
  sheet_name?: string;
  text_content?: string;
  server_manifest?: string;
}): Promise<import("./types").ImportPreviewResult> {
  return api.post<import("./types").ImportPreviewResult>("/api/catalog/imports/preview", data);
}

export async function submitCatalogImport(data: {
  source_name: string;
  source_type?: string;
  file_name: string;
  sheet_name?: string;
  text_content?: string;
  server_manifest?: string;
}): Promise<import("./types").ImportExecutionResult> {
  return api.post<import("./types").ImportExecutionResult>("/api/catalog/imports/submit", data);
}

export async function listCatalogServerManifests(): Promise<Array<{ id: string; size_bytes: number }>> {
  return api.get<Array<{ id: string; size_bytes: number }>>("/api/catalog/imports/manifests");
}

export async function listCatalogImportRuns(): Promise<import("./types").ImportRun[]> {
  return api.get<import("./types").ImportRun[]>("/api/catalog/imports/runs");
}

export async function getCatalogImportRun(id: string): Promise<import("./types").ImportRun> {
  return api.get<import("./types").ImportRun>(`/api/catalog/imports/runs/${id}`);
}

export async function listCatalogQuarantined(): Promise<import("./types").QuarantinedRecord[]> {
  return api.get<import("./types").QuarantinedRecord[]>("/api/catalog/imports/quarantine");
}

export async function resolveCatalogQuarantine(
  id: string,
  data: {
    corrected_title?: string;
    corrected_author?: string;
    corrected_publisher?: string;
    corrected_isbn?: string;
  }
): Promise<{ success: boolean; work_id: string; edition_id: string }> {
  return api.post<{ success: boolean; work_id: string; edition_id: string }>(
    `/api/catalog/imports/quarantine/${id}/resolve`,
    data
  );
}

export async function listCatalogAcquisitions(params: {
  query?: string;
  acquisition_status?: string;
  format?: string;
  limit?: number;
  offset?: number;
  cursor?: string;
}): Promise<import("./types").CatalogSearchResponse> {
  return api.get<import("./types").CatalogSearchResponse>("/api/catalog/acquisitions", params);
}

export async function retryCatalogAcquisition(id: string): Promise<{ success: boolean }> {
  return api.post<{ success: boolean }>(`/api/catalog/acquisitions/${id}/retry`);
}

export async function updateCatalogAcquisitionPriority(id: string, priority: number): Promise<{ success: boolean }> {
  return api.post<{ success: boolean }>(`/api/catalog/acquisitions/${id}/priority`, { priority });
}

export async function mergeCatalogWorks(source_work_id: string, target_work_id: string): Promise<{ success: boolean }> {
  return api.post<{ success: boolean }>("/api/catalog/resolutions/merge", { source_work_id, target_work_id });
}

export interface MergeImpactItem {
  work_id: string;
  title: string;
  editions: number;
  source_records: number;
  holdings: number;
}

export async function previewCatalogWorksMerge(
  source_work_id: string,
  target_work_id: string,
): Promise<{ source: MergeImpactItem; target: MergeImpactItem }> {
  return api.get("/api/catalog/resolutions/merge-preview", { source_work_id, target_work_id });
}

// ---------------------------------------------------------------- 邮件验证码 Provider 接口

export async function getMailProviderConfig(): Promise<import("./types").MailProviderConfig | null> {
  return api.get<import("./types").MailProviderConfig | null>("/api/settings/mail-provider");
}

export async function getMailProviderStatus(): Promise<import("./types").MailProviderStatus | null> {
  return api.get<import("./types").MailProviderStatus | null>("/api/mail-provider/status");
}

export async function updateMailProviderConfig(
  data: import("./types").UpdateMailProviderPayload
): Promise<import("./types").MailProviderConfig> {
  return api.put<import("./types").MailProviderConfig>("/api/settings/mail-provider", data);
}

export async function testMailProvider(
  data: import("./types").TestMailProviderPayload
): Promise<import("./types").TestMailProviderResult> {
  return api.post<import("./types").TestMailProviderResult>("/api/settings/mail-provider/test", data);
}

// ---------------------------------------------------------------- Webhook 接口

export async function getWebhookDetails(): Promise<import("./types").WebhookDetailsResponse> {
  return api.get<import("./types").WebhookDetailsResponse>("/api/settings/webhook");
}

export async function updateWebhookConfig(
  data: import("./types").WebhookConfig
): Promise<import("./types").WebhookConfig> {
  return api.put<import("./types").WebhookConfig>("/api/settings/webhook", data);
}

export async function sendWebhookManual(
  data?: { custom_note?: string }
): Promise<import("./types").SendWebhookResponse> {
  return api.post<import("./types").SendWebhookResponse>("/api/settings/webhook/send", data ?? {});
}
