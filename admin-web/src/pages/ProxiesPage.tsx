// 代理页：列表 + 新增 + 删除 + 状态更新。
import { useState } from "react";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can } from "../lib/permissions";
import { api } from "../lib/api";
import { ApiError, type Proxy } from "../lib/types";
import { formatTime } from "../lib/format";
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

export function ProxiesPage() {
  const { user } = useAuth();
  const toast = useToast();
  const canManage = can(user?.role, "manage_proxy");
  const { data, loading, error, reload } = useApi<Proxy[]>(() => api.get("/api/proxies"));

  const [creating, setCreating] = useState(false);
  const [label, setLabel] = useState("");
  const [scheme, setScheme] = useState("http");
  const [host, setHost] = useState("");
  const [port, setPort] = useState("1080");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const create = async () => {
    setFormError(null);
    if (!host.trim()) {
      setFormError("主机地址不能为空");
      return;
    }
    setSubmitting(true);
    try {
      await api.post("/api/proxies", {
        label: label || undefined,
        scheme,
        host: host.trim(),
        port: Number(port),
        username: username || undefined,
        password: password || undefined,
      });
      toast.success(`代理 ${scheme}://${host.trim()}:${port} 已创建`);
      setCreating(false);
      setHost("");
      setLabel("");
      reload();
    } catch (e) {
      setFormError(e instanceof ApiError ? e.message : "创建失败");
    } finally {
      setSubmitting(false);
    }
  };

  const remove = async (p: Proxy) => {
    if (!window.confirm(`确认删除代理 ${p.host}:${p.port}？`)) return;
    try {
      await api.delete(`/api/proxies/${p.id}`);
      toast.success("代理已删除");
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "删除失败");
    }
  };

  if (loading) return <Spinner label="正在加载代理..." />;
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">代理</h2>
          <p className="text-sm text-slate-500">出口代理池：一本书固定一个代理 IP</p>
        </div>
        <div className="flex items-center gap-2">
          {canManage && <Button onClick={() => setCreating(true)}>新增代理</Button>}
          <Button variant="secondary" size="sm" onClick={reload}>
            刷新
          </Button>
        </div>
      </div>
      <Card>
        <Table
          headers={["标签", "地址", "状态", "出口 IP", "成功率", "占用会话", "最近检查", "操作"]}
          empty={!data || data.length === 0 ? <EmptyRow colSpan={8} text="暂无代理" /> : undefined}
        >
          {(data ?? []).map((p) => {
            const total = p.success_count + p.failure_count;
            const rate = total > 0 ? Math.round((p.success_count / total) * 100) : null;
            return (
              <tr key={p.id}>
                <Td className="text-xs text-slate-600">{p.label ?? "-"}</Td>
                <Td className="font-mono text-xs text-slate-700">
                  {p.scheme}://{p.host}:{p.port}
                </Td>
                <Td>
                  <StatusBadge status={p.status} />
                </Td>
                <Td className="font-mono text-xs text-slate-500">{p.exit_ip ?? "-"}</Td>
                <Td className="text-xs text-slate-500">{rate === null ? "-" : `${rate}%`}</Td>
                <Td className="text-xs text-slate-500">{p.lease_session_id ? "占用中" : "-"}</Td>
                <Td className="text-xs text-slate-500">{formatTime(p.last_checked_at)}</Td>
                <Td>
                  {canManage && (
                    <Button size="sm" variant="ghost" onClick={() => remove(p)}>
                      删除
                    </Button>
                  )}
                </Td>
              </tr>
            );
          })}
        </Table>
      </Card>

      <Dialog
        open={creating}
        title="新增代理"
        onClose={() => setCreating(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setCreating(false)}>
              取消
            </Button>
            <Button loading={submitting} onClick={create}>
              创建
            </Button>
          </>
        }
      >
        <div className="space-y-4">
          <Input label="标签（可选）" value={label} onChange={(e) => setLabel(e.target.value)} placeholder="例如：主代理" />
          <Select label="协议" value={scheme} onChange={(e) => setScheme(e.target.value)}>
            <option value="http">http</option>
            <option value="socks5">socks5</option>
          </Select>
          <div className="grid grid-cols-3 gap-3">
            <div className="col-span-2">
              <Input label="主机" value={host} onChange={(e) => setHost(e.target.value)} placeholder="127.0.0.1" />
            </div>
            <Input label="端口" type="number" value={port} onChange={(e) => setPort(e.target.value)} />
          </div>
          <Input label="用户名（可选）" value={username} onChange={(e) => setUsername(e.target.value)} />
          <Input label="密码（可选）" type="password" value={password} onChange={(e) => setPassword(e.target.value)} />
          {formError && <div className="rounded bg-red-50 px-3 py-2 text-sm text-red-700">{formError}</div>}
        </div>
      </Dialog>
    </div>
  );
}
