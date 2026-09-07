// API 客户端测试：错误映射、422 字段错误、401 会话失效回调（V5 第 11.1 节）。
import { describe, expect, it, vi } from "vitest";
import { api, setUnauthorizedHandler } from "../lib/api";

function mockFetch(status: number, body: unknown, headers?: Record<string, string>) {
  return vi.spyOn(globalThis, "fetch").mockResolvedValue({
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
    headers: new Headers(headers),
  } as Response);
}

describe("api 客户端", () => {
  it("合并相同在途读取，完成后重新查询", async () => {
    const spy = mockFetch(200, { total: 3 });
    const first = api.get("/api/books", { limit: 20, query: "book" });
    const second = api.get("/api/books", { query: "book", limit: 20 });
    expect(spy).toHaveBeenCalledTimes(1);
    expect(await first).toEqual(await second);
    await api.get("/api/books", { query: "book", limit: 20 });
    expect(spy).toHaveBeenCalledTimes(2);
  });

  it("不同参数独立请求，失败后可重试", async () => {
    const spy = mockFetch(500, {});
    await Promise.allSettled([api.get("/api/books", { limit: 1 }), api.get("/api/books", { limit: 2 })]);
    expect(spy).toHaveBeenCalledTimes(2);
    spy.mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    await expect(api.get("/api/books", { limit: 1 })).resolves.toEqual([]);
    expect(spy).toHaveBeenCalledTimes(3);
  });

  it("写后刷新不复用旧读取，旧读取不能删除新读取", async () => {
    let resolveOld!: (value: Response) => void;
    let resolveNew!: (value: Response) => void;
    const spy = mockFetch(200, {});
    spy.mockImplementationOnce(() => new Promise<Response>(resolve => { resolveOld = resolve; }));
    const old = api.get("/api/books");
    await api.post("/api/books", {});
    spy.mockImplementationOnce(() => new Promise<Response>(resolve => { resolveNew = resolve; }));
    const fresh = api.get("/api/books");
    resolveOld({ ok: true, status: 200, json: async () => ["old"] } as Response);
    await old;
    const shared = api.get("/api/books");
    expect(spy).toHaveBeenCalledTimes(3);
    resolveNew({ ok: true, status: 200, json: async () => ["new"] } as Response);
    expect(await fresh).toEqual(["new"]);
    expect(await shared).toEqual(["new"]);
  });

  it("会话切换隔离在途读取", async () => {
    const spy = mockFetch(200, {});
    const first = api.get("/api/auth/me");
    setUnauthorizedHandler(null);
    const second = api.get("/api/auth/me");
    await Promise.all([first, second]);
    expect(spy).toHaveBeenCalledTimes(2);
  });

  it("成功请求返回 JSON 数据", async () => {
    mockFetch(200, { id: "1" });
    const data = await api.get<{ id: string }>("/api/workers/1");
    expect(data.id).toBe("1");
  });

  it("403 抛中文「权限不足」且携带稳定 code", async () => {
    mockFetch(403, { code: "forbidden", message: "权限不足" });
    await expect(api.get("/api/workers")).rejects.toMatchObject({
      status: 403,
      code: "forbidden",
      message: "权限不足",
    });
  });

  it("409 携带资源冲突提示（页面据此刷新资源）", async () => {
    mockFetch(409, { code: "conflict", message: "节点当前注册状态为「已批准」，只有待审核节点可以被拒绝" });
    await expect(api.post("/api/workers/x/reject", { reason: "r" })).rejects.toMatchObject({
      status: 409,
      message: expect.stringContaining("待审核"),
    });
  });

  it("422 展示字段错误提示", async () => {
    mockFetch(422, { code: "validation", message: "提交内容有误", errors: { reason: "拒绝原因不能为空" } });
    await expect(api.post("/api/workers/x/reject", { reason: "" })).rejects.toMatchObject({
      status: 422,
      message: "提交内容有误",
    });
  });

  it("401 触发全局会话失效回调", async () => {
    const handler = vi.fn();
    setUnauthorizedHandler(handler);
    mockFetch(401, { code: "unauthorized", message: "登录已失效" });
    await expect(api.get("/api/overview")).rejects.toMatchObject({ status: 401 });
    expect(handler).toHaveBeenCalledTimes(1);
    setUnauthorizedHandler(null);
  });

  it("后端没有 message 时给出稳定的中文默认文案", async () => {
    mockFetch(500, {});
    await expect(api.get("/api/x")).rejects.toMatchObject({ message: "请求失败（HTTP 500）" });
  });

  it("写操作携带 X-Requested-With 头（CSRF 中间件要求）", async () => {
    const spy = mockFetch(200, {});
    await api.post("/api/workers/x/approve", { configured_slots: 5 });
    const [, init] = spy.mock.calls[0] as [RequestInfo | URL, RequestInit];
    const headers = (init.headers ?? {}) as Record<string, string>;
    expect(headers["X-Requested-With"]).toBe("fetch");
    expect(headers["Content-Type"]).toBe("application/json");
  });
});
