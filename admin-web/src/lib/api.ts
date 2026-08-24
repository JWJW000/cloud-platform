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

export async function listAccountRegistrationBatches(params?: { limit?: number }): Promise<BatchWithProgress[]> {
  return api.get<BatchWithProgress[]>("/api/account-registration-batches", params);
}

export async function createAccountRegistrationBatch(data: {
  name: string;
  source_file?: string;
  priority?: number;
  account_ids: string[];
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
