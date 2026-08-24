// 执行会话页：列表 + 终止。
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can } from "../lib/permissions";
import { api } from "../lib/api";
import { ApiError, type Session } from "../lib/types";
import { formatTime, shortId } from "../lib/format";
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

export function SessionsPage() {
  const { user } = useAuth();
  const toast = useToast();
  const canManage = can(user?.role, "manage_session");
  const { data, loading, error, reload } = useApi<Session[]>(() => api.get("/api/sessions"));

  const terminate = async (s: Session) => {
    if (!window.confirm(`确认终止会话 #${shortId(s.id)}（节点 ${shortId(s.node_id)} 槽位 ${s.slot_index}）？`)) return;
    try {
      await api.post(`/api/sessions/${s.id}/terminate`, { reason: "管理员终止" });
      toast.success("会话已终止");
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "终止失败");
    }
  };

  if (loading) return <Spinner label="正在加载会话..." />;
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">执行会话</h2>
          <p className="text-sm text-slate-500">当前与历史的浏览器执行会话</p>
        </div>
        <Button variant="secondary" size="sm" onClick={reload}>
          刷新
        </Button>
      </div>
      <Card>
        <Table
          headers={["会话", "节点", "槽位", "类型", "状态", "本地端口", "完成数", "开始时间", "操作"]}
          empty={!data || data.length === 0 ? <EmptyRow colSpan={9} text="暂无会话" /> : undefined}
        >
          {(data ?? []).map((s) => (
            <tr key={s.id}>
              <Td className="font-mono text-xs">{shortId(s.id)}</Td>
              <Td className="font-mono text-xs text-slate-500">{shortId(s.node_id)}</Td>
              <Td className="text-xs">#{s.slot_index}</Td>
              <Td className="text-xs text-slate-500">{s.task_type}</Td>
              <Td>
                <StatusBadge status={s.status} />
              </Td>
              <Td className="font-mono text-xs text-slate-500">{s.local_forward_port}</Td>
              <Td className="text-xs text-slate-500">{s.completed_count}</Td>
              <Td className="text-xs text-slate-500">{formatTime(s.started_at)}</Td>
              <Td>
                {canManage && s.status !== "已结束" && s.status !== "失败" ? (
                  <Button size="sm" variant="danger" onClick={() => terminate(s)}>
                    终止
                  </Button>
                ) : (
                  <span className="text-xs text-slate-300">-</span>
                )}
              </Td>
            </tr>
          ))}
        </Table>
      </Card>
    </div>
  );
}
