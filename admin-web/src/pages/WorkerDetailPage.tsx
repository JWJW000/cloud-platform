// Worker 详情页：节点信息 + 槽位 + 证书（含指纹、吊销）。
import { useParams } from "react-router-dom";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can, deniedMessage } from "../lib/permissions";
import { api } from "../lib/api";
import { ApiError, type NodeCertificate, type WorkerNode, type WorkerSlot } from "../lib/types";
import { formatBytes, formatTime, shortId } from "../lib/format";
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

export function WorkerDetailPage() {
  const { id = "" } = useParams();
  const { user } = useAuth();
  const toast = useToast();
  const node = useApi<WorkerNode>(() => api.get(`/api/workers/${id}`), [id]);
  const slots = useApi<WorkerSlot[]>(() => api.get(`/api/workers/${id}/slots`), [id]);
  const certs = useApi<NodeCertificate[]>(() => api.get(`/api/workers/${id}/certificates`), [id]);

  const canApprove = can(user?.role, "approve_node");

  const revokeCert = async (fingerprint: string) => {
    try {
      await api.post(`/api/workers/certificates/${fingerprint}/revoke`);
      toast.success("证书已吊销");
      certs.reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "吊销失败");
    }
  };

  if (node.loading) return <Spinner label="正在加载节点详情..." />;
  if (node.error) return <ErrorBox message={node.error} onRetry={node.reload} />;
  const n = node.data;
  if (!n) return null;

  return (
    <div className="space-y-5">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">{n.name}</h2>
          <p className="text-sm text-slate-500">
            {n.hostname} · {n.os} {n.os_version} · Agent {n.agent_version}
          </p>
        </div>
        <StatusBadge status={n.status} />
      </div>

      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <InfoCard label="注册状态" value={n.registration_status} />
        <InfoCard label="安装标识" value={shortId(n.installation_id)} />
        <InfoCard label="来源 IP" value={n.first_seen_ip ?? "-"} />
        <InfoCard label="注册申请时间" value={formatTime(n.last_registration_at)} />
        <InfoCard label="配置槽位" value={String(n.configured_slots ?? "-")} />
        <InfoCard label="公钥指纹" value={shortId(n.public_key_fingerprint)} />
        <InfoCard label="NAS 健康" value={n.nas_healthy ? "正常" : "异常"} />
        <InfoCard label="NAS 剩余" value={`${n.nas_free_gb ?? 0} GB`} />
      </div>

      {n.reject_reason && (
        <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
          拒绝原因：{n.reject_reason}
        </div>
      )}

      <Card>
        <CardHeader title="执行槽位" />
        <Table
          headers={["槽位", "状态", "会话", "任务", "说明"]}
          empty={!slots.data || slots.data.length === 0 ? <EmptyRow colSpan={5} text="暂无槽位" /> : undefined}
        >
          {(slots.data ?? []).map((s) => (
            <tr key={s.slot_index}>
              <Td>#{s.slot_index}</Td>
              <Td>
                <StatusBadge status={s.status} />
              </Td>
              <Td className="text-xs text-slate-500">{s.session_id ? shortId(s.session_id) : "-"}</Td>
              <Td className="text-xs text-slate-500">{s.task_id ? shortId(s.task_id) : "-"}</Td>
              <Td className="text-xs text-slate-400">{s.detail ?? "-"}</Td>
            </tr>
          ))}
        </Table>
      </Card>

      <Card>
        <CardHeader
          title="客户端证书"
          description="批准后由 Master 签发；吊销后节点无法再建立任务链路"
        />
        <Table
          headers={["指纹", "签发时间", "到期时间", "状态", "操作"]}
          empty={!certs.data || certs.data.length === 0 ? <EmptyRow colSpan={5} text="暂无证书（待审核节点不签发）" /> : undefined}
        >
          {(certs.data ?? []).map((c) => (
            <tr key={c.id}>
              <Td className="font-mono text-xs">{c.fingerprint.slice(0, 24)}…</Td>
              <Td className="text-xs">{formatTime(c.issued_at)}</Td>
              <Td className="text-xs">{formatTime(c.not_after)}</Td>
              <Td>
                {c.revoked_at ? (
                  <span className="text-xs text-red-600">已吊销</span>
                ) : (
                  <span className="text-xs text-green-600">有效</span>
                )}
              </Td>
              <Td>
                {!c.revoked_at && (
                  canApprove ? (
                    <Button size="sm" variant="danger" onClick={() => revokeCert(c.fingerprint)}>
                      吊销
                    </Button>
                  ) : (
                    <span className="text-xs text-slate-400" title={deniedMessage("approve_node")}>
                      仅超级管理员
                    </span>
                  )
                )}
              </Td>
            </tr>
          ))}
        </Table>
      </Card>

      <Card>
        <CardHeader title="资源使用" />
        <div className="grid grid-cols-2 gap-4 p-5 text-sm lg:grid-cols-4">
          <div>
            <div className="text-slate-500">CPU</div>
            <div className="mt-1 font-medium">{n.cpu_percent?.toFixed(1) ?? "-"}%</div>
          </div>
          <div>
            <div className="text-slate-500">内存</div>
            <div className="mt-1 font-medium">
              {n.memory_used_mb ? formatBytes(n.memory_used_mb * 1024 * 1024) : "-"} /{" "}
              {n.memory_total_mb ? formatBytes(n.memory_total_mb * 1024 * 1024) : "-"}
            </div>
          </div>
          <div>
            <div className="text-slate-500">暂存区剩余</div>
            <div className="mt-1 font-medium">{n.staging_free_gb ?? 0} GB</div>
          </div>
          <div>
            <div className="text-slate-500">最近心跳</div>
            <div className="mt-1 font-medium">{formatTime(n.last_heartbeat_at)}</div>
          </div>
        </div>
      </Card>
    </div>
  );
}

function InfoCard({ label, value }: { label: string; value: string }) {
  return (
    <Card className="p-4">
      <div className="text-xs text-slate-500">{label}</div>
      <div className="mt-1 truncate text-sm font-medium text-slate-800" title={value}>
        {value}
      </div>
    </Card>
  );
}
