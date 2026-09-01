// 系统设置页：键值列表 + 修改（仅超级管理员）+ 修改密码。
import { useState } from "react";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can } from "../lib/permissions";
import { api, changePassword } from "../lib/api";
import { ApiError, type Setting } from "../lib/types";
import { MailCodeSettings } from "../features/system/MailCodeSettings";
import { WebhookSettings } from "../features/system/WebhookSettings";
import { DownloadSearchSettings } from "../features/system/DownloadSearchSettings";
import {
  Button,
  Card,
  CardHeader,
  Dialog,
  ErrorBox,
  Input,
  Spinner,
} from "../components/ui";

const GROUP_RULES = [
  { title: "获取与重试", description: "下载、任务租约、退避与重试参数", pattern: /download|acquisition|task|retry|backoff|lease/i },
  { title: "账户与额度", description: "账号每日额度与注册策略", pattern: /account|quota|daily|registration/i },
  { title: "Worker", description: "节点心跳、槽位与执行参数", pattern: /worker|node|slot|heartbeat/i },
  { title: "存储", description: "NAS、暂存与文件校验", pattern: /storage|nas|file|staging/i },
  { title: "导入与安全", description: "书目导入、审计与安全边界", pattern: /import|security|audit|limit/i },
] as const;

function settingText(value: unknown): string {
  return typeof value === "string" ? value : JSON.stringify(value);
}

function parseSettingInput(value: string): unknown {
  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}

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
      await api.put(`/api/settings/${editing.key}`, { value: parseSettingInput(value) });
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

  const settings = (data ?? []).filter(
    (setting) =>
      ![
        "mail_code_provider",
        "global_download_paused",
        "webhook_notification_config",
        "download_search_options",
      ].includes(setting.key),
  );
  const grouped: Array<{ title: string; description: string; pattern: RegExp; items: Setting[] }> = GROUP_RULES.map((group) => ({
    ...group,
    items: settings.filter((setting) => group.pattern.test(setting.key)),
  }));
  const categorized = new Set(grouped.flatMap((group) => group.items.map((item) => item.key)));
  grouped.push({
    title: "其他受控参数",
    description: "未归类的兼容设置；修改前请确认服务端含义",
    pattern: /.*/,
    items: settings.filter((setting) => !categorized.has(setting.key)),
  });

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

      <MailCodeSettings canManage={canManage} />

      <WebhookSettings canManage={canManage} />

      <DownloadSearchSettings canManage={canManage} />

      <Card className="p-5">
        <div className="text-sm font-semibold text-slate-800">其余运行参数</div>
        <p className="mt-1 text-xs leading-5 text-slate-500">
          获取、重试、Worker、存储和安全边界由部署配置与对应业务页面管理，不在这里提供任意键值新增入口。
        </p>
      </Card>

      {settings.length > 0 && (
        <details className="rounded-lg border border-slate-200 bg-white">
          <summary className="cursor-pointer px-5 py-4 text-sm font-medium text-slate-700">
            兼容设置（高级，{settings.length} 项）
          </summary>
          <div className="grid gap-4 border-t border-slate-100 p-4 lg:grid-cols-2">
            {grouped.filter((group) => group.items.length > 0).map((group) => (
              <Card key={group.title}>
                <CardHeader title={group.title} description={group.description} />
                <div className="divide-y divide-slate-100">
                  {group.items.map((setting) => {
                    const display = settingText(setting.value);
                    return (
                      <div key={setting.key} className="flex items-center justify-between gap-3 px-5 py-3">
                        <div className="min-w-0">
                          <div className="font-mono text-xs font-medium text-slate-700">{setting.key}</div>
                          <div className="mt-1 truncate text-xs text-slate-500" title={display}>{display || "-"}</div>
                        </div>
                        <Button size="sm" variant="secondary" onClick={() => {
                          setEditing(setting);
                          setValue(display);
                        }}>编辑</Button>
                      </div>
                    );
                  })}
                </div>
              </Card>
            ))}
          </div>
        </details>
      )}

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
