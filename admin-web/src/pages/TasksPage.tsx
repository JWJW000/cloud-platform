// 统一任务页：支持图书下载与账号注册任务类型筛选、状态过滤、重试/取消与待确认处理。
import { useState } from "react";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can } from "../lib/permissions";
import {
  api,
  cancelAccountRegistrationTask,
  listAccountRegistrationTasks,
  retryAccountRegistrationTask,
} from "../lib/api";
import {
  AccountRegistrationTask,
  ApiError,
  type Task,
} from "../lib/types";
import { formatBytes, shortId } from "../lib/format";
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

const STATUS_FILTERS = [
  "全部",
  "待处理",
  "已分配",
  "执行中",
  "等待入库",
  "待确认",
  "等待人工确认",
  "正在重试",
  "已完成",
  "失败",
  "已跳过",
  "已取消",
];

export function TasksPage() {
  const { user } = useAuth();
  const toast = useToast();
  const canManage = can(user?.role, "manage_task");
  const [taskTypeFilter, setTaskTypeFilter] = useState<"全部" | "图书下载" | "账号注册">("全部");
  const [status, setStatus] = useState("全部");

  // 图书下载任务
  const bookTasksApi = useApi<Task[]>(
    () =>
      api.get("/api/tasks", {
        status: status === "全部" ? undefined : status,
        limit: 200,
      }),
    [status]
  );

  // 账号注册任务
  const regTasksApi = useApi<AccountRegistrationTask[]>(
    () =>
      listAccountRegistrationTasks({
        status: status === "全部" ? undefined : status,
        limit: 200,
      }),
    [status]
  );

  const needsConfirm = useApi<Task[]>(() => api.get("/api/tasks/needs-confirm"));

  const retryBookTask = async (t: Task) => {
    try {
      await api.post(`/api/tasks/${t.id}/retry`);
      toast.success(`已重新排队《${t.title}》`);
      bookTasksApi.reload();
      needsConfirm.reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "重试失败");
    }
  };

  const cancelBookTask = async (t: Task) => {
    try {
      await api.post(`/api/tasks/${t.id}/cancel`, { reason: "管理员取消" });
      toast.success(`已取消《${t.title}》`);
      bookTasksApi.reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "取消失败");
    }
  };

  const retryRegTask = async (t: AccountRegistrationTask) => {
    try {
      await retryAccountRegistrationTask(t.id);
      toast.success(`已重新排队注册账号 ${t.email}`);
      regTasksApi.reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "重试失败");
    }
  };

  const cancelRegTask = async (t: AccountRegistrationTask) => {
    try {
      await cancelAccountRegistrationTask(t.id);
      toast.success(`已取消注册账号 ${t.email}`);
      regTasksApi.reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "取消失败");
    }
  };

  const isLoading = taskTypeFilter === "账号注册" ? regTasksApi.loading : bookTasksApi.loading;
  const isError = taskTypeFilter === "账号注册" ? regTasksApi.error : bookTasksApi.error;
  const reloadCurrent = () => {
    bookTasksApi.reload();
    regTasksApi.reload();
    needsConfirm.reload();
  };

  if (isLoading) return <Spinner label="正在加载任务..." />;
  if (isError) return <ErrorBox message={isError} onRetry={reloadCurrent} />;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">统一任务列表</h2>
          <p className="text-sm text-slate-500">
            {needsConfirm.data && needsConfirm.data.length > 0
              ? `${needsConfirm.data.length} 个图书任务等待 NAS 核验确认`
              : "查看图书下载与账号注册任务执行进展"}
          </p>
        </div>
        <div className="flex items-center gap-2">
          {/* 任务类型筛选 */}
          <Select
            value={taskTypeFilter}
            onChange={(e) => setTaskTypeFilter(e.target.value as any)}
            aria-label="任务类型筛选"
          >
            <option value="全部">全部类型（图书下载）</option>
            <option value="图书下载">图书下载</option>
            <option value="账号注册">账号注册</option>
          </Select>

          {/* 状态筛选 */}
          <Select value={status} onChange={(e) => setStatus(e.target.value)} aria-label="任务状态筛选">
            {STATUS_FILTERS.map((s) => (
              <option key={s} value={s}>
                {s === "全部" ? "全部状态" : s}
              </option>
            ))}
          </Select>
          <Button variant="secondary" size="sm" onClick={reloadCurrent}>
            刷新
          </Button>
        </div>
      </div>

      {/* 待确认图书任务卡片 */}
      {taskTypeFilter !== "账号注册" && needsConfirm.data && needsConfirm.data.length > 0 && (
        <Card className="border-amber-200 bg-amber-50/50">
          <div className="px-5 py-3 text-sm font-medium text-amber-800">
            待 NAS 核验确认图书任务（{needsConfirm.data.length}）
          </div>
          <Table headers={["书名", "状态", "NAS 相对路径", "操作"]} empty={undefined}>
            {needsConfirm.data.map((t) => (
              <tr key={t.id}>
                <Td className="max-w-64">
                  <div className="truncate font-medium" title={t.title}>
                    {t.title}
                  </div>
                </Td>
                <Td>
                  <StatusBadge status={t.status} />
                </Td>
                <Td className="max-w-48 truncate font-mono text-xs text-slate-500" title={t.nas_relative_path ?? ""}>
                  {t.nas_relative_path ?? "-"}
                </Td>
                <Td>
                  {canManage ? (
                    <div className="flex gap-1">
                      <Button
                        size="sm"
                        variant="success"
                        onClick={() => confirmTask(t, needsConfirm.reload, bookTasksApi.reload, toast)}
                      >
                        确认
                      </Button>
                      <Button size="sm" variant="secondary" onClick={() => retryBookTask(t)}>
                        重试
                      </Button>
                    </div>
                  ) : (
                    <span className="text-xs text-slate-400">只读</span>
                  )}
                </Td>
              </tr>
            ))}
          </Table>
        </Card>
      )}

      {/* 主表格 */}
      <Card>
        {taskTypeFilter === "账号注册" ? (
          // 账号注册任务表格
          <Table
            headers={["账号邮箱", "状态", "阶段", "尝试", "节点", "最近错误", "操作"]}
            empty={
              !regTasksApi.data || regTasksApi.data.length === 0 ? (
                <EmptyRow colSpan={7} text="暂无账号注册任务" />
              ) : undefined
            }
          >
            {(regTasksApi.data ?? []).map((t) => (
              <tr key={t.id}>
                <Td className="max-w-56">
                  <div className="font-medium text-slate-800">{t.email}</div>
                  <div className="text-[11px] text-slate-400">#{shortId(t.id)}</div>
                </Td>
                <Td>
                  <StatusBadge status={t.status} />
                </Td>
                <Td className="text-xs text-slate-500">{t.stage || "-"}</Td>
                <Td className="text-xs text-slate-500">
                  {t.attempts}/{t.max_attempts}
                </Td>
                <Td className="text-xs text-slate-500 font-mono">
                  {t.lease_node_id ? shortId(t.lease_node_id) : "-"}
                </Td>
                <Td className="max-w-44 truncate text-xs text-red-600" title={t.last_error ?? ""}>
                  {t.last_error ?? "-"}
                </Td>
                <Td>
                  {canManage ? (
                    <div className="flex gap-1">
                      {(t.status === "失败" || t.status === "已取消" || t.status === "等待人工确认") && (
                        <Button size="sm" variant="secondary" onClick={() => retryRegTask(t)}>
                          重试
                        </Button>
                      )}
                      {(t.status === "待处理" || t.status === "已分配" || t.status === "执行中") && (
                        <Button size="sm" variant="ghost" onClick={() => cancelRegTask(t)}>
                          取消
                        </Button>
                      )}
                    </div>
                  ) : (
                    <span className="text-xs text-slate-400">只读</span>
                  )}
                </Td>
              </tr>
            ))}
          </Table>
        ) : (
          // 图书下载任务表格
          <Table
            headers={["书名", "状态", "阶段", "尝试", "下载量", "节点", "NAS 相对路径", "操作"]}
            empty={
              !bookTasksApi.data || bookTasksApi.data.length === 0 ? (
                <EmptyRow colSpan={8} text="暂无图书任务" />
              ) : undefined
            }
          >
            {(bookTasksApi.data ?? []).map((t) => (
              <tr key={t.id}>
                <Td className="max-w-56">
                  <div className="truncate font-medium text-slate-800" title={t.title}>
                    {t.title}
                  </div>
                  <div className="text-xs text-slate-400">#{shortId(t.id)}</div>
                </Td>
                <Td>
                  <StatusBadge status={t.status} />
                </Td>
                <Td className="text-xs text-slate-500">{t.stage || "-"}</Td>
                <Td className="text-xs text-slate-500">
                  {t.attempts}/{t.max_attempts}
                </Td>
                <Td className="text-xs text-slate-500">{formatBytes(t.downloaded_bytes)}</Td>
                <Td className="text-xs text-slate-500 font-mono">
                  {t.lease_node_id ? shortId(t.lease_node_id) : "-"}
                </Td>
                <Td className="max-w-44 truncate font-mono text-xs text-slate-500" title={t.nas_relative_path ?? ""}>
                  {t.nas_relative_path ?? "-"}
                </Td>
                <Td>
                  {canManage ? (
                    <div className="flex gap-1">
                      {t.status === "失败" && (
                        <Button size="sm" variant="secondary" onClick={() => retryBookTask(t)}>
                          重试
                        </Button>
                      )}
                      {(t.status === "待处理" || t.status === "已分配" || t.status === "执行中") && (
                        <Button size="sm" variant="ghost" onClick={() => cancelBookTask(t)}>
                          取消
                        </Button>
                      )}
                    </div>
                  ) : (
                    <span className="text-xs text-slate-400">只读</span>
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

async function confirmTask(
  t: Task,
  reloadNeeds: () => void,
  reload: () => void,
  toast: ReturnType<typeof useToast>
) {
  try {
    await api.post(`/api/tasks/${t.id}/verify-nas`);
    toast.success(`已确认《${t.title}》并触发 NAS 核验`);
    reloadNeeds();
    reload();
  } catch (e) {
    toast.error(e instanceof ApiError ? e.message : "确认失败");
  }
}
