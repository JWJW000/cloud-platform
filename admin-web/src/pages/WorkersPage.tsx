// Worker 节点页（V5）：列表 + 注册状态筛选 + 批准/拒绝/禁用/启用 + 详情。
import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can, deniedMessage } from "../lib/permissions";
import { api } from "../lib/api";
import { ApiError, type WorkerNode } from "../lib/types";
import { relativeTime, shortId } from "../lib/format";
import {
  Badge,
  Button,
  Card,
  Dialog,
  EmptyRow,
  ErrorBox,
  Input,
  Select,
  SkeletonTable,
  StatusBadge,
  Table,
  Td,
} from "../components/ui";

type Filter = "全部" | "待审核" | "已批准" | "已拒绝" | "已过期";

const FILTERS: Filter[] = ["全部", "待审核", "已批准", "已拒绝", "已过期"];

export function WorkersPage() {
  const { user } = useAuth();
  const toast = useToast();
  const [filter, setFilter] = useState<Filter>("全部");
  const { data: nodes, loading, error, reload } = useApi<WorkerNode[]>(
    () => api.get("/api/workers", { registration_status: filter === "全部" ? undefined : filter }),
    [filter],
  );

  // 批准弹窗
  const [approving, setApproving] = useState<WorkerNode | null>(null);
  const [slots, setSlots] = useState("5");
  const [remark, setRemark] = useState("");
  const [submitting, setSubmitting] = useState(false);

  // 拒绝弹窗
  const [rejecting, setRejecting] = useState<WorkerNode | null>(null);
  const [rejectReason, setRejectReason] = useState("");
  const [rejectError, setRejectError] = useState<string | null>(null);

  const canApprove = can(user?.role, "approve_node");

  const pendingCount = useMemo(
    () => (nodes ?? []).filter((n) => n.registration_status === "待审核").length,
    [nodes],
  );

  const handleApprove = async () => {
    if (!approving) return;
    setSubmitting(true);
    try {
      const slotsNum = Number(slots);
      if (!Number.isInteger(slotsNum) || slotsNum < 1 || slotsNum > 50) {
        toast.error("槽位数必须在 1-50 之间");
        return;
      }
      await api.post(`/api/workers/${approving.id}/approve`, {
        configured_slots: slotsNum,
        remark: remark || undefined,
      });
      toast.success(`节点「${approving.name}」已批准`);
      setApproving(null);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "批准失败");
      // 409 状态冲突：刷新资源
      if (e instanceof ApiError && e.status === 409) reload();
    } finally {
      setSubmitting(false);
    }
  };

  const handleReject = async () => {
    if (!rejecting) return;
    setRejectError(null);
    setSubmitting(true);
    try {
      await api.post(`/api/workers/${rejecting.id}/reject`, { reason: rejectReason });
      toast.success(`节点「${rejecting.name}」已拒绝`);
      setRejecting(null);
      setRejectReason("");
      reload();
    } catch (e) {
      const msg = e instanceof ApiError ? e.message : "拒绝失败";
      setRejectError(msg);
      if (e instanceof ApiError && e.status === 409) reload();
    } finally {
      setSubmitting(false);
    }
  };

  const toggleEnabled = async (node: WorkerNode) => {
    const action = node.status === "已禁用" ? "enable" : "disable";
    try {
      await api.post(`/api/workers/${node.id}/${action}`);
      toast.success(`节点「${node.name}」已${action === "enable" ? "启用" : "禁用"}`);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "操作失败");
      if (e instanceof ApiError && e.status === 409) reload();
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">Worker 节点</h2>
          <p className="text-sm text-slate-500">
            {pendingCount > 0
              ? `有 ${pendingCount} 个节点等待审核`
              : "V5 直连注册：新节点自动出现在「待审核」列表"}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Select value={filter} onChange={(e) => setFilter(e.target.value as Filter)} aria-label="注册状态筛选">
            {FILTERS.map((f) => (
              <option key={f} value={f}>
                {f === "全部" ? "全部状态" : f}
              </option>
            ))}
          </Select>
          <Button variant="secondary" size="sm" onClick={reload}>
            刷新
          </Button>
        </div>
      </div>

      {error && <ErrorBox message={error} onRetry={reload} />}

      <Card>
        <Table
          headers={["名称", "状态", "注册状态", "系统", "槽位", "来源 IP", "心跳", "操作"]}
          empty={!loading && (!nodes || nodes.length === 0) ? <EmptyRow colSpan={8} text="暂无节点" /> : undefined}
        >
          {loading && (!nodes || nodes.length === 0) ? (
            <SkeletonTable columns={8} rows={5} />
          ) : (
            (nodes ?? []).map((n) => (
              <tr key={n.id}>
                <Td>
                  <Link to={`/workers/${n.id}`} className="font-medium text-blue-700 hover:underline">
                    {n.name}
                  </Link>
                  <div className="text-xs text-slate-400">#{shortId(n.id)}</div>
                </Td>
                <Td>
                  <StatusBadge status={n.status} />
                </Td>
                <Td>
                  <RegistrationBadge status={n.registration_status} />
                </Td>
                <Td className="text-xs text-slate-600">
                  {n.os} {n.os_version}
                </Td>
                <Td>
                  {n.configured_slots ?? n.requested_slots ?? n.max_slots}
                  <span className="text-xs text-slate-400">/ {n.max_slots}</span>
                </Td>
                <Td className="text-xs text-slate-500">{n.first_seen_ip ?? "-"}</Td>
                <Td className="text-xs text-slate-500">
                  {n.connected ? (
                    <Badge className="bg-green-100 text-green-700">在线</Badge>
                  ) : (
                    relativeTime(n.last_heartbeat_at)
                  )}
                </Td>
                <Td>
                  <div className="flex items-center gap-1">
                    {n.registration_status === "待审核" && (
                      <>
                        {canApprove ? (
                          <>
                            <Button size="sm" variant="success" onClick={() => setApproving(n)}>
                              批准
                            </Button>
                            <Button size="sm" variant="danger" onClick={() => setRejecting(n)}>
                              拒绝
                            </Button>
                          </>
                        ) : (
                          <span className="text-xs text-slate-400" title={deniedMessage("approve_node")}>
                            仅超级管理员可审批
                          </span>
                        )}
                      </>
                    )}
                    {n.status === "已禁用" ? (
                      <Button size="sm" variant="secondary" onClick={() => toggleEnabled(n)}>
                        启用
                      </Button>
                    ) : (
                      <Button size="sm" variant="ghost" onClick={() => toggleEnabled(n)}>
                        禁用
                      </Button>
                    )}
                    <Link to={`/workers/${n.id}`}>
                      <Button size="sm" variant="ghost">
                        详情
                      </Button>
                    </Link>
                  </div>
                </Td>
              </tr>
            ))
          )}
        </Table>
      </Card>

      {/* 批准弹窗 */}
      <Dialog
        open={!!approving}
        title={`批准节点「${approving?.name ?? ""}」`}
        onClose={() => setApproving(null)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setApproving(null)}>
              取消
            </Button>
            <Button variant="success" loading={submitting} onClick={handleApprove}>
              批准并签发证书
            </Button>
          </>
        }
      >
        <div className="space-y-4">
          <p className="text-sm text-slate-600">
            批准后将签发正式客户端证书与节点令牌，节点即可上线领取任务。
          </p>
          <Input
            label="配置槽位数（1-50）"
            type="number"
            min={1}
            max={50}
            value={slots}
            onChange={(e) => setSlots(e.target.value)}
          />
          <Input label="备注（可选）" value={remark} onChange={(e) => setRemark(e.target.value)} placeholder="例如：办公室下载节点" />
        </div>
      </Dialog>

      {/* 拒绝弹窗 */}
      <Dialog
        open={!!rejecting}
        title={`拒绝节点「${rejecting?.name ?? ""}」`}
        onClose={() => setRejecting(null)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setRejecting(null)}>
              取消
            </Button>
            <Button variant="danger" loading={submitting} onClick={handleReject}>
              确认拒绝
            </Button>
          </>
        }
      >
        <div className="space-y-3">
          <Input
            label="拒绝原因（必填）"
            value={rejectReason}
            onChange={(e) => setRejectReason(e.target.value)}
            placeholder="例如：来源设备未知，请联系管理员确认"
          />
          {rejectError && <div className="rounded bg-red-50 px-3 py-2 text-sm text-red-700">{rejectError}</div>}
        </div>
      </Dialog>
    </div>
  );
}

function RegistrationBadge({ status }: { status: string }) {
  const color =
    status === "已批准"
      ? "bg-green-100 text-green-700"
      : status === "待审核"
        ? "bg-amber-100 text-amber-700"
        : status === "已拒绝" || status === "已过期"
          ? "bg-red-100 text-red-700"
          : "bg-slate-100 text-slate-600";
  return <Badge className={color}>{status}</Badge>;
}
