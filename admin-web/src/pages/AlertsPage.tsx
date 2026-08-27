// 告警页：列表 + 处理（resolve）。
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can } from "../lib/permissions";
import { api } from "../lib/api";
import { ApiError, type Alert } from "../lib/types";
import { formatTime, relativeTime } from "../lib/format";
import {
  Badge,
  Button,
  Card,
  EmptyRow,
  ErrorBox,
  SkeletonTable,
  Table,
  Td,
} from "../components/ui";

export function AlertsPage() {
  const { user } = useAuth();
  const toast = useToast();
  const canResolve = can(user?.role, "resolve_alert");
  const { data, loading, error, reload } = useApi<Alert[]>(() => api.get("/api/alerts"));

  const resolve = async (a: Alert) => {
    try {
      await api.post(`/api/alerts/${a.id}/resolve`);
      toast.success("告警已处理");
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "处理失败");
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">告警</h2>
          <p className="text-sm text-slate-500">节点、任务、存储等异常事件</p>
        </div>
        <Button variant="secondary" size="sm" onClick={reload}>
          刷新
        </Button>
      </div>

      {error && <ErrorBox message={error} onRetry={reload} />}

      <Card>
        <Table
          headers={["级别", "分类", "标题", "详情", "时间", "状态", "操作"]}
          empty={!loading && (!data || data.length === 0) ? <EmptyRow colSpan={7} text="暂无告警" /> : undefined}
        >
          {loading && (!data || data.length === 0) ? (
            <SkeletonTable columns={7} rows={6} />
          ) : (
            (data ?? []).map((a) => (
              <tr key={a.id}>
                <Td>
                  <Badge
                    className={
                      a.level === "严重"
                        ? "bg-red-100 text-red-700"
                        : a.level === "警告"
                          ? "bg-amber-100 text-amber-700"
                          : "bg-blue-100 text-blue-700"
                    }
                  >
                    {a.level}
                  </Badge>
                </Td>
                <Td className="text-xs text-slate-500">{a.category}</Td>
                <Td className="max-w-56 font-medium text-slate-800">
                  <div className="truncate" title={a.title}>{a.title}</div>
                </Td>
                <Td className="max-w-64 truncate text-xs text-slate-500" title={a.detail}>
                  {a.detail}
                </Td>
                <Td className="text-xs text-slate-500" title={formatTime(a.created_at)}>
                  {relativeTime(a.created_at)}
                </Td>
                <Td>
                  {a.resolved_at ? (
                    <Badge className="bg-green-100 text-green-700">已处理</Badge>
                  ) : (
                    <Badge className="bg-red-100 text-red-700">未处理</Badge>
                  )}
                </Td>
                <Td>
                  {canResolve && !a.resolved_at ? (
                    <Button size="sm" variant="secondary" onClick={() => resolve(a)}>
                      处理
                    </Button>
                  ) : (
                    <span className="text-xs text-slate-300">-</span>
                  )}
                </Td>
              </tr>
            ))
          )}
        </Table>
      </Card>
    </div>
  );
}
