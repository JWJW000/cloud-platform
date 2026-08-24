// V6 导入、账号注册批次与人工确认接口测试。
import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  previewBooksImport,
  commitBooksImport,
  previewAccountsImport,
  commitAccountsImport,
  startAccountRegistrationBatch,
  resolveManualAction,
} from "../lib/api";

describe("V6 导入与业务任务接口测试", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("图书预检调用 /api/imports/books/preview 且发送 FormData", async () => {
    const mockPreview = {
      import_token: "tok_123",
      file_name: "test.csv",
      file_sha256: "abc",
      total_rows: 10,
      valid_rows: 10,
      duplicate_in_file: 0,
      duplicate_in_library: 0,
      already_ingested: 0,
      error_rows: 0,
      warnings: [],
      preview: [],
    };

    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce({
      ok: true,
      status: 200,
      json: async () => mockPreview,
    } as Response);

    const fd = new FormData();
    fd.append("file", new Blob(["书名\n测试书"]), "test.csv");

    const res = await previewBooksImport(fd);
    expect(fetchSpy).toHaveBeenCalledWith("/api/imports/books/preview", expect.objectContaining({
      method: "POST",
    }));
    expect(res.import_token).toBe("tok_123");
    expect(res.valid_rows).toBe(10);
  });

  it("图书提交创建批次调用 /api/imports/books/commit", async () => {
    const mockCommit = {
      batch: {
        id: "batch-1",
        name: "批次1",
        source_file: "test.csv",
        status: "待开始",
        priority: 10,
        download_format: "pdf",
        created_at: "2026-08-23T00:00:00Z",
        updated_at: "2026-08-23T00:00:00Z",
      },
      deduplicated: 2,
      already_ingested: 1,
    };

    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce({
      ok: true,
      status: 200,
      json: async () => mockCommit,
    } as Response);

    const res = await commitBooksImport({
      import_token: "tok_123",
      start_immediately: true,
    });

    expect(fetchSpy).toHaveBeenCalledWith("/api/imports/books/commit", expect.objectContaining({
      method: "POST",
      body: JSON.stringify({ import_token: "tok_123", start_immediately: true }),
    }));
    expect(res.batch.id).toBe("batch-1");
  });

  it("账号预检调用 /api/imports/accounts/preview", async () => {
    const mockAccPreview = {
      import_token: "tok_acc_123",
      file_name: "acc.txt",
      file_sha256: "def",
      total_rows: 5,
      valid_rows: 5,
      duplicate_in_file: 0,
      duplicate_in_library: 0,
      error_rows: 0,
      warnings: [],
      preview: [
        {
          line: 1,
          email_masked: "u***r@test.com",
          nickname: "user",
          password_provided: true,
          status: "有效待导入",
          reason: null,
        },
      ],
    };

    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce({
      ok: true,
      status: 200,
      json: async () => mockAccPreview,
    } as Response);

    const fd = new FormData();
    fd.append("file", new Blob(["u@test.com----pwd"]), "acc.txt");

    const res = await previewAccountsImport(fd);
    expect(fetchSpy).toHaveBeenCalledWith("/api/imports/accounts/preview", expect.objectContaining({
      method: "POST",
    }));
    expect(res.preview[0].password_provided).toBe(true);
  });

  it("账号提交支持创建注册批次", async () => {
    const mockCommit = {
      imported_accounts: 5,
      registration_batch: {
        id: "reg-batch-1",
        name: "注册批次",
        source_file: "acc.txt",
        status: "待开始",
        priority: 10,
        created_at: "2026-08-23T00:00:00Z",
        updated_at: "2026-08-23T00:00:00Z",
      },
    };

    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce({
      ok: true,
      status: 200,
      json: async () => mockCommit,
    } as Response);

    const res = await commitAccountsImport({
      import_token: "tok_acc_123",
      mode: "待注册",
      create_registration_batch: true,
      batch_name: "注册批次",
      priority: 10,
      start_immediately: false,
    });

    expect(fetchSpy).toHaveBeenCalledWith("/api/imports/accounts/commit", expect.objectContaining({
      method: "POST",
    }));
    expect(res.registration_batch?.id).toBe("reg-batch-1");
  });

  it("账号注册批次启动调用 /api/account-registration-batches/:id/start", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce({
      ok: true,
      status: 200,
      json: async () => ({ id: "reg-1", status: "执行中" }),
    } as Response);

    await startAccountRegistrationBatch("reg-1");
    expect(fetchSpy).toHaveBeenCalledWith("/api/account-registration-batches/reg-1/start", expect.objectContaining({
      method: "POST",
    }));
  });

  it("人工确认验证码解决调用 /api/manual-actions/:id/resolve", async () => {
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce({
      ok: true,
      status: 200,
      json: async () => ({ id: "act-1", status: "已解决" }),
    } as Response);

    await resolveManualAction("act-1", "123456");
    expect(fetchSpy).toHaveBeenCalledWith("/api/manual-actions/act-1/resolve", expect.objectContaining({
      method: "POST",
      body: JSON.stringify({ input_code: "123456" }),
    }));
  });
});
