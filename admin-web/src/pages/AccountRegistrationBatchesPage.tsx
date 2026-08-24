// 账号注册批次页：列表 + 开始/暂停/恢复/取消 + 批次进度展示。
import { Link } from "react-router-dom";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import {
  cancelAccountRegistrationBatch,
  listAccountRegistrationBatches,
  pauseAccountRegistrationBatch,
  resumeAccountRegistrationBatch,
  startAccountRegistrationBatch,
} from "../lib/api";
import { ApiError, type BatchWithProgress } from "../lib/types";
import { formatTime } from "../lib/format";
import {
  Button,
  Card,
  EmptyRow,
  ErrorBox,
  Spinner,
  StatusBadge,
  Table,
  Td,
} from "../components/ui";
import { Users, UserPlus } from "lucide-react";

export function AccountRegistrationBatchesPage() {
  const { user } = useAuth();
  const toast = useToast();
  const isSuperAdmin = user?.role === "超级管理员";
  const { data, loading, error, reload } = useApi<BatchWithProgress[]>(listAccountRegistrationBatches);

  const runAction = async (batch: BatchWithProgress, action: "start" | "pause" | "resume" | "cancel") => {
    const label = {
      start: "开始",
      pause: "暂停",
      resume: "恢复",
      cancel: "取消",
    }[action];

    try {
      if (action === "start") await startAccountRegistrationBatch(batch.id);
      else if (action === "pause") await pauseAccountRegistrationBatch(batch.id);
      else if (action === "resume") await resumeAccountRegistrationBatch(batch.id);
      else if (action === "cancel") await cancelAccountRegistrationBatch(batch.id);

      toast.success(`注册批次「${batch.name}」已${label}`);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : `${label}失败`);
      if (e instanceof ApiError && e.status === 409) reload();
    }
  };

  if (loading) return <Spinner label="正在加载账号注册批次..." />;
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">账号注册批次</h2>
          <p className="text-sm text-slate-500">统一编排待注册账号，Master 自动下发给在线 Worker 执行浏览器自动化注册</p>
        </div>
        <div className="flex items-center gap-2">
          <Link to="/accounts">
            <Button variant="secondary" size="sm">
              <UserPlus className="mr-1 h-4 w-4" />
              前往账号页导入
            </Button>
          </Link>
          <Button variant="secondary" size="sm" onClick={reload}>
            刷新
          </Button>
        </div>
      </div>

      <Card>
        <Table
          headers={["批次名称", "状态", "优先级", "任务进度", "创建时间", "操作"]}
          empty={!data || data.length === 0 ? <EmptyRow colSpan={6} text="暂无注册批次" /> : undefined}
        >
          {(data ?? []).map((b) => {
            const p = b.progress;
            const pct = p.total > 0 ? Math.round((p.completed / p.total) * 100) : 0;
            return (
              <tr key={b.id}>
                <Td>
                  <Link
                    to={`/account-registration-batches/${b.id}`}
                    className="font-medium text-blue-700 hover:underline flex items-center gap-1.5"
                  >
                    <Users className="h-4 w-4 text-slate-400" />
                    {b.name}
                  </Link>
                </Td>
                <Td>
                  <StatusBadge status={b.status} />
                </Td>
                <Td>{b.priority}</Td>
                <Td className="min-w-44">
                  <div className="space-y-1">
                    <div className="flex justify-between text-xs text-slate-500">
                      <span>已完成 {p.completed}/{p.total} ({pct}%)</span>
                      {p.failed > 0 && <span className="text-red-600">失败 {p.failed}</span>}
                    </div>
                    <div className="h-2 w-full overflow-hidden rounded-full bg-slate-100">
                      <div
                        className="h-full bg-blue-600 transition-all duration-300"
                        style={{ width: `${pct}%` }}
                      />
                    </div>
                  </div>
                </Td>
                <Td className="text-xs text-slate-500">{formatTime(b.created_at)}</Td>
                <Td>
                  {isSuperAdmin ? (
                    <div className="flex items-center gap-1">
                      {b.status === "待开始" && (
                        <Button size="sm" onClick={() => runAction(b, "start")}>
                          开始
                        </Button>
                      )}
                      {b.status === "执行中" && (
                        <>
                          <Button size="sm" variant="secondary" onClick={() => runAction(b, "pause")}>
                            暂停
                          </Button>
                          <Button size="sm" variant="danger" onClick={() => runAction(b, "cancel")}>
                            取消
                          </Button>
                        </>
                      )}
                      {b.status === "已暂停" && (
                        <Button size="sm" variant="success" onClick={() => runAction(b, "resume")}>
                          恢复
                        </Button>
                      )}
                      <Link to={`/account-registration-batches/${b.id}`}>
                        <Button size="sm" variant="ghost">
                          详情
                        </Button>
                      </Link>
                    </div>
                  ) : (
                    <Link to={`/account-registration-batches/${b.id}`}>
                      <Button size="sm" variant="ghost">
                        查看
                      </Button>
                    </Link>
                  )}
                </Td>
              </tr>
            );
          })}
        </Table>
      </Card>
    </div>
  );
}
