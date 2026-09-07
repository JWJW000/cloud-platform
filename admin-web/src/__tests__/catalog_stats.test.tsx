import { act, renderHook, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useCatalogStats } from "../hooks/useCatalogStats";
import { StatsSnapshotStatus } from "../components/StatsSnapshotStatus";
import * as api from "../lib/api";
import type { CatalogStats } from "../lib/types";

const snapshot = { ready: true, computed_at: "2026-09-07T00:00:00Z", stale: false, total_editions: 42 } as CatalogStats;
afterEach(() => vi.useRealTimers());

describe("统计快照读取", () => {
  it("首份统计未完成时不显示零值，自动重试，卸载后停止", async () => {
    vi.useFakeTimers();
    const read = vi.spyOn(api, "getCatalogStats")
      .mockResolvedValueOnce({ ...snapshot, ready: false, computed_at: null, total_editions: 0 })
      .mockResolvedValue(snapshot);
    const { result, unmount } = renderHook(useCatalogStats);
    await act(async () => {});
    expect(result.current.stats).toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
    expect(result.current.stats?.total_editions).toBe(42);
    expect(read).toHaveBeenCalledTimes(2);
    unmount();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("刷新失败保留上一份成功数据，并在后续轮询恢复", async () => {
    vi.useFakeTimers();
    vi.spyOn(api, "getCatalogStats").mockResolvedValueOnce(snapshot)
      .mockRejectedValueOnce(new Error("网络暂不可用"))
      .mockResolvedValue({ ...snapshot, total_editions: 43 });
    const { result } = renderHook(useCatalogStats);
    await act(async () => {});
    await act(async () => { await vi.advanceTimersByTimeAsync(30_000); });
    expect(result.current.stats?.total_editions).toBe(42);
    expect(result.current.error).toBe("网络暂不可用");
    await act(async () => { await vi.advanceTimersByTimeAsync(30_000); });
    expect(result.current.stats?.total_editions).toBe(43);
    expect(result.current.error).toBeNull();
  });

  it("明确展示快照时间及过期提示", () => {
    render(<StatsSnapshotStatus stats={{ ...snapshot, stale: true }} error={null} />);
    expect(screen.getByRole("status")).toHaveTextContent("统计更新时间");
    expect(screen.getByRole("status")).toHaveTextContent("上次成功结果");
  });
});
