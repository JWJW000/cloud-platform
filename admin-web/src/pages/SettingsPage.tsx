// 系统设置页：键值列表 + 修改（仅超级管理员）+ 修改密码。
import { useState } from "react";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can, deniedMessage } from "../lib/permissions";
import { api, changePassword } from "../lib/api";
import { ApiError, type Setting } from "../lib/types";
import {
  Button,
  Card,
  CardHeader,
  Dialog,
  EmptyRow,
  ErrorBox,
  Input,
  Spinner,
  Table,
  Td,
} from "../components/ui";

export function SettingsPage() {
  const { user } = useAuth();
  const toast = useToast();
  const canManage = can(user?.role, "manage_settings");
  const { data, loading, error, reload } = useApi<Setting[]>(() => api.get("/api/settings"));

  const [editing, setEditing] = useState<Setting | null>(null);
  const [value, setValue] = useState("");
  const [saving, setSaving] = useState(false);

  // 修改密码
  const [pwdOpen, setPwdOpen] = useState(false);
  const [oldPwd, setOldPwd] = useState("");
  const [newPwd, setNewPwd] = useState("");
  const [confirmPwd, setConfirmPwd] = useState("");
  const [pwdError, setPwdError] = useState<string | null>(null);

  const saveSetting = async () => {
    if (!editing) return;
    setSaving(true);
    try {
      await api.put(`/api/settings/${editing.key}`, { value });
      toast.success(`设置 ${editing.key} 已更新`);
      setEditing(null);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "保存失败");
    } finally {
      setSaving(false);
    }
  };

  const changePwd = async () => {
    setPwdError(null);
    if (newPwd.length < 8) {
      setPwdError("新密码至少 8 位");
      return;
    }
    if (newPwd !== confirmPwd) {
      setPwdError("两次输入的新密码不一致");
      return;
    }
    try {
      await changePassword({ old_password: oldPwd, new_password: newPwd });
      toast.success("密码已修改");
      setPwdOpen(false);
      setOldPwd("");
      setNewPwd("");
      setConfirmPwd("");
    } catch (e) {
      setPwdError(e instanceof ApiError ? e.message : "修改失败");
    }
  };

  if (loading) return <Spinner label="正在加载设置..." />;
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  return (
    <div className="space-y-5">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">系统设置</h2>
          <p className="text-sm text-slate-500">运行参数与账号安全</p>
        </div>
        <div className="flex items-center gap-2">
          <Button variant="secondary" onClick={() => setPwdOpen(true)}>
            修改密码
          </Button>
          <Button variant="secondary" size="sm" onClick={reload}>
            刷新
          </Button>
        </div>
      </div>

      <Card>
        <CardHeader
          title="设置项"
          description={canManage ? "点击编辑可修改" : deniedMessage("manage_settings")}
        />
        <Table
          headers={["键", "值", "操作"]}
          empty={!data || data.length === 0 ? <EmptyRow colSpan={3} text="暂无设置项" /> : undefined}
        >
          {(data ?? []).map((s) => (
            <tr key={s.key}>
              <Td className="font-mono text-xs font-medium text-slate-700">{s.key}</Td>
              <Td className="max-w-96">
                <div className="truncate text-xs text-slate-600" title={s.value}>
                  {s.value || "-"}
                </div>
              </Td>
              <Td>
                {canManage ? (
                  <Button
                    size="sm"
                    variant="secondary"
                    onClick={() => {
                      setEditing(s);
                      setValue(s.value);
                    }}
                  >
                    编辑
                  </Button>
                ) : (
                  <span className="text-xs text-slate-300">-</span>
                )}
              </Td>
            </tr>
          ))}
        </Table>
      </Card>

      <Dialog
        open={!!editing}
        title={`编辑设置 ${editing?.key ?? ""}`}
        onClose={() => setEditing(null)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setEditing(null)}>
              取消
            </Button>
            <Button loading={saving} onClick={saveSetting}>
              保存
            </Button>
          </>
        }
      >
        <Input label="值" value={value} onChange={(e) => setValue(e.target.value)} />
      </Dialog>

      <Dialog
        open={pwdOpen}
        title="修改密码"
        onClose={() => setPwdOpen(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setPwdOpen(false)}>
              取消
            </Button>
            <Button onClick={changePwd}>确认修改</Button>
          </>
        }
      >
        <div className="space-y-4">
          <Input label="当前密码" type="password" value={oldPwd} onChange={(e) => setOldPwd(e.target.value)} />
          <Input label="新密码（至少 8 位）" type="password" value={newPwd} onChange={(e) => setNewPwd(e.target.value)} />
          <Input label="确认新密码" type="password" value={confirmPwd} onChange={(e) => setConfirmPwd(e.target.value)} />
          {pwdError && <div className="rounded bg-red-50 px-3 py-2 text-sm text-red-700">{pwdError}</div>}
        </div>
      </Dialog>
    </div>
  );
}
