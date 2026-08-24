// 待确认事项页：人工验证码输入、风控人工确认、超时释放管理。
import { useState } from "react";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can } from "../lib/permissions";
import { cancelManualAction, listManualActions, resolveManualAction } from "../lib/api";
import { ApiError, type ManualAction } from "../lib/types";
import { formatTime, shortId } from "../lib/format";
import {
  Button,
  Card,
  Dialog,
  EmptyRow,
  ErrorBox,
  Input,
  Select,
  Spinner,
  StatusBadge,
  Table,
  Td,
} from "../components/ui";
import { KeyRound } from "lucide-react";

export function ManualActionsPage() {
  const { user } = useAuth();
  const toast = useToast();
  const canManage = can(user?.role, "manage_task");
  const [statusFilter, setStatusFilter] = useState("待处理");

  const { data, loading, error, reload } = useApi<ManualAction[]>(
    () => listManualActions({ status: statusFilter === "全部" ? undefined : statusFilter, limit: 100 }),
    [statusFilter]
  );

  const [resolvingAction, setResolvingAction] = useState<ManualAction | null>(null);
  const [inputCode, setInputCode] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const handleOpenResolve = (action: ManualAction) => {
    setResolvingAction(action);
    setInputCode("");
  };

  const handleResolveSubmit = async () => {
    if (!resolvingAction) return;
    if (!inputCode.trim()) {
      toast.error("请输入验证码或确认内容");
      return;
    }
    setSubmitting(true);
    try {
      await resolveManualAction(resolvingAction.id, inputCode.trim());
      toast.success("验证码已提交，Worker 将继续执行");
      setResolvingAction(null);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "提交失败");
    } finally {
      setSubmitting(false);
    }
  };

  const handleCancelAction = async (action: ManualAction) => {
    try {
      await cancelManualAction(action.id);
      toast.success("该事项已取消");
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "取消失败");
    }
  };

  if (loading) return <Spinner label="正在加载待确认事项..." />;
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">待确认事项</h2>
          <p className="text-sm text-slate-500">人工确认闭环：输入邮箱验证码、图片验证码或确认风控，驱动 Worker 继续执行</p>
        </div>
        <div className="flex items-center gap-2">
          <Select value={statusFilter} onChange={(e) => setStatusFilter(e.target.value)}>
            <option value="待处理">待处理</option>
            <option value="已解决">已解决</option>
            <option value="已过期">已过期</option>
            <option value="全部">全部状态</option>
          </Select>
          <Button variant="secondary" size="sm" onClick={reload}>
            刷新
          </Button>
        </div>
      </div>

      <Card>
        <Table
          headers={["类型", "任务类型", "提示说明", "节点", "过期时间", "状态", "操作"]}
          empty={!data || data.length === 0 ? <EmptyRow colSpan={7} text="当前无待确认事项" /> : undefined}
        >
          {(data ?? []).map((a) => (
            <tr key={a.id}>
              <Td>
                <div className="flex items-center gap-1.5 font-medium text-slate-800">
                  <KeyRound className="h-4 w-4 text-blue-600" />
                  {a.action_type}
                </div>
              </Td>
              <Td className="text-xs text-slate-600">{a.task_type}</Td>
              <Td className="max-w-72 truncate text-xs text-slate-700 font-mono" title={a.prompt}>
                {a.prompt}
              </Td>
              <Td className="text-xs text-slate-500 font-mono">
                {a.node_id ? shortId(a.node_id) : "-"}
              </Td>
              <Td className="text-xs text-slate-500">{formatTime(a.expires_at)}</Td>
              <Td>
                <StatusBadge status={a.status} />
              </Td>
              <Td>
                {canManage && a.status === "待处理" ? (
                  <div className="flex items-center gap-1">
                    <Button size="sm" onClick={() => handleOpenResolve(a)}>
                      输入验证码
                    </Button>
                    <Button size="sm" variant="danger" onClick={() => handleCancelAction(a)}>
                      取消
                    </Button>
                  </div>
                ) : (
                  <span className="text-xs text-slate-400">-</span>
                )}
              </Td>
            </tr>
          ))}
        </Table>
      </Card>

      {/* 验证码输入 Dialog */}
      <Dialog
        open={!!resolvingAction}
        title="输入验证码 / 人工确认"
        onClose={() => !submitting && setResolvingAction(null)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setResolvingAction(null)}>
              取消
            </Button>
            <Button loading={submitting} onClick={handleResolveSubmit}>
              提交并继续
            </Button>
          </>
        }
      >
        {resolvingAction && (
          <div className="space-y-4">
            <div className="rounded-md bg-blue-50 p-3 text-xs text-blue-800 border border-blue-200">
              <div className="font-semibold mb-1">Worker 提示信息：</div>
              <div className="font-mono">{resolvingAction.prompt}</div>
            </div>

            <Input
              label="验证码 / 确认内容"
              value={inputCode}
              onChange={(e) => setInputCode(e.target.value)}
              placeholder="请输入接收到的验证码"
              autoFocus
            />

            <p className="text-xs text-slate-400">
              提交后将通过安全通道仅下发给当前持有执行租约的 Worker，验证码不会记录进操作日志。
            </p>
          </div>
        )}
      </Dialog>
    </div>
  );
}
