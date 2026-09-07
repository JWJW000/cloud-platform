import type { CatalogStats } from "../lib/types";
import { formatTime } from "../lib/format";

export function StatsSnapshotStatus({ stats, error }: { stats: CatalogStats | null; error: string | null }) {
  return (
    <p role="status" className="text-xs text-slate-500">
      {stats?.computed_at ? `统计更新时间：${formatTime(stats.computed_at)}` : stats ? "统计每五分钟更新" : "统计生成中，完成后将自动显示"}
      {stats?.stale && " · 当前为上次成功结果，等待后台更新"}
      {error && ` · ${error}，稍后自动重试`}
    </p>
  );
}
