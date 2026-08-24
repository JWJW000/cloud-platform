// 账号注册批次详情页：进度指标 + 注册任务列表 + 单任务重试/取消。
import { useParams, Link } from "react-router-dom";
import { useState } from "react";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import {
  cancelAccountRegistrationBatch,
  cancelAccountRegistrationTask,
  getAccountRegistrationBatch,
  listAccountRegistrationBatchTasks,
  pauseAccountRegistrationBatch,
  resumeAccountRegistrationBatch,
  retryAccountRegistrationTask,
  startAccountRegistrationBatch,
} from "../lib/api";
import { ApiError, type AccountRegistrationTask, type BatchWithProgress } from "../lib/types";
import { formatTime, shortId } from "../lib/format";
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
} from "../components/ui";
import { ArrowLeft, HelpCircle } from "lucide-react";

const STATUS_FILTERS = [
  "全部",
  "待处理",
  "已分配",
  "执行中",
  "等待人工确认",
  "正在重试",
  "已完成",
  "失败",
  "已取消",
];

export function AccountRegistrationBatchDetailPage() {
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

  const runBatchAction = async (action: "start" | "pause" | "resume" | "cancel") => {
    if (!id) return;
    try {
      if (action === "start") await startAccountRegistrationBatch(id);
      else if (action === "pause") await pauseAccountRegistrationBatch(id);
      else if (action === "resume") await resumeAccountRegistrationBatch(id);
      else if (action === "cancel") await cancelAccountRegistrationBatch(id);

      toast.success("操作成功");
      batchApi.reload();
      tasksApi.reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "操作失败");
    }
  };

  const handleRetryTask = async (task: AccountRegistrationTask) => {
    try {
      await retryAccountRegistrationTask(task.id);
      toast.success(`任务 ${task.email} 已重新排队`);
      tasksApi.reload();
      batchApi.reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "重试失败");
    }
  };

  const handleCancelTask = async (task: AccountRegistrationTask) => {
    try {
      await cancelAccountRegistrationTask(task.id);
      toast.success(`任务 ${task.email} 已取消`);
      tasksApi.reload();
      batchApi.reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "取消失败");
    }
  };

  if (batchApi.loading) return <Spinner label="正在加载注册批次详情..." />;
  if (batchApi.error) return <ErrorBox message={batchApi.error} onRetry={batchApi.reload} />;
  const batchData = batchApi.data;
  if (!batchData) return <ErrorBox message="批次不存在" />;

  const p = batchData.progress;
  const pct = p.total > 0 ? Math.round((p.completed / p.total) * 100) : 0;

  return (
    <div className="space-y-5">
      <div className="flex items-center gap-2">
        <Link to="/account-registration-batches" className="text-slate-500 hover:text-slate-700">
          <ArrowLeft className="h-5 w-5" />
        </Link>
        <div className="flex-1">
          <div className="flex items-center gap-2">
            <h2 className="text-xl font-bold text-slate-900">{batchData.name}</h2>
            <StatusBadge status={batchData.status} />
          </div>
          <p className="text-xs text-slate-500">
            优先级：{batchData.priority} | 创建时间：{formatTime(batchData.created_at)}
          </p>
        </div>
        {isSuperAdmin && (
          <div className="flex gap-2">
            {batchData.status === "待开始" && (
              <Button size="sm" onClick={() => runBatchAction("start")}>
                开始执行
              </Button>
            )}
            {batchData.status === "执行中" && (
              <>
                <Button size="sm" variant="secondary" onClick={() => runBatchAction("pause")}>
                  暂停
                </Button>
                <Button size="sm" variant="danger" onClick={() => runBatchAction("cancel")}>
                  取消
                </Button>
              </>
            )}
            {batchData.status === "已暂停" && (
              <Button size="sm" variant="success" onClick={() => runBatchAction("resume")}>
                恢复执行
              </Button>
            )}
          </div>
        )}
      </div>

      {/* 指标统计面板 */}
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-6">
        <div className="rounded-lg bg-white p-3 border border-slate-200 shadow-sm text-center">
          <div className="text-xs text-slate-500">总任务数</div>
          <div className="text-xl font-bold text-slate-900">{p.total}</div>
        </div>
        <div className="rounded-lg bg-blue-50 p-3 border border-blue-200 shadow-sm text-center">
          <div className="text-xs text-blue-600">待处理</div>
          <div className="text-xl font-bold text-blue-700">{p.pending}</div>
        </div>
        <div className="rounded-lg bg-indigo-50 p-3 border border-indigo-200 shadow-sm text-center">
          <div className="text-xs text-indigo-600">执行中</div>
          <div className="text-xl font-bold text-indigo-700">{p.running}</div>
        </div>
        <div className="rounded-lg bg-amber-50 p-3 border border-amber-200 shadow-sm text-center">
          <div className="text-xs text-amber-600">等待人工确认</div>
          <div className="text-xl font-bold text-amber-700">{p.awaiting_confirm}</div>
        </div>
        <div className="rounded-lg bg-green-50 p-3 border border-green-200 shadow-sm text-center">
          <div className="text-xs text-green-600">已完成</div>
          <div className="text-xl font-bold text-green-700">{p.completed} ({pct}%)</div>
        </div>
        <div className="rounded-lg bg-red-50 p-3 border border-red-200 shadow-sm text-center">
          <div className="text-xs text-red-600">失败</div>
          <div className="text-xl font-bold text-red-700">{p.failed}</div>
        </div>
      </div>

      {/* 待确认事项快捷跳转提示 */}
      {p.awaiting_confirm > 0 && (
        <div className="flex items-center justify-between rounded-lg border border-amber-300 bg-amber-50 px-4 py-3 text-amber-800 text-sm">
          <div className="flex items-center gap-2">
            <HelpCircle className="h-5 w-5 text-amber-600" />
            <span>本批次有 <strong>{p.awaiting_confirm}</strong> 个注册任务需要人工输入验证码或确认。</span>
          </div>
          <Link to="/manual-actions">
            <Button size="sm" variant="secondary">
              前往处理
            </Button>
          </Link>
        </div>
      )}

      {/* 任务列表 */}
      <Card>
        <div className="flex items-center justify-between border-b border-slate-100 p-4">
          <h3 className="font-semibold text-slate-800">注册任务列表</h3>
          <div className="flex items-center gap-2">
            <Select value={statusFilter} onChange={(e) => setStatusFilter(e.target.value)}>
              {STATUS_FILTERS.map((s) => (
                <option key={s} value={s}>
                  {s === "全部" ? "全部状态" : s}
                </option>
              ))}
            </Select>
            <Button variant="secondary" size="sm" onClick={tasksApi.reload}>
              刷新
            </Button>
          </div>
        </div>

        {tasksApi.loading ? (
          <div className="p-8">
            <Spinner label="加载任务列表中..." />
          </div>
        ) : (
          <Table
            headers={["账号邮箱", "状态", "阶段", "尝试", "执行节点", "最近错误 / 备注", "操作"]}
            empty={
              !tasksApi.data || tasksApi.data.length === 0 ? (
                <EmptyRow colSpan={7} text="暂无任务" />
              ) : undefined
            }
          >
            {(tasksApi.data ?? []).map((t) => (
              <tr key={t.id}>
                <Td>
                  <div className="font-medium text-slate-800">{t.email}</div>
                  <div className="text-[11px] text-slate-400">#{shortId(t.id)}</div>
                </Td>
                <Td>
                  <StatusBadge status={t.status} />
                </Td>
                <Td className="text-xs text-slate-600">{t.stage || "-"}</Td>
                <Td className="text-xs text-slate-500">
                  {t.attempts}/{t.max_attempts}
                </Td>
                <Td className="text-xs text-slate-500 font-mono">
                  {t.lease_node_id ? shortId(t.lease_node_id) : "-"}
                </Td>
                <Td className="max-w-48 truncate text-xs text-red-600" title={t.last_error ?? ""}>
                  {t.last_error ?? "-"}
                </Td>
                <Td>
                  {isSuperAdmin && (
                    <div className="flex items-center gap-1">
                      {(t.status === "失败" || t.status === "已取消" || t.status === "等待人工确认") && (
                        <Button size="sm" variant="secondary" onClick={() => handleRetryTask(t)}>
                          重试
                        </Button>
                      )}
                      {(t.status === "待处理" || t.status === "已分配" || t.status === "执行中") && (
                        <Button size="sm" variant="ghost" onClick={() => handleCancelTask(t)}>
                          取消
                        </Button>
                      )}
                    </div>
                  )}
                </Td>
              </tr>
            ))}
          </Table>
        )}
      </Card>
    </div>
  );
}
