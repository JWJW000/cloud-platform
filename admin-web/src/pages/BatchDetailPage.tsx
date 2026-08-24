// 批次详情页：进度 + 任务列表 + 重试失败。
import { useParams } from "react-router-dom";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can } from "../lib/permissions";
import { api } from "../lib/api";
import { ApiError, type Batch, type BatchProgress, type Task } from "../lib/types";
import { formatTime, shortId } from "../lib/format";
import {
  Button,
  Card,
  CardHeader,
  EmptyRow,
  ErrorBox,
  Spinner,
  StatusBadge,
  Table,
  Td,
} from "../components/ui";

export function BatchDetailPage() {
  const { id = "" } = useParams();
  const { user } = useAuth();
  const toast = useToast();
  const canManage = can(user?.role, "manage_batch");
  const batch = useApi<Batch>(() => api.get(`/api/batches/${id}`), [id]);
  const progress = useApi<BatchProgress>(() => api.get(`/api/batches/${id}/progress`), [id]);
  const tasks = useApi<Task[]>(() => api.get("/api/tasks", { batch_id: id, limit: 200 }), [id]);

  const retryFailed = async () => {
    try {
      await api.post(`/api/batches/${id}/retry-failed`);
      toast.success("已重新排队失败任务");
      tasks.reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "重试失败");
    }
  };

  if (batch.loading) return <Spinner label="正在加载批次..." />;
  if (batch.error) return <ErrorBox message={batch.error} onRetry={batch.reload} />;
  const b = batch.data;
  if (!b) return null;

  const p = progress.data;

  return (
    <div className="space-y-5">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">{b.name}</h2>
          <p className="text-sm text-slate-500">
            创建于 {formatTime(b.created_at)} · 优先级 {b.priority} · 格式 {b.download_format}
          </p>
        </div>
        <StatusBadge status={b.status} />
      </div>

      {p && (
        <Card className="p-5">
          <div className="mb-2 flex items-center justify-between text-sm">
            <span className="text-slate-500">
              进度 {p.done}/{p.total}（失败 {p.failed}，运行中 {p.running}）
            </span>
            <span className="font-medium text-slate-800">{p.percent}%</span>
          </div>
          <div className="h-2 w-full overflow-hidden rounded-full bg-slate-100">
            <div
              className="h-full rounded-full bg-blue-600 transition-all"
              style={{ width: `${Math.min(100, Math.max(0, p.percent))}%` }}
            />
          </div>
        </Card>
      )}

      <Card>
        <CardHeader
          title="任务列表"
          action={
            canManage ? (
              <Button size="sm" variant="secondary" onClick={retryFailed}>
                重试失败任务
              </Button>
            ) : undefined
          }
        />
        <Table
          headers={["书名", "状态", "阶段", "尝试", "NAS 相对路径", "最近错误"]}
          empty={!tasks.data || tasks.data.length === 0 ? <EmptyRow colSpan={6} text="暂无任务" /> : undefined}
        >
          {(tasks.data ?? []).map((t) => (
            <tr key={t.id}>
              <Td className="max-w-64">
                <div className="truncate" title={t.title}>
                  {t.title}
                </div>
              </Td>
              <Td>
                <StatusBadge status={t.status} />
              </Td>
              <Td className="text-xs text-slate-500">{t.stage}</Td>
              <Td className="text-xs text-slate-500">
                {t.attempts}/{t.max_attempts}
              </Td>
              <Td className="max-w-48 truncate font-mono text-xs text-slate-500" title={t.nas_relative_path ?? ""}>
                {t.nas_relative_path ?? "-"}
              </Td>
              <Td className="max-w-48 truncate text-xs text-red-500" title={t.last_error ?? ""}>
                {t.last_error ?? "-"}
              </Td>
            </tr>
          ))}
        </Table>
      </Card>

      {tasks.data && tasks.data.length > 0 && (
        <div className="text-xs text-slate-400">
          任务 ID：{tasks.data.map((t) => shortId(t.id)).join("、")}
        </div>
      )}
    </div>
  );
}
