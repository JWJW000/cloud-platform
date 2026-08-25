import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { useApi } from "../../hooks/useApi";
import { useToast } from "../../context/ToastContext";
import { useAuth } from "../../context/AuthContext";
import {
  cancelAccountRegistrationBatch,
  cancelAccountRegistrationTask,
  getAccountRegistrationBatch,
  listAccountRegistrationBatches,
  listAccountRegistrationBatchTasks,
  pauseAccountRegistrationBatch,
  resumeAccountRegistrationBatch,
  retryAccountRegistrationTask,
  startAccountRegistrationBatch,
} from "../../lib/api";
import { ApiError, type AccountRegistrationTask, type BatchWithProgress } from "../../lib/types";
import { formatTime, shortId } from "../../lib/format";
import { MailProviderStatus } from "./MailProviderStatus";
import { parseTaskPhase, REGISTRATION_PHASE_CONFIG } from "./registrationPhases";
import {
  Button,
  Card,
  EmptyRow,
  ErrorBox,
  Select,
  Spinner,
  StatusBadge,
  Table,
  Td,
} from "../../components/ui";
import { ArrowLeft, Play, Pause, XCircle, ShieldAlert } from "lucide-react";

export function RegistrationQueuePage() {
  const { user } = useAuth();
  const toast = useToast();
  const isSuperAdmin = user?.role === "超级管理员";
  const { data, loading, error, reload } = useApi<BatchWithProgress[]>(listAccountRegistrationBatches);

  const runAction = async (batch: BatchWithProgress, action: "start" | "pause" | "resume" | "cancel") => {
    const label = {
      start: "启动",
      pause: "暂停",
      resume: "恢复",
      cancel: "取消",
    }[action];

    try {
      if (action === "start") await startAccountRegistrationBatch(batch.id);
      else if (action === "pause") await pauseAccountRegistrationBatch(batch.id);
      else if (action === "resume") await resumeAccountRegistrationBatch(batch.id);
      else if (action === "cancel") await cancelAccountRegistrationBatch(batch.id);

      toast.success(`注册分组「${batch.name}」已${label}`);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : `${label}失败`);
      if (e instanceof ApiError && e.status === 409) reload();
    }
  };

  if (loading) return <Spinner label="正在加载注册队列..." />;
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  const batches = data ?? [];

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <div className="flex items-center gap-2">
            <Link to="/accounts" className="text-xs text-slate-500 hover:text-slate-800">
              账号中心
            </Link>
            <span className="text-slate-300">/</span>
            <h1 className="text-xl font-bold text-slate-900">注册队列</h1>
          </div>
          <p className="text-xs text-slate-500">
            调度待注册账号池，Master 自动分发给在线 Worker 执行分布式自动化注册与验证
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Link to="/attention/manual?type=account">
            <Button variant="secondary" size="sm">
              <ShieldAlert className="mr-1.5 h-4 w-4 text-amber-600" />
              人工验证事项
            </Button>
          </Link>
          <Button variant="secondary" size="sm" onClick={reload}>
            刷新队列
          </Button>
        </div>
      </div>

      <MailProviderStatus />

      <Card>
        <div className="border-b border-slate-100 p-4">
          <h3 className="text-sm font-semibold text-slate-800">注册分组与执行进度</h3>
        </div>
        <Table
          headers={["分组名称", "状态", "优先级", "执行进度", "创建时间", "操作"]}
          empty={batches.length === 0 ? <EmptyRow colSpan={6} text="当前无注册分组" /> : undefined}
        >
          {batches.map((b) => {
            const p = b.progress;
            const pct = p && p.total > 0 ? Math.round(((p.completed + p.failed) / p.total) * 100) : 0;
            return (
              <tr key={b.id}>
                <Td>
                  <Link
                    to={`/accounts/registrations/${b.id}`}
                    className="font-medium text-blue-600 hover:underline"
                  >
                    {b.name}
                  </Link>
                </Td>
                <Td>
                  <StatusBadge status={b.status} />
                </Td>
                <Td className="text-xs text-slate-500">{b.priority}</Td>
                <Td>
                  {p ? (
                    <div className="w-48 space-y-1">
                      <div className="flex justify-between text-xs text-slate-500">
                        <span>
                          {p.completed}/{p.total}
                          {p.failed > 0 && <span className="ml-1 text-red-500">({p.failed} 失败)</span>}
                        </span>
                        <span>{pct}%</span>
                      </div>
                      <div className="h-1.5 w-full overflow-hidden rounded-full bg-slate-100">
                        <div
                          className={`h-full transition-all ${
                            b.status === "已完成"
                              ? "bg-green-500"
                              : b.status === "执行中"
                                ? "bg-blue-500"
                                : "bg-slate-400"
                          }`}
                          style={{ width: `${pct}%` }}
                        />
                      </div>
                    </div>
                  ) : (
                    <span className="text-xs text-slate-400">-</span>
                  )}
                </Td>
                <Td className="text-xs text-slate-500">{formatTime(b.created_at)}</Td>
                <Td>
                  {isSuperAdmin && (
                    <div className="flex items-center gap-1">
                      {b.status === "草稿" && (
                        <Button size="sm" variant="ghost" onClick={() => runAction(b, "start")}>
                          <Play className="mr-1 h-3.5 w-3.5 text-green-600" />
                          启动
                        </Button>
                      )}
                      {b.status === "执行中" && (
                        <Button size="sm" variant="ghost" onClick={() => runAction(b, "pause")}>
                          <Pause className="mr-1 h-3.5 w-3.5 text-amber-600" />
                          暂停
                        </Button>
                      )}
                      {b.status === "已暂停" && (
                        <Button size="sm" variant="ghost" onClick={() => runAction(b, "resume")}>
                          <Play className="mr-1 h-3.5 w-3.5 text-blue-600" />
                          恢复
                        </Button>
                      )}
                      {["草稿", "执行中", "已暂停"].includes(b.status) && (
                        <Button size="sm" variant="ghost" onClick={() => runAction(b, "cancel")}>
                          <XCircle className="mr-1 h-3.5 w-3.5 text-red-600" />
                          取消
                        </Button>
                      )}
                      <Link to={`/accounts/registrations/${b.id}`}>
                        <Button size="sm" variant="ghost">
                          详情
                        </Button>
                      </Link>
                    </div>
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

export function RegistrationGroupDetailPage() {
  const { id } = useParams<{ id: string }>();
  const { user } = useAuth();
  const toast = useToast();
  const isSuperAdmin = user?.role === "超级管理员";

  const [statusFilter, setStatusFilter] = useState("全部");

  const batchApi = useApi<BatchWithProgress>(() => getAccountRegistrationBatch(id!), [id]);
  const tasksApi = useApi<AccountRegistrationTask[]>(
    () =>
      listAccountRegistrationBatchTasks(id!, {
        status: statusFilter === "全部" ? undefined : statusFilter,
        limit: 200,
      }),
    [id, statusFilter]
  );

  const handleRetryTask = async (task: AccountRegistrationTask) => {
    try {
      await retryAccountRegistrationTask(task.id);
      toast.success(`任务 ${shortId(task.id)} 已重新排队`);
      tasksApi.reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "重试失败");
    }
  };

  const handleCancelTask = async (task: AccountRegistrationTask) => {
    try {
      await cancelAccountRegistrationTask(task.id);
      toast.success(`任务 ${shortId(task.id)} 已取消`);
      tasksApi.reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "取消失败");
    }
  };

  if (batchApi.loading) return <Spinner label="正在加载注册分组..." />;
  if (batchApi.error) return <ErrorBox message={batchApi.error} onRetry={batchApi.reload} />;
  if (!batchApi.data) return null;

  const b = batchApi.data;
  const p = b.progress;

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <Link to="/accounts/registrations" className="text-slate-500 hover:text-slate-900">
          <ArrowLeft className="h-5 w-5" />
        </Link>
        <div>
          <div className="flex items-center gap-2">
            <h1 className="text-xl font-bold text-slate-900">{b.name}</h1>
            <StatusBadge status={b.status} />
          </div>
          <p className="text-xs text-slate-500">优先级 {b.priority} · 创建于 {formatTime(b.created_at)}</p>
        </div>
      </div>

      {/* 进度摘要 */}
      {p && (
        <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
          <Card className="p-4">
            <div className="text-xs text-slate-500">总任务数</div>
            <div className="mt-1 text-2xl font-bold text-slate-900">{p.total}</div>
          </Card>
          <Card className="p-4">
            <div className="text-xs text-slate-500">已完成</div>
            <div className="mt-1 text-2xl font-bold text-green-600">{p.completed}</div>
          </Card>
          <Card className="p-4">
            <div className="text-xs text-slate-500">执行中 / 待处理</div>
            <div className="mt-1 text-2xl font-bold text-blue-600">{p.pending + p.running}</div>
          </Card>
          <Card className="p-4">
            <div className="text-xs text-slate-500">失败</div>
            <div className="mt-1 text-2xl font-bold text-red-600">{p.failed}</div>
          </Card>
        </div>
      )}

      {/* 任务列表 */}
      <Card>
        <div className="flex items-center justify-between border-b border-slate-100 p-4">
          <div className="flex items-center gap-3">
            <h3 className="text-sm font-semibold text-slate-800">注册任务明细</h3>
            <Select
              value={statusFilter}
              onChange={(e) => setStatusFilter(e.target.value)}
              className="text-xs py-1"
            >
              {["全部", "待处理", "已分配", "执行中", "等待人工确认", "正在重试", "已完成", "失败", "已取消"].map(
                (s) => (
                  <option key={s} value={s}>
                    {s}
                  </option>
                )
              )}
            </Select>
          </div>
          <Button variant="ghost" size="sm" onClick={tasksApi.reload}>
            刷新明细
          </Button>
        </div>

        <Table
          headers={["任务 ID", "注册邮箱", "状态 / 阶段", "重试次数", "Worker", "最近错误", "操作"]}
          empty={!tasksApi.data || tasksApi.data.length === 0 ? <EmptyRow colSpan={7} text="暂无任务" /> : undefined}
        >
          {(tasksApi.data ?? []).map((t) => {
            const phaseKey = parseTaskPhase(
              t.status,
              [t.stage, t.last_error].filter(Boolean).join(" · "),
            );
            const phaseInfo = REGISTRATION_PHASE_CONFIG[phaseKey];

            return (
              <tr key={t.id}>
                <Td className="font-mono text-xs text-slate-500">{shortId(t.id)}</Td>
                <Td className="text-xs font-medium text-slate-800">{t.email || "-"}</Td>
                <Td>
                  <div className="flex flex-col gap-1">
                    <StatusBadge status={t.status} />
                    {t.stage && <span className="text-[10px] text-slate-500">{t.stage}</span>}
                    {phaseInfo && t.status !== "已完成" && (
                      <span
                        className={`inline-flex items-center rounded px-1.5 py-0.5 text-[10px] font-medium w-fit ${phaseInfo.badgeClass}`}
                        title={phaseInfo.description}
                      >
                        {phaseInfo.label}
                      </span>
                    )}
                  </div>
                </Td>
                <Td className="text-xs text-slate-500">{t.attempts}</Td>
                <Td className="text-xs font-mono text-slate-500">{t.lease_node_id ? shortId(t.lease_node_id) : "-"}</Td>
                <Td className="max-w-xs truncate text-xs text-red-500">{t.last_error ?? "-"}</Td>
                <Td>
                  {isSuperAdmin && (
                    <div className="flex items-center gap-1">
                      {["失败", "等待人工确认"].includes(t.status) && (
                        <Button size="sm" variant="ghost" onClick={() => handleRetryTask(t)}>
                          重试
                        </Button>
                      )}
                      {["待处理", "已分配", "执行中", "正在重试"].includes(t.status) && (
                        <Button size="sm" variant="ghost" onClick={() => handleCancelTask(t)}>
                          取消
                        </Button>
                      )}
                    </div>
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
