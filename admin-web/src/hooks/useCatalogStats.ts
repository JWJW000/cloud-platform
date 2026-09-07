import { useEffect, useState } from "react";
import { getCatalogStats } from "../lib/api";
import type { CatalogStats } from "../lib/types";

/** 统计独立刷新；首次生成不重复请求页面中的列表、导入记录或任务数据。 */
export function useCatalogStats() {
  const [stats, setStats] = useState<CatalogStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const read = async () => {
      let delay = 30_000;
      try {
        const next = await getCatalogStats();
        if (disposed) return;
        setError(null);
        if (next.ready !== false) setStats(next);
        if (next.ready === false || next.stale) delay = 5_000;
      } catch (caught) {
        if (disposed) return;
        setError(caught instanceof Error ? caught.message : "读取统计失败");
      }
      if (!disposed) timer = setTimeout(read, delay);
    };
    void read();
    return () => { disposed = true; clearTimeout(timer); };
  }, []);
  return { stats, error };
}
