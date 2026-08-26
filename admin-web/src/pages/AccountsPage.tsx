// 下载账号页：列表 + 新增 + 批量文件导入（待注册/已注册） + 禁用/启用。
import { useRef, useState } from "react";
import { useApi } from "../hooks/useApi";
import { useToast } from "../context/ToastContext";
import { useAuth } from "../context/AuthContext";
import { can } from "../lib/permissions";
import {
  api,
  commitAccountsImport,
  previewAccountsImport,
  previewOutlookAccounts,
  syncOutlookAccounts,
} from "../lib/api";
import {
  AccountImportMode,
  AccountImportPreview,
  AccountListResponse,
  ApiError,
  OutlookPreviewAccount,
  OutlookPreviewResponse,
  type Account,
} from "../lib/types";
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
import { FileUp, Plus, RefreshCw } from "lucide-react";

export function AccountsPage() {
  const { user } = useAuth();
  const toast = useToast();
  const canManage = can(user?.role, "manage_account");
  const isSuperAdmin = user?.role === "超级管理员";
  const { data, loading, error, reload } = useApi<AccountListResponse>(() =>
    api.get("/api/accounts", { limit: 200 }),
  );

  // 单个新增状态
  const [creating, setCreating] = useState(false);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [nickname, setNickname] = useState("");
  const [dailyLimit, setDailyLimit] = useState("50");
  const [submitting, setSubmitting] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  // 批量导入向导状态
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [importWizardOpen, setImportWizardOpen] = useState(false);
  const [importStep, setImportStep] = useState<"select" | "preview">("select");
  const [selectedFile, setSelectedFile] = useState<File | null>(null);
  const [importMode, setImportMode] = useState<AccountImportMode>("待注册");
  const [createRegBatch, setCreateRegBatch] = useState(true);
  const [regBatchName, setRegBatchName] = useState("");
  const [regPriority, setRegPriority] = useState("10");
  const [startImmediately, setStartImmediately] = useState(false);
  const [previewing, setPreviewing] = useState(false);
  const [previewData, setPreviewData] = useState<AccountImportPreview | null>(null);
  const [committing, setCommitting] = useState(false);

  // Outlook 同步向导状态
  const [outlookWizardOpen, setOutlookWizardOpen] = useState(false);
  const [outlookStep, setOutlookStep] = useState<"settings" | "select">("settings");
  const [defaultPassword, setDefaultPassword] = useState("");
  const [outlookCreateBatch, setOutlookCreateBatch] = useState(true);
  const [outlookBatchName, setOutlookBatchName] = useState("");
  const [outlookPriority, setOutlookPriority] = useState("10");
  const [outlookStartImmediately, setOutlookStartImmediately] = useState(false);
  const [previewingOutlook, setPreviewingOutlook] = useState(false);
  const [outlookPreview, setOutlookPreview] = useState<OutlookPreviewResponse | null>(null);
  const [selectedOutlookEmails, setSelectedOutlookEmails] = useState<Set<string>>(new Set());
  const [syncingOutlook, setSyncingOutlook] = useState(false);

  const create = async () => {
    setFormError(null);
    if (!email.trim()) {
      setFormError("邮箱不能为空");
      return;
    }
    setSubmitting(true);
    try {
      await api.post("/api/accounts", {
        email: email.trim(),
        password: password || undefined,
        nickname: nickname || undefined,
        daily_limit: Number(dailyLimit),
      });
      toast.success(`账号 ${email.trim()} 已创建`);
      setCreating(false);
      setEmail("");
      setPassword("");
      setNickname("");
      reload();
    } catch (e) {
      setFormError(e instanceof ApiError ? e.message : "创建失败");
    } finally {
      setSubmitting(false);
    }
  };

  const toggle = async (a: Account) => {
    const next = a.status === "已禁用" ? "启用" : "禁用";
    try {
      await api.put(`/api/accounts/${a.id}/status`, { status: next });
      toast.success(`账号 ${a.email} 已${next}`);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "操作失败");
    }
  };

  const handleOpenImportWizard = () => {
    setImportStep("select");
    setSelectedFile(null);
    setImportMode("待注册");
    setCreateRegBatch(true);
    setRegBatchName("");
    setRegPriority("10");
    setStartImmediately(false);
    setPreviewData(null);
    setImportWizardOpen(true);
  };

  const handleUploadAndPreview = async () => {
    if (!selectedFile) {
      toast.error("请先选择账号文件");
      return;
    }
    setPreviewing(true);
    try {
      const formData = new FormData();
      formData.append("file", selectedFile);

      const preview = await previewAccountsImport(formData);
      setPreviewData(preview);
      setImportStep("preview");
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "文件预检失败");
    } finally {
      setPreviewing(false);
    }
  };

  const handleCommitImport = async () => {
    if (!previewData) return;
    setCommitting(true);
    try {
      const res = await commitAccountsImport({
        import_token: previewData.import_token,
        mode: importMode,
        create_registration_batch: importMode === "待注册" ? createRegBatch : false,
        batch_name: regBatchName.trim() || undefined,
        priority: Number(regPriority),
        start_immediately: startImmediately,
      });

      let msg = `已成功导入 ${res.imported_accounts} 个${importMode}账号`;
      if (res.registration_batch) {
        msg += `，并创建注册批次「${res.registration_batch.name}」`;
      }
      toast.success(msg);
      setImportWizardOpen(false);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "提交导入失败");
    } finally {
      setCommitting(false);
    }
  };

  const handleDownloadTemplate = (format: "txt" | "csv") => {
    let content = "";
    let filename = "";
    if (format === "txt") {
      content = "user1@example.com----Password123\nuser2@example.com----Password456\n";
      filename = "accounts_template.txt";
    } else {
      content = "邮箱,密码,昵称\nuser1@example.com,Password123,昵称1\nuser2@example.com,Password456,昵称2\n";
      filename = "accounts_template.csv";
    }
    const blob = new Blob([new Uint8Array([0xef, 0xbb, 0xbf]), content], {
      type: "text/plain;charset=utf-8;",
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
  };

  const handleOpenOutlookWizard = () => {
    setOutlookStep("settings");
    setDefaultPassword("");
    setOutlookCreateBatch(true);
    setOutlookBatchName("");
    setOutlookPriority("10");
    setOutlookStartImmediately(false);
    setOutlookPreview(null);
    setSelectedOutlookEmails(new Set());
    setOutlookWizardOpen(true);
  };

  const handleFetchOutlookAccounts = async () => {
    if (defaultPassword.trim().length < 6 || defaultPassword.trim().length > 64) {
      toast.error("请先设置云端统一注册密码（6–64 字符）");
      return;
    }
    setPreviewingOutlook(true);
    try {
      const preview = await previewOutlookAccounts();
      setOutlookPreview(preview);
      setSelectedOutlookEmails(new Set(preview.accounts.map((a) => a.email)));
      setOutlookStep("select");
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "拉取 Outlook 账号清单失败");
    } finally {
      setPreviewingOutlook(false);
    }
  };

  const toggleOutlookAccount = (email: string) => {
    setSelectedOutlookEmails((prev) => {
      const next = new Set(prev);
      if (next.has(email)) {
        next.delete(email);
      } else {
        next.add(email);
      }
      return next;
    });
  };

  const toggleSelectAllOutlook = (checked: boolean) => {
    if (!outlookPreview) return;
    setSelectedOutlookEmails(
      new Set(checked ? outlookPreview.accounts.map((a) => a.email) : []),
    );
  };

  const handleSyncOutlook = async () => {
    if (!outlookPreview) return;
    if (selectedOutlookEmails.size === 0) {
      toast.error("请至少勾选一个要注册的账号");
      return;
    }
    setSyncingOutlook(true);
    try {
      const res = await syncOutlookAccounts({
        default_password: defaultPassword.trim(),
        emails: Array.from(selectedOutlookEmails),
        create_batch: outlookCreateBatch,
        batch_name: outlookBatchName.trim() || undefined,
        priority: Number(outlookPriority),
        start_immediately: outlookStartImmediately,
      });
      let msg = `已同步新增 ${res.inserted} 个待注册账号`;
      if (res.duplicates > 0) msg += `，跳过 ${res.duplicates} 个已存在`;
      if (res.registration_batch) {
        msg += `，已创建注册批次「${res.registration_batch.name}」`;
        if (outlookStartImmediately) msg += "（已下发 Worker 注册）";
      }
      toast.success(msg);
      setOutlookWizardOpen(false);
      reload();
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : "同步 Outlook 账号失败");
    } finally {
      setSyncingOutlook(false);
    }
  };

  if (loading) return <Spinner label="正在加载账号..." />;
  if (error) return <ErrorBox message={error} onRetry={reload} />;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold text-slate-900">下载账号</h2>
          <p className="text-sm text-slate-500">业务账号资源池：已注册可直接下载，待注册可统一创建注册批次</p>
        </div>
        <div className="flex items-center gap-2">
          {isSuperAdmin && (
            <Button variant="secondary" size="sm" onClick={handleOpenImportWizard}>
              <FileUp className="mr-1 h-4 w-4" />
              批量导入账号
            </Button>
          )}
          {isSuperAdmin && (
            <Button variant="secondary" size="sm" onClick={handleOpenOutlookWizard}>
              <RefreshCw className="mr-1 h-4 w-4" />
              同步 Outlook 账号
            </Button>
          )}
          {canManage && (
            <Button size="sm" onClick={() => setCreating(true)}>
              <Plus className="mr-1 h-4 w-4" />
              新增账号
            </Button>
          )}
          <Button variant="secondary" size="sm" onClick={reload}>
            刷新
          </Button>
        </div>
      </div>
      <Card>
        <Table
          headers={["邮箱", "昵称", "状态", "当日用量", "额度", "最近登录", "操作"]}
          empty={!data || data.items.length === 0 ? <EmptyRow colSpan={7} text="暂无账号" /> : undefined}
        >
          {(data?.items ?? []).map((a) => (
            <tr key={a.id}>
              <Td className="font-medium text-slate-800">{a.email}</Td>
              <Td className="text-xs text-slate-500">{a.nickname ?? "-"}</Td>
              <Td>
                <StatusBadge status={a.status} />
              </Td>
              <Td className="text-xs text-slate-500">{a.daily_used}</Td>
              <Td className="text-xs text-slate-500">{a.daily_limit}</Td>
              <Td className="text-xs text-slate-500">{formatTime(a.last_login_at)}</Td>
              <Td>
                {canManage && (
                  <Button size="sm" variant="ghost" onClick={() => toggle(a)}>
                    {a.status === "已禁用" ? "启用" : "禁用"}
                  </Button>
                )}
              </Td>
            </tr>
          ))}
        </Table>
      </Card>

      {/* 单个新增 Dialog */}
      <Dialog
        open={creating}
        title="新增下载账号"
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
          <Input label="邮箱" value={email} onChange={(e) => setEmail(e.target.value)} placeholder="user@example.com" />
          <Input label="密码（可选）" type="password" value={password} onChange={(e) => setPassword(e.target.value)} />
          <Input label="昵称（可选）" value={nickname} onChange={(e) => setNickname(e.target.value)} />
          <Input label="每日额度" type="number" min={1} value={dailyLimit} onChange={(e) => setDailyLimit(e.target.value)} />
          {formError && <div className="rounded bg-red-50 px-3 py-2 text-sm text-red-700">{formError}</div>}
        </div>
      </Dialog>

      {/* 批量导入向导 Dialog */}
      <Dialog
        open={importWizardOpen}
        title={importStep === "select" ? "批量导入账号（第 1/2 步：选择文件与模式）" : "账号脱敏预检与确认（第 2/2 步：核对与提交）"}
        onClose={() => !committing && setImportWizardOpen(false)}
        footer={
          importStep === "select" ? (
            <>
              <Button variant="secondary" onClick={() => setImportWizardOpen(false)}>
                取消
              </Button>
              <Button loading={previewing} onClick={handleUploadAndPreview}>
                上传并预检
              </Button>
            </>
          ) : (
            <>
              <Button variant="secondary" disabled={committing} onClick={() => setImportStep("select")}>
                上一步
              </Button>
              <Button loading={committing} onClick={handleCommitImport}>
                确认导入
              </Button>
            </>
          )
        }
      >
        {importStep === "select" ? (
          <div className="space-y-4">
            <div>
              <label className="mb-1 block text-xs font-medium text-slate-600">导入模式</label>
              <Select value={importMode} onChange={(e) => setImportMode(e.target.value as AccountImportMode)}>
                <option value="待注册">导入为「待注册账号」（可下发 Worker 自动注册）</option>
                <option value="已注册">导入为「已注册账号」（直接可用于下载）</option>
              </Select>
            </div>

            <div>
              <div className="mb-1 flex items-center justify-between">
                <label className="text-xs font-medium text-slate-600">选择文件（支持 txt 或 csv）</label>
                <div className="flex gap-2 text-xs">
                  <button type="button" onClick={() => handleDownloadTemplate("txt")} className="text-blue-600 hover:underline">
                    下载 .txt 模板
                  </button>
                  <span className="text-slate-300">|</span>
                  <button type="button" onClick={() => handleDownloadTemplate("csv")} className="text-blue-600 hover:underline">
                    下载 .csv 模板
                  </button>
                </div>
              </div>
              <input
                ref={fileInputRef}
                type="file"
                accept=".txt,.csv"
                onChange={(e) => setSelectedFile(e.target.files?.[0] ?? null)}
                className="w-full text-sm text-slate-500 file:mr-4 file:rounded-md file:border-0 file:bg-blue-50 file:px-4 file:py-2 file:text-sm file:font-semibold file:text-blue-700 hover:file:bg-blue-100"
              />
              <p className="mt-1 text-xs text-slate-400">
                支持格式：<code>email----password</code> 或 <code>邮箱,密码,昵称</code>
              </p>
            </div>

            {importMode === "待注册" && (
              <div className="rounded-lg bg-blue-50/60 p-3 border border-blue-100 space-y-3">
                <label className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={createRegBatch}
                    onChange={(e) => setCreateRegBatch(e.target.checked)}
                    className="rounded border-slate-300 text-blue-600 focus:ring-blue-500"
                  />
                  <span className="text-xs font-semibold text-blue-900">同时为这批待注册账号创建「账号注册批次」</span>
                </label>

                {createRegBatch && (
                  <div className="space-y-2 pl-6">
                    <Input
                      label="注册批次名称（选填）"
                      value={regBatchName}
                      onChange={(e) => setRegBatchName(e.target.value)}
                      placeholder="默认自动生成时间戳批次名"
                    />
                    <div className="grid grid-cols-2 gap-2">
                      <Input
                        label="批次优先级"
                        type="number"
                        value={regPriority}
                        onChange={(e) => setRegPriority(e.target.value)}
                      />
                      <div className="flex items-center pt-5">
                        <label className="flex items-center gap-1.5 cursor-pointer">
                          <input
                            type="checkbox"
                            checked={startImmediately}
                            onChange={(e) => setStartImmediately(e.target.checked)}
                            className="rounded border-slate-300 text-blue-600 focus:ring-blue-500"
                          />
                          <span className="text-xs text-slate-700">创建后立即启动注册</span>
                        </label>
                      </div>
                    </div>
                  </div>
                )}
              </div>
            )}
          </div>
        ) : (
          previewData && (
            <div className="space-y-4">
              <div className="grid grid-cols-4 gap-2 text-center">
                <div className="rounded-lg bg-slate-50 p-2 border border-slate-200">
                  <div className="text-xs text-slate-500">总行数</div>
                  <div className="text-lg font-bold text-slate-800">{previewData.total_rows}</div>
                </div>
                <div className="rounded-lg bg-blue-50 p-2 border border-blue-200">
                  <div className="text-xs text-blue-600">有效待导入</div>
                  <div className="text-lg font-bold text-blue-700">{previewData.valid_rows}</div>
                </div>
                <div className="rounded-lg bg-amber-50 p-2 border border-amber-200">
                  <div className="text-xs text-amber-600">文件内重复</div>
                  <div className="text-lg font-bold text-amber-700">{previewData.duplicate_in_file}</div>
                </div>
                <div className="rounded-lg bg-purple-50 p-2 border border-purple-200">
                  <div className="text-xs text-purple-600">库内已有</div>
                  <div className="text-lg font-bold text-purple-700">{previewData.duplicate_in_library}</div>
                </div>
              </div>

              {previewData.warnings.length > 0 && (
                <div className="rounded-md bg-amber-50 p-2 border border-amber-200 text-xs text-amber-800">
                  {previewData.warnings.map((w, i) => (
                    <div key={i}>• {w}</div>
                  ))}
                </div>
              )}

              <div>
                <div className="mb-1 text-xs font-medium text-slate-700">
                  账号脱敏预览（密文安全存储，明文密码不回显）：
                </div>
                <div className="max-h-56 overflow-y-auto rounded border border-slate-200">
                  <table className="w-full text-left text-xs">
                    <thead className="bg-slate-50 text-slate-600 border-b">
                      <tr>
                        <th className="p-1.5 w-12 text-center">行号</th>
                        <th className="p-1.5">脱敏邮箱</th>
                        <th className="p-1.5">昵称</th>
                        <th className="p-1.5 w-20 text-center">密码状态</th>
                        <th className="p-1.5 w-24">状态</th>
                      </tr>
                    </thead>
                    <tbody className="divide-y divide-slate-100">
                      {previewData.preview.map((p, idx) => (
                        <tr key={idx} className={p.status === "错误" ? "bg-red-50/50" : ""}>
                          <td className="p-1.5 text-center text-slate-400">{p.line}</td>
                          <td className="p-1.5 font-medium text-slate-800">{p.email_masked}</td>
                          <td className="p-1.5 text-slate-500">{p.nickname || "-"}</td>
                          <td className="p-1.5 text-center">
                            {p.password_provided ? (
                              <span className="text-[11px] text-green-700 font-medium">已提供</span>
                            ) : (
                              <span className="text-[11px] text-slate-400">无</span>
                            )}
                          </td>
                          <td className="p-1.5">
                            <span
                              className={`inline-block px-1.5 py-0.5 rounded text-[11px] font-medium ${
                                p.status === "有效待导入"
                                  ? "bg-blue-100 text-blue-800"
                                  : p.status === "库内已有"
                                  ? "bg-purple-100 text-purple-800"
                                  : p.status === "文件内重复"
                                  ? "bg-amber-100 text-amber-800"
                                  : "bg-red-100 text-red-800"
                              }`}
                            >
                              {p.status}
                            </span>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            </div>
          )
        )}
      </Dialog>

      {/* Outlook 同步向导 Dialog */}
      <Dialog
        open={outlookWizardOpen}
        title={
          outlookStep === "settings"
            ? "同步 Outlook 账号（第 1/2 步：设置云端统一注册密码与批次）"
            : "选择要注册的账号（第 2/2 步：勾选并确认同步）"
        }
        onClose={() => !syncingOutlook && setOutlookWizardOpen(false)}
        footer={
          outlookStep === "settings" ? (
            <>
              <Button variant="secondary" onClick={() => setOutlookWizardOpen(false)}>
                取消
              </Button>
              <Button loading={previewingOutlook} onClick={handleFetchOutlookAccounts}>
                拉取 Outlook 账号清单
              </Button>
            </>
          ) : (
            <>
              <Button variant="secondary" disabled={syncingOutlook} onClick={() => setOutlookStep("settings")}>
                上一步
              </Button>
              <Button loading={syncingOutlook} onClick={handleSyncOutlook}>
                确认同步并注册
              </Button>
            </>
          )
        }
      >
        {outlookStep === "settings" ? (
          <div className="space-y-4">
            <div className="rounded-lg bg-blue-50/60 p-3 border border-blue-100 space-y-3">
              <Input
                label="云端统一注册密码（必填，6–64 字符）"
                type="password"
                value={defaultPassword}
                onChange={(e) => setDefaultPassword(e.target.value)}
                placeholder="所有新账号统一使用该密码，由云端加密存储"
              />
              <p className="text-xs text-slate-500">
                outlookmail 服务只提供账号清单、不返回密码，注册密码由云端统一设置并加密下发 Worker。
              </p>
            </div>

            <div className="rounded-lg bg-slate-50 p-3 border border-slate-200 space-y-3">
              <label className="flex items-center gap-2 cursor-pointer">
                <input
                  type="checkbox"
                  checked={outlookCreateBatch}
                  onChange={(e) => setOutlookCreateBatch(e.target.checked)}
                  className="rounded border-slate-300 text-blue-600 focus:ring-blue-500"
                />
                <span className="text-xs font-semibold text-slate-800">同步后为勾选账号创建「账号注册批次」</span>
              </label>

              {outlookCreateBatch && (
                <div className="space-y-2 pl-6">
                  <Input
                    label="注册批次名称（选填）"
                    value={outlookBatchName}
                    onChange={(e) => setOutlookBatchName(e.target.value)}
                    placeholder="默认自动生成时间戳批次名"
                  />
                  <div className="grid grid-cols-2 gap-2">
                    <Input
                      label="批次优先级"
                      type="number"
                      value={outlookPriority}
                      onChange={(e) => setOutlookPriority(e.target.value)}
                    />
                    <div className="flex items-center pt-5">
                      <label className="flex items-center gap-1.5 cursor-pointer">
                        <input
                          type="checkbox"
                          checked={outlookStartImmediately}
                          onChange={(e) => setOutlookStartImmediately(e.target.checked)}
                          className="rounded border-slate-300 text-blue-600 focus:ring-blue-500"
                        />
                        <span className="text-xs text-slate-700">创建后立即启动注册</span>
                      </label>
                    </div>
                  </div>
                </div>
              )}
            </div>
          </div>
        ) : (
          outlookPreview && (
            <div className="space-y-4">
              <div className="grid grid-cols-3 gap-2 text-center">
                <div className="rounded-lg bg-blue-50 p-2 border border-blue-200">
                  <div className="text-xs text-blue-600">outlookmail 待注册账号</div>
                  <div className="text-lg font-bold text-blue-700">{outlookPreview.fetched}</div>
                </div>
                <div className="rounded-lg bg-amber-50 p-2 border border-amber-200">
                  <div className="text-xs text-amber-600">已勾选</div>
                  <div className="text-lg font-bold text-amber-700">{selectedOutlookEmails.size}</div>
                </div>
                <div className="rounded-lg bg-slate-50 p-2 border border-slate-200">
                  <div className="text-xs text-slate-500">跳过（非法/重复）</div>
                  <div className="text-lg font-bold text-slate-700">{outlookPreview.skipped}</div>
                </div>
              </div>

              <div className="flex items-center justify-between text-xs text-slate-600">
                <span>勾选要注册的账号（云端写入为「待注册」并下发 Worker 注册）：</span>
                <label className="flex items-center gap-1.5 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={selectedOutlookEmails.size === outlookPreview.accounts.length}
                    onChange={(e) => toggleSelectAllOutlook(e.target.checked)}
                    className="rounded border-slate-300 text-blue-600 focus:ring-blue-500"
                  />
                  全选
                </label>
              </div>

              <div className="max-h-64 overflow-y-auto rounded border border-slate-200">
                <table className="w-full text-left text-xs">
                  <thead className="bg-slate-50 text-slate-600 border-b">
                    <tr>
                      <th className="p-1.5 w-10 text-center">勾选</th>
                      <th className="p-1.5">邮箱</th>
                      <th className="p-1.5">昵称</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-slate-100">
                    {outlookPreview.accounts.map((acc: OutlookPreviewAccount) => (
                      <tr
                        key={acc.email}
                        className={selectedOutlookEmails.has(acc.email) ? "bg-blue-50/40" : ""}
                      >
                        <td className="p-1.5 text-center">
                          <input
                            type="checkbox"
                            checked={selectedOutlookEmails.has(acc.email)}
                            onChange={() => toggleOutlookAccount(acc.email)}
                            className="rounded border-slate-300 text-blue-600 focus:ring-blue-500"
                          />
                        </td>
                        <td className="p-1.5 font-medium text-slate-800">{acc.email}</td>
                        <td className="p-1.5 text-slate-500">{acc.nickname || "-"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )
        )}
      </Dialog>
    </div>
  );
}
