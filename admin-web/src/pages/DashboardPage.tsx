// 总览页：真实 /api/overview 数据 + 卡片 + 最近执行。
import { useApi } from "../hooks/useApi";
import { api } from "../lib/api";
import { formatBytes, formatTime } from "../lib/format";
import type { Overview } from "../lib/types";
import { Card, CardHeader, EmptyRow, ErrorBox, Spinner, Table, Td } from "../components/ui";
import { Link } from "react-router-dom";

interface RecentExecution {
  id: string;
  task_id: string;
  task_type: string;
  result: string;
  started_at: string;
  finished_at: string;
  duration_ms: number;
}

function StatCard({
  label,
  value,
  hint,
  color = "text-slate-900",
}: {
  label: string;
  value: string | number;
  hint?: string;
  color?: string;
}) {
  return (
    <Card className="p-5">
      <div className="text-sm text-slate-500">{label}</div>
      <div className={`mt-1 text-3xl font-semibold ${color}`}>{value}</div>
      {hint && <div className="mt-1 text-xs text-slate-400">{hint}</div>}
    </Card>
  );
}

export function DashboardPage() {
  const { data, loading, error, reload } = useApi<Overview>(() => api.get("/api/overview"));
  const recent = useApi<RecentExecution[]>(() => api.get("/api/overview/recent-executions"));

  if (loading) return <Spinner label="正在加载总览..." />;
  if (error) return <ErrorBox message={error} onRetry={reload} />;
  if (!data) return null;

  const o = data;
  return (
    <div className="space-y-5">
      <div>
        <h2 className="text-lg font-semibold text-slate-900">运行总览</h2>
        <p className="text-sm text-slate-500">Worker、槽位、今日下载与告警概况</p>
      </div>

      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <StatCard
          label="Worker 节点"
          value={`${o.workers.online}/${o.workers.total}`}
          hint="在线/总数"
          color="text-blue-600"
        />
        <StatCard
          label="执行槽位"
          value={`${o.slots.running}/${o.slots.total}`}
          hint={`空闲 ${o.slots.idle}`}
        />
        <StatCard
          label="今日下载完成"
          value={o.today.completed}
          hint={formatBytes(o.today.bytes_total)}
          color="text-green-600"
        />
        <StatCard
          label="未解决告警"
          value={o.open_alerts}
          hint="需要关注"
          color={o.open_alerts > 0 ? "text-red-600" : "text-slate-900"}
        />
      </div>

      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <StatCard label="待处理任务" value={o.tasks.pending} />
        <StatCard label="执行中任务" value={o.tasks.running} color="text-amber-600" />
        <StatCard label="下载账号" value={o.accounts.total} hint={`可用 ${o.accounts.available}`} />
        <StatCard label="可用代理" value={o.proxies.available} hint={`共 ${o.proxies.total}`} />
      </div>

      <Card>
        <CardHeader
          title="最近执行"
          action={
            <Link to="/tasks" className="text-xs text-blue-600 hover:underline">
              查看全部任务 →
            </Link>
          }
        />
        <Table
          headers={["任务类型", "结果", "耗时", "完成时间"]}
          empty={
            recent.data?.length === 0 ? (
              <EmptyRow colSpan={4} text="暂无执行记录" />
            ) : undefined
          }
        >
          {(recent.data ?? []).map((r) => (
            <tr key={r.id}>
              <Td>{r.task_type}</Td>
              <Td>
                <span
                  className={
                    r.result.includes("成功")
                      ? "text-green-600"
                      : r.result.includes("失败")
                        ? "text-red-600"
                        : "text-slate-600"
                  }
                >
                  {r.result}
                </span>
              </Td>
              <Td>{r.duration_ms ? `${(r.duration_ms / 1000).toFixed(1)}s` : "-"}</Td>
              <Td>{formatTime(r.finished_at)}</Td>
            </tr>
          ))}
        </Table>
      </Card>
    </div>
  );
}
