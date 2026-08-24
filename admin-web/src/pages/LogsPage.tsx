// 操作日志页：按级别/来源筛选 + 列表。
import { useState } from "react";
import { useApi } from "../hooks/useApi";
import { api } from "../lib/api";
import type { LogEntry } from "../lib/types";
import { formatTime } from "../lib/format";
import {
  Badge,
  Button,
  Card,
  EmptyRow,
  ErrorBox,
  Select,
  Spinner,
  Table,
  Td,
} from "../components/ui";

const LEVELS = ["全部", "信息", "警告", "错误"];

export function LogsPage() {
  const [level, setLevel] = useState("全部");
  const { data, loading, error, reload } = useApi<LogEntry[]>(
    () => api.get("/api/logs", { level: level === "全部" ? undefined : level, limit: 200 }),
    [level],
  );

  if (loading) return <Spinner label="正在加载日志..." />;
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">操作日志</h2>
          <p className="text-sm text-slate-500">管理员操作与系统事件的审计记录</p>
        </div>
        <div className="flex items-center gap-2">
          <Select value={level} onChange={(e) => setLevel(e.target.value)} aria-label="日志级别筛选">
            {LEVELS.map((l) => (
              <option key={l} value={l}>
                {l === "全部" ? "全部级别" : l}
              </option>
            ))}
          </Select>
          <Button variant="secondary" size="sm" onClick={reload}>
            刷新
          </Button>
        </div>
      </div>
      <Card>
        <Table
          headers={["时间", "级别", "来源", "操作人", "动作", "目标", "详情"]}
          empty={!data || data.length === 0 ? <EmptyRow colSpan={7} text="暂无日志" /> : undefined}
        >
          {(data ?? []).map((l) => (
            <tr key={l.id}>
              <Td className="whitespace-nowrap text-xs text-slate-500">{formatTime(l.created_at)}</Td>
              <Td>
                <Badge
                  className={
                    l.level === "错误"
                      ? "bg-red-100 text-red-700"
                      : l.level === "警告"
                        ? "bg-amber-100 text-amber-700"
                        : "bg-slate-100 text-slate-600"
                  }
                >
                  {l.level}
                </Badge>
              </Td>
              <Td className="text-xs text-slate-500">{l.source}</Td>
              <Td className="text-xs text-slate-600">{l.actor}</Td>
              <Td className="text-xs font-medium text-slate-700">{l.action}</Td>
              <Td className="max-w-40 truncate font-mono text-xs text-slate-500" title={l.target}>
                {l.target || "-"}
              </Td>
              <Td className="max-w-96">
                <div className="truncate text-xs text-slate-500" title={l.detail}>
                  {l.detail}
                </div>
              </Td>
            </tr>
          ))}
        </Table>
      </Card>
    </div>
  );
}
